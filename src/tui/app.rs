use super::file_name_picker::FileNamePickerComponent;
use super::preview::FilePreviewComponent;
use super::state::{AppSignal, State};
use crate::loader::LoadedFile;
use crate::lsp;
use crate::supervisor::{Supervisor, TaskStatus};
use crate::watcher::start_watcher;
use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use nucleo::Matcher;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Utf32Str};
use r3bl_tui::{
    App, BoxedSafeApp, CommonResult, ComponentRegistry, ComponentRegistryMap, ContainsResult,
    EditorBuffer, EventPropagation, FlexBoxId, GlobalData, HasFocus, InputDevice, InputEvent, Key,
    KeyPress, LayoutDirection, LayoutManagement, LengthOps, ModifierKeysMask, OutputDevice,
    PerformPositioningAndSizing, RenderOpCommon, RenderOpIR, RenderOpIRVec, RenderPipeline,
    SPACER_GLYPH, Size, SpecialKey, Surface, SurfaceProps, SurfaceRender, TerminalWindow,
    TerminalWindowMainThreadSignal, TuiStylesheet, ZOrder, box_end, box_start, col, height,
    new_style, ok, render_component_in_current_box, render_tui_styled_texts_into, req_size_pc, row,
    send_signal, surface, throws, throws_with_return, tui_color, tui_styled_text, tui_styled_texts,
    tui_stylesheet,
};
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

type PickerResultMsg = (u64, Vec<(usize, Vec<u32>)>);

/// Shared slot holding the sender side of the active LSP request channel.
///
/// The supervisor spawn closure creates a fresh `(tx, rx)` pair on every
/// (re)spawn and stores `tx` here, so `AppMain` always sends to the live task.
type LspTxSlot = Arc<Mutex<mpsc::Sender<usize>>>;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Id {
    Container = 1,
    FileNamePicker = 2,
    Preview = 3,
    FileNamePickerEditor = 4,
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
    lsp_tx: LspTxSlot,
    files: Arc<ArcSwap<Vec<LoadedFile>>>,
    root: Utf8PathBuf,
    picker_results_tx: mpsc::Sender<PickerResultMsg>,
    picker_results_rx: mpsc::Receiver<PickerResultMsg>,
    picker_generation: Arc<AtomicU64>,
}

impl AppMain {
    fn new_boxed(
        files: Arc<ArcSwap<Vec<LoadedFile>>>,
        root: Utf8PathBuf,
    ) -> BoxedSafeApp<State, AppSignal> {
        let (lsp_tx, _) = mpsc::channel(32);
        let (picker_results_tx, picker_results_rx) = mpsc::channel(32);
        Box::new(Self {
            lsp_tx: Arc::new(Mutex::new(lsp_tx)),
            files,
            root,
            picker_results_tx,
            picker_results_rx,
            picker_generation: Arc::new(AtomicU64::new(0)),
        })
    }

    fn trigger_match(
        &self,
        query: String,
        main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
    ) {
        let generation = self.picker_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let snapshot = self.files.load_full();
        let root = self.root.clone();
        let tx = self.picker_results_tx.clone();
        let gen_counter = Arc::clone(&self.picker_generation);
        tokio::task::spawn_blocking(move || {
            let results = run_file_name_match(&query, &snapshot, &root);
            if gen_counter.load(Ordering::Relaxed) == generation {
                let _ = tx.try_send((generation, results));
                send_signal!(
                    main_tx,
                    TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::Noop)
                );
            }
        });
    }

    fn all_files_results(files: &[LoadedFile]) -> Vec<(usize, Vec<u32>)> {
        files
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.removed.load(Ordering::Relaxed))
            .map(|(i, _)| (i, vec![]))
            .collect()
    }
}

