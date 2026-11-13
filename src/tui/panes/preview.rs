#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::time::{Duration, Instant};

const GUTTER_GAP: &str = "   ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragModifier {
    Shift,
    Ctrl,
}

pub struct FilePreviewComponent {
    id: FlexBoxId,
    error: Option<(String, Instant)>,
    drag_snapshot: Option<Vec<(usize, usize)>>,
    drag_start_line: Option<usize>,
    drag_modifier: Option<DragModifier>,
    /// Cached geometry of the content area at last render, used for mouse event handling.
    content_origin_row: usize,
    content_origin_col: usize,
    content_col_count: usize,
    content_row_count: usize,
}

/// How long an error or notice of the file preview's title bar stays up.
const ERROR_SHOWN_FOR: Duration = Duration::from_secs(3);

impl FilePreviewComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self {
            id,
            error: None,
            drag_snapshot: None,
            drag_start_line: None,
            drag_modifier: None,
            content_origin_row: 0,
            content_origin_col: 0,
            content_col_count: 0,
            content_row_count: 0,
        }
    }

    /// Repaints once the error just set has expired. The 1s `Tick` repaints only on
    /// change, so nothing else would take the message off the title bar.
    fn repaint_when_error_expires(
        &self,
        global_data: &GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
    ) {
        use crate::signal::AppSignal;
        if self.error.is_none() {
            return;
        }
        let main_tx = global_data.main_thread_channel_sender.clone();
        tokio::spawn(async move {
            tokio::time::sleep(ERROR_SHOWN_FOR).await;
            crate::signal::try_send(&main_tx, AppSignal::Redraw);
        });
    }

    fn title_text_render(&self, state: &crate::tui::state::AppState) -> Option<(String, bool)> {
        if let Some((msg, set_at)) = &self.error
            && set_at.elapsed() < ERROR_SHOWN_FOR
        {
            return Some((msg.clone(), false));
        }
        let key = self.file_key(state)?;
        let snapshot = state.files.load();
        let file = &snapshot[key.0];
        let removed = file.removed.load(std::sync::atomic::Ordering::Relaxed);
        let rel = file.path.strip_prefix(&state.root).unwrap_or(&file.path);
        let base = if removed {
            format!("[deleted] {rel}")
        } else {
            rel.as_str().to_string()
        };
        let title = if let Some(ranges) = state.highlight_ranges.get(&key) {
            format!(
                "{}  [{}]",
                base,
                crate::tui::pane_manager::format_line_ranges(ranges)
            )
        } else {
            base
        };
        Some((title, removed))
    }

    /// Returns the highlight range (lo, hi) at the given column within the title bar,
    /// or None if no range is at that column or the title is truncated.
    pub fn range_at_title_col(
        &self,
        state: &crate::tui::state::AppState,
        col: usize,
        pane_width: usize,
    ) -> Option<(usize, usize)> {
        let key = self.file_key(state)?;
        let ranges = state.highlight_ranges.get(&key)?;
        if ranges.is_empty() {
            return None;
        }

        let snapshot = state.files.load();
        let file = &snapshot[key.0];
        let rel = file.path.strip_prefix(&state.root).unwrap_or(&file.path);
        let base = if file.removed.load(std::sync::atomic::Ordering::Relaxed) {
            format!("[deleted] {rel}")
        } else {
            rel.as_str().to_string()
        };

        let title = format!(
            "{}  [{}]",
            base,
            crate::tui::pane_manager::format_line_ranges(ranges)
        );
        let padded = format!(" {title} ");
        if padded.len() > pane_width {
            return None;
        }

        let bracket_pos = padded.find('[')?;
        let mut current_col = bracket_pos + 1;
        for &(lo, hi) in ranges.iter() {
            let text = if lo == hi {
                lo.to_string()
            } else {
                format!("{lo}-{hi}")
            };
            let start = current_col;
            let end = current_col + text.len();
            if col >= start && col < end {
                return Some((lo, hi));
            }
            current_col = end + 2; // ", "
        }
        None
    }

    pub fn scroll_to_range(
        &self,
        state: &mut crate::tui::state::AppState,
        key: crate::loader::FileKey,
        lo: usize,
        hi: usize,
    ) {
        use crate::tui::pane_manager::Window;
        let window = Window::FilePreview(key);
        let page_size = state.pane_manager.window_page_size(&window);
        let target = crate::tui::pane_manager::block_scroll(lo, hi - lo + 1, page_size);
        state.pane_manager.set_window_scroll(&window, target);
        state.pane_manager.clamp_scroll(&window);
    }

    /// Commits the in-progress search input for `key`. An empty pattern clears the active search;
    /// an invalid regex shows a transient error and leaves any prior search intact; a valid pattern
    /// becomes the active search and jumps to the first match in the search direction.
    fn commit_search(
        &mut self,
        state: &mut crate::tui::state::AppState,
        key: crate::loader::FileKey,
    ) {
        use crate::tui::state::PreviewInput;
        use crate::tui::state::PreviewSearch;
        let Some(PreviewInput::Search(input, direction)) = state.preview_inputs.get(&key) else {
            return;
        };
        let (pattern, direction) = (input.text().to_string(), *direction);
        if pattern.is_empty() {
            state.search.remove(&key);
            return;
        }
        match regex::Regex::new(&pattern) {
            Ok(regex) => {
                state.search.insert(
                    key,
                    PreviewSearch {
                        pattern,
                        regex,
                        direction,
                        current: None,
                    },
                );
                self.goto_match(state, key, direction);
            }
            Err(e) => {
                self.error = Some((format!("bad regex: {e}"), Instant::now()));
            }
        }
    }

    /// Moves the active search for `key` to the next match in `direction`, wrapping around, and
    /// scrolls it into view. Match positions are collected on demand (only on a keypress).
    fn goto_match(
        &mut self,
        state: &mut crate::tui::state::AppState,
        key: crate::loader::FileKey,
        direction: crate::tui::state::SearchDirection,
    ) {
        use crate::tui::pane_manager::Window;
        use crate::tui::state::SearchDirection;
        // Collect non-empty matches in document order: (line_idx, start_byte, end_byte) per line.
        let matches: Vec<(usize, usize, usize)> = {
            let Some(search) = state.search.get(&key) else {
                return;
            };
            let snapshot = state.files.load();
            let data = snapshot[key.0].data.read();
            let total_lines = data.line_starts.len();
            let mut acc = Vec::new();
            for line_idx in 0..total_lines {
                let line = data.line(line_idx);
                for m in search.regex.find_iter(line) {
                    if m.start() < m.end() {
                        acc.push((line_idx, m.start(), m.end()));
                    }
                }
            }
            acc
        };

        if matches.is_empty() {
            if let Some(search) = state.search.get_mut(&key) {
                search.current = None;
            }
            self.error = Some(("no matches".to_string(), Instant::now()));
            return;
        }

        let current = state.search.get(&key).and_then(|s| s.current);
        let window = Window::FilePreview(key);
        let scroll = state.pane_manager.window_scroll(&window);
        let target_idx = match direction {
            SearchDirection::Forward => match current {
                Some((cl, cs)) => matches
                    .iter()
                    .position(|&(l, s, _)| (l, s) > (cl, cs))
                    .unwrap_or(0),
                None => matches
                    .iter()
                    .position(|&(l, _, _)| l >= scroll)
                    .unwrap_or(0),
            },
            SearchDirection::Backward => match current {
                Some((cl, cs)) => matches
                    .iter()
                    .rposition(|&(l, s, _)| (l, s) < (cl, cs))
                    .unwrap_or(matches.len() - 1),
                None => matches
                    .iter()
                    .rposition(|&(l, _, _)| l <= scroll)
                    .unwrap_or(matches.len() - 1),
            },
        };

        let (line_idx, start, _end) = matches[target_idx];
        if let Some(search) = state.search.get_mut(&key) {
            search.current = Some((line_idx, start));
        }
        self.scroll_to_range(state, key, line_idx + 1, line_idx + 1);
        state.mark_session_dirty();
    }

    #[allow(clippy::too_many_arguments)]
    fn render_input_title(
        ops: &mut RenderOpIRVec,
        input: &mut crate::tui::widgets::input_line::InputLine,
        prompt: &str,
        origin: VPPos,
        width: u16,
        height: usize,
        focused: bool,
        theme: &crate::tui::theme::HelixTheme,
    ) {
        let (bg_rgb, fg_rgb) = crate::tui::widgets::title_row::title_bar_colors(focused, theme);
        let color_bg = TuiColor::from(bg_rgb);
        let color_fg = TuiColor::from(fg_rgb);
        let color_selection = TuiColor::from(crate::tui::widgets::title_row::title_selection_bg(
            focused, theme,
        ));
        let label_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});

        for row_offset in 0..height {
            *ops += RenderOpCommon::MoveCursorPositionRelTo(
                origin,
                vp_col(0) + vp_row(row_offset as u16),
            );
            *ops += RenderOpCommon::SetBgColor(color_bg);
            *ops += RenderOpIR::PaintTextWithAttributes(
                " ".repeat(width as usize).as_str().into(),
                Some(label_style),
            );
        }
        input.render(
            ops,
            None,
            prompt,
            origin,
            width,
            height,
            focused,
            (color_bg, color_fg, color_selection),
        );
    }

    fn is_line_highlighted(
        state: &crate::tui::state::AppState,
        file_key: crate::loader::FileKey,
        line_1_indexed: usize,
    ) -> bool {
        state
            .highlight_ranges
            .get(&file_key)
            .map(|ranges| {
                ranges
                    .iter()
                    .any(|&(lo, hi)| line_1_indexed >= lo && line_1_indexed <= hi)
            })
            .unwrap_or(false)
    }

    /// Returns the `FileKey` this pane slot should render, or `None` if the slot holds a
    /// non-preview window or the stack has no entry for this slot.
    pub(super) fn file_key(
        &self,
        state: &crate::tui::state::AppState,
    ) -> Option<crate::loader::FileKey> {
        use crate::tui::pane_manager::Window;
        let slot = crate::tui::pane_component::pane_slot(self.id)?;
        let Window::FilePreview(key) = state.pane_manager.slot_window(slot)? else {
            return None;
        };
        Some(key)
    }

    pub fn start_drag(
        &mut self,
        state: &crate::tui::state::AppState,
        key: crate::loader::FileKey,
        line: usize,
        modifier: DragModifier,
    ) {
        self.drag_snapshot = state.highlight_ranges.get(&key).cloned();
        self.drag_start_line = Some(line);
        self.drag_modifier = Some(modifier);
    }

    pub fn update_drag(
        &mut self,
        state: &mut crate::tui::state::AppState,
        key: crate::loader::FileKey,
        current_line: usize,
    ) {
        let Some(start) = self.drag_start_line else {
            return;
        };
        let Some(modifier) = self.drag_modifier else {
            return;
        };
        let snapshot: &[(usize, usize)] = self.drag_snapshot.as_deref().unwrap_or(&[]);
        let lo = start.min(current_line);
        let hi = start.max(current_line);
        let new_ranges = match modifier {
            DragModifier::Shift => union_ranges(snapshot, lo, hi),
            DragModifier::Ctrl => subtract_ranges(snapshot, lo, hi),
        };
        if new_ranges.is_empty() {
            state.highlight_ranges.remove(&key);
        } else {
            state.highlight_ranges.insert(key, new_ranges);
        }
    }

    pub fn end_drag(&mut self) {
        self.drag_snapshot = None;
        self.drag_start_line = None;
        self.drag_modifier = None;
    }

    /// Maps a screen position relative to the preview content origin to a
    /// `(line_idx_0_based, char_idx, cursor_byte)` tuple. `cursor_byte` is the
    /// absolute byte offset within `FileData.content`. Accounts for line wrapping.
    pub fn screen_pos_to_line_char(
        &self,
        state: &crate::tui::state::AppState,
        row: usize,
        col: usize,
        key: crate::loader::FileKey,
        data: &parking_lot::RwLockReadGuard<'_, crate::loader::FileData>,
    ) -> (usize, usize, usize) {
        use crate::tui::pane_manager::Window;
        let window = Window::FilePreview(key);
        let scroll = state.pane_manager.window_scroll(&window);

        let total_lines = data.line_starts.len();
        let line_num_width = total_lines.max(1).to_string().len();
        let content_start_col = line_num_width + GUTTER_GAP.len();

        let content_width = self
            .content_col_count
            .saturating_sub(content_start_col)
            .max(1);
        let rel_y = row.saturating_sub(self.content_origin_row);
        let rel_x = col.saturating_sub(self.content_origin_col + content_start_col);

        let mut rendered = 0usize;
        for line_idx in scroll..total_lines {
            let line = data.line(line_idx);
            let char_len = line.chars().count();
            let row_count_for_line = if char_len == 0 {
                1
            } else {
                char_len.div_ceil(content_width)
            };
            if rel_y < rendered + row_count_for_line {
                let sub_row = rel_y - rendered;
                let seg_start_char = sub_row * content_width;
                let char_idx = (seg_start_char + rel_x).min(char_len);
                let cursor_byte =
                    data.line_starts[line_idx] + data.char_to_byte(line_idx, char_idx);
                return (line_idx, char_idx, cursor_byte);
            }
            rendered += row_count_for_line;
            if rendered >= self.content_row_count {
                break;
            }
        }
        let last_line = total_lines.saturating_sub(1);
        let line = data.line(last_line);
        let cursor_byte = data.line_starts[last_line] + line.len();
        (last_line, line.chars().count(), cursor_byte)
    }
}

