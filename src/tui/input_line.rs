use crate::tui::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use unicode_segmentation::UnicodeSegmentation;

pub struct InputLine {
    cursor: usize,
    /// Graphemes that fit on one row, as of the last render; drives `Up`/`Down`.
    row_capacity: AtomicUsize,
    /// First visible row when the text is taller than the rows it is given.
    scroll: AtomicUsize,
}

/// One screen row of a soft-wrapped input: a run of graphemes of the text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct VisualRow {
    start: usize,
    len: usize,
}

/// Splits `text` into screen rows of at most `capacity` graphemes. A row that is exactly
/// full is followed by another one, possibly empty, where the cursor can sit.
fn visual_rows(text: &str, capacity: usize) -> Vec<VisualRow> {
    let capacity = capacity.max(1);
    let mut rows = Vec::new();
    let mut offset = 0;
    for line in text.split('\n') {
        let count = line.graphemes(true).count();
        let mut done = 0;
        loop {
            let len = (count - done).min(capacity);
            rows.push(VisualRow {
                start: offset + done,
                len,
            });
            done += len;
            if len < capacity {
                break;
            }
        }
        offset += count + 1;
    }
    rows
}

fn row_of_cursor(rows: &[VisualRow], cursor: usize) -> usize {
    rows.iter()
        .rposition(|row| row.start <= cursor)
        .unwrap_or(0)
}

impl InputLine {
    pub fn new() -> Self {
        Self {
            cursor: 0,
            row_capacity: AtomicUsize::new(0),
            scroll: AtomicUsize::new(0),
        }
    }

