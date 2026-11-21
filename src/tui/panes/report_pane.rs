#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

/// A window that shows markdown to read and nothing else: no input, no model, nothing
/// saved. A report is not part of the conversation, so it lives in no node and leaves
/// nothing behind when it closes.
pub struct ReportComponent {
    id: FlexBoxId,
}

/// The markdown of a report as last laid out, kept in its `ReportState` so a frame at the
/// same width with the same report does not render it again. Laying out a long one costs
/// milliseconds.
#[derive(Clone)]
pub(crate) struct ReportCache {
    width: usize,
    theme: String,
    lines: Vec<crate::tui::widgets::markdown::MdLine>,
    /// How many times the markdown was laid out, for the tests that pin the caching.
    #[cfg(test)]
    renders: usize,
}

impl ReportComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self { id }
    }

    /// The rendered lines of `report` at `width`, laid out only when the width or the theme
    /// moved since the last frame (a new report starts with no cache).
    fn lines<'a>(
        report: &'a mut crate::tui::state::ReportState,
        width: usize,
        theme: &crate::tui::theme::HelixTheme,
    ) -> &'a [crate::tui::widgets::markdown::MdLine] {
        let stale = report
            .cache
            .as_ref()
            .is_none_or(|cache| cache.width != width || cache.theme != theme.name());
        if stale {
            #[cfg(test)]
            let renders = report.cache.as_ref().map_or(0, |cache| cache.renders) + 1;
            report.cache = Some(ReportCache {
                width,
                theme: theme.name().to_string(),
                lines: crate::tui::widgets::markdown::render(&report.markdown, theme, width),
                #[cfg(test)]
                renders,
            });
        }
        &report.cache.as_ref().expect("just laid out").lines
    }

    fn active_report(&self, state: &crate::tui::state::AppState) -> Option<usize> {
        use crate::tui::pane_manager::Window;
        match state
            .pane_manager
            .slot_window(crate::tui::pane_component::pane_slot(self.id)?)?
        {
            Window::Report(id) => Some(id),
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
            None => state.next_id(),
        };
        let window = Window::Report(id);
        crate::tui::focus::focus_window(state, window);
        // The text is new, so it is read from its top, not from where the last one was left.
        state.pane_manager.set_window_scroll(&window, 0);
        state.reports.insert(
            id,
            ReportState {
                title,
                markdown,
                cache: None,
            },
        );
        id
    }
}

