use super::file_list::FileListComponent;
use super::preview::FilePreviewComponent;
use super::state::{AppSignal, State};
use crate::LoadedFile;
use crate::lsp;
use camino::Utf8PathBuf;
use r3bl_tui::{
    App, BoxedSafeApp, CommonResult, ComponentRegistry, ComponentRegistryMap, ContainsResult,
    EventPropagation, FlexBoxId, GlobalData, HasFocus, InputDevice, InputEvent, LayoutDirection,
    LayoutManagement, LengthOps, OutputDevice, PerformPositioningAndSizing, RenderOpCommon,
    RenderOpIR, RenderOpIRVec, RenderPipeline, SPACER_GLYPH, Size, Surface, SurfaceProps,
    SurfaceRender, TerminalWindow, TuiStylesheet, ZOrder, box_end, box_start, col, height,
    key_press, new_style, ok, render_component_in_current_box, render_tui_styled_texts_into,
    req_size_pc, row, surface, throws, throws_with_return, tui_color, tui_styled_text,
    tui_styled_texts, tui_stylesheet,
};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Id {
    Container = 1,
    FileList = 2,
    Preview = 3,
}

impl From<Id> for u8 {
    fn from(id: Id) -> u8 {
        id as u8
    }
}

impl From<Id> for FlexBoxId {
    fn from(id: Id) -> FlexBoxId {
        FlexBoxId::new(id)
    }
}

pub struct AppMain {
    lsp_tx: mpsc::Sender<usize>,
    lsp_rx: Option<mpsc::Receiver<usize>>,
    files: Arc<Vec<LoadedFile>>,
    root: Utf8PathBuf,
    warmup_ms: Arc<Mutex<Option<u128>>>,
}

impl AppMain {
    fn new_boxed(
        files: Arc<Vec<LoadedFile>>,
        root: Utf8PathBuf,
        warmup_ms: Arc<Mutex<Option<u128>>>,
    ) -> BoxedSafeApp<State, AppSignal> {
        let (lsp_tx, lsp_rx) = mpsc::channel(32);
        Box::new(Self {
            lsp_tx,
            lsp_rx: Some(lsp_rx),
            files,
            root,
            warmup_ms,
        })
    }
}

impl App for AppMain {
    type S = State;
    type AS = AppSignal;

    fn app_init(
        &mut self,
        component_registry_map: &mut ComponentRegistryMap<Self::S, Self::AS>,
        has_focus: &mut HasFocus,
    ) {
        let file_list_id = FlexBoxId::from(Id::FileList);
        if let ContainsResult::DoesNotContain =
            ComponentRegistry::contains(component_registry_map, file_list_id)
        {
            ComponentRegistry::put(
                component_registry_map,
                file_list_id,
                FileListComponent::new_boxed(file_list_id),
            );
        }

        let preview_id = FlexBoxId::from(Id::Preview);
        if let ContainsResult::DoesNotContain =
            ComponentRegistry::contains(component_registry_map, preview_id)
        {
            ComponentRegistry::put(
                component_registry_map,
                preview_id,
                FilePreviewComponent::new_boxed(preview_id),
            );
        }

        if has_focus.get_id().is_none() {
            has_focus.set_id(file_list_id);
        }
    }

    fn app_handle_input_event(
        &mut self,
        input_event: InputEvent,
        global_data: &mut GlobalData<State, AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<State, AppSignal>,
        has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        ComponentRegistry::route_event_to_focused_component(
            global_data,
            input_event,
            component_registry_map,
            has_focus,
        )
    }

    fn app_handle_signal(
        &mut self,
        action: &AppSignal,
        global_data: &mut GlobalData<State, AppSignal>,
        _component_registry_map: &mut ComponentRegistryMap<State, AppSignal>,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        throws_with_return!({
            let state = &mut global_data.state;
            let file_count = state.files.len();
            match action {
                AppSignal::SelectNext => {
                    if file_count > 0 {
                        state.selected = (state.selected + 1).min(file_count - 1);
                    }
                }
                AppSignal::SelectPrev => {
                    state.selected = state.selected.saturating_sub(1);
                }
                AppSignal::OpenSelected => {
                    if file_count > 0 {
                        state.open_file = Some(state.selected);
                        state.preview_scroll = 0;
                        let _ = self.lsp_tx.try_send(state.selected);
                    }
                }
                AppSignal::ScrollPreviewDown(n) => {
                    state.preview_scroll = state.preview_scroll.saturating_add(*n);
                }
                AppSignal::ScrollPreviewUp(n) => {
                    state.preview_scroll = state.preview_scroll.saturating_sub(*n);
                }
                AppSignal::Noop => {}
            }
            EventPropagation::ConsumedRender
        });
    }

    fn app_render(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<State, AppSignal>,
        has_focus: &mut HasFocus,
    ) -> CommonResult<RenderPipeline> {
        throws_with_return!({
            {
                if let Some(lsp_rx) = self.lsp_rx.take() {
                    let notify_tx = global_data.main_thread_channel_sender.clone();
                    let files = Arc::clone(&self.files);
                    let root = self.root.clone();
                    let warmup_ms = Arc::clone(&self.warmup_ms);
                    tokio::spawn(async move {
                        lsp::run(root, files, lsp_rx, notify_tx, warmup_ms).await;
                    });
                }
            }
            let window_size = global_data.window_size;
            let has_open_file = global_data.state.open_file.is_some();

            let mut surface = {
                let mut it = surface!(stylesheet: create_stylesheet()?);
                it.surface_start(SurfaceProps {
                    pos: col(0) + row(0),
                    size: {
                        let col_count = window_size.col_width;
                        let row_count = window_size.row_height - height(1);
                        col_count + row_count
                    },
                })?;

                ContainerRenderer { has_open_file }.render_in_surface(
                    &mut it,
                    global_data,
                    component_registry_map,
                    has_focus,
                )?;

                it.surface_end()?;
                it
            };

            render_status_bar(
                &mut surface.render_pipeline,
                window_size,
                *global_data.state.warmup_ms.lock().unwrap(),
            );

            surface.render_pipeline
        });
    }
}

