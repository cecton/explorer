#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

pub(crate) mod spawn;

pub struct TerminalPaneComponent {
    id: FlexBoxId,
    slot: usize,
    content_origin_row: usize,
    content_origin_col: usize,
    content_col_count: usize,
    content_row_count: usize,
}

impl TerminalPaneComponent {
    pub fn new(id: FlexBoxId, slot: usize) -> Self {
        Self {
            id,
            slot,
            content_origin_row: 0,
            content_origin_col: 0,
            content_col_count: 0,
            content_row_count: 0,
        }
    }

    fn terminal_id(&self, state: &crate::tui::state::AppState) -> Option<usize> {
        use crate::tui::pane_manager::Window;
        let slot = crate::tui::pane_component::pane_slot(self.id)?;
        let Window::Terminal(id) = state.pane_manager.slot_window(slot)? else {
            return None;
        };
        Some(id)
    }

    fn pane_height(
        &self,
        global_data: &GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
    ) -> usize {
        global_data
            .state
            .pane_manager
            .slot_box(self.slot)
            .map(|b| b.style_adjusted_bounds.bounds_size.row_height.as_usize())
            .unwrap_or(0)
    }
}

impl crate::tui::widgets::title_row::TitleRow for TerminalPaneComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        state: &mut crate::tui::state::AppState,
    ) -> usize {
        let theme = &state.theme;
        let id = self.terminal_id(state).unwrap_or(0);
        let (base, exited, exit_code, exit_signal) = state
            .with_terminal(id, |g| {
                (
                    g.title.clone().unwrap_or_else(|| format!("Terminal {id}")),
                    g.exited,
                    g.exit_code,
                    g.exit_signal.clone(),
                )
            })
            .unwrap_or_else(|| (format!("Terminal {id}"), false, None, None));
        let title = if let Some(ref sig) = exit_signal {
            format!("{} [{}]", base, sig)
        } else if let Some(code) = exit_code {
            format!("{} [exit {}]", base, code)
        } else if exited {
            format!("{} [done]", base)
        } else {
            base
        };
        crate::tui::widgets::title_row::render_pane_title(
            ops, pane_box, &title, false, theme, focused,
        );
        1
    }
}