impl Component<crate::tui::state::AppState, crate::signal::AppSignal> for FilePreviewComponent {
    fn reset(&mut self) {
        self.drag_snapshot = None;
        self.drag_start_line = None;
        self.drag_modifier = None;
    }

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        input_event: InputEvent,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        use crate::signal::AppSignal;
        use crate::tui::command_line::CommandLine;
        use crate::tui::pane_manager::Window;
        use crate::tui::state::PreviewInput;
        use crate::tui::state::SearchDirection;
        use crate::tui::widgets::input_line::InputLine;
        throws_with_return!({
            let Some(key) = self.file_key(&global_data.state) else {
                return Ok(EventPropagation::Propagate);
            };
            let window = Window::FilePreview(key);

            if let Some(PreviewInput::Search(..)) = global_data.state.preview_inputs.get(&key) {
                match input_event {
                    InputEvent::Keyboard(KeyPress::Plain {
                        key: Key::SpecialKey(SpecialKey::Enter),
                    }) => {
                        self.commit_search(&mut global_data.state, key);
                        global_data.state.preview_inputs.remove(&key);
                        self.repaint_when_error_expires(global_data);
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    InputEvent::Keyboard(KeyPress::Plain {
                        key: Key::SpecialKey(SpecialKey::Esc),
                    }) => {
                        global_data.state.search.remove(&key);
                        global_data.state.preview_inputs.remove(&key);
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    InputEvent::Keyboard(KeyPress::WithModifiers {
                        key: Key::Character('c'),
                        mask,
                    }) if mask == ModifierKeysMask::new().with_ctrl() => {
                        if let Some(input) = global_data.state.preview_inputs.get_mut(&key) {
                            input.line_mut().clear();
                        }
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    InputEvent::Keyboard(KeyPress::WithModifiers {
                        key: Key::Character('d'),
                        mask,
                    }) if mask == ModifierKeysMask::new().with_ctrl()
                        && global_data
                            .state
                            .preview_inputs
                            .get(&key)
                            .is_some_and(|input| input.line().text().is_empty()) =>
                    {
                        global_data.state.preview_inputs.remove(&key);
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    _ => {
                        if let Some(input) = global_data.state.preview_inputs.get_mut(&key) {
                            input.line_mut().handle_key(&input_event);
                        }
                        return Ok(EventPropagation::ConsumedRender);
                    }
                }
            }

            if let InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character(':'),
            }) = input_event
            {
                // The highlights are typed on the global command line, which knows this file.
                global_data.state.command_line = Some(CommandLine::prefilled(
                    Some(window),
                    format!("{} ", crate::tui::command_line::HIGHLIGHT),
                ));
                return Ok(EventPropagation::ConsumedRender);
            }

            if let InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character(c @ ('/' | '?')),
            }) = input_event
            {
                let direction = if c == '/' {
                    SearchDirection::Forward
                } else {
                    SearchDirection::Backward
                };
                let prefill = global_data
                    .state
                    .search
                    .get(&key)
                    .map(|s| s.pattern.clone())
                    .unwrap_or_default();
                global_data.state.preview_inputs.insert(
                    key,
                    PreviewInput::Search(InputLine::with_text(prefill), direction),
                );
                return Ok(EventPropagation::ConsumedRender);
            }

            if let InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character(c @ ('n' | 'N')),
            }) = input_event
                && let Some(stored) = global_data.state.search.get(&key).map(|s| s.direction)
            {
                let direction = if c == 'n' {
                    stored
                } else {
                    match stored {
                        SearchDirection::Forward => SearchDirection::Backward,
                        SearchDirection::Backward => SearchDirection::Forward,
                    }
                };
                self.goto_match(&mut global_data.state, key, direction);
                self.repaint_when_error_expires(global_data);
                return Ok(EventPropagation::ConsumedRender);
            }

