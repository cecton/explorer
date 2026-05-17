use super::file_name_picker::FileNamePickerComponent;
use super::preview::FilePreviewComponent;
use super::state::{AppSignal, State, Window};
use crate::loader::{FileKey, LoadedFile};
use crate::lsp;
use crate::supervisor::{Supervisor, TaskStatus};
use crate::watcher::{WATCHER_RRT, set_watcher_root};
use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use nucleo::Matcher;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Utf32Str};
use r3bl_tui::{
    App, BoxedSafeApp, BoxedSafeComponent, CommonResult, Component, ComponentRegistry,
    ComponentRegistryMap, ContainsResult, EditorBuffer, EventPropagation, FlexBox, FlexBoxId,
    GlobalData, HasFocus, InputDevice, InputEvent, IntoErr, Key, KeyPress, LayoutDirection,
    LayoutManagement, LengthOps, ModifierKeysMask, OutputDevice, PerformPositioningAndSizing,
    RenderOpCommon, RenderOpIR, RenderOpIRVec, RenderPipeline, SPACER_GLYPH, Size, SpecialKey,
    Surface, SurfaceBounds, SurfaceProps, SurfaceRender, TerminalWindow,
    TerminalWindowMainThreadSignal, TuiAvailability, TuiStylesheet, ZOrder, box_end, box_start,
    col, height, new_style, ok, render_component_in_current_box, render_tui_styled_texts_into,
    req_size_pc, row, send_signal, surface, throws, throws_with_return, tui_color, tui_styled_text,
    tui_styled_texts, tui_stylesheet,
};
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::mpsc;

type PickerResultMsg = (u64, Vec<(FileKey, Vec<u32>)>);

/// Shared slot holding the sender side of the active LSP request channel.
///
/// The supervisor spawn closure creates a fresh `(tx, rx)` pair on every
/// (re)spawn and stores `tx` here, so `AppMain` always sends to the live task.
type LspTxSlot = Arc<Mutex<mpsc::Sender<usize>>>;

/// Maximum number of simultaneously visible panes. Terminals wider than 500 cols are not
/// expected in practice (500 / MIN_PANE_WIDTH = 5).
const MAX_PANES: usize = 5;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Id {
    Container = 1,
    /// Pane slots 0-4 (positional, not tied to a specific window).
    Pane0 = 2,
    Pane1 = 3,
    Pane2 = 4,
    Pane3 = 5,
    Pane4 = 6,
    FileNamePickerEditor = 7,
}

impl Id {
    pub fn pane(slot: usize) -> Self {
        match slot {
            0 => Id::Pane0,
            1 => Id::Pane1,
            2 => Id::Pane2,
            3 => Id::Pane3,
            _ => Id::Pane4,
        }
    }
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

/// Dispatcher component for a single pane slot. Holds both inner component types and
/// delegates to the correct one based on which `Window` is currently assigned to this slot
/// in `state.window_stack`.
struct PaneComponent {
    id: FlexBoxId,
    slot: usize,
    picker: FileNamePickerComponent,
    preview: FilePreviewComponent,
}

impl PaneComponent {
    fn new_boxed(slot: usize, id: FlexBoxId) -> BoxedSafeComponent<State, AppSignal> {
        Box::new(Self {
            id,
            slot,
            picker: FileNamePickerComponent::new(id),
            preview: FilePreviewComponent::new(id),
        })
    }

    fn active_window<'s>(&self, state: &'s State) -> Option<&'s Window> {
        state.window_stack.get(self.slot)
    }
}

