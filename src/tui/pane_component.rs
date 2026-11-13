#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

/// Dispatcher component for a single pane slot. Holds both inner component types and
/// delegates to the correct one based on the window its slot holds in `window_stack`, whose
/// box is the one `PaneManager::relayout` computed for the frame.
pub struct PaneComponent {
    pub id: FlexBoxId,
    pub slot: usize,
    picker: crate::tui::panes::pickers::window_picker::WindowPickerComponent,
    theme_picker: crate::tui::panes::pickers::theme_picker::ThemePickerComponent,
    git_log: crate::tui::panes::git_log_pane::GitLogPaneComponent,
    commit_diff: crate::tui::panes::commit_diff_pane::CommitDiffPaneComponent,
    report: crate::tui::panes::report_pane::ReportComponent,
    conversation_tree: crate::tui::panes::conversation_tree::ConversationTreeComponent,
    discussion: crate::tui::panes::discussion::DiscussionComponent,
    model_picker: crate::tui::panes::pickers::model_picker::ModelPickerComponent,
    preview: crate::tui::panes::preview::FilePreviewComponent,
    terminal: crate::tui::panes::terminal::TerminalPaneComponent,
    /// Origin row of the content area (below title bar), used for scrollbar mouse events.
    content_origin_row: u16,
    /// Total columns in the content area (full width including scrollbar column).
    content_col_count: u16,
    /// Total rows in the content area.
    content_row_count: u16,
    /// Rows the scrollbar spans from the top of the content area.
    scrollbar_row_count: u16,
    /// Origin column of the content area, used for absolute scrollbar column calculation.
    content_origin_col: u16,
    /// Whether the user is currently dragging the scrollbar thumb.
    scrollbar_dragging: bool,
    /// (scroll, rel_y) at thumb grab time (None if drag started on track).
    scrollbar_grab_state: Option<(usize, usize)>,
}

impl PaneComponent {
    pub fn new_boxed(
        slot: usize,
        id: FlexBoxId,
        picker_results_tx: tokio::sync::mpsc::Sender<
            crate::tui::panes::pickers::window_picker::PickerResultMsg,
        >,
        picker_generation: Arc<AtomicU64>,
    ) -> BoxedSafeComponent<crate::tui::state::AppState, crate::signal::AppSignal> {
        use crate::tui::panes::commit_diff_pane::CommitDiffPaneComponent;
        use crate::tui::panes::conversation_tree::ConversationTreeComponent;
        use crate::tui::panes::discussion::DiscussionComponent;
        use crate::tui::panes::git_log_pane::GitLogPaneComponent;
        use crate::tui::panes::pickers::model_picker::ModelPickerComponent;
        use crate::tui::panes::pickers::theme_picker::ThemePickerComponent;
        use crate::tui::panes::pickers::window_picker::WindowPickerComponent;
        use crate::tui::panes::preview::FilePreviewComponent;
        use crate::tui::panes::report_pane::ReportComponent;
        use crate::tui::panes::terminal::TerminalPaneComponent;
        Box::new(Self {
            id,
            slot,
            picker: WindowPickerComponent::new(id, picker_results_tx, picker_generation),
            theme_picker: ThemePickerComponent::new(id),
            git_log: GitLogPaneComponent::new(id),
            commit_diff: CommitDiffPaneComponent::new(id),
            report: ReportComponent::new(id),
            conversation_tree: ConversationTreeComponent::new(id),
            discussion: DiscussionComponent::new(id),
            model_picker: ModelPickerComponent::new(id),
            preview: FilePreviewComponent::new(id),
            terminal: TerminalPaneComponent::new(id, slot),
            content_origin_row: 0,
            content_col_count: 0,
            content_row_count: 0,
            scrollbar_row_count: 0,
            content_origin_col: 0,
            scrollbar_dragging: false,
            scrollbar_grab_state: None,
        })
    }

