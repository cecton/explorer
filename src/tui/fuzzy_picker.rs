use crate::tui::*;
use std::collections::HashSet;

pub struct FuzzyPicker {
    pub scroll_offset: usize,
}

impl FuzzyPicker {
    pub fn new() -> Self {
        Self { scroll_offset: 0 }
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

        let (key, _) = &picker_state.results[new];
        picker_state.selected = Some(key.clone());
        Some(EventPropagation::ConsumedRender)
    }

    pub fn render_results<K: Clone + PartialEq>(
        &mut self,
        state: &AppState,
        picker_state: &FuzzyPickerState<K>,
        results_origin: Pos,
        result_rows: usize,
        pane_width: usize,
        display: impl Fn(&K, &AppState) -> String,
    ) -> RenderOpIRVec {
        let bg_rgb = state.theme.ui_bg("ui.background").unwrap_or([15, 15, 25]);
        let color_bg = tui_color!(bg_rgb[0], bg_rgb[1], bg_rgb[2]);

        let match_rgb = state
            .theme
            .ui_fg("ui.cursor.match")
            .unwrap_or([255, 200, 60]);
        let normal_rgb = state.theme.ui_fg("ui.text").unwrap_or([170, 170, 200]);
        let selected_rgb = state.theme.ui_bg("ui.selection").unwrap_or([50, 50, 90]);
        let color_match_fg = tui_color!(match_rgb[0], match_rgb[1], match_rgb[2]);
        let color_normal_fg = tui_color!(normal_rgb[0], normal_rgb[1], normal_rgb[2]);
        let color_selected_bg = tui_color!(selected_rgb[0], selected_rgb[1], selected_rgb[2]);

        let mut render_ops = RenderOpIRVec::new();

        let selected_idx =
            Self::resolve_selected_index(&picker_state.selected, &picker_state.results);
        let result_count = picker_state.results.len();

        if selected_idx < self.scroll_offset {
            self.scroll_offset = selected_idx;
        } else if result_count > 0 && selected_idx >= self.scroll_offset + result_rows {
            self.scroll_offset = selected_idx + 1 - result_rows;
        }

        for row_offset in 0..result_rows {
            let result_idx = self.scroll_offset + row_offset;
            render_ops +=
                RenderOpCommon::MoveCursorPositionRelTo(results_origin, col(0) + row(row_offset));

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
                        col(0) + row(row_offset),
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

            render_ops +=
                RenderOpCommon::MoveCursorPositionRelTo(results_origin, col(0) + row(row_offset));

            let (key, matched_positions) = {
                let (k, pos) = &picker_state.results[result_idx];
                (k.clone(), pos.clone())
            };
            let display_str = display(&key, state);
            let matched_set: HashSet<u32> = matched_positions.iter().copied().collect();

            for (char_idx, ch) in display_str.chars().enumerate() {
                let is_match = matched_set.contains(&(char_idx as u32));
                let fg = if is_match {
                    color_match_fg
                } else {
                    color_normal_fg
                };
                let style = if is_selected && is_match {
                    new_style!(bold color_fg: {fg} color_bg: {row_bg})
                } else if is_selected {
                    new_style!(color_fg: {fg} color_bg: {row_bg})
                } else if is_match {
                    new_style!(bold color_fg: {fg} color_bg: {row_bg})
                } else {
                    new_style!(color_fg: {fg} color_bg: {row_bg})
                };
                let mut buf = [0u8; 4];
                render_ops += RenderOpIR::PaintTextWithAttributes(
                    ch.encode_utf8(&mut buf).to_string().into(),
                    Some(style),
                );
            }
        }

        render_ops
    }

    fn resolve_selected_index<K: PartialEq>(
        selected: &Option<K>,
        results: &[(K, Vec<u32>)],
    ) -> usize {
        let key = match selected {
            None => return 0,
            Some(k) => k,
        };
        results
            .iter()
            .position(|(result_key, _)| result_key == key)
            .unwrap_or(0)
    }
}
