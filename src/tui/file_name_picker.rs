use super::app::Id;
use super::state::{AppSignal, State};
use r3bl_tui::{
    BoxedSafeComponent, CommonResult, Component, EditMode, EditorComponent, EditorEngineConfig,
    EventPropagation, FlexBox, FlexBoxId, GlobalData, HasFocus, InputEvent, LayoutDirection,
    LineMode, MouseInputKind, RenderOpCommon, RenderOpIR, RenderOpIRVec, RenderPipeline,
    SurfaceBounds, SyntaxHighlightMode, TerminalWindowMainThreadSignal, ZOrder, col, height,
    new_style, row, send_signal, throws_with_return, tui_color,
};
use std::collections::HashSet;
use tokio::sync::mpsc;

pub struct FileNamePickerComponent {
    id: FlexBoxId,
    scroll_offset: usize,
    editor: EditorComponent<State, AppSignal>,
}

impl FileNamePickerComponent {
    pub fn new_boxed(id: FlexBoxId) -> BoxedSafeComponent<State, AppSignal> {
        let editor_id = FlexBoxId::from(Id::FileNamePickerEditor);

        fn on_buffer_change(
            _id: FlexBoxId,
            main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
        ) {
            send_signal!(
                main_tx,
                TerminalWindowMainThreadSignal::ApplyAppSignal(
                    AppSignal::FileNamePickerQueryChanged
                )
            );
        }

        let config = EditorEngineConfig {
            multiline_mode: LineMode::SingleLine,
            syntax_highlight: SyntaxHighlightMode::Disable,
            edit_mode: EditMode::ReadWrite,
        };

        Box::new(Self {
            id,
            scroll_offset: 0,
            editor: EditorComponent::new(editor_id, config, on_buffer_change),
        })
    }
}

impl Component<State, AppSignal> for FileNamePickerComponent {
    fn reset(&mut self) {}

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
        input_event: InputEvent,
        has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        throws_with_return!({
            if let InputEvent::Mouse(mouse_input) = input_event {
                match mouse_input.kind {
                    MouseInputKind::ScrollUp => {
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::FileNamePickerSelectPrev,
                            )
                        );
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    MouseInputKind::ScrollDown => {
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::FileNamePickerSelectNext,
                            )
                        );
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    _ => {}
                }
            }

            // Forward all other input to the editor component.
            self.editor
                .handle_event(global_data, input_event, has_focus)?
        });
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
        current_box: FlexBox,
        surface_bounds: SurfaceBounds,
        has_focus: &mut HasFocus,
    ) -> CommonResult<RenderPipeline> {
        throws_with_return!({
            let origin = current_box.style_adjusted_origin_pos;
            let bounds = current_box.style_adjusted_bounds_size;
            let total_rows = bounds.row_height.as_usize();

            // Row 0: editor input bar.
            let editor_box = FlexBox {
                id: FlexBoxId::from(Id::FileNamePickerEditor),
                dir: LayoutDirection::Horizontal,
                origin_pos: origin,
                bounds_size: bounds.col_width + height(1),
                style_adjusted_origin_pos: origin,
                style_adjusted_bounds_size: bounds.col_width + height(1),
                ..Default::default()
            };
            // Temporarily give focus to the editor id so render_caret paints the
            // reverse-video fake caret, then restore focus to the picker id.
            has_focus.set_id(FlexBoxId::from(Id::FileNamePickerEditor));
            let mut pipeline =
                self.editor
                    .render(global_data, editor_box, surface_bounds, has_focus)?;
            has_focus.set_id(self.id);

            // Remaining rows: results list.
            if total_rows < 2 {
                return Ok(pipeline);
            }

            let results_origin = origin + height(1);
            let result_rows = total_rows - 1;

            let color_match_fg = tui_color!(255, 200, 60);
            let color_normal_fg = tui_color!(170, 170, 200);
            let color_selected_bg = tui_color!(50, 50, 90);
            let color_dim_fg = tui_color!(90, 90, 110);

            let mut render_ops = RenderOpIRVec::new();

            let selected = global_data.state.file_name_picker_selected;
            let result_count = global_data.state.file_name_picker_results.len();

            if selected < self.scroll_offset {
                self.scroll_offset = selected;
            } else if result_count > 0 && selected >= self.scroll_offset + result_rows {
                self.scroll_offset = selected + 1 - result_rows;
            }

            for row_offset in 0..result_rows {
                let result_idx = self.scroll_offset + row_offset;
                render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                    results_origin,
                    col(0) + row(row_offset),
                );

                if result_idx >= result_count {
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        " ".into(),
                        Some(new_style!(color_fg: {color_dim_fg})),
                    );
                    continue;
                }

                let (file_idx, matched_positions) = {
                    let (idx, ref pos) = global_data.state.file_name_picker_results[result_idx];
                    (idx, pos.clone())
                };
                let root = global_data.state.root.clone();
                let snapshot = global_data.state.files.load_full();
                let file = &snapshot[file_idx];
                let rel = file.path.strip_prefix(&root).unwrap_or(&file.path);
                let path_str = rel.to_string();
                let is_selected = result_idx == selected;

                let matched_set: HashSet<u32> = matched_positions.iter().copied().collect();

                if is_selected {
                    render_ops += RenderOpCommon::SetBgColor(color_selected_bg);
                }

                for (char_idx, ch) in path_str.chars().enumerate() {
                    let is_match = matched_set.contains(&(char_idx as u32));
                    let fg = if is_match {
                        color_match_fg
                    } else {
                        color_normal_fg
                    };
                    let style = if is_selected && is_match {
                        new_style!(bold color_fg: {fg} color_bg: {color_selected_bg})
                    } else if is_selected {
                        new_style!(color_fg: {fg} color_bg: {color_selected_bg})
                    } else if is_match {
                        new_style!(bold color_fg: {fg})
                    } else {
                        new_style!(color_fg: {fg})
                    };
                    let mut buf = [0u8; 4];
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        ch.encode_utf8(&mut buf).to_string().into(),
                        Some(style),
                    );
                }

                if is_selected {
                    render_ops += RenderOpCommon::ResetColor;
                }
            }

            pipeline.push(ZOrder::Normal, render_ops);
            pipeline
        });
    }
}