    fn active_window(
        &self,
        state: &crate::tui::state::AppState,
    ) -> Option<crate::tui::pane_manager::Window> {
        state.pane_manager.slot_window(self.slot)
    }

    fn handle_scrollbar(
        &mut self,
        mouse: MouseInput,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        window: &crate::tui::pane_manager::Window,
    ) -> EventPropagation {
        use crate::tui::pane_manager::Window;
        let row = mouse.pos.row_index.as_usize();
        let rel_y = row.saturating_sub(self.content_origin_row as usize);

        let state = &mut global_data.state;
        let Some((scroll, scroll_max, page_size)) = scroll_metrics(state, window) else {
            return EventPropagation::Propagate;
        };
        if matches!(window, Window::Terminal(_)) && scroll_max <= page_size {
            return EventPropagation::Propagate;
        }
        let scrollbar_height = self.scrollbar_row_count as usize;

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
                        return self.apply_scroll(state, window, target);
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
                    return self.apply_scroll(state, window, target);
                }
                EventPropagation::ConsumedRender
            }
            MouseInputKind::MouseUp(Button::Left) => {
                self.scrollbar_dragging = false;
                self.scrollbar_grab_state = None;
                EventPropagation::ConsumedRender
            }
            MouseInputKind::ScrollUp => {
                let target = scroll.saturating_sub(crate::tui::pane_manager::WHEEL_STEP);
                self.apply_scroll(state, window, target)
            }
            MouseInputKind::ScrollDown => {
                let target = scroll.saturating_add(crate::tui::pane_manager::WHEEL_STEP);
                self.apply_scroll(state, window, target)
            }
            _ => EventPropagation::ConsumedRender,
        }
    }

    fn apply_scroll(
        &mut self,
        state: &mut crate::tui::state::AppState,
        window: &crate::tui::pane_manager::Window,
        target: usize,
    ) -> EventPropagation {
        use crate::tui::pane_manager::Window;
        use crate::tui::theme::HelixTheme;
        match window {
            Window::FilePreview(_) => {
                state.pane_manager.set_window_scroll(window, target);
                state.pane_manager.clamp_scroll(window);
                state.mark_session_dirty();
                EventPropagation::ConsumedRender
            }
            Window::GitLog(_)
            | Window::CommitDiff(_)
            | Window::Report(_)
            | Window::ConversationTree
            | Window::Discussion(_) => {
                state.pane_manager.set_window_scroll(window, target);
                state.pane_manager.clamp_scroll(window);
                EventPropagation::ConsumedRender
            }
            Window::WindowPicker => {
                let scroll_max = state.pane_manager.window_scroll_max(window);
                if scroll_max == 0 {
                    return EventPropagation::ConsumedRender;
                }
                let idx = target.min(scroll_max.saturating_sub(1));
                if let Some(key) = state.window_picker.results.get(idx) {
                    state.window_picker.selected = Some(*key);
                }
                EventPropagation::ConsumedRender
            }
            Window::ThemePicker => {
                let scroll_max = state.pane_manager.window_scroll_max(window);
                if scroll_max == 0 {
                    return EventPropagation::ConsumedRender;
                }
                let idx = target.min(scroll_max.saturating_sub(1));
                if let Some(name) = state.theme_picker.results.get(idx) {
                    state.theme_picker.selected = Some(name.clone());
                    if let Some(theme) = HelixTheme::from_name(name) {
                        state.theme = theme;
                    }
                }
                EventPropagation::ConsumedRender
            }
            Window::ModelPicker => {
                let scroll_max = state.pane_manager.window_scroll_max(window);
                if scroll_max == 0 {
                    return EventPropagation::ConsumedRender;
                }
                let idx = target.min(scroll_max.saturating_sub(1));
                if let Some(key) = state.model_picker.results.get(idx) {
                    state.model_picker.selected = Some(*key);
                }
                EventPropagation::ConsumedRender
            }
            Window::Terminal(id) => {
                let Some(pane) = state.terminal_panes.get(id) else {
                    return EventPropagation::ConsumedRender;
                };
                let mut pane = pane.lock();
                let scrollback_len = pane.ofs_buf.get_history_len();
                let clamped = target.min(scrollback_len);
                pane.scroll_offset = scrollback_len.saturating_sub(clamped);
                state.terminal_grabbed = false;
                EventPropagation::ConsumedRender
            }
        }
    }
}

