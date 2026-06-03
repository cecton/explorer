use crate::loader::LoadedFile;
use crate::lsp::{self, LSP_RRT};
use crate::tui::preview::DragModifier;
use crate::tui::*;
use crate::watcher::{WATCHER_RRT, set_watcher_root};
use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use r3bl_tui::ClipboardService;
use r3bl_tui::core::osc::OscEvent;
use r3bl_tui::core::pty::{
    CursorKeyMode, DefaultPtySessionConfig, MouseTrackingMode, PtyInputEvent, PtyOutputEvent,
    PtySessionBuilder, PtySessionConfigOption,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Id {
    Container = 1,
    /// Pane slots 0-4 (positional, not tied to a specific window).
    Pane0 = 2,
    Pane1 = 3,
    Pane2 = 4,
    Pane3 = 5,
    Pane4 = 6,
}

impl Id {
    pub fn pane(slot: usize) -> Self {
        match slot {
            0 => Id::Pane0,
            1 => Id::Pane1,
            2 => Id::Pane2,
            3 => Id::Pane3,
            _ => Id::Pane4,
        }
    }
}

impl From<Id> for u8 {
    fn from(id: Id) -> u8 {
        id as u8
    }
}

impl From<Id> for FlexBoxId {
    fn from(id: Id) -> FlexBoxId {
        FlexBoxId::new(id)
    }
}

/// Dispatcher component for a single pane slot. Holds both inner component types and
/// delegates to the correct one based on which `Window` is currently assigned to this slot
/// in `state.window_stack`.
struct PaneComponent {
    id: FlexBoxId,
    slot: usize,
    picker: FileNamePickerComponent,
    theme_picker: ThemePickerComponent,
    preview: FilePreviewComponent,
    terminal: TerminalPaneComponent,
    /// Origin row of the content area (below title bar), used for scrollbar mouse events.
    content_origin_row: u16,
    /// Total columns in the content area (full width including scrollbar column).
    content_col_count: u16,
    /// Total rows in the content area.
    content_row_count: u16,
    /// Origin column of the content area, used for absolute scrollbar column calculation.
    content_origin_col: u16,
    /// Whether the user is currently dragging the scrollbar thumb.
    scrollbar_dragging: bool,
    /// (scroll, rel_y) at thumb grab time (None if drag started on track).
    scrollbar_grab_state: Option<(usize, usize)>,
    preview_drag_active: bool,
    text_drag_active: bool,
    last_click: Option<(Instant, Pos)>,
    consecutive_clicks: u8,
}

impl PaneComponent {
    fn new_boxed(
        slot: usize,
        id: FlexBoxId,
        picker_results_tx: mpsc::Sender<PickerResultMsg>,
        picker_generation: Arc<AtomicU64>,
    ) -> BoxedSafeComponent<AppState, AppSignal> {
        Box::new(Self {
            id,
            slot,
            picker: FileNamePickerComponent::new(id, picker_results_tx, picker_generation),
            theme_picker: ThemePickerComponent::new(id),
            preview: FilePreviewComponent::new(id),
            terminal: TerminalPaneComponent::new(id),
            content_origin_row: 0,
            content_col_count: 0,
            content_row_count: 0,
            content_origin_col: 0,
            scrollbar_dragging: false,
            scrollbar_grab_state: None,
            preview_drag_active: false,
            text_drag_active: false,
            last_click: None,
            consecutive_clicks: 0,
        })
    }

    fn active_window<'s>(&self, state: &'s AppState) -> Option<&'s Window> {
        state.window_stack.get(self.slot)
    }

    fn handle_scrollbar(
        &mut self,
        mouse: MouseInput,
        global_data: &mut GlobalData<AppState, AppSignal>,
    ) -> EventPropagation {
        let Some(window) = self.active_window(&global_data.state).cloned() else {
            return EventPropagation::Propagate;
        };

        let row = mouse.pos.row_index.as_usize();
        let rel_y = row.saturating_sub(self.content_origin_row as usize);

        let state = &mut global_data.state;
        let scroll = state.window_scroll(&window);
        let scroll_max = state.window_scroll_max(&window);
        let page_size = state.window_page_size(&window);
        let scrollbar_height = self.content_row_count as usize;

        match mouse.kind {
            MouseInputKind::MouseDown(Button::Left) => {
                if scroll_max > page_size && scrollbar_height > 0 {
                    let thumb_height = thumb_size(scrollbar_height, page_size, scroll_max);
                    let thumb_pos = thumb_position(
                        scroll,
                        scrollbar_height,
                        thumb_height,
                        scroll_max,
                        page_size,
                    );
                    if rel_y >= thumb_pos && rel_y < thumb_pos + thumb_height {
                        // Grab the thumb: snapshot current state, no scroll jump.
                        self.scrollbar_dragging = true;
                        self.scrollbar_grab_state = Some((scroll, rel_y));
                        return EventPropagation::ConsumedRender;
                    } else {
                        // Click on track: jump directly to clicked position.
                        self.scrollbar_dragging = true;
                        self.scrollbar_grab_state = None;
                        let target = scroll_from_y(rel_y, scrollbar_height, scroll_max, page_size);
                        return self.apply_scroll(state, &window, target);
                    }
                }
                EventPropagation::ConsumedRender
            }
            MouseInputKind::MouseDrag(Button::Left) if self.scrollbar_dragging => {
                if scroll_max > page_size && scrollbar_height > 0 {
                    let target = if let Some((grab_scroll, grab_rel_y)) = self.scrollbar_grab_state
                    {
                        let thumb_height = thumb_size(scrollbar_height, page_size, scroll_max);
                        let denom = (scrollbar_height - thumb_height).max(1);
                        let range = scroll_max - page_size;
                        let delta_y = (rel_y as isize) - (grab_rel_y as isize);
                        let delta_scroll = delta_y * (range as isize) / (denom as isize);
                        ((grab_scroll as isize) + delta_scroll).max(0) as usize
                    } else {
                        scroll_from_y(rel_y, scrollbar_height, scroll_max, page_size)
                    };
                    return self.apply_scroll(state, &window, target);
                }
                EventPropagation::ConsumedRender
            }
            MouseInputKind::MouseUp(Button::Left) => {
                self.scrollbar_dragging = false;
                self.scrollbar_grab_state = None;
                EventPropagation::ConsumedRender
            }
            MouseInputKind::ScrollUp => {
                let target = scroll.saturating_sub(3);
                self.apply_scroll(state, &window, target)
            }
            MouseInputKind::ScrollDown => {
                let target = scroll.saturating_add(3);
                self.apply_scroll(state, &window, target)
            }
            _ => EventPropagation::ConsumedRender,
        }
    }

    fn apply_scroll(
        &mut self,
        state: &mut AppState,
        window: &Window,
        target: usize,
    ) -> EventPropagation {
        match window {
            Window::FilePreview(_) => {
                state.set_window_scroll(window, target);
                state.clamp_scroll(window);
                EventPropagation::ConsumedRender
            }
            Window::FileNamePicker => {
                let scroll_max = state.window_scroll_max(window);
                if scroll_max == 0 {
                    return EventPropagation::ConsumedRender;
                }
                let idx = target.min(scroll_max.saturating_sub(1));
                if let Some((key, _)) = state.file_name_picker.results.get(idx) {
                    state.file_name_picker.selected = Some(*key);
                }
                EventPropagation::ConsumedRender
            }
            Window::ThemePicker => {
                let scroll_max = state.window_scroll_max(window);
                if scroll_max == 0 {
                    return EventPropagation::ConsumedRender;
                }
                let idx = target.min(scroll_max.saturating_sub(1));
                if let Some((name, _)) = state.theme_picker.results.get(idx) {
                    state.theme_picker.selected = Some(name.clone());
                    if let Some(theme) = HelixTheme::from_name(name) {
                        state.theme = theme;
                    }
                }
                EventPropagation::ConsumedRender
            }
            Window::Terminal(_) => EventPropagation::ConsumedRender,
        }
    }
}