impl Component<crate::tui::state::AppState, crate::signal::AppSignal> for TerminalPaneComponent {
    fn reset(&mut self) {}

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        input_event: InputEvent,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        use crate::tui::pane_manager::Window;
        throws_with_return!({
            let Some(id) = self.terminal_id(&global_data.state) else {
                return Ok(EventPropagation::Propagate);
            };

            if matches!(
                &input_event,
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Esc) | Key::SpecialKey(SpecialKey::Enter)
                })
            ) && global_data
                .state
                .with_terminal(id, |p| p.exited)
                .unwrap_or(false)
            {
                crate::tui::focus::close_terminal(&mut global_data.state, id);
                global_data.state.mark_session_dirty();
                return Ok(EventPropagation::ConsumedRender);
            }

            let Some(pane_arc) = global_data.state.terminal_panes.get(&id).cloned() else {
                return Ok(EventPropagation::Propagate);
            };

            let mut pane = pane_arc.lock();
            let tx = std::sync::Arc::clone(&pane.pty_input_tx);
            let alternate_screen_active = pane.ofs_buf.get_terminal_mode().active_screen_buffer
                == ActiveScreenBuffer::Alternate;
            let scrollback_len = pane.ofs_buf.get_history_len();

            if global_data.state.terminal_grabbed {
                match &input_event {
                    InputEvent::Keyboard(keypress) => {
                        if let Some(pty_event) = pane.ofs_buf.keypress_to_pty_input_event(*keypress)
                        {
                            let _ = tx.try_send(pty_event);
                            return Ok(EventPropagation::ConsumedRender);
                        }
                    }
                    InputEvent::Mouse(mouse)
                        if pane.ofs_buf.get_terminal_mode().mouse_tracking_mode
                            == MouseTrackingMode::Enabled =>
                    {
                        let col = mouse
                            .pos
                            .col_index
                            .as_usize()
                            .saturating_sub(self.content_origin_col);
                        let row = mouse
                            .pos
                            .row_index
                            .as_usize()
                            .saturating_sub(self.content_origin_row);
                        if let Some(bytes) = r3bl_tui::ansi::mouse_sgr::generate(
                            mouse,
                            TermCol::from_zero_based(vp_col(col as u16)),
                            TermRow::from_zero_based(vp_row(row as u16)),
                        ) {
                            let _ = tx.try_send(PtyInputEvent::Write(bytes));
                            return Ok(EventPropagation::ConsumedRender);
                        }
                    }
                    InputEvent::BracketedPaste(text) => {
                        let bytes = if pane
                            .ofs_buf
                            .get_terminal_mode()
                            .is_bracketed_paste_enabled()
                        {
                            let mut b = Vec::with_capacity(text.len() + 6);
                            b.extend_from_slice(b"\x1b[200~");
                            b.extend_from_slice(text.as_bytes());
                            b.extend_from_slice(b"\x1b[201~");
                            b
                        } else {
                            text.as_bytes().to_vec()
                        };
                        let _ = tx.try_send(PtyInputEvent::Write(bytes));
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    _ => {}
                }
            }

            match input_event {
                InputEvent::Mouse(MouseInput {
                    kind: MouseInputKind::ScrollUp,
                    maybe_modifier_keys: None,
                    ..
                }) if !alternate_screen_active && scrollback_len > 0 => {
                    global_data.state.terminal_grabbed = false;
                    let old_offset = pane.scroll_offset;
                    pane.scroll_offset = pane.scroll_offset.saturating_add(3).min(scrollback_len);
                    if pane.scroll_offset != old_offset {
                        EventPropagation::ConsumedRender
                    } else {
                        EventPropagation::Consumed
                    }
                }

                InputEvent::Mouse(MouseInput {
                    kind: MouseInputKind::ScrollDown,
                    maybe_modifier_keys: None,
                    ..
                }) if !alternate_screen_active && scrollback_len > 0 => {
                    let old_offset = pane.scroll_offset;
                    if old_offset > 0 {
                        pane.scroll_offset = old_offset.saturating_sub(3);
                        if pane.scroll_offset == 0 {
                            global_data.state.terminal_grabbed = true;
                        }
                    }
                    if pane.scroll_offset != old_offset {
                        EventPropagation::ConsumedRender
                    } else {
                        EventPropagation::Consumed
                    }
                }

                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Enter),
                })
                | InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::Character('q'),
                }) => {
                    global_data.state.terminal_grabbed = true;
                    pane.scroll_offset = 0;
                    EventPropagation::ConsumedRender
                }

                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Backspace),
                }) => {
                    drop(pane);
                    let window = Window::Terminal(id);
                    global_data.state.pane_manager.send_to_back(&window);
                    crate::tui::focus::refocus(&mut global_data.state, Some(window));
                    global_data.state.mark_session_dirty();
                    EventPropagation::ConsumedRender
                }

                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::PageUp),
                }) => {
                    let pane_height = self.pane_height(global_data);
                    pane.scroll_offset = pane
                        .scroll_offset
                        .saturating_add(pane_height)
                        .min(pane.ofs_buf.get_history_len());
                    EventPropagation::ConsumedRender
                }

                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::PageDown),
                }) => {
                    let pane_height = self.pane_height(global_data);
                    pane.scroll_offset = pane.scroll_offset.saturating_sub(pane_height);
                    EventPropagation::ConsumedRender
                }

                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Up),
                }) => {
                    pane.scroll_offset = pane
                        .scroll_offset
                        .saturating_add(1)
                        .min(pane.ofs_buf.get_history_len());
                    EventPropagation::ConsumedRender
                }

                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Down),
                }) => {
                    pane.scroll_offset = pane.scroll_offset.saturating_sub(1);
                    EventPropagation::ConsumedRender
                }

                _ => EventPropagation::Propagate,
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
            let Some(id) = self.terminal_id(&global_data.state) else {
                return Ok(());
            };
            let Some(pane) = global_data.state.terminal_panes.get(&id) else {
                return Ok(());
            };
            let mut pane = pane.lock();
            let origin = current_box.style_adjusted_bounds.origin_pos;

            let pane_width = current_box
                .style_adjusted_bounds
                .bounds_size
                .col_width
                .as_usize();
            let pane_height = current_box
                .style_adjusted_bounds
                .bounds_size
                .row_height
                .as_usize();
            self.content_origin_row = origin.row_index.as_usize();
            self.content_origin_col = origin.col_index.as_usize();
            self.content_col_count = pane_width;
            self.content_row_count = pane_height;

            // Resize the offscreen buffer if the pane dimensions changed.
            let new_size = VPSize {
                col_width: vp_width(pane_width as u16),
                row_height: vp_height(pane_height as u16),
            };
            if pane.last_size != new_size {
                pane.ofs_buf.resize(new_size);
                let _ = pane.pty_input_tx.try_send(PtyInputEvent::Resize(new_size));
                pane.last_size = new_size;
            }

            let scrollback_len = pane.ofs_buf.get_history_len();
            let scroll_offset = pane.scroll_offset.min(scrollback_len);

            let hl_rgb = global_data
                .state
                .theme
                .ui_bg("ui.selection")
                .unwrap_or(crate::tui::widgets::title_row::DEFAULT_SELECTION_BG);
            let selection_rect = global_data.state.text_selection.as_ref().and_then(|sel| {
                if sel.window != Window::Terminal(id) || !sel.active {
                    return None;
                }
                match (sel.start, sel.end) {
                    (
                        SelPoint::Terminal {
                            viewport_row: sr,
                            col: sc,
                        },
                        SelPoint::Terminal {
                            viewport_row: er,
                            col: ec,
                        },
                    ) => Some(SelectionRect {
                        start_row: sr,
                        start_col: sc,
                        end_row: er,
                        end_col: ec,
                    }),
                    _ => None,
                }
            });

            let cursor_pos = if scroll_offset == 0 {
                let is_active = matches!(
                    global_data.state.pane_manager.focused_window,
                    Some(Window::Terminal(focused_id)) if focused_id == id
                );
                let cursor_visible = pane.ofs_buf.get_parser_global_state().cursor_visibility
                    == CursorVisibilityMode::Visible
                    && global_data.state.terminal_grabbed
                    && is_active;
                cursor_visible.then(|| pane.ofs_buf.get_active_screen_buffer().get_cursor_pos())
            } else {
                None
            };
            let lines: Vec<&[PixelChar]> = (0..pane_height)
                .map_while(|r| terminal_row(&pane.ofs_buf, scroll_offset, r))
                .collect();
            let mut render_ops = render_lines_to_ir(&lines, origin, cursor_pos);
            for row_idx in lines.len()..pane_height {
                render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                    origin,
                    vp_col(0) + vp_row(row_idx as u16),
                );
                render_ops +=
                    RenderOpIR::PaintTextWithAttributes(" ".repeat(pane_width).into(), None);
            }
            if let Some(rect) = selection_rect {
                overlay_selection(&mut render_ops, &lines, origin, rect, hl_rgb);
            }

            global_data.pipeline.push(ZOrder::Normal, render_ops);
        });
    }
}