impl ReportComponent {
    pub(crate) fn render_title_row(
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
                _ => {}
            }
            if state.pane_manager.scroll_for_input(&window, &input_event) {
                return Ok(EventPropagation::ConsumedRender);
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
            let origin = current_box.style_adjusted_bounds.origin_pos;
            let bounds = current_box.style_adjusted_bounds.bounds_size;
            let rows = bounds.row_height.as_usize();
            let width = bounds.col_width.as_usize();
            if rows == 0 || width == 0 {
                return Ok(());
            }
            let Some(report) = state.reports.get_mut(&id) else {
                return Ok(());
            };
            let lines = Self::lines(report, width, &state.theme);
            let window = Window::Report(id);
            let pane_bg = state.theme.ui.background;
            let text_fg = state.theme.ui.text;

            state.pane_manager.set_window_page_size(&window, rows);
            state
                .pane_manager
                .set_window_scroll_max(&window, lines.len());
            state.pane_manager.clamp_scroll(&window);
            let scroll = state.pane_manager.window_scroll(&window);

            let mut ops = RenderOpIRVec::new();
            for row_offset in 0..rows {
                let line = lines.get(scroll + row_offset);
                crate::tui::widgets::markdown::paint_row(
                    &mut ops,
                    origin,
                    row_offset,
                    line.map_or(&[][..], |line| &line.spans),
                    &crate::tui::widgets::markdown::RowColors {
                        fg: text_fg,
                        bg: line.and_then(|line| line.bg).unwrap_or(pane_bg),
                    },
                    &crate::tui::widgets::markdown::Marks::none(pane_bg),
                    width,
                );
            }
            global_data.pipeline.push(ZOrder::Normal, ops);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_gets_its_own_window_and_is_replaced_in_place() {
        use crate::tui::pane_manager::Window;
        use crate::tui::state::AppState;
        let mut state = AppState::blank_for_test();
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
        use crate::tui::state::AppState;
        let mut state = AppState::blank_for_test();
        ReportComponent::show(&mut state, "t".to_string(), "m".to_string());
        let session = Session::from_state(&state);
        assert!(session.panes.is_empty(), "{:?}", session.panes.len());
    }

    fn key(k: Key) -> InputEvent {
        InputEvent::Keyboard(KeyPress::Plain { key: k })
    }

    /// A report is read and gone: closing its window leaves nothing in `AppState`.
    #[test]
    fn esc_closes_the_window_and_drops_the_report() {
        use crate::tui::pane_manager::Window;
        use crate::tui::state::AppState;
        let mut state = AppState::blank_for_test();
        let id = ReportComponent::show(&mut state, "t".to_string(), "m".to_string());
        let mut global_data = crate::tui::test_support::global_data(state);
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

    /// The markdown is laid out once, not on every frame that paints the report.
    #[test]
    fn a_frame_lays_the_markdown_out_again_only_when_the_report_or_the_width_moves() {
        use crate::tui::state::AppState;
        let mut state = AppState::blank_for_test();
        let id = ReportComponent::show(&mut state, "t".to_string(), "# one\n\n- a".to_string());
        let mut global_data = crate::tui::test_support::global_data(state);
        let mut component = ReportComponent::new(crate::tui::pane_component::pane_id(0));
        // The count lives in the report's own cache, so a replaced report starts it again.
        let laid_out = |state: &crate::tui::state::AppState| {
            state.reports[&id]
                .cache
                .as_ref()
                .map_or(0, |cache| cache.renders)
        };

        component
            .render(
                &mut global_data,
                frame_box(80, 24),
                VPBoundingBox::new(vp_col(0) + vp_row(0), vp_width(80) + vp_height(24)),
                &mut HasFocus::default(),
            )
            .unwrap();
        let first = laid_out(&global_data.state);
        assert_eq!(first, 1, "a frame renders the markdown");
        component
            .render(
                &mut global_data,
                frame_box(80, 24),
                VPBoundingBox::new(vp_col(0) + vp_row(0), vp_width(80) + vp_height(24)),
                &mut HasFocus::default(),
            )
            .unwrap();
        assert_eq!(
            laid_out(&global_data.state),
            first,
            "a second frame at the same width keeps the rendered lines"
        );

        component
            .render(
                &mut global_data,
                frame_box(12, 24),
                VPBoundingBox::new(vp_col(0) + vp_row(0), vp_width(12) + vp_height(24)),
                &mut HasFocus::default(),
            )
            .unwrap();
        assert_eq!(
            laid_out(&global_data.state),
            first + 1,
            "a new width lays the markdown out again"
        );

        // Back at the first width, so the replacement below is told apart by the report
        // alone.
        component
            .render(
                &mut global_data,
                frame_box(80, 24),
                VPBoundingBox::new(vp_col(0) + vp_row(0), vp_width(80) + vp_height(24)),
                &mut HasFocus::default(),
            )
            .unwrap();
        assert_eq!(
            laid_out(&global_data.state),
            first + 2,
            "the width moved back"
        );

        // A new report of the same length in the same window is a new layout, which no
        // length would tell apart.
        ReportComponent::show(
            &mut global_data.state,
            "t".to_string(),
            "# two\n\n- b".to_string(),
        );
        component
            .render(
                &mut global_data,
                frame_box(80, 24),
                VPBoundingBox::new(vp_col(0) + vp_row(0), vp_width(80) + vp_height(24)),
                &mut HasFocus::default(),
            )
            .unwrap();
        let second = laid_out(&global_data.state);
        assert_eq!(second, 1, "a replaced report is laid out again");
        component
            .render(
                &mut global_data,
                frame_box(80, 24),
                VPBoundingBox::new(vp_col(0) + vp_row(0), vp_width(80) + vp_height(24)),
                &mut HasFocus::default(),
            )
            .unwrap();
        assert_eq!(
            laid_out(&global_data.state),
            second,
            "the frame after a replacement keeps the lines it laid out"
        );
        assert_eq!(global_data.state.reports[&id].markdown, "# two\n\n- b");

        // Every span carries the color it was laid out with, so a new theme is a new layout.
        global_data.state.theme =
            crate::tui::theme::HelixTheme::from_name("adwaita-light").expect("bundled theme");
        for (expected, why) in [
            (second + 1, "a new theme lays the markdown out again"),
            (second + 1, "the same theme keeps the lines it laid out"),
        ] {
            component
                .render(
                    &mut global_data,
                    frame_box(80, 24),
                    VPBoundingBox::new(vp_col(0) + vp_row(0), vp_width(80) + vp_height(24)),
                    &mut HasFocus::default(),
                )
                .unwrap();
            assert_eq!(laid_out(&global_data.state), expected, "{why}");
        }
    }

    /// A window pushed in front moves the report to the next pane slot; the markdown it
    /// had laid out goes with it rather than staying with the slot.
    #[test]
    fn a_report_moved_to_another_slot_is_not_laid_out_again() {
        use crate::tui::pane_manager::Window;
        use crate::tui::state::AppState;
        let mut state = AppState::blank_for_test();
        let id = ReportComponent::show(&mut state, "t".to_string(), "# one\n\n- a".to_string());
        let mut global_data = crate::tui::test_support::global_data(state);
        let renders = |state: &crate::tui::state::AppState| {
            state.reports[&id]
                .cache
                .as_ref()
                .map_or(0, |cache| cache.renders)
        };
        let frame = frame_box(80, 24);

        let mut first_slot = ReportComponent::new(crate::tui::pane_component::pane_id(0));
        first_slot
            .render(
                &mut global_data,
                frame,
                frame.style_adjusted_bounds,
                &mut HasFocus::default(),
            )
            .unwrap();
        assert_eq!(renders(&global_data.state), 1);

        let other = global_data.state.next_id();
        global_data
            .state
            .pane_manager
            .push_window(Window::Discussion(other));
        assert_eq!(
            global_data.state.pane_manager.slot_window(1),
            Some(Window::Report(id))
        );
        let mut second_slot = ReportComponent::new(crate::tui::pane_component::pane_id(1));
        second_slot
            .render(
                &mut global_data,
                frame,
                frame.style_adjusted_bounds,
                &mut HasFocus::default(),
            )
            .unwrap();
        assert_eq!(
            renders(&global_data.state),
            1,
            "the report keeps its lines in its new slot"
        );
    }

    /// The box a pane of `cols` columns and `rows` rows is given, at the terminal's top left.
    fn frame_box(cols: u16, rows: u16) -> FlexBox {
        FlexBox {
            style_adjusted_bounds: VPBoundingBox::new(
                vp_col(0) + vp_row(0),
                vp_width(cols) + vp_height(rows),
            ),
            ..FlexBox::default()
        }
    }
}
