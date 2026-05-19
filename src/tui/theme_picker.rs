use super::app::Id;
use super::fuzzy_picker::FuzzyPicker;
use super::state::{AppSignal, State, Window};
use r3bl_tui::{
    CommonResult, Component, EditMode, EditorComponent, EditorEngineConfig, EventPropagation,
    FlexBox, FlexBoxId, GlobalData, HasFocus, InputEvent, LayoutDirection, LineMode,
    RenderOpCommon, RenderOpIR, RenderOpIRVec, RenderPipeline, SurfaceBounds, SyntaxHighlightMode,
    TerminalWindowMainThreadSignal, ZOrder, col, height, new_style, render_pipeline, row,
    send_signal, throws_with_return, tui_color,
};
use tokio::sync::mpsc;

pub struct ThemePickerComponent {
    id: FlexBoxId,
    picker: FuzzyPicker,
    editor: EditorComponent<State, AppSignal>,
}

impl ThemePickerComponent {
    pub fn new(id: FlexBoxId) -> Self {
        let editor_id = FlexBoxId::from(Id::ThemePickerEditor);

        fn on_buffer_change(
            _id: FlexBoxId,
            main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
        ) {
            send_signal!(
                main_tx,
                TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::ThemePickerQueryChanged)
            );
        }

        let config = EditorEngineConfig {
            multiline_mode: LineMode::SingleLine,
            syntax_highlight: SyntaxHighlightMode::Disable,
            edit_mode: EditMode::ReadWrite,
        };

        Self {
            id,
            picker: FuzzyPicker::new(),
            editor: EditorComponent::new(editor_id, config, on_buffer_change),
        }
    }
}

impl Component<State, AppSignal> for ThemePickerComponent {
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
        let page_size = global_data.state.window_page_size(&Window::ThemePicker);
        let results = &global_data.state.theme_picker_results;
        let selected = &mut global_data.state.theme_picker_selected;
        if let Some(result) =
            self.picker
                .handle_navigation(&input_event, page_size, results, selected)
        {
            if let Some(ref name) = global_data.state.theme_picker_selected {
                if let Some(theme) = crate::tui::theme::HelixTheme::from_name(name) {
                    global_data.state.theme = theme;
                }
            }
            return Ok(result);
        }
        self.editor
            .handle_event(global_data, input_event, has_focus)
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
                id: self.editor.get_id(),
                dir: LayoutDirection::Horizontal,
                origin_pos: origin,
                bounds_size: bounds.col_width + height(1),
                style_adjusted_origin_pos: origin,
                style_adjusted_bounds_size: bounds.col_width + height(1),
                ..Default::default()
            };
            let saved_focus = has_focus.get_id();
            has_focus.set_id(self.editor.get_id());
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

            let results = &global_data.state.theme_picker_results;
            let selected = &global_data.state.theme_picker_selected;
            let result_ops = self.picker.render_results(
                &global_data.state,
                results_origin,
                result_rows,
                pane_width,
                results,
                selected,
                |name, _state| name.clone(),
            );
            let result_count = results.len();

            global_data
                .state
                .set_window_scroll(&Window::ThemePicker, self.picker.scroll_offset);
            global_data
                .state
                .set_window_scroll_max(&Window::ThemePicker, result_count);
            global_data
                .state
                .set_window_page_size(&Window::ThemePicker, result_rows);

            pipeline.push(ZOrder::Normal, result_ops);
            pipeline
        });
    }
}