impl Component<State, AppSignal> for PaneComponent {
    fn reset(&mut self) {
        self.picker.reset();
        self.preview.reset();
    }

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
        input_event: InputEvent,
        has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        match self.active_window(&global_data.state).cloned() {
            Some(Window::FileNamePicker) => {
                self.picker
                    .handle_event(global_data, input_event, has_focus)
            }
            Some(Window::FilePreview(_)) => {
                self.preview
                    .handle_event(global_data, input_event, has_focus)
            }
            None => Ok(EventPropagation::Propagate),
        }
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
        current_box: FlexBox,
        surface_bounds: SurfaceBounds,
        has_focus: &mut HasFocus,
    ) -> CommonResult<RenderPipeline> {
        match self.active_window(&global_data.state).cloned() {
            Some(Window::FileNamePicker) => {
                self.picker
                    .render(global_data, current_box, surface_bounds, has_focus)
            }
            Some(Window::FilePreview(_)) => {
                self.preview
                    .render(global_data, current_box, surface_bounds, has_focus)
            }
            None => Ok(r3bl_tui::render_pipeline!()),
        }
    }
}

pub struct AppMain {
    lsp_tx: LspTxSlot,
    files: Arc<ArcSwap<Vec<LoadedFile>>>,
    root: Utf8PathBuf,
    picker_results_tx: mpsc::Sender<PickerResultMsg>,
    picker_results_rx: mpsc::Receiver<PickerResultMsg>,
    picker_generation: Arc<AtomicU64>,
    exit_tx: Arc<OnceLock<mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>>>,
}

impl AppMain {
    fn new_boxed(
        files: Arc<ArcSwap<Vec<LoadedFile>>>,
        root: Utf8PathBuf,
        exit_tx: Arc<OnceLock<mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>>>,
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
            exit_tx,
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

    fn all_files_results(files: &[LoadedFile]) -> Vec<(FileKey, Vec<u32>)> {
        files
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.removed.load(Ordering::Relaxed))
            .map(|(i, _)| (FileKey(i), vec![]))
            .collect()
    }
}

fn run_file_name_match(
    query: &str,
    files: &[LoadedFile],
    root: &Utf8PathBuf,
) -> Vec<(FileKey, Vec<u32>)> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);

    if pattern.atoms.is_empty() {
        return AppMain::all_files_results(files);
    }

    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut buf = Vec::new();
    let mut scored: Vec<(FileKey, u32, Vec<u32>)> = files
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
                    (FileKey(i), score, indices)
                })
        })
        .collect();
    scored.sort_by_key(|&(_, score, _)| std::cmp::Reverse(score));
    scored.into_iter().map(|(key, _, idx)| (key, idx)).collect()
}

pub(super) fn resolve_selected(
    selected: &Option<FileKey>,
    results: &[(FileKey, Vec<u32>)],
) -> usize {
    let key = match selected {
        None => return 0,
        Some(k) => k,
    };
    results
        .iter()
        .position(|(result_key, _)| result_key == key)
        .unwrap_or(0)
}

impl App for AppMain {
    type S = State;
    type AS = AppSignal;

    fn app_init(
        &mut self,
        component_registry_map: &mut ComponentRegistryMap<Self::S, Self::AS>,
        has_focus: &mut HasFocus,
    ) {
        for slot in 0..MAX_PANES {
            let pane_id = FlexBoxId::from(Id::pane(slot));
            if let ContainsResult::DoesNotContain =
                ComponentRegistry::contains(component_registry_map, pane_id)
            {
                ComponentRegistry::put(
                    component_registry_map,
                    pane_id,
                    PaneComponent::new_boxed(slot, pane_id),
                );
            }
        }

        if has_focus.get_id().is_none() {
            has_focus.set_id(FlexBoxId::from(Id::Pane0));
        }
    }

