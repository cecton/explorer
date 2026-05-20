use super::app::Id;
use super::state::{AppSignal, State, Window};
use super::theme::HelixTheme;
use crate::loader::FileKey;
use r3bl_tui::{
    CommonResult, Component, EventPropagation, FlexBox, FlexBoxId, GlobalData, HasFocus,
    InputEvent, Key, KeyPress, ModifierKeysMask, MouseInputKind, RenderOpCommon, RenderOpIR,
    RenderOpIRVec, RenderPipeline, SpecialKey, SurfaceBounds, ZOrder, col, new_style,
    render_pipeline, row, throws_with_return, tui_color,
};

const GUTTER_GAP: &str = "  ";

pub struct FilePreviewComponent {
    id: FlexBoxId,
}

impl FilePreviewComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self { id }
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
    fn reset(&mut self) {}

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
            let line_num_style =
                new_style!(color_fg: {line_num_fg_rgb} color_bg: {line_num_bg_rgb});

            let mut rendered = 0usize;
            'rendered: for line_idx in scroll..total_lines {
                let line = file_line(&data.content, &data.line_starts, line_idx);
                let char_len = line.chars().count();
                let mut seg_start_char = 0_usize;
                loop {
                    let seg_end_char = (seg_start_char + content_width).min(char_len);
                    let is_first_sub = seg_start_char == 0;

                    render_ops +=
                        RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(rendered));
                    render_ops += RenderOpCommon::ApplyColors(Some(bg_style));
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        " ".repeat(pane_width).as_str().into(),
                        Some(bg_style),
                    );

                    if is_first_sub {
                        let line_num = line_idx + 1;
                        render_ops +=
                            RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(rendered));
                        render_ops += RenderOpCommon::ApplyColors(Some(line_num_style));
                        let line_num_str =
                            format!("{:>width$}{GUTTER_GAP}", line_num, width = line_num_width);
                        render_ops += RenderOpIR::PaintTextWithAttributes(
                            line_num_str.as_str().into(),
                            Some(line_num_style),
                        );
                    } else {
                        render_ops +=
                            RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(rendered));
                        render_ops += RenderOpCommon::ApplyColors(Some(line_num_style));
                        render_ops += RenderOpIR::PaintTextWithAttributes(
                            " ".repeat(line_num_width + GUTTER_GAP.len())
                                .as_str()
                                .into(),
                            Some(bg_style),
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
                        pane_bg,
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
