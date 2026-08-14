use crate::tui::*;
use unicode_segmentation::UnicodeSegmentation;

pub struct InputLine {
    cursor: usize,
}

impl InputLine {
    pub fn new() -> Self {
        Self { cursor: 0 }
    }

    pub fn handle_key(&mut self, input_event: &InputEvent, query: &mut String) -> bool {
        let grapheme_count = query.graphemes(true).count();
        self.cursor = self.cursor.min(grapheme_count);

        match input_event {
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('a'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => {
                self.cursor = 0;
                true
            }
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('e'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => {
                self.cursor = grapheme_count;
                true
            }
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('d'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => self.delete_grapheme(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('b'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => {
                if self.cursor == 0 {
                    false
                } else {
                    self.cursor -= 1;
                    true
                }
            }
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('f'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => {
                if self.cursor >= grapheme_count {
                    false
                } else {
                    self.cursor += 1;
                    true
                }
            }
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('k'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => self.kill_to_end(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('c'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => {
                query.clear();
                self.cursor = 0;
                true
            }
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('u'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => self.kill_to_start(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('w'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => self.kill_word_backward(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(SpecialKey::Left),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => self.cursor_prev_word(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(SpecialKey::Right),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => self.cursor_next_word(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('b'),
                mask:
                    ModifierKeysMask {
                        alt_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        ctrl_key_state: KeyState::NotPressed,
                    },
            }) => self.cursor_prev_word(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('f'),
                mask:
                    ModifierKeysMask {
                        alt_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        ctrl_key_state: KeyState::NotPressed,
                    },
            }) => self.cursor_next_word(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('d'),
                mask:
                    ModifierKeysMask {
                        alt_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        ctrl_key_state: KeyState::NotPressed,
                    },
            }) => self.kill_word_forward(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(SpecialKey::Backspace),
                mask:
                    ModifierKeysMask {
                        alt_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        ctrl_key_state: KeyState::NotPressed,
                    },
            }) => self.kill_word_backward(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(SpecialKey::Enter),
                mask:
                    ModifierKeysMask {
                        shift_key_state: KeyState::Pressed,
                        alt_key_state: KeyState::NotPressed,
                        ctrl_key_state: KeyState::NotPressed,
                    },
            }) => {
                let byte_pos = grapheme_byte_offset(query, self.cursor);
                query.insert(byte_pos, '\n');
                self.cursor += 1;
                true
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character(ch),
            })
            | InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character(ch),
                mask:
                    ModifierKeysMask {
                        shift_key_state: KeyState::Pressed,
                        alt_key_state: KeyState::NotPressed,
                        ctrl_key_state: KeyState::NotPressed,
                    },
            }) => {
                if ch.is_control() {
                    return false;
                }
                let byte_pos = grapheme_byte_offset(query, self.cursor);
                query.insert(byte_pos, *ch);
                self.cursor += 1;
                true
            }
            InputEvent::BracketedPaste(text) => {
                if text.is_empty() {
                    return false;
                }
                let byte_pos = grapheme_byte_offset(query, self.cursor);
                query.insert_str(byte_pos, text);
                self.cursor += text.graphemes(true).count();
                true
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Backspace),
            }) => self.backspace_grapheme(query),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Delete),
            }) => self.delete_grapheme(query),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Home),
            }) => {
                if self.cursor == 0 {
                    false
                } else {
                    self.cursor = 0;
                    true
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::End),
            }) => {
                if self.cursor == grapheme_count {
                    false
                } else {
                    self.cursor = grapheme_count;
                    true
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Left),
            }) => {
                if self.cursor == 0 {
                    false
                } else {
                    self.cursor -= 1;
                    true
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Right),
            }) => {
                if self.cursor >= grapheme_count {
                    false
                } else {
                    self.cursor += 1;
                    true
                }
            }
            _ => false,
        }
    }

    fn backspace_grapheme(&mut self, query: &mut String) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let graphemes: Vec<(usize, &str)> = query.grapheme_indices(true).collect();
        let target = self.cursor - 1;
        let start = graphemes[target].0;
        let end = if target + 1 < graphemes.len() {
            graphemes[target + 1].0
        } else {
            query.len()
        };
        query.replace_range(start..end, "");
        self.cursor = target;
        true
    }

    fn delete_grapheme(&mut self, query: &mut String) -> bool {
        let graphemes: Vec<(usize, &str)> = query.grapheme_indices(true).collect();
        if self.cursor >= graphemes.len() {
            return false;
        }
        let start = graphemes[self.cursor].0;
        let end = if self.cursor + 1 < graphemes.len() {
            graphemes[self.cursor + 1].0
        } else {
            query.len()
        };
        query.replace_range(start..end, "");
        true
    }

    fn kill_to_end(&mut self, query: &mut String) -> bool {
        let graphemes: Vec<(usize, &str)> = query.grapheme_indices(true).collect();
        if self.cursor >= graphemes.len() {
            return false;
        }
        let byte = graphemes[self.cursor].0;
        query.truncate(byte);
        true
    }

    fn kill_to_start(&mut self, query: &mut String) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let end_byte = grapheme_byte_offset(query, self.cursor);
        *query = query[end_byte..].to_string();
        self.cursor = 0;
        true
    }

    fn kill_word_backward(&mut self, query: &mut String) -> bool {
        let word_start = self.prev_word_start(query);
        if word_start >= self.cursor {
            return false;
        }
        let graphemes: Vec<(usize, &str)> = query.grapheme_indices(true).collect();
        let start_byte = graphemes[word_start].0;
        let end_byte = if self.cursor < graphemes.len() {
            graphemes[self.cursor].0
        } else {
            query.len()
        };
        query.replace_range(start_byte..end_byte, "");
        self.cursor = word_start;
        true
    }

    fn cursor_prev_word(&mut self, query: &str) -> bool {
        let new = self.prev_word_start(query);
        if new == self.cursor {
            return false;
        }
        self.cursor = new;
        true
    }

    fn cursor_next_word(&mut self, query: &str) -> bool {
        let new = self.next_word_start(query);
        if new == self.cursor {
            return false;
        }
        self.cursor = new;
        true
    }

    fn is_word_boundary(grapheme: &str) -> bool {
        grapheme
            .chars()
            .all(|c| c.is_whitespace() || c.is_ascii_punctuation())
    }

    fn prev_word_start(&self, text: &str) -> usize {
        let graphemes: Vec<(usize, &str)> = text.grapheme_indices(true).collect();
        let count = graphemes.len();
        if count == 0 || self.cursor == 0 {
            return 0;
        }
        let mut idx = self.cursor.saturating_sub(1).min(count - 1);
        while idx > 0 && Self::is_word_boundary(graphemes[idx].1) {
            idx -= 1;
        }
        if idx == 0 && Self::is_word_boundary(graphemes[0].1) {
            return 0;
        }
        while idx > 0 && !Self::is_word_boundary(graphemes[idx].1) {
            idx -= 1;
        }
        if idx > 0 && Self::is_word_boundary(graphemes[idx].1) {
            idx += 1;
        }
        idx
    }

    fn next_word_start(&self, text: &str) -> usize {
        let graphemes: Vec<(usize, &str)> = text.grapheme_indices(true).collect();
        let count = graphemes.len();
        if count == 0 {
            return 0;
        }
        if self.cursor >= count {
            return count;
        }
        let mut idx = self.cursor;
        while idx < count && !Self::is_word_boundary(graphemes[idx].1) {
            idx += 1;
        }
        if idx >= count {
            return count;
        }
        while idx < count && Self::is_word_boundary(graphemes[idx].1) {
            idx += 1;
        }
        idx
    }

    fn next_word_end(&self, text: &str) -> usize {
        let graphemes: Vec<(usize, &str)> = text.grapheme_indices(true).collect();
        let count = graphemes.len();
        if count == 0 {
            return 0;
        }
        if self.cursor >= count {
            return count;
        }
        let mut idx = self.cursor;
        while idx < count && Self::is_word_boundary(graphemes[idx].1) {
            idx += 1;
        }
        if idx >= count {
            return count;
        }
        while idx < count && !Self::is_word_boundary(graphemes[idx].1) {
            idx += 1;
        }
        idx
    }

    fn kill_word_forward(&mut self, query: &mut String) -> bool {
        let word_end = self.next_word_end(query);
        if word_end <= self.cursor || word_end == 0 {
            return false;
        }
        let graphemes: Vec<(usize, &str)> = query.grapheme_indices(true).collect();
        let start_byte = if self.cursor < graphemes.len() {
            graphemes[self.cursor].0
        } else {
            query.len()
        };
        let end_byte = if word_end < graphemes.len() {
            graphemes[word_end].0
        } else {
            query.len()
        };
        query.replace_range(start_byte..end_byte, "");
        true
    }

    pub fn line_count(query: &str) -> usize {
        query.matches('\n').count() + 1
    }

    /// Places the cursor at the end of `query`. Used when opening an input prefilled with existing
    /// text (e.g. the preview search box seeded with the active pattern).
    pub fn set_cursor_end(&mut self, query: &str) {
        self.cursor = query.graphemes(true).count();
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        mut ops: &mut RenderOpIRVec,
        query: &str,
        prompt: &str,
        origin: Pos,
        width: u16,
        focused: bool,
        colors: (TuiColor, TuiColor),
    ) {
        let width = width as usize;
        let prompt_width = prompt.graphemes(true).count();
        let (color_bg, color_text) = colors;
        let text_style = new_style!(color_fg: {color_text} color_bg: {color_bg});
        // The compositor takes a painted cell's *colors* from the current pen
        // (`ApplyColors`/`SetBgColor`), not from the style handed to
        // `PaintTextWithAttributes` — so we set the pen explicitly instead of relying on
        // whatever the previously rendered pane left behind. The cursor is a solid block
        // drawn with foreground/background swapped, which stays visible at any pane
        // position (a bare `reverse` attribute inherits the stale pen and can vanish).
        let cursor_style = new_style!(color_fg: {color_bg} color_bg: {color_text});
        ops += RenderOpCommon::ApplyColors(Some(text_style));

        let lines: Vec<&str> = query.split('\n').collect();
        let line_grapheme_counts: Vec<usize> =
            lines.iter().map(|l| l.graphemes(true).count()).collect();
        let total_graphemes: usize =
            line_grapheme_counts.iter().sum::<usize>() + lines.len().saturating_sub(1);
        let cursor = self.cursor.min(total_graphemes);

        let mut cursor_line = 0usize;
        let mut cursor_col = 0usize;
        let mut remaining = cursor;
        for (i, &count) in line_grapheme_counts.iter().enumerate() {
            if remaining <= count {
                cursor_line = i;
                cursor_col = remaining;
                break;
            }
            remaining = remaining.saturating_sub(count + 1);
        }

        for (line_idx, line_text) in lines.iter().enumerate() {
            let line_graphemes: Vec<(usize, &str)> = line_text.grapheme_indices(true).collect();
            let line_gr_count = line_graphemes.len();
            let col_offset = if line_idx == 0 { prompt_width } else { 0 };
            let line_width = width.saturating_sub(col_offset);

            if line_idx == 0 && prompt_width > 0 {
                ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
                ops += RenderOpIR::PaintTextWithAttributes(prompt.into(), Some(text_style));
            }

            let scroll = if line_idx == cursor_line
                && line_gr_count >= line_width
                && cursor_col >= line_width
            {
                cursor_col - line_width + 1
            } else {
                0
            };

            ops += RenderOpCommon::MoveCursorPositionRelTo(
                origin,
                col(col_offset as u16) + row(line_idx as u16),
            );

            for (vis_col, (gi, &(_, gr))) in
                line_graphemes.iter().enumerate().skip(scroll).enumerate()
            {
                if vis_col >= line_width {
                    break;
                }
                if line_idx == cursor_line && gi == cursor_col && focused {
                    ops += RenderOpCommon::ApplyColors(Some(cursor_style));
                    ops += RenderOpIR::PaintTextWithAttributes(gr.into(), Some(cursor_style));
                    ops += RenderOpCommon::ApplyColors(Some(text_style));
                } else {
                    ops += RenderOpIR::PaintTextWithAttributes(gr.into(), Some(text_style));
                }
            }

            if focused && line_idx == cursor_line && cursor_col >= line_gr_count {
                let vis_cursor_col = col_offset + cursor_col.saturating_sub(scroll);
                if vis_cursor_col < width {
                    ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        col(vis_cursor_col as u16) + row(line_idx as u16),
                    );
                    ops += RenderOpCommon::ApplyColors(Some(cursor_style));
                    ops += RenderOpIR::PaintTextWithAttributes(" ".into(), Some(cursor_style));
                    ops += RenderOpCommon::ApplyColors(Some(text_style));
                }
            }
        }
    }
}