#[derive(Clone, Copy, Debug)]
struct SelectionRect {
    start_row: usize,
    start_col: usize,
    end_row: usize,
    end_col: usize,
}

fn cursor_at(row_idx: usize, col_idx: usize, cursor: Option<r3bl_tui::VPPos>) -> bool {
    cursor == Some(vp_col(col_idx as u16) + vp_row(row_idx as u16))
}

fn terminal_row(
    ofs_buf: &OfsBufVT100,
    scroll_offset: usize,
    viewport_row: usize,
) -> Option<&[PixelChar]> {
    ofs_buf.get_active_screen_buffer().get_row_with_scrollback(
        vp_row(viewport_row as u16),
        scrollback_amount(scroll_offset),
    )
}

fn render_lines_to_ir(
    lines: &[&[PixelChar]],
    origin: r3bl_tui::VPPos,
    cursor_pos: Option<r3bl_tui::VPPos>,
) -> RenderOpIRVec {
    let mut ops = RenderOpIRVec::new();
    for (row_index, line) in lines.iter().enumerate() {
        let mut col_index = 0usize;
        while col_index < line.len() {
            if line[col_index] == PixelChar::Void {
                col_index += 1;
                continue;
            }

            // Emit cursor at this cell if it matches.
            if cursor_at(row_index, col_index, cursor_pos) {
                let mut s = TuiStyle::default();
                s.attribs.reverse = Some(r3bl_tui::tui_style_attrib::Reverse);
                let ch = match &line[col_index] {
                    PixelChar::PlainText { display_char, .. } => *display_char,
                    _ => ' ',
                };
                ops += RenderOpCommon::MoveCursorPositionRelTo(
                    origin,
                    vp_col(col_index as u16) + vp_row(row_index as u16),
                );
                ops += RenderOpCommon::ApplyColors(Some(s));
                ops += RenderOpIR::PaintTextWithAttributes(ch.to_string().into(), Some(s));
                col_index += 1;
                continue;
            }

            let run_start = col_index;
            let run_style = match &line[col_index] {
                PixelChar::Spacer => None,
                PixelChar::PlainText { style, .. } => Some(*style),
                PixelChar::Void => unreachable!(),
            };

            let mut text = String::new();
            while col_index < line.len() {
                if cursor_at(row_index, col_index, cursor_pos) {
                    break;
                }
                match &line[col_index] {
                    PixelChar::PlainText {
                        display_char,
                        style,
                        ..
                    } if Some(*style) == run_style => {
                        text.push(*display_char);
                        col_index += 1;
                    }
                    PixelChar::Spacer if run_style.is_none() => {
                        text.push(' ');
                        col_index += 1;
                    }
                    PixelChar::Void => {
                        col_index += 1;
                        break;
                    }
                    _ => break,
                }
            }

            if !text.is_empty() {
                ops += RenderOpCommon::MoveCursorPositionRelTo(
                    origin,
                    vp_col(run_start as u16) + vp_row(row_index as u16),
                );
                if run_style.is_some() {
                    ops += RenderOpCommon::ApplyColors(run_style);
                } else {
                    ops += RenderOpCommon::ResetColor;
                }
                ops += RenderOpIR::PaintTextWithAttributes(text.into(), run_style);
            }
        }
    }

    ops
}

