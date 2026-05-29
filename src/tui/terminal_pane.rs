use super::state::{AppSignal, State, Window};
use r3bl_tui::core::pty::{MouseTrackingMode, PtyInputEvent};
use r3bl_tui::{
    self, Button, CommonResult, Component, EventPropagation, FlexBox, FlexBoxId, GlobalData,
    HasFocus, InputEvent, MouseInputKind, PixelChar, RenderOpCommon, RenderOpIR, RenderOpIRVec,
    RenderPipeline, Size, SurfaceBounds, TuiStyle, ZOrder, col, height, render_pipeline, row,
    throws_with_return, width,
};

pub struct TerminalPaneComponent {
    id: FlexBoxId,
}

impl TerminalPaneComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self { id }
    }

    fn terminal_id(&self, state: &State) -> Option<usize> {
        let slot = pane_slot(self.id)?;
        let Window::Terminal(id) = state.window_stack.get(slot)? else {
            return None;
        };
        Some(*id)
    }
}

fn pane_slot(id: FlexBoxId) -> Option<usize> {
    use super::app::Id;
    match id.inner {
        x if x == Id::Pane0 as u8 => Some(0),
        x if x == Id::Pane1 as u8 => Some(1),
        x if x == Id::Pane2 as u8 => Some(2),
        x if x == Id::Pane3 as u8 => Some(3),
        x if x == Id::Pane4 as u8 => Some(4),
        _ => None,
    }
}

impl Component<State, AppSignal> for TerminalPaneComponent {
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
            let Some(id) = self.terminal_id(&global_data.state) else {
                return Ok(EventPropagation::Propagate);
            };
            let Some(pane) = global_data.state.terminal_panes.get(&id) else {
                return Ok(EventPropagation::Propagate);
            };
            let Ok(pane) = pane.lock() else {
                return Ok(EventPropagation::Propagate);
            };
            let tx = pane.pty_input_tx.clone();
            let mouse_tracking_mode = pane.mouse_tracking_mode;
            drop(pane);

            match input_event {
                InputEvent::Keyboard(keypress) => {
                    if let Some(pty_event) = Option::<PtyInputEvent>::from(keypress) {
                        let _ = tx.try_send(pty_event);
                    }
                }
                InputEvent::Mouse(mouse) => {
                    if !should_forward_mouse(&mouse.kind, mouse_tracking_mode) {
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    let Some(slot) = pane_slot(self.id) else {
                        return Ok(EventPropagation::Propagate);
                    };
                    let box_ = &global_data.state.pane_boxes[slot];
                    let origin_row = box_.style_adjusted_origin_pos.row_index.as_u16() + 1;
                    let origin_col = box_.style_adjusted_origin_pos.col_index.as_u16();
                    let encoded = encode_mouse_event(mouse, origin_row, origin_col);
                    let _ = tx.try_send(PtyInputEvent::Write(encoded));
                }
                InputEvent::Resize(new_size) => {
                    let _ = tx.try_send(PtyInputEvent::Resize(new_size));
                }
                _ => {}
            }

            EventPropagation::ConsumedRender
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
            let Some(id) = self.terminal_id(&global_data.state) else {
                return Ok(render_pipeline!());
            };
            let Some(pane) = global_data.state.terminal_panes.get(&id) else {
                return Ok(render_pipeline!());
            };
            let Ok(mut pane) = pane.lock() else {
                return Ok(render_pipeline!());
            };
            let origin = current_box.style_adjusted_origin_pos;

            let pane_width = current_box.style_adjusted_bounds_size.col_width.as_usize();
            let pane_height = current_box.style_adjusted_bounds_size.row_height.as_usize();

            // Resize the offscreen buffer if the pane dimensions changed.
            let new_size = Size {
                col_width: width(pane_width as u16),
                row_height: height(pane_height as u16),
            };
            if pane.last_size != new_size {
                pane.ofs_buf.resize(new_size);
                let _ = pane.pty_input_tx.try_send(PtyInputEvent::Resize(new_size));
                pane.last_size = new_size;
            }

            let visible_rows = pane_height.min(pane.ofs_buf.buffer.len());
            let cursor_visible = pane.ofs_buf.ansi_parser_support.cursor_visible;
            let cursor_pos = if cursor_visible {
                Some(pane.ofs_buf.cursor_pos)
            } else {
                None
            };
            let mut render_ops =
                render_ofs_buf_to_ir(&pane.ofs_buf, origin, visible_rows, cursor_pos);

            let ofs_height = visible_rows;

            if ofs_height < pane_height {
                for row_idx in ofs_height..pane_height {
                    render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        col(0) + row(row_idx as u16),
                    );
                    render_ops +=
                        RenderOpIR::PaintTextWithAttributes(" ".repeat(pane_width).into(), None);
                }
            }