fn thumb_size(scrollbar_height: usize, page_size: usize, scroll_max: usize) -> usize {
    std::cmp::max(1, (scrollbar_height * page_size) / scroll_max.max(1))
}

fn thumb_position(
    scroll: usize,
    scrollbar_height: usize,
    thumb_height: usize,
    scroll_max: usize,
    page_size: usize,
) -> usize {
    if scroll_max <= page_size {
        0
    } else {
        (scroll * (scrollbar_height - thumb_height)) / (scroll_max - page_size)
    }
}

fn scroll_from_y(
    rel_y: usize,
    scrollbar_height: usize,
    scroll_max: usize,
    page_size: usize,
) -> usize {
    if scroll_max <= page_size || scrollbar_height == 0 {
        return 0;
    }
    (rel_y * (scroll_max - page_size)) / (scrollbar_height.saturating_sub(1).max(1))
}

fn drag_modifier_from_mouse(mouse: &MouseInput) -> Option<DragModifier> {
    let mask = mouse.maybe_modifier_keys?;
    let shift = mask.shift_key_state == KeyState::Pressed;
    let ctrl = mask.ctrl_key_state == KeyState::Pressed;
    let alt = mask.alt_key_state == KeyState::Pressed;
    if alt {
        return None;
    }
    match (shift, ctrl) {
        (true, false) => Some(DragModifier::Shift),
        (false, true) => Some(DragModifier::Ctrl),
        _ => None,
    }
}

impl Component<AppState, AppSignal> for PaneComponent {
    fn reset(&mut self) {
        self.picker.reset();
        self.theme_picker.reset();
        self.preview.reset();
        self.terminal.reset();
        self.preview_drag_active = false;
        self.text_drag_active = false;
        self.last_click = None;
        self.consecutive_clicks = 0;
    }

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        input_event: InputEvent,
        has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        // If a preview drag was active but the window changed, clean up.
        if self.preview_drag_active
            && !matches!(
                self.active_window(&global_data.state),
                Some(Window::FilePreview(_))
            )
        {
            self.preview_drag_active = false;
            self.preview.end_drag();
            global_data.state.mouse_drag_active = false;
        }

        if self.text_drag_active
            && !matches!(
                self.active_window(&global_data.state),
                Some(Window::FilePreview(_))
            )
        {
            self.text_drag_active = false;
            self.preview.end_text_drag();
            global_data.state.mouse_drag_active = false;
            self.last_click = None;
            self.consecutive_clicks = 0;
        }

        let active_is_terminal = matches!(
            self.active_window(&global_data.state),
            Some(Window::Terminal(_))
        );

        // Check for scrollbar mouse interaction first.
        if !active_is_terminal
            && !self.preview_drag_active
            && !self.text_drag_active
            && let InputEvent::Mouse(mouse) = input_event
        {
            let col = mouse.pos.col_index.as_usize();
            let row = mouse.pos.row_index.as_usize();
            let origin_col = self.content_origin_col as usize;
            let origin_row = self.content_origin_row as usize;
            let col_count = self.content_col_count as usize;
            let scrollbar_col = origin_col + col_count.saturating_sub(1);
            let bottom_row = origin_row + self.content_row_count as usize;

            let in_vertical = self.scrollbar_dragging || (row >= origin_row && row < bottom_row);
            let in_horizontal = col == scrollbar_col
                || (self.scrollbar_dragging && col >= origin_col && col < origin_col + col_count);

            if self.content_row_count > 0 && in_vertical && in_horizontal {
                return Ok(self.handle_scrollbar(mouse, global_data));
            }
        }

        // Check for title bar range click.
        if let InputEvent::Mouse(mouse) = input_event
            && let Some(window) = self.active_window(&global_data.state).cloned()
            && let Window::FilePreview(key) = window
        {
            let title_bar_row = self.content_origin_row.saturating_sub(1) as usize;
            let origin_col = self.content_origin_col as usize;
            let pane_width = self.content_col_count as usize;
            let col = mouse.pos.col_index.as_usize();
            let row = mouse.pos.row_index.as_usize();

            if row == title_bar_row
                && col >= origin_col
                && col < origin_col + pane_width
                && matches!(mouse.kind, MouseInputKind::MouseDown(Button::Left))
                && let Some((lo, hi)) = self.preview.range_at_title_col(
                    &global_data.state,
                    col - origin_col,
                    pane_width,
                )
            {
                self.preview
                    .scroll_to_range(&mut global_data.state, key, lo, hi);
                return Ok(EventPropagation::ConsumedRender);
            }
        }

        // Preview content drag.
        if let InputEvent::Mouse(mouse) = input_event
            && let Some(window) = self.active_window(&global_data.state).cloned()
            && let Window::FilePreview(key) = window
        {
            let col = mouse.pos.col_index.as_usize();
            let row = mouse.pos.row_index.as_usize();
            let origin_row = self.content_origin_row as usize;
            let origin_col = self.content_origin_col as usize;
            let col_count = self.content_col_count as usize;
            let row_count = self.content_row_count as usize;

            let in_content_rows = row >= origin_row && row < origin_row + row_count;
            let in_content_cols =
                col >= origin_col && col < origin_col + col_count.saturating_sub(1);

            match mouse.kind {
                MouseInputKind::MouseDown(Button::Left) if in_content_rows && in_content_cols => {
                    if let Some(modifier) = drag_modifier_from_mouse(&mouse) {
                        let state = &mut global_data.state;
                        let window = Window::FilePreview(key);
                        let scroll = state.window_scroll(&window);
                        let scroll_max = state.window_scroll_max(&window);
                        let rel_y = row.saturating_sub(origin_row);
                        let line = (scroll + rel_y + 1).clamp(1, scroll_max.max(1));

                        self.preview_drag_active = true;
                        self.preview.start_drag(state, key, line, modifier);
                        state.mouse_drag_active = true;
                        return Ok(EventPropagation::ConsumedRender);
                    } else {
                        let state = &mut global_data.state;
                        let now = Instant::now();
                        let is_same_pos = self.last_click.is_some_and(|(_, p)| p == mouse.pos);
                        let is_quick = self
                            .last_click
                            .is_some_and(|(t, _)| now.duration_since(t).as_millis() < 300);
                        if is_same_pos && is_quick {
                            self.consecutive_clicks = self.consecutive_clicks.saturating_add(1);
                        } else {
                            self.consecutive_clicks = 1;
                        }
                        self.last_click = Some((now, mouse.pos));

                        self.text_drag_active = true;
                        state.mouse_drag_active = true;
                        self.preview.start_text_drag_from_pos(
                            state,
                            row,
                            col,
                            self.consecutive_clicks,
                        );
                        return Ok(EventPropagation::ConsumedRender);
                    }
                }
                MouseInputKind::MouseDrag(Button::Left) if self.text_drag_active => {
                    let state = &mut global_data.state;
                    self.preview.update_text_drag_from_pos(state, row, col);
                    return Ok(EventPropagation::ConsumedRender);
                }
                MouseInputKind::MouseDrag(Button::Left) if self.preview_drag_active => {
                    let state = &mut global_data.state;
                    let window = Window::FilePreview(key);
                    let scroll = state.window_scroll(&window);
                    let scroll_max = state.window_scroll_max(&window);
                    let rel_y = row.saturating_sub(origin_row);
                    let line = (scroll + rel_y + 1).clamp(1, scroll_max.max(1));

                    self.preview.update_drag(state, key, line);
                    return Ok(EventPropagation::ConsumedRender);
                }
                MouseInputKind::MouseUp(Button::Left) if self.text_drag_active => {
                    let state = &mut global_data.state;
                    if let Some(text) = self.preview.end_text_drag_with_text(state) {
                        let mut cb = r3bl_tui::SystemClipboard;
                        let _ = cb.try_to_put_content_into_clipboard(text);
                    }
                    self.text_drag_active = false;
                    state.mouse_drag_active = false;
                    return Ok(EventPropagation::ConsumedRender);
                }
                MouseInputKind::MouseUp(Button::Left) if self.preview_drag_active => {
                    self.preview_drag_active = false;
                    self.preview.end_drag();
                    global_data.state.mouse_drag_active = false;
                    return Ok(EventPropagation::ConsumedRender);
                }
                _ => {}
            }
        }

