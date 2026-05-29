use super::file_name_picker::FileNamePickerComponent;
use super::fuzzy_picker::resolve_selected_index;
use super::preview::FilePreviewComponent;
use super::state::{AppSignal, MAX_PANES, State, TerminalPane, Window};
use super::terminal_pane::TerminalPaneComponent;
use super::theme::HelixTheme;
use super::theme_picker::ThemePickerComponent;
use crate::loader::{FileKey, LoadedFile};
use crate::lsp::{self, LSP_RRT};
use crate::watcher::{WATCHER_RRT, set_watcher_root};
use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use nucleo::Matcher;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Utf32Str};
use r3bl_tui::core::osc::OscEvent;
use r3bl_tui::core::pty::{
    CursorKeyMode, DefaultPtySessionConfig, MouseTrackingMode, PtyInputEvent, PtyOutputEvent,
    PtySessionBuilder, PtySessionConfigOption,
};
use r3bl_tui::{
    App, BoxedSafeApp, BoxedSafeComponent, Button, CommonResult, Component, ComponentRegistry,
    ComponentRegistryMap, ContainsResult, EventPropagation, FlexBox, FlexBoxId, GlobalData,
    HasFocus, InputDevice, InputEvent, IntoErr, Key, KeyPress, LayoutDirection, LayoutManagement,
    LengthOps, ModifierKeysMask, MouseInput, MouseInputKind, OutputDevice,
    PerformPositioningAndSizing, RenderOpCommon, RenderOpIR, RenderOpIRVec, RenderPipeline,
    SPACER_GLYPH, Size, SpecialKey, Surface, SurfaceBounds, SurfaceProps, SurfaceRender,
    TerminalWindow, TerminalWindowMainThreadSignal, TuiAvailability, TuiStylesheet, ZOrder,
    box_end, box_start, col, height, new_style, ok, render_component_in_current_box,
    render_pipeline, render_tui_styled_texts_into, req_size_pc, row, send_signal, surface, throws,
    throws_with_return, tui_color, tui_styled_text, tui_styled_texts, tui_stylesheet, width,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

type PickerResultMsg = (u64, Vec<(FileKey, Vec<u32>)>);

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
}

impl PaneComponent {
    fn new_boxed(slot: usize, id: FlexBoxId) -> BoxedSafeComponent<State, AppSignal> {
        Box::new(Self {
            id,
            slot,
            picker: FileNamePickerComponent::new(id),
            theme_picker: ThemePickerComponent::new(id),
            preview: FilePreviewComponent::new(id),
            terminal: TerminalPaneComponent::new(id),
            content_origin_row: 0,
            content_col_count: 0,
            content_row_count: 0,
            content_origin_col: 0,
            scrollbar_dragging: false,
            scrollbar_grab_state: None,
        })
    }

