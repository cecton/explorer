use super::file_name_picker::FileNamePickerComponent;
use super::preview::FilePreviewComponent;
use super::state::{AppSignal, State};
use crate::loader::LoadedFile;
use crate::lsp;
use camino::Utf8PathBuf;
use nucleo::Matcher;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Utf32Str};
use r3bl_tui::{
    App, BoxedSafeApp, CommonResult, ComponentRegistry, ComponentRegistryMap, ContainsResult,
    EventPropagation, FlexBoxId, GlobalData, HasFocus, InputDevice, InputEvent, Key, KeyPress,
    LayoutDirection, LayoutManagement, LengthOps, ModifierKeysMask, OutputDevice,
    PerformPositioningAndSizing, RenderOpCommon, RenderOpIR, RenderOpIRVec, RenderPipeline,
    SPACER_GLYPH, Size, Surface, SurfaceProps, SurfaceRender, TerminalWindow,
    TerminalWindowMainThreadSignal, TuiStylesheet, ZOrder, box_end, box_start, col, height,
    key_press, new_style, ok, render_component_in_current_box, render_tui_styled_texts_into,
    req_size_pc, row, send_signal, surface, throws, throws_with_return, tui_color, tui_styled_text,
    tui_styled_texts, tui_stylesheet,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;

type PickerResultMsg = (u64, Vec<(usize, Vec<u32>)>);

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Id {
    Container = 1,
    FileNamePicker = 2,
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
    picker_results_tx: mpsc::Sender<PickerResultMsg>,
    picker_results_rx: mpsc::Receiver<PickerResultMsg>,
    picker_generation: Arc<AtomicU64>,
}

impl AppMain {
    fn new_boxed(files: Arc<Vec<LoadedFile>>, root: Utf8PathBuf) -> BoxedSafeApp<State, AppSignal> {
        let (lsp_tx, lsp_rx) = mpsc::channel(32);
        let (picker_results_tx, picker_results_rx) = mpsc::channel(32);
        Box::new(Self {
            lsp_tx,
            lsp_rx: Some(lsp_rx),
            files,
            root,
            picker_results_tx,
            picker_results_rx,
            picker_generation: Arc::new(AtomicU64::new(0)),
        })
    }

    fn trigger_match(&self, query: String) {
        let generation = self.picker_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let files = Arc::clone(&self.files);
        let root = self.root.clone();
        let tx = self.picker_results_tx.clone();
        let gen_counter = Arc::clone(&self.picker_generation);
        tokio::task::spawn_blocking(move || {
            let results = run_file_name_match(&query, &files, &root);
            if gen_counter.load(Ordering::Relaxed) == generation {
                let _ = tx.try_send((generation, results));
            }
        });
    }
}

fn run_file_name_match(
    query: &str,
    files: &[LoadedFile],
    root: &Utf8PathBuf,
) -> Vec<(usize, Vec<u32>)> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);

    if pattern.atoms.is_empty() {
        return (0..files.len()).map(|i| (i, vec![])).collect();
    }

    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut buf = Vec::new();
    let mut scored: Vec<(usize, u32, Vec<u32>)> = files
        .iter()
        .enumerate()
        .filter_map(|(i, file)| {
            let rel = file.path.strip_prefix(root).unwrap_or(&file.path);
            let haystack = Utf32Str::new(rel.as_str(), &mut buf);
            let mut indices = Vec::new();
            pattern
                .indices(haystack, &mut matcher, &mut indices)
                .map(|score| {
                    indices.sort_unstable();
                    indices.dedup();
                    (i, score, indices)
                })
        })
        .collect();
    scored.sort_by_key(|&(_, score, _)| std::cmp::Reverse(score));
    scored.into_iter().map(|(i, _, idx)| (i, idx)).collect()
}

impl App for AppMain {
    type S = State;
    type AS = AppSignal;