        match self.active_window(&global_data.state).cloned() {
            Some(Window::FileNamePicker) => {
                self.picker
                    .handle_event(global_data, input_event, has_focus)
            }
            Some(Window::ThemePicker) => {
                self.theme_picker
                    .handle_event(global_data, input_event, has_focus)
            }
            Some(Window::FilePreview(_)) => {
                self.preview
                    .handle_event(global_data, input_event, has_focus)
            }
            Some(Window::Terminal(_)) => {
                self.terminal
                    .handle_event(global_data, input_event, has_focus)
            }
            None => Ok(EventPropagation::Propagate),
        }
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        current_box: FlexBox,
        surface_bounds: SurfaceBounds,
        has_focus: &mut HasFocus,
    ) -> CommonResult<RenderPipeline> {
        throws_with_return!({
            global_data.state.pane_boxes[self.slot] = current_box;

            let active_window = self.active_window(&global_data.state).cloned();
            let add_title = active_window.is_some();

            let mut title_ops = RenderOpIRVec::new();
            if add_title {
                let focused = has_focus.get_id() == Some(self.id);
                let title_origin = current_box.style_adjusted_origin_pos;
                let title_width = current_box.style_adjusted_bounds_size.col_width.as_u16();
                let theme = &global_data.state.theme;

                match active_window.as_ref().unwrap() {
                    Window::FileNamePicker => {
                        let query = global_data.state.file_name_picker.query.clone();
                        self.picker.render_title_row(
                            &mut title_ops,
                            title_origin,
                            title_width,
                            focused,
                            theme,
                            &query,
                        );
                    }
                    Window::ThemePicker => {
                        let query = global_data.state.theme_picker.query.clone();
                        self.theme_picker.render_title_row(
                            &mut title_ops,
                            title_origin,
                            title_width,
                            focused,
                            theme,
                            &query,
                        );
                    }
                    Window::FilePreview(key) => {
                        if !self.preview.render_title_row(
                            &mut title_ops,
                            title_origin,
                            title_width,
                            focused,
                            theme,
                        ) {
                            let snapshot = global_data.state.files.load();
                            let removed = snapshot[key.0]
                                .removed
                                .load(std::sync::atomic::Ordering::Relaxed);
                            let title = self.preview.title_text(&global_data.state);
                            render_pane_title(
                                &mut title_ops,
                                &current_box,
                                &title,
                                removed,
                                theme,
                                focused,
                            );
                        }
                    }
                    Window::Terminal(id) => {
                        let (base, exited, exit_code, exit_signal) = global_data
                            .state
                            .terminal_panes
                            .get(id)
                            .and_then(|p| p.lock().ok())
                            .map(|g| {
                                (
                                    g.title
                                        .clone()
                                        .unwrap_or_else(|| format!("Terminal {}", id)),
                                    g.exited,
                                    g.exit_code,
                                    g.exit_signal.clone(),
                                )
                            })
                            .unwrap_or_else(|| (format!("Terminal {}", id), false, None, None));
                        let title = if let Some(ref sig) = exit_signal {
                            format!("{} [{}]", base, sig)
                        } else if let Some(code) = exit_code {
                            format!("{} [exit {}]", base, code)
                        } else if exited {
                            format!("{} [done]", base)
                        } else {
                            base
                        };
                        render_pane_title(
                            &mut title_ops,
                            &current_box,
                            &title,
                            false,
                            theme,
                            focused,
                        );
                    }
                }
            }

            let (content_box, inner_bounds) = if add_title {
                let origin = current_box.style_adjusted_origin_pos + height(1);
                let bounds = current_box.style_adjusted_bounds_size.col_width
                    + (current_box.style_adjusted_bounds_size.row_height - height(1));
                let scrollbar_col = bounds.col_width - width(1);
                let inner_bounds = scrollbar_col + bounds.row_height;
                let boxed = FlexBox {
                    style_adjusted_origin_pos: origin,
                    style_adjusted_bounds_size: bounds,
                    ..current_box
                };
                (
                    boxed,
                    FlexBox {
                        style_adjusted_origin_pos: origin,
                        style_adjusted_bounds_size: inner_bounds,
                        ..current_box
                    },
                )
            } else {
                let bounds = current_box.style_adjusted_bounds_size;
                let scrollbar_col = bounds.col_width - width(1);
                let inner_bounds = scrollbar_col + bounds.row_height;
                let boxed = FlexBox {
                    style_adjusted_bounds_size: bounds,
                    ..current_box
                };
                (
                    boxed,
                    FlexBox {
                        style_adjusted_bounds_size: inner_bounds,
                        ..current_box
                    },
                )
            };

            // Store content area geometry for scrollbar mouse event handling.
            self.content_origin_row = content_box.style_adjusted_origin_pos.row_index.as_u16();
            self.content_origin_col = content_box.style_adjusted_origin_pos.col_index.as_u16();
            self.content_col_count = content_box.style_adjusted_bounds_size.col_width.as_u16();
            self.content_row_count = content_box.style_adjusted_bounds_size.row_height.as_u16();

            let inner_pipeline = match active_window {
                Some(Window::FileNamePicker) => {
                    self.picker
                        .render(global_data, inner_bounds, surface_bounds, has_focus)?
                }
                Some(Window::ThemePicker) => self.theme_picker.render(
                    global_data,
                    inner_bounds,
                    surface_bounds,
                    has_focus,
                )?,
                Some(Window::FilePreview(_)) => {
                    self.preview
                        .render(global_data, inner_bounds, surface_bounds, has_focus)?
                }
                Some(Window::Terminal(_)) => {
                    self.terminal
                        .render(global_data, content_box, surface_bounds, has_focus)?
                }
                None => r3bl_tui::render_pipeline!(),
            };

            let mut pipeline = if add_title {
                let mut p = r3bl_tui::render_pipeline!();
                p.push(ZOrder::Normal, title_ops);
                p.join_into(inner_pipeline);
                p
            } else {
                inner_pipeline
            };

            // Render scrollbar on the rightmost column if there's an active window.
            if let Some(ref window) = self.active_window(&global_data.state).cloned()
                && !matches!(window, Window::Terminal(_))
            {
                let state = &global_data.state;
                let scroll = state.window_scroll(window);
                let scroll_max = state.window_scroll_max(window);
                let page_size = state.window_page_size(window);
                let mut scrollbar_ops = RenderOpIRVec::new();
                render_scrollbar(
                    &mut scrollbar_ops,
                    &content_box,
                    scroll,
                    scroll_max,
                    page_size,
                    &state.theme,
                );
                pipeline.push(ZOrder::Normal, scrollbar_ops);
            }

            pipeline
        });
    }
}

pub struct AppMain {
    files: Arc<ArcSwap<Vec<LoadedFile>>>,
    root: Utf8PathBuf,
    picker_results_tx: mpsc::Sender<PickerResultMsg>,
    picker_results_rx: mpsc::Receiver<PickerResultMsg>,
    picker_generation: Arc<AtomicU64>,
    exit_tx: Arc<OnceLock<mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>>>,
    terminal_event_tx: mpsc::UnboundedSender<(usize, PtyOutputEvent)>,
    terminal_event_rx: mpsc::UnboundedReceiver<(usize, PtyOutputEvent)>,
}