    fn active_window<'s>(&self, state: &'s State) -> Option<&'s Window> {
        state.window_stack.get(self.slot)
    }

    fn handle_scrollbar(
        &mut self,
        mouse: MouseInput,
        global_data: &mut GlobalData<State, AppSignal>,
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
        state: &mut State,
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
                if let Some((key, _)) = state.file_name_picker_results.get(idx) {
                    state.file_name_picker_selected = Some(*key);
                }
                EventPropagation::ConsumedRender
            }
            Window::ThemePicker => {
                let scroll_max = state.window_scroll_max(window);
                if scroll_max == 0 {
                    return EventPropagation::ConsumedRender;
                }
                let idx = target.min(scroll_max.saturating_sub(1));
                if let Some((name, _)) = state.theme_picker_results.get(idx) {
                    state.theme_picker_selected = Some(name.clone());
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

impl Component<State, AppSignal> for PaneComponent {
    fn reset(&mut self) {
        self.picker.reset();
        self.theme_picker.reset();
        self.preview.reset();
        self.terminal.reset();
    }

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
        input_event: InputEvent,
        has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        let active_is_terminal = matches!(
            self.active_window(&global_data.state),
            Some(Window::Terminal(_))
        );

        // Check for scrollbar mouse interaction first.
        if !active_is_terminal && let InputEvent::Mouse(mouse) = input_event {
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
        global_data: &mut GlobalData<State, AppSignal>,
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
                let (title, is_deleted) = match active_window.as_ref().unwrap() {
                    Window::FileNamePicker => (self.picker.title_text(&global_data.state), false),
                    Window::ThemePicker => {
                        (self.theme_picker.title_text(&global_data.state), false)
                    }
                    Window::FilePreview(key) => {
                        let snapshot = global_data.state.files.load();
                        let removed = snapshot[key.0]
                            .removed
                            .load(std::sync::atomic::Ordering::Relaxed);
                        (self.preview.title_text(&global_data.state), removed)
                    }
                    Window::Terminal(id) => {
                        let title = global_data
                            .state
                            .terminal_panes
                            .get(id)
                            .and_then(|p| p.lock().ok())
                            .and_then(|g| g.title.clone())
                            .unwrap_or_else(|| format!("Terminal {}", id));
                        (title, false)
                    }
                };
                render_pane_title(
                    &mut title_ops,
                    &current_box,
                    &title,
                    is_deleted,
                    &global_data.state.theme,
                    has_focus.get_id() == Some(self.id),
                );
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
    ) -> BoxedSafeApp<State, AppSignal> {
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

    fn trigger_match(
        &self,
        query: String,
        main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
    ) {
        let generation = self.picker_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let snapshot = self.files.load_full();
        let root = self.root.clone();
        let tx = self.picker_results_tx.clone();
        let gen_counter = Arc::clone(&self.picker_generation);
        tokio::task::spawn_blocking(move || {
            let results = run_file_name_match(&query, &snapshot, &root);
            if gen_counter.load(Ordering::Relaxed) == generation {
                let _ = tx.try_send((generation, results));
                send_signal!(
                    main_tx,
                    TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::Noop)
                );
            }
        });
    }

    fn all_files_results(files: &[LoadedFile]) -> Vec<(FileKey, Vec<u32>)> {
        files
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.removed.load(Ordering::Relaxed))
            .map(|(i, _)| (FileKey(i), vec![]))
            .collect()
    }

    fn open_terminal(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
        has_focus: &mut HasFocus,
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

            let mut session = match PtySessionBuilder::new(shell_command())
                .env_var("TERM", "xterm-256color")
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
            let pane = Arc::new(Mutex::new(TerminalPane {
                ofs_buf,
                cursor_key_mode: CursorKeyMode::Normal,
                mouse_tracking_mode: MouseTrackingMode::None,
                title: None,
                pty_input_tx,
                last_size: pty_size,
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
            has_focus.set_id(focused_pane_id(state));

            EventPropagation::ConsumedRender
        });
    }
}

fn shell_command() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "bash".into())
}

fn poll_terminal_output(app: &mut AppMain, state: &mut State) {
    while let Ok((id, event)) = app.terminal_event_rx.try_recv() {
        if let PtyOutputEvent::Exit(_) = event {
            state.terminal_panes.remove(&id);
            state.remove_window(&Window::Terminal(id));
        }
    }
}

fn run_file_name_match(
    query: &str,
    files: &[LoadedFile],
    root: &Utf8PathBuf,
) -> Vec<(FileKey, Vec<u32>)> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);

    if pattern.atoms.is_empty() {
        return AppMain::all_files_results(files);
    }

    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut buf = Vec::new();
    let mut scored: Vec<(FileKey, u32, Vec<u32>)> = files
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.removed.load(Ordering::Relaxed))
        .filter_map(|(i, file)| {
            let rel = file.path.strip_prefix(root).unwrap_or(&file.path);
            let haystack = Utf32Str::new(rel.as_str(), &mut buf);
            let mut indices = Vec::new();
            pattern
                .indices(haystack, &mut matcher, &mut indices)
                .map(|score| {
                    indices.sort_unstable();
                    indices.dedup();
                    (FileKey(i), score, indices)
                })
        })
        .collect();
    scored.sort_by_key(|&(_, score, _)| std::cmp::Reverse(score));
    scored.into_iter().map(|(key, _, idx)| (key, idx)).collect()
}

