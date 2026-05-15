use super::state::{AppSignal, State};
use r3bl_tui::{
    BoxedSafeComponent, CommonResult, Component, EventPropagation, FlexBox, FlexBoxId, GlobalData,
    HasFocus, InputEvent, Key, KeyPress, MouseInputKind, RenderOpCommon, RenderOpIR, RenderOpIRVec,
    RenderPipeline, SpecialKey, SurfaceBounds, TerminalWindowMainThreadSignal, ZOrder, col,
    new_style, render_pipeline, row, send_signal, throws_with_return, tui_color,
};
use std::collections::HashSet;

pub struct FileNamePickerComponent {
    id: FlexBoxId,
    scroll_offset: usize,
}

impl FileNamePickerComponent {
    pub fn new_boxed(id: FlexBoxId) -> BoxedSafeComponent<State, AppSignal> {
        Box::new(Self {
            id,
            scroll_offset: 0,
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
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        throws_with_return!({
            let mut consumed = false;
            if let InputEvent::Keyboard(KeyPress::Plain { key }) = input_event {
                match key {
                    Key::SpecialKey(SpecialKey::Esc) => {
                        consumed = true;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::CloseFileNamePicker,
                            )
                        );
                    }
                    Key::SpecialKey(SpecialKey::Enter) => {
                        consumed = true;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::FileNamePickerConfirm,
                            )
                        );
                    }
                    Key::SpecialKey(SpecialKey::Up) => {
                        consumed = true;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::FileNamePickerSelectPrev,
                            )
                        );
                    }
                    Key::SpecialKey(SpecialKey::Down) => {
                        consumed = true;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::FileNamePickerSelectNext,
                            )
                        );
                    }
                    Key::SpecialKey(SpecialKey::Backspace) => {
                        consumed = true;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::FileNamePickerBackspace,
                            )
                        );
                    }
                    Key::Character(c) => {
                        consumed = true;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::FileNamePickerChar(c),
                            )
                        );
                    }
                    _ => {}
                }
            }
            if !consumed && let InputEvent::Mouse(mouse_input) = input_event {
                match mouse_input.kind {
                    MouseInputKind::ScrollUp => {
                        consumed = true;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::FileNamePickerSelectPrev,
                            )
                        );
                    }
                    MouseInputKind::ScrollDown => {
                        consumed = true;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::FileNamePickerSelectNext,
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
            let state = &global_data.state;
            let origin = current_box.style_adjusted_origin_pos;
            let bounds = current_box.style_adjusted_bounds_size;
            let visible_rows = bounds.row_height.as_usize();

            let color_query_bg = tui_color!(30, 30, 50);
            let color_query_fg = tui_color!(220, 220, 220);
            let color_match_fg = tui_color!(255, 200, 60);
            let color_normal_fg = tui_color!(170, 170, 200);
            let color_selected_bg = tui_color!(50, 50, 90);
            let color_dim_fg = tui_color!(90, 90, 110);

            let mut render_ops = RenderOpIRVec::new();

            // Row 0: query bar.
            render_ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
            let query_text = format!("> {}_", state.file_name_picker_query);
            render_ops += RenderOpIR::PaintTextWithAttributes(
                query_text.into(),
                Some(new_style!(bold color_fg: {color_query_fg} color_bg: {color_query_bg})),
            );

            if visible_rows < 2 {
                let mut pipeline = render_pipeline!();
                pipeline.push(ZOrder::Normal, render_ops);
                return Ok(pipeline);
            }

            let result_rows = visible_rows - 1;
            let selected = state.file_name_picker_selected;
            let result_count = state.file_name_picker_results.len();

            if selected < self.scroll_offset {
                self.scroll_offset = selected;
            } else if result_count > 0 && selected >= self.scroll_offset + result_rows {
                self.scroll_offset = selected + 1 - result_rows;
            }

            for row_offset in 0..result_rows {
                let result_idx = self.scroll_offset + row_offset;
                render_ops +=
                    RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(row_offset + 1));

                if result_idx >= result_count {
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        " ".into(),
                        Some(new_style!(color_fg: {color_dim_fg})),
                    );
                    continue;
                }

                let (file_idx, ref matched_positions) = state.file_name_picker_results[result_idx];
                let snapshot = state.files.load();
                let file = &snapshot[file_idx];
                let rel = file.path.strip_prefix(&state.root).unwrap_or(&file.path);
                let path_str = rel.as_str();
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

            let mut pipeline = render_pipeline!();
            pipeline.push(ZOrder::Normal, render_ops);
            pipeline
        });
    }
}
