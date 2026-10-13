use crate::loader::{drag_range, word_bounds};
use crate::tui::*;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

#[derive(Clone, Debug, Default)]
pub struct InputLine {
    text: String,
    cursor: usize,
    /// First visible row when the text is taller than the rows it is given.
    scroll: usize,
    /// Where the input was last painted; drives `Up`/`Down` and the mouse.
    painted: Option<Painted>,
    selection: Option<Selection>,
    /// Time and grapheme of the last press, to recognize a double click.
    last_press: Option<(Instant, usize)>,
}

/// The cells an input was painted on: its first one, prompt included, and its size.
#[derive(Clone, Copy, Debug)]
struct Painted {
    row: usize,
    col: usize,
    prompt_width: usize,
    width: usize,
    rows: usize,
}

impl Painted {
    /// Graphemes that fit on one row.
    fn capacity(&self) -> usize {
        self.width.saturating_sub(self.prompt_width).max(1)
    }
}

/// A mouse selection in progress, in bytes of the text.
#[derive(Clone, Copy, Debug)]
struct Selection {
    /// Byte under the press, and the word around it.
    anchor: usize,
    word: (usize, usize),
    range: (usize, usize),
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
        Self::default()
    }

    /// An input holding `text`, with the cursor at its end.
    pub fn with_text(text: impl Into<String>) -> Self {
        let mut input = Self::new();
        input.set_text(text);
        input
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replaces the text and puts the cursor at its end.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.graphemes(true).count();
        self.selection = None;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.selection = None;
    }

    /// The grapheme a screen position puts the cursor on, and the one it points at:
    /// they differ past the end of a row. `None` outside the cells the input was last
    /// painted on, unless `clamp` brings the position back into them.
    fn locate(&self, pos: Pos, clamp: bool) -> Option<(usize, usize)> {
        let painted = self.painted?;
        let (row, col) = (pos.row_index.as_usize(), pos.col_index.as_usize());
        let inside = (painted.row..painted.row + painted.rows).contains(&row)
            && (painted.col..painted.col + painted.width).contains(&col);
        if !inside && !clamp {
            return None;
        }
        let screen_row = row
            .saturating_sub(painted.row)
            .min(painted.rows.saturating_sub(1));
        let column = col.saturating_sub(painted.col + painted.prompt_width);
        let rows = visual_rows(&self.text, painted.capacity());
        Some(match rows.get(self.scroll + screen_row) {
            Some(row) => (
                row.start + column.min(row.len),
                row.start + column.min(row.len.saturating_sub(1)),
            ),
            None => {
                let end = self.text.graphemes(true).count();
                (end, end)
            }
        })
    }

    /// Puts the cursor under the mouse and anchors a selection on the word there; a
    /// second press on the same grapheme selects that word. False outside the input.
    fn press(&mut self, pos: Pos, now: Instant) -> bool {
        let Some((cursor, pointed)) = self.locate(pos, false) else {
            return false;
        };
        self.cursor = cursor;
        let anchor = grapheme_byte_offset(&self.text, pointed);
        let word = if anchor < self.text.len() {
            word_bounds(&self.text, anchor)
        } else {
            (anchor, anchor)
        };
        let double = self
            .last_press
            .is_some_and(|(at, last)| last == pointed && now.duration_since(at) < DOUBLE_CLICK);
        self.last_press = Some((now, pointed));
        self.selection = Some(Selection {
            anchor,
            word,
            range: if double { word } else { (anchor, anchor) },
        });
        true
    }

    /// Extends the selection to the word under the mouse, whatever the rows in between.
    /// A position outside the input stands for the nearest cell of it.
    fn drag(&mut self, pos: Pos) {
        let (Some(selection), Some((_, pointed))) = (self.selection, self.locate(pos, true)) else {
            return;
        };
        let cursor = grapheme_byte_offset(&self.text, pointed);
        let range = if cursor < self.text.len() {
            drag_range(&self.text, selection.anchor, selection.word, cursor)
        } else {
            (selection.word.0, self.text.len())
        };
        self.selection = Some(Selection { range, ..selection });
    }

    /// Ends the selection and returns its text, if it holds any.
    fn release(&mut self) -> Option<String> {
        let (lo, hi) = self.selection.take()?.range;
        self.text
            .get(lo..hi)
            .filter(|selected| !selected.is_empty())
            .map(str::to_string)
    }

    /// Whether a mouse selection is in progress.
    pub fn selecting(&self) -> bool {
        self.selection.is_some()
    }

    pub fn cancel_select(&mut self) {
        self.selection = None;
    }

    /// Takes the mouse events that are the input's: a press on it places the cursor, a
    /// drag from there selects whole words, and the release copies them to the
    /// clipboard. Returns false for any other event.
    pub fn handle_mouse(&mut self, mouse: &MouseInput) -> bool {
        match mouse {
            MouseInput {
                kind: MouseInputKind::MouseDown(Button::Left),
                pos,
                ..
            } => self.press(*pos, Instant::now()),
            MouseInput {
                kind: MouseInputKind::MouseDrag(Button::Left),
                pos,
                ..
            } if self.selection.is_some() => {
                self.drag(*pos);
                true
            }
            MouseInput {
                kind: MouseInputKind::MouseUp(Button::Left),
                ..
            } if self.selection.is_some() => {
                if let Some(selected) = self.release() {
                    let mut clipboard = r3bl_tui::SystemClipboard;
                    let _ = clipboard.try_to_put_content_into_clipboard(selected);
                }
                true
            }
            _ => false,
        }
    }

    /// Moves the cursor one screen row up or down, keeping its column. Returns false at
    /// the first or last row so the caller can use the key for something else.
    fn move_vertically(&mut self, down: bool) -> bool {
        let capacity = self
            .painted
            .map_or(usize::MAX, |painted| painted.capacity());
        let rows = visual_rows(&self.text, capacity);
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

    /// The line the cursor is on, from one newline to the next, however many rows it wraps over.
    fn current_line(&self) -> VisualRow {
        let rows = visual_rows(&self.text, usize::MAX);
        rows[row_of_cursor(&rows, self.cursor)]
    }

    pub fn handle_key(&mut self, input_event: &InputEvent) -> bool {
        let grapheme_count = self.text.graphemes(true).count();
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
                self.cursor = self.current_line().start;
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
                let line = self.current_line();
                self.cursor = line.start + line.len;
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
            }) => self.delete_grapheme(),
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
            }) => self.kill_to_end(),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('c'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => {
                self.text.clear();
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
            }) => self.kill_to_start(),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('w'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => self.kill_word_backward(),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(SpecialKey::Left),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => self.cursor_prev_word(),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(SpecialKey::Right),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => self.cursor_next_word(),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('b'),
                mask:
                    ModifierKeysMask {
                        alt_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        ctrl_key_state: KeyState::NotPressed,
                    },
            }) => self.cursor_prev_word(),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('f'),
                mask:
                    ModifierKeysMask {
                        alt_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        ctrl_key_state: KeyState::NotPressed,
                    },
            }) => self.cursor_next_word(),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('d'),
                mask:
                    ModifierKeysMask {
                        alt_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        ctrl_key_state: KeyState::NotPressed,
                    },
            }) => self.kill_word_forward(),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(SpecialKey::Backspace),
                mask:
                    ModifierKeysMask {
                        alt_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        ctrl_key_state: KeyState::NotPressed,
                    },
            }) => self.kill_word_backward(),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(SpecialKey::Enter),
                mask:
                    ModifierKeysMask {
                        shift_key_state: KeyState::Pressed,
                        alt_key_state: KeyState::NotPressed,
                        ctrl_key_state: KeyState::NotPressed,
                    },
            }) => {
                let byte_pos = grapheme_byte_offset(&self.text, self.cursor);
                self.text.insert(byte_pos, '\n');
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
                let byte_pos = grapheme_byte_offset(&self.text, self.cursor);
                self.text.insert(byte_pos, *ch);
                self.cursor += 1;
                true
            }
            InputEvent::BracketedPaste(text) => {
                if text.is_empty() {
                    return false;
                }
                let byte_pos = grapheme_byte_offset(&self.text, self.cursor);
                self.text.insert_str(byte_pos, text);
                self.cursor += text.graphemes(true).count();
                true
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Backspace),
            }) => self.backspace_grapheme(),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Delete),
            }) => self.delete_grapheme(),
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
            }) => self.move_vertically(false),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Down),
            }) => self.move_vertically(true),
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

    fn backspace_grapheme(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let graphemes: Vec<(usize, &str)> = self.text.grapheme_indices(true).collect();
        let target = self.cursor - 1;
        let start = graphemes[target].0;
        let end = if target + 1 < graphemes.len() {
            graphemes[target + 1].0
        } else {
            self.text.len()
        };
        self.text.replace_range(start..end, "");
        self.cursor = target;
        true
    }

    fn delete_grapheme(&mut self) -> bool {
        let graphemes: Vec<(usize, &str)> = self.text.grapheme_indices(true).collect();
        if self.cursor >= graphemes.len() {
            return false;
        }
        let start = graphemes[self.cursor].0;
        let end = if self.cursor + 1 < graphemes.len() {
            graphemes[self.cursor + 1].0
        } else {
            self.text.len()
        };
        self.text.replace_range(start..end, "");
        true
    }

    fn kill_to_end(&mut self) -> bool {
        let line = self.current_line();
        let line_end = line.start + line.len;
        let end = if self.cursor == line_end {
            line_end + 1
        } else {
            line_end
        };
        let start_byte = grapheme_byte_offset(&self.text, self.cursor);
        let end_byte = grapheme_byte_offset(&self.text, end);
        if start_byte == end_byte {
            return false;
        }
        self.text.replace_range(start_byte..end_byte, "");
        true
    }

    fn kill_to_start(&mut self) -> bool {
        let start = self.current_line().start;
        if self.cursor == start {
            return false;
        }
        let start_byte = grapheme_byte_offset(&self.text, start);
        let end_byte = grapheme_byte_offset(&self.text, self.cursor);
        self.text.replace_range(start_byte..end_byte, "");
        self.cursor = start;
        true
    }

    fn kill_word_backward(&mut self) -> bool {
        let word_start = self.prev_word_start();
        if word_start >= self.cursor {
            return false;
        }
        let graphemes: Vec<(usize, &str)> = self.text.grapheme_indices(true).collect();
        let start_byte = graphemes[word_start].0;
        let end_byte = if self.cursor < graphemes.len() {
            graphemes[self.cursor].0
        } else {
            self.text.len()
        };
        self.text.replace_range(start_byte..end_byte, "");
        self.cursor = word_start;
        true
    }

    fn cursor_prev_word(&mut self) -> bool {
        let new = self.prev_word_start();
        if new == self.cursor {
            return false;
        }
        self.cursor = new;
        true
    }

    fn cursor_next_word(&mut self) -> bool {
        let new = self.next_word_start();
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

    fn prev_word_start(&self) -> usize {
        let graphemes: Vec<(usize, &str)> = self.text.grapheme_indices(true).collect();
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

    fn next_word_start(&self) -> usize {
        let graphemes: Vec<(usize, &str)> = self.text.grapheme_indices(true).collect();
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

    fn next_word_end(&self) -> usize {
        let graphemes: Vec<(usize, &str)> = self.text.grapheme_indices(true).collect();
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

    fn kill_word_forward(&mut self) -> bool {
        let word_end = self.next_word_end();
        if word_end <= self.cursor || word_end == 0 {
            return false;
        }
        let graphemes: Vec<(usize, &str)> = self.text.grapheme_indices(true).collect();
        let start_byte = if self.cursor < graphemes.len() {
            graphemes[self.cursor].0
        } else {
            self.text.len()
        };
        let end_byte = if word_end < graphemes.len() {
            graphemes[word_end].0
        } else {
            self.text.len()
        };
        self.text.replace_range(start_byte..end_byte, "");
        true
    }

    /// Rows the input occupies once soft-wrapped to `width`, at most `max_rows`. The
    /// placeholder counts as text that follows the typed one.
    pub fn height(
        &self,
        placeholder: Option<&str>,
        prompt: &str,
        width: u16,
        max_rows: usize,
    ) -> usize {
        let capacity = (width as usize).saturating_sub(prompt.graphemes(true).count());
        let shown = format!("{}{}", self.text, placeholder.unwrap_or_default());
        visual_rows(&shown, capacity).len().min(max_rows).max(1)
    }

    /// Paints the input soft-wrapped to `width` on at most `max_rows` rows, scrolling
    /// inside them so the cursor row stays visible. The prompt stays on the first row
    /// shown and every row starts after it. The placeholder is laid out as text that
    /// follows the typed text, in its own color; the cursor never enters it. `colors`
    /// are the background, the text and the background of a mouse selection.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        mut ops: &mut RenderOpIRVec,
        placeholder: Option<(&str, TuiColor)>,
        prompt: &str,
        origin: Pos,
        width: u16,
        max_rows: usize,
        focused: bool,
        colors: (TuiColor, TuiColor, TuiColor),
    ) {
        let prompt_width = prompt.graphemes(true).count();
        let capacity = (width as usize).saturating_sub(prompt_width).max(1);
        let (color_bg, color_text, color_selection) = colors;
        let text_style = new_style!(color_fg: {color_text} color_bg: {color_bg});
        // The compositor takes a painted cell's *colors* from the current pen
        // (`ApplyColors`/`SetBgColor`), not from the style handed to
        // `PaintTextWithAttributes` — so we set the pen explicitly instead of relying on
        // whatever the previously rendered pane left behind. The cursor is a solid block
        // drawn with foreground/background swapped, which stays visible at any pane
        // position (a bare `reverse` attribute inherits the stale pen and can vanish).
        let cursor_style = new_style!(color_fg: {color_bg} color_bg: {color_text});
        let selection_style = new_style!(color_fg: {color_text} color_bg: {color_selection});
        ops += RenderOpCommon::ApplyColors(Some(text_style));

        let typed = self.text.graphemes(true).count();
        let (placeholder, placeholder_style) = match placeholder {
            Some((text, color)) => (text, new_style!(color_fg: {color} color_bg: {color_bg})),
            None => ("", text_style),
        };
        let shown = format!("{}{placeholder}", self.text);
        let graphemes: Vec<&str> = shown.graphemes(true).collect();
        let cursor = self.cursor.min(typed);
        let selected = self.selection.map_or(0..0, |selection| {
            let index = |byte| {
                self.text
                    .grapheme_indices(true)
                    .take_while(|(at, _)| *at < byte)
                    .count()
            };
            index(selection.range.0)..index(selection.range.1)
        });
        let style_at = |index: usize| {
            if focused && index == cursor {
                cursor_style
            } else if selected.contains(&index) {
                selection_style
            } else if index < typed {
                text_style
            } else {
                placeholder_style
            }
        };
        let rows = visual_rows(&shown, capacity);
        let cursor_row = row_of_cursor(&rows, cursor);
        let max_rows = max_rows.max(1);

        let mut scroll = self.scroll.min(rows.len().saturating_sub(max_rows));
        if cursor_row < scroll {
            scroll = cursor_row;
        } else if cursor_row >= scroll + max_rows {
            scroll = cursor_row + 1 - max_rows;
        }
        self.scroll = scroll;
        self.painted = Some(Painted {
            row: origin.row_index.as_usize(),
            col: origin.col_index.as_usize(),
            prompt_width,
            width: width as usize,
            rows: (rows.len() - scroll).min(max_rows),
        });

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
            let mut pen = None;
            for (column, gr) in graphemes[visual.start..visual.start + visual.len]
                .iter()
                .enumerate()
            {
                let style = style_at(visual.start + column);
                if pen != Some(style) {
                    ops += RenderOpCommon::ApplyColors(Some(style));
                    pen = Some(style);
                }
                ops += RenderOpIR::PaintTextWithAttributes((*gr).into(), Some(style));
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

    fn selection() -> TuiColor {
        TuiColor::from(RgbValue::from_u8(90, 60, 20))
    }

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
        let mut input = InputLine::new();
        let mut ops = RenderOpIRVec::new();
        input.render(
            &mut ops,
            None,
            "",
            col(0) + row(0),
            20,
            1,
            true,
            (color_bg, color_text, selection()),
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
        let placeholder = " <ref>\nSquash the commits since a ref";
        let mut input = InputLine::new();
        input.set_text("/squash");
        let mut ops = RenderOpIRVec::new();
        input.render(
            &mut ops,
            Some((placeholder, weak)),
            "",
            col(0) + row(0),
            20,
            4,
            true,
            (color_bg, color_text, selection()),
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

        assert_eq!(input.height(Some(placeholder), "", 20, 4), 3);
        assert_eq!(input.height(Some(placeholder), "", 20, 2), 2);
        assert_eq!(input.height(None, "", 20, 4), 1);
    }

    #[test]
    fn cursor_and_keys_ignore_the_placeholder() {
        let (color_bg, color_text) = colors();
        let weak = TuiColor::from(RgbValue::from_u8(90, 90, 90));
        let mut input = InputLine::new();
        input.set_text("/squash");
        let mut ops = RenderOpIRVec::new();
        input.render(
            &mut ops,
            Some(("\nSquash the commits", weak)),
            "",
            col(0) + row(0),
            20,
            4,
            true,
            (color_bg, color_text, selection()),
        );

        // The cursor ends the typed row; the placeholder starts on the next one.
        let painted = painted(&ops);
        assert_eq!(painted[7], (" ".to_string(), Some(color_bg)));
        assert_eq!(painted[8], ("S".to_string(), Some(weak)));
        let down = InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(SpecialKey::Down),
        });
        assert!(!input.handle_key(&down));
        assert_eq!(input.cursor, 7);
        assert_eq!(input.text(), "/squash");
    }

    #[test]
    fn mid_line_cursor_grapheme_sets_swapped_pen() {
        let (color_bg, color_text) = colors();
        let mut input = InputLine::new();
        input.set_text("ab");
        input.cursor = 0;
        let mut ops = RenderOpIRVec::new();
        input.render(
            &mut ops,
            None,
            "",
            col(0) + row(0),
            20,
            1,
            true,
            (color_bg, color_text, selection()),
        );

        let pen = pen_before_paint(&ops, "a").expect("cursor grapheme must set the pen");
        assert_eq!(pen.color_fg, Some(color_bg));
        assert_eq!(pen.color_bg, Some(color_text));
    }

    #[test]
    fn unfocused_input_paints_no_cursor_cell() {
        let (color_bg, color_text) = colors();
        let mut input = InputLine::new();
        let mut ops = RenderOpIRVec::new();
        input.render(
            &mut ops,
            None,
            "",
            col(0) + row(0),
            20,
            1,
            false,
            (color_bg, color_text, selection()),
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
    fn replacing_the_text_moves_the_cursor_to_its_end() {
        let mut input = InputLine::with_text("abc");
        input.cursor = 1;
        input.set_text("héllo\nyou");
        assert_eq!(input.cursor, 9);
        input.cursor = 4;
        input.clear();
        assert_eq!((input.text(), input.cursor), ("", 0));
        let copy = InputLine::with_text("ab").clone();
        assert_eq!((copy.text(), copy.cursor), ("ab", 2));
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
        let mut input = InputLine::new();
        assert_eq!(input.height(None, "> ", 10, 5), 1);
        input.set_text("abcdefghij");
        assert_eq!(input.height(None, "> ", 10, 5), 2);
        input.set_text("a\nb\nc\nd");
        assert_eq!(input.height(None, "", 10, 3), 3);
    }

    #[test]
    fn vertical_keys_are_not_consumed_on_a_single_row() {
        let mut input = InputLine::new();
        input.set_text("abc");
        assert!(!input.handle_key(&up()));
        assert!(!input.handle_key(&down()));
    }

    #[test]
    fn vertical_keys_move_between_rows_keeping_the_column() {
        let mut input = InputLine::new();
        input.set_text("abcdef\ngh\nijkl");
        input.cursor = 5;
        assert!(input.handle_key(&down()));
        assert_eq!(input.cursor, 9, "clamped to the end of the shorter row");
        assert!(input.handle_key(&down()));
        assert_eq!(input.cursor, 12);
        assert!(!input.handle_key(&down()));
        assert!(input.handle_key(&up()));
        assert!(input.handle_key(&up()));
        assert_eq!(input.cursor, 2);
        assert!(!input.handle_key(&up()));
    }

    fn ctrl(ch: char) -> InputEvent {
        InputEvent::Keyboard(KeyPress::WithModifiers {
            key: Key::Character(ch),
            mask: ModifierKeysMask::new().with_ctrl(),
        })
    }

    fn plain(key: SpecialKey) -> InputEvent {
        InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(key),
        })
    }

    #[test]
    fn ctrl_a_and_ctrl_e_stay_on_the_line_of_the_cursor() {
        let mut input = InputLine::new();
        input.set_text("abcdef\ngh\n\nijkl");
        input.cursor = 8;
        assert!(input.handle_key(&ctrl('a')));
        assert_eq!(input.cursor, 7);
        assert!(input.handle_key(&ctrl('e')));
        assert_eq!(input.cursor, 9);
        input.cursor = 10;
        assert!(input.handle_key(&ctrl('a')));
        assert_eq!(input.cursor, 10, "an empty line starts at the cursor");
        assert!(input.handle_key(&ctrl('e')));
        assert_eq!(input.cursor, 10);
        assert_eq!(input.text(), "abcdef\ngh\n\nijkl");
    }

    #[test]
    fn ctrl_a_and_ctrl_e_ignore_soft_wrapping() {
        let mut input = InputLine::new();
        input.set_text("abcdefgh\nij");
        input.painted = Some(Painted {
            row: 0,
            col: 0,
            prompt_width: 0,
            width: 4,
            rows: 1,
        });
        input.cursor = 6;
        assert!(input.handle_key(&ctrl('a')));
        assert_eq!(input.cursor, 0);
        input.cursor = 1;
        assert!(input.handle_key(&ctrl('e')));
        assert_eq!(input.cursor, 8);
    }

    #[test]
    fn ctrl_k_kills_to_the_end_of_the_line_then_joins_the_next_one() {
        let mut input = InputLine::new();
        input.set_text("abcdef\ngh\nijkl");
        input.cursor = 2;
        assert!(input.handle_key(&ctrl('k')));
        assert_eq!(input.text(), "ab\ngh\nijkl");
        assert_eq!(input.cursor, 2);
        assert!(input.handle_key(&ctrl('k')));
        assert_eq!(input.text(), "abgh\nijkl");
        assert_eq!(input.cursor, 2);
        input.cursor = 9;
        assert!(!input.handle_key(&ctrl('k')));
        assert_eq!(input.text(), "abgh\nijkl");
    }

    #[test]
    fn ctrl_u_kills_to_the_start_of_the_line_only() {
        let mut input = InputLine::new();
        input.set_text("abcdef\ngh\nijkl");
        input.cursor = 12;
        assert!(input.handle_key(&ctrl('u')));
        assert_eq!(input.text(), "abcdef\ngh\nkl");
        assert_eq!(input.cursor, 10);
        assert!(!input.handle_key(&ctrl('u')));
        assert_eq!(input.text(), "abcdef\ngh\nkl");
    }

    #[test]
    fn ctrl_k_and_ctrl_u_ignore_soft_wrapping() {
        let mut input = InputLine::new();
        input.set_text("abcdefgh\nij");
        input.painted = Some(Painted {
            row: 0,
            col: 0,
            prompt_width: 0,
            width: 4,
            rows: 1,
        });
        input.cursor = 6;
        assert!(input.handle_key(&ctrl('u')));
        assert_eq!(input.text(), "gh\nij");
        input.set_text("abcdefgh\nij");
        input.cursor = 1;
        assert!(input.handle_key(&ctrl('k')));
        assert_eq!(input.text(), "a\nij");
    }

    #[test]
    fn home_and_end_span_the_whole_text() {
        let mut input = InputLine::new();
        input.set_text("abcdef\ngh\nijkl");
        input.cursor = 8;
        assert!(input.handle_key(&plain(SpecialKey::Home)));
        assert_eq!(input.cursor, 0);
        input.cursor = 8;
        assert!(input.handle_key(&plain(SpecialKey::End)));
        assert_eq!(input.cursor, 14);
    }

    #[test]
    fn vertical_keys_follow_soft_wrapping_after_a_render() {
        let mut input = InputLine::new();
        input.set_text("abcdefgh");
        let mut ops = RenderOpIRVec::new();
        let colors = (
            TuiColor::from(RgbValue::from_u8(0, 0, 0)),
            TuiColor::from(RgbValue::from_u8(255, 255, 255)),
            TuiColor::from(RgbValue::from_u8(90, 60, 20)),
        );
        input.render(&mut ops, None, "", col(0) + row(0), 4, 3, true, colors);
        input.cursor = 1;
        assert!(input.handle_key(&down()));
        assert_eq!(input.cursor, 5);
    }

    #[test]
    fn render_scrolls_to_keep_the_cursor_row_visible() {
        let mut input = InputLine::new();
        input.set_text("a\nb\nc\nd");
        let mut ops = RenderOpIRVec::new();
        let colors = (
            TuiColor::from(RgbValue::from_u8(0, 0, 0)),
            TuiColor::from(RgbValue::from_u8(255, 255, 255)),
            TuiColor::from(RgbValue::from_u8(90, 60, 20)),
        );
        input.render(&mut ops, None, "", col(0) + row(0), 10, 2, false, colors);
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

#[cfg(test)]
mod selection_tests {
    use super::*;

    fn colors() -> (TuiColor, TuiColor, TuiColor) {
        (
            TuiColor::from(RgbValue::from_u8(0, 0, 0)),
            TuiColor::from(RgbValue::from_u8(255, 255, 255)),
            TuiColor::from(RgbValue::from_u8(90, 60, 20)),
        )
    }

    /// An input holding `text`, painted with `prompt` at row 5 and column 3.
    fn painted(text: &str, prompt: &str, width: u16, max_rows: usize) -> InputLine {
        let mut input = InputLine::with_text(text);
        let mut ops = RenderOpIRVec::new();
        let origin = col(3) + row(5);
        input.render(
            &mut ops,
            None,
            prompt,
            origin,
            width,
            max_rows,
            true,
            colors(),
        );
        input
    }

    fn at(row_index: u16, col_index: u16) -> Pos {
        col(col_index) + row(row_index)
    }

    /// Background of the pen in force when each grapheme of the text is painted.
    fn backgrounds(input: &mut InputLine) -> Vec<(String, Option<TuiColor>)> {
        let mut ops = RenderOpIRVec::new();
        input.render(&mut ops, None, "", col(3) + row(5), 20, 3, false, colors());
        let mut pen = None;
        let mut cells = Vec::new();
        for op in ops.iter() {
            match op {
                RenderOpIR::Common(RenderOpCommon::ApplyColors(Some(style))) => {
                    pen = style.color_bg;
                }
                RenderOpIR::PaintTextWithAttributes(text, _) => {
                    cells.push((text.to_string(), pen));
                }
                _ => {}
            }
        }
        cells
    }

    #[test]
    fn a_press_puts_the_cursor_under_the_mouse_after_the_prompt() {
        let mut input = painted("abcdef\ngh", "> ", 12, 3);
        let now = Instant::now();
        assert!(input.press(at(5, 9), now));
        assert_eq!(input.cursor, 4);
        assert!(input.press(at(5, 3), now), "on the prompt");
        assert_eq!(input.cursor, 0);
        assert!(input.press(at(6, 14), now), "past the end of a row");
        assert_eq!(input.cursor, 9);
        assert!(!input.press(at(4, 9), now), "above the input");
        assert!(!input.press(at(7, 9), now), "below its rows");
        assert!(!input.press(at(5, 15), now), "past its width");
        assert_eq!(input.cursor, 9);
    }

    #[test]
    fn a_press_follows_soft_wrapping_and_the_scroll() {
        let mut input = painted("abcdefgh", "", 4, 3);
        assert!(input.press(at(6, 4), Instant::now()));
        assert_eq!(input.cursor, 5);

        let mut input = painted("a\nb\nc\nd", "", 10, 2);
        assert!(input.press(at(5, 3), Instant::now()));
        assert_eq!(input.cursor, 4, "the first row shown is the third one");
        assert!(!input.press(at(7, 3), Instant::now()));
    }

    #[test]
    fn a_drag_selects_whole_words_across_lines_in_both_directions() {
        let mut input = painted("one two\nthree four", "", 20, 3);
        let now = Instant::now();
        assert!(input.press(at(5, 8), now));
        input.drag(at(6, 4));
        assert_eq!(input.release().as_deref(), Some("two\nthree"));
        assert!(!input.selecting());

        assert!(input.press(at(6, 4), now));
        input.drag(at(5, 4));
        assert_eq!(input.release().as_deref(), Some("one two\nthree"));

        assert!(input.press(at(5, 8), now));
        input.drag(at(9, 40));
        assert_eq!(input.release().as_deref(), Some("two\nthree four"));
        assert!(input.press(at(6, 4), now));
        input.drag(at(0, 0));
        assert_eq!(input.release().as_deref(), Some("one two\nthree"));
    }

    #[test]
    fn a_click_copies_nothing_and_a_double_click_takes_the_word() {
        let mut input = painted("one two", "", 20, 1);
        let now = Instant::now();
        assert!(input.press(at(5, 8), now));
        assert_eq!(input.release(), None);
        assert!(input.press(at(5, 8), now + Duration::from_millis(100)));
        assert_eq!(input.release().as_deref(), Some("two"));
        assert!(input.press(at(5, 8), now + Duration::from_secs(2)));
        assert_eq!(input.release(), None);
    }

    #[test]
    fn a_press_at_the_end_of_the_text_selects_backwards_only() {
        let mut input = painted("one two", "", 20, 1);
        assert!(input.press(at(5, 15), Instant::now()));
        assert_eq!(input.cursor, 7);
        input.drag(at(5, 4));
        assert_eq!(input.release().as_deref(), Some("one two"));

        let mut empty = painted("", "", 20, 1);
        assert!(empty.press(at(5, 3), Instant::now()));
        empty.drag(at(5, 9));
        assert_eq!(empty.release(), None);
    }

    #[test]
    fn the_selection_is_painted_on_its_own_background() {
        let (color_bg, _, color_selection) = colors();
        let mut input = painted("one two", "", 20, 1);
        assert!(input.press(at(5, 3), Instant::now()));
        input.drag(at(5, 4));
        let cells = backgrounds(&mut input);
        let selected: String = cells
            .iter()
            .filter(|(_, bg)| *bg == Some(color_selection))
            .map(|(text, _)| text.as_str())
            .collect();
        assert_eq!(selected, "one");
        assert_eq!(cells[3], (" ".to_string(), Some(color_bg)));
    }

    #[test]
    fn replacing_the_text_drops_the_selection() {
        let mut input = painted("one two", "", 20, 1);
        assert!(input.press(at(5, 3), Instant::now()));
        input.set_text("x");
        assert!(!input.selecting());
        assert!(input.press(at(5, 3), Instant::now()));
        input.clear();
        assert!(!input.selecting());
        assert!(input.press(at(5, 3), Instant::now()));
        input.cancel_select();
        assert_eq!(input.release(), None);
    }

    #[test]
    fn the_mouse_is_the_inputs_only_on_its_cells_or_while_it_selects() {
        let mut input = painted("one two", "", 20, 1);
        let mouse = |kind, pos| MouseInput {
            pos,
            kind,
            maybe_modifier_keys: None,
        };
        let left = Button::Left;
        assert!(!input.handle_mouse(&mouse(MouseInputKind::MouseDrag(left), at(5, 4))));
        assert!(!input.handle_mouse(&mouse(MouseInputKind::MouseUp(left), at(5, 4))));
        assert!(!input.handle_mouse(&mouse(MouseInputKind::MouseDown(left), at(9, 4))));
        assert!(input.handle_mouse(&mouse(MouseInputKind::MouseDown(left), at(5, 4))));
        assert!(input.selecting());
        assert!(input.handle_mouse(&mouse(MouseInputKind::MouseUp(left), at(9, 40))));
        assert!(!input.selecting());
    }
}