fn grapheme_byte_offset(text: &str, grapheme_idx: usize) -> usize {
    text.grapheme_indices(true)
        .nth(grapheme_idx)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod cursor_render_tests {
    use super::*;

    fn colors() -> (TuiColor, TuiColor) {
        // (color_bg, color_text)
        (
            TuiColor::from(RgbValue::from_u8(50, 50, 90)),
            TuiColor::from(RgbValue::from_u8(220, 220, 255)),
        )
    }

    /// Returns the `ApplyColors` style immediately preceding the first
    /// `PaintTextWithAttributes(text)` op, or `None`.
    fn pen_before_paint(ops: &RenderOpIRVec, text: &str) -> Option<TuiStyle> {
        let idx = ops.iter().position(
            |op| matches!(op, RenderOpIR::PaintTextWithAttributes(s, _) if s.as_str() == text),
        )?;
        match ops.get(idx.checked_sub(1)?)? {
            RenderOpIR::Common(RenderOpCommon::ApplyColors(Some(style))) => Some(*style),
            _ => None,
        }
    }

    // The cursor cell must be preceded by an explicit `ApplyColors` that sets the pen to
    // the swapped (block-cursor) colors. The compositor reads a painted cell's colors from
    // the current pen, not from the style handed to the paint op, so without this the
    // cursor inherits whatever the previously rendered pane left behind and can vanish in
    // non-top / non-left panes.
    #[test]
    fn end_of_line_cursor_sets_swapped_pen() {
        let (color_bg, color_text) = colors();
        let input = InputLine::new();
        let mut ops = RenderOpIRVec::new();
        input.render(
            &mut ops,
            "",
            "",
            col(0) + row(0),
            20,
            true,
            (color_bg, color_text),
        );

        let pen = pen_before_paint(&ops, " ").expect("cursor space must set the pen");
        assert_eq!(pen.color_fg, Some(color_bg));
        assert_eq!(pen.color_bg, Some(color_text));
    }

    #[test]
    fn mid_line_cursor_grapheme_sets_swapped_pen() {
        let (color_bg, color_text) = colors();
        // Fresh InputLine keeps the cursor at grapheme 0, so 'a' is the cursor cell.
        let input = InputLine::new();
        let mut ops = RenderOpIRVec::new();
        input.render(
            &mut ops,
            "ab",
            "",
            col(0) + row(0),
            20,
            true,
            (color_bg, color_text),
        );

        let pen = pen_before_paint(&ops, "a").expect("cursor grapheme must set the pen");
        assert_eq!(pen.color_fg, Some(color_bg));
        assert_eq!(pen.color_bg, Some(color_text));
    }

    #[test]
    fn unfocused_input_paints_no_cursor_cell() {
        let (color_bg, color_text) = colors();
        let input = InputLine::new();
        let mut ops = RenderOpIRVec::new();
        input.render(
            &mut ops,
            "",
            "",
            col(0) + row(0),
            20,
            false,
            (color_bg, color_text),
        );

        assert!(
            !ops.iter().any(|op| matches!(
                op,
                RenderOpIR::PaintTextWithAttributes(s, _) if s.as_str() == " "
            )),
            "an unfocused input must not paint a cursor cell",
        );
    }
}
