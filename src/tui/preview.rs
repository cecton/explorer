use super::state::{AppSignal, State};
use r3bl_tui::{
    col, new_style, render_pipeline, row, send_signal, throws_with_return, tui_color,
    BoxedSafeComponent, CommonResult, Component, EventPropagation, FlexBox, FlexBoxId, GlobalData,
    HasFocus, InputEvent, Key, KeyPress, RenderOpCommon, RenderOpIR, RenderOpIRVec, RenderPipeline,
    SpecialKey, SurfaceBounds, TerminalWindowMainThreadSignal, ZOrder,
};

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

            let guard = state.lsp_colors.lock().unwrap();
            let colored = guard.get(&file_idx);

            for row_offset in 0..visible_rows {
                let line_idx = scroll + row_offset;
                if line_idx >= total_lines {
                    break;
                }
                render_ops +=
                    RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(row_offset));

                if let Some(spans) = colored.and_then(|lines| lines.get(line_idx)) {
                    for (text, color) in spans {
                        if let Some([r, g, b]) = color {
                            let fg = tui_color!(*r, *g, *b);
                            let style = new_style!(color_fg: {fg});
                            render_ops += RenderOpCommon::ApplyColors(Some(style));
                            render_ops += RenderOpIR::PaintTextWithAttributes(
                                text.as_str().into(),
                                Some(style),
                            );
                            render_ops += RenderOpCommon::ResetColor;
                        } else {
                            render_ops +=
                                RenderOpIR::PaintTextWithAttributes(text.as_str().into(), None);
                        }
                    }
                    continue;
                }

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

fn file_line<'a>(content: &'a str, line_starts: &[usize], idx: usize) -> &'a str {
    let start = line_starts[idx];
    let end = line_starts
        .get(idx + 1)
        .map(|&e| e - 1)
        .unwrap_or(content.len());
    &content[start..end]
}
