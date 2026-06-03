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
                        ..
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
                        ..
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
                        ..
                    },
            }) => self.delete_grapheme(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('b'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        ..
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
                        ..
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
                        ..
                    },
            }) => self.kill_to_end(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('u'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        ..
                    },
            }) => self.kill_to_start(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('w'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        ..
                    },
            }) => self.kill_word_backward(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(SpecialKey::Left),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        ..
                    },
            }) => self.cursor_prev_word(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(SpecialKey::Right),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        ..
                    },
            }) => self.cursor_next_word(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('b'),
                mask:
                    ModifierKeysMask {
                        alt_key_state: KeyState::Pressed,
                        ..
                    },
            }) => self.cursor_prev_word(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('f'),
                mask:
                    ModifierKeysMask {
                        alt_key_state: KeyState::Pressed,
                        ..
                    },
            }) => self.cursor_next_word(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('d'),
                mask:
                    ModifierKeysMask {
                        alt_key_state: KeyState::Pressed,
                        ..
                    },
            }) => self.kill_word_forward(query),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(SpecialKey::Backspace),
                mask:
                    ModifierKeysMask {
                        alt_key_state: KeyState::Pressed,
                        ..
                    },
            }) => self.kill_word_backward(query),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character(ch),
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

    pub fn render(
        &self,
        mut ops: &mut RenderOpIRVec,
        query: &str,
        origin: Pos,
        width: u16,
        focused: bool,
        bg_rgb: [u8; 3],
        fg_rgb: [u8; 3],
    ) {
        let width = width as usize;

        let color_bg = tui_color!(bg_rgb[0], bg_rgb[1], bg_rgb[2]);
        let color_text = tui_color!(fg_rgb[0], fg_rgb[1], fg_rgb[2]);
        let cursor_style = new_style!(reverse);

        let bg_style = new_style!(color_bg: {color_bg});
        let text_style = new_style!(color_fg: {color_text} color_bg: {color_bg});

        ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
        ops += RenderOpCommon::ApplyColors(Some(bg_style));
        ops +=
            RenderOpIR::PaintTextWithAttributes(" ".repeat(width).as_str().into(), Some(bg_style));

        let graphemes: Vec<(usize, &str)> = query.grapheme_indices(true).collect();
        let count = graphemes.len();
        let cursor = self.cursor.min(count);

        let scroll = if count >= width && cursor >= width {
            cursor - width + 1
        } else {
            0
        };

        ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));

        for (char_col, (i, &(_, gr))) in graphemes.iter().enumerate().skip(scroll).enumerate() {
            if char_col >= width {
                break;
            }
            if i == cursor && focused {
                ops += RenderOpIR::PaintTextWithAttributes(gr.into(), Some(cursor_style));
            } else {
                ops += RenderOpIR::PaintTextWithAttributes(gr.into(), Some(text_style));
            }
        }

        if cursor >= count && focused && count.saturating_sub(scroll) < width {
            ops += RenderOpIR::PaintTextWithAttributes(" ".into(), Some(cursor_style));
        }
    }
}

fn grapheme_byte_offset(text: &str, grapheme_idx: usize) -> usize {
    text.grapheme_indices(true)
        .nth(grapheme_idx)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}
