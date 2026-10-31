use crate::tui::pane_component::pane_slot;
use crate::tui::text_layout::{display_width, truncate};
use crate::tui::*;

const SCROLL_STEP: isize = 3;

/// A window that shows markdown to read and nothing else: no input, no model, nothing
/// saved. A report is not part of the conversation, so it lives in no node and leaves
/// nothing behind when it closes.
pub struct ReportComponent {
    id: FlexBoxId,
}

impl ReportComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self { id }
    }

    fn active_report(&self, state: &AppState) -> Option<usize> {
        match state.pane_manager.window_stack.get(pane_slot(self.id)?)? {
            Window::Report(id) => Some(*id),
            _ => None,
        }
    }

    pub(crate) fn title(state: &AppState, id: usize) -> String {
        state
            .reports
            .get(&id)
            .map(|report| report.title.clone())
            .unwrap_or_else(|| "report".to_string())
    }

    /// Shows `markdown` in the report window already open, or in a new one when there is
    /// none, and focuses it. The report is not saved: it lives as long as its window.
    pub(crate) fn show(state: &mut AppState, title: String, markdown: String) -> usize {
        let existing = state
            .pane_manager
            .window_stack
            .iter()
            .find_map(|window| match window {
                Window::Report(id) => Some(*id),
                _ => None,
            });
        let id = match existing {
            Some(id) => id,
            None => {
                let id = state.next_report_id;
                state.next_report_id += 1;
                id
            }
        };
        let window = Window::Report(id);
        let old = state.pane_manager.focused_window;
        state.pane_manager.push_window(window);
        state.pane_manager.focused_window = Some(window);
        crate::tui::app::notify_terminal_focus_change(state, old, Some(window));
        crate::tui::app::sync_terminal_grabbed(state);
        // The text is new, so it is read from its top, not from where the last one was left.
        state.pane_manager.set_window_scroll(&window, 0);
        state.reports.insert(id, ReportState { title, markdown });
        id
    }
}

impl TitleRow for ReportComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        state: &mut AppState,
    ) -> usize {
        let Some(id) = self.active_report(state) else {
            return 0;
        };
        let title = Self::title(state, id);
        let theme = &state.theme;
        render_pane_title(ops, pane_box, &title, false, theme, focused);
        1
    }
}

impl Component<AppState, AppSignal> for ReportComponent {
    fn reset(&mut self) {}

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        input_event: InputEvent,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        throws_with_return!({
            let Some(id) = self.active_report(&global_data.state) else {
                return Ok(EventPropagation::Propagate);
            };
            let window = Window::Report(id);
            let state = &mut global_data.state;
            match input_event {
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Esc),
                }) => {
                    state.pane_manager.remove_window(&window);
                    state.reports.remove(&id);
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Backspace),
                }) => {
                    state.pane_manager.send_to_back(&window);
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::PageUp),
                }) => {
                    scroll_by(state, &window, -(page_size(state, &window) as isize));
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::PageDown),
                }) => {
                    scroll_by(state, &window, page_size(state, &window) as isize);
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Mouse(MouseInput {
                    kind: MouseInputKind::ScrollUp,
                    ..
                }) => {
                    scroll_by(state, &window, -SCROLL_STEP);
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Mouse(MouseInput {
                    kind: MouseInputKind::ScrollDown,
                    ..
                }) => {
                    scroll_by(state, &window, SCROLL_STEP);
                    return Ok(EventPropagation::ConsumedRender);
                }
                _ => {}
            }
            EventPropagation::Propagate
        });
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        current_box: FlexBox,
        _surface_bounds: SurfaceBounds,
        _has_focus: &mut HasFocus,
    ) -> CommonResult {
        throws!({
            let Some(id) = self.active_report(&global_data.state) else {
                return Ok(());
            };
            let state = &mut global_data.state;
            let Some(report) = state.reports.get(&id) else {
                return Ok(());
            };
            let window = Window::Report(id);
            let origin = current_box.style_adjusted_origin_pos;
            let bounds = current_box.style_adjusted_bounds_size;
            let rows = bounds.row_height.as_usize();
            let width = bounds.col_width.as_usize();
            if rows == 0 || width == 0 {
                return Ok(());
            }
            let pane_bg = state
                .theme
                .ui_bg("ui.background")
                .unwrap_or(RgbValue::from_u8(30, 30, 46));

            let blank = new_style!(color_bg: {TuiColor::from(pane_bg)});
            let mut ops = RenderOpIRVec::new();
            for row_offset in 0..rows {
                ops += RenderOpCommon::MoveCursorPositionRelTo(
                    origin,
                    col(0) + row(row_offset as u16),
                );
                ops += RenderOpCommon::ApplyColors(Some(blank));
                ops += RenderOpIR::PaintTextWithAttributes(" ".repeat(width).into(), Some(blank));
            }

            let lines = markdown::render(&report.markdown, &state.theme, width);
            state.pane_manager.set_window_page_size(&window, rows);
            state
                .pane_manager
                .set_window_scroll_max(&window, lines.len());
            state.pane_manager.clamp_scroll(&window);
            let scroll = state.pane_manager.window_scroll(&window);

            for (row_offset, line) in lines.iter().skip(scroll).take(rows).enumerate() {
                let bg = line.bg.unwrap_or(pane_bg);
                let background = new_style!(color_bg: {TuiColor::from(bg)});
                ops += RenderOpCommon::MoveCursorPositionRelTo(
                    origin,
                    col(0) + row(row_offset as u16),
                );
                ops += RenderOpCommon::ApplyColors(Some(background));
                ops +=
                    RenderOpIR::PaintTextWithAttributes(" ".repeat(width).into(), Some(background));
                let mut x = 0usize;
                for span in &line.spans {
                    if x >= width {
                        break;
                    }
                    let text = if x + display_width(&span.text) > width {
                        truncate(&span.text, width - x)
                    } else {
                        span.text.clone()
                    };
                    let style = crate::tui::discussion_pane::span_style(span, bg);
                    ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        col(x as u16) + row(row_offset as u16),
                    );
                    ops += RenderOpCommon::ApplyColors(Some(style));
                    ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
                    x += display_width(&span.text);
                }
            }
            global_data.pipeline.push(ZOrder::Normal, ops);
        })
    }
}