            if let InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('e'),
            }) = input_event
            {
                let file_path = {
                    let snapshot = global_data.state.files.load();
                    snapshot[key.0].path.clone()
                };
                let cwd = global_data.state.root.clone();
                let editor = std::env::var("VISUAL")
                    .or_else(|_| std::env::var("EDITOR"))
                    .unwrap_or_else(|_| "nvim".into());
                let editor_cmd = format!(
                    "{} {}",
                    editor,
                    crate::tui::command_line::sh_quote(file_path.as_str())
                );
                crate::signal::send(
                    &global_data.main_thread_channel_sender,
                    AppSignal::OpenEditor {
                        cmd: editor_cmd,
                        cwd,
                        file_key: key,
                    },
                );
                return Ok(EventPropagation::ConsumedRender);
            }

            if let InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Backspace),
            }) = input_event
            {
                global_data.state.pane_manager.send_to_back(&window);
                global_data.state.mark_session_dirty();
                return Ok(EventPropagation::ConsumedRender);
            }

            let state = &mut global_data.state;
            if state.pane_manager.scroll_for_input(&window, &input_event) {
                state.mark_session_dirty();
                EventPropagation::ConsumedRender
            } else {
                EventPropagation::Propagate
            }
        });
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        current_box: FlexBox,
        _surface_bounds: VPBoundingBox,
        _has_focus: &mut HasFocus,
    ) -> CommonResult {
        use crate::tui::pane_manager::SelPoint;
        use crate::tui::pane_manager::Window;
        throws!({
            let origin = current_box.style_adjusted_bounds.origin_pos;
            let bounds = current_box.style_adjusted_bounds.bounds_size;
            let visible_rows = bounds.row_height.as_usize();

            self.content_origin_row = origin.row_index.as_usize();
            self.content_origin_col = origin.col_index.as_usize();
            self.content_col_count = bounds.col_width.as_usize();
            self.content_row_count = visible_rows;

            let Some(file_key) = self.file_key(&global_data.state) else {
                return Ok(());
            };

            let window = Window::FilePreview(file_key);
            global_data
                .state
                .pane_manager
                .set_window_page_size(&window, visible_rows);

            let total_lines = {
                let snapshot = global_data.state.files.load();
                let file = &snapshot[file_key.0];
                let data = file.data.read();
                data.line_starts.len()
            };
            global_data
                .state
                .pane_manager
                .set_window_scroll_max(&window, total_lines);
            global_data.state.pane_manager.clamp_scroll(&window);

            let state = &global_data.state;
            let mut render_ops = RenderOpIRVec::new();

            let snapshot = state.files.load();
            let file = &snapshot[file_key.0];

            let data = file.data.read();
            let scroll = state.pane_manager.window_scroll(&window);
            let colored_guard = file.colored_lines.lock();

            let pane_bg = state
                .theme
                .ui_bg("ui.background")
                .unwrap_or(RgbValue::from_u8(15, 15, 25));
            let pane_width = bounds.col_width.as_usize();
            let bg = TuiColor::from(pane_bg);
            let bg_style = new_style!(color_bg: {bg});
            let hl_rgb = state
                .theme
                .ui_bg("ui.selection")
                .unwrap_or(RgbValue::from_u8(50, 50, 90));
            let hl_bg = TuiColor::from(hl_rgb);
            let hl_bg_style = new_style!(color_bg: {hl_bg});
            let line_num_width = (total_lines.max(1)).to_string().len();
            let content_start_col = line_num_width + GUTTER_GAP.len();
            let content_width = pane_width.saturating_sub(content_start_col).max(1);
            let line_num_fg = state.theme.ui_fg("ui.linenr").unwrap_or({
                let default_fg = state
                    .theme
                    .ui_fg("ui.text")
                    .unwrap_or(RgbValue::from_u8(212, 212, 212));
                RgbValue::from_u8(
                    default_fg.red / 3,
                    default_fg.green / 3,
                    default_fg.blue / 3,
                )
            });
            let line_num_bg = state.theme.ui_bg("ui.linenr").unwrap_or(pane_bg);
            let line_num_fg_rgb = TuiColor::from(line_num_fg);
            let line_num_bg_rgb = TuiColor::from(line_num_bg);
            let line_num_style = new_style!(color_fg: {line_num_fg_rgb} color_bg: {bg});
            let hl_fg_rgb = state
                .theme
                .ui_fg("ui.selection")
                .unwrap_or(RgbValue::from_u8(255, 255, 255));
            let hl_line_num_fg = TuiColor::from(hl_fg_rgb);
            let hl_line_num_style =
                new_style!(color_fg: {hl_line_num_fg} color_bg: {line_num_bg_rgb});

            let text_drag_lo;
            let text_drag_hi;
            let text_drag_single;
            let text_drag_single_lo;
            let text_drag_single_hi;
            let has_text_selection = global_data
                .state
                .text_selection
                .as_ref()
                .is_some_and(|sel| sel.window == window && sel.active);
            if has_text_selection {
                let (s, e) = match (
                    global_data.state.text_selection.as_ref().unwrap().start,
                    global_data.state.text_selection.as_ref().unwrap().end,
                ) {
                    (
                        SelPoint::Preview {
                            line_idx: s_line,
                            byte_offset: s_byte,
                        },
                        SelPoint::Preview {
                            line_idx: e_line,
                            byte_offset: e_byte,
                        },
                    ) => ((s_line, s_byte), (e_line, e_byte)),
                    _ => ((0, 0), (0, 0)),
                };
                text_drag_lo = s.0.min(e.0);
                text_drag_hi = s.0.max(e.0);
                text_drag_single = s.0 == e.0;
                text_drag_single_lo = if text_drag_single { s.1.min(e.1) } else { 0 };
                text_drag_single_hi = if text_drag_single { s.1.max(e.1) } else { 0 };
            } else {
                text_drag_lo = 0;
                text_drag_hi = 0;
                text_drag_single = false;
                text_drag_single_lo = 0;
                text_drag_single_hi = 0;
            }

            // The symbol highlight ranges of this file, once per frame rather than once per
            // wrapped sub-row: absolute byte ranges with the colour of their group.
            let symbol_ranges: Vec<(usize, usize, RgbValue)> = state
                .symbol_highlights
                .iter()
                .flat_map(|group| {
                    group
                        .locations
                        .iter()
                        .filter(|loc| loc.file_key == file_key)
                        .map(move |loc| (loc.start_byte, loc.end_byte, group.color))
                })
                .collect();

            let mut overlays: Vec<(usize, usize, RgbValue, Option<RgbValue>)> = Vec::new();
            let mut intervals: Vec<(usize, usize, RgbValue, Option<RgbValue>)> =
                Vec::with_capacity(8);
            let mut rendered = 0usize;
            'rendered: for line_idx in scroll..total_lines {
                let line = data.line(line_idx);
                let line_byte_start = data.line_starts[line_idx];
                let line_byte_end = line_byte_start + line.len();
                let is_text_drag_multi = has_text_selection
                    && !text_drag_single
                    && line_idx >= text_drag_lo
                    && line_idx <= text_drag_hi;
                let is_text_drag_single =
                    has_text_selection && text_drag_single && line_idx == text_drag_lo;
                let is_hl =
                    !has_text_selection && Self::is_line_highlighted(state, file_key, line_idx + 1);
                let row_bg_style = if is_text_drag_multi || is_hl {
                    hl_bg_style
                } else {
                    bg_style
                };
                let row_ln_style = if is_text_drag_multi || is_hl {
                    hl_line_num_style
                } else {
                    line_num_style
                };
                let content_bg = if is_text_drag_multi || is_hl {
                    hl_rgb
                } else {
                    pane_bg
                };

                // What this line paints over its default background, once per line rather than
                // once per sub-row: symbol highlights, then search matches (which win over them),
                // then the selection (which wins over both). Left empty for a fully selected row
                // so the selection's background stays uniform. Line-local bytes.
                overlays.clear();
                if !is_text_drag_multi {
                    for &(start, end, color) in &symbol_ranges {
                        if end <= line_byte_start || start >= line_byte_end {
                            continue;
                        }
                        overlays.push((
                            start.max(line_byte_start) - line_byte_start,
                            end.min(line_byte_end) - line_byte_start,
                            color,
                            None,
                        ));
                    }
                    if let Some(search) = state.search.get(&file_key) {
                        for m in search.regex.find_iter(line) {
                            if m.start() >= m.end() {
                                continue;
                            }
                            let is_current = search.current == Some((line_idx, m.start()));
                            let (bg, fg) = if is_current {
                                (
                                    crate::tui::state::SEARCH_CURRENT,
                                    crate::tui::state::SEARCH_CURRENT_FG,
                                )
                            } else {
                                (
                                    crate::tui::state::SEARCH_MATCH,
                                    crate::tui::state::SEARCH_MATCH_FG,
                                )
                            };
                            overlays.push((m.start(), m.end(), bg, Some(fg)));
                        }
                    }
                    if is_text_drag_single {
                        overlays.push((
                            text_drag_single_lo.saturating_sub(line_byte_start),
                            text_drag_single_hi.saturating_sub(line_byte_start),
                            hl_rgb,
                            None,
                        ));
                    }
                }

                let mut seg_start = 0usize;
                let line_is_ascii = line.is_ascii();
                loop {
                    let seg_end = segment_end(line, seg_start, content_width, line_is_ascii);
                    let is_first_sub = seg_start == 0;

                    render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        vp_col(0) + vp_row(rendered),
                    );
                    render_ops += RenderOpCommon::ApplyColors(Some(row_bg_style));
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        " ".repeat(pane_width).as_str().into(),
                        Some(row_bg_style),
                    );

                    if is_first_sub {
                        let line_num = line_idx + 1;
                        render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                            origin,
                            vp_col(0) + vp_row(rendered),
                        );
                        render_ops += RenderOpCommon::ApplyColors(Some(row_ln_style));
                        let line_num_str =
                            format!("{:>width$}{GUTTER_GAP}", line_num, width = line_num_width);
                        render_ops += RenderOpIR::PaintTextWithAttributes(
                            line_num_str.as_str().into(),
                            Some(row_ln_style),
                        );
                    } else {
                        render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                            origin,
                            vp_col(0) + vp_row(rendered),
                        );
                        render_ops += RenderOpCommon::ApplyColors(Some(row_ln_style));
                        render_ops += RenderOpIR::PaintTextWithAttributes(
                            " ".repeat(line_num_width + GUTTER_GAP.len())
                                .as_str()
                                .into(),
                            Some(row_bg_style),
                        );
                    }

                    render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        vp_col(content_start_col) + vp_row(rendered),
                    );
                    // Each interval is (start, end, bg_color, fg_override), in line-local bytes.
                    // A `Some` fg_override paints a fixed foreground (search matches); `None`
                    // defers to syntax colors via `ensure_readable_fg`. The overlays land in the
                    // order built above, so the later ones win.
                    intervals.clear();
                    intervals.push((seg_start, seg_end, content_bg, None));
                    for &(lo, hi, color, fg) in &overlays {
                        overlay_interval(
                            &mut intervals,
                            seg_start.max(lo),
                            seg_end.min(hi),
                            color,
                            fg,
                        );
                    }

                    for &(i_lo, i_hi, bg_color, fg_override) in &intervals {
                        if i_lo >= i_hi {
                            continue;
                        }
                        paint_line_segment(
                            &mut render_ops,
                            &data,
                            &colored_guard,
                            line_idx,
                            (i_lo, i_hi),
                            &state.theme,
                            bg_color,
                            fg_override,
                        );
                    }

                    rendered += 1;
                    if rendered >= visible_rows {
                        break 'rendered;
                    }
                    seg_start = seg_end;
                    if seg_start >= line.len() {
                        break;
                    }
                }
            }

            global_data.pipeline.push(ZOrder::Normal, render_ops);
        });
    }
}

