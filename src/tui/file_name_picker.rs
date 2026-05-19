use super::app::{Id, resolve_selected};
use super::state::{AppSignal, State, Window};
use r3bl_tui::{
    CommonResult, Component, EditMode, EditorComponent, EditorEngineConfig, EventPropagation,
    FlexBox, FlexBoxId, GlobalData, HasFocus, InputEvent, Key, KeyPress, LayoutDirection, LineMode,
    MouseInputKind, RenderOpCommon, RenderOpIR, RenderOpIRVec, RenderPipeline, SpecialKey,
    SurfaceBounds, SyntaxHighlightMode, TerminalWindowMainThreadSignal, ZOrder, col, height,
    new_style, render_pipeline, row, send_signal, throws_with_return, tui_color,
};
use std::collections::HashSet;
use tokio::sync::mpsc;

pub struct FileNamePickerComponent {
    id: FlexBoxId,
    scroll_offset: usize,
    editor: EditorComponent<State, AppSignal>,
}

impl FileNamePickerComponent {
    pub fn new(id: FlexBoxId) -> Self {
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

        Self {
            id,
            scroll_offset: 0,
            editor: EditorComponent::new(editor_id, config, on_buffer_change),
        }
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
                        let state = &mut global_data.state;
                        let count = state.file_name_picker_results.len();
                        if count > 0 {
                            let current = resolve_selected(
                                &state.file_name_picker_selected,
                                &state.file_name_picker_results,
                            );
                            let prev = current.saturating_sub(1);
                            if let Some((key, _)) = state.file_name_picker_results.get(prev) {
                                state.file_name_picker_selected = Some(*key);
                            }
                        }
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    MouseInputKind::ScrollDown => {
                        let state = &mut global_data.state;
                        let count = state.file_name_picker_results.len();
                        if count > 0 {
                            let current = resolve_selected(
                                &state.file_name_picker_selected,
                                &state.file_name_picker_results,
                            );
                            let next = (current + 1).min(count - 1);
                            let (key, _) = &state.file_name_picker_results[next];
                            state.file_name_picker_selected = Some(*key);
                        }
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    _ => {
                        // Fall through to editor component for other mouse events.
                    }
                }
            }

            if let InputEvent::Keyboard(KeyPress::Plain { key }) = input_event
                && matches!(
                    key,
                    Key::SpecialKey(SpecialKey::PageUp | SpecialKey::PageDown)
                )
            {
                let state = &mut global_data.state;
                let count = state.file_name_picker_results.len();
                if count > 0 {
                    let current = resolve_selected(
                        &state.file_name_picker_selected,
                        &state.file_name_picker_results,
                    );
                    let page = state
                        .window_page_size(&Window::FileNamePicker)
                        .saturating_sub(1)
                        .max(1);
                    if key == Key::SpecialKey(SpecialKey::PageDown) {
                        let next = (current + page).min(count - 1);
                        let (key, _) = state.file_name_picker_results[next];
                        state.file_name_picker_selected = Some(key);
                    } else {
                        let prev = current.saturating_sub(page);
                        if let Some((key, _)) = state.file_name_picker_results.get(prev) {
                            state.file_name_picker_selected = Some(*key);
                        }
                    }
                }
                return Ok(EventPropagation::ConsumedRender);
            }

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
            let pane_width = bounds.col_width.as_usize();

            let bg_rgb = global_data
                .state
                .theme
                .ui_bg("ui.background")
                .unwrap_or([15, 15, 25]);
            let color_bg = tui_color!(bg_rgb[0], bg_rgb[1], bg_rgb[2]);
            let bg_style = new_style!(color_bg: {color_bg});

            // Fill editor row with pane background, then render editor on top.
            let mut pipeline = render_pipeline!();
            let mut editor_bg_ops = RenderOpIRVec::new();
            editor_bg_ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
            editor_bg_ops += RenderOpCommon::ApplyColors(Some(bg_style));
            editor_bg_ops += RenderOpIR::PaintTextWithAttributes(
                " ".repeat(pane_width).as_str().into(),
                Some(bg_style),
            );
            pipeline.push(ZOrder::Normal, editor_bg_ops);

