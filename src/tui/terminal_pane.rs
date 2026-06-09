use crate::tui::*;

pub struct TerminalPaneComponent {
    id: FlexBoxId,
}

impl TerminalPaneComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self { id }
    }

    fn terminal_id(&self, state: &AppState) -> Option<usize> {
        let slot = pane_slot(self.id)?;
        let Window::Terminal(id) = state.window_stack.get(slot)? else {
            return None;
        };
        Some(*id)
    }
}

impl TitleRow for TerminalPaneComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        theme: &HelixTheme,
        state: &AppState,
    ) -> usize {
        let id = self.terminal_id(state).unwrap_or(0);
        let (base, exited, exit_code, exit_signal) = state
            .terminal_panes
            .get(&id)
            .and_then(|p| p.lock().ok())
            .map(|g| {
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
        render_pane_title(ops, pane_box, &title, false, theme, focused);
        1
    }
}

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

impl Component<AppState, AppSignal> for TerminalPaneComponent {
    fn reset(&mut self) {}

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
                .terminal_panes
                .get(&id)
                .and_then(|p| p.lock().ok())
                .is_some_and(|p| p.exited)
            {
                if let Some(pane) = global_data.state.terminal_panes.remove(&id) {
                    let mut p = match pane.lock() {
                        Ok(guard) => guard,
                        Err(poison) => {
                            tracing::error!("pane lock poisoned: {poison}");
                            poison.into_inner()
                        }
                    };
                    if let Some(mut killer) = p.child_killer.take() {
                        let _ = killer.kill();
                    }
                }
                global_data.state.remove_window(&Window::Terminal(id));
                return Ok(EventPropagation::ConsumedRender);
            }

            let Some(pane_arc) = global_data.state.terminal_panes.get(&id).cloned() else {
                return Ok(EventPropagation::Propagate);
            };

            let mut pane = match pane_arc.lock() {
                Ok(guard) => guard,
                Err(poison) => {
                    tracing::error!("pane lock poisoned: {poison}");
                    poison.into_inner()
                }
            };
            let tx = pane.pty_input_tx.clone();
            let mouse_tracking_mode = pane.mouse_tracking_mode;
            let alternate_screen_active =
                pane.ofs_buf.terminal_mode.alternate_screen == AlternateScreenState::Active;
            let scrollback_len = pane.ofs_buf.scrollback_len();

            if global_data.state.terminal_grabbed
                && let InputEvent::Keyboard(keypress) = &input_event
            {
                if let Some(pty_event) = Option::<PtyInputEvent>::from(*keypress) {
                    let _ = tx.try_send(pty_event);
                }
                return Ok(EventPropagation::ConsumedRender);
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
                    global_data.state.terminal_grabbed = false;
                    let old_offset = pane.scroll_offset;
                    pane.scroll_offset = pane.scroll_offset.saturating_sub(3);
                    if pane.scroll_offset != old_offset {
                        EventPropagation::ConsumedRender
                    } else {
                        EventPropagation::Consumed
                    }
                }

                InputEvent::Mouse(mouse) => {
                    if !should_forward_mouse(&mouse.kind, mouse_tracking_mode) {
                        return Ok(EventPropagation::Consumed);
                    }
                    let Some(slot) = pane_slot(self.id) else {
                        return Ok(EventPropagation::Propagate);
                    };
                    let box_ = &global_data.state.pane_boxes[slot];
                    let origin_row = box_.style_adjusted_origin_pos.row_index.as_u16() + 1;
                    let origin_col = box_.style_adjusted_origin_pos.col_index.as_u16();
                    let encoded = encode_mouse_event(mouse, origin_row, origin_col);
                    let _ = tx.try_send(PtyInputEvent::Write(encoded));
                    EventPropagation::ConsumedRender
                }

                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Esc),
                })
                | InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Enter),
                }) => {
                    global_data.state.terminal_grabbed = true;
                    pane.scroll_offset = 0;
                    EventPropagation::ConsumedRender
                }

                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::PageUp),
                }) => {
                    let slot = pane_slot(self.id).unwrap_or(0);
                    let pane_height = global_data.state.pane_boxes[slot]
                        .style_adjusted_bounds_size
                        .row_height
                        .as_usize();
                    pane.scroll_offset = pane
                        .scroll_offset
                        .saturating_add(pane_height)
                        .min(pane.ofs_buf.scrollback_len());
                    EventPropagation::ConsumedRender
                }

                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::PageDown),
                }) => {
                    let slot = pane_slot(self.id).unwrap_or(0);
                    let pane_height = global_data.state.pane_boxes[slot]
                        .style_adjusted_bounds_size
                        .row_height
                        .as_usize();
                    pane.scroll_offset = pane.scroll_offset.saturating_sub(pane_height);
                    EventPropagation::ConsumedRender
                }

                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Up),
                }) => {
                    pane.scroll_offset = pane
                        .scroll_offset
                        .saturating_add(1)
                        .min(pane.ofs_buf.scrollback_len());
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
        global_data: &mut GlobalData<AppState, AppSignal>,
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
            let mut pane = match pane.lock() {
                Ok(guard) => guard,
                Err(poison) => {
                    tracing::error!("pane lock poisoned: {poison}");
                    poison.into_inner()
                }
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

            let scrollback_len = pane.ofs_buf.scrollback_len();
            let scroll_offset = pane.scroll_offset.min(scrollback_len);
            let buffer_height = pane.ofs_buf.buffer.len();

            let render_ops = if scroll_offset == 0 {
                let visible_rows = pane_height.min(buffer_height);
                let is_grabbed = global_data.state.terminal_grabbed;
                let cursor_visible = pane.ofs_buf.ansi_parser_support.cursor_visible && is_grabbed;
                let cursor_pos = if cursor_visible {
                    Some(pane.ofs_buf.cursor_pos)
                } else {
                    None
                };
                let mut ops = render_ofs_buf_to_ir(&pane.ofs_buf, origin, visible_rows, cursor_pos);
                if visible_rows < pane_height {
                    for row_idx in visible_rows..pane_height {
                        ops += RenderOpCommon::MoveCursorPositionRelTo(
                            origin,
                            col(0) + row(row_idx as u16),
                        );
                        ops += RenderOpIR::PaintTextWithAttributes(
                            " ".repeat(pane_width).into(),
                            None,
                        );
                    }
                }
                ops
            } else {
                let combined_bottom = scrollback_len + buffer_height;
                let viewport_bottom = combined_bottom.saturating_sub(scroll_offset);
                let viewport_top = viewport_bottom.saturating_sub(pane_height);

                let mut lines: Vec<&PixelCharLine> = Vec::with_capacity(pane_height);
                for combined_idx in viewport_top..viewport_bottom {
                    if let Some(line) = pane.ofs_buf.scrollback_get(combined_idx) {
                        lines.push(line);
                    } else if let Some(line) =
                        pane.ofs_buf.buffer.get(combined_idx - scrollback_len)
                    {
                        lines.push(line);
                    } else {
                        break;
                    }
                }

                let mut ops = render_lines_to_ir(&lines, origin, None);
                if lines.len() < pane_height {
                    for row_idx in lines.len()..pane_height {
                        ops += RenderOpCommon::MoveCursorPositionRelTo(
                            origin,
                            col(0) + row(row_idx as u16),
                        );
                        ops += RenderOpIR::PaintTextWithAttributes(
                            " ".repeat(pane_width).into(),
                            None,
                        );
                    }
                }
                ops
            };

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
    let lines: Vec<&PixelCharLine> = ofs_buf.buffer.iter().take(max_rows).collect();
    render_lines_to_ir(&lines, origin, cursor_pos)
}

fn render_lines_to_ir(
    lines: &[&PixelCharLine],
    origin: r3bl_tui::Pos,
    cursor_pos: Option<r3bl_tui::Pos>,
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