    /// Moves the cursor one screen row up or down, keeping its column. Returns false at
    /// the first or last row so the caller can use the key for something else.
    fn move_vertically(&mut self, query: &str, down: bool) -> bool {
        let capacity = match self.row_capacity.load(Ordering::Relaxed) {
            0 => usize::MAX,
            capacity => capacity,
        };
        let rows = visual_rows(query, capacity);
        let current = row_of_cursor(&rows, self.cursor);
        let target = if down {
            current + 1
        } else {
            match current.checked_sub(1) {
                Some(target) => target,
                None => return false,
            }
        };
        let Some(row) = rows.get(target) else {
            return false;
        };
        let column = self.cursor - rows[current].start;
        self.cursor = row.start + column.min(row.len);
        true
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
                key: Key::SpecialKey(SpecialKey::Up),
            }) => self.move_vertically(query, false),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Down),
            }) => self.move_vertically(query, true),
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

    /// Rows the input occupies once soft-wrapped to `width`, at most `max_rows`. The
    /// placeholder counts as text that follows `query`.
    pub fn height(
        query: &str,
        placeholder: Option<&str>,
        prompt: &str,
        width: u16,
        max_rows: usize,
    ) -> usize {
        let capacity = (width as usize).saturating_sub(prompt.graphemes(true).count());
        let shown = format!("{query}{}", placeholder.unwrap_or_default());
        visual_rows(&shown, capacity).len().min(max_rows).max(1)
    }

    /// Places the cursor at the end of `query`. Used when opening an input prefilled with existing
    /// text (e.g. the preview search box seeded with the active pattern).
    pub fn set_cursor_end(&mut self, query: &str) {
        self.cursor = query.graphemes(true).count();
    }

    /// Paints the input soft-wrapped to `width` on at most `max_rows` rows, scrolling
    /// inside them so the cursor row stays visible. The prompt stays on the first row
    /// shown and every row starts after it. The placeholder is laid out as text that
    /// follows `query`, in its own color; the cursor never enters it.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        mut ops: &mut RenderOpIRVec,
        query: &str,
        placeholder: Option<(&str, TuiColor)>,
        prompt: &str,
        origin: Pos,
        width: u16,
        max_rows: usize,
        focused: bool,
        colors: (TuiColor, TuiColor),
    ) {
        let prompt_width = prompt.graphemes(true).count();
        let capacity = (width as usize).saturating_sub(prompt_width).max(1);
        self.row_capacity.store(capacity, Ordering::Relaxed);
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

        let typed = query.graphemes(true).count();
        let (placeholder, placeholder_style) = match placeholder {
            Some((text, color)) => (text, new_style!(color_fg: {color} color_bg: {color_bg})),
            None => ("", text_style),
        };
        let shown = format!("{query}{placeholder}");
        let style_at = |index: usize| {
            if index < typed {
                text_style
            } else {
                placeholder_style
            }
        };
        let graphemes: Vec<&str> = shown.graphemes(true).collect();
        let cursor = self.cursor.min(typed);
        let rows = visual_rows(&shown, capacity);
        let cursor_row = row_of_cursor(&rows, cursor);
        let max_rows = max_rows.max(1);

        let mut scroll = self
            .scroll
            .load(Ordering::Relaxed)
            .min(rows.len().saturating_sub(max_rows));
        if cursor_row < scroll {
            scroll = cursor_row;
        } else if cursor_row >= scroll + max_rows {
            scroll = cursor_row + 1 - max_rows;
        }
        self.scroll.store(scroll, Ordering::Relaxed);

        if prompt_width > 0 {
            ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
            ops += RenderOpIR::PaintTextWithAttributes(prompt.into(), Some(text_style));
        }

        for (screen_row, (row_idx, visual)) in rows
            .iter()
            .enumerate()
            .skip(scroll)
            .take(max_rows)
            .enumerate()
        {
            ops += RenderOpCommon::MoveCursorPositionRelTo(
                origin,
                col(prompt_width as u16) + row(screen_row as u16),
            );
            ops += RenderOpCommon::ApplyColors(Some(style_at(visual.start)));
            for (column, gr) in graphemes[visual.start..visual.start + visual.len]
                .iter()
                .enumerate()
            {
                let index = visual.start + column;
                if focused && row_idx == cursor_row && index == cursor {
                    ops += RenderOpCommon::ApplyColors(Some(cursor_style));
                    ops += RenderOpIR::PaintTextWithAttributes((*gr).into(), Some(cursor_style));
                    ops += RenderOpCommon::ApplyColors(Some(style_at(index + 1)));
                } else {
                    if index == typed {
                        ops += RenderOpCommon::ApplyColors(Some(placeholder_style));
                    }
                    ops += RenderOpIR::PaintTextWithAttributes((*gr).into(), Some(style_at(index)));
                }
            }
            if focused && row_idx == cursor_row && cursor == visual.start + visual.len {
                ops += RenderOpCommon::MoveCursorPositionRelTo(
                    origin,
                    col((prompt_width + visual.len) as u16) + row(screen_row as u16),
                );
                ops += RenderOpCommon::ApplyColors(Some(cursor_style));
                ops += RenderOpIR::PaintTextWithAttributes(" ".into(), Some(cursor_style));
                ops += RenderOpCommon::ApplyColors(Some(text_style));
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
            None,
            "",
            col(0) + row(0),
            20,
            1,
            true,
            (color_bg, color_text),
        );

        let pen = pen_before_paint(&ops, " ").expect("cursor space must set the pen");
        assert_eq!(pen.color_fg, Some(color_bg));
        assert_eq!(pen.color_bg, Some(color_text));
    }

    /// Text and foreground of every paint op, in order.
    fn painted(ops: &RenderOpIRVec) -> Vec<(String, Option<TuiColor>)> {
        ops.iter()
            .filter_map(|op| match op {
                RenderOpIR::PaintTextWithAttributes(text, style) => {
                    Some((text.to_string(), style.and_then(|style| style.color_fg)))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn placeholder_follows_the_text_in_its_own_color_over_several_rows() {
        let (color_bg, color_text) = colors();
        let weak = TuiColor::from(RgbValue::from_u8(90, 90, 90));
        let query = "/squash";
        let placeholder = " <ref>\nSquash the commits since a ref";
        let mut input = InputLine::new();
        input.set_cursor_end(query);
        let mut ops = RenderOpIRVec::new();
        input.render(
            &mut ops,
            query,
            Some((placeholder, weak)),
            "",
            col(0) + row(0),
            20,
            4,
            true,
            (color_bg, color_text),
        );

        let painted = painted(&ops);
        let text: String = painted.iter().map(|(text, _)| text.as_str()).collect();
        assert_eq!(text, "/squash <ref>Squash the commits since a ref");
        assert_eq!(painted[6], ("h".to_string(), Some(color_text)));
        // The cursor sits on the first cell of the placeholder.
        assert_eq!(painted[7], (" ".to_string(), Some(color_bg)));
        assert_eq!(painted[8], ("<".to_string(), Some(weak)));
        assert_eq!(painted.last().map(|(_, color)| *color), Some(Some(weak)));
        let pen = pen_before_paint(&ops, "<").expect("the placeholder must set the pen");
        assert_eq!(pen.color_fg, Some(weak));
        assert_eq!(pen.color_bg, Some(color_bg));

        assert_eq!(InputLine::height(query, Some(placeholder), "", 20, 4), 3);
        assert_eq!(InputLine::height(query, Some(placeholder), "", 20, 2), 2);
        assert_eq!(InputLine::height(query, None, "", 20, 4), 1);
    }

    #[test]
    fn cursor_and_keys_ignore_the_placeholder() {
        let (color_bg, color_text) = colors();
        let weak = TuiColor::from(RgbValue::from_u8(90, 90, 90));
        let mut query = "/squash".to_string();
        let mut input = InputLine::new();
        input.set_cursor_end(&query);
        let mut ops = RenderOpIRVec::new();
        input.render(
            &mut ops,
            &query,
            Some(("\nSquash the commits", weak)),
            "",
            col(0) + row(0),
            20,
            4,
            true,
            (color_bg, color_text),
        );

        // The cursor ends the typed row; the placeholder starts on the next one.
        let painted = painted(&ops);
        assert_eq!(painted[7], (" ".to_string(), Some(color_bg)));
        assert_eq!(painted[8], ("S".to_string(), Some(weak)));
        let down = InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(SpecialKey::Down),
        });
        assert!(!input.handle_key(&down, &mut query));
        assert_eq!(input.cursor, 7);
        assert_eq!(query, "/squash");
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
            None,
            "",
            col(0) + row(0),
            20,
            1,
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
            None,
            "",
            col(0) + row(0),
            20,
            1,
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

#[cfg(test)]
mod wrap_tests {
    use super::*;

    fn down() -> InputEvent {
        InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(SpecialKey::Down),
        })
    }

    fn up() -> InputEvent {
        InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(SpecialKey::Up),
        })
    }

    fn spans(text: &str, capacity: usize) -> Vec<(usize, usize)> {
        visual_rows(text, capacity)
            .iter()
            .map(|row| (row.start, row.len))
            .collect()
    }

    #[test]
    fn rows_wrap_at_capacity_and_follow_newlines() {
        assert_eq!(spans("", 4), [(0, 0)]);
        assert_eq!(spans("abcdef", 4), [(0, 4), (4, 2)]);
        assert_eq!(spans("ab\ncd", 4), [(0, 2), (3, 2)]);
    }

    #[test]
    fn a_full_row_is_followed_by_a_row_for_the_cursor() {
        assert_eq!(spans("abcd", 4), [(0, 4), (4, 0)]);
        assert_eq!(spans("abcd\nef", 4), [(0, 4), (4, 0), (5, 2)]);
    }

    #[test]
    fn height_counts_wrapped_rows_and_respects_the_cap() {
        assert_eq!(InputLine::height("", None, "> ", 10, 5), 1);
        assert_eq!(InputLine::height("abcdefghij", None, "> ", 10, 5), 2);
        assert_eq!(InputLine::height("a\nb\nc\nd", None, "", 10, 3), 3);
    }

    #[test]
    fn vertical_keys_are_not_consumed_on_a_single_row() {
        let mut input = InputLine::new();
        let mut query = "abc".to_string();
        assert!(!input.handle_key(&up(), &mut query));
        assert!(!input.handle_key(&down(), &mut query));
    }

    #[test]
    fn vertical_keys_move_between_rows_keeping_the_column() {
        let mut input = InputLine::new();
        let mut query = "abcdef\ngh\nijkl".to_string();
        input.cursor = 5;
        assert!(input.handle_key(&down(), &mut query));
        assert_eq!(input.cursor, 9, "clamped to the end of the shorter row");
        assert!(input.handle_key(&down(), &mut query));
        assert_eq!(input.cursor, 12);
        assert!(!input.handle_key(&down(), &mut query));
        assert!(input.handle_key(&up(), &mut query));
        assert!(input.handle_key(&up(), &mut query));
        assert_eq!(input.cursor, 2);
        assert!(!input.handle_key(&up(), &mut query));
    }

    #[test]
    fn vertical_keys_follow_soft_wrapping_after_a_render() {
        let mut input = InputLine::new();
        let mut query = "abcdefgh".to_string();
        let mut ops = RenderOpIRVec::new();
        let colors = (
            TuiColor::from(RgbValue::from_u8(0, 0, 0)),
            TuiColor::from(RgbValue::from_u8(255, 255, 255)),
        );
        input.render(
            &mut ops,
            &query,
            None,
            "",
            col(0) + row(0),
            4,
            3,
            true,
            colors,
        );
        input.cursor = 1;
        assert!(input.handle_key(&down(), &mut query));
        assert_eq!(input.cursor, 5);
    }

    #[test]
    fn render_scrolls_to_keep_the_cursor_row_visible() {
        let mut input = InputLine::new();
        let query = "a\nb\nc\nd";
        input.set_cursor_end(query);
        let mut ops = RenderOpIRVec::new();
        let colors = (
            TuiColor::from(RgbValue::from_u8(0, 0, 0)),
            TuiColor::from(RgbValue::from_u8(255, 255, 255)),
        );
        input.render(
            &mut ops,
            query,
            None,
            "",
            col(0) + row(0),
            10,
            2,
            false,
            colors,
        );
        let painted: Vec<&str> = ops
            .iter()
            .filter_map(|op| match op {
                RenderOpIR::PaintTextWithAttributes(text, _) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(painted, ["c", "d"]);
    }
}