impl AppMain {
    fn new_boxed(
        files: Arc<ArcSwap<Vec<LoadedFile>>>,
        root: Utf8PathBuf,
        exit_tx: Arc<OnceLock<mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>>>,
    ) -> BoxedSafeApp<AppState, AppSignal> {
        let (picker_results_tx, picker_results_rx) = mpsc::channel(32);
        let (terminal_event_tx, terminal_event_rx) = mpsc::unbounded_channel();
        Box::new(Self {
            files,
            root,
            picker_results_tx,
            picker_results_rx,
            picker_generation: Arc::new(AtomicU64::new(0)),
            exit_tx,
            terminal_event_tx,
            terminal_event_rx,
        })
    }

    fn open_terminal(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        cmd: Option<String>,
        cwd: Option<Utf8PathBuf>,
    ) -> CommonResult<EventPropagation> {
        throws_with_return!({
            let state = &mut global_data.state;
            let id = state.next_terminal_id;
            state.next_terminal_id += 1;

            let window_size = global_data.window_size;
            let visible_count = state.window_stack.len().max(1) as u16;
            let pty_cols = (window_size.col_width.as_u16() / visible_count).max(80);
            let pty_rows = window_size.row_height.as_u16().saturating_sub(2);
            let pty_size = Size {
                col_width: width(pty_cols),
                row_height: height(pty_rows),
            };

            let is_command_pane = cmd.as_deref().is_some_and(|s| !s.is_empty());
            let mut builder = if is_command_pane {
                PtySessionBuilder::new("/bin/sh").cli_args(["-c", cmd.as_deref().unwrap()])
            } else {
                PtySessionBuilder::new(shell_command())
            };
            builder = builder.env_var("TERM", "xterm-256color");
            if let Some(ref cwd_path) = cwd {
                builder = builder.cwd(cwd_path.as_std_path());
            }
            let mut session = match builder
                .with_config(DefaultPtySessionConfig + PtySessionConfigOption::Size(pty_size))
                .start()
            {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("Failed to start PTY: {e}");
                    return Ok(EventPropagation::ConsumedRender);
                }
            };

            let ofs_buf = r3bl_tui::OffscreenBuffer::new_empty(pty_size);
            let pty_input_tx = Arc::new(session.tx_input_event.clone());
            let child_killer = session.child_process_termination_handle;
            let initial_title = cmd.filter(|s| !s.is_empty());
            let pane = Arc::new(Mutex::new(TerminalPane {
                ofs_buf,
                cursor_key_mode: CursorKeyMode::Normal,
                mouse_tracking_mode: MouseTrackingMode::None,
                title: initial_title,
                pty_input_tx,
                child_killer: Some(child_killer),
                last_size: pty_size,
                is_command_pane,
                exited: false,
                exit_code: None,
                exit_signal: None,
            }));

            state.terminal_panes.insert(id, Arc::clone(&pane));

            let notify_tx = global_data.main_thread_channel_sender.clone();
            let event_tx = self.terminal_event_tx.clone();
            tokio::spawn(async move {
                let mut last_event = Instant::now();
                let mut backoff: Option<Instant> = None;
                let mut burst_start: Option<Instant> = None;
                while let Some(event) = session.rx_output_event.recv().await {
                    let is_exit = matches!(&event, PtyOutputEvent::Exit(_));
                    match event {
                        PtyOutputEvent::Output(bytes) => {
                            if let Ok(mut pane) = pane.lock() {
                                let (osc_events, _, da_responses) =
                                    pane.ofs_buf.apply_ansi_bytes(&bytes);
                                for osc_event in osc_events {
                                    if let OscEvent::SetTitleAndTab(title) = osc_event {
                                        pane.title = Some(title);
                                    }
                                }
                                for da_response in da_responses {
                                    let _ = pane
                                        .pty_input_tx
                                        .try_send(PtyInputEvent::Write(da_response.into_bytes()));
                                }
                            }
                        }
                        PtyOutputEvent::CursorModeChange(mode) => {
                            if let Ok(mut pane) = pane.lock() {
                                pane.cursor_key_mode = mode;
                            }
                        }
                        PtyOutputEvent::MouseModeChange(mode) => {
                            if let Ok(mut pane) = pane.lock() {
                                pane.mouse_tracking_mode = mode;
                            }
                        }
                        PtyOutputEvent::Exit(status) => {
                            if event_tx.send((id, PtyOutputEvent::Exit(status))).is_err() {
                                break;
                            }
                        }
                        _ => {}
                    }

                    // Exit: always send Noop (never throttled) so the
                    // terminal pane is removed from the UI immediately.
                    if is_exit {
                        let _ = notify_tx.try_send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                            AppSignal::Noop,
                        ));
                        break;
                    }

                    let now = Instant::now();

                    // Throttle: once the channel has filled (backoff ==
                    // Some), suppress all Noops as long as events keep
                    // arriving within 100ms gaps.  This threshold catches
                    // program output (≥10 events/s) while cleanly
                    // separating interactive typing (~200ms between keys).
                    // last_event is updated on every event (including
                    // suppressed), so a sustained burst keeps the gate
                    // closed indefinitely.  A gap ≥100ms in events resets
                    // burst tracking and the task tries to send again.
                    if last_event.elapsed().as_millis() < 100 {
                        if backoff.is_some()
                            || burst_start.is_some_and(|t| t.elapsed().as_secs() >= 3)
                        {
                            backoff = Some(now);
                            last_event = now;
                            continue;
                        } else if burst_start.is_none() {
                            burst_start = Some(now);
                        }
                    } else {
                        burst_start = None;
                    }

                    // Channel has room (or backoff expired): try to send.
                    // If it succeeds, clear backoff.  If it fails (buffer
                    // full at 1000), enter backoff — subsequent events are
                    // suppressed until activity pauses for >=1s.
                    match notify_tx.try_send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                        AppSignal::Noop,
                    )) {
                        Ok(()) => backoff = None,
                        Err(_) => backoff = Some(now),
                    }

                    last_event = now;
                }
            });

            let window = Window::Terminal(id);
            state.push_window(window.clone());
            state.focused_window = Some(window);

            EventPropagation::ConsumedRender
        });
    }
}

fn shell_command() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "bash".into())
}

fn is_buffer_empty(ofs_buf: &OffscreenBuffer) -> bool {
    ofs_buf.buffer.iter().all(|line| {
        line.iter()
            .all(|pc| !matches!(pc, PixelChar::PlainText { .. }))
    })
}

fn poll_terminal_output(app: &mut AppMain, state: &mut AppState) {
    while let Ok((id, event)) = app.terminal_event_rx.try_recv() {
        if let PtyOutputEvent::Exit(status) = event {
            let exit_code = Some(status.exit_code());
            let exit_signal = status.signal().map(String::from);
            let remove_now = state
                .terminal_panes
                .get(&id)
                .and_then(|pane| pane.lock().ok())
                .is_some_and(|p| is_buffer_empty(&p.ofs_buf));

            if remove_now {
                if let Some(pane) = state.terminal_panes.remove(&id)
                    && let Ok(mut p) = pane.lock()
                    && let Some(mut killer) = p.child_killer.take()
                {
                    let _ = killer.kill();
                }
                state.remove_window(&Window::Terminal(id));
            } else if let Some(pane) = state.terminal_panes.get(&id)
                && let Ok(mut p) = pane.lock()
            {
                p.exited = true;
                p.exit_code = exit_code;
                p.exit_signal = exit_signal;
            }
        }
    }
}

impl App for AppMain {
    type S = AppState;
    type AS = AppSignal;

