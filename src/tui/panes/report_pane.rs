#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

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

    fn active_report(&self, state: &crate::tui::state::AppState) -> Option<usize> {
        use crate::tui::pane_manager::Window;
        match state
            .pane_manager
            .window_stack
            .get(crate::tui::pane_component::pane_slot(self.id)?)?
        {
            Window::Report(id) => Some(*id),
            _ => None,
        }
    }

    pub(crate) fn title(state: &crate::tui::state::AppState, id: usize) -> String {
        state
            .reports
            .get(&id)
            .map(|report| report.title.clone())
            .unwrap_or_else(|| "report".to_string())
    }

    /// Shows `markdown` in the report window already open, or in a new one when there is
    /// none, and focuses it. The report is not saved: it lives as long as its window.
    pub(crate) fn show(
        state: &mut crate::tui::state::AppState,
        title: String,
        markdown: String,
    ) -> usize {
        use crate::tui::pane_manager::Window;
        use crate::tui::state::ReportState;
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
        crate::tui::focus::focus_window(state, window);
        // The text is new, so it is read from its top, not from where the last one was left.
        state.pane_manager.set_window_scroll(&window, 0);
        state.reports.insert(id, ReportState { title, markdown });
        id
    }
}

impl crate::tui::widgets::title_row::TitleRow for ReportComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        state: &mut crate::tui::state::AppState,
    ) -> usize {
        let Some(id) = self.active_report(state) else {
            return 0;
        };
        let title = Self::title(state, id);
        let theme = &state.theme;
        crate::tui::widgets::title_row::render_pane_title(
            ops, pane_box, &title, false, theme, focused,
        );
        1
    }
}

impl Component<crate::tui::state::AppState, crate::signal::AppSignal> for ReportComponent {
    fn reset(&mut self) {}

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        input_event: InputEvent,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        use crate::tui::pane_manager::Window;
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
                    crate::tui::focus::close_window(state, window);
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
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        current_box: FlexBox,
        _surface_bounds: VPBoundingBox,
        _has_focus: &mut HasFocus,
    ) -> CommonResult {
        use crate::tui::pane_manager::Window;
        throws!({
            let Some(id) = self.active_report(&global_data.state) else {
                return Ok(());
            };
            let state = &mut global_data.state;
            let Some(report) = state.reports.get(&id) else {
                return Ok(());
            };
            let window = Window::Report(id);
            let origin = current_box.style_adjusted_bounds.origin_pos;
            let bounds = current_box.style_adjusted_bounds.bounds_size;
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
                    vp_col(0) + vp_row(row_offset as u16),
                );
                ops += RenderOpCommon::ApplyColors(Some(blank));
                ops += RenderOpIR::PaintTextWithAttributes(" ".repeat(width).into(), Some(blank));
            }

            let lines =
                crate::tui::widgets::markdown::render(&report.markdown, &state.theme, width);
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
                    vp_col(0) + vp_row(row_offset as u16),
                );
                ops += RenderOpCommon::ApplyColors(Some(background));
                ops +=
                    RenderOpIR::PaintTextWithAttributes(" ".repeat(width).into(), Some(background));
                let mut x = 0usize;
                for span in &line.spans {
                    if x >= width {
                        break;
                    }
                    let text = if x + crate::tui::widgets::text_layout::display_width(&span.text)
                        > width
                    {
                        crate::tui::widgets::text_layout::truncate(&span.text, width - x)
                    } else {
                        span.text.clone()
                    };
                    let style = crate::tui::panes::discussion::status::span_style(span, bg);
                    ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        vp_col(x as u16) + vp_row(row_offset as u16),
                    );
                    ops += RenderOpCommon::ApplyColors(Some(style));
                    ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
                    x += crate::tui::widgets::text_layout::display_width(&span.text);
                }
            }
            global_data.pipeline.push(ZOrder::Normal, ops);
        })
    }
}

fn page_size(
    state: &crate::tui::state::AppState,
    window: &crate::tui::pane_manager::Window,
) -> usize {
    state.pane_manager.window_page_size(window).max(1)
}

fn scroll_by(
    state: &mut crate::tui::state::AppState,
    window: &crate::tui::pane_manager::Window,
    delta: isize,
) {
    let current = state.pane_manager.window_scroll(window) as isize;
    let target = (current + delta).max(0) as usize;
    state.pane_manager.set_window_scroll(window, target);
    state.pane_manager.clamp_scroll(window);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn empty_state() -> crate::tui::state::AppState {
        use crate::tui::keymap::Keymap;
        use crate::tui::state::AppState;
        use crate::tui::theme::HelixTheme;
        AppState::new(
            std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(Vec::new())),
            camino::Utf8PathBuf::from("/tmp/nowhere"),
            HelixTheme::default(),
            Keymap::default(),
        )
    }

    #[test]
    fn a_report_gets_its_own_window_and_is_replaced_in_place() {
        use crate::tui::pane_manager::Window;
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
        use crate::tui::session::Session;
        let mut state = empty_state();
        ReportComponent::show(&mut state, "t".to_string(), "m".to_string());
        let session = Session::from_state(&state);
        assert!(session.panes.is_empty(), "{:?}", session.panes.len());
    }

    fn global_data_with_a_report(
        state: crate::tui::state::AppState,
    ) -> GlobalData<crate::tui::state::AppState, crate::signal::AppSignal> {
        let (main_tx, _) = mpsc::channel(8);
        let size = vp_width(80) + vp_height(24);
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
        use crate::tui::pane_manager::Window;
        let mut state = empty_state();
        let id = ReportComponent::show(&mut state, "t".to_string(), "m".to_string());
        let mut global_data = global_data_with_a_report(state);
        let mut component = ReportComponent::new(crate::tui::pane_component::pane_id(0));

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