/// Merge `lo..=hi` with `snapshot`, combining overlapping or adjacent ranges.
fn union_ranges(snapshot: &[(usize, usize)], lo: usize, hi: usize) -> Vec<(usize, usize)> {
    debug_assert!(lo <= hi, "range must be well-formed (lo <= hi)");
    let mut ranges = snapshot.to_vec();
    ranges.push((lo, hi));
    ranges.sort_by_key(|&(s, _)| s);
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for &(s, e) in &ranges {
        if let Some(last) = merged.last_mut()
            && s <= last.1.saturating_add(1)
        {
            last.1 = last.1.max(e);
            continue;
        }
        merged.push((s, e));
    }
    merged
}

/// Remove `lo..=hi` from `snapshot`, splitting partially overlapped ranges.
fn subtract_ranges(snapshot: &[(usize, usize)], lo: usize, hi: usize) -> Vec<(usize, usize)> {
    debug_assert!(lo <= hi, "range must be well-formed (lo <= hi)");
    let mut result = Vec::new();
    for &(r_lo, r_hi) in snapshot {
        if r_hi < lo || r_lo > hi {
            result.push((r_lo, r_hi));
        } else if lo <= r_lo && hi >= r_hi {
            // fully covered, drop
        } else if lo <= r_lo && hi < r_hi {
            result.push((hi + 1, r_hi));
        } else if lo > r_lo && hi >= r_hi {
            result.push((r_lo, lo - 1));
        } else {
            // splits
            result.push((r_lo, lo - 1));
            result.push((hi + 1, r_hi));
        }
    }
    result
}

