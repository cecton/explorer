#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

/// Dispatcher component for a single pane slot. Holds both inner component types and
/// delegates to the correct one based on the window its slot holds in `window_stack`, whose
/// box is the one `PaneManager::relayout` computed for the frame.
pub struct PaneComponent {
    pub id: FlexBoxId,
    pub slot: usize,
    git_log: crate::tui::panes::git_log_pane::GitLogPaneComponent,
    commit_diff: crate::tui::panes::commit_diff_pane::CommitDiffPaneComponent,
    report: crate::tui::panes::report_pane::ReportComponent,
    conversation_tree: crate::tui::panes::conversation_tree::ConversationTreeComponent,
    discussion: crate::tui::panes::discussion::DiscussionComponent,
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
    ) -> BoxedSafeComponent<crate::tui::state::AppState, crate::signal::AppSignal> {
        use crate::tui::panes::commit_diff_pane::CommitDiffPaneComponent;
        use crate::tui::panes::conversation_tree::ConversationTreeComponent;
        use crate::tui::panes::discussion::DiscussionComponent;
        use crate::tui::panes::git_log_pane::GitLogPaneComponent;
        use crate::tui::panes::preview::FilePreviewComponent;
        use crate::tui::panes::report_pane::ReportComponent;
        use crate::tui::panes::terminal::TerminalPaneComponent;
        Box::new(Self {
            id,
            slot,
            git_log: GitLogPaneComponent::new(id),
            commit_diff: CommitDiffPaneComponent::new(id),
            report: ReportComponent::new(id),
            conversation_tree: ConversationTreeComponent::new(id),
            discussion: DiscussionComponent::new(id),
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

impl PaneComponent {
    pub(crate) fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        state: &mut crate::tui::state::AppState,
    ) -> usize {
        use crate::tui::pane_manager::Window;
        let Some(window) = self.active_window(state) else {
            return 0;
        };
        match window {
            Window::FilePreview(_) => self.preview.render_title_row(ops, pane_box, focused, state),
            Window::Terminal(_) => self
                .terminal
                .render_title_row(ops, pane_box, focused, state),
            Window::GitLog(_) => self.git_log.render_title_row(ops, pane_box, focused, state),
            Window::CommitDiff(_) => self
                .commit_diff
                .render_title_row(ops, pane_box, focused, state),
            Window::ConversationTree => self
                .conversation_tree
                .render_title_row(ops, pane_box, focused, state),
            Window::Discussion(_) => self
                .discussion
                .render_title_row(ops, pane_box, focused, state),
            Window::Report(_) => self.report.render_title_row(ops, pane_box, focused, state),
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

pub(super) fn thumb_size(scrollbar_height: usize, page_size: usize, scroll_max: usize) -> usize {
    std::cmp::max(1, (scrollbar_height * page_size) / scroll_max.max(1))
}

pub(super) fn thumb_position(
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

impl Component<crate::tui::state::AppState, crate::signal::AppSignal> for PaneComponent {
    fn reset(&mut self) {}

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
                crate::tui::panes::preview::scroll_to_ranges(
                    &mut global_data.state,
                    key,
                    &[(lo, hi)],
                );
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
                None => {}
            };

            // The Discussion keeps its input out of the scrollbar's column.
            let scrollbar_rows = match active_window {
                Some(Window::Discussion(_)) => self.discussion.scrollbar_rows(),
                _ => self.content_row_count as usize,
            };
            // No scrollbar is painted where the window has no scroll metrics (a terminal on
            // its alternate screen): that column belongs to the window.
            let has_scrollbar = active_window
                .is_some_and(|window| scroll_metrics(&global_data.state, &window).is_some());
            self.scrollbar_row_count = if has_scrollbar {
                scrollbar_rows as u16
            } else {
                0
            };

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

    let track_bg = theme.ui.scroll_track;
    let thumb_bg = theme.ui.scroll_thumb;

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

        let style = bg.on(track_bg);
        *render_ops += RenderOpCommon::MoveCursorPositionRelTo(
            origin,
            vp_col(scroll_col as u16) + vp_row(row_offset),
        );
        *render_ops += RenderOpCommon::ApplyColors(Some(style));
        *render_ops += RenderOpIR::PaintTextWithAttributes(ch.to_string().into(), Some(style));
    }
}

/// The id of the first pane slot; each slot takes the next value.
const FIRST_PANE_ID: u8 = 2;

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
    fn a_slot_past_the_last_pane_stays_on_the_last_pane() {
        let last = crate::tui::pane_manager::MAX_PANES - 1;
        assert_eq!(pane_id(last + 1), pane_id(last));
        assert_eq!(pane_slot(pane_id(last + 1)), Some(last));
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
            use crate::tui::state::AppState;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("sample.txt");
            std::fs::write(&path, "foo beta gamma\nsecond line here\n").unwrap();
            let file = crate::loader::LoadedFile::load(path).unwrap();
            let mut state = AppState::for_test_with_files(
                vec![file],
                camino::Utf8PathBuf::from("/tmp/nowhere"),
            );
            let window = Window::FilePreview(FileKey(0));
            state.pane_manager.push_window(window);
            state.pane_manager.focused_window = Some(window);
            let size = vp_width(80) + vp_height(24);
            state.pane_manager.relayout(size);
            let pane_box = *state.pane_manager.slot_box(0).unwrap();
            let mut global_data = crate::tui::test_support::global_data(state);
            let mut component = PaneComponent::new_boxed(0, pane_id(0));
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
            let pos = vp_col(self.origin.col_index.as_u16() + 3 + col as u16)
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
            use crate::tui::selection::SelPoint;
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

    /// A click on the rightmost column of a full-screen program reaches the program.
    #[test]
    fn a_click_on_the_rightmost_column_of_a_full_screen_program_reaches_it() {
        use crate::tui::pane_manager::Window;
        use crate::tui::state::{AppState, TerminalPane};
        let mut ofs_buf = OfsBufVT100::new_empty(vp_width(20) + vp_height(5));
        let _unused = ofs_buf.apply_ansi_bytes("\x1b[?1049h\x1b[?1000h\x1b[?1006h");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let pane = TerminalPane {
            ofs_buf,
            title: None,
            pty_input_tx: std::sync::Arc::new(tx),
            child_killer: None,
            last_size: vp_width(20) + vp_height(5),
            is_command_pane: false,
            exited: false,
            exit_code: None,
            exit_signal: None,
            scroll_offset: 0,
            cwd: camino::Utf8PathBuf::from("/"),
            command: None,
            flush_pending: Default::default(),
        };
        let mut state = AppState::for_test(camino::Utf8PathBuf::from("/tmp/nowhere"));
        state
            .terminal_panes
            .insert(0, std::sync::Arc::new(parking_lot::Mutex::new(pane)));
        state.pane_manager.push_window(Window::Terminal(0));
        state.pane_manager.focused_window = Some(Window::Terminal(0));
        state.terminal_grabbed = true;
        let size = vp_width(80) + vp_height(24);
        state.pane_manager.relayout(size);
        let pane_box = *state.pane_manager.slot_box(0).unwrap();
        let mut global_data = crate::tui::test_support::global_data(state);
        let mut component = PaneComponent::new_boxed(0, pane_id(0));
        let origin = pane_box.style_adjusted_bounds.origin_pos;
        let bounds = VPBoundingBox::new(origin, size);
        component
            .render(&mut global_data, pane_box, bounds, &mut HasFocus::default())
            .unwrap();

        let last_col = origin.col_index.as_u16()
            + pane_box
                .style_adjusted_bounds
                .bounds_size
                .col_width
                .as_u16()
            - 1;
        let click = InputEvent::Mouse(MouseInput {
            pos: vp_col(last_col) + vp_row(origin.row_index.as_u16() + 1),
            kind: MouseInputKind::MouseDown(Button::Left),
            maybe_modifier_keys: None,
        });
        // Rendering may have sent a resize first; only a write is the click.
        while rx.try_recv().is_ok() {}
        component
            .handle_event(&mut global_data, click, &mut HasFocus::default())
            .unwrap();
        assert!(
            matches!(rx.try_recv(), Ok(PtyInputEvent::Write(_))),
            "the click was written to the program"
        );
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
