use crate::loader::LoadedFile;
use crate::lsp::{self, LSP_RRT};
use crate::session::save_session;
use crate::tui::pane_component::PaneComponent;
use crate::tui::panes_renderer::PanesRenderer;
use crate::tui::*;
use crate::watcher::{WATCHER_RRT, WatcherWorker};
use arc_swap::ArcSwap;
use camino::{Utf8Path, Utf8PathBuf};
use r3bl_tui::SubscriberGuard;
use r3bl_tui::core::osc::OscEvent;
use r3bl_tui::core::pty::{
    CursorKeyMode, DefaultPtySessionConfig, PtyInputEvent, PtyOutputEvent, PtySessionBuilder,
    PtySessionConfigOption,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Id {
    Container = 1,
    Pane0 = 2,
    Pane1 = 3,
    Pane2 = 4,
    Pane3 = 5,
    Pane4 = 6,
    Pane5 = 7,
    Pane6 = 8,
    Pane7 = 9,
    Pane8 = 10,
    Pane9 = 11,
    Pane10 = 12,
    Pane11 = 13,
    Pane12 = 14,
    Pane13 = 15,
    Pane14 = 16,
    Pane15 = 17,
}

impl Id {
    pub fn pane(slot: usize) -> Self {
        match slot {
            0 => Id::Pane0,
            1 => Id::Pane1,
            2 => Id::Pane2,
            3 => Id::Pane3,
            4 => Id::Pane4,
            5 => Id::Pane5,
            6 => Id::Pane6,
            7 => Id::Pane7,
            8 => Id::Pane8,
            9 => Id::Pane9,
            10 => Id::Pane10,
            11 => Id::Pane11,
            12 => Id::Pane12,
            13 => Id::Pane13,
            14 => Id::Pane14,
            _ => Id::Pane15,
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

pub struct AppMain {
    files: Arc<ArcSwap<Vec<Arc<LoadedFile>>>>,
    root: Utf8PathBuf,
    picker_results_tx: mpsc::Sender<PickerResultMsg>,
    picker_results_rx: mpsc::Receiver<PickerResultMsg>,
    picker_generation: Arc<AtomicU64>,
    exit_tx: Arc<OnceLock<mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>>>,
    terminal_event_tx: mpsc::UnboundedSender<(usize, PtyOutputEvent)>,
    terminal_event_rx: mpsc::UnboundedReceiver<(usize, PtyOutputEvent)>,
    lsp_guard: Option<SubscriberGuard<crate::lsp::LspWorker>>,
    lsp_last_gen: u8,
    lsp_health_skip: u32,
    watcher_guard: Option<SubscriberGuard<WatcherWorker>>,
}

impl AppMain {
    fn new_boxed(
        files: Arc<ArcSwap<Vec<Arc<LoadedFile>>>>,
        root: Utf8PathBuf,
        exit_tx: Arc<OnceLock<mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>>>,
    ) -> BoxedSafeApp<AppState, AppSignal> {
        let (picker_results_tx, picker_results_rx) = mpsc::channel(32);
        let (terminal_event_tx, terminal_event_rx) = mpsc::unbounded_channel();
        Box::new(Self {
            files,
            root,
            picker_results_tx,
            picker_results_rx,
            picker_generation: Arc::new(AtomicU64::new(0)),
            exit_tx,
            terminal_event_tx,
            terminal_event_rx,
            lsp_guard: None,
            lsp_last_gen: 0,
            lsp_health_skip: 0,
            watcher_guard: None,
        })
    }

    fn open_terminal(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        cmd: Option<String>,
        cwd: Option<Utf8PathBuf>,
        pane_size: Option<PaneSize>,
        id: Option<usize>,
    ) -> CommonResult<EventPropagation> {
        throws_with_return!({
            let state = &mut global_data.state;
            let restore = id.is_some();
            let id = id.unwrap_or_else(|| {
                let id = state.next_terminal_id;
                state.next_terminal_id += 1;
                id
            });

            let window_size = global_data.window_size;
            // Start with the full window width; the first render will resize to
            // the actual pane slot assigned by the layout.
            let pty_cols = window_size.col_width.as_u16().max(80);
            let pty_rows = window_size.row_height.as_u16().saturating_sub(2);
            let pty_size = Size {
                col_width: width(pty_cols),
                row_height: height(pty_rows),
            };

            let is_command_pane = cmd.as_deref().is_some_and(|s| !s.is_empty());
            let mut builder = if is_command_pane {
                PtySessionBuilder::new("/bin/sh").cli_args(["-c", cmd.as_deref().unwrap()])
            } else {
                PtySessionBuilder::new(shell_command())
            };
            if let Some(ref cwd_path) = cwd {
                builder = builder.cwd(cwd_path.as_std_path());
            }
            let mut session = match builder
                .with_config(DefaultPtySessionConfig + PtySessionConfigOption::Size(pty_size))
                .start()
            {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("Failed to start PTY: {e}");
                    return Ok(EventPropagation::ConsumedRender);
                }
            };

            let ofs_buf = r3bl_tui::OfsBufVT100::new_empty(pty_size);
            let pty_input_tx = Arc::new(session.tx_input_event.clone());
            let child_killer = session.child_process_termination_handle;
            let initial_title = cmd.as_ref().filter(|s| !s.is_empty()).cloned();
            let pane_cwd = cwd.clone().unwrap_or_else(|| self.root.clone());
            let pane_command = if is_command_pane { cmd.clone() } else { None };
            let pane = Arc::new(Mutex::new(TerminalPane {
                ofs_buf,
                cursor_key_mode: CursorKeyMode::Normal,
                title: initial_title,
                pty_input_tx,
                child_killer: Some(child_killer),
                last_size: pty_size,
                is_command_pane,
                exited: false,
                exit_code: None,
                exit_signal: None,
                scroll_offset: 0,
                cwd: pane_cwd,
                command: pane_command,
            }));

            state.terminal_panes.insert(id, Arc::clone(&pane));

            let notify_tx = global_data.main_thread_channel_sender.clone();
            let event_tx = self.terminal_event_tx.clone();
            tokio::spawn(async move {
                let mut last_event = Instant::now();
                let mut backoff: Option<Instant> = None;
                while let Some(event) = session.rx_output_event.recv().await {
                    let is_exit = matches!(&event, PtyOutputEvent::Exit(_));
                    match event {
                        PtyOutputEvent::Output(bytes) => {
                            if let Ok(mut pane) = pane.lock() {
                                let combined_before = pane
                                    .ofs_buf
                                    .scrollback_len()
                                    .saturating_add(pane.ofs_buf.buffer.len());
                                let (osc_events, _, da_responses) =
                                    pane.ofs_buf.apply_ansi_bytes(&bytes);
                                let combined_after = pane
                                    .ofs_buf
                                    .scrollback_len()
                                    .saturating_add(pane.ofs_buf.buffer.len());
                                if pane.scroll_offset > 0 {
                                    pane.scroll_offset = pane.scroll_offset.saturating_add(
                                        combined_after.saturating_sub(combined_before),
                                    );
                                }
                                for osc_event in osc_events {
                                    if let OscEvent::SetTitleAndTab(title) = osc_event {
                                        pane.title = Some(title);
                                    }
                                }
                                for da_response in da_responses {
                                    let _ = pane
                                        .pty_input_tx
                                        .try_send(PtyInputEvent::Write(da_response.into_bytes()));
                                }
                            }
                        }
                        PtyOutputEvent::CursorModeChange(mode) => {
                            if let Ok(mut pane) = pane.lock() {
                                pane.cursor_key_mode = mode;
                            }
                        }
                        PtyOutputEvent::MouseModeChange(mode) => {
                            if let Ok(mut pane) = pane.lock() {
                                pane.ofs_buf.terminal_mode.mouse_tracking_mode = mode;
                            }
                        }
                        PtyOutputEvent::Exit(status) => {
                            if event_tx.send((id, PtyOutputEvent::Exit(status))).is_err() {
                                break;
                            }
                        }
                        _ => {}
                    }

                    // Exit: always send Noop (never throttled) so the
                    // terminal pane is removed from the UI immediately.
                    if is_exit {
                        let _ = notify_tx.try_send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                            AppSignal::Noop,
                        ));
                        break;
                    }

                    let now = Instant::now();

                    // Throttle: once the channel has filled (backoff ==
                    // Some), suppress all Noops as long as events keep
                    // arriving within 100ms gaps.  This threshold catches
                    // program output (≥10 events/s) while cleanly
                    // separating interactive typing (~200ms between keys).
                    // last_event is updated on every event (including
                    // suppressed), so a sustained burst keeps the gate
                    // closed until the channel has room again.  A gap
                    // ≥100ms in events resets and the task tries to send.
                    if last_event.elapsed().as_millis() < 100 && backoff.is_some() {
                        backoff = Some(now);
                        last_event = now;
                        continue;
                    }

                    // Channel has room (or backoff expired): try to send.
                    // If it succeeds, clear backoff.  If it fails (buffer
                    // full at 1000), enter backoff — subsequent events are
                    // suppressed until activity pauses for >=100ms.
                    match notify_tx.try_send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                        AppSignal::Noop,
                    )) {
                        Ok(()) => backoff = None,
                        Err(_) => backoff = Some(now),
                    }

                    last_event = now;
                }
            });

            let window = Window::Terminal(id);
            if !restore {
                let old = state.pane_manager.focused_window;
                state.pane_manager.push_window(window);
                if let Some(size) = pane_size {
                    state
                        .pane_manager
                        .window_states
                        .entry(window)
                        .or_default()
                        .pane_size = size;
                }
                state.pane_manager.focused_window = Some(window);
                notify_terminal_focus_change(state, old, Some(window));
                state.terminal_grabbed = true;
                state.mark_session_dirty();
            }

            EventPropagation::ConsumedRender
        });
    }
}