/// Compute half-open column ranges for each row in a terminal text selection.
/// `sr`/`er` and `sc`/`ec` are raw coordinates (may be in any order).
/// `line_lengths` gives the length of each visible row, starting at `line_offset`.
/// Returns `(row_idx, col_start, col_end)` tuples where `col_start < col_end`.
fn compute_terminal_selection_ranges(
    sr: usize,
    sc: usize,
    er: usize,
    ec: usize,
    line_lengths: &[usize],
    line_offset: usize,
) -> Vec<(usize, usize, usize)> {
    let (sr, er) = (sr.min(er), sr.max(er));
    let (sc, ec) = (sc.min(ec), sc.max(ec));

    let mut ranges = Vec::with_capacity(er - sr + 1);
    for row_idx in sr..=er {
        let line_len = line_lengths
            .get(row_idx - line_offset)
            .copied()
            .unwrap_or(0);
        let row_start = if row_idx == sr { sc.min(line_len) } else { 0 };
        let row_end = if row_idx == er {
            ec.min(line_len)
        } else {
            line_len
        };
        if row_start < row_end {
            ranges.push((row_idx, row_start, row_end));
        }
    }
    ranges
}

fn overlay_selection(
    ops: &mut RenderOpIRVec,
    lines: &[&[PixelChar]],
    origin: r3bl_tui::VPPos,
    selection: SelectionRect,
    hl_bg: RgbValue,
) {
    let line_lengths: Vec<usize> = lines
        .iter()
        .map(|l| {
            l.iter()
                .rposition(|pc| !matches!(pc, PixelChar::Spacer | PixelChar::Void))
                .map(|i| i + 1)
                .unwrap_or(0)
        })
        .collect();
    let ranges = compute_terminal_selection_ranges(
        selection.start_row,
        selection.start_col,
        selection.end_row,
        selection.end_col,
        &line_lengths,
        0,
    );

    let sel_bg = TuiColor::from(hl_bg);
    let default_fg = tui_color!(255, 255, 255);

    for (row_idx, col_start, col_end) in ranges {
        let line = lines[row_idx];
        #[allow(clippy::needless_range_loop)]
        // col_idx is the absolute column, needed for cursor pos
        for col_idx in col_start..col_end {
            let pc = &line[col_idx];
            let ch = match pc {
                PixelChar::PlainText { display_char, .. } => *display_char,
                PixelChar::Spacer => ' ',
                PixelChar::Void => continue,
            };
            let style = match pc {
                PixelChar::PlainText { style, .. } => Some(*style),
                _ => None,
            };
            let fg = style.and_then(|s| s.color_fg).unwrap_or(default_fg);
            let sel_style = new_style!(color_fg: {fg} color_bg: {sel_bg});
            *ops += RenderOpCommon::MoveCursorPositionRelTo(
                origin,
                vp_col(col_idx as u16) + vp_row(row_idx as u16),
            );
            *ops += RenderOpCommon::ApplyColors(Some(sel_style));
            *ops += RenderOpIR::PaintTextWithAttributes(ch.to_string().into(), Some(sel_style));
        }
    }
}

