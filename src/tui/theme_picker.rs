#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

pub struct ThemePickerComponent {
    id: FlexBoxId,
    picker: crate::tui::fuzzy_picker::FuzzyPicker,
}

impl ThemePickerComponent {
    pub fn new(id: FlexBoxId) -> Self {
        use crate::tui::fuzzy_picker::FuzzyPicker;
        Self {
            id,
            picker: FuzzyPicker::new(),
        }
    }
}

impl crate::tui::title_row::TitleRow for ThemePickerComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        state: &mut crate::tui::state::AppState,
    ) -> usize {
        let origin = pane_box.style_adjusted_bounds.origin_pos;
        let width = pane_box
            .style_adjusted_bounds
            .bounds_size
            .col_width
            .as_u16();
        let max_rows = (pane_box
            .style_adjusted_bounds
            .bounds_size
            .row_height
            .as_usize()
            / 2)
        .max(1);
        let height = state.theme_picker.query.height(None, "", width, max_rows);

        let (bg_rgb, fg_rgb) = crate::tui::title_row::title_bar_colors(focused, &state.theme);
        let color_bg = TuiColor::from(bg_rgb);
        let color_fg = TuiColor::from(fg_rgb);
        let color_selection = TuiColor::from(crate::tui::title_row::title_selection_bg(
            focused,
            &state.theme,
        ));
        let bg_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});
        let query = &mut state.theme_picker.query;

        for row_offset in 0..height {
            *ops += RenderOpCommon::MoveCursorPositionRelTo(
                origin,
                vp_col(0) + vp_row(row_offset as u16),
            );
            *ops += RenderOpCommon::SetBgColor(color_bg);
            *ops += RenderOpIR::PaintTextWithAttributes(
                " ".repeat(width as usize).as_str().into(),
                Some(bg_style),
            );
        }
        query.render(
            ops,
            None,
            "",
            origin,
            width,
            height,
            focused,
            (color_bg, color_fg, color_selection),
        );

        height
    }
}

fn run_theme_name_match(query: &str) -> Vec<(String, Vec<u32>)> {
    use crate::tui::theme::HelixTheme;
    let pattern = nucleo::pattern::Pattern::parse(
        query,
        nucleo::pattern::CaseMatching::Smart,
        nucleo::pattern::Normalization::Smart,
    );

    if pattern.atoms.is_empty() {
        return HelixTheme::theme_names()
            .map(|n| (n.to_string(), vec![]))
            .collect();
    }

    let mut matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(String, u32, Vec<u32>)> = HelixTheme::theme_names()
        .filter_map(|name| {
            let haystack = nucleo::Utf32Str::new(name, &mut buf);
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

impl Component<crate::tui::state::AppState, crate::tui::state::AppSignal> for ThemePickerComponent {
    fn reset(&mut self) {}

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::tui::state::AppSignal>,
        input_event: InputEvent,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        use crate::tui::pane_manager::Window;
        use crate::tui::theme::HelixTheme;
        match &input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }) => {
                let state = &mut global_data.state;
                state.theme = state.saved_theme.clone();
                state.pane_manager.remove_window(&Window::ThemePicker);
                state.theme_picker.reset();
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Enter),
            }) => {
                let state = &mut global_data.state;
                state.saved_theme = state.theme.clone();
                state.mark_session_dirty();
                state.pane_manager.remove_window(&Window::ThemePicker);
                state.theme_picker.reset();
                return Ok(EventPropagation::ConsumedRender);
            }
            _ => {}
        }

        if global_data
            .state
            .theme_picker
            .query
            .handle_key(&input_event)
        {
            let query = global_data.state.theme_picker.query.text().to_string();
            global_data.state.theme_picker.results = run_theme_name_match(&query);
            if let Some((name, _)) = global_data.state.theme_picker.results.first() {
                global_data.state.theme_picker.selected = Some(name.clone());
                if let Some(theme) = HelixTheme::from_name(name) {
                    global_data.state.theme = theme;
                }
            }
            return Ok(EventPropagation::ConsumedRender);
        }

        let page_size = global_data
            .state
            .pane_manager
            .window_page_size(&Window::ThemePicker);
        if let Some(result) = self.picker.handle_navigation(
            &input_event,
            page_size,
            &mut global_data.state.theme_picker,
        ) {
            if let Some(ref name) = global_data.state.theme_picker.selected
                && let Some(theme) = HelixTheme::from_name(name)
            {
                global_data.state.theme = theme;
            }
            return Ok(result);
        }

        Ok(EventPropagation::Propagate)
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::tui::state::AppSignal>,
        current_box: FlexBox,
        _surface_bounds: VPBoundingBox,
        _has_focus: &mut HasFocus,
    ) -> CommonResult {
        use crate::tui::pane_manager::Window;
        throws!({
            let origin = current_box.style_adjusted_bounds.origin_pos;
            let bounds = current_box.style_adjusted_bounds.bounds_size;
            let total_rows = bounds.row_height.as_usize();
            let pane_width = bounds.col_width.as_usize();

            if total_rows == 0 {
                return Ok(());
            }

            let result_ops = self.picker.render_results(
                &global_data.state,
                &global_data.state.theme_picker,
                origin,
                total_rows,
                pane_width,
                |name, _state| (name.clone(), None),
            );
            let result_count = global_data.state.theme_picker.results.len();

            global_data
                .state
                .pane_manager
                .set_window_scroll(&Window::ThemePicker, self.picker.scroll_offset);
            global_data
                .state
                .pane_manager
                .set_window_scroll_max(&Window::ThemePicker, result_count);
            global_data
                .state
                .pane_manager
                .set_window_page_size(&Window::ThemePicker, total_rows);

            global_data.pipeline.push(ZOrder::Normal, result_ops);
        });
    }
}