fn run_theme_name_match(query: &str) -> Vec<(String, Vec<u32>)> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);

    if pattern.atoms.is_empty() {
        return HelixTheme::theme_names()
            .map(|n| (n.to_string(), vec![]))
            .collect();
    }

    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(String, u32, Vec<u32>)> = HelixTheme::theme_names()
        .filter_map(|name| {
            let haystack = Utf32Str::new(name, &mut buf);
            let mut indices = Vec::new();
            pattern
                .indices(haystack, &mut matcher, &mut indices)
                .map(|score| {
                    indices.sort_unstable();
                    indices.dedup();
                    (name.to_string(), score, indices)
                })
        })
        .collect();
    scored.sort_by_key(|&(_, score, _)| std::cmp::Reverse(score));
    scored
        .into_iter()
        .map(|(name, _, idx)| (name, idx))
        .collect()
}

impl App for AppMain {
    type S = State;
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
                    PaneComponent::new_boxed(slot, pane_id),
                );
            }
        }

        if has_focus.get_id().is_none() {
            has_focus.set_id(FlexBoxId::from(Id::Pane0));
        }
    }

    fn app_start(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
        _component_registry_map: &mut ComponentRegistryMap<State, AppSignal>,
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
        global_data: &mut GlobalData<State, AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<State, AppSignal>,
        has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        // Leader key activation.
        if let InputEvent::Keyboard(KeyPress::WithModifiers { key, mask }) = input_event
            && key == Key::Character('`')
            && mask == ModifierKeysMask::new().with_alt()
        {
            global_data.state.leader_active = true;
            return Ok(EventPropagation::ConsumedRender);
        }

        // Leader key dispatch.
        if global_data.state.leader_active {
            global_data.state.leader_active = false;
            if let InputEvent::Keyboard(keypress) = input_event {
                let key = match keypress {
                    KeyPress::Plain { key } => key,
                    KeyPress::WithModifiers { key, .. } => key,
                };
                match key {
                    Key::Character('f') => {
                        let state = &mut global_data.state;
                        state.push_window(Window::FileNamePicker);
                        state.focused_window = Some(Window::FileNamePicker);
                        state.file_name_picker_open = true;
                        state.file_name_picker_selected = None;
                        let snapshot = state.files.load();
                        state.file_name_picker_results = AppMain::all_files_results(&snapshot);
                        state.file_name_picker_query = String::new();
                        has_focus.set_id(focused_pane_id(state));
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    Key::Character('t') => {
                        return self.open_terminal(global_data, has_focus);
                    }
                    Key::Character('T') => {
                        let state = &mut global_data.state;
                        if !state.theme_picker_open {
                            state.saved_theme = state.theme.clone();
                        }
                        state.push_window(Window::ThemePicker);
                        state.focused_window = Some(Window::ThemePicker);
                        state.theme_picker_open = true;
                        let all_themes: Vec<(String, Vec<u32>)> = HelixTheme::theme_names()
                            .map(|n| (n.to_string(), vec![]))
                            .collect();
                        state.theme_picker_selected = all_themes
                            .iter()
                            .position(|(n, _)| n == state.theme.name())
                            .and_then(|i| all_themes.get(i).map(|(n, _)| n.clone()));
                        state.theme_picker_results = all_themes;
                        state.theme_picker_query = String::new();
                        has_focus.set_id(focused_pane_id(state));
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    Key::Character('q') => {
                        return Ok(EventPropagation::ExitMainEventLoop);
                    }
                    Key::SpecialKey(SpecialKey::Tab) => {
                        let state = &mut global_data.state;
                        let visible =
                            state.visible_windows(global_data.window_size.col_width.as_u16());
                        cycle_focus(state, has_focus, &visible, 1);
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    Key::SpecialKey(SpecialKey::BackTab) => {
                        let state = &mut global_data.state;
                        let visible =
                            state.visible_windows(global_data.window_size.col_width.as_u16());
                        cycle_focus(state, has_focus, &visible, -1);
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    Key::SpecialKey(SpecialKey::Esc) => {
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    _ => {}
                }
            }
        }

        if let InputEvent::Keyboard(KeyPress::Plain { key }) = input_event
            && !matches!(global_data.state.focused_window, Some(Window::Terminal(_)))
        {
            match key {
                Key::SpecialKey(SpecialKey::Tab) => {
                    let state = &mut global_data.state;
                    let visible = state.visible_windows(global_data.window_size.col_width.as_u16());
                    cycle_focus(state, has_focus, &visible, 1);
                    return Ok(EventPropagation::ConsumedRender);
                }
                Key::SpecialKey(SpecialKey::BackTab) => {
                    let state = &mut global_data.state;
                    let visible = state.visible_windows(global_data.window_size.col_width.as_u16());
                    cycle_focus(state, has_focus, &visible, -1);
                    return Ok(EventPropagation::ConsumedRender);
                }
                _ => {}
            }
        }

        if global_data.state.file_name_picker_open
            && global_data.state.focused_window == Some(Window::FileNamePicker)
            && let InputEvent::Keyboard(KeyPress::Plain { key }) = input_event
        {
            let state = &mut global_data.state;
            match key {
                Key::SpecialKey(SpecialKey::Esc) => {
                    state.remove_window(&Window::FileNamePicker);
                    state.file_name_picker_open = false;
                    state.file_name_picker_results.clear();
                    state.file_name_picker_selected = None;
                    state.file_name_picker_query = String::new();
                    has_focus.set_id(focused_pane_id(state));
                    return Ok(EventPropagation::ConsumedRender);
                }
                Key::SpecialKey(SpecialKey::Enter) => {
                    let selected = resolve_selected_index(
                        &state.file_name_picker_selected,
                        &state.file_name_picker_results,
                    );
                    if let Some(&(key, _)) = state.file_name_picker_results.get(selected) {
                        if !state.window_states.contains_key(&Window::FilePreview(key)) {
                            state.set_window_scroll(&Window::FilePreview(key), 0);
                        }
                        state.push_window(Window::FilePreview(key));
                        state.focused_window = Some(Window::FilePreview(key));
                        lsp::send_file_request(key.0);
                    }
                    state.remove_window(&Window::FileNamePicker);
                    state.file_name_picker_open = false;
                    state.file_name_picker_results.clear();
                    state.file_name_picker_selected = None;
                    state.file_name_picker_query = String::new();
                    has_focus.set_id(focused_pane_id(state));
                    return Ok(EventPropagation::ConsumedRender);
                }
                Key::SpecialKey(SpecialKey::Up) => {
                    let current = resolve_selected_index(
                        &state.file_name_picker_selected,
                        &state.file_name_picker_results,
                    );
                    let prev = current.saturating_sub(1);
                    if let Some((key, _)) = state.file_name_picker_results.get(prev) {
                        state.file_name_picker_selected = Some(*key);
                    }
                    return Ok(EventPropagation::ConsumedRender);
                }
                Key::SpecialKey(SpecialKey::Down) => {
                    let count = state.file_name_picker_results.len();
                    if count > 0 {
                        let current = resolve_selected_index(
                            &state.file_name_picker_selected,
                            &state.file_name_picker_results,
                        );
                        let next = (current + 1).min(count - 1);
                        let (key, _) = &state.file_name_picker_results[next];
                        state.file_name_picker_selected = Some(*key);
                    }
                    return Ok(EventPropagation::ConsumedRender);
                }
                _ => {}
            }
        }

        if global_data.state.file_name_picker_open
            && global_data.state.focused_window == Some(Window::FileNamePicker)
            && let InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('d'),
                mask,
            }) = input_event
            && mask == ModifierKeysMask::new().with_ctrl()
        {
            let state = &mut global_data.state;
            state.remove_window(&Window::FileNamePicker);
            state.file_name_picker_open = false;
            state.file_name_picker_results.clear();
            state.file_name_picker_selected = None;
            state.file_name_picker_query = String::new();
            has_focus.set_id(focused_pane_id(state));
            return Ok(EventPropagation::ConsumedRender);
        }

        if global_data.state.theme_picker_open
            && global_data.state.focused_window == Some(Window::ThemePicker)
            && let InputEvent::Keyboard(KeyPress::Plain { key }) = input_event
        {
            let state = &mut global_data.state;
            match key {
                Key::SpecialKey(SpecialKey::Esc) => {
                    state.theme = state.saved_theme.clone();
                    state.remove_window(&Window::ThemePicker);
                    state.theme_picker_open = false;
                    state.theme_picker_results.clear();
                    state.theme_picker_selected = None;
                    state.theme_picker_query = String::new();
                    has_focus.set_id(focused_pane_id(state));
                    return Ok(EventPropagation::ConsumedRender);
                }
                Key::SpecialKey(SpecialKey::Enter) => {
                    let selected = resolve_selected_index(
                        &state.theme_picker_selected,
                        &state.theme_picker_results,
                    );
                    if let Some((name, _)) = state.theme_picker_results.get(selected)
                        && let Err(e) = crate::config::save_theme(name)
                    {
                        tracing::error!("Failed to save theme to config: {e}");
                    }
                    state.remove_window(&Window::ThemePicker);
                    state.theme_picker_open = false;
                    state.theme_picker_results.clear();
                    state.theme_picker_selected = None;
                    state.theme_picker_query = String::new();
                    has_focus.set_id(focused_pane_id(state));
                    return Ok(EventPropagation::ConsumedRender);
                }
                Key::SpecialKey(SpecialKey::Up) => {
                    let current = resolve_selected_index(
                        &state.theme_picker_selected,
                        &state.theme_picker_results,
                    );
                    let prev = current.saturating_sub(1);
                    if let Some((name, _)) = state.theme_picker_results.get(prev) {
                        state.theme_picker_selected = Some(name.clone());
                        if let Some(theme) = HelixTheme::from_name(name) {
                            state.theme = theme;
                        }
                    }
                    return Ok(EventPropagation::ConsumedRender);
                }
                Key::SpecialKey(SpecialKey::Down) => {
                    let count = state.theme_picker_results.len();
                    if count > 0 {
                        let current = resolve_selected_index(
                            &state.theme_picker_selected,
                            &state.theme_picker_results,
                        );
                        let next = (current + 1).min(count - 1);
                        let (name, _) = &state.theme_picker_results[next];
                        state.theme_picker_selected = Some(name.clone());
                        if let Some(theme) = HelixTheme::from_name(name) {
                            state.theme = theme;
                        }
                    }
                    return Ok(EventPropagation::ConsumedRender);
                }
                _ => {}
            }
        }

        if global_data.state.theme_picker_open
            && global_data.state.focused_window == Some(Window::ThemePicker)
            && let InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('d'),
                mask,
            }) = input_event
            && mask == ModifierKeysMask::new().with_ctrl()
        {
            let state = &mut global_data.state;
            state.theme = state.saved_theme.clone();
            state.remove_window(&Window::ThemePicker);
            state.theme_picker_open = false;
            state.theme_picker_results.clear();
            state.theme_picker_selected = None;
            state.theme_picker_query = String::new();
            has_focus.set_id(focused_pane_id(state));
            return Ok(EventPropagation::ConsumedRender);
        }

        if matches!(
            global_data.state.focused_window,
            Some(Window::FilePreview(_))
        ) && let InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(SpecialKey::Esc),
        }) = input_event
            && !global_data.state.command_mode_active
        {
            let state = &mut global_data.state;
            if let Some(window) = state.focused_window.clone() {
                state.send_to_back(&window);
                has_focus.set_id(focused_pane_id(state));
            }
            return Ok(EventPropagation::ConsumedRender);
        }

        if let InputEvent::Mouse(mouse) = &input_event
            && mouse.kind == MouseInputKind::MouseMove
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
                        has_focus.set_id(FlexBoxId::from(Id::pane(slot)));
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
        global_data: &mut GlobalData<State, AppSignal>,
        _component_registry_map: &mut ComponentRegistryMap<State, AppSignal>,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        throws_with_return!({
            let state = &mut global_data.state;
            match action {
                AppSignal::FileNamePickerQueryChanged => {
                    let query = state.file_name_picker_query.clone();
                    if query.is_empty() {
                        let snapshot = state.files.load();
                        state.file_name_picker_results = AppMain::all_files_results(&snapshot);
                    } else {
                        self.trigger_match(query, global_data.main_thread_channel_sender.clone());
                    }
                }
                AppSignal::ThemePickerQueryChanged => {
                    let query = state.theme_picker_query.clone();
                    state.theme_picker_results = run_theme_name_match(&query);
                    if let Some((name, _)) = state.theme_picker_results.first() {
                        state.theme_picker_selected = Some(name.clone());
                        if let Some(theme) = HelixTheme::from_name(name) {
                            state.theme = theme;
                        }
                    }
                }
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
                    if state.file_name_picker_open {
                        let query = state.file_name_picker_query.clone();
                        if query.is_empty() {
                            state.file_name_picker_results = AppMain::all_files_results(&snapshot);
                        } else {
                            self.trigger_match(
                                query,
                                global_data.main_thread_channel_sender.clone(),
                            );
                        }
                    }
                    state.bump_files_version();
                }
                AppSignal::Noop => {}
            }

            EventPropagation::ConsumedRender
        });
    }

    fn app_render(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<State, AppSignal>,
        has_focus: &mut HasFocus,
    ) -> CommonResult<RenderPipeline> {
        let current_generation = self.picker_generation.load(Ordering::Relaxed);
        while let Ok((arrived_generation, results)) = self.picker_results_rx.try_recv() {
            if arrived_generation == current_generation {
                global_data.state.file_name_picker_results = results;
            }
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
            if !focused_is_visible && let Some((front, _)) = visible.first() {
                global_data.state.focused_window = Some(front.clone());
                has_focus.set_id(FlexBoxId::from(Id::pane(0)));
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
fn focused_pane_id(state: &State) -> FlexBoxId {
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

fn cycle_focus(
    state: &mut State,
    has_focus: &mut HasFocus,
    visible: &[(Window, u16)],
    direction: i32,
) {
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
    has_focus.set_id(FlexBoxId::from(Id::pane(next_pos)));
}

struct PanesRenderer<'a> {
    visible: &'a [(Window, u16)],
}

impl SurfaceRender<State, AppSignal> for PanesRenderer<'_> {
    fn render_in_surface(
        &mut self,
        surface: &mut Surface,
        global_data: &mut GlobalData<State, AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<State, AppSignal>,
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
            Window::FileNamePicker => {
                "Esc:Close  \u{2191}\u{2193}:Select  PgUp/PgDn:Page  Enter:Open"
            }
            Window::ThemePicker => {
                "Esc:Cancel  \u{2191}\u{2193}:Select  PgUp/PgDn:Page  Enter:Save"
            }
            Window::FilePreview(_) => {
                "Esc:Send to back  \u{2191}\u{2193}/PgUp/PgDn/Home/End:Scroll  ::Command"
            }
            Window::Terminal(_) => "Alt+`:Leader  Leader+Tab:Next pane",
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
            "f:Picker  t:Term  T:Theme  q:Quit  Tab:Next  Shift+Tab:Prev  Esc:Cancel".to_string(),
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
        rest.push_str("  Tab:Switch");
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
) -> State {
    State::new(files, root, theme)
}

pub async fn run(
    initial_state: State,
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
    let _unused: (GlobalData<_, _>, InputDevice, OutputDevice) =
        match TerminalWindow::main_event_loop(app, exit_keys, initial_state) {
            TuiAvailability::Available(future) => future.await?,
            it => return it.into_err(),
        };
    if let Some(msg) = exit_message.get() {
        eprintln!("{msg}");
    }
    ok!()
}