fn run_file_name_match(
    query: &str,
    files: &[LoadedFile],
    root: &Utf8PathBuf,
) -> Vec<(usize, Vec<u32>)> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);

    if pattern.atoms.is_empty() {
        return AppMain::all_files_results(files);
    }

    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut buf = Vec::new();
    let mut scored: Vec<(usize, u32, Vec<u32>)> = files
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.removed.load(Ordering::Relaxed))
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
            has_focus.set_id(FlexBoxId::from(Id::FileNamePicker));
        }
    }

    fn app_start(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
        _component_registry_map: &mut ComponentRegistryMap<State, AppSignal>,
        _has_focus: &mut HasFocus,
    ) {
        let notify_tx = global_data.main_thread_channel_sender.clone();
        let files = Arc::clone(&self.files);
        let root = self.root.clone();

        // The spawn closure creates a fresh channel pair on every (re)spawn:
        // stores tx in the shared slot so AppMain always sends to the live task.
        let lsp_tx_slot = Arc::clone(&self.lsp_tx);
        let lsp_notify = notify_tx.clone();
        let lsp_files = Arc::clone(&files);
        let lsp_root = root.clone();
        let mut supervisor = Supervisor::new();
        supervisor.add(
            "lsp",
            Box::new(move || {
                let (tx, rx) = mpsc::channel(32);
                *lsp_tx_slot.lock().unwrap() = tx;
                let notify = lsp_notify.clone();
                let f = Arc::clone(&lsp_files);
                let r = lsp_root.clone();
                Box::pin(lsp::run(r, f, rx, notify))
            }),
        );

        supervisor.start(notify_tx.clone(), |name, status| {
            let signal = match status {
                TaskStatus::Restarting => AppSignal::TaskRestarting(name),
                TaskStatus::Running => AppSignal::TaskRunning(name),
            };
            TerminalWindowMainThreadSignal::ApplyAppSignal(signal)
        });

        if let Err(e) = start_watcher(&root, notify_tx) {
            tracing::warn!("watcher failed to start: {e}");
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

        if global_data.state.file_name_picker_open
            && let InputEvent::Keyboard(KeyPress::Plain { key }) = input_event
        {
            match key {
                Key::SpecialKey(SpecialKey::Esc) => {
                    send_signal!(
                        global_data.main_thread_channel_sender,
                        TerminalWindowMainThreadSignal::ApplyAppSignal(
                            AppSignal::CloseFileNamePicker
                        )
                    );
                    return Ok(EventPropagation::ConsumedRender);
                }
                Key::SpecialKey(SpecialKey::Enter) => {
                    send_signal!(
                        global_data.main_thread_channel_sender,
                        TerminalWindowMainThreadSignal::ApplyAppSignal(
                            AppSignal::FileNamePickerConfirm
                        )
                    );
                    return Ok(EventPropagation::ConsumedRender);
                }
                Key::SpecialKey(SpecialKey::Up) => {
                    send_signal!(
                        global_data.main_thread_channel_sender,
                        TerminalWindowMainThreadSignal::ApplyAppSignal(
                            AppSignal::FileNamePickerSelectPrev
                        )
                    );
                    return Ok(EventPropagation::ConsumedRender);
                }
                Key::SpecialKey(SpecialKey::Down) => {
                    send_signal!(
                        global_data.main_thread_channel_sender,
                        TerminalWindowMainThreadSignal::ApplyAppSignal(
                            AppSignal::FileNamePickerSelectNext
                        )
                    );
                    return Ok(EventPropagation::ConsumedRender);
                }
                _ => {}
            }
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
                    state.file_name_picker_selected = 0;
                    let snapshot = state.files.load();
                    state.file_name_picker_results = AppMain::all_files_results(&snapshot);
                    let editor_id = FlexBoxId::from(Id::FileNamePickerEditor);
                    if let Some(buf) = state.editor_buffers.get_mut(&editor_id) {
                        buf.init_with([""])
                    } else {
                        state
                            .editor_buffers
                            .insert(editor_id, EditorBuffer::new_empty(None, None));
                    }
                    has_focus.set_id(FlexBoxId::from(Id::FileNamePicker));
                }
                AppSignal::CloseFileNamePicker => {
                    state.file_name_picker_open = false;
                    state.file_name_picker_results.clear();
                    state.file_name_picker_selected = 0;
                    has_focus.set_id(FlexBoxId::from(Id::Preview));
                }
                AppSignal::FileNamePickerQueryChanged => {
                    state.file_name_picker_selected = 0;
                    let editor_id = FlexBoxId::from(Id::FileNamePickerEditor);
                    let query = state
                        .editor_buffers
                        .get(&editor_id)
                        .map(|b| b.get_as_string_with_newlines().to_string())
                        .unwrap_or_default();
                    if query.is_empty() {
                        let snapshot = state.files.load();
                        state.file_name_picker_results = AppMain::all_files_results(&snapshot);
                    } else {
                        self.trigger_match(query, global_data.main_thread_channel_sender.clone());
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
                        let _ = self.lsp_tx.lock().unwrap().try_send(file_idx);
                    }
                    state.file_name_picker_open = false;
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
                AppSignal::FilesChanged(batch) => {
                    let snapshot = self.files.load_full();

                    for path in &batch.removed {
                        if let Some(file) = snapshot.iter().find(|f| &f.path == path) {
                            file.removed.store(true, Ordering::Relaxed);
                        }
                    }

                    for path in &batch.modified {
                        if let Some(file) = snapshot
                            .iter()
                            .find(|f| &f.path == path && !f.removed.load(Ordering::Relaxed))
                        {
                            file.reload();
                        }
                    }

                    let mut new_files: Vec<LoadedFile> = vec![];
                    for path in &batch.created {
                        if let Some(file) = snapshot
                            .iter()
                            .find(|f| &f.path == path && f.removed.load(Ordering::Relaxed))
                        {
                            file.removed.store(false, Ordering::Relaxed);
                            file.reload();
                        } else if !snapshot.iter().any(|f| &f.path == path)
                            && let Some(loaded) = LoadedFile::load(path.clone().into_std_path_buf())
                        {
                            new_files.push(loaded);
                        }
                    }

                    if !new_files.is_empty() {
                        let mut next: Vec<LoadedFile> = snapshot
                            .iter()
                            .map(|f| LoadedFile {
                                path: f.path.clone(),
                                data: std::sync::Mutex::new({
                                    let d = f.data.lock().unwrap();
                                    crate::loader::FileData {
                                        content: d.content.clone(),
                                        line_starts: d.line_starts.clone(),
                                    }
                                }),
                                colored_lines: std::sync::Mutex::new(
                                    f.colored_lines.lock().unwrap().clone(),
                                ),
                                removed: std::sync::atomic::AtomicBool::new(
                                    f.removed.load(Ordering::Relaxed),
                                ),
                            })
                            .collect();
                        next.extend(new_files);
                        next.sort_by(|a, b| a.path.cmp(&b.path));
                        self.files.store(Arc::new(next));
                    }

                    let snapshot = self.files.load();
                    if state.file_name_picker_open {
                        let editor_id = FlexBoxId::from(Id::FileNamePickerEditor);
                        let query = state
                            .editor_buffers
                            .get(&editor_id)
                            .map(|b| b.get_as_string_with_newlines().to_string())
                            .unwrap_or_default();
                        if query.is_empty() {
                            state.file_name_picker_results = AppMain::all_files_results(&snapshot);
                        } else {
                            self.trigger_match(
                                query,
                                global_data.main_thread_channel_sender.clone(),
                            );
                        }
                    }
                    state.bump_files_version();
                }
                AppSignal::TaskRestarting(name) => {
                    state.set_task_status(name, TaskStatus::Restarting);
                }
                AppSignal::TaskRunning(name) => {
                    state.set_task_status(name, TaskStatus::Running);
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
        let current_generation = self.picker_generation.load(Ordering::Relaxed);
        while let Ok((arrived_generation, results)) = self.picker_results_rx.try_recv() {
            if arrived_generation == current_generation {
                global_data.state.file_name_picker_results = results;
            }
        }

        throws_with_return!({
            let window_size = global_data.window_size;
            let picker_open = global_data.state.file_name_picker_open;

            let surface = {
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

            let mut pipeline = surface.render_pipeline;

            render_status_bar(&mut pipeline, window_size, picker_open, &global_data.state);

            pipeline
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

fn render_status_bar(pipeline: &mut RenderPipeline, size: Size, picker_open: bool, state: &State) {
    let color_bg = tui_color!(30, 30, 50);
    let color_fg = tui_color!(180, 180, 220);
    let color_warn = tui_color!(220, 160, 60);

    let hint = if picker_open {
        " Esc:Close  ↑↓:Select  Enter:Open"
    } else {
        " Ctrl+P:Open file  ↑↓/PgUp/PgDn:Scroll  Ctrl+C:Quit"
    };

    let task_note = state.task_status_line();

    let (text, fg) = if task_note.is_empty() {
        (hint.to_string(), color_fg)
    } else {
        (format!("{hint}  [{task_note}]"), color_warn)
    };

    let styled_texts = tui_styled_texts! {
        tui_styled_text! {
            @style: new_style!(bold color_fg: {fg} color_bg: {color_bg}),
            @text: text
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

pub fn build_state(files: Arc<ArcSwap<Vec<LoadedFile>>>, root: Utf8PathBuf) -> State {
    State::new(files, root)
}

pub async fn run(
    initial_state: State,
    files: Arc<ArcSwap<Vec<LoadedFile>>>,
    root: Utf8PathBuf,
) -> CommonResult<()> {
    let app = AppMain::new_boxed(files, root);
    let exit_keys = &[InputEvent::Keyboard(KeyPress::WithModifiers {
        key: Key::Character('c'),
        mask: ModifierKeysMask::new().with_ctrl(),
    })];
    let _unused: (GlobalData<_, _>, InputDevice, OutputDevice) =
        TerminalWindow::main_event_loop(app, exit_keys, initial_state)?.await?;
    ok!()
}
