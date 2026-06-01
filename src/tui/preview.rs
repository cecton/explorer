use super::app::Id;
use super::input_line::InputLine;
use super::state::{AppSignal, State, Window};
use super::theme::HelixTheme;
use crate::loader::FileKey;
use camino::{Utf8Path, Utf8PathBuf};
use r3bl_tui::{
    CommonResult, Component, EventPropagation, FlexBox, FlexBoxId, GlobalData, HasFocus,
    InputEvent, Key, KeyPress, ModifierKeysMask, MouseInputKind, Pos, RenderOpCommon, RenderOpIR,
    RenderOpIRVec, RenderPipeline, SpecialKey, SurfaceBounds, TerminalWindowMainThreadSignal,
    ZOrder, col, new_style, render_pipeline, row, send_signal, throws_with_return, tui_color,
    width,
};
use std::time::{Duration, Instant};

const GUTTER_GAP: &str = "   ";

pub struct FilePreviewComponent {
    id: FlexBoxId,
    command_mode: Option<String>,
    command_input: InputLine,
    error: Option<(String, Instant)>,
}

impl FilePreviewComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self {
            id,
            command_mode: None,
            command_input: InputLine::new(),
            error: None,
        }
    }

    pub fn title_text(&mut self, state: &State) -> String {
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
        if file.removed.load(std::sync::atomic::Ordering::Relaxed) {
            format!("[deleted] {rel}")
        } else {
            rel.as_str().to_string()
        }
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
            bg_rgb,
            fg_rgb,
        );
        true
    }

    fn execute_command(&mut self, global_data: &mut GlobalData<State, AppSignal>, window: &Window) {
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

    fn is_line_highlighted(state: &State, file_key: FileKey, line_1_indexed: usize) -> bool {
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
    pub(super) fn file_key(&self, state: &State) -> Option<FileKey> {
        let slot = pane_slot(self.id)?;
        let Window::FilePreview(key) = state.window_stack.get(slot)? else {
            return None;
        };
        Some(*key)
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

impl Component<State, AppSignal> for FilePreviewComponent {
    fn reset(&mut self) {
        self.command_mode = None;
    }

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
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
        global_data: &mut GlobalData<State, AppSignal>,
        current_box: FlexBox,
        _surface_bounds: SurfaceBounds,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<RenderPipeline> {
        throws_with_return!({
            let origin = current_box.style_adjusted_origin_pos;
            let bounds = current_box.style_adjusted_bounds_size;
            let visible_rows = bounds.row_height.as_usize();

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

            let mut rendered = 0usize;
            'rendered: for line_idx in scroll..total_lines {
                let line = file_line(&data.content, &data.line_starts, line_idx);
                let char_len = line.chars().count();
                let mut seg_start_char = 0_usize;
                let is_hl = Self::is_line_highlighted(state, file_key, line_idx + 1);
                let row_bg_style = if is_hl { hl_bg_style } else { bg_style };
                let row_ln_style = if is_hl {
                    hl_line_num_style
                } else {
                    line_num_style
                };
                let content_bg = if is_hl { hl_rgb } else { pane_bg };
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
                    paint_line_segment(
                        &mut render_ops,
                        (&data.content, &data.line_starts),
                        &colored_guard,
                        line_idx,
                        (seg_start_char, seg_end_char),
                        &state.theme,
                        content_bg,
                    );

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
    (content, line_starts): (&str, &[usize]),
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
    let line_content = file_line(content, line_starts, line_idx);
    let (seg_byte_start, seg_byte_end) =
        char_offsets_to_bytes(line_content, seg_start_char, seg_end_char);

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

fn char_offsets_to_bytes(s: &str, start_char: usize, end_char: usize) -> (usize, usize) {
    let mut char_count = 0usize;
    let mut byte_start = s.len();
    let mut byte_end = s.len();
    for (byte_idx, _ch) in s.char_indices() {
        if char_count == start_char {
            byte_start = byte_idx;
        }
        if char_count == end_char {
            byte_end = byte_idx;
            break;
        }
        char_count += 1;
    }
    if start_char >= char_count {
        byte_start = s.len();
    }
    (byte_start, byte_end)
}

fn file_line<'a>(content: &'a str, line_starts: &[usize], idx: usize) -> &'a str {
    let start = line_starts[idx];
    let end = line_starts
        .get(idx + 1)
        .map(|&e| e - 1)
        .unwrap_or(content.len());
    &content[start..end]
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
