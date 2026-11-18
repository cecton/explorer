#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

#[derive(Clone, Debug)]
pub struct FuzzyPickerState<T> {
    pub query: crate::tui::widgets::input_line::InputLine,
    /// The candidates the query kept, best match first. Only the keys: the chars a row
    /// highlights are matched for the rows a frame paints, not for all of them.
    pub results: Vec<T>,
    pub selected: Option<T>,
}

// Manual `Default` avoids the `T: Default` bound the derive would add; the
// picker's fields (`InputLine`, `Vec`, `Option`) are all `Default` for any `T`.
impl<T> Default for FuzzyPickerState<T> {
    fn default() -> Self {
        use crate::tui::widgets::input_line::InputLine;
        Self {
            query: InputLine::new(),
            results: Vec::new(),
            selected: None,
        }
    }
}

impl<T: Clone + PartialEq> FuzzyPickerState<T> {
    pub fn reset(&mut self) {
        self.results.clear();
        self.selected = None;
        self.query.clear();
    }

    pub fn resolve_selected_index(&self) -> usize {
        let key = match &self.selected {
            None => return 0,
            Some(k) => k,
        };
        self.results
            .iter()
            .position(|result_key| result_key == key)
            .unwrap_or(0)
    }
}

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
        picker_state: &mut FuzzyPickerState<K>,
    ) -> Option<EventPropagation> {
        let count = picker_state.results.len();
        if count == 0 {
            return None;
        }

        let current = picker_state.resolve_selected_index();

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
        theme: &crate::tui::theme::HelixTheme,
        picker_state: &FuzzyPickerState<K>,
        results_origin: VPPos,
        result_rows: usize,
        pane_width: usize,
        display: impl Fn(&K) -> (String, Option<crate::tui::theme::Color>),
    ) -> RenderOpIRVec {
        let ui = &theme.ui;
        let color_bg = ui.background;
        let color_match_fg = ui.matched;
        let color_normal_fg = ui.text;
        let color_selected_bg = ui.selection;

        let mut render_ops = RenderOpIRVec::new();

        let selected_idx = picker_state.resolve_selected_index();
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
            let row_bg_style = color_normal_fg.on(row_bg);

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
                        Some(color_normal_fg.on(color_bg)),
                    );
                }
                continue;
            }

            render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                results_origin,
                vp_col(0) + vp_row(row_offset),
            );

            let (display_str, accent) = display(&picker_state.results[result_idx]);
            // Rows that supply an accent color (e.g. terminals) render in that
            // color and bold; others use the normal text color.
            let base_fg = accent.unwrap_or(color_normal_fg);
            let emphasized = accent.is_some();
            self.query.match_indices(&display_str, &mut indices);

            let style_of = |char_idx: usize| {
                let is_match = crate::tui::widgets::fuzzy::matched(&indices, char_idx);
                let fg = if is_match { color_match_fg } else { base_fg };
                // Bold for fuzzy-match characters and for accented rows;
                // selection is carried by `row_bg`.
                if is_match || emphasized {
                    fg.on_bold(row_bg)
                } else {
                    fg.on(row_bg)
                }
            };
            crate::tui::widgets::fuzzy::paint_runs(&mut render_ops, &display_str, style_of);
        }

        render_ops
    }
}