/// The `(start, end)` char range of the word under terminal column `cursor_col`: a
/// terminal click names a column, `word_bounds` a byte.
pub fn terminal_word_bounds(line: &str, cursor_col: usize) -> (usize, usize) {
    let cursor_byte = line
        .char_indices()
        .nth(cursor_col)
        .or_else(|| line.char_indices().next_back())
        .map_or(0, |(byte, _)| byte);
    let (start, end) = crate::util::text::word_bounds(line, cursor_byte);
    (line[..start].chars().count(), line[..end].chars().count())
}

/// Convert a PixelCharLine to a string, trimming trailing Spacer/Void cells.
/// Returns `(trimmed_string, trimmed_len_in_chars)`.
fn trimmed_pixel_char_line(line: &[PixelChar]) -> (String, usize) {
    let trimmed_len = line
        .iter()
        .rposition(|pc| !matches!(pc, PixelChar::Spacer | PixelChar::Void))
        .map(|i| i + 1)
        .unwrap_or(0);
    let s = line[..trimmed_len]
        .iter()
        .map(|pc| match pc {
            PixelChar::PlainText { display_char, .. } => *display_char,
            PixelChar::Spacer => ' ',
            PixelChar::Void => ' ',
        })
        .collect();
    (s, trimmed_len)
}

pub fn terminal_line_at_viewport_row(
    pane: &crate::tui::state::TerminalPane,
    viewport_row: usize,
) -> (String, usize) {
    let scroll_offset = pane.scroll_offset.min(pane.ofs_buf.get_history_len());
    terminal_row(&pane.ofs_buf, scroll_offset, viewport_row)
        .map_or((String::new(), 0), trimmed_pixel_char_line)
}