            let mut pipeline = render_pipeline!();
            pipeline.push(ZOrder::Normal, render_ops);
            pipeline
        });
    }
}

fn should_forward_mouse(kind: &MouseInputKind, mode: MouseTrackingMode) -> bool {
    match mode {
        MouseTrackingMode::None => false,
        MouseTrackingMode::Basic => matches!(
            kind,
            MouseInputKind::MouseDown(_) | MouseInputKind::MouseUp(_)
        ),
        MouseTrackingMode::ButtonDrag => matches!(
            kind,
            MouseInputKind::MouseDown(_)
                | MouseInputKind::MouseUp(_)
                | MouseInputKind::MouseDrag(_)
        ),
        MouseTrackingMode::AnyEvent => true,
    }
}

fn encode_mouse_event(mouse: r3bl_tui::MouseInput, origin_row: u16, origin_col: u16) -> Vec<u8> {
    let x = mouse
        .pos
        .col_index
        .as_u16()
        .saturating_sub(origin_col)
        .saturating_add(1) as usize;
    let y = mouse
        .pos
        .row_index
        .as_u16()
        .saturating_sub(origin_row)
        .saturating_add(1) as usize;

    let (code, suffix) = match mouse.kind {
        MouseInputKind::MouseDown(button) => {
            let b = match button {
                Button::Left => 0,
                Button::Middle => 1,
                Button::Right => 2,
            };
            (b, b'M')
        }
        MouseInputKind::MouseUp(button) => {
            let b = match button {
                Button::Left => 0,
                Button::Middle => 1,
                Button::Right => 2,
            };
            (b, b'm')
        }
        MouseInputKind::MouseDrag(button) => {
            let b = match button {
                Button::Left => 32,
                Button::Middle => 33,
                Button::Right => 34,
            };
            (b, b'M')
        }
        MouseInputKind::MouseMove => (35, b'M'),
        MouseInputKind::ScrollUp => (64, b'M'),
        MouseInputKind::ScrollDown => (65, b'M'),
        MouseInputKind::ScrollLeft => (66, b'M'),
        MouseInputKind::ScrollRight => (67, b'M'),
    };

    format!("\x1b[<{};{};{}{}", code, x, y, suffix as char).into_bytes()
}

fn cursor_at(row_idx: usize, col_idx: usize, cursor: Option<r3bl_tui::Pos>) -> bool {
    cursor == Some(col(col_idx as u16) + row(row_idx as u16))
}

fn render_ofs_buf_to_ir(
    ofs_buf: &r3bl_tui::OffscreenBuffer,
    origin: r3bl_tui::Pos,
    max_rows: usize,
    cursor_pos: Option<r3bl_tui::Pos>,
) -> RenderOpIRVec {
    let mut ops = RenderOpIRVec::new();
    for (row_index, line) in ofs_buf.buffer.iter().enumerate().take(max_rows) {
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
                    col(col_index as u16) + row(row_index as u16),
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
                    col(run_start as u16) + row(row_index as u16),
                );
                ops += RenderOpCommon::ApplyColors(run_style);
                ops += RenderOpIR::PaintTextWithAttributes(text.into(), run_style);
            }
        }
    }

    ops
}
