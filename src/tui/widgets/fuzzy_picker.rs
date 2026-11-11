#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

pub struct FuzzyPicker {
    pub scroll_offset: usize,
    /// The query the painted rows are highlighted with, kept across frames so its pattern
    /// is parsed only when the draft moves.
    query: crate::tui::widgets::fuzzy::Query,
}

impl FuzzyPicker {
    pub fn new(kind: crate::tui::widgets::fuzzy::Kind) -> Self {
        Self {
            scroll_offset: 0,
            query: crate::tui::widgets::fuzzy::Query::new(kind),
        }
    }

    pub fn handle_navigation<K: Clone + PartialEq>(
        &mut self,
        input_event: &InputEvent,
        page_size: usize,
        picker_state: &mut crate::tui::state::FuzzyPickerState<K>,
    ) -> Option<EventPropagation> {
        let count = picker_state.results.len();
        if count == 0 {
            return None;
        }

        let current = Self::resolve_selected_index(&picker_state.selected, &picker_state.results);

        let new = match input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Down),
            })
            | InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::ScrollDown,
                ..
            }) => (current + 1).min(count - 1),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Up),
            })
            | InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::ScrollUp,
                ..
            }) => current.saturating_sub(1),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::PageDown),
            }) => {
                let page = page_size.saturating_sub(1).max(1);
                (current + page).min(count - 1)
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::PageUp),
            }) => {
                let page = page_size.saturating_sub(1).max(1);
                current.saturating_sub(page)
            }
            _ => {
                return None;
            }
        };

        picker_state.selected = Some(picker_state.results[new].clone());
        Some(EventPropagation::ConsumedRender)
    }

    pub fn render_results<K: Clone + PartialEq>(
        &mut self,
        state: &crate::tui::state::AppState,
        picker_state: &crate::tui::state::FuzzyPickerState<K>,
        results_origin: VPPos,
        result_rows: usize,
        pane_width: usize,
        display: impl Fn(&K, &crate::tui::state::AppState) -> (String, Option<RgbValue>),
    ) -> RenderOpIRVec {
        let bg_rgb = state
            .theme
            .ui_bg("ui.background")
            .unwrap_or(crate::tui::widgets::title_row::DEFAULT_BG);
        let color_bg = TuiColor::from(bg_rgb);

        let match_rgb = state
            .theme
            .ui_fg("ui.cursor.match")
            .unwrap_or(RgbValue::from_u8(255, 200, 60));
        let normal_rgb = state
            .theme
            .ui_fg("ui.text")
            .unwrap_or(RgbValue::from_u8(170, 170, 200));
        let selected_rgb = state
            .theme
            .ui_bg("ui.selection")
            .unwrap_or(crate::tui::widgets::title_row::DEFAULT_SELECTION_BG);
        let color_match_fg = TuiColor::from(match_rgb);
        let color_normal_fg = TuiColor::from(normal_rgb);
        let color_selected_bg = TuiColor::from(selected_rgb);

        let mut render_ops = RenderOpIRVec::new();

        let selected_idx =
            Self::resolve_selected_index(&picker_state.selected, &picker_state.results);
        let result_count = picker_state.results.len();
        self.query.set(picker_state.query.text());
        let mut indices = Vec::new();

        if selected_idx < self.scroll_offset {
            self.scroll_offset = selected_idx;
        } else if result_count > 0 && selected_idx >= self.scroll_offset + result_rows {
            self.scroll_offset = selected_idx + 1 - result_rows;
        }

        for row_offset in 0..result_rows {
            let result_idx = self.scroll_offset + row_offset;
            render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                results_origin,
                vp_col(0) + vp_row(row_offset),
            );

            let is_selected = result_idx < result_count && result_idx == selected_idx;
            let row_bg = if is_selected {
                color_selected_bg
            } else {
                color_bg
            };
            let row_bg_style = new_style!(color_bg: {row_bg});

            render_ops += RenderOpCommon::ApplyColors(Some(row_bg_style));
            render_ops += RenderOpIR::PaintTextWithAttributes(
                " ".repeat(pane_width).as_str().into(),
                Some(row_bg_style),
            );

            if result_idx >= result_count {
                if result_count == 0 && row_offset == 0 {
                    render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                        results_origin,
                        vp_col(0) + vp_row(row_offset),
                    );
                    let msg = "No results";
                    let pad = (pane_width.saturating_sub(msg.len())) / 2;
                    let text = format!("{:pad$}{}", "", msg, pad = pad);
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        text.into(),
                        Some(new_style!(color_fg: {color_normal_fg} color_bg: {color_bg})),
                    );
                }
                continue;
            }

            render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                results_origin,
                vp_col(0) + vp_row(row_offset),
            );

            let (display_str, accent) = display(&picker_state.results[result_idx], state);
            // Rows that supply an accent color (e.g. terminals) render in that
            // color and bold; others use the normal text color.
            let base_fg = accent.map(TuiColor::from).unwrap_or(color_normal_fg);
            let emphasized = accent.is_some();
            self.query.match_indices(&display_str, &mut indices);

            let style_of = |char_idx: usize| {
                let is_match = crate::tui::widgets::fuzzy::matched(&indices, char_idx);
                let fg = if is_match { color_match_fg } else { base_fg };
                // Bold for fuzzy-match characters and for accented rows;
                // selection is carried by `row_bg`.
                if is_match || emphasized {
                    new_style!(bold color_fg: {fg} color_bg: {row_bg})
                } else {
                    new_style!(color_fg: {fg} color_bg: {row_bg})
                }
            };
            crate::tui::widgets::fuzzy::paint_runs(&mut render_ops, &display_str, style_of);
        }

        render_ops
    }

    fn resolve_selected_index<K: PartialEq>(selected: &Option<K>, results: &[K]) -> usize {
        let key = match selected {
            None => return 0,
            Some(k) => k,
        };
        results
            .iter()
            .position(|result_key| result_key == key)
            .unwrap_or(0)
    }
}