pub fn extract_terminal_text(
    pane: &crate::tui::state::TerminalPane,
    start: crate::tui::pane_manager::SelPoint,
    end: crate::tui::pane_manager::SelPoint,
) -> Option<String> {
    use crate::tui::pane_manager::SelPoint;
    let (
        SelPoint::Terminal {
            viewport_row: sr,
            col: sc,
        },
        SelPoint::Terminal {
            viewport_row: er,
            col: ec,
        },
    ) = (start, end)
    else {
        return None;
    };
    let (sr, er) = (sr.min(er), sr.max(er));
    let (raw_sc, raw_ec) = (sc, ec);
    let mut sc = raw_sc.min(raw_ec);
    let mut ec = raw_sc.max(raw_ec);

    let scroll_offset = pane.scroll_offset.min(pane.ofs_buf.get_history_len());

    let mut lines: Vec<String> = Vec::with_capacity(er - sr + 1);
    let mut line_lengths: Vec<usize> = Vec::with_capacity(er - sr + 1);

    for viewport_row in sr..=er {
        if let Some(line) = terminal_row(&pane.ofs_buf, scroll_offset, viewport_row) {
            let (s, trimmed_len) = trimmed_pixel_char_line(line);
            line_lengths.push(trimmed_len);
            lines.push(s);
        }
    }

    if lines.is_empty() {
        return None;
    }

    if sr != er {
        sc = 0;
        ec = line_lengths.last().copied().unwrap_or(0);
    }

    let ranges = compute_terminal_selection_ranges(sr, sc, er, ec, &line_lengths, sr);

    let mut result = String::new();
    for (i, (row_idx, col_start, col_end)) in ranges.iter().enumerate() {
        if i > 0 {
            result.push('\n');
        }
        let line = &lines[row_idx - sr];
        let byte_start = line
            .char_indices()
            .nth(*col_start)
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        let byte_end = line
            .char_indices()
            .nth(*col_end)
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        result.push_str(&line[byte_start..byte_end]);
    }

    Some(result)
}

#[cfg(test)]
mod tests {
    use super::{terminal_word_bounds, trimmed_pixel_char_line};
    use r3bl_tui::{PixelChar, PixelCharLine, TuiStyle};

    #[test]
    fn empty_line() {
        assert_eq!(terminal_word_bounds("", 0), (0, 0));
    }

    #[test]
    fn ascii_word_at_start() {
        let line = "hello world";
        let (start, end) = terminal_word_bounds(line, 0);
        assert_eq!(&line[start..end], "hello");
    }

    #[test]
    fn ascii_word_in_middle() {
        let line = "hello world";
        let (start, end) = terminal_word_bounds(line, 6);
        assert_eq!(&line[start..end], "world");
    }

    #[test]
    fn ascii_word_at_end() {
        let line = "hello world";
        let (start, end) = terminal_word_bounds(line, 10);
        assert_eq!(&line[start..end], "world");
    }

    #[test]
    fn whitespace_run() {
        let line = "hello   world";
        let (start, end) = terminal_word_bounds(line, 5);
        assert_eq!(&line[start..end], "   ");
    }

    #[test]
    fn multibyte_box_drawing_chars() {
        let line = " ╭──╮";
        // column 2 -> first ─
        let (start, end) = terminal_word_bounds(line, 2);
        let byte_start = line
            .char_indices()
            .nth(start)
            .map(|(b, _)| b)
            .unwrap_or(line.len());
        let byte_end = line
            .char_indices()
            .nth(end)
            .map(|(b, _)| b)
            .unwrap_or(line.len());
        let selected = &line[byte_start..byte_end];
        assert_eq!(selected, "─");
    }

    #[test]
    fn multibyte_long_box_drawing_line() {
        let line = " ╭────────────────────────────────────────────────────────────────────────────────────────────────────────────────────╮ ";
        // column 39 -> a ─ in the middle of the run
        let (start, end) = terminal_word_bounds(line, 39);
        let byte_start = line
            .char_indices()
            .nth(start)
            .map(|(b, _)| b)
            .unwrap_or(line.len());
        let byte_end = line
            .char_indices()
            .nth(end)
            .map(|(b, _)| b)
            .unwrap_or(line.len());
        let selected = &line[byte_start..byte_end];
        assert_eq!(selected, "─");
    }

    #[test]
    fn multibyte_click_on_first_multibyte_char() {
        let line = "╭─";
        let (start, end) = terminal_word_bounds(line, 0);
        let byte_start = line
            .char_indices()
            .nth(start)
            .map(|(b, _)| b)
            .unwrap_or(line.len());
        let byte_end = line
            .char_indices()
            .nth(end)
            .map(|(b, _)| b)
            .unwrap_or(line.len());
        let selected = &line[byte_start..byte_end];
        assert_eq!(selected, "╭");
    }