    fn app_start(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
        _component_registry_map: &mut ComponentRegistryMap<State, AppSignal>,
        _has_focus: &mut HasFocus,
    ) {
        let notify_tx = global_data.main_thread_channel_sender.clone();

        // Publish the channel sender so the SIGTERM handler can request a clean exit.
        let _ = self.exit_tx.set(notify_tx.clone());
        let files = Arc::clone(&self.files);
        let root = self.root.clone();

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

        set_watcher_root(&root);
        match WATCHER_RRT.try_subscribe() {
            Ok(guard) => {
                let watcher_notify = notify_tx.clone();
                tokio::spawn(async move {
                    let mut rx = guard.receiver;
                    loop {
                        match rx.recv().await {
                            Ok(r3bl_tui::RRTEvent::Worker(signal)) => {
                                let _ = watcher_notify
                                    .send(TerminalWindowMainThreadSignal::ApplyAppSignal(signal))
                                    .await;
                            }
                            Ok(r3bl_tui::RRTEvent::Shutdown(_)) | Err(_) => break,
                        }
                    }
                });
            }
            Err(e) => {
                tracing::warn!("watcher failed to start: {e}");
            }
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
            && key == Key::Character('c')
            && mask == ModifierKeysMask::new().with_ctrl()
        {
            return Ok(EventPropagation::ExitMainEventLoop);
        }

        if let InputEvent::Keyboard(KeyPress::WithModifiers { key, mask }) = input_event
            && key == Key::Character('p')
            && mask == ModifierKeysMask::new().with_ctrl()
        {
            send_signal!(
                global_data.main_thread_channel_sender,
                TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::OpenFileNamePicker)
            );
            return Ok(EventPropagation::ConsumedRender);
        }

        if let InputEvent::Keyboard(KeyPress::Plain { key }) = input_event {
            match key {
                Key::SpecialKey(SpecialKey::Tab) => {
                    send_signal!(
                        global_data.main_thread_channel_sender,
                        TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::FocusNextPane)
                    );
                    return Ok(EventPropagation::ConsumedRender);
                }
                Key::SpecialKey(SpecialKey::BackTab) => {
                    send_signal!(
                        global_data.main_thread_channel_sender,
                        TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::FocusPrevPane)
                    );
                    return Ok(EventPropagation::ConsumedRender);
                }
                _ => {}
            }
        }

        if global_data.state.file_name_picker_open
            && global_data.state.focused_window == Some(Window::FileNamePicker)
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

        if matches!(
            global_data.state.focused_window,
            Some(Window::FilePreview(_))
        ) && let InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(SpecialKey::Esc),
        }) = input_event
        {
            send_signal!(
                global_data.main_thread_channel_sender,
                TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::SendFocusedWindowToBack)
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
                    state.push_window(Window::FileNamePicker);
                    state.focused_window = Some(Window::FileNamePicker);
                    state.file_name_picker_open = true;
                    state.file_name_picker_selected = None;
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
                    has_focus.set_id(focused_pane_id(state));
                }
                AppSignal::CloseFileNamePicker => {
                    state.remove_window(&Window::FileNamePicker);
                    state.file_name_picker_open = false;
                    state.file_name_picker_results.clear();
                    state.file_name_picker_selected = None;
                    has_focus.set_id(focused_pane_id(state));
                }
                AppSignal::FileNamePickerQueryChanged => {
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
                        let current = resolve_selected(
                            &state.file_name_picker_selected,
                            &state.file_name_picker_results,
                        );
                        let next = (current + 1).min(count - 1);
                        let (key, _) = &state.file_name_picker_results[next];
                        state.file_name_picker_selected = Some(*key);
                    }
                }
                AppSignal::FileNamePickerSelectPrev => {
                    let current = resolve_selected(
                        &state.file_name_picker_selected,
                        &state.file_name_picker_results,
                    );
                    let prev = current.saturating_sub(1);
                    if let Some((key, _)) = state.file_name_picker_results.get(prev) {
                        state.file_name_picker_selected = Some(*key);
                    }
                }
                AppSignal::FileNamePickerConfirm => {
                    let selected = resolve_selected(
                        &state.file_name_picker_selected,
                        &state.file_name_picker_results,
                    );
                    if let Some(&(key, _)) = state.file_name_picker_results.get(selected) {
                        if !state.window_states.contains_key(&Window::FilePreview(key)) {
                            state.set_window_scroll(&Window::FilePreview(key), 0);
                        }
                        state.push_window(Window::FilePreview(key));
                        state.focused_window = Some(Window::FilePreview(key));
                        let _ = self.lsp_tx.lock().unwrap().try_send(key.0);
                    }
                    state.remove_window(&Window::FileNamePicker);
                    state.file_name_picker_open = false;
                    state.file_name_picker_results.clear();
                    state.file_name_picker_selected = None;
                    has_focus.set_id(focused_pane_id(state));
                }
                AppSignal::ScrollPreviewDown(n) => {
                    if let Some(window) = state.focused_window.clone() {
                        let current = state.window_scroll(&window);
                        state.set_window_scroll(&window, current.saturating_add(*n));
                    }
                }
                AppSignal::ScrollPreviewUp(n) => {
                    if let Some(window) = state.focused_window.clone() {
                        let current = state.window_scroll(&window);
                        state.set_window_scroll(&window, current.saturating_sub(*n));
                    }
                }
                AppSignal::SendFocusedWindowToBack => {
                    if let Some(window) = state.focused_window.clone() {
                        state.send_to_back(&window);
                        has_focus.set_id(focused_pane_id(state));
                    }
                }
                AppSignal::FocusNextPane => {
                    let visible = state.visible_windows(
                        // Use a generous default; actual width is corrected at render time.
                        u16::MAX,
                    );
                    cycle_focus(state, has_focus, &visible, 1);
                }
                AppSignal::FocusPrevPane => {
                    let visible = state.visible_windows(u16::MAX);
                    cycle_focus(state, has_focus, &visible, -1);
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
            let surface_cols = window_size.col_width.as_u16();

            let visible = global_data.state.visible_windows(surface_cols);

            // Sync focused window with actual visible windows: if the currently focused
            // window is not visible, focus the frontmost visible one.
            let focused = global_data.state.focused_window.clone();
            let focused_is_visible = focused
                .as_ref()
                .map(|f| visible.iter().any(|(w, _)| w == f))
                .unwrap_or(false);
            if !focused_is_visible && let Some((front, _)) = visible.first() {
                global_data.state.focused_window = Some(front.clone());
                has_focus.set_id(FlexBoxId::from(Id::pane(0)));
            }

            let surface = {
                let mut it = surface!(stylesheet: create_stylesheet()?);
                it.surface_start(SurfaceProps {
                    pos: col(0) + row(1),
                    size: {
                        let col_count = window_size.col_width;
                        let row_count = window_size.row_height - height(2);
                        col_count + row_count
                    },
                })?;

                PanesRenderer { visible: &visible }.render_in_surface(
                    &mut it,
                    global_data,
                    component_registry_map,
                    has_focus,
                )?;

                it.surface_end()?;
                it
            };

            let mut pipeline = surface.render_pipeline;

            let picker_open = global_data.state.file_name_picker_open;
            let focused_window = global_data.state.focused_window.clone();
            render_status_bar(
                &mut pipeline,
                window_size,
                picker_open,
                focused_window.as_ref(),
                &global_data.state,
            );

            render_title_bars(
                &mut pipeline,
                window_size,
                &visible,
                focused_window.as_ref(),
                &global_data.state,
            );

            pipeline
        });
    }
}