impl crate::tui::widgets::title_row::TitleRow for PaneComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        state: &mut crate::tui::state::AppState,
    ) -> usize {
        use crate::tui::pane_manager::Window;
        use crate::tui::widgets::title_row::TitleRow;
        let Some(window) = self.active_window(state) else {
            return 0;
        };
        match window {
            Window::WindowPicker => {
                TitleRow::render_title_row(&self.picker, ops, pane_box, focused, state)
            }
            Window::ThemePicker => {
                TitleRow::render_title_row(&self.theme_picker, ops, pane_box, focused, state)
            }
            Window::FilePreview(_) => {
                TitleRow::render_title_row(&self.preview, ops, pane_box, focused, state)
            }
            Window::Terminal(_) => {
                TitleRow::render_title_row(&self.terminal, ops, pane_box, focused, state)
            }
            Window::GitLog(_) => self.git_log.render_title_row(ops, pane_box, focused, state),
            Window::CommitDiff(_) => {
                TitleRow::render_title_row(&self.commit_diff, ops, pane_box, focused, state)
            }
            Window::ConversationTree => {
                TitleRow::render_title_row(&self.conversation_tree, ops, pane_box, focused, state)
            }
            Window::Discussion(_) => self
                .discussion
                .render_title_row(ops, pane_box, focused, state),
            Window::Report(_) => {
                TitleRow::render_title_row(&self.report, ops, pane_box, focused, state)
            }
            Window::ModelPicker => {
                TitleRow::render_title_row(&self.model_picker, ops, pane_box, focused, state)
            }
        }
    }
}

