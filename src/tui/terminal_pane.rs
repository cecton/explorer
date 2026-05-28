use super::rmux_bridge::RmuxCommand;
use super::state::{AppSignal, State, Window};
use r3bl_tui::core::pty::PtyInputEvent;
use r3bl_tui::{
    Button, CommonResult, Component, EventPropagation, FlexBox, FlexBoxId, GlobalData, HasFocus,
    InputEvent, MouseInputKind, RenderOpCommon, RenderOpIR, RenderOpIRVec, RenderPipeline,
    SurfaceBounds, ZOrder, col, render_pipeline, row, throws_with_return,
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
            let pane_id = pane.rmux_pane_id;
            let tx = pane.rmux_cmd_tx.clone();

            match input_event {
                InputEvent::Keyboard(keypress) => {
                    if let Some(pty_event) = Option::<PtyInputEvent>::from(keypress) {
                        let data = match pty_event {
                            PtyInputEvent::Write(bytes) => bytes,
                            PtyInputEvent::WriteLine(text) => {
                                let mut b = text.into_bytes();
                                b.push(b'\n');
                                b
                            }
                            PtyInputEvent::SendControl(ctrl, mode) => {
                                ctrl.to_bytes(mode).into_owned()
                            }
                            _ => return Ok(EventPropagation::ConsumedRender),
                        };
                        let _ = tx.send(RmuxCommand::SendInput { pane_id, data });
                    }
                }
                InputEvent::Mouse(mouse) => {
                    let Some(slot) = pane_slot(self.id) else {
                        return Ok(EventPropagation::Propagate);
                    };
                    let box_ = &global_data.state.pane_boxes[slot];
                    let origin_row = box_.style_adjusted_origin_pos.row_index.as_u16() + 1;
                    let origin_col = box_.style_adjusted_origin_pos.col_index.as_u16();
                    let encoded = encode_mouse_event(mouse, origin_row, origin_col);
                    let _ = tx.send(RmuxCommand::SendInput {
                        pane_id,
                        data: encoded,
                    });
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
            let ofs_buf = &pane.ofs_buf;
            let origin = current_box.style_adjusted_origin_pos;

            let mut render_ops = render_ofs_buf_to_ir(ofs_buf, origin);

            let pane_width = current_box.style_adjusted_bounds_size.col_width.as_usize();
            let pane_height = current_box.style_adjusted_bounds_size.row_height.as_usize();
            let ofs_height = ofs_buf.window_size.row_height.as_usize();

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

fn render_ofs_buf_to_ir(
    ofs_buf: &r3bl_tui::OffscreenBuffer,
    origin: r3bl_tui::Pos,
) -> RenderOpIRVec {
    let mut ops = RenderOpIRVec::new();

    for (row_index, line) in ofs_buf.buffer.iter().enumerate() {
        let mut col_index = 0usize;
        while col_index < line.len() {
            if line[col_index] == r3bl_tui::PixelChar::Void {
                col_index += 1;
                continue;
            }

            let run_start = col_index;
            let run_style = match &line[col_index] {
                r3bl_tui::PixelChar::Spacer => None,
                r3bl_tui::PixelChar::PlainText { style, .. } => Some(*style),
                r3bl_tui::PixelChar::Void => unreachable!(),
            };

            let mut text = String::new();
            while col_index < line.len() {
                match &line[col_index] {
                    r3bl_tui::PixelChar::PlainText {
                        display_char,
                        style,
                        ..
                    } if Some(*style) == run_style => {
                        text.push(*display_char);
                        col_index += 1;
                    }
                    r3bl_tui::PixelChar::Spacer if run_style.is_none() => {
                        text.push(' ');
                        col_index += 1;
                    }
                    r3bl_tui::PixelChar::Void => {
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