struct ContainerRenderer {
    has_open_file: bool,
}

impl SurfaceRender<State, AppSignal> for ContainerRenderer {
    fn render_in_surface(
        &mut self,
        surface: &mut Surface,
        global_data: &mut GlobalData<State, AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<State, AppSignal>,
        has_focus: &mut HasFocus,
    ) -> CommonResult<()> {
        throws!({
            let container_id = FlexBoxId::from(Id::Container);
            box_start!(
                in: surface,
                id: container_id,
                dir: LayoutDirection::Horizontal,
                requested_size_percent: req_size_pc!(width: 100, height: 100),
                styles: [container_id],
            );

            let file_list_id = FlexBoxId::from(Id::FileList);
            let file_list_width = if self.has_open_file { 30 } else { 100 };
            {
                box_start!(
                    in: surface,
                    id: file_list_id,
                    dir: LayoutDirection::Vertical,
                    requested_size_percent: req_size_pc!(width: file_list_width, height: 100),
                    styles: [file_list_id],
                );
                render_component_in_current_box!(
                    in: surface,
                    component_id: file_list_id,
                    from: component_registry_map,
                    global_data: global_data,
                    has_focus: has_focus
                );
                box_end!(in: surface);
            }

            if self.has_open_file {
                let preview_id = FlexBoxId::from(Id::Preview);
                {
                    box_start!(
                        in: surface,
                        id: preview_id,
                        dir: LayoutDirection::Vertical,
                        requested_size_percent: req_size_pc!(width: 70, height: 100),
                        styles: [preview_id],
                    );
                    render_component_in_current_box!(
                        in: surface,
                        component_id: preview_id,
                        from: component_registry_map,
                        global_data: global_data,
                        has_focus: has_focus
                    );
                    box_end!(in: surface);
                }
            }

            box_end!(in: surface);
        });
    }
}

fn create_stylesheet() -> CommonResult<TuiStylesheet> {
    throws_with_return!({
        tui_stylesheet! {
            new_style!(
                id: {Id::Container}
            ),
            new_style!(
                id: {Id::FileList}
                padding: {1}
                color_bg: {tui_color!(20, 20, 30)}
            ),
            new_style!(
                id: {Id::Preview}
                padding: {1}
                color_bg: {tui_color!(15, 15, 25)}
            )
        }
    })
}

fn render_status_bar(pipeline: &mut RenderPipeline, size: Size, warmup_ms: Option<u128>) {
    let color_bg = tui_color!(30, 30, 50);
    let color_fg = tui_color!(180, 180, 220);
    let color_warm_fg = tui_color!(120, 220, 120);
    let color_pending_fg = tui_color!(220, 180, 80);

    let warmup_text = match warmup_ms {
        Some(ms) => format!("  warmed up in {ms}ms"),
        None => "  warming up…".to_string(),
    };
    let warmup_color = if warmup_ms.is_some() {
        color_warm_fg
    } else {
        color_pending_fg
    };

    let styled_texts = tui_styled_texts! {
        tui_styled_text! {
            @style: new_style!(bold color_fg: {color_fg} color_bg: {color_bg}),
            @text: " q:Quit  ↑↓:Navigate  Enter:Open  PgUp/PgDn:Scroll"
        },
        tui_styled_text! {
            @style: new_style!(color_fg: {warmup_color} color_bg: {color_bg}),
            @text: warmup_text
        }
    };

    let row_idx = size.row_height.convert_to_index();
    let mut render_ops = RenderOpIRVec::new();
    render_ops += RenderOpCommon::MoveCursorPositionAbs(col(0) + row_idx);
    render_ops += RenderOpCommon::ResetColor;
    render_ops += RenderOpCommon::SetBgColor(color_bg);
    render_ops += RenderOpIR::PaintTextWithAttributes(
        SPACER_GLYPH.repeat(size.col_width.as_usize()).into(),
        None,
    );
    render_ops += RenderOpCommon::MoveCursorPositionAbs(col(0) + row_idx);
    render_tui_styled_texts_into(&styled_texts, &mut render_ops);
    pipeline.push(ZOrder::Normal, render_ops);
}

pub fn build_state(
    files: Arc<Vec<LoadedFile>>,
    root: Utf8PathBuf,
    warmup_ms: Arc<std::sync::Mutex<Option<u128>>>,
) -> State {
    State::new(files, root, warmup_ms)
}

pub async fn run(
    initial_state: State,
    files: Arc<Vec<LoadedFile>>,
    root: Utf8PathBuf,
    warmup_ms: Arc<std::sync::Mutex<Option<u128>>>,
) -> CommonResult<()> {
    let app = AppMain::new_boxed(files, root, warmup_ms);
    let exit_keys = &[InputEvent::Keyboard(key_press! { @char 'q' })];
    let _unused: (GlobalData<_, _>, InputDevice, OutputDevice) =
        TerminalWindow::main_event_loop(app, exit_keys, initial_state)?.await?;
    ok!()
}