    fn app_init(
        &mut self,
        component_registry_map: &mut ComponentRegistryMap<Self::S, Self::AS>,
        has_focus: &mut HasFocus,
    ) {
        let picker_id = FlexBoxId::from(Id::FileNamePicker);
        if let ContainsResult::DoesNotContain =
            ComponentRegistry::contains(component_registry_map, picker_id)
        {
            ComponentRegistry::put(
                component_registry_map,
                picker_id,
                FileNamePickerComponent::new_boxed(picker_id),
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
            has_focus.set_id(preview_id);
        }
    }

    fn app_handle_input_event(
        &mut self,
        input_event: InputEvent,
        global_data: &mut GlobalData<State, AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<State, AppSignal>,
        has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        if let InputEvent::Keyboard(KeyPress::WithModifiers { key, mask }) = input_event
            && key == Key::Character('p')
            && mask == ModifierKeysMask::new().with_ctrl()
        {
            send_signal!(
                global_data.main_thread_channel_sender,
                TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::OpenFileNamePicker,)
            );
            return Ok(EventPropagation::ConsumedRender);
        }

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
        has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        throws_with_return!({
            let state = &mut global_data.state;
            match action {
                AppSignal::OpenFileNamePicker => {
                    state.file_name_picker_open = true;
                    state.file_name_picker_query.clear();
                    state.file_name_picker_selected = 0;
                    state.file_name_picker_results =
                        (0..state.files.len()).map(|i| (i, vec![])).collect();
                    has_focus.set_id(FlexBoxId::from(Id::FileNamePicker));
                }
                AppSignal::CloseFileNamePicker => {
                    state.file_name_picker_open = false;
                    state.file_name_picker_query.clear();
                    state.file_name_picker_results.clear();
                    state.file_name_picker_selected = 0;
                    has_focus.set_id(FlexBoxId::from(Id::Preview));
                }
                AppSignal::FileNamePickerChar(c) => {
                    state.file_name_picker_query.push(*c);
                    state.file_name_picker_selected = 0;
                }
                AppSignal::FileNamePickerBackspace => {
                    state.file_name_picker_query.pop();
                    state.file_name_picker_selected = 0;
                    if state.file_name_picker_query.is_empty() {
                        state.file_name_picker_results =
                            (0..state.files.len()).map(|i| (i, vec![])).collect();
                    }
                }
                AppSignal::FileNamePickerSelectNext => {
                    let count = state.file_name_picker_results.len();
                    if count > 0 {
                        state.file_name_picker_selected =
                            (state.file_name_picker_selected + 1).min(count - 1);
                    }
                }
                AppSignal::FileNamePickerSelectPrev => {
                    state.file_name_picker_selected =
                        state.file_name_picker_selected.saturating_sub(1);
                }
                AppSignal::FileNamePickerConfirm => {
                    if let Some(&(file_idx, _)) = state
                        .file_name_picker_results
                        .get(state.file_name_picker_selected)
                    {
                        state.open_file = Some(file_idx);
                        state.preview_scroll = 0;
                        let _ = self.lsp_tx.try_send(file_idx);
                    }
                    state.file_name_picker_open = false;
                    state.file_name_picker_query.clear();
                    state.file_name_picker_results.clear();
                    state.file_name_picker_selected = 0;
                    has_focus.set_id(FlexBoxId::from(Id::Preview));
                }
                AppSignal::ScrollPreviewDown(n) => {
                    state.preview_scroll = state.preview_scroll.saturating_add(*n);
                }
                AppSignal::ScrollPreviewUp(n) => {
                    state.preview_scroll = state.preview_scroll.saturating_sub(*n);
                }
                AppSignal::Noop => {}
            }

            // Trigger async match for non-empty queries only.
            match action {
                AppSignal::FileNamePickerChar(_) => {
                    let query = global_data.state.file_name_picker_query.clone();
                    self.trigger_match(query);
                }
                AppSignal::FileNamePickerBackspace
                    if !global_data.state.file_name_picker_query.is_empty() =>
                {
                    let query = global_data.state.file_name_picker_query.clone();
                    self.trigger_match(query);
                }
                _ => {}
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
        // Start the LSP task on the first render.
        if let Some(lsp_rx) = self.lsp_rx.take() {
            let notify_tx = global_data.main_thread_channel_sender.clone();
            let files = Arc::clone(&self.files);
            let root = self.root.clone();
            tokio::spawn(async move {
                lsp::run(root, files, lsp_rx, notify_tx).await;
            });
        }

        // Drain async picker results; apply the latest matching generation.
        let current_generation = self.picker_generation.load(Ordering::Relaxed);
        while let Ok((arrived_generation, results)) = self.picker_results_rx.try_recv() {
            if arrived_generation == current_generation {
                global_data.state.file_name_picker_results = results;
            }
        }

        throws_with_return!({
            let window_size = global_data.window_size;
            let picker_open = global_data.state.file_name_picker_open;

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

                ContainerRenderer { picker_open }.render_in_surface(
                    &mut it,
                    global_data,
                    component_registry_map,
                    has_focus,
                )?;

                it.surface_end()?;
                it
            };

            render_status_bar(&mut surface.render_pipeline, window_size, picker_open);

            surface.render_pipeline
        });
    }
}

struct ContainerRenderer {
    picker_open: bool,
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

            if self.picker_open {
                let picker_id = FlexBoxId::from(Id::FileNamePicker);
                box_start!(
                    in: surface,
                    id: picker_id,
                    dir: LayoutDirection::Vertical,
                    requested_size_percent: req_size_pc!(width: 100, height: 100),
                    styles: [picker_id],
                );
                render_component_in_current_box!(
                    in: surface,
                    component_id: picker_id,
                    from: component_registry_map,
                    global_data: global_data,
                    has_focus: has_focus
                );
                box_end!(in: surface);
            } else {
                let preview_id = FlexBoxId::from(Id::Preview);
                box_start!(
                    in: surface,
                    id: preview_id,
                    dir: LayoutDirection::Vertical,
                    requested_size_percent: req_size_pc!(width: 100, height: 100),
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
                id: {Id::FileNamePicker}
                padding: {1}
                color_bg: {tui_color!(18, 18, 28)}
            ),
            new_style!(
                id: {Id::Preview}
                padding: {1}
                color_bg: {tui_color!(15, 15, 25)}
            )
        }
    })
}