fn shell_command() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "bash".into())
}

fn is_buffer_empty(ofs_buf: &r3bl_tui::OfsBufVT100) -> bool {
    ofs_buf.buffer.iter().all(|line| {
        line.iter()
            .all(|pc| !matches!(pc, PixelChar::PlainText { .. }))
    })
}

fn poll_terminal_output(app: &mut AppMain, state: &mut AppState) {
    while let Ok((id, event)) = app.terminal_event_rx.try_recv() {
        if let PtyOutputEvent::Exit(status) = event {
            let exit_code = Some(status.exit_code());
            let exit_signal = status.signal().map(String::from);
            let remove_now = state
                .terminal_panes
                .get(&id)
                .and_then(|pane| pane.lock().ok())
                .is_some_and(|p| is_buffer_empty(&p.ofs_buf));

            if remove_now {
                // Send focus-lost before removing the pane.
                if let Some(pane) = state.terminal_panes.get(&id)
                    && let Ok(p) = pane.lock()
                    && p.ofs_buf.terminal_mode.focus_events
                {
                    let _ = p
                        .pty_input_tx
                        .try_send(PtyInputEvent::Write(b"\x1b[O".to_vec()));
                }
                let old = state.pane_manager.focused_window;
                if let Some(pane) = state.terminal_panes.remove(&id)
                    && let Ok(mut p) = pane.lock()
                    && let Some(mut killer) = p.child_killer.take()
                {
                    let _ = killer.kill();
                }
                state.pane_manager.remove_window(&Window::Terminal(id));
                notify_terminal_focus_change(state, old, state.pane_manager.focused_window);
                state.mark_session_dirty();
                sync_terminal_grabbed(state);
            } else if let Some(pane) = state.terminal_panes.get(&id)
                && let Ok(mut p) = pane.lock()
            {
                p.exited = true;
                p.exit_code = exit_code;
                p.exit_signal = exit_signal;
            }
        }
    }
}