    fn app_init(
        &mut self,
        component_registry_map: &mut ComponentRegistryMap<Self::S, Self::AS>,
        has_focus: &mut HasFocus,
    ) {
        for slot in 0..MAX_PANES {
            let pane_id = FlexBoxId::from(Id::pane(slot));
            if let ContainsResult::DoesNotContain =
                ComponentRegistry::contains(component_registry_map, pane_id)
            {
                ComponentRegistry::put(
                    component_registry_map,
                    pane_id,
                    PaneComponent::new_boxed(
                        slot,
                        pane_id,
                        self.picker_results_tx.clone(),
                        Arc::clone(&self.picker_generation),
                    ),
                );
            }
        }

        if has_focus.get_id().is_none() {
            has_focus.set_id(FlexBoxId::from(Id::Pane0));
        }
    }

    fn app_start(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        _component_registry_map: &mut ComponentRegistryMap<AppState, AppSignal>,
        _has_focus: &mut HasFocus,
    ) {
        let notify_tx = global_data.main_thread_channel_sender.clone();

        // Publish the channel sender so the SIGTERM handler can request a clean exit.
        let _ = self.exit_tx.set(notify_tx.clone());
        let files = Arc::clone(&self.files);
        let root = self.root.clone();

        lsp::set_lsp_config(root.clone(), Arc::clone(&files));
        match LSP_RRT.try_subscribe() {
            Ok(guard) => {
                let lsp_notify = notify_tx.clone();
                // LSP uses blocking send().await — natural backpressure
                // via the bounded channel (capacity 1000). No explicit
                // backoff needed; the task blocks when the channel is
                // full and resumes once the main thread drains it.
                tokio::spawn(async move {
                    let mut rx = guard.receiver;
                    while let Ok(r3bl_tui::RRTEvent::Worker(_)) = rx.recv().await {
                        let _ = lsp_notify
                            .send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::Noop,
                            ))
                            .await;
                    }
                });
            }
            Err(e) => {
                tracing::warn!("LSP worker failed to start: {e}");
            }
        }

        set_watcher_root(&root);
        match WATCHER_RRT.try_subscribe() {
            Ok(guard) => {
                let watcher_notify = notify_tx.clone();
                tokio::spawn(async move {
                    let mut rx = guard.receiver;
                    while let Ok(r3bl_tui::RRTEvent::Worker(signal)) = rx.recv().await {
                        let _ = watcher_notify
                            .send(TerminalWindowMainThreadSignal::ApplyAppSignal(signal))
                            .await;
                    }
                });
            }
            Err(e) => {
                tracing::warn!("watcher failed to start: {e}");
            }
        }

        // Global 1s refresh timer — catches any final render state that the
        // per-task 1s debounce might miss (burst ends, no more events).
        let timer_notify = notify_tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.tick().await;
            loop {
                interval.tick().await;
                let _ = timer_notify.try_send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                    AppSignal::Noop,
                ));
            }
        });
    }

    fn app_handle_input_event(
        &mut self,
        input_event: InputEvent,
        global_data: &mut GlobalData<AppState, AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<AppState, AppSignal>,
        has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        sync_has_focus(&global_data.state, has_focus);

        // Leader key activation.
        if let InputEvent::Keyboard(KeyPress::WithModifiers { key, mask }) = input_event
            && key == Key::Character('`')
            && mask == ModifierKeysMask::new().with_alt()
            && !global_data.state.mouse_drag_active
        {
            global_data.state.leader_active = true;
            return Ok(EventPropagation::ConsumedRender);
        }

        // Leader key dispatch.
        if global_data.state.leader_active && !global_data.state.mouse_drag_active {
            global_data.state.leader_active = false;
            match &input_event {
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::Character('f'),
                })
                | InputEvent::Keyboard(KeyPress::WithModifiers {
                    key: Key::Character('f'),
                    ..
                }) => {
                    let state = &mut global_data.state;
                    state.push_window(Window::FileNamePicker);
                    state.focused_window = Some(Window::FileNamePicker);
                    state.file_name_picker.selected = None;
                    let snapshot = state.files.load();
                    state.file_name_picker.results =
                        FileNamePickerComponent::all_files_results(&snapshot, &state.window_stack);
                    state.file_name_picker.query = String::new();
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::Character('t'),
                })
                | InputEvent::Keyboard(KeyPress::WithModifiers {
                    key: Key::Character('t'),
                    ..
                }) => {
                    return self.open_terminal(global_data, None, None);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::Character('T'),
                })
                | InputEvent::Keyboard(KeyPress::WithModifiers {
                    key: Key::Character('T'),
                    ..
                }) => {
                    let state = &mut global_data.state;
                    if !state.window_stack.contains(&Window::ThemePicker) {
                        state.saved_theme = state.theme.clone();
                    }
                    let all_themes: Vec<(String, Vec<u32>)> = HelixTheme::theme_names()
                        .map(|n| (n.to_string(), Vec::new()))
                        .collect();
                    state.push_window(Window::ThemePicker);
                    state.focused_window = Some(Window::ThemePicker);
                    state.theme_picker.selected = all_themes
                        .iter()
                        .position(|(n, _)| n == state.theme.name())
                        .and_then(|i| all_themes.get(i).map(|(n, _)| n.clone()));
                    state.theme_picker.results = all_themes;
                    state.theme_picker.query = String::new();
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::Character('q'),
                })
                | InputEvent::Keyboard(KeyPress::WithModifiers {
                    key: Key::Character('q'),
                    ..
                }) => {
                    return Ok(EventPropagation::ExitMainEventLoop);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Tab),
                })
                | InputEvent::Keyboard(KeyPress::WithModifiers {
                    key: Key::SpecialKey(SpecialKey::Tab),
                    ..
                }) => {
                    let state = &mut global_data.state;
                    let visible = state.visible_windows(global_data.window_size.col_width.as_u16());
                    cycle_focus(state, &visible, 1);
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::BackTab),
                })
                | InputEvent::Keyboard(KeyPress::WithModifiers {
                    key: Key::SpecialKey(SpecialKey::BackTab),
                    ..
                }) => {
                    let state = &mut global_data.state;
                    let visible = state.visible_windows(global_data.window_size.col_width.as_u16());
                    cycle_focus(state, &visible, -1);
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::Character('x'),
                })
                | InputEvent::Keyboard(KeyPress::WithModifiers {
                    key: Key::Character('x'),
                    ..
                }) => {
                    let state = &mut global_data.state;
                    let tid = match state.focused_window.clone() {
                        Some(Window::Terminal(tid)) => tid,
                        _ => return Ok(EventPropagation::ConsumedRender),
                    };
                    if let Some(pane) = state.terminal_panes.remove(&tid)
                        && let Ok(mut p) = pane.lock()
                        && let Some(mut killer) = p.child_killer.take()
                    {
                        let _ = killer.kill();
                    }
                    state.remove_window(&Window::Terminal(tid));
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Esc),
                })
                | InputEvent::Keyboard(KeyPress::WithModifiers {
                    key: Key::SpecialKey(SpecialKey::Esc),
                    ..
                }) => {
                    return Ok(EventPropagation::ConsumedRender);
                }
                _ => {}
            }
        }

        if !matches!(global_data.state.focused_window, Some(Window::Terminal(_)))
            && !global_data.state.mouse_drag_active
        {
            match &input_event {
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Tab),
                }) => {
                    let state = &mut global_data.state;
                    let visible = state.visible_windows(global_data.window_size.col_width.as_u16());
                    cycle_focus(state, &visible, 1);
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::BackTab),
                }) => {
                    let state = &mut global_data.state;
                    let visible = state.visible_windows(global_data.window_size.col_width.as_u16());
                    cycle_focus(state, &visible, -1);
                    return Ok(EventPropagation::ConsumedRender);
                }
                _ => {}
            }
        }

        if !global_data.state.mouse_drag_active
            && matches!(
                global_data.state.focused_window,
                Some(Window::FilePreview(_))
            )
            && let InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }) = input_event
            && !global_data.state.command_mode_active
        {
            let state = &mut global_data.state;
            if let Some(window) = state.focused_window.clone() {
                state.send_to_back(&window);
            }
            return Ok(EventPropagation::ConsumedRender);
        }

        if !global_data.state.mouse_drag_active
            && let Some(Window::Terminal(tid)) = global_data.state.focused_window.clone()
        {
            let should_dismiss = global_data
                .state
                .terminal_panes
                .get(&tid)
                .and_then(|p| p.lock().ok())
                .map(|p| p.exited)
                .unwrap_or(false);
            if should_dismiss
                && matches!(
                    input_event,
                    InputEvent::Keyboard(KeyPress::Plain {
                        key: Key::SpecialKey(SpecialKey::Esc) | Key::SpecialKey(SpecialKey::Enter)
                    })
                )
            {
                if let Some(pane) = global_data.state.terminal_panes.remove(&tid)
                    && let Ok(mut p) = pane.lock()
                    && let Some(mut killer) = p.child_killer.take()
                {
                    let _ = killer.kill();
                }
                global_data.state.remove_window(&Window::Terminal(tid));
                return Ok(EventPropagation::ConsumedRender);
            }
        }

        if let InputEvent::Mouse(mouse) = &input_event
            && mouse.kind == MouseInputKind::MouseMove
            && !global_data.state.mouse_drag_active
        {
            let px = mouse.pos.col_index;
            let py = mouse.pos.row_index;
            for (slot, box_) in global_data.state.pane_boxes.iter().enumerate() {
                let ox = box_.style_adjusted_origin_pos.col_index;
                let oy = box_.style_adjusted_origin_pos.row_index;
                let w = box_.style_adjusted_bounds_size.col_width;
                let h = box_.style_adjusted_bounds_size.row_height;
                if px >= ox && px < ox + w && py >= oy && py < oy + h {
                    if let Some(window) = global_data.state.window_stack.get(slot)
                        && global_data.state.focused_window.as_ref() != Some(window)
                    {
                        global_data.state.focused_window = Some(window.clone());
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    break;
                }
            }
        }

        ComponentRegistry::route_event_to_focused_component(
            global_data,
            input_event,
            component_registry_map,
            has_focus,
        )
    }

    fn app_handle_signal(
        &mut self,
        action: &AppSignal,
        global_data: &mut GlobalData<AppState, AppSignal>,
        _component_registry_map: &mut ComponentRegistryMap<AppState, AppSignal>,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        sync_has_focus(&global_data.state, _has_focus);

        if let AppSignal::OpenTerminal { cmd, cwd } = action {
            let cmd = cmd.clone();
            let cwd = cwd.clone();
            return self.open_terminal(global_data, cmd, Some(cwd));
        }
        throws_with_return!({
            let state = &mut global_data.state;
            match action {
                AppSignal::FilesChanged(batch) => {
                    let snapshot = self.files.load_full();

                    for path in &batch.removed {
                        if let Some(file) = snapshot.iter().find(|f| &f.path == path) {
                            file.removed.store(true, Ordering::Relaxed);
                        }
                    }

                    for path in &batch.modified {
                        if let Some(file) = snapshot
                            .iter()
                            .find(|f| &f.path == path && !f.removed.load(Ordering::Relaxed))
                        {
                            file.reload();
                        }
                    }

                    let mut new_files: Vec<LoadedFile> = vec![];
                    for path in &batch.created {
                        if let Some(file) = snapshot
                            .iter()
                            .find(|f| &f.path == path && f.removed.load(Ordering::Relaxed))
                        {
                            file.removed.store(false, Ordering::Relaxed);
                            file.reload();
                        } else if !snapshot.iter().any(|f| &f.path == path)
                            && let Some(loaded) = LoadedFile::load(path.clone().into_std_path_buf())
                        {
                            new_files.push(loaded);
                        }
                    }

                    if !new_files.is_empty() {
                        let mut next: Vec<LoadedFile> = snapshot
                            .iter()
                            .map(|f| LoadedFile {
                                path: f.path.clone(),
                                data: std::sync::Mutex::new({
                                    let d = f.data.lock().unwrap();
                                    crate::loader::FileData {
                                        content: d.content.clone(),
                                        line_starts: d.line_starts.clone(),
                                    }
                                }),
                                colored_lines: std::sync::Mutex::new(
                                    f.colored_lines.lock().unwrap().clone(),
                                ),
                                removed: std::sync::atomic::AtomicBool::new(
                                    f.removed.load(Ordering::Relaxed),
                                ),
                            })
                            .collect();
                        next.extend(new_files);
                        next.sort_by(|a, b| a.path.cmp(&b.path));
                        self.files.store(Arc::new(next));
                    }

                    let snapshot = self.files.load();
                    if state.window_stack.contains(&Window::FileNamePicker) {
                        if state.file_name_picker.query.is_empty() {
                            state.file_name_picker.results =
                                FileNamePickerComponent::all_files_results(
                                    &snapshot,
                                    &state.window_stack,
                                );
                        } else {
                            FileNamePickerComponent::spawn_match(
                                &*state,
                                Arc::clone(&self.picker_generation),
                                self.picker_results_tx.clone(),
                                global_data.main_thread_channel_sender.clone(),
                            );
                        }
                    }
                    state.bump_files_version();
                }
                AppSignal::OpenTerminal { .. } => {}
                AppSignal::Noop => {}
            }

            EventPropagation::ConsumedRender
        });
    }

    fn app_render(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<AppState, AppSignal>,
        has_focus: &mut HasFocus,
    ) -> CommonResult<RenderPipeline> {
        sync_has_focus(&global_data.state, has_focus);

        let mut best_generation = 0u64;
        let mut best_results = None;
        while let Ok((arrived_generation, results)) = self.picker_results_rx.try_recv() {
            if arrived_generation > best_generation {
                best_generation = arrived_generation;
                best_results = Some(results);
            }
        }
        if let Some(results) = best_results {
            global_data.state.file_name_picker.results = results;
        }

        poll_terminal_output(self, &mut global_data.state);

        throws_with_return!({
            let window_size = global_data.window_size;
            let surface_cols = window_size.col_width.as_u16();

            let visible = global_data.state.visible_windows(surface_cols);

            // Sync focused window with actual visible windows: if the currently focused
            // window is not visible, focus the frontmost visible one.
            let focused = global_data.state.focused_window.clone();
            let focused_is_visible = focused
                .as_ref()
                .map(|f| visible.iter().any(|(w, _)| w == f))
                .unwrap_or(false);
            if !global_data.state.mouse_drag_active
                && !focused_is_visible
                && let Some((front, _)) = visible.first()
            {
                global_data.state.focused_window = Some(front.clone());
            }

            let surface = {
                let mut it = surface!(stylesheet: create_stylesheet(&global_data.state.theme)?);
                it.surface_start(SurfaceProps {
                    pos: col(0) + row(0),
                    size: {
                        let col_count = window_size.col_width;
                        let row_count = window_size.row_height - height(1);
                        col_count + row_count
                    },
                })?;

                PanesRenderer { visible: &visible }.render_in_surface(
                    &mut it,
                    global_data,
                    component_registry_map,
                    has_focus,
                )?;

                it.surface_end()?;
                it
            };

            let mut pipeline = surface.render_pipeline;

            // Fill entire surface area with pane background (covers padding
            // between panes, which the FlexBox layout system does not fill).
            let bg_rgb = global_data
                .state
                .theme
                .ui_bg("ui.background")
                .unwrap_or([15, 15, 25]);
            let bg = tui_color!(bg_rgb[0], bg_rgb[1], bg_rgb[2]);
            let bg_style = new_style!(color_bg: {bg});
            let mut bg_ops = RenderOpIRVec::new();
            let surface_rows = (window_size.row_height - height(2)).as_usize();
            let surface_col_count = window_size.col_width.as_usize();
            for row_idx in 0..surface_rows {
                let abs_row: u16 = 1 + row_idx as u16;
                bg_ops += RenderOpCommon::MoveCursorPositionAbs(col(0) + row(abs_row));
                bg_ops += RenderOpCommon::ApplyColors(Some(bg_style));
                bg_ops += RenderOpIR::PaintTextWithAttributes(
                    " ".repeat(surface_col_count).as_str().into(),
                    Some(bg_style),
                );
            }
            let mut fill_pipeline = render_pipeline!();
            fill_pipeline.push(ZOrder::Normal, bg_ops);
            fill_pipeline.join_into(pipeline);
            pipeline = fill_pipeline;

            let focused_window = global_data.state.focused_window.clone();
            render_status_bar(
                &mut pipeline,
                window_size,
                focused_window.as_ref(),
                global_data.state.leader_active,
                &global_data.state.theme,
            );

            pipeline
        });
    }
}