fn render_status_bar(pipeline: &mut RenderPipeline, size: Size, picker_open: bool) {
    let color_bg = tui_color!(30, 30, 50);
    let color_fg = tui_color!(180, 180, 220);

    let hint = if picker_open {
        " Esc:Close  ↑↓:Select  Enter:Open"
    } else {
        " Ctrl+P:Open file  PgUp/PgDn:Scroll  q/Ctrl+C:Quit"
    };

    let styled_texts = tui_styled_texts! {
        tui_styled_text! {
            @style: new_style!(bold color_fg: {color_fg} color_bg: {color_bg}),
            @text: hint
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

pub fn build_state(files: Arc<Vec<LoadedFile>>, root: Utf8PathBuf) -> State {
    State::new(files, root)
}

pub async fn run(
    initial_state: State,
    files: Arc<Vec<LoadedFile>>,
    root: Utf8PathBuf,
) -> CommonResult<()> {
    let app = AppMain::new_boxed(files, root);
    let exit_keys = &[
        InputEvent::Keyboard(key_press! { @char 'q' }),
        InputEvent::Keyboard(KeyPress::WithModifiers {
            key: Key::Character('c'),
            mask: ModifierKeysMask::new().with_ctrl(),
        }),
    ];
    let _unused: (GlobalData<_, _>, InputDevice, OutputDevice) =
        TerminalWindow::main_event_loop(app, exit_keys, initial_state)?.await?;
    ok!()
}
