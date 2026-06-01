use super::fuzzy_picker::FuzzyPicker;
use super::input_line::InputLine;
use super::state::{AppSignal, State, Window};
use super::theme::HelixTheme;
use r3bl_tui::{
    CommonResult, Component, EventPropagation, FlexBox, FlexBoxId, GlobalData, HasFocus,
    InputEvent, Pos, RenderOpCommon, RenderOpIR, RenderOpIRVec, RenderPipeline, SurfaceBounds,
    TerminalWindowMainThreadSignal, ZOrder, col, new_style, render_pipeline, row,
    throws_with_return, tui_color,
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

    pub fn render_title_row(
        &self,
        mut ops: &mut RenderOpIRVec,
        origin: Pos,
        width: u16,
        focused: bool,
        theme: &HelixTheme,
        query: &str,
    ) {
        let (bg_rgb, fg_rgb) = title_bar_colors(focused, theme);
        let color_bg = tui_color!(bg_rgb[0], bg_rgb[1], bg_rgb[2]);
        let color_fg = tui_color!(fg_rgb[0], fg_rgb[1], fg_rgb[2]);
        let bg_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});

        ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
        ops += RenderOpCommon::SetBgColor(color_bg);
        ops += RenderOpIR::PaintTextWithAttributes(
            " ".repeat(width as usize).as_str().into(),
            Some(bg_style),
        );
        self.input_line
            .render(ops, query, origin, width, focused, bg_rgb, fg_rgb);
    }
}

fn title_bar_colors(focused: bool, theme: &HelixTheme) -> ([u8; 3], [u8; 3]) {
    if focused {
        (
            theme.ui_bg("ui.selection").unwrap_or([50, 50, 90]),
            theme.ui_fg("ui.text").unwrap_or([220, 220, 255]),
        )
    } else {
        (
            theme.ui_bg("ui.statusline").unwrap_or([30, 30, 50]),
            theme.ui_fg("ui.statusline").unwrap_or([180, 180, 220]),
        )
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
        _has_focus: &mut HasFocus,
    ) -> CommonResult<RenderPipeline> {
        throws_with_return!({
            let origin = current_box.style_adjusted_origin_pos;
            let bounds = current_box.style_adjusted_bounds_size;
            let total_rows = bounds.row_height.as_usize();
            let pane_width = bounds.col_width.as_usize();

            let mut pipeline = render_pipeline!();

            if total_rows == 0 {
                return Ok(pipeline);
            }

            let results = &global_data.state.file_name_picker_results;
            let selected = &global_data.state.file_name_picker_selected;
            let result_ops = self.picker.render_results(
                &global_data.state,
                origin,
                total_rows,
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
                .set_window_page_size(&Window::FileNamePicker, total_rows);

            pipeline.push(ZOrder::Normal, result_ops);
            pipeline
        });
    }
}