/// Returns the `FlexBoxId` for the pane slot that corresponds to the focused window.
pub(super) fn focused_pane_id(state: &AppState) -> FlexBoxId {
    let Some(focused) = &state.focused_window else {
        return FlexBoxId::from(Id::Pane0);
    };
    let slot = state
        .window_stack
        .iter()
        .position(|w| w == focused)
        .unwrap_or(0);
    FlexBoxId::from(Id::pane(slot))
}

fn sync_has_focus(state: &AppState, has_focus: &mut HasFocus) {
    has_focus.set_id(focused_pane_id(state));
}

fn cycle_focus(state: &mut AppState, visible: &[(Window, u16)], direction: i32) {
    if visible.is_empty() {
        return;
    }
    let current_pos = state
        .focused_window
        .as_ref()
        .and_then(|f| visible.iter().position(|(w, _)| w == f))
        .unwrap_or(0);
    let len = visible.len() as i32;
    let next_pos = ((current_pos as i32 + direction).rem_euclid(len)) as usize;
    let next_window = visible[next_pos].0.clone();
    state.focused_window = Some(next_window);
}

struct PanesRenderer<'a> {
    visible: &'a [(Window, u16)],
}

impl SurfaceRender<AppState, AppSignal> for PanesRenderer<'_> {
    fn render_in_surface(
        &mut self,
        surface: &mut Surface,
        global_data: &mut GlobalData<AppState, AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<AppState, AppSignal>,
        has_focus: &mut HasFocus,
    ) -> CommonResult<()> {
        throws!({
            let container_id = FlexBoxId::from(Id::Container);
            box_start!(
                in: surface,
                id: container_id,
                dir: LayoutDirection::Horizontal,
                requested_size_percent: req_size_pc!(width: 100, height: 100),
                styles: [container_id],
            );

            for (slot, (window, col_width)) in self.visible.iter().enumerate() {
                let pane_id = FlexBoxId::from(Id::pane(slot));

                // Store which window is in this slot so components can read it from state.
                global_data.state.window_stack[slot] = window.clone();

                let width_pc: i32 = (*col_width as i32) * 100
                    / (global_data.window_size.col_width.as_u32().max(1) as i32);
                box_start!(
                    in: surface,
                    id: pane_id,
                    dir: LayoutDirection::Vertical,
                    requested_size_percent: req_size_pc!(width: {width_pc}, height: 100),
                    styles: [pane_id],
                );
                render_component_in_current_box!(
                    in: surface,
                    component_id: pane_id,
                    from: component_registry_map,
                    global_data: global_data,
                    has_focus: has_focus
                );
                box_end!(in: surface);
            }

            box_end!(in: surface);
        });
    }
}