const SESSION_SAVE_DEBOUNCE: Duration = Duration::from_secs(2);

// save_session is intentionally a synchronous fast JSON write on the main thread.
fn maybe_save_session(state: &mut AppState, root: &Utf8Path) {
    let Some(dirty_at) = state.session_dirty_at else {
        return;
    };
    if dirty_at.elapsed() < SESSION_SAVE_DEBOUNCE {
        return;
    }
    match save_session(root, state) {
        Ok(()) => {
            state.session_dirty_at = None;
        }
        Err(e) => tracing::warn!("Failed to save session: {e}"),
    }
}

impl App for AppMain {
    type S = AppState;
    type AS = AppSignal;

    fn app_init_components(
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
                    PaneComponent::new_boxed(
                        slot,
                        pane_id,
                        self.picker_results_tx.clone(),
                        Arc::clone(&self.picker_generation),
                    ),
                );
            }
        }

        if has_focus.get_id().is_none() {
            has_focus.set_id(FlexBoxId::from(Id::Pane0));
        }
    }

    fn app_start_background_services(&mut self, global_data: &mut GlobalData<AppState, AppSignal>) {
        let notify_tx = global_data.main_thread_channel_sender.clone();

        // Publish the channel sender so the SIGTERM handler can request a clean exit.
        let _ = self.exit_tx.set(notify_tx.clone());
        let files = Arc::clone(&self.files);
        let root = self.root.clone();

        match LSP_RRT.try_subscribe(lsp::LspConfig {
            root: root.clone(),
            files: Arc::clone(&files),
            app_tx: notify_tx.clone(),
        }) {
            Ok(guard) => {
                self.lsp_guard = Some(guard);
                tracing::info!("LSP worker started");
            }
            Err(e) => tracing::warn!("LSP worker failed to start: {e}"),
        }

        match WATCHER_RRT.try_subscribe(crate::watcher::WatcherConfig {
            root,
            app_tx: notify_tx.clone(),
        }) {
            Ok(guard) => {
                self.watcher_guard = Some(guard);
                tracing::info!("watcher started");
            }
            Err(e) => tracing::warn!("watcher failed to start: {e}"),
        }

        // Global 1s refresh timer — catches any final render state that the
        // per-task 1s debounce might miss (burst ends, no more events).
        let timer_notify = notify_tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.tick().await;
            loop {
                interval.tick().await;
                let _ = timer_notify.try_send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                    AppSignal::Noop,
                ));
            }
        });

        let stack = global_data.state.pane_manager.window_stack.clone();
        for window in stack {
            let Window::Terminal(id) = window else {
                continue;
            };
            let Some(info) = global_data.state.pending_terminals.remove(&id) else {
                continue;
            };
            let cwd = if info.cwd.exists() {
                Some(info.cwd)
            } else {
                tracing::warn!(
                    "Terminal cwd no longer exists, falling back to repo root: {}",
                    info.cwd
                );
                Some(global_data.state.root.clone())
            };
            let cmd = if info.is_command_pane {
                info.command
            } else {
                None
            };
            let _ = self.open_terminal(global_data, cmd, cwd, None, Some(id));
            if !global_data.state.terminal_panes.contains_key(&id) {
                global_data.state.pane_manager.remove_window(&window);
                global_data.state.mark_session_dirty();
            }
        }

        // Request full tokens for session-restored file previews.
        for window in global_data.state.pane_manager.window_stack.iter() {
            if let Window::FilePreview(key) = window {
                crate::lsp::send_file_request(key.0);
            }
        }
        crate::lsp::try_drain_pending_requests();
    }

    fn app_handle_input_event(
        &mut self,
        input_event: InputEvent,
        global_data: &mut GlobalData<AppState, AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<AppState, AppSignal>,
        has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        sync_has_focus(&global_data.state, has_focus);
        let surface_size = surface_size(global_data.window_size);
        global_data.state.last_surface_size = surface_size;

        // Leader key activation.
        if let InputEvent::Keyboard(KeyPress::WithModifiers {
            key: Key::Character('`'),
            mask:
                ModifierKeysMask {
                    alt_key_state: KeyState::Pressed,
                    shift_key_state: KeyState::NotPressed,
                    ctrl_key_state: KeyState::NotPressed,
                },
        }) = input_event
            && !global_data.state.mouse_drag_active
            && !global_data.state.leader_active
        {
            global_data.state.leader_active = true;
            global_data.state.terminal_grabbed = false;
            return Ok(EventPropagation::ConsumedRender);
        }

        // Leader key dispatch.
        if global_data.state.leader_active && !global_data.state.mouse_drag_active {
            global_data.state.leader_active = false;
            match &input_event {
                InputEvent::Keyboard(KeyPress::WithModifiers {
                    key: Key::Character('`'),
                    mask:
                        ModifierKeysMask {
                            alt_key_state: KeyState::Pressed,
                            shift_key_state: KeyState::NotPressed,
                            ctrl_key_state: KeyState::NotPressed,
                        },
                }) if matches!(
                    global_data.state.pane_manager.focused_window,
                    Some(Window::Terminal(_))
                ) =>
                {
                    global_data.state.terminal_grabbed = true;
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::Character('f'),
                }) => {
                    let state = &mut global_data.state;
                    let old = state.pane_manager.focused_window;
                    state.pane_manager.push_window(Window::FileNamePicker);
                    state.pane_manager.focused_window = Some(Window::FileNamePicker);
                    notify_terminal_focus_change(state, old, Some(Window::FileNamePicker));
                    state.file_name_picker.selected = None;
                    let snapshot = state.files.load();
                    state.file_name_picker.results = FileNamePickerComponent::open_previews_results(
                        &snapshot,
                        &state.pane_manager.window_stack,
                    );
                    state.file_name_picker.query = String::new();
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::Character('t'),
                }) => {
                    return self.open_terminal(global_data, None, None, None, None);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::Character('T'),
                }) => {
                    let state = &mut global_data.state;
                    let old = state.pane_manager.focused_window;
                    if !state
                        .pane_manager
                        .window_stack
                        .contains(&Window::ThemePicker)
                    {
                        state.saved_theme = state.theme.clone();
                    }
                    let all_themes: Vec<(String, Vec<u32>)> = HelixTheme::theme_names()
                        .map(|n| (n.to_string(), Vec::new()))
                        .collect();
                    state.pane_manager.push_window(Window::ThemePicker);
                    state.pane_manager.focused_window = Some(Window::ThemePicker);
                    notify_terminal_focus_change(state, old, Some(Window::ThemePicker));
                    state.theme_picker.selected = all_themes
                        .iter()
                        .position(|(n, _)| n == state.theme.name())
                        .and_then(|i| all_themes.get(i).map(|(n, _)| n.clone()));
                    state.theme_picker.results = all_themes;
                    state.theme_picker.query = String::new();
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::Character('q'),
                }) => {
                    return Ok(EventPropagation::ExitMainEventLoop);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::Character('='),
                }) => {
                    global_data
                        .state
                        .pane_manager
                        .resize_focused(ResizeDelta::Grow);
                    global_data.state.mark_session_dirty();
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::Character('-'),
                }) => {
                    global_data
                        .state
                        .pane_manager
                        .resize_focused(ResizeDelta::Shrink);
                    global_data.state.mark_session_dirty();
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Up),
                }) => {
                    let Some(focused) = global_data.state.pane_manager.focused_window else {
                        return Ok(EventPropagation::ConsumedRender);
                    };
                    global_data.state.pane_manager.move_forward(&focused);
                    global_data.state.mark_session_dirty();
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Down),
                }) => {
                    let Some(focused) = global_data.state.pane_manager.focused_window else {
                        return Ok(EventPropagation::ConsumedRender);
                    };
                    global_data.state.pane_manager.move_backward(&focused);
                    global_data.state.mark_session_dirty();
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Tab),
                }) => {
                    let visible = global_data.state.pane_manager.layout(surface_size);
                    let old = global_data.state.pane_manager.focused_window;
                    global_data.state.pane_manager.cycle_focus(&visible, 1);
                    notify_terminal_focus_change(
                        &global_data.state,
                        old,
                        global_data.state.pane_manager.focused_window,
                    );
                    sync_terminal_grabbed(&mut global_data.state);
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::BackTab),
                }) => {
                    let visible = global_data.state.pane_manager.layout(surface_size);
                    let old = global_data.state.pane_manager.focused_window;
                    global_data.state.pane_manager.cycle_focus(&visible, -1);
                    notify_terminal_focus_change(
                        &global_data.state,
                        old,
                        global_data.state.pane_manager.focused_window,
                    );
                    sync_terminal_grabbed(&mut global_data.state);
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::Character('x'),
                }) => {
                    let state = &mut global_data.state;
                    let tid = match state.pane_manager.focused_window {
                        Some(Window::Terminal(tid)) => tid,
                        _ => return Ok(EventPropagation::ConsumedRender),
                    };
                    // Send focus-lost before removing the pane.
                    if let Some(pane) = state.terminal_panes.get(&tid)
                        && let Ok(p) = pane.lock()
                        && p.ofs_buf.terminal_mode.focus_events
                    {
                        let _ = p
                            .pty_input_tx
                            .try_send(PtyInputEvent::Write(b"\x1b[O".to_vec()));
                    }
                    let old = state.pane_manager.focused_window;
                    if let Some(pane) = state.terminal_panes.remove(&tid)
                        && let Ok(mut p) = pane.lock()
                        && let Some(mut killer) = p.child_killer.take()
                    {
                        let _ = killer.kill();
                    }
                    state.pane_manager.remove_window(&Window::Terminal(tid));
                    notify_terminal_focus_change(state, old, state.pane_manager.focused_window);
                    sync_terminal_grabbed(state);
                    state.mark_session_dirty();
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Esc),
                }) => {
                    return Ok(EventPropagation::ConsumedRender);
                }
                _ => {}
            }
        }

        // Global shortcuts: Tab/BackTab cycles focus; Ctrl+=/- resize; Ctrl+Up/Down reorder.
        // Skipped for Terminal panes so Tab reaches the PTY (e.g. shell completion).
        // When a terminal is ungrabbed Tab cycles focus.
        if (!matches!(
            global_data.state.pane_manager.focused_window,
            Some(Window::Terminal(_))
        ) || !global_data.state.terminal_grabbed)
            && !global_data.state.mouse_drag_active
        {
            match &input_event {
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Tab),
                }) => {
                    let visible = global_data.state.pane_manager.layout(surface_size);
                    let old = global_data.state.pane_manager.focused_window;
                    global_data.state.pane_manager.cycle_focus(&visible, 1);
                    notify_terminal_focus_change(
                        &global_data.state,
                        old,
                        global_data.state.pane_manager.focused_window,
                    );
                    sync_terminal_grabbed(&mut global_data.state);
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::BackTab),
                }) => {
                    let visible = global_data.state.pane_manager.layout(surface_size);
                    let old = global_data.state.pane_manager.focused_window;
                    global_data.state.pane_manager.cycle_focus(&visible, -1);
                    notify_terminal_focus_change(
                        &global_data.state,
                        old,
                        global_data.state.pane_manager.focused_window,
                    );
                    sync_terminal_grabbed(&mut global_data.state);
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::WithModifiers {
                    key: Key::Character('='),
                    mask:
                        ModifierKeysMask {
                            ctrl_key_state: KeyState::Pressed,
                            ..
                        },
                }) => {
                    global_data
                        .state
                        .pane_manager
                        .resize_focused(ResizeDelta::Grow);
                    global_data.state.mark_session_dirty();
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::WithModifiers {
                    key: Key::Character('-'),
                    mask:
                        ModifierKeysMask {
                            ctrl_key_state: KeyState::Pressed,
                            ..
                        },
                }) => {
                    global_data
                        .state
                        .pane_manager
                        .resize_focused(ResizeDelta::Shrink);
                    global_data.state.mark_session_dirty();
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::WithModifiers {
                    key: Key::SpecialKey(SpecialKey::Up),
                    mask:
                        ModifierKeysMask {
                            ctrl_key_state: KeyState::Pressed,
                            ..
                        },
                }) => {
                    let Some(focused) = global_data.state.pane_manager.focused_window else {
                        return Ok(EventPropagation::ConsumedRender);
                    };
                    global_data.state.pane_manager.move_forward(&focused);
                    global_data.state.mark_session_dirty();
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::WithModifiers {
                    key: Key::SpecialKey(SpecialKey::Down),
                    mask:
                        ModifierKeysMask {
                            ctrl_key_state: KeyState::Pressed,
                            ..
                        },
                }) => {
                    let Some(focused) = global_data.state.pane_manager.focused_window else {
                        return Ok(EventPropagation::ConsumedRender);
                    };
                    global_data.state.pane_manager.move_backward(&focused);
                    global_data.state.mark_session_dirty();
                    return Ok(EventPropagation::ConsumedRender);
                }
                _ => {}
            }
        }

        // Focus follows mouse: hovering over a pane makes it the focused window.
        if let InputEvent::Mouse(MouseInput {
            kind: MouseInputKind::MouseMove,
            maybe_modifier_keys: None,
            pos,
        }) = &input_event
            && !global_data.state.mouse_drag_active
        {
            let layout = global_data.state.pane_manager.layout(surface_size);
            for slot in &layout {
                let ox = slot.box_.style_adjusted_origin_pos.col_index;
                let oy = slot.box_.style_adjusted_origin_pos.row_index;
                let w = slot.box_.style_adjusted_bounds_size.col_width;
                let h = slot.box_.style_adjusted_bounds_size.row_height;
                if pos.col_index >= ox
                    && pos.col_index < ox + w
                    && pos.row_index >= oy
                    && pos.row_index < oy + h
                {
                    if global_data.state.pane_manager.focused_window.as_ref() != Some(&slot.window)
                    {
                        let old = global_data.state.pane_manager.focused_window;
                        global_data.state.pane_manager.focused_window = Some(slot.window);
                        notify_terminal_focus_change(&global_data.state, old, Some(slot.window));
                        // Grab state on mouse focus change depends on pane type + scroll.
                        match slot.window {
                            Window::Terminal(id) => {
                                let scrolled = global_data
                                    .state
                                    .terminal_panes
                                    .get(&id)
                                    .and_then(|p| p.lock().ok())
                                    .is_some_and(|p| p.scroll_offset > 0);
                                global_data.state.terminal_grabbed = !scrolled;
                            }
                            _ => {
                                global_data.state.terminal_grabbed = false;
                            }
                        }
                        return Ok(EventPropagation::ConsumedRender);
                    }
                    break;
                }
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
        global_data: &mut GlobalData<AppState, AppSignal>,
        _component_registry_map: &mut ComponentRegistryMap<AppState, AppSignal>,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        sync_has_focus(&global_data.state, _has_focus);

        if let AppSignal::OpenTerminal { cmd, cwd } = action {
            let cmd = cmd.clone();
            let cwd = cwd.clone();
            return self.open_terminal(global_data, cmd, Some(cwd), None, None);
        }
        throws_with_return!({
            let state = &mut global_data.state;
            match action {
                AppSignal::FilesChanged(batch) => {
                    tracing::info!(
                        "FilesChanged: {} modified, {} created, {} removed",
                        batch.modified.len(),
                        batch.created.len(),
                        batch.removed.len()
                    );
                    let snapshot = self.files.load_full();

                    for path in &batch.removed {
                        if let Some((file_idx, file)) =
                            snapshot.iter().enumerate().find(|(_, f)| &f.path == path)
                        {
                            tracing::debug!(
                                "FilesChanged: removed match idx={} path={}",
                                file_idx,
                                path
                            );
                            file.removed.store(true, Ordering::Relaxed);
                            crate::lsp::send_file_request(file_idx);
                        } else {
                            tracing::debug!("FilesChanged: removed no match for path={}", path);
                        }
                    }

                    for path in &batch.modified {
                        if let Some((file_idx, file)) = snapshot
                            .iter()
                            .enumerate()
                            .find(|(_, f)| &f.path == path && !f.removed.load(Ordering::Relaxed))
                        {
                            tracing::debug!(
                                "FilesChanged: modified match idx={} path={}",
                                file_idx,
                                path
                            );
                            file.reload();
                            crate::lsp::send_file_request(file_idx);
                        } else {
                            tracing::debug!("FilesChanged: modified no match for path={}", path);
                        }
                    }

                    let mut new_files: Vec<Arc<LoadedFile>> = vec![];
                    for path in &batch.created {
                        if let Some((file_idx, file)) = snapshot
                            .iter()
                            .enumerate()
                            .find(|(_, f)| &f.path == path && f.removed.load(Ordering::Relaxed))
                        {
                            tracing::debug!(
                                "FilesChanged: created resurrect idx={} path={}",
                                file_idx,
                                path
                            );
                            file.removed.store(false, Ordering::Relaxed);
                            file.reload();
                            crate::lsp::send_file_request(file_idx);
                        } else if let Some((file_idx, file)) = snapshot
                            .iter()
                            .enumerate()
                            .find(|(_, f)| &f.path == path && !f.removed.load(Ordering::Relaxed))
                        {
                            tracing::debug!(
                                "FilesChanged: created replace idx={} path={}",
                                file_idx,
                                path
                            );
                            file.reload();
                            crate::lsp::send_file_request(file_idx);
                        } else if !snapshot.iter().any(|f| &f.path == path)
                            && let Some(loaded) = LoadedFile::load(path.clone().into_std_path_buf())
                        {
                            tracing::debug!("FilesChanged: created new path={}", path);
                            new_files.push(loaded);
                        } else {
                            tracing::debug!("FilesChanged: created no match for path={}", path);
                        }
                    }

                    if !new_files.is_empty() {
                        let mut next: Vec<Arc<LoadedFile>> =
                            snapshot.iter().map(Arc::clone).collect();
                        next.extend(new_files);
                        self.files.store(Arc::new(next));
                    }

                    if state
                        .pane_manager
                        .window_stack
                        .contains(&Window::FileNamePicker)
                    {
                        if state.file_name_picker.query.is_empty() {
                            state.recompute_file_name_picker_results();
                        } else {
                            FileNamePickerComponent::spawn_match(
                                &*state,
                                Arc::clone(&self.picker_generation),
                                self.picker_results_tx.clone(),
                                global_data.main_thread_channel_sender.clone(),
                            );
                        }
                    }
                    state.bump_files_version();
                }
                AppSignal::OpenTerminal { .. } => {}
                AppSignal::Noop => {
                    maybe_save_session(state, &self.root);
                    crate::lsp::try_drain_pending_requests();
                    let health = crate::lsp::health_check();
                    let generation = match &health {
                        crate::lsp::LspHealth::Running { generation, .. } => *generation,
                        crate::lsp::LspHealth::NotRunning => 0,
                    };
                    self.lsp_health_skip = self.lsp_health_skip.wrapping_add(1);
                    if generation != self.lsp_last_gen {
                        tracing::info!("LSP health: {:?}", health);
                        self.lsp_last_gen = generation;
                    }
                    match &health {
                        crate::lsp::LspHealth::Running {
                            input_receivers: 0, ..
                        } if self.lsp_health_skip.is_multiple_of(10) => {
                            tracing::warn!("LSP health: {:?}", health);
                        }
                        crate::lsp::LspHealth::NotRunning => {
                            tracing::warn!("LSP health: NotRunning");
                        }
                        _ if self.lsp_health_skip.is_multiple_of(10) => {
                            tracing::debug!("LSP health: {:?}", health);
                        }
                        _ => {}
                    }
                }
            }

            EventPropagation::ConsumedRender
        });
    }

    fn app_render(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<AppState, AppSignal>,
        has_focus: &mut HasFocus,
    ) -> CommonResult<RenderPipeline> {
        sync_has_focus(&global_data.state, has_focus);

        let mut best_generation = 0u64;
        let mut best_results = None;
        while let Ok((arrived_generation, results)) = self.picker_results_rx.try_recv() {
            if arrived_generation > best_generation {
                best_generation = arrived_generation;
                best_results = Some(results);
            }
        }
        if let Some(results) = best_results {
            global_data.state.file_name_picker.results = results;
        }

        poll_terminal_output(self, &mut global_data.state);

        throws_with_return!({
            let window_size = global_data.window_size;
            let surface_size = surface_size(window_size);
            global_data.state.last_surface_size = surface_size;
            let visible = global_data.state.pane_manager.layout(surface_size);

            // Sync focused window with actual visible windows: if the currently focused
            // window is not visible, focus the frontmost visible one.
            let focused = global_data.state.pane_manager.focused_window;
            let focused_is_visible = focused
                .as_ref()
                .map(|f| visible.iter().any(|s| &s.window == f))
                .unwrap_or(false);
            if !global_data.state.mouse_drag_active
                && !focused_is_visible
                && let Some(first) = visible.first()
            {
                global_data.state.pane_manager.focused_window = Some(first.window);
                sync_terminal_grabbed(&mut global_data.state);
            }
            sync_has_focus(&global_data.state, has_focus);

            let surface = {
                let mut it = surface!(stylesheet: create_stylesheet(&global_data.state.theme)?);
                it.surface_start(SurfaceProps {
                    pos: col(0) + row(0),
                    size: surface_size,
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

            // Fill entire surface area with pane background (covers padding
            // between panes, which the FlexBox layout system does not fill).
            let bg_rgb = global_data
                .state
                .theme
                .ui_bg("ui.background")
                .unwrap_or([15, 15, 25]);
            let bg = tui_color!(bg_rgb[0], bg_rgb[1], bg_rgb[2]);
            let bg_style = new_style!(color_bg: {bg});
            let mut bg_ops = RenderOpIRVec::new();
            let surface_rows = surface_size.row_height.as_usize();
            let surface_col_count = window_size.col_width.as_usize();
            for row_idx in 0..surface_rows {
                let abs_row: u16 = row_idx as u16;
                bg_ops += RenderOpCommon::MoveCursorPositionAbs(col(0) + row(abs_row));
                bg_ops += RenderOpCommon::ApplyColors(Some(bg_style));
                bg_ops += RenderOpIR::PaintTextWithAttributes(
                    " ".repeat(surface_col_count).as_str().into(),
                    Some(bg_style),
                );
            }
            let mut fill_pipeline = render_pipeline!();
            fill_pipeline.push(ZOrder::Normal, bg_ops);
            fill_pipeline.join_into(pipeline);
            pipeline = fill_pipeline;

            render_status_bar(
                &mut pipeline,
                window_size,
                &global_data.state,
                &global_data.state.theme,
            );

            pipeline
        });
    }
}

/// Returns the `FlexBoxId` for the pane slot that corresponds to the focused window.
pub(super) fn focused_pane_id(state: &AppState) -> FlexBoxId {
    let Some(slot) = state.pane_manager.focused_slot() else {
        return FlexBoxId::from(Id::Pane0);
    };
    FlexBoxId::from(Id::pane(slot))
}

fn surface_size(window_size: Size) -> Size {
    let col_count = window_size.col_width;
    let row_count = window_size.row_height - height(1);
    col_count + row_count
}

fn sync_terminal_grabbed(state: &mut AppState) {
    if !matches!(state.pane_manager.focused_window, Some(Window::Terminal(_))) {
        state.terminal_grabbed = false;
    }
}

fn notify_terminal_focus_change(state: &AppState, old: Option<Window>, new: Option<Window>) {
    if old == new {
        return;
    }
    for (window, bytes) in [(old, b"\x1b[O" as &[u8]), (new, b"\x1b[I")] {
        let Some(Window::Terminal(id)) = window else {
            continue;
        };
        let Some(pane) = state.terminal_panes.get(&id) else {
            continue;
        };
        let Ok(p) = pane.lock() else {
            continue;
        };
        if p.ofs_buf.terminal_mode.focus_events {
            let _ = p
                .pty_input_tx
                .try_send(PtyInputEvent::Write(bytes.to_vec()));
        }
    }
}

fn sync_has_focus(state: &AppState, has_focus: &mut HasFocus) {
    has_focus.set_id(focused_pane_id(state));
}

fn create_stylesheet(theme: &HelixTheme) -> CommonResult<TuiStylesheet> {
    let bg = theme.ui_bg("ui.background").unwrap_or([15, 15, 25]);
    throws_with_return!({
        let mut styles = tui_stylesheet! {
            new_style!(
                id: {Id::Container}
                color_bg: {tui_color!(bg[0], bg[1], bg[2])}
            )
        };
        for col_idx in 0..MAX_PANES {
            let id = FlexBoxId::new(100 + col_idx as u8);
            styles.add_style(new_style!(
                id: {id}
                padding: {0}
                color_bg: {tui_color!(bg[0], bg[1], bg[2])}
            ))?;
        }
        for slot in 0..MAX_PANES {
            let id = Id::pane(slot);
            styles.add_style(new_style!(
                id: {id}
                padding: {0}
                color_bg: {tui_color!(bg[0], bg[1], bg[2])}
            ))?;
        }
        styles
    })
}

fn pane_action_hints(prefix: &str, sep: &str) -> String {
    [
        ("=", "Grow"),
        ("-", "Shrink"),
        ("↑", "Forward"),
        ("↓", "Backward"),
    ]
    .iter()
    .map(|(key, label)| format!("{}{}:{}", prefix, key, label))
    .collect::<Vec<_>>()
    .join(sep)
}

pub trait WindowHints {
    fn pane_key_hints(&self) -> &'static str;
}

impl WindowHints for Window {
    fn pane_key_hints(&self) -> &'static str {
        match self {
            Window::FileNamePicker => "Esc:Close  Enter:Open",
            Window::ThemePicker => "Esc:Cancel  Enter:Save",
            Window::FilePreview(_) => "Esc:Send to back  ::Command",
            Window::Terminal(_) => "",
        }
    }
}

fn render_status_bar(
    pipeline: &mut RenderPipeline,
    size: Size,
    state: &AppState,
    theme: &HelixTheme,
) {
    let bg_rgb = theme.ui_bg("ui.statusline").unwrap_or([30, 30, 50]);
    let fg_rgb = theme.ui_fg("ui.statusline").unwrap_or([180, 180, 220]);
    let color_bg = tui_color!(bg_rgb[0], bg_rgb[1], bg_rgb[2]);
    let color_fg = tui_color!(fg_rgb[0], fg_rgb[1], fg_rgb[2]);

    let leader_style = new_style!(bold color_fg: {color_fg} color_bg: {color_bg});
    let normal_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});

    let (leader_text, rest_text) = if state.leader_active {
        (
            " Leader ".to_string(),
            format!(
                "f:Picker  t:Term  T:Theme  x:Close  q:Quit  Tab:Next  Shift+Tab:Prev  {}  Esc:Cancel",
                pane_action_hints("", "  ")
            ),
        )
    } else {
        let pane = match state.pane_manager.focused_window.as_ref() {
            Some(w) => {
                let base = w.pane_key_hints();
                if matches!(w, Window::Terminal(_)) && !state.terminal_grabbed {
                    "Enter:grab  ↑↓PgUp/PgDn:scroll"
                } else {
                    base
                }
            }
            None => "",
        };
        let mut rest = pane_action_hints("Ctrl+", "  ");
        if !pane.is_empty() {
            rest.push_str("  ");
            rest.push_str(pane);
        }
        (" Alt+`: Leader ".to_string(), rest)
    };

    let styled_texts = tui_styled_texts! {
        tui_styled_text! {
            @style: leader_style,
            @text: leader_text
        },
        tui_styled_text! {
            @style: normal_style,
            @text: rest_text
        },
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
    files: Arc<ArcSwap<Vec<Arc<LoadedFile>>>>,
    root: Utf8PathBuf,
    theme: crate::tui::theme::HelixTheme,
) -> AppState {
    AppState::new(files, root, theme)
}

pub async fn run(
    initial_state: AppState,
    files: Arc<ArcSwap<Vec<Arc<LoadedFile>>>>,
    root: Utf8PathBuf,
) -> CommonResult<()> {
    let exit_tx: Arc<OnceLock<mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>>> =
        Arc::new(OnceLock::new());
    let exit_message: Arc<OnceLock<&'static str>> = Arc::new(OnceLock::new());

    // Send Exit to the TUI event loop on SIGTERM/SIGINT/SIGHUP so RawMode::end() runs cleanly.
    for (kind, message) in [
        (
            tokio::signal::unix::SignalKind::terminate(),
            "MUST TERMINATE ALL HUMANS",
        ),
        (
            tokio::signal::unix::SignalKind::interrupt(),
            "How DARE you interrupt me!",
        ),
        (
            tokio::signal::unix::SignalKind::hangup(),
            "HANG UP AND DRIVE",
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
    let exit_keys: &[InputEvent] = &[];
    let (global_data, _, _): (GlobalData<_, _>, _, _) =
        match TerminalWindow::main_event_loop(app, exit_keys, initial_state) {
            TuiAvailability::Available(future) => future.await?,
            it => return it.into_err(),
        };

    // Kill all terminal children gracefully.
    for pane in global_data.state.terminal_panes.values() {
        if let Ok(mut pane) = pane.lock() {
            if let Some(mut killer) = pane.child_killer.take() {
                let _ = killer.kill(); // Sends SIGHUP.
            }
            let _ = pane.pty_input_tx.try_send(PtyInputEvent::Close);
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    if let Err(e) = save_session(&global_data.state.root, &global_data.state) {
        tracing::warn!("Failed to save session on exit: {e}");
    }

    if let Some(msg) = exit_message.get() {
        eprintln!("{msg}");
    }
    ok!()
}