fn page_size(state: &AppState, window: &Window) -> usize {
    state.pane_manager.window_page_size(window).max(1)
}

fn scroll_by(state: &mut AppState, window: &Window, delta: isize) {
    let current = state.pane_manager.window_scroll(window) as isize;
    let target = (current + delta).max(0) as usize;
    state.pane_manager.set_window_scroll(window, target);
    state.pane_manager.clamp_scroll(window);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn empty_state() -> AppState {
        AppState::new(
            std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(Vec::new())),
            camino::Utf8PathBuf::from("/tmp/nowhere"),
            HelixTheme::default(),
            crate::keymap::Keymap::default(),
        )
    }

    #[test]
    fn a_report_gets_its_own_window_and_is_replaced_in_place() {
        let mut state = empty_state();
        let first = ReportComponent::show(
            &mut state,
            "Repository scan".to_string(),
            "# found".to_string(),
        );
        assert!(
            state
                .pane_manager
                .window_stack
                .contains(&Window::Report(first))
        );
        assert_eq!(
            state.pane_manager.focused_window,
            Some(Window::Report(first))
        );
        assert_eq!(state.reports[&first].markdown, "# found");
        assert_eq!(ReportComponent::title(&state, first), "Repository scan");

        let again = ReportComponent::show(
            &mut state,
            "Repository scan".to_string(),
            "# again".to_string(),
        );
        assert_eq!(again, first);
        assert_eq!(state.reports[&first].markdown, "# again");
        assert_eq!(
            state.pane_manager.window_scroll(&Window::Report(first)),
            0,
            "a replaced report is read from its top"
        );
        assert_eq!(
            state
                .pane_manager
                .window_stack
                .iter()
                .filter(|window| matches!(window, Window::Report(_)))
                .count(),
            1,
            "a second scan replaces the report instead of stacking another window"
        );
    }

    /// A report is not part of the conversation, so the session never carries one.
    #[test]
    fn a_report_is_never_saved_in_the_session() {
        let mut state = empty_state();
        ReportComponent::show(&mut state, "t".to_string(), "m".to_string());
        let session = crate::session::Session::from_state(&state);
        assert!(session.panes.is_empty(), "{:?}", session.panes.len());
    }

    fn global_data_with_a_report(state: AppState) -> GlobalData<AppState, AppSignal> {
        let (main_tx, _) = mpsc::channel(8);
        let size = width(80) + height(24);
        GlobalData::try_to_create_instance(
            main_tx,
            state,
            size,
            OutputDevice::new_mock().0,
            OfsBufPool::new(size),
        )
        .unwrap()
    }

    fn key(k: Key) -> InputEvent {
        InputEvent::Keyboard(KeyPress::Plain { key: k })
    }

    /// A report is read and gone: closing its window leaves nothing in `AppState`.
    #[test]
    fn esc_closes_the_window_and_drops_the_report() {
        let mut state = empty_state();
        let id = ReportComponent::show(&mut state, "t".to_string(), "m".to_string());
        let mut global_data = global_data_with_a_report(state);
        let mut component = ReportComponent::new(FlexBoxId::from(Id::pane(0)));

        let closed = component
            .handle_event(
                &mut global_data,
                key(Key::SpecialKey(SpecialKey::Esc)),
                &mut HasFocus::default(),
            )
            .unwrap();
        assert_eq!(closed, EventPropagation::ConsumedRender);
        assert!(!global_data.state.reports.contains_key(&id));
        assert!(
            !global_data
                .state
                .pane_manager
                .window_stack
                .contains(&Window::Report(id))
        );
    }
}