impl crate::tui::widgets::title_row::TitleRow for FilePreviewComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        state: &mut crate::tui::state::AppState,
    ) -> usize {
        use crate::tui::state::PreviewInput;
        use crate::tui::state::SearchDirection;
        let theme = &state.theme;
        let input = self
            .file_key(state)
            .and_then(|key| state.preview_inputs.get_mut(&key));
        if let Some(input) = input {
            let prompt = match input {
                PreviewInput::Search(_, SearchDirection::Forward) => "/",
                PreviewInput::Search(_, SearchDirection::Backward) => "?",
            };
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
            let height = input.line().height(None, prompt, width, max_rows);
            Self::render_input_title(
                ops,
                input.line_mut(),
                prompt,
                origin,
                width,
                height,
                focused,
                theme,
            );
            height
        } else {
            let (title, removed) = self
                .title_text_render(state)
                .unwrap_or_else(|| (String::new(), false));
            crate::tui::widgets::title_row::render_pane_title(
                ops, pane_box, &title, removed, theme, focused,
            );
            1
        }
    }
}

#[allow(clippy::too_many_arguments)] // cohesive per-line paint context; single call site
fn paint_line_segment(
    render_ops: &mut RenderOpIRVec,
    data: &crate::loader::FileData,
    colored_guard: &[crate::highlight::ColoredLine],
    line_idx: usize,
    (seg_start_byte, seg_end_byte): (usize, usize),
    theme: &crate::tui::theme::HelixTheme,
    pane_bg: RgbValue,
    fg_override: Option<RgbValue>,
) {
    if seg_start_byte >= seg_end_byte {
        return;
    }
    let line_content = data.line(line_idx);
    // An offset can outlive the content it was computed against: a highlight of a file an
    // external editor just rewrote reaches this frame still holding the old bytes. Clamping to
    // the line's length is not enough, a stale offset can land inside a character.
    let seg_start_byte = floor_to_char(line_content, seg_start_byte);
    let seg_end_byte = ceil_to_char(line_content, seg_end_byte);
    if seg_start_byte >= seg_end_byte {
        return;
    }
    let default_fg = theme
        .ui_fg("ui.text")
        .unwrap_or(RgbValue::from_u8(212, 212, 212));
    let bg = TuiColor::from(pane_bg);

    // A fixed foreground (search matches) paints the whole segment as one run, ignoring syntax
    // spans and the `ensure_readable_fg` auto-contrast so the match text color is uniform.
    if let Some(fg_rgb) = fg_override {
        let text = &line_content[seg_start_byte..seg_end_byte];
        let fg = TuiColor::from(fg_rgb);
        let style = new_style!(color_fg: {fg} color_bg: {bg});
        *render_ops += RenderOpCommon::ApplyColors(Some(style));
        *render_ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
        return;
    }

    if let Some(spans) = colored_guard.get(line_idx) {
        for span in spans {
            let overlap_start = floor_to_char(line_content, span.start.max(seg_start_byte));
            let overlap_end = ceil_to_char(line_content, span.end.min(seg_end_byte));
            if overlap_start >= overlap_end {
                continue;
            }
            let text = &line_content[overlap_start..overlap_end];
            let fg_rgb = theme.token_color(span.kind).unwrap_or(default_fg);
            let fg_rgb = ensure_readable_fg(fg_rgb, pane_bg);
            let fg = TuiColor::from(fg_rgb);
            let style = new_style!(color_fg: {fg} color_bg: {bg});
            *render_ops += RenderOpCommon::ApplyColors(Some(style));
            *render_ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
        }
        return;
    }

    let text = &line_content[seg_start_byte..seg_end_byte];
    let fg_rgb = ensure_readable_fg(default_fg, pane_bg);
    let fg = TuiColor::from(fg_rgb);
    let style = new_style!(color_fg: {fg} color_bg: {bg});
    *render_ops += RenderOpCommon::ApplyColors(Some(style));
    *render_ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
}

