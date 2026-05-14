use super::state::{AppSignal, State};
use r3bl_tui::{
    BoxedSafeComponent, CommonResult, Component, EventPropagation, FlexBox, FlexBoxId, GlobalData,
    HasFocus, InputEvent, Key, KeyPress, MouseInputKind, RenderOpCommon, RenderOpIR, RenderOpIRVec,
    RenderPipeline, SpecialKey, SurfaceBounds, TerminalWindowMainThreadSignal, ZOrder, col,
    new_style, render_pipeline, row, send_signal, throws_with_return, tui_color,
};

const DEFAULT_FG: [u8; 3] = [212, 212, 212];

pub struct FilePreviewComponent {
    id: FlexBoxId,
}

impl FilePreviewComponent {
    pub fn new_boxed(id: FlexBoxId) -> BoxedSafeComponent<State, AppSignal> {
        Box::new(Self { id })
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
            let mut consumed = false;
            if let InputEvent::Keyboard(KeyPress::Plain { key }) = input_event {
                match key {
                    Key::SpecialKey(SpecialKey::PageUp) => {
                        consumed = true;
                        let page = global_data.state.preview_page_size;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::ScrollPreviewUp(page),
                            )
                        );
                    }
                    Key::SpecialKey(SpecialKey::PageDown) => {
                        consumed = true;
                        let page = global_data.state.preview_page_size;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::ScrollPreviewDown(page),
                            )
                        );
                    }
                    Key::SpecialKey(SpecialKey::Up) => {
                        consumed = true;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::ScrollPreviewUp(1),
                            )
                        );
                    }
                    Key::SpecialKey(SpecialKey::Down) => {
                        consumed = true;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::ScrollPreviewDown(1),
                            )
                        );
                    }
                    _ => {}
                }
            }
            if let InputEvent::Mouse(mouse) = input_event {
                match mouse.kind {
                    MouseInputKind::ScrollUp => {
                        consumed = true;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::ScrollPreviewUp(1),
                            )
                        );
                    }
                    MouseInputKind::ScrollDown => {
                        consumed = true;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::ScrollPreviewDown(1),
                            )
                        );
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

            global_data.state.preview_page_size = visible_rows;

            let state = &global_data.state;
            let mut render_ops = RenderOpIRVec::new();

            let Some(file_idx) = state.open_file else {
                let mut pipeline = render_pipeline!();
                pipeline.push(ZOrder::Normal, render_ops);
                return Ok(pipeline);
            };

            let file = &state.files[file_idx];
            let scroll = state.preview_scroll;
            let total_lines = file.line_starts.len();

            let colored_guard = file.colored_lines.lock().unwrap();

            for row_offset in 0..visible_rows {
                let line_idx = scroll + row_offset;
                if line_idx >= total_lines {
                    break;
                }
                render_ops +=
                    RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(row_offset));
                render_ops += RenderOpCommon::ResetColor;

                if let Some(spans) = colored_guard.get(line_idx) {
                    let line_content = file_line(&file.content, &file.line_starts, line_idx);
                    for &(start, end, token_type) in spans {
                        let text = &line_content[start..end];
                        if let Some([r, g, b]) = token_color(token_type) {
                            let fg = tui_color!(r, g, b);
                            let style = new_style!(color_fg: {fg});
                            render_ops += RenderOpCommon::ApplyColors(Some(style));
                            render_ops +=
                                RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
                            render_ops += RenderOpCommon::ResetColor;
                        } else {
                            let default_style = new_style!(color_fg: {tui_color!(DEFAULT_FG[0], DEFAULT_FG[1], DEFAULT_FG[2])});
                            render_ops += RenderOpCommon::ApplyColors(Some(default_style));
                            render_ops += RenderOpIR::PaintTextWithAttributes(text.into(), None);
                        }
                    }
                    continue;
                }

                let default_style =
                    new_style!(color_fg: {tui_color!(DEFAULT_FG[0], DEFAULT_FG[1], DEFAULT_FG[2])});
                render_ops += RenderOpCommon::ApplyColors(Some(default_style));
                render_ops += RenderOpIR::PaintTextWithAttributes(
                    file_line(&file.content, &file.line_starts, line_idx).into(),
                    None,
                );
            }

            let mut pipeline = render_pipeline!();
            pipeline.push(ZOrder::Normal, render_ops);
            pipeline
        });
    }
}

fn token_color(token_type: &str) -> Option<[u8; 3]> {
    match token_type {
        "keyword" | "modifier" | "selfKeyword" | "boolean" => Some([204, 120, 50]),
        "string" | "comment" | "character" | "escapeSequence" => Some([106, 153, 85]),
        "number" | "const" | "static" => Some([181, 206, 168]),
        "type" | "class" | "struct" | "enum" | "interface" | "namespace" | "builtinType"
        | "typeAlias" | "typeParameter" | "constParameter" | "generic" | "toolModule" => {
            Some([78, 201, 176])
        }
        "function" | "method" => Some([220, 220, 170]),
        "macro" | "attributeBracket" | "builtinAttribute" | "decorator" => Some([189, 99, 197]),
        "variable" | "parameter" => Some([156, 220, 254]),
        "property" | "enumMember" => Some([206, 145, 120]),
        "operator" | "lifetime" => Some([212, 212, 212]),
        _ => None,
    }
}

fn file_line<'a>(content: &'a str, line_starts: &[usize], idx: usize) -> &'a str {
    let start = line_starts[idx];
    let end = line_starts
        .get(idx + 1)
        .map(|&e| e - 1)
        .unwrap_or(content.len());
    &content[start..end]
}
