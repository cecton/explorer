use crate::loader::{FileData, FileKey};
use crate::tui::*;
use camino::{Utf8Path, Utf8PathBuf};
use std::time::{Duration, Instant};

const GUTTER_GAP: &str = "   ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragModifier {
    Shift,
    Ctrl,
}

pub struct FilePreviewComponent {
    id: FlexBoxId,
    command_mode: Option<String>,
    command_input: InputLine,
    error: Option<(String, Instant)>,
    drag_snapshot: Option<Vec<(usize, usize)>>,
    drag_start_line: Option<usize>,
    drag_modifier: Option<DragModifier>,
    text_drag_active: bool,
    text_drag_start: Option<(usize, usize)>,
    text_drag_end: Option<(usize, usize)>,
    /// Cached geometry of the content area at last render, used for mouse event handling.
    content_origin_row: usize,
    content_origin_col: usize,
    content_col_count: usize,
    content_row_count: usize,
}

impl FilePreviewComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self {
            id,
            command_mode: None,
            command_input: InputLine::new(),
            error: None,
            drag_snapshot: None,
            drag_start_line: None,
            drag_modifier: None,
            text_drag_active: false,
            text_drag_start: None,
            text_drag_end: None,
            content_origin_row: 0,
            content_origin_col: 0,
            content_col_count: 0,
            content_row_count: 0,
        }
    }

    pub fn title_text(&mut self, state: &AppState) -> String {
        if let Some((msg, set_at)) = &self.error {
            if set_at.elapsed() < Duration::from_secs(3) {
                return msg.clone();
            }
            self.error = None;
        }
        let Some(key) = self.file_key(state) else {
            return String::new();
        };
        let snapshot = state.files.load();
        let file = &snapshot[key.0];
        let rel = file.path.strip_prefix(&state.root).unwrap_or(&file.path);
        let base = if file.removed.load(std::sync::atomic::Ordering::Relaxed) {
            format!("[deleted] {rel}")
        } else {
            rel.as_str().to_string()
        };
        if let Some(ranges) = state.highlight_ranges.get(&key) {
            let groups: Vec<String> = ranges
                .iter()
                .map(|&(lo, hi)| {
                    if lo == hi {
                        lo.to_string()
                    } else {
                        format!("{lo}-{hi}")
                    }
                })
                .collect();
            format!("{}  [{}]", base, groups.join(", "))
        } else {
            base
        }
    }

    /// Returns the highlight range (lo, hi) at the given column within the title bar,
    /// or None if no range is at that column or the title is truncated.
    pub fn range_at_title_col(
        &self,
        state: &AppState,
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

        let groups: Vec<String> = ranges
            .iter()
            .map(|&(lo, hi)| {
                if lo == hi {
                    lo.to_string()
                } else {
                    format!("{lo}-{hi}")
                }
            })
            .collect();
        let title = format!("{}  [{}]", base, groups.join(", "));
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

    pub fn scroll_to_range(&self, state: &mut AppState, key: FileKey, lo: usize, hi: usize) {
        let window = Window::FilePreview(key);
        let page_size = state.window_page_size(&window);
        let target = compute_single_block_scroll(lo, hi - lo + 1, page_size);
        state.set_window_scroll(&window, target);
        state.clamp_scroll(&window);
    }

    pub fn render_title_row(
        &self,
        mut ops: &mut RenderOpIRVec,
        origin: Pos,
        width_u16: u16,
        focused: bool,
        theme: &HelixTheme,
    ) -> bool {
        let Some(cmd) = self.command_mode.as_deref() else {
            return false;
        };
        let (bg_rgb, fg_rgb) = title_bar_colors(focused, theme);
        let color_bg = tui_color!(bg_rgb[0], bg_rgb[1], bg_rgb[2]);
        let color_fg = tui_color!(fg_rgb[0], fg_rgb[1], fg_rgb[2]);
        let label_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});

        ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
        ops += RenderOpCommon::SetBgColor(color_bg);
        ops += RenderOpIR::PaintTextWithAttributes(
            " ".repeat(width_u16 as usize).as_str().into(),
            Some(label_style),
        );
        ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
        ops += RenderOpIR::PaintTextWithAttributes(":".into(), Some(label_style));
        self.command_input.render(
            ops,
            cmd,
            origin + width(1),
            width_u16.saturating_sub(1),
            focused,
            (color_bg, color_fg),
        );
        true
    }

    fn execute_command(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        window: &Window,
    ) {
        let Window::FilePreview(file_key) = window else {
            return;
        };
        let Some(cmd) = self.command_mode.clone() else {
            return;
        };

        if let Some(shell_cmd) = cmd.strip_prefix('!') {
            let file_path = {
                let snapshot = global_data.state.files.load();
                snapshot[file_key.0].path.clone()
            };
            let cwd = global_data.state.root.clone();
            let open_cmd = if shell_cmd.is_empty() {
                Ok(None)
            } else {
                expand_placeholders(shell_cmd, &file_path, &global_data.state.root).map(Some)
            };
            match open_cmd {
                Ok(cmd_opt) => {
                    send_signal!(
                        global_data.main_thread_channel_sender,
                        TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::OpenTerminal {
                            cmd: cmd_opt,
                            cwd,
                        })
                    );
                }
                Err(msg) => {
                    self.error = Some((msg, Instant::now()));
                }
            }
            return;
        }

        let state = &mut global_data.state;
        let max = state.window_scroll_max(window);
        if max == 0 {
            return;
        }
        let mut ranges = Vec::new();
        for spec in cmd.split(',') {
            let spec = spec.trim();
            if spec.is_empty() {
                continue;
            }
            if let Some((start_str, end_str)) = spec.split_once('-') {
                let Ok(start) = start_str.trim().parse::<usize>() else {
                    continue;
                };
                let Ok(end) = end_str.trim().parse::<usize>() else {
                    continue;
                };
                if start > 0 && end > 0 {
                    let lo = start.min(end).min(max);
                    let hi = start.max(end).min(max);
                    ranges.push((lo, hi));
                }
            } else if let Ok(n) = spec.trim().parse::<usize>()
                && n > 0
                && n <= max
            {
                ranges.push((n, n));
            }
        }

        if ranges.is_empty() {
            return;
        }

        state.highlight_ranges.insert(*file_key, ranges);

        let page_size = state.window_page_size(window);
        if let Some(ranges) = state.highlight_ranges.get(file_key) {
            let target = compute_scroll_target(ranges, page_size, max);
            state.set_window_scroll(window, target);
            state.clamp_scroll(window);
        }
    }

    fn is_line_highlighted(state: &AppState, file_key: FileKey, line_1_indexed: usize) -> bool {
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
    pub(super) fn file_key(&self, state: &AppState) -> Option<FileKey> {
        let slot = pane_slot(self.id)?;
        let Window::FilePreview(key) = state.window_stack.get(slot)? else {
            return None;
        };
        Some(*key)
    }

    pub fn start_drag(
        &mut self,
        state: &AppState,
        key: FileKey,
        line: usize,
        modifier: DragModifier,
    ) {
        self.drag_snapshot = state.highlight_ranges.get(&key).cloned();
        self.drag_start_line = Some(line);
        self.drag_modifier = Some(modifier);
    }

    pub fn update_drag(&mut self, state: &mut AppState, key: FileKey, current_line: usize) {
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

    pub fn start_text_drag(&mut self, line: usize, byte_offset: usize) {
        self.text_drag_active = true;
        self.text_drag_start = Some((line, byte_offset));
        self.text_drag_end = Some((line, byte_offset));
    }

    pub fn update_text_drag(&mut self, line: usize, byte_offset: usize) {
        self.text_drag_end = Some((line, byte_offset));
    }

    pub fn end_text_drag(&mut self) -> Option<((usize, usize), (usize, usize))> {
        self.text_drag_active = false;
        let result = self.text_drag_start.zip(self.text_drag_end);
        self.text_drag_start = None;
        self.text_drag_end = None;
        result
    }

    /// Start a text drag from an absolute mouse position in the pane.
    /// `click_count` is used for double/triple-click word/line selection.
    /// Returns true if a drag was started.
    pub fn start_text_drag_from_pos(
        &mut self,
        state: &AppState,
        row: usize,
        col: usize,
        click_count: u8,
    ) -> bool {
        let key = match self.file_key(state) {
            Some(k) => k,
            None => return false,
        };

        let snapshot = state.files.load();
        let file = &snapshot[key.0];
        let data = file.data.lock().unwrap();

        let (line_idx, _char_idx, cursor_byte) =
            self.screen_pos_to_line_char(state, row, col, key, &data);

        let (sel_start, sel_end) = match click_count {
            2 => data.word_bounds(cursor_byte),
            3.. => data.line_bounds(line_idx),
            _ => (cursor_byte, cursor_byte),
        };

        self.start_text_drag(line_idx, sel_start);
        self.update_text_drag(line_idx, sel_end);
        true
    }

    pub fn update_text_drag_from_pos(&mut self, state: &AppState, row: usize, col: usize) {
        let key = match self.file_key(state) {
            Some(k) => k,
            None => return,
        };

        let snapshot = state.files.load();
        let file = &snapshot[key.0];
        let data = file.data.lock().unwrap();

        let (line_idx, _char_idx, cursor_byte) =
            self.screen_pos_to_line_char(state, row, col, key, &data);

        self.update_text_drag(line_idx, cursor_byte);
    }

    /// Maps a screen position relative to the preview content origin to a
    /// `(line_idx_0_based, char_idx, cursor_byte)` tuple. `cursor_byte` is the
    /// absolute byte offset within `FileData.content`. Accounts for line wrapping.
    fn screen_pos_to_line_char(
        &self,
        state: &AppState,
        row: usize,
        col: usize,
        key: FileKey,
        data: &std::sync::MutexGuard<'_, crate::loader::FileData>,
    ) -> (usize, usize, usize) {
        let window = Window::FilePreview(key);
        let scroll = state.window_scroll(&window);

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

    pub fn end_text_drag_with_text(&mut self, state: &AppState) -> Option<String> {
        let key = self.file_key(state)?;
        let ((_, start_byte), (_, end_byte)) = self.end_text_drag()?;
        let snapshot = state.files.load();
        let file = &snapshot[key.0];
        let data = file.data.lock().unwrap();
        data.extract_text(start_byte, end_byte)
    }
}

/// Maps a pane `FlexBoxId` back to its zero-based slot index.
fn pane_slot(id: FlexBoxId) -> Option<usize> {
    match id.inner {
        x if x == Id::Pane0 as u8 => Some(0),
        x if x == Id::Pane1 as u8 => Some(1),
        x if x == Id::Pane2 as u8 => Some(2),
        x if x == Id::Pane3 as u8 => Some(3),
        x if x == Id::Pane4 as u8 => Some(4),
        _ => None,
    }
}

impl Component<AppState, AppSignal> for FilePreviewComponent {
    fn reset(&mut self) {
        self.command_mode = None;
        self.drag_snapshot = None;
        self.drag_start_line = None;
        self.drag_modifier = None;
        self.text_drag_active = false;
        self.text_drag_start = None;
        self.text_drag_end = None;
    }

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        input_event: InputEvent,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        throws_with_return!({
            let Some(key) = self.file_key(&global_data.state) else {
                return Ok(EventPropagation::Propagate);
            };
            let window = Window::FilePreview(key);

            if self.command_mode.is_some() {
                match input_event {
                    InputEvent::Keyboard(KeyPress::Plain {
                        key: Key::SpecialKey(SpecialKey::Enter),
                    }) => {
                        self.execute_command(global_data, &window);
                        self.command_mode = None;
                        global_data.state.command_mode_active = false;
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    InputEvent::Keyboard(KeyPress::Plain {
                        key: Key::SpecialKey(SpecialKey::Esc),
                    }) => {
                        self.command_mode = None;
                        global_data.state.command_mode_active = false;
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    InputEvent::Keyboard(KeyPress::WithModifiers {
                        key: Key::Character('c'),
                        mask,
                    }) if mask == ModifierKeysMask::new().with_ctrl() => {
                        self.command_mode = None;
                        global_data.state.command_mode_active = false;
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    InputEvent::Keyboard(KeyPress::WithModifiers {
                        key: Key::Character('d'),
                        mask,
                    }) if mask == ModifierKeysMask::new().with_ctrl()
                        && self.command_mode.as_deref() == Some("") =>
                    {
                        self.command_mode = None;
                        global_data.state.command_mode_active = false;
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    _ => {
                        let query = self.command_mode.as_mut().unwrap();
                        self.command_input.handle_key(&input_event, query);
                        return Ok(EventPropagation::ConsumedRender);
                    }
                }
            }

            if let InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character(':'),
            }) = input_event
            {
                self.command_mode = Some(String::new());
                global_data.state.command_mode_active = true;
                return Ok(EventPropagation::ConsumedRender);
            }

            let state = &mut global_data.state;
            let mut consumed = false;
            if let InputEvent::Keyboard(KeyPress::Plain { key: kb_key }) = input_event {
                match kb_key {
                    Key::SpecialKey(SpecialKey::PageUp) => {
                        consumed = true;
                        let page = state.window_page_size(&window);
                        let current = state.window_scroll(&window);
                        state.set_window_scroll(&window, current.saturating_sub(page));
                        state.clamp_scroll(&window);
                    }
                    Key::SpecialKey(SpecialKey::PageDown) => {
                        consumed = true;
                        let page = state.window_page_size(&window);
                        let current = state.window_scroll(&window);
                        state.set_window_scroll(&window, current.saturating_add(page));
                        state.clamp_scroll(&window);
                    }
                    Key::SpecialKey(SpecialKey::Up) => {
                        consumed = true;
                        let current = state.window_scroll(&window);
                        state.set_window_scroll(&window, current.saturating_sub(1));
                        state.clamp_scroll(&window);
                    }
                    Key::SpecialKey(SpecialKey::Down) => {
                        consumed = true;
                        let current = state.window_scroll(&window);
                        state.set_window_scroll(&window, current.saturating_add(1));
                        state.clamp_scroll(&window);
                    }
                    Key::SpecialKey(SpecialKey::Home) => {
                        consumed = true;
                        state.set_window_scroll(&window, 0);
                    }
                    Key::SpecialKey(SpecialKey::End) => {
                        consumed = true;
                        let max = state.window_scroll_max(&window);
                        state.set_window_scroll(&window, max);
                        state.clamp_scroll(&window);
                    }
                    _ => {}
                }
            }
            if let InputEvent::Keyboard(KeyPress::WithModifiers {
                key: modifier_key,
                mask: modifier_mask,
            }) = input_event
                && modifier_mask == ModifierKeysMask::new().with_ctrl()
            {
                match modifier_key {
                    Key::Character('a') => {
                        consumed = true;
                        state.set_window_scroll(&window, 0);
                    }
                    Key::Character('e') => {
                        consumed = true;
                        let max = state.window_scroll_max(&window);
                        state.set_window_scroll(&window, max);
                        state.clamp_scroll(&window);
                    }
                    _ => {}
                }
            }
            if let InputEvent::Mouse(mouse) = input_event {
                match mouse.kind {
                    MouseInputKind::ScrollUp => {
                        consumed = true;
                        let current = state.window_scroll(&window);
                        state.set_window_scroll(&window, current.saturating_sub(3));
                        state.clamp_scroll(&window);
                    }
                    MouseInputKind::ScrollDown => {
                        consumed = true;
                        let current = state.window_scroll(&window);
                        state.set_window_scroll(&window, current.saturating_add(3));
                        state.clamp_scroll(&window);
                    }
                    _ => {}
                }
            }
            if consumed {
                EventPropagation::ConsumedRender
            } else {
                EventPropagation::Propagate
            }
        });
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
            let visible_rows = bounds.row_height.as_usize();

            self.content_origin_row = origin.row_index.as_usize();
            self.content_origin_col = origin.col_index.as_usize();
            self.content_col_count = bounds.col_width.as_usize();
            self.content_row_count = visible_rows;

            let Some(file_key) = self.file_key(&global_data.state) else {
                let mut pipeline = render_pipeline!();
                pipeline.push(ZOrder::Normal, RenderOpIRVec::new());
                return Ok(pipeline);
            };

            let window = Window::FilePreview(file_key);
            global_data
                .state
                .set_window_page_size(&window, visible_rows);

            let total_lines = {
                let snapshot = global_data.state.files.load();
                let file = &snapshot[file_key.0];
                let data = file.data.lock().unwrap();
                data.line_starts.len()
            };
            global_data
                .state
                .set_window_scroll_max(&window, total_lines);
            global_data.state.clamp_scroll(&window);

            let state = &global_data.state;
            let mut render_ops = RenderOpIRVec::new();

            let snapshot = state.files.load();
            let file = &snapshot[file_key.0];

            let data = file.data.lock().unwrap();
            let scroll = state.window_scroll(&window);
            let colored_guard = file.colored_lines.lock().unwrap();

            let pane_bg = state.theme.ui_bg("ui.background").unwrap_or([15, 15, 25]);
            let pane_width = bounds.col_width.as_usize();
            let bg = tui_color!(pane_bg[0], pane_bg[1], pane_bg[2]);
            let bg_style = new_style!(color_bg: {bg});
            let hl_rgb = state.theme.ui_bg("ui.selection").unwrap_or([50, 50, 90]);
            let hl_bg = tui_color!(hl_rgb[0], hl_rgb[1], hl_rgb[2]);
            let hl_bg_style = new_style!(color_bg: {hl_bg});
            let line_num_width = (total_lines.max(1)).to_string().len();
            let content_start_col = line_num_width + GUTTER_GAP.len();
            let content_width = pane_width.saturating_sub(content_start_col).max(1);
            let line_num_fg = state.theme.ui_fg("ui.linenr").unwrap_or({
                let default_fg = state.theme.ui_fg("ui.text").unwrap_or([212, 212, 212]);
                [default_fg[0] / 3, default_fg[1] / 3, default_fg[2] / 3]
            });
            let line_num_bg = state.theme.ui_bg("ui.linenr").unwrap_or(pane_bg);
            let line_num_fg_rgb = tui_color!(line_num_fg[0], line_num_fg[1], line_num_fg[2]);
            let line_num_bg_rgb = tui_color!(line_num_bg[0], line_num_bg[1], line_num_bg[2]);
            let line_num_style = new_style!(color_fg: {line_num_fg_rgb} color_bg: {bg});
            let hl_fg_rgb = state.theme.ui_fg("ui.selection").unwrap_or([255, 255, 255]);
            let hl_line_num_fg = tui_color!(hl_fg_rgb[0], hl_fg_rgb[1], hl_fg_rgb[2]);
            let hl_line_num_style =
                new_style!(color_fg: {hl_line_num_fg} color_bg: {line_num_bg_rgb});

            let text_drag_lo;
            let text_drag_hi;
            let text_drag_single;
            let text_drag_single_lo;
            let text_drag_single_hi;
            if self.text_drag_active {
                let (s, e) = (self.text_drag_start.unwrap(), self.text_drag_end.unwrap());
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

            let mut rendered = 0usize;
            'rendered: for line_idx in scroll..total_lines {
                let line = data.line(line_idx);
                let char_len = line.chars().count();
                let mut seg_start_char = 0_usize;
                let is_text_drag_multi = self.text_drag_active
                    && !text_drag_single
                    && line_idx >= text_drag_lo
                    && line_idx <= text_drag_hi;
                let is_text_drag_single =
                    self.text_drag_active && text_drag_single && line_idx == text_drag_lo;
                let is_hl = !self.text_drag_active
                    && Self::is_line_highlighted(state, file_key, line_idx + 1);
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
                loop {
                    let seg_end_char = (seg_start_char + content_width).min(char_len);
                    let is_first_sub = seg_start_char == 0;

                    render_ops +=
                        RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(rendered));
                    render_ops += RenderOpCommon::ApplyColors(Some(row_bg_style));
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        " ".repeat(pane_width).as_str().into(),
                        Some(row_bg_style),
                    );

                    if is_first_sub {
                        let line_num = line_idx + 1;
                        render_ops +=
                            RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(rendered));
                        render_ops += RenderOpCommon::ApplyColors(Some(row_ln_style));
                        let line_num_str =
                            format!("{:>width$}{GUTTER_GAP}", line_num, width = line_num_width);
                        render_ops += RenderOpIR::PaintTextWithAttributes(
                            line_num_str.as_str().into(),
                            Some(row_ln_style),
                        );
                    } else {
                        render_ops +=
                            RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(rendered));
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
                        col(content_start_col) + row(rendered),
                    );
                    if is_text_drag_single {
                        let line_byte_start = data.line_starts[line_idx];
                        let sel_lo = byte_to_char(line, text_drag_single_lo - line_byte_start);
                        let sel_hi = byte_to_char(line, text_drag_single_hi - line_byte_start);
                        if seg_end_char <= sel_lo || seg_start_char >= sel_hi {
                            paint_line_segment(
                                &mut render_ops,
                                &data,
                                &colored_guard,
                                line_idx,
                                (seg_start_char, seg_end_char),
                                &state.theme,
                                content_bg,
                            );
                        } else {
                            if seg_start_char < sel_lo {
                                paint_line_segment(
                                    &mut render_ops,
                                    &data,
                                    &colored_guard,
                                    line_idx,
                                    (seg_start_char, sel_lo),
                                    &state.theme,
                                    content_bg,
                                );
                            }
                            let o_lo = seg_start_char.max(sel_lo);
                            let o_hi = seg_end_char.min(sel_hi);
                            paint_line_segment(
                                &mut render_ops,
                                &data,
                                &colored_guard,
                                line_idx,
                                (o_lo, o_hi),
                                &state.theme,
                                hl_rgb,
                            );
                            if seg_end_char > sel_hi {
                                paint_line_segment(
                                    &mut render_ops,
                                    &data,
                                    &colored_guard,
                                    line_idx,
                                    (sel_hi, seg_end_char),
                                    &state.theme,
                                    content_bg,
                                );
                            }
                        }
                    } else {
                        paint_line_segment(
                            &mut render_ops,
                            &data,
                            &colored_guard,
                            line_idx,
                            (seg_start_char, seg_end_char),
                            &state.theme,
                            content_bg,
                        );
                    }

                    rendered += 1;
                    seg_start_char += content_width;
                    if seg_start_char >= char_len {
                        break;
                    }
                    if rendered >= visible_rows {
                        break 'rendered;
                    }
                }
            }

            let mut pipeline = render_pipeline!();
            pipeline.push(ZOrder::Normal, render_ops);
            pipeline
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

fn title_bar_colors(focused: bool, theme: &HelixTheme) -> ([u8; 3], [u8; 3]) {
    if focused {
        (
            theme.ui_bg("ui.selection").unwrap_or([50, 50, 90]),
            theme.ui_fg("ui.text").unwrap_or([220, 220, 255]),
        )
    } else {
        (
            theme.ui_bg("ui.statusline").unwrap_or([30, 30, 50]),
            theme.ui_fg("ui.statusline").unwrap_or([180, 180, 220]),
        )
    }
}

fn paint_line_segment(
    render_ops: &mut RenderOpIRVec,
    data: &FileData,
    colored_guard: &[crate::lsp::ColoredLine],
    line_idx: usize,
    (seg_start_char, seg_end_char): (usize, usize),
    theme: &HelixTheme,
    pane_bg: [u8; 3],
) {
    if seg_start_char >= seg_end_char {
        return;
    }
    let default_fg = theme.ui_fg("ui.text").unwrap_or([212, 212, 212]);
    let bg = tui_color!(pane_bg[0], pane_bg[1], pane_bg[2]);
    let line_content = data.line(line_idx);
    let seg_byte_start = data.char_to_byte(line_idx, seg_start_char);
    let seg_byte_end = data.char_to_byte(line_idx, seg_end_char);

    if let Some(spans) = colored_guard.get(line_idx) {
        for &(span_start, span_end, token_type) in spans {
            let overlap_start = span_start.max(seg_byte_start);
            let overlap_end = span_end.min(seg_byte_end);
            if overlap_start >= overlap_end {
                continue;
            }
            let text = &line_content[overlap_start..overlap_end];
            let fg_rgb = theme.color_for_lsp_token(token_type).unwrap_or(default_fg);
            let fg = tui_color!(fg_rgb[0], fg_rgb[1], fg_rgb[2]);
            let style = new_style!(color_fg: {fg} color_bg: {bg});
            *render_ops += RenderOpCommon::ApplyColors(Some(style));
            *render_ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
        }
        return;
    }

    let text = &line_content[seg_byte_start..seg_byte_end];
    let fg = tui_color!(default_fg[0], default_fg[1], default_fg[2]);
    let style = new_style!(color_fg: {fg} color_bg: {bg});
    *render_ops += RenderOpCommon::ApplyColors(Some(style));
    *render_ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
}

/// Converts a byte offset within `s` to a character count.
fn byte_to_char(s: &str, byte_offset: usize) -> usize {
    s[..byte_offset.min(s.len())].chars().count()
}

fn compute_scroll_target(ranges: &[(usize, usize)], page_size: usize, _max: usize) -> usize {
    if ranges.is_empty() || page_size == 0 {
        return 0;
    }

    let min_start = ranges.iter().map(|&(s, _)| s).min().unwrap();
    let max_end = ranges.iter().map(|&(_, e)| e).max().unwrap();
    let total_span = max_end - min_start + 1;

    if ranges.len() == 1 {
        let (start, end) = ranges[0];
        compute_single_block_scroll(start, end - start + 1, page_size)
    } else if total_span <= page_size {
        compute_single_block_scroll(min_start, total_span, page_size)
    } else {
        let (start, end) = ranges[0];
        compute_single_block_scroll(start, end - start + 1, page_size)
    }
}

fn compute_single_block_scroll(
    start_1_indexed: usize,
    block_height: usize,
    page_size: usize,
) -> usize {
    if block_height <= page_size {
        start_1_indexed
            .saturating_sub(1)
            .saturating_sub((page_size - block_height) / 2)
    } else {
        start_1_indexed
            .saturating_sub(1)
            .saturating_sub(page_size * 20 / 100)
    }
}

// ── Shell command helpers ─────────────────────────────────────────────────────

/// Wraps a string in single quotes for safe embedding in a `/bin/sh -c` command string.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Returns true if `s` does not start with an alphanumeric character or `_`,
/// i.e. the previous token ended at a word boundary.
fn is_word_end(s: &str) -> bool {
    !s.starts_with(|c: char| c.is_alphanumeric() || c == '_')
}

/// Finds the nearest ancestor directory of `file_path` that contains a `Cargo.toml`.
fn find_crate_root(file_path: &Utf8Path) -> Option<Utf8PathBuf> {
    let mut dir = file_path.parent()?;
    loop {
        if dir.join("Cargo.toml").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Finds the nearest ancestor directory of `file_path` that contains a `.git` directory.
fn find_repo_root(file_path: &Utf8Path) -> Option<Utf8PathBuf> {
    let mut dir = file_path.parent()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Expands `%`, `%c`/`%crate`, and `%r`/`%repo` placeholders in `cmd`.
///
/// - `%`            → shell-quoted relative path of `file_path` from `root`
/// - `%d` / `%dir`   → shell-quoted directory of `file_path` (relative to `root`)
/// - `%c` / `%crate` → shell-quoted path to the nearest `Cargo.toml` ancestor directory
/// - `%r` / `%repo`  → shell-quoted path to the nearest `.git` ancestor directory
///
/// Returns an error string if a placeholder is used but its root cannot be found.
fn expand_placeholders(cmd: &str, file_path: &Utf8Path, root: &Utf8Path) -> Result<String, String> {
    let mut result = String::with_capacity(cmd.len());
    let mut s = cmd;
    while !s.is_empty() {
        let Some(pct) = s.find('%') else {
            result.push_str(s);
            break;
        };
        result.push_str(&s[..pct]);
        s = &s[pct + 1..];

        if s.starts_with("crate") && is_word_end(&s[5..]) {
            let root = find_crate_root(file_path)
                .ok_or_else(|| "no Cargo.toml found (needed by %crate)".to_string())?;
            result.push_str(&sh_quote(root.as_str()));
            s = &s[5..];
        } else if s.starts_with("repo") && is_word_end(&s[4..]) {
            let root = find_repo_root(file_path)
                .ok_or_else(|| "no .git found (needed by %repo)".to_string())?;
            result.push_str(&sh_quote(root.as_str()));
            s = &s[4..];
        } else if s.starts_with('c') && is_word_end(&s[1..]) {
            let root = find_crate_root(file_path)
                .ok_or_else(|| "no Cargo.toml found (needed by %c)".to_string())?;
            result.push_str(&sh_quote(root.as_str()));
            s = &s[1..];
        } else if s.starts_with("dir") && is_word_end(&s[3..]) {
            let parent = file_path.parent().unwrap_or(file_path);
            let rel = parent.strip_prefix(root).unwrap_or(parent);
            result.push_str(&sh_quote(rel.as_str()));
            s = &s[3..];
        } else if s.starts_with('r') && is_word_end(&s[1..]) {
            let root = find_repo_root(file_path)
                .ok_or_else(|| "no .git found (needed by %r)".to_string())?;
            result.push_str(&sh_quote(root.as_str()));
            s = &s[1..];
        } else if s.starts_with('d') && is_word_end(&s[1..]) {
            let parent = file_path.parent().unwrap_or(file_path);
            let rel = parent.strip_prefix(root).unwrap_or(parent);
            result.push_str(&sh_quote(rel.as_str()));
            s = &s[1..];
        } else {
            let rel = file_path.strip_prefix(root).unwrap_or(file_path);
            result.push_str(&sh_quote(rel.as_str()));
        }
    }
    Ok(result)
}
