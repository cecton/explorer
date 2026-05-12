use super::state::{AppSignal, State};
use r3bl_tui::{
    BoxedSafeComponent, CommonResult, Component, EventPropagation, FlexBox, FlexBoxId, GlobalData,
    HasFocus, InputEvent, Key, KeyPress, RenderOpCommon, RenderOpIR, RenderOpIRVec, RenderPipeline,
    SpecialKey, SurfaceBounds, TerminalWindowMainThreadSignal, ZOrder, col, new_style,
    render_pipeline, row, send_signal, throws_with_return, tui_color,
};

pub struct FileListComponent {
    id: FlexBoxId,
    scroll_offset: usize,
}

impl FileListComponent {
    pub fn new_boxed(id: FlexBoxId) -> BoxedSafeComponent<State, AppSignal> {
        Box::new(Self {
            id,
            scroll_offset: 0,
        })
    }
}

impl Component<State, AppSignal> for FileListComponent {
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
                    Key::SpecialKey(SpecialKey::Up) => {
                        consumed = true;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::SelectPrev,)
                        );
                    }
                    Key::SpecialKey(SpecialKey::Down) => {
                        consumed = true;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::SelectNext,)
                        );
                    }
                    Key::SpecialKey(SpecialKey::Enter) => {
                        consumed = true;
                        send_signal!(
                            global_data.main_thread_channel_sender,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::OpenSelected,)
                        );
                    }
                    Key::SpecialKey(SpecialKey::PageUp) => {
                        if global_data.state.open_file.is_some() {
                            consumed = true;
                            let page = global_data.state.preview_page_size.max(1);
                            send_signal!(
                                global_data.main_thread_channel_sender,
                                TerminalWindowMainThreadSignal::ApplyAppSignal(
                                    AppSignal::ScrollPreviewUp(page),
                                )
                            );
                        }
                    }
                    Key::SpecialKey(SpecialKey::PageDown) => {
                        if global_data.state.open_file.is_some() {
                            consumed = true;
                            let page = global_data.state.preview_page_size.max(1);
                            send_signal!(
                                global_data.main_thread_channel_sender,
                                TerminalWindowMainThreadSignal::ApplyAppSignal(
                                    AppSignal::ScrollPreviewDown(page),
                                )
                            );
                        }
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
            let files = &state.files;
            let selected = state.selected;
            let origin = current_box.style_adjusted_origin_pos;
            let bounds = current_box.style_adjusted_bounds_size;
            let visible_rows = bounds.row_height.as_usize();

            if selected < self.scroll_offset {
                self.scroll_offset = selected;
            } else if selected >= self.scroll_offset + visible_rows {
                self.scroll_offset = selected + 1 - visible_rows;
            }

            let color_selected_bg = tui_color!(60, 60, 100);
            let color_selected_fg = tui_color!(255, 220, 100);
            let color_normal_fg = tui_color!(180, 180, 210);
            let color_dim_fg = tui_color!(100, 100, 130);

            let mut render_ops = RenderOpIRVec::new();

            for (row_offset, idx) in
                (self.scroll_offset..(self.scroll_offset + visible_rows)).enumerate()
            {
                render_ops +=
                    RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(row_offset));

                if idx < files.len() {
                    let file = &files[idx];
                    let rel = file.path.strip_prefix(&state.root).unwrap_or(&file.path);
                    let text = rel.as_str();
                    let style = if idx == selected {
                        new_style!(bold color_fg: {color_selected_fg} color_bg: {color_selected_bg})
                    } else {
                        new_style!(color_fg: {color_normal_fg})
                    };
                    render_ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
                } else {
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        " ".into(),
                        Some(new_style!(color_fg: {color_dim_fg})),
                    );
                }
            }

            let mut pipeline = render_pipeline!();
            pipeline.push(ZOrder::Normal, render_ops);
            pipeline
        });
    }
}
