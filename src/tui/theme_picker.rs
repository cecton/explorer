use crate::tui::*;
use nucleo::Matcher;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Utf32Str};

pub struct ThemePickerComponent {
    id: FlexBoxId,
    picker: FuzzyPicker,
    input_line: InputLine,
}

impl ThemePickerComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self {
            id,
            picker: FuzzyPicker::new(),
            input_line: InputLine::new(),
        }
    }
}

impl TitleRow for ThemePickerComponent {
    fn render_title_row(
        &self,
        mut ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        theme: &HelixTheme,
        state: &AppState,
    ) -> usize {
        let origin = pane_box.style_adjusted_origin_pos;
        let width = pane_box.style_adjusted_bounds_size.col_width.as_u16();
        let query = &state.theme_picker.query;
        let height = InputLine::line_count(query);

        let (bg_rgb, fg_rgb) = title_bar_colors(focused, theme);
        let color_bg = tui_color!(bg_rgb[0], bg_rgb[1], bg_rgb[2]);
        let color_fg = tui_color!(fg_rgb[0], fg_rgb[1], fg_rgb[2]);
        let bg_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});

        for row_offset in 0..height {
            ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(row_offset as u16));
            ops += RenderOpCommon::SetBgColor(color_bg);
            ops += RenderOpIR::PaintTextWithAttributes(
                " ".repeat(width as usize).as_str().into(),
                Some(bg_style),
            );
        }
        self.input_line
            .render(ops, query, "", origin, width, focused, (color_bg, color_fg));

        height
    }
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

impl Component<AppState, AppSignal> for ThemePickerComponent {
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
        match &input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            })
            | InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('c'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        ..
                    },
            }) => {
                let state = &mut global_data.state;
                state.theme = state.saved_theme.clone();
                state.remove_window(&Window::ThemePicker);
                state.theme_picker.reset();
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Enter),
            }) => {
                let state = &mut global_data.state;
                let selected = state.theme_picker.resolve_selected_index();
                if let Some((name, _)) = state.theme_picker.results.get(selected)
                    && let Err(e) = crate::config::save_theme(name)
                {
                    tracing::error!("Failed to save theme to config: {e}");
                }
                state.saved_theme = state.theme.clone();
                state.remove_window(&Window::ThemePicker);
                state.theme_picker.reset();
                return Ok(EventPropagation::ConsumedRender);
            }
            _ => {}
        }

        if self
            .input_line
            .handle_key(&input_event, &mut global_data.state.theme_picker.query)
        {
            let query = global_data.state.theme_picker.query.clone();
            global_data.state.theme_picker.results = run_theme_name_match(&query);
            if let Some((name, _)) = global_data.state.theme_picker.results.first() {
                global_data.state.theme_picker.selected = Some(name.clone());
                if let Some(theme) = HelixTheme::from_name(name) {
                    global_data.state.theme = theme;
                }
            }
            return Ok(EventPropagation::ConsumedRender);
        }

        let page_size = global_data.state.window_page_size(&Window::ThemePicker);
        if let Some(result) = self.picker.handle_navigation(
            &input_event,
            page_size,
            &mut global_data.state.theme_picker,
        ) {
            if let Some(ref name) = global_data.state.theme_picker.selected
                && let Some(theme) = crate::tui::theme::HelixTheme::from_name(name)
            {
                global_data.state.theme = theme;
            }
            return Ok(result);
        }

        Ok(EventPropagation::Propagate)
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        current_box: FlexBox,
        _surface_bounds: SurfaceBounds,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<RenderPipeline> {
        throws_with_return!({
            let origin = current_box.style_adjusted_origin_pos;
            let bounds = current_box.style_adjusted_bounds_size;
            let total_rows = bounds.row_height.as_usize();
            let pane_width = bounds.col_width.as_usize();

            let mut pipeline = render_pipeline!();

            if total_rows == 0 {
                return Ok(pipeline);
            }

            let result_ops = self.picker.render_results(
                &global_data.state,
                &global_data.state.theme_picker,
                origin,
                total_rows,
                pane_width,
                |name, _state| name.clone(),
            );
            let result_count = global_data.state.theme_picker.results.len();

            global_data
                .state
                .set_window_scroll(&Window::ThemePicker, self.picker.scroll_offset);
            global_data
                .state
                .set_window_scroll_max(&Window::ThemePicker, result_count);
            global_data
                .state
                .set_window_page_size(&Window::ThemePicker, total_rows);

            pipeline.push(ZOrder::Normal, result_ops);
            pipeline
        });
    }
}