/// Returns the `FlexBoxId` for the pane slot that corresponds to the focused window.
fn focused_pane_id(state: &State) -> FlexBoxId {
    let Some(focused) = &state.focused_window else {
        return FlexBoxId::from(Id::Pane0);
    };
    let slot = state
        .window_stack
        .iter()
        .position(|w| w == focused)
        .unwrap_or(0);
    FlexBoxId::from(Id::pane(slot))
}

fn cycle_focus(
    state: &mut State,
    has_focus: &mut HasFocus,
    visible: &[(Window, u16)],
    direction: i32,
) {
    if visible.is_empty() {
        return;
    }
    let current_pos = state
        .focused_window
        .as_ref()
        .and_then(|f| visible.iter().position(|(w, _)| w == f))
        .unwrap_or(0);
    let len = visible.len() as i32;
    let next_pos = ((current_pos as i32 + direction).rem_euclid(len)) as usize;
    let next_window = visible[next_pos].0.clone();
    state.focused_window = Some(next_window);
    has_focus.set_id(FlexBoxId::from(Id::pane(next_pos)));
}

struct PanesRenderer<'a> {
    visible: &'a [(Window, u16)],
}

impl SurfaceRender<State, AppSignal> for PanesRenderer<'_> {
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

            for (slot, (window, col_width)) in self.visible.iter().enumerate() {
                let pane_id = FlexBoxId::from(Id::pane(slot));

                // Store which window is in this slot so components can read it from state.
                global_data.state.window_stack[slot] = window.clone();

                let width_pc: i32 = (*col_width as i32) * 100
                    / (global_data.window_size.col_width.as_u32().max(1) as i32);
                box_start!(
                    in: surface,
                    id: pane_id,
                    dir: LayoutDirection::Vertical,
                    requested_size_percent: req_size_pc!(width: {width_pc}, height: 100),
                    styles: [pane_id],
                );
                render_component_in_current_box!(
                    in: surface,
                    component_id: pane_id,
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
            new_style!(id: {Id::Container}),
            new_style!(
                id: {Id::Pane0}
                padding: {1}
                color_bg: {tui_color!(15, 15, 25)}
            ),
            new_style!(
                id: {Id::Pane1}
                padding: {1}
                color_bg: {tui_color!(15, 15, 25)}
            ),
            new_style!(
                id: {Id::Pane2}
                padding: {1}
                color_bg: {tui_color!(15, 15, 25)}
            ),
            new_style!(
                id: {Id::Pane3}
                padding: {1}
                color_bg: {tui_color!(15, 15, 25)}
            ),
            new_style!(
                id: {Id::Pane4}
                padding: {1}
                color_bg: {tui_color!(15, 15, 25)}
            )
        }
    })
}