fn create_stylesheet(theme: &HelixTheme) -> CommonResult<TuiStylesheet> {
    let bg = theme.ui_bg("ui.background").unwrap_or([15, 15, 25]);
    throws_with_return!({
        tui_stylesheet! {
            new_style!(
                id: {Id::Container}
                color_bg: {tui_color!(bg[0], bg[1], bg[2])}
            ),
            new_style!(
                id: {Id::Pane0}
                padding: {0}
                color_bg: {tui_color!(bg[0], bg[1], bg[2])}
            ),
            new_style!(
                id: {Id::Pane1}
                padding: {0}
                color_bg: {tui_color!(bg[0], bg[1], bg[2])}
            ),
            new_style!(
                id: {Id::Pane2}
                padding: {0}
                color_bg: {tui_color!(bg[0], bg[1], bg[2])}
            ),
            new_style!(
                id: {Id::Pane3}
                padding: {0}
                color_bg: {tui_color!(bg[0], bg[1], bg[2])}
            ),
            new_style!(
                id: {Id::Pane4}
                padding: {0}
                color_bg: {tui_color!(bg[0], bg[1], bg[2])}
            )
        }
    })
}

pub trait WindowHints {
    fn pane_key_hints(&self) -> &'static str;
}

impl WindowHints for Window {
    fn pane_key_hints(&self) -> &'static str {
        match self {
            Window::FileNamePicker => "Esc:Close  Enter:Open",
            Window::ThemePicker => "Esc:Cancel  Enter:Save",
            Window::FilePreview(_) => "Esc:Send to back  ::Command",
            Window::Terminal(_) => "",
        }
    }
}

fn render_status_bar(
    pipeline: &mut RenderPipeline,
    size: Size,
    focused_window: Option<&Window>,
    leader_active: bool,
    theme: &HelixTheme,
) {
    let bg_rgb = theme.ui_bg("ui.statusline").unwrap_or([30, 30, 50]);
    let fg_rgb = theme.ui_fg("ui.statusline").unwrap_or([180, 180, 220]);
    let color_bg = tui_color!(bg_rgb[0], bg_rgb[1], bg_rgb[2]);
    let color_fg = tui_color!(fg_rgb[0], fg_rgb[1], fg_rgb[2]);

    let leader_style = new_style!(bold color_fg: {color_fg} color_bg: {color_bg});
    let normal_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});

    let (leader_text, rest_text) = if leader_active {
        (
            " Leader ".to_string(),
            "f:Picker  t:Term  T:Theme  x:Close  q:Quit  Tab:Next  Shift+Tab:Prev  Esc:Cancel"
                .to_string(),
        )
    } else {
        let pane = match focused_window {
            Some(w) => w.pane_key_hints(),
            None => "",
        };
        let mut rest = String::new();
        if !pane.is_empty() {
            rest.push_str("  ");
            rest.push_str(pane);
        }
        (" Alt+`: Leader ".to_string(), rest)
    };

    let styled_texts = tui_styled_texts! {
        tui_styled_text! {
            @style: leader_style,
            @text: leader_text
        },
        tui_styled_text! {
            @style: normal_style,
            @text: rest_text
        },
    };

    let row_idx = size.row_height.convert_to_index();
    let mut render_ops = RenderOpIRVec::new();
    render_ops += RenderOpCommon::MoveCursorPositionAbs(col(0) + row_idx);
    render_ops += RenderOpCommon::ResetColor;
    render_ops += RenderOpCommon::SetBgColor(color_bg);
    render_ops += RenderOpIR::PaintTextWithAttributes(
        SPACER_GLYPH.repeat(size.col_width.as_usize()).into(),
        None,
    );
    render_ops += RenderOpCommon::MoveCursorPositionAbs(col(0) + row_idx);
    render_tui_styled_texts_into(&styled_texts, &mut render_ops);
    pipeline.push(ZOrder::Normal, render_ops);
}