/// The scroll, content height and page size of `window`; `None` for a missing or alternate terminal.
fn scroll_metrics(
    state: &crate::tui::state::AppState,
    window: &crate::tui::pane_manager::Window,
) -> Option<(usize, usize, usize)> {
    use crate::tui::pane_manager::Window;
    if let Window::Terminal(id) = window {
        let pane = state.terminal_panes.get(id)?.lock();
        if pane.ofs_buf.get_terminal_mode().active_screen_buffer == ActiveScreenBuffer::Alternate {
            return None;
        }
        let scrollback_len = pane.ofs_buf.get_history_len();
        let buffer_height = pane.ofs_buf.get_window_size().row_height.as_usize();
        let scroll = scrollback_len.saturating_sub(pane.scroll_offset.min(scrollback_len));
        return Some((scroll, scrollback_len + buffer_height, buffer_height));
    }
    Some((
        state.pane_manager.window_scroll(window),
        state.pane_manager.window_scroll_max(window),
        state.pane_manager.window_page_size(window),
    ))
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

pub(super) fn drag_modifier_from_mouse(
    mouse: &MouseInput,
) -> Option<crate::tui::panes::preview::DragModifier> {
    use crate::tui::panes::preview::DragModifier;
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

impl Component<crate::tui::state::AppState, crate::signal::AppSignal> for PaneComponent {
    fn reset(&mut self) {
        self.picker.reset();
        self.theme_picker.reset();
        self.git_log.reset();
        self.commit_diff.reset();
        self.report.reset();
        self.conversation_tree.reset();
        self.discussion.reset();
        self.model_picker.reset();
        self.preview.reset();
        self.terminal.reset();
    }

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        input_event: InputEvent,
        has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        use crate::tui::pane_manager::Window;
        let active_window = self.active_window(&global_data.state);

        // A drag ends with the window that started it.
        if !matches!(active_window, Some(Window::FilePreview(_))) {
            self.preview.cancel_drag(&mut global_data.state);
        }
        if !matches!(active_window, Some(Window::Terminal(_))) {
            self.terminal.cancel_drag(&mut global_data.state);
        }
        if !matches!(active_window.as_ref(), Some(Window::Discussion(_))) {
            self.discussion.cancel_drag(&mut global_data.state);
        }

        // An input selection ends with anything but the mouse on its own window.
        if let Some(window) = global_data.state.input_drag {
            let selecting = global_data
                .state
                .open_input(&window)
                .is_some_and(|input| input.selecting());
            if !selecting
                || active_window != Some(window)
                || !matches!(input_event, InputEvent::Mouse(_))
            {
                global_data.state.cancel_input_drag();
            }
        }

        // The window's input takes its clicks and drags before anything else.
        if let InputEvent::Mouse(mouse) = &input_event
            && let Some(window) = active_window
            && global_data.state.input_mouse(window, mouse)
        {
            return Ok(EventPropagation::ConsumedRender);
        }

        // Check for scrollbar mouse interaction first.
        if !global_data.state.mouse_drag_active
            && let InputEvent::Mouse(mouse) = input_event
        {
            let col = mouse.pos.col_index.as_usize();
            let row = mouse.pos.row_index.as_usize();
            let origin_col = self.content_origin_col as usize;
            let origin_row = self.content_origin_row as usize;
            let col_count = self.content_col_count as usize;
            let scrollbar_col = origin_col + col_count.saturating_sub(1);
            let bottom_row = origin_row + self.scrollbar_row_count as usize;

            let in_vertical = self.scrollbar_dragging || (row >= origin_row && row < bottom_row);
            let in_horizontal = col == scrollbar_col
                || (self.scrollbar_dragging && col >= origin_col && col < origin_col + col_count);

            if self.scrollbar_row_count > 0
                && in_vertical
                && in_horizontal
                && let Some(ref window) = active_window
            {
                return Ok(self.handle_scrollbar(mouse, global_data, window));
            }
        }

        // Check for title bar range click.
        if let InputEvent::Mouse(mouse) = input_event
            && let Some(window) = active_window
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
                global_data.state.mark_session_dirty();
                return Ok(EventPropagation::ConsumedRender);
            }
        }

        if let InputEvent::Mouse(mouse) = input_event {
            let taken = match active_window {
                Some(Window::FilePreview(key)) => {
                    self.preview.handle_mouse(global_data, mouse, key)
                }
                Some(Window::Terminal(id)) => self.terminal.handle_mouse(global_data, mouse, id),
                _ => false,
            };
            if taken {
                return Ok(EventPropagation::ConsumedRender);
            }
        }

        match active_window {
            Some(Window::WindowPicker) => r3bl_tui::Component::handle_event(
                &mut self.picker,
                global_data,
                input_event,
                has_focus,
            ),
            Some(Window::ThemePicker) => r3bl_tui::Component::handle_event(
                &mut self.theme_picker,
                global_data,
                input_event,
                has_focus,
            ),
            Some(Window::FilePreview(_)) => r3bl_tui::Component::handle_event(
                &mut self.preview,
                global_data,
                input_event,
                has_focus,
            ),
            Some(Window::Terminal(_)) => r3bl_tui::Component::handle_event(
                &mut self.terminal,
                global_data,
                input_event,
                has_focus,
            ),
            Some(Window::GitLog(_)) => r3bl_tui::Component::handle_event(
                &mut self.git_log,
                global_data,
                input_event,
                has_focus,
            ),
            Some(Window::CommitDiff(_)) => r3bl_tui::Component::handle_event(
                &mut self.commit_diff,
                global_data,
                input_event,
                has_focus,
            ),
            Some(Window::ConversationTree) => r3bl_tui::Component::handle_event(
                &mut self.conversation_tree,
                global_data,
                input_event,
                has_focus,
            ),
            Some(Window::Discussion(_)) => r3bl_tui::Component::handle_event(
                &mut self.discussion,
                global_data,
                input_event,
                has_focus,
            ),
            Some(Window::Report(_)) => r3bl_tui::Component::handle_event(
                &mut self.report,
                global_data,
                input_event,
                has_focus,
            ),
            Some(Window::ModelPicker) => r3bl_tui::Component::handle_event(
                &mut self.model_picker,
                global_data,
                input_event,
                has_focus,
            ),
            None => Ok(EventPropagation::Propagate),
        }
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        current_box: FlexBox,
        surface_bounds: VPBoundingBox,
        has_focus: &mut HasFocus,
    ) -> CommonResult {
        use crate::tui::pane_manager::Window;
        use crate::tui::widgets::title_row::TitleRow;
        throws!({
            let mut title_ops = RenderOpIRVec::new();
            let focused = has_focus.get_id() == Some(self.id);
            let active_window = self.active_window(&global_data.state);
            let title_height = self.render_title_row(
                &mut title_ops,
                &current_box,
                focused,
                &mut global_data.state,
            );

            let has_scrollbar = match active_window {
                Some(Window::Terminal(id)) => global_data
                    .state
                    .with_terminal(id, |pane| {
                        pane.ofs_buf.get_terminal_mode().active_screen_buffer
                            != ActiveScreenBuffer::Alternate
                    })
                    .unwrap_or(true),
                _ => true,
            };

            let (content_box, inner_bounds) = if title_height > 0 {
                let origin =
                    current_box.style_adjusted_bounds.origin_pos + vp_height(title_height as u16);
                let bounds = current_box.style_adjusted_bounds.bounds_size.col_width
                    + (current_box.style_adjusted_bounds.bounds_size.row_height
                        - vp_height(title_height as u16));
                let scrollbar_col = if has_scrollbar {
                    bounds.col_width - vp_width(1)
                } else {
                    bounds.col_width
                };
                let inner_bounds = scrollbar_col + bounds.row_height;
                let boxed = FlexBox {
                    style_adjusted_bounds: VPBoundingBox::new(origin, bounds),
                    ..current_box
                };
                (
                    boxed,
                    FlexBox {
                        style_adjusted_bounds: VPBoundingBox::new(origin, inner_bounds),
                        ..current_box
                    },
                )
            } else {
                let bounds = current_box.style_adjusted_bounds.bounds_size;
                let scrollbar_col = if has_scrollbar {
                    bounds.col_width - vp_width(1)
                } else {
                    bounds.col_width
                };
                let inner_bounds = scrollbar_col + bounds.row_height;
                let boxed = FlexBox {
                    style_adjusted_bounds: VPBoundingBox::new(
                        current_box.style_adjusted_bounds.origin_pos,
                        bounds,
                    ),
                    ..current_box
                };
                (
                    boxed,
                    FlexBox {
                        style_adjusted_bounds: VPBoundingBox::new(
                            current_box.style_adjusted_bounds.origin_pos,
                            inner_bounds,
                        ),
                        ..current_box
                    },
                )
            };

            // Store content area geometry for scrollbar mouse event handling.
            self.content_origin_row = content_box
                .style_adjusted_bounds
                .origin_pos
                .row_index
                .as_u16();
            self.content_origin_col = content_box
                .style_adjusted_bounds
                .origin_pos
                .col_index
                .as_u16();
            self.content_col_count = content_box
                .style_adjusted_bounds
                .bounds_size
                .col_width
                .as_u16();
            self.content_row_count = content_box
                .style_adjusted_bounds
                .bounds_size
                .row_height
                .as_u16();

            // Push title ops first (display behind content).
            if title_height > 0 {
                global_data.pipeline.push(ZOrder::Normal, title_ops);
            }

            // Render active child component (pushes into global_data.pipeline).
            match active_window {
                Some(Window::Report(_)) => {
                    self.report
                        .render(global_data, inner_bounds, surface_bounds, has_focus)?
                }
                Some(Window::WindowPicker) => {
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
                        .render(global_data, inner_bounds, surface_bounds, has_focus)?
                }
                Some(Window::GitLog(_)) => {
                    self.git_log
                        .render(global_data, inner_bounds, surface_bounds, has_focus)?
                }
                Some(Window::CommitDiff(_)) => {
                    self.commit_diff
                        .render(global_data, inner_bounds, surface_bounds, has_focus)?
                }
                Some(Window::ConversationTree) => self.conversation_tree.render(
                    global_data,
                    inner_bounds,
                    surface_bounds,
                    has_focus,
                )?,
                Some(Window::Discussion(_)) => {
                    self.discussion
                        .render(global_data, content_box, surface_bounds, has_focus)?
                }
                Some(Window::ModelPicker) => self.model_picker.render(
                    global_data,
                    inner_bounds,
                    surface_bounds,
                    has_focus,
                )?,
                None => {}
            };

            // The Discussion keeps its input out of the scrollbar's column.
            let scrollbar_rows = match active_window {
                Some(Window::Discussion(_)) => self.discussion.scrollbar_rows(),
                _ => self.content_row_count as usize,
            };
            self.scrollbar_row_count = scrollbar_rows as u16;

            // Render scrollbar on the rightmost column if there's an active window.
            if let Some(window) = active_window {
                let Some((scroll, scroll_max, page_size)) =
                    scroll_metrics(&global_data.state, &window)
                else {
                    return Ok(());
                };
                let mut scrollbar_ops = RenderOpIRVec::new();
                render_scrollbar(
                    &mut scrollbar_ops,
                    &content_box,
                    scrollbar_rows,
                    scroll,
                    scroll_max,
                    page_size,
                    &global_data.state.theme,
                );
                global_data.pipeline.push(ZOrder::Normal, scrollbar_ops);
            }
        });
    }
}
fn render_scrollbar(
    render_ops: &mut RenderOpIRVec,
    content_box: &FlexBox,
    visible_rows: usize,
    scroll: usize,
    scroll_max: usize,
    page_size: usize,
    theme: &crate::tui::theme::HelixTheme,
) {
    if visible_rows == 0 {
        return;
    }

    let origin = content_box.style_adjusted_bounds.origin_pos;
    let scroll_col = (content_box
        .style_adjusted_bounds
        .bounds_size
        .col_width
        .as_usize())
    .saturating_sub(1);

    // Track: theme scrollbar bg -> virtual ruler -> default.
    let track_rgb = theme
        .ui_bg("ui.menu.scroll")
        .or_else(|| theme.ui_bg("ui.virtual.ruler"))
        .unwrap_or(crate::tui::widgets::title_row::DEFAULT_STATUSLINE_BG);

    // Thumb: theme scrollbar fg -> selection -> cursorline -> default.
    let mut thumb_rgb = theme
        .ui_fg("ui.menu.scroll")
        .or_else(|| theme.ui_bg("ui.selection"))
        .or_else(|| theme.ui_bg("ui.cursorline.primary"))
        .or_else(|| theme.ui_bg("ui.cursorline"))
        .unwrap_or(crate::tui::widgets::title_row::DEFAULT_SELECTION_BG);

    // If thumb and track are the same, force contrast using cursor/text accent.
    if thumb_rgb == track_rgb {
        thumb_rgb = theme
            .ui_bg("ui.cursor")
            .or_else(|| theme.ui_fg("ui.text.focus"))
            .or_else(|| theme.ui_fg("ui.text"))
            .unwrap_or(RgbValue::from_u8(120, 120, 160));
    }
    // Last resort: if every theme fallback still collides, hardcode contrast.
    if thumb_rgb == track_rgb {
        thumb_rgb = RgbValue::from_u8(120, 120, 160);
    }
    let track_bg = TuiColor::from(track_rgb);
    let thumb_bg = TuiColor::from(thumb_rgb);

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
            vp_col(scroll_col as u16) + vp_row(row_offset),
        );
        *render_ops += RenderOpCommon::ApplyColors(Some(style));
        *render_ops += RenderOpIR::PaintTextWithAttributes(ch.to_string().into(), Some(style));
    }
}