            let editor_box = FlexBox {
                id: FlexBoxId::from(Id::FileNamePickerEditor),
                dir: LayoutDirection::Horizontal,
                origin_pos: origin,
                bounds_size: bounds.col_width + height(1),
                style_adjusted_origin_pos: origin,
                style_adjusted_bounds_size: bounds.col_width + height(1),
                ..Default::default()
            };
            let saved_focus = has_focus.get_id();
            has_focus.set_id(FlexBoxId::from(Id::FileNamePickerEditor));
            let editor_pipeline =
                self.editor
                    .render(global_data, editor_box, surface_bounds, has_focus)?;
            pipeline.join_into(editor_pipeline);
            if let Some(id) = saved_focus {
                has_focus.set_id(id);
            }

            if total_rows < 2 {
                return Ok(pipeline);
            }

            let results_origin = origin + height(1);
            let result_rows = total_rows - 1;

            let match_rgb = global_data
                .state
                .theme
                .ui_fg("ui.cursor.match")
                .unwrap_or([255, 200, 60]);
            let normal_rgb = global_data
                .state
                .theme
                .ui_fg("ui.text")
                .unwrap_or([170, 170, 200]);
            let selected_rgb = global_data
                .state
                .theme
                .ui_bg("ui.selection")
                .unwrap_or([50, 50, 90]);
            let color_match_fg = tui_color!(match_rgb[0], match_rgb[1], match_rgb[2]);
            let color_normal_fg = tui_color!(normal_rgb[0], normal_rgb[1], normal_rgb[2]);
            let color_selected_bg = tui_color!(selected_rgb[0], selected_rgb[1], selected_rgb[2]);

            let mut render_ops = RenderOpIRVec::new();

            let selected = resolve_selected(
                &global_data.state.file_name_picker_selected,
                &global_data.state.file_name_picker_results,
            );
            let result_count = global_data.state.file_name_picker_results.len();

            if selected < self.scroll_offset {
                self.scroll_offset = selected;
            } else if result_count > 0 && selected >= self.scroll_offset + result_rows {
                self.scroll_offset = selected + 1 - result_rows;
            }

            let picker_window = Window::FileNamePicker;
            global_data
                .state
                .set_window_scroll(&picker_window, self.scroll_offset);
            global_data
                .state
                .set_window_scroll_max(&picker_window, result_count);
            global_data
                .state
                .set_window_page_size(&picker_window, result_rows);

            for row_offset in 0..result_rows {
                let result_idx = self.scroll_offset + row_offset;
                render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                    results_origin,
                    col(0) + row(row_offset),
                );

                let is_selected = result_idx < result_count && result_idx == selected;
                let row_bg = if is_selected {
                    color_selected_bg
                } else {
                    color_bg
                };
                let row_bg_style = new_style!(color_bg: {row_bg});

                render_ops += RenderOpCommon::ApplyColors(Some(row_bg_style));
                render_ops += RenderOpIR::PaintTextWithAttributes(
                    " ".repeat(pane_width).as_str().into(),
                    Some(row_bg_style),
                );

                if result_idx >= result_count {
                    continue;
                }

                render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                    results_origin,
                    col(0) + row(row_offset),
                );

                let (file_key, matched_positions) = {
                    let (key, ref pos) = global_data.state.file_name_picker_results[result_idx];
                    (key, pos.clone())
                };
                let root = global_data.state.root.clone();
                let snapshot = global_data.state.files.load_full();
                let file = &snapshot[file_key.0];
                let rel = file.path.strip_prefix(&root).unwrap_or(&file.path);
                let path_str = rel.to_string();

                let matched_set: HashSet<u32> = matched_positions.iter().copied().collect();

                for (char_idx, ch) in path_str.chars().enumerate() {
                    let is_match = matched_set.contains(&(char_idx as u32));
                    let fg = if is_match {
                        color_match_fg
                    } else {
                        color_normal_fg
                    };
                    let style = if is_selected && is_match {
                        new_style!(bold color_fg: {fg} color_bg: {row_bg})
                    } else if is_selected {
                        new_style!(color_fg: {fg} color_bg: {row_bg})
                    } else if is_match {
                        new_style!(bold color_fg: {fg} color_bg: {row_bg})
                    } else {
                        new_style!(color_fg: {fg} color_bg: {row_bg})
                    };
                    let mut buf = [0u8; 4];
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        ch.encode_utf8(&mut buf).to_string().into(),
                        Some(style),
                    );
                }
            }

            pipeline.push(ZOrder::Normal, render_ops);
            pipeline
        });
    }
}