fn render_pane_title(
    mut render_ops: &mut RenderOpIRVec,
    pane_box: &FlexBox,
    title: &str,
    is_deleted: bool,
    theme: &HelixTheme,
    focused: bool,
) {
    let origin = pane_box.style_adjusted_origin_pos;
    let width = pane_box.style_adjusted_bounds_size.col_width.as_usize();

    let (bg_active_rgb, fg_active_rgb) = (
        theme.ui_bg("ui.selection").unwrap_or([50, 50, 90]),
        theme.ui_fg("ui.text").unwrap_or([220, 220, 255]),
    );
    let bg_inactive_rgb = theme.ui_bg("ui.statusline").unwrap_or([30, 30, 50]);
    let fg_inactive_rgb = theme.ui_fg("ui.statusline").unwrap_or([180, 180, 220]);
    let fg_deleted_rgb = theme.ui_fg("error").unwrap_or([220, 80, 80]);

    let color_bg_active = tui_color!(bg_active_rgb[0], bg_active_rgb[1], bg_active_rgb[2]);
    let color_fg_active = tui_color!(fg_active_rgb[0], fg_active_rgb[1], fg_active_rgb[2]);
    let color_bg_inactive = tui_color!(bg_inactive_rgb[0], bg_inactive_rgb[1], bg_inactive_rgb[2]);
    let color_fg_inactive = tui_color!(fg_inactive_rgb[0], fg_inactive_rgb[1], fg_inactive_rgb[2]);
    let color_fg_deleted = tui_color!(fg_deleted_rgb[0], fg_deleted_rgb[1], fg_deleted_rgb[2]);

    let color_bg = if focused {
        color_bg_active
    } else {
        color_bg_inactive
    };
    let color_fg = if is_deleted {
        color_fg_deleted
    } else if focused {
        color_fg_active
    } else {
        color_fg_inactive
    };

    let padded = format!(" {title} ");
    let display = if padded.len() > width {
        let truncated = &padded[..width.saturating_sub(1)];
        format!("{truncated}…")
    } else {
        padded
    };

    render_ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
    render_ops += RenderOpCommon::ResetColor;
    render_ops += RenderOpCommon::SetBgColor(color_bg);
    render_ops += RenderOpIR::PaintTextWithAttributes(SPACER_GLYPH.repeat(width).into(), None);
    render_ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
    render_ops += RenderOpIR::PaintTextWithAttributes(
        display.into(),
        Some(if focused {
            new_style!(bold color_fg: {color_fg} color_bg: {color_bg})
        } else {
            new_style!(color_fg: {color_fg} color_bg: {color_bg})
        }),
    );
}

fn render_scrollbar(
    render_ops: &mut RenderOpIRVec,
    content_box: &FlexBox,
    scroll: usize,
    scroll_max: usize,
    page_size: usize,
    theme: &HelixTheme,
) {
    let visible_rows = content_box.style_adjusted_bounds_size.row_height.as_usize();
    if visible_rows == 0 {
        return;
    }

    let origin = content_box.style_adjusted_origin_pos;
    let scroll_col =
        (content_box.style_adjusted_bounds_size.col_width.as_usize()).saturating_sub(1);

    // Track: theme scrollbar bg -> virtual ruler -> default.
    let track_rgb = theme
        .ui_bg("ui.menu.scroll")
        .or_else(|| theme.ui_bg("ui.virtual.ruler"))
        .unwrap_or([30, 30, 50]);

    // Thumb: theme scrollbar fg -> selection -> cursorline -> default.
    let mut thumb_rgb = theme
        .ui_fg("ui.menu.scroll")
        .or_else(|| theme.ui_bg("ui.selection"))
        .or_else(|| theme.ui_bg("ui.cursorline.primary"))
        .or_else(|| theme.ui_bg("ui.cursorline"))
        .unwrap_or([50, 50, 90]);

    // If thumb and track are the same, force contrast using cursor/text accent.
    if thumb_rgb == track_rgb {
        thumb_rgb = theme
            .ui_bg("ui.cursor")
            .or_else(|| theme.ui_fg("ui.text.focus"))
            .or_else(|| theme.ui_fg("ui.text"))
            .unwrap_or([120, 120, 160]);
    }
    // Last resort: if every theme fallback still collides, hardcode contrast.
    if thumb_rgb == track_rgb {
        thumb_rgb = [120, 120, 160];
    }
    let track_bg = tui_color!(track_rgb[0], track_rgb[1], track_rgb[2]);
    let thumb_bg = tui_color!(thumb_rgb[0], thumb_rgb[1], thumb_rgb[2]);

    // Double the vertical resolution using half-block characters.
    let sub_rows = visible_rows * 2;
    let sub_thumb = std::cmp::max(1, (sub_rows * page_size) / scroll_max.max(1));
    let sub_thumb_start = if scroll_max <= page_size {
        0
    } else {
        (scroll * (sub_rows - sub_thumb)) / (scroll_max - page_size)
    };

    for row_offset in 0..visible_rows {
        let sub_top = row_offset * 2;
        let sub_bot = row_offset * 2 + 1;

        let in_top = sub_top >= sub_thumb_start && sub_top < sub_thumb_start + sub_thumb;
        let in_bot = sub_bot >= sub_thumb_start && sub_bot < sub_thumb_start + sub_thumb;

        let (ch, bg) = match (in_top, in_bot) {
            (true, true) => ('█', thumb_bg),
            (true, false) => ('▀', thumb_bg),
            (false, true) => ('▄', thumb_bg),
            (false, false) => (' ', track_bg),
        };

        let style = new_style!(color_fg: {bg} color_bg: {track_bg});
        *render_ops += RenderOpCommon::MoveCursorPositionRelTo(
            origin,
            col(scroll_col as u16) + row(row_offset),
        );
        *render_ops += RenderOpCommon::ApplyColors(Some(style));
        *render_ops += RenderOpIR::PaintTextWithAttributes(ch.to_string().into(), Some(style));
    }
}

pub fn build_state(
    files: Arc<ArcSwap<Vec<LoadedFile>>>,
    root: Utf8PathBuf,
    theme: crate::tui::theme::HelixTheme,
) -> AppState {
    AppState::new(files, root, theme)
}

pub async fn run(
    initial_state: AppState,
    files: Arc<ArcSwap<Vec<LoadedFile>>>,
    root: Utf8PathBuf,
) -> CommonResult<()> {
    let exit_tx: Arc<OnceLock<mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>>> =
        Arc::new(OnceLock::new());
    let exit_message: Arc<OnceLock<&'static str>> = Arc::new(OnceLock::new());

    // Send Exit to the TUI event loop on SIGTERM/SIGINT so RawMode::end() runs cleanly.
    for (kind, message) in [
        (
            tokio::signal::unix::SignalKind::terminate(),
            "MUST TERMINATE ALL HUMANS",
        ),
        (
            tokio::signal::unix::SignalKind::interrupt(),
            "How DARE you interrupt me!",
        ),
    ] {
        let exit_tx_signal = Arc::clone(&exit_tx);
        let exit_message_signal = Arc::clone(&exit_message);
        tokio::spawn(async move {
            if tokio::signal::unix::signal(kind)
                .expect("failed to register signal handler")
                .recv()
                .await
                .is_some()
            {
                let _ = exit_message_signal.set(message);
                if let Some(tx) = exit_tx_signal.get() {
                    let _ = tx.send(TerminalWindowMainThreadSignal::Exit).await;
                }
            }
        });
    }

    let app = AppMain::new_boxed(files, root, exit_tx);
    let exit_keys: &[InputEvent] = &[];
    let (global_data, _, _): (GlobalData<_, _>, _, _) =
        match TerminalWindow::main_event_loop(app, exit_keys, initial_state) {
            TuiAvailability::Available(future) => future.await?,
            it => return it.into_err(),
        };

    // Kill all terminal children gracefully.
    for pane in global_data.state.terminal_panes.values() {
        let mut pane = pane.lock().unwrap();
        if let Some(mut killer) = pane.child_killer.take() {
            let _ = killer.kill(); // Sends SIGHUP.
        }
        let _ = pane.pty_input_tx.try_send(PtyInputEvent::Close);
    }
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    if let Some(msg) = exit_message.get() {
        eprintln!("{msg}");
    }
    ok!()
}