    #[test]
    fn multibyte_click_on_last_char() {
        let line = "╭─";
        let (start, end) = terminal_word_bounds(line, 1);
        let byte_start = line
            .char_indices()
            .nth(start)
            .map(|(b, _)| b)
            .unwrap_or(line.len());
        let byte_end = line
            .char_indices()
            .nth(end)
            .map(|(b, _)| b)
            .unwrap_or(line.len());
        let selected = &line[byte_start..byte_end];
        assert_eq!(selected, "─");
    }

    #[test]
    fn url_detection() {
        let line = "see https://example.com for more";
        let (start, end) = terminal_word_bounds(line, 5);
        assert_eq!(&line[start..end], "https://example.com");
    }

    #[test]
    fn cursor_beyond_line_length() {
        let line = "hi";
        let (start, end) = terminal_word_bounds(line, 100);
        assert_eq!(&line[start..end], "hi");
    }

    #[test]
    fn trimmed_line_multibyte_char_count() {
        let line = PixelCharLine {
            pixel_chars: vec![
                PixelChar::PlainText {
                    display_char: '╭',
                    style: TuiStyle::default(),
                },
                PixelChar::PlainText {
                    display_char: '─',
                    style: TuiStyle::default(),
                },
                PixelChar::PlainText {
                    display_char: '╮',
                    style: TuiStyle::default(),
                },
            ],
        };
        let (s, count) = trimmed_pixel_char_line(&line);
        assert_eq!(count, 3);
        assert_eq!(s, "╭─╮");
        // Each char is 3 bytes; byte len would be 9, not 3.
        assert_eq!(s.len(), 9);
    }

    #[test]
    fn trimmed_line_multibyte_with_trailing_spacers() {
        let line = PixelCharLine {
            pixel_chars: vec![
                PixelChar::PlainText {
                    display_char: '█',
                    style: TuiStyle::default(),
                },
                PixelChar::PlainText {
                    display_char: '▒',
                    style: TuiStyle::default(),
                },
                PixelChar::Spacer,
                PixelChar::Spacer,
            ],
        };
        let (s, count) = trimmed_pixel_char_line(&line);
        assert_eq!(count, 2);
        assert_eq!(s, "█▒");
        // Byte len: 6 (2 × 3-byte UTF-8 chars).
        assert_eq!(s.len(), 6);
    }

    #[test]
    fn extracts_the_selected_text_of_the_live_screen() {
        use super::extract_terminal_text;
        use crate::tui::pane_manager::SelPoint;
        use crate::tui::state::TerminalPane;
        use r3bl_tui::{OfsBufVT100, vp_height, vp_width};
        use std::sync::Arc;

        let mut ofs_buf = OfsBufVT100::new_empty(vp_width(20) + vp_height(5));
        let _unused = ofs_buf.apply_ansi_bytes("hello world\r\nsecond line");
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let pane = TerminalPane {
            ofs_buf,
            title: None,
            pty_input_tx: Arc::new(tx),
            child_killer: None,
            last_size: vp_width(20) + vp_height(5),
            is_command_pane: false,
            exited: false,
            exit_code: None,
            exit_signal: None,
            scroll_offset: 0,
            cwd: camino::Utf8PathBuf::from("/"),
            command: None,
            flush_pending: Default::default(),
        };

        let start = SelPoint::Terminal {
            viewport_row: 0,
            col: 6,
        };
        let end = SelPoint::Terminal {
            viewport_row: 0,
            col: 11,
        };
        assert_eq!(
            extract_terminal_text(&pane, start, end).as_deref(),
            Some("world")
        );

        let end = SelPoint::Terminal {
            viewport_row: 1,
            col: 6,
        };
        assert_eq!(
            extract_terminal_text(&pane, start, end).as_deref(),
            Some("hello world\nsecond line")
        );
    }
}