/// The id of the container box that holds every pane slot.
const CONTAINER_ID: u8 = 1;

/// The id of the first pane slot; each slot takes the next value.
const FIRST_PANE_ID: u8 = 2;

pub(crate) fn container_id() -> FlexBoxId {
    FlexBoxId::new(CONTAINER_ID)
}

pub(crate) fn pane_id(slot: usize) -> FlexBoxId {
    FlexBoxId::new(FIRST_PANE_ID + slot.min(crate::tui::pane_manager::MAX_PANES - 1) as u8)
}

pub(crate) fn pane_slot(id: FlexBoxId) -> Option<usize> {
    let slot = usize::from(id.inner.checked_sub(FIRST_PANE_ID)?);
    (slot < crate::tui::pane_manager::MAX_PANES).then_some(slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pane_slot_and_its_id_map_back_to_each_other() {
        for slot in 0..crate::tui::pane_manager::MAX_PANES {
            assert_eq!(pane_slot(pane_id(slot)), Some(slot));
        }
    }

    #[test]
    fn the_slot_ids_leave_room_for_the_container_and_the_columns() {
        // Column styles start at 100, so the pane slots have to stay under it.
        assert!(u8::from(container_id()) < u8::from(pane_id(0)));
        assert!(u8::from(pane_id(crate::tui::pane_manager::MAX_PANES)) < 100);
    }

    #[test]
    fn a_slot_past_the_last_pane_stays_on_the_last_pane() {
        let last = crate::tui::pane_manager::MAX_PANES - 1;
        assert_eq!(pane_id(last + 1), pane_id(last));
        assert_eq!(pane_slot(pane_id(last + 1)), Some(last));
    }

    #[test]
    fn the_container_is_no_pane_slot() {
        assert_eq!(pane_slot(container_id()), None);
    }

    /// A preview of two lines in slot 0 of an 80x24 screen, rendered once for its geometry.
    struct PreviewFixture {
        _dir: tempfile::TempDir,
        component: BoxedSafeComponent<crate::tui::state::AppState, crate::signal::AppSignal>,
        global_data: GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        origin: VPPos,
    }

    impl PreviewFixture {
        fn new() -> Self {
            use crate::loader::FileKey;
            use crate::tui::pane_manager::Window;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("sample.txt");
            std::fs::write(&path, "foo beta gamma\nsecond line here\n").unwrap();
            let file = crate::loader::LoadedFile::load(path).unwrap();
            let files = Arc::new(arc_swap::ArcSwap::from_pointee(vec![file]));
            let mut state = crate::tui::state::AppState::new(
                files,
                camino::Utf8PathBuf::from("/tmp/nowhere"),
                crate::tui::theme::HelixTheme::default(),
                crate::tui::keymap::Keymap::default(),
            );
            let window = Window::FilePreview(FileKey(0));
            state.pane_manager.push_window(window);
            state.pane_manager.focused_window = Some(window);
            let size = vp_width(80) + vp_height(24);
            state.pane_manager.relayout(size);
            let pane_box = *state.pane_manager.slot_box(0).unwrap();
            let (main_tx, _) = tokio::sync::mpsc::channel(8);
            let mut global_data = GlobalData::try_to_create_instance(
                main_tx,
                state,
                size,
                OutputDevice::new_mock().0,
                OfsBufPool::new(size),
            )
            .unwrap();
            let (tx, _) = tokio::sync::mpsc::channel(1);
            let mut component = PaneComponent::new_boxed(0, pane_id(0), tx, Arc::default());
            let origin = pane_box.style_adjusted_bounds.origin_pos;
            component
                .render(
                    &mut global_data,
                    pane_box,
                    VPBoundingBox::new(origin, size),
                    &mut HasFocus::default(),
                )
                .unwrap();
            Self {
                _dir: dir,
                component,
                global_data,
                origin,
            }
        }

        /// A mouse event on character `col` of line `line` (0-based), past the gutter.
        fn mouse(&mut self, kind: MouseInputKind, line: usize, col: usize) -> EventPropagation {
            let pos = vp_col(self.origin.col_index.as_u16() + 4 + col as u16)
                + vp_row(self.origin.row_index.as_u16() + 1 + line as u16);
            self.component
                .handle_event(
                    &mut self.global_data,
                    InputEvent::Mouse(MouseInput {
                        pos,
                        kind,
                        maybe_modifier_keys: None,
                    }),
                    &mut HasFocus::default(),
                )
                .unwrap()
        }

        fn selection(&self) -> Option<(usize, usize, usize, usize)> {
            use crate::tui::pane_manager::SelPoint;
            let sel = self.global_data.state.text_selection.as_ref()?;
            match (sel.start, sel.end) {
                (
                    SelPoint::Preview {
                        line_idx: sl,
                        byte_offset: sb,
                    },
                    SelPoint::Preview {
                        line_idx: el,
                        byte_offset: eb,
                    },
                ) => Some((sl, sb, el, eb)),
                _ => None,
            }
        }
    }

    #[test]
    fn a_click_on_a_preview_selects_its_word_and_a_drag_extends_it() {
        let mut fixture = PreviewFixture::new();
        let down = MouseInputKind::MouseDown(Button::Left);
        let drag = MouseInputKind::MouseDrag(Button::Left);
        assert!(matches!(
            fixture.mouse(down, 0, 5),
            EventPropagation::ConsumedRender
        ));
        assert_eq!(
            fixture.selection(),
            Some((0, 4, 0, 4)),
            "a click starts the selection at the start of its word"
        );
        assert!(fixture.global_data.state.mouse_drag_active);
        fixture.mouse(drag, 0, 11);
        assert_eq!(
            fixture.selection(),
            Some((0, 4, 0, 14)),
            "from the clicked word to the end of the one under the pointer"
        );
        fixture.mouse(drag, 1, 3);
        assert_eq!(
            fixture.selection(),
            Some((0, 4, 1, 15 + 3)),
            "another line: from the clicked word to the pointer"
        );
    }

    #[test]
    fn three_quick_clicks_on_a_preview_select_its_line() {
        let mut fixture = PreviewFixture::new();
        let down = MouseInputKind::MouseDown(Button::Left);
        // No MouseUp: it copies. The highlight keeps the second click off the language server.
        fixture
            .global_data
            .state
            .symbol_highlights
            .push(crate::tui::state::SymbolHighlightGroup {
                qualified_name: "beta".to_string(),
                origin_file: crate::loader::FileKey(0),
                origin_line: 0,
                origin_char: 4,
                origin_word: Some("beta".to_string()),
                origin_byte_start: Some(4),
                origin_byte_end: Some(8),
                color: RgbValue::from_u8(255, 0, 0),
                locations: Vec::new(),
                needs_rebuild: false,
            });
        fixture.mouse(down, 0, 5);
        assert_eq!(fixture.selection(), Some((0, 4, 0, 4)), "first click");
        fixture.mouse(down, 0, 5);
        assert_eq!(
            fixture.selection(),
            Some((0, 4, 0, 4)),
            "the second click leaves the selection alone"
        );
        assert!(
            fixture.global_data.state.symbol_highlights.is_empty(),
            "and removes the highlight under it"
        );
        fixture.mouse(down, 0, 5);
        assert_eq!(
            fixture.selection(),
            Some((0, 0, 0, 14)),
            "the third: the line"
        );
        assert!(fixture.global_data.state.mouse_drag_active);
    }
}
