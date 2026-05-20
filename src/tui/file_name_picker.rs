use super::fuzzy_picker::FuzzyPicker;
use super::input_line::InputLine;
use super::state::{AppSignal, State, Window};
use r3bl_tui::{
    CommonResult, Component, EventPropagation, FlexBox, FlexBoxId, GlobalData, HasFocus,
    InputEvent, RenderPipeline, SurfaceBounds, TerminalWindowMainThreadSignal, ZOrder, height,
    render_pipeline, throws_with_return,
};

pub struct FileNamePickerComponent {
    id: FlexBoxId,
    picker: FuzzyPicker,
    input_line: InputLine,
}

impl FileNamePickerComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self {
            id,
            picker: FuzzyPicker::new(),
            input_line: InputLine::new(),
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
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        let page_size = global_data.state.window_page_size(&Window::FileNamePicker);
        let results = &global_data.state.file_name_picker_results;
        let selected = &mut global_data.state.file_name_picker_selected;
        if let Some(result) =
            self.picker
                .handle_navigation(&input_event, page_size, results, selected)
        {
            return Ok(result);
        }
        if self
            .input_line
            .handle_key(&input_event, &mut global_data.state.file_name_picker_query)
        {
            let _ = global_data.main_thread_channel_sender.try_send(
                TerminalWindowMainThreadSignal::ApplyAppSignal(
                    AppSignal::FileNamePickerQueryChanged,
                ),
            );
            return Ok(EventPropagation::ConsumedRender);
        }
        Ok(EventPropagation::Propagate)
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
        current_box: FlexBox,
        _surface_bounds: SurfaceBounds,
        has_focus: &mut HasFocus,
    ) -> CommonResult<RenderPipeline> {
        throws_with_return!({
            let origin = current_box.style_adjusted_origin_pos;
            let bounds = current_box.style_adjusted_bounds_size;
            let total_rows = bounds.row_height.as_usize();
            let pane_width = bounds.col_width.as_usize();

            let focused = has_focus.get_id() == Some(self.id);
            let query = global_data.state.file_name_picker_query.clone();

            let mut pipeline = render_pipeline!();
            let editor_ops = self.input_line.render(
                &query,
                &global_data.state,
                origin,
                bounds.col_width.as_u16(),
                focused,
            );
            pipeline.push(ZOrder::Normal, editor_ops);

            if total_rows < 2 {
                return Ok(pipeline);
            }

            let results_origin = origin + height(1);
            let result_rows = total_rows - 1;

            let results = &global_data.state.file_name_picker_results;
            let selected = &global_data.state.file_name_picker_selected;
            let result_ops = self.picker.render_results(
                &global_data.state,
                results_origin,
                result_rows,
                pane_width,
                results,
                selected,
                |key, state| {
                    let snapshot = state.files.load_full();
                    let file = &snapshot[key.0];
                    let rel = file.path.strip_prefix(&state.root).unwrap_or(&file.path);
                    rel.to_string()
                },
            );
            let result_count = results.len();

            global_data
                .state
                .set_window_scroll(&Window::FileNamePicker, self.picker.scroll_offset);
            global_data
                .state
                .set_window_scroll_max(&Window::FileNamePicker, result_count);
            global_data
                .state
                .set_window_page_size(&Window::FileNamePicker, result_rows);

            pipeline.push(ZOrder::Normal, result_ops);
            pipeline
        });
    }
}