fn render_status_bar(
    pipeline: &mut RenderPipeline,
    size: Size,
    picker_open: bool,
    focused_window: Option<&Window>,
    state: &State,
) {
    let color_bg = tui_color!(30, 30, 50);
    let color_fg = tui_color!(180, 180, 220);
    let color_warn = tui_color!(220, 160, 60);

    let hint = if picker_open {
        " Esc:Close  ↑↓:Select  Enter:Open  Tab:Switch  Ctrl+P:Picker  Ctrl+C:Quit"
    } else {
        match focused_window {
            Some(Window::FilePreview(_)) => {
                " Esc:Send to back  ↑↓/PgUp/PgDn:Scroll  Tab:Switch  Ctrl+P:Picker  Ctrl+C:Quit"
            }
            _ => " Ctrl+P:Open file  Tab:Switch  Ctrl+C:Quit",
        }
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

fn render_title_bars(
    pipeline: &mut RenderPipeline,
    _size: Size,
    visible: &[(Window, u16)],
    focused_window: Option<&Window>,
    state: &State,
) {
    let color_bg_active = tui_color!(50, 50, 90);
    let color_fg_active = tui_color!(220, 220, 255);
    let color_bg_inactive = tui_color!(25, 25, 45);
    let color_fg_inactive = tui_color!(120, 120, 160);
    let color_fg_deleted = tui_color!(220, 80, 80);

    let snapshot = state.files.load();

    let mut col_offset: u16 = 0;
    for (window, pane_width) in visible.iter() {
        let is_focused = focused_window == Some(window);

        let is_deleted = match window {
            Window::FilePreview(key) => snapshot[key.0]
                .removed
                .load(std::sync::atomic::Ordering::Relaxed),
            Window::FileNamePicker => false,
        };

        let color_bg = if is_focused {
            color_bg_active
        } else {
            color_bg_inactive
        };
        let color_fg = if is_deleted {
            color_fg_deleted
        } else if is_focused {
            color_fg_active
        } else {
            color_fg_inactive
        };

        let title = match window {
            Window::FileNamePicker => state
                .root
                .file_name()
                .unwrap_or(state.root.as_str())
                .to_string(),
            Window::FilePreview(key) => {
                let file = &snapshot[key.0];
                let rel = file.path.strip_prefix(&state.root).unwrap_or(&file.path);
                let removed = file.removed.load(std::sync::atomic::Ordering::Relaxed);
                if removed {
                    format!("[deleted] {}", rel)
                } else {
                    rel.as_str().to_string()
                }
            }
        };

        let available = *pane_width as usize;
        let padded = format!(" {title} ");
        let display = if padded.len() > available {
            let truncated = &padded[..available.saturating_sub(1)];
            format!("{truncated}…")
        } else {
            padded
        };

        let mut render_ops = RenderOpIRVec::new();
        render_ops += RenderOpCommon::MoveCursorPositionAbs(col(col_offset) + row(0));
        render_ops += RenderOpCommon::ResetColor;
        render_ops += RenderOpCommon::SetBgColor(color_bg);
        render_ops += RenderOpIR::PaintTextWithAttributes(
            SPACER_GLYPH.repeat(*pane_width as usize).into(),
            None,
        );
        render_ops += RenderOpCommon::MoveCursorPositionAbs(col(col_offset) + row(0));
        render_ops += RenderOpIR::PaintTextWithAttributes(
            display.into(),
            Some(if is_focused {
                new_style!(bold color_fg: {color_fg} color_bg: {color_bg})
            } else {
                new_style!(color_fg: {color_fg} color_bg: {color_bg})
            }),
        );
        pipeline.push(ZOrder::Normal, render_ops);

        col_offset += pane_width;
    }
}

pub fn build_state(files: Arc<ArcSwap<Vec<LoadedFile>>>, root: Utf8PathBuf) -> State {
    State::new(files, root)
}

pub async fn run(
    initial_state: State,
    files: Arc<ArcSwap<Vec<LoadedFile>>>,
    root: Utf8PathBuf,
) -> CommonResult<()> {
    let exit_tx: Arc<OnceLock<mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>>> =
        Arc::new(OnceLock::new());
    let exit_message: Arc<OnceLock<&'static str>> = Arc::new(OnceLock::new());

    // Send Exit to the TUI event loop on SIGTERM/SIGINT so RawMode::end() runs cleanly.
    for (kind, message) in [
        (
            tokio::signal::unix::SignalKind::terminate(),
            "MUST TERMINATE ALL HUMANS",
        ),
        (
            tokio::signal::unix::SignalKind::interrupt(),
            "How DARE you interrupt me!",
        ),
    ] {
        let exit_tx_signal = Arc::clone(&exit_tx);
        let exit_message_signal = Arc::clone(&exit_message);
        tokio::spawn(async move {
            if tokio::signal::unix::signal(kind)
                .expect("failed to register signal handler")
                .recv()
                .await
                .is_some()
            {
                let _ = exit_message_signal.set(message);
                if let Some(tx) = exit_tx_signal.get() {
                    let _ = tx.send(TerminalWindowMainThreadSignal::Exit).await;
                }
            }
        });
    }

    let app = AppMain::new_boxed(files, root, exit_tx);
    let exit_keys = &[InputEvent::Keyboard(KeyPress::WithModifiers {
        key: Key::Character('c'),
        mask: ModifierKeysMask::new().with_ctrl(),
    })];
    let _unused: (GlobalData<_, _>, InputDevice, OutputDevice) =
        match TerminalWindow::main_event_loop(app, exit_keys, initial_state) {
            TuiAvailability::Available(future) => future.await?,
            it => return it.into_err(),
        };
    if let Some(msg) = exit_message.get() {
        eprintln!("{msg}");
    }
    ok!()
}