/// `offset` moved back to the start of the character it falls inside.
fn floor_to_char(line: &str, offset: usize) -> usize {
    let mut offset = offset.min(line.len());
    while !line.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// `offset` moved past the character it falls inside, so a range whose ends both fall within
/// one character still covers it rather than collapsing to nothing.
fn ceil_to_char(line: &str, offset: usize) -> usize {
    let mut offset = offset.min(line.len());
    while offset < line.len() && !line.is_char_boundary(offset) {
        offset += 1;
    }
    offset
}

/// Where the sub-row starting at the byte offset `start` of `line` ends: `width` cells on,
/// by bytes on an ASCII line and by whole chars otherwise, clamped to the line. The caller
/// passes the answer to `line.is_ascii()`, which is linear in the line and so must not be
/// asked once per sub-row of a long one.
fn segment_end(line: &str, start: usize, width: usize, line_is_ascii: bool) -> usize {
    if line_is_ascii {
        (start + width).min(line.len())
    } else {
        advance_chars(line, start, width)
    }
}

/// The byte offset `chars` characters past `from`, clamped to the end of `line`.
fn advance_chars(line: &str, from: usize, chars: usize) -> usize {
    match line[from..].char_indices().nth(chars) {
        Some((offset, _)) => from + offset,
        None => line.len(),
    }
}

/// Perceived brightness on a 0-255 scale (standard luma weights). Good enough for
/// picking readable terminal text; not meant to be WCAG-certified color science.
fn luminance(c: RgbValue) -> f32 {
    0.299 * c.red as f32 + 0.587 * c.green as f32 + 0.114 * c.blue as f32
}

/// Minimum acceptable luminance gap between text and its background, on the same
/// 0-255 scale as `luminance`. Chosen empirically against the symbol-highlight
/// palette (`SYMBOL_PALETTE`), several of whose colors sit close in perceived
/// brightness to common syntax-highlight foregrounds despite differing in hue.
const MIN_FG_BG_CONTRAST: f32 = 100.0;

/// Returns `fg` unchanged if it contrasts adequately against `bg`; otherwise swaps
/// to near-black or near-white, whichever contrasts better with `bg`. Applied to
/// every span so it transparently covers symbol-highlight and selection overlay
/// backgrounds (`overlay_interval` in `render()`) as well as the normal pane
/// background, without either call site needing to know which case it is.
pub(super) fn ensure_readable_fg(fg: RgbValue, bg: RgbValue) -> RgbValue {
    if (luminance(fg) - luminance(bg)).abs() >= MIN_FG_BG_CONTRAST {
        return fg;
    }
    if luminance(bg) > 127.0 {
        RgbValue::from_u8(20, 20, 20)
    } else {
        RgbValue::from_u8(235, 235, 235)
    }
}

/// Repaints `o_lo..o_hi` in `intervals` with `color`, splitting partially
/// overlapped pieces. No-op when the range is empty.
fn overlay_interval(
    intervals: &mut Vec<(usize, usize, RgbValue, Option<RgbValue>)>,
    o_lo: usize,
    o_hi: usize,
    color: RgbValue,
    fg: Option<RgbValue>,
) {
    if o_lo >= o_hi {
        return;
    }
    let mut result = Vec::with_capacity(intervals.len() + 2);
    for &(i_lo, i_hi, i_color, i_fg) in intervals.iter() {
        if i_hi <= o_lo || i_lo >= o_hi {
            result.push((i_lo, i_hi, i_color, i_fg));
        } else {
            if i_lo < o_lo {
                result.push((i_lo, o_lo, i_color, i_fg));
            }
            let lo = i_lo.max(o_lo);
            let hi = i_hi.min(o_hi);
            if lo < hi {
                result.push((lo, hi, color, fg));
            }
            if i_hi > o_hi {
                result.push((o_hi, i_hi, i_color, i_fg));
            }
        }
    }
    *intervals = result;
}

/// Formats highlight ranges as the canonical comma-separated string (single line `N`,
/// range `lo-hi`, joined with `, `). Used for the pane title, title-bar hit testing, and
/// the `highlight` command's hint.
#[cfg(test)]
mod contrast_tests {
    use super::*;

    #[test]
    fn keeps_fg_when_contrast_is_already_adequate() {
        // White text on a near-black background: plenty of contrast, unchanged.
        assert_eq!(
            ensure_readable_fg(
                RgbValue::from_u8(230, 230, 230),
                RgbValue::from_u8(10, 10, 10),
            ),
            RgbValue::from_u8(230, 230, 230)
        );
    }

    #[test]
    fn overrides_fg_when_luminance_nearly_matches() {
        // Yellow-ish syntax fg on a similarly bright symbol-highlight orange bg
        // (SYMBOL_PALETTE[2] = [255, 200, 100]) — nearly identical luminance.
        let fg = RgbValue::from_u8(255, 255, 100);
        let bg = RgbValue::from_u8(255, 200, 100);
        let result = ensure_readable_fg(fg, bg);
        assert_ne!(result, fg);
        assert!((luminance(result) - luminance(bg)).abs() >= MIN_FG_BG_CONTRAST);
    }

    #[test]
    fn overridden_fg_picks_dark_text_on_light_bg_and_light_text_on_dark_bg() {
        assert_eq!(
            ensure_readable_fg(
                RgbValue::from_u8(255, 255, 100),
                RgbValue::from_u8(255, 200, 100),
            ),
            RgbValue::from_u8(20, 20, 20)
        );
        assert_eq!(
            ensure_readable_fg(RgbValue::from_u8(50, 50, 50), RgbValue::from_u8(30, 30, 30),),
            RgbValue::from_u8(235, 235, 235)
        );
    }
}

#[cfg(test)]
mod segment_tests {
    use super::*;

    /// How a line is cut into painted rows at `width` cells: the byte ranges of its segments.
    fn segments(line: &str, width: usize) -> Vec<(usize, usize)> {
        let mut segments = Vec::new();
        let line_is_ascii = line.is_ascii();
        let mut start = 0usize;
        loop {
            let end = segment_end(line, start, width, line_is_ascii);
            segments.push((start, end));
            if start >= line.len() || end >= line.len() {
                break;
            }
            start = end;
        }
        segments
    }

    const LINES: [&str; 7] = [
        "plain ascii line",
        "héllo wörld ünicode",
        "日本語のテキストです",
        "e\u{301} combining accent",
        "👨‍👩‍👧‍👦 family emoji and more",
        "",
        "x",
    ];

    #[test]
    fn segments_partition_the_line_and_never_split_a_char() {
        for line in LINES {
            for width in [1, 2, 3, 5, 7, 80] {
                let segments = segments(line, width);
                assert_eq!(
                    segments.first().map(|s| s.0),
                    Some(0),
                    "{line:?} at {width}"
                );
                assert_eq!(
                    segments.last().map(|s| s.1),
                    Some(line.len()),
                    "{line:?} at {width}"
                );
                for window in segments.windows(2) {
                    assert_eq!(window[0].1, window[1].0, "{line:?} at {width}: a gap");
                }
                for &(start, end) in &segments {
                    assert!(
                        start <= end,
                        "{line:?} at {width}: {start}..{end} is backwards"
                    );
                    assert!(
                        start < end || line.is_empty(),
                        "{line:?} at {width}: {start}..{end} is empty"
                    );
                    assert!(line.is_char_boundary(start), "{line:?} at {width}: {start}");
                    assert!(line.is_char_boundary(end), "{line:?} at {width}: {end}");
                }
            }
        }
    }

    #[test]
    fn a_width_past_the_line_paints_it_in_one_segment() {
        for line in LINES {
            assert_eq!(segments(line, 200), vec![(0, line.len())], "{line:?}");
        }
    }

    #[test]
    fn a_segment_of_a_multibyte_line_holds_whole_chars() {
        let line = "héllo wörld";
        for &(start, end) in &segments(line, 3) {
            let text = &line[start..end];
            assert!(
                !text.is_empty() || line.is_empty(),
                "{start}..{end} of {line:?} is empty"
            );
            // a split char would have made the slice panic above; this pins the widths
            assert!(
                line[start..end].chars().count() <= 3,
                "{line:?}: {start}..{end}"
            );
        }
    }

    #[test]
    fn an_overlay_splits_the_segment_it_lands_in() {
        let marked = RgbValue::from_u8(1, 2, 3);
        let plain = RgbValue::default();
        let mut intervals = vec![(4, 9, plain, None)];
        overlay_interval(&mut intervals, 6, 20, marked, None);
        assert_eq!(intervals, vec![(4, 6, plain, None), (6, 9, marked, None)]);
    }

    #[test]
    fn an_overlay_that_misses_the_segment_leaves_it_alone() {
        let marked = RgbValue::from_u8(1, 2, 3);
        let plain = RgbValue::default();
        let mut intervals = vec![(0, 4, plain, None)];
        overlay_interval(&mut intervals, 4, 9, marked, None);
        assert_eq!(intervals, vec![(0, 4, plain, None)]);
    }

    #[test]
    fn a_later_overlay_wins_over_an_earlier_one() {
        let symbol = RgbValue::from_u8(1, 2, 3);
        let search = RgbValue::from_u8(4, 5, 6);
        let plain = RgbValue::default();
        let mut intervals = vec![(0, 10, plain, None)];
        overlay_interval(&mut intervals, 2, 8, symbol, None);
        overlay_interval(&mut intervals, 4, 6, search, Some(search));
        assert_eq!(
            intervals,
            vec![
                (0, 2, plain, None),
                (2, 4, symbol, None),
                (4, 6, search, Some(search)),
                (6, 8, symbol, None),
                (8, 10, plain, None),
            ]
        );
    }
}
#[cfg(test)]
mod paint_tests {
    use super::*;

    fn painted_text(ops: &RenderOpIRVec) -> Vec<String> {
        ops.list
            .iter()
            .filter_map(|op| match op {
                RenderOpIR::PaintTextWithAttributes(text, _) => Some(text.to_string()),
                _ => None,
            })
            .collect()
    }

    fn data(content: &str) -> crate::loader::FileData {
        let mut line_starts = vec![0usize];
        for (i, &byte) in content.as_bytes().iter().enumerate() {
            if byte == b'\n' && i + 1 < content.len() {
                line_starts.push(i + 1);
            }
        }
        crate::loader::FileData {
            content: content.to_string(),
            line_starts,
        }
    }

    #[test]
    fn a_multibyte_segment_paints_whole_chars_and_their_text() {
        let file = data("héllo wörld ünicode");
        let theme = crate::tui::theme::HelixTheme::default();
        let mut ops = RenderOpIRVec::new();
        // the segments the render loop cuts at width 5, painted one after the other
        let line = file.line(0);
        let line_is_ascii = line.is_ascii();
        let mut start = 0usize;
        while start < line.len() {
            let end = segment_end(line, start, 5, line_is_ascii);
            paint_line_segment(
                &mut ops,
                &file,
                &[],
                0,
                (start, end),
                &theme,
                RgbValue::from_u8(0, 0, 0),
                None,
            );
            start = end;
        }
        let text = painted_text(&ops).concat();
        assert_eq!(text, "héllo wörld ünicode");
    }

    #[test]
    fn a_search_match_paints_its_text_in_one_run() {
        let file = data("fn main() { target(); }");
        let theme = crate::tui::theme::HelixTheme::default();
        let mut ops = RenderOpIRVec::new();
        let before = "fn main() { ".len();
        paint_line_segment(
            &mut ops,
            &file,
            &[],
            0,
            (0, before),
            &theme,
            RgbValue::from_u8(0, 0, 0),
            None,
        );
        paint_line_segment(
            &mut ops,
            &file,
            &[],
            0,
            (before, before + "target".len()),
            &theme,
            RgbValue::from_u8(0, 0, 0),
            Some(RgbValue::from_u8(255, 255, 0)),
        );
        assert_eq!(painted_text(&ops).concat(), "fn main() { target");
        // the match is one run, the plain text another
        assert_eq!(painted_text(&ops).len(), 2);
    }

    #[test]
    fn an_empty_segment_paints_nothing() {
        let file = data("some text");
        let theme = crate::tui::theme::HelixTheme::default();
        let mut ops = RenderOpIRVec::new();
        paint_line_segment(
            &mut ops,
            &file,
            &[],
            0,
            (3, 3),
            &theme,
            RgbValue::from_u8(0, 0, 0),
            None,
        );
        assert!(ops.list.is_empty());
    }

    /// A highlight computed against an older content of the line reaches the frame holding
    /// offsets that no longer name character boundaries. Painting has to cut at one rather
    /// than panic: this is what an external edit does to a highlighted file, and 1..2 are
    /// both inside the three bytes of an em dash.
    #[test]
    fn an_offset_inside_a_character_paints_rather_than_panicking() {
        use crate::highlight::TokenKind;
        let file = data("— dash");
        let theme = crate::tui::theme::HelixTheme::default();

        let mut ops = RenderOpIRVec::new();
        paint_line_segment(
            &mut ops,
            &file,
            &[],
            0,
            (0, 1),
            &theme,
            RgbValue::from_u8(0, 0, 0),
            None,
        );
        assert_eq!(painted_text(&ops).concat(), "—");

        let mut ops = RenderOpIRVec::new();
        paint_line_segment(
            &mut ops,
            &file,
            &[],
            0,
            (1, 2),
            &theme,
            RgbValue::from_u8(0, 0, 0),
            None,
        );
        assert_eq!(painted_text(&ops).concat(), "—");

        // a search match, which paints its segment as one fixed-foreground run
        let mut ops = RenderOpIRVec::new();
        paint_line_segment(
            &mut ops,
            &file,
            &[],
            0,
            (0, 1),
            &theme,
            RgbValue::from_u8(0, 0, 0),
            Some(RgbValue::from_u8(255, 255, 0)),
        );
        assert_eq!(painted_text(&ops).concat(), "—");

        // a syntax span whose offsets came from the server for an older content
        let spans = [vec![crate::highlight::ColoredSpan {
            start: 1,
            end: 2,
            kind: TokenKind::Keyword,
        }]];
        let mut ops = RenderOpIRVec::new();
        paint_line_segment(
            &mut ops,
            &file,
            &spans,
            0,
            (0, file.content.len()),
            &theme,
            RgbValue::from_u8(0, 0, 0),
            None,
        );
        assert_eq!(painted_text(&ops).concat(), "—");
    }
}
