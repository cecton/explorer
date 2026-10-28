use crate::agent::mcp::{McpStatus, Notify};
use crate::keymap::Action;
use crate::loader::LoadedFile;
use crate::lsp::{self, LSP_RRT};
use crate::session::save_session;
use crate::tui::pane_component::PaneComponent;
use crate::tui::panes_renderer::PanesRenderer;
use crate::tui::*;
use crate::watcher::{WATCHER_RRT, WatcherWorker};
use arc_swap::ArcSwap;
use camino::{Utf8Path, Utf8PathBuf};
use parking_lot::Mutex;
use r3bl_tui::SubscriberGuard;
use r3bl_tui::core::osc::OscEvent;
use r3bl_tui::core::pty::{
    CursorKeyMode, DefaultPtySessionConfig, PtyInputEvent, PtyOutputEvent, PtySessionBuilder,
    PtySessionConfigOption,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
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
    /// Set when a `:features`/`:all-features`/`:default-features` command dropped
    /// `lsp_guard` to kill the old rust-analyzer; the fresh one is only spawned once
    /// `health_check()` confirms the old worker thread has actually exited (see the
    /// `AppSignal::Noop` tick handler) since resubscribing too early would silently
    /// rejoin the still-running old worker instead of restarting it.
    lsp_restart_pending: bool,
    watcher_guard: Option<SubscriberGuard<WatcherWorker>>,
    /// Counts 1s Noop ticks so the time counters are flushed to disk periodically.
    time_flush_ticks: u64,
    /// Counts 1s Noop ticks so the status-bar dirty flag is rechecked periodically.
    dirty_check_ticks: u64,
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
            lsp_restart_pending: false,
            watcher_guard: None,
            time_flush_ticks: 0,
            dirty_check_ticks: 0,
        })
    }

    fn open_terminal(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        cmd: Option<String>,
        cwd: Option<Utf8PathBuf>,
        pane_size: Option<PaneSize>,
        id: Option<usize>,
        replace_window: Option<Window>,
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

            let mut ofs_buf = r3bl_tui::OfsBufVT100::new_empty(pty_size);
            ofs_buf.terminal_mode.cursor_key_mode = CursorKeyMode::Normal;
            let pty_input_tx = Arc::new(session.tx_input_event.clone());
            let child_killer = session.child_process_termination_handle;
            let initial_title = cmd.as_deref().filter(|s| !s.is_empty()).map(|cmd| {
                let mut lines = cmd.lines();
                let first = lines.next().unwrap_or_default();
                match lines.next() {
                    Some(_) => format!("{first} …"),
                    None => first.to_string(),
                }
            });
            let pane_cwd = cwd.clone().unwrap_or_else(|| self.root.clone());
            let pane_command = if is_command_pane { cmd.clone() } else { None };
            let pane = Arc::new(Mutex::new(TerminalPane {
                ofs_buf,
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
                last_output: Instant::now(),
            }));

            state.terminal_panes.insert(id, Arc::clone(&pane));

            let notify_tx = global_data.main_thread_channel_sender.clone();
            let event_tx = self.terminal_event_tx.clone();
            tokio::spawn(async move {
                let mut last_event = Instant::now();
                let mut backoff: Option<Instant> = None;
                while let Some(event) = session.rx_output_event.recv().await {
                    let is_exit = matches!(&event, PtyOutputEvent::Exit(_));
                    let mut is_buffering_sync = false;
                    match event {
                        PtyOutputEvent::Output(bytes) => {
                            let mut pane = pane.lock();
                            // Recorded even when sync-buffered (the bytes render once
                            // sync releases). This — not the render signal below — is
                            // what keeps the time counters accruing during output: the
                            // signal path goes silent once the main-thread channel
                            // saturates, while the 1s Noop tick keeps polling this.
                            pane.last_output = Instant::now();
                            match pane.ofs_buf.feed_pty_bytes(&bytes) {
                                Some(result) => {
                                    if pane.scroll_offset > 0 {
                                        let true_delta = result
                                            .combined_after
                                            .saturating_sub(result.combined_before)
                                            .saturating_sub(result.evictions);
                                        pane.scroll_offset = pane
                                            .scroll_offset
                                            .saturating_add(true_delta)
                                            .min(pane.ofs_buf.scrollback_len());
                                    }
                                    for osc_event in result.osc_events {
                                        match osc_event {
                                            OscEvent::SetTitleAndTab(title) => {
                                                pane.title = Some(title);
                                            }
                                            // An app in the pane asked to set the
                                            // clipboard via OSC 52. Forward it to the
                                            // real (outer) terminal so the host
                                            // handles the copy. Skip queries
                                            // (Pd == "?"): the host would reply to
                                            // explorer's stdin, which we can't route
                                            // back into the inner PTY.
                                            OscEvent::ClipboardCopy { selection, data }
                                                if data != "?" =>
                                            {
                                                let seq = format!("\x1b]52;{selection};{data}\x07")
                                                    .into_bytes();
                                                let _ = notify_tx.try_send(
                                                    TerminalWindowMainThreadSignal::ApplyAppSignal(
                                                        AppSignal::ForwardOscToTerminal(seq),
                                                    ),
                                                );
                                            }
                                            _ => {}
                                        }
                                    }
                                    for pty_response in result.pty_response_events {
                                        let _ = pane.pty_input_tx.try_send(PtyInputEvent::Write(
                                            pty_response.to_string().into_bytes(),
                                        ));
                                    }
                                }
                                None => {
                                    is_buffering_sync = true;
                                }
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

                    // Synchronized output: skip render signal while the
                    // program has requested atomic screen updates via
                    // DEC private mode 2026.  OfsBufVT100 buffers raw PTY
                    // bytes internally; the first feed_pty_bytes call after
                    // sync-off returns Some(result) and triggers a render.
                    if is_buffering_sync {
                        continue;
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
                        AppSignal::TerminalOutput,
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
                if let Some(old_window) = replace_window {
                    if let Window::FilePreview(file_key) = old_window {
                        state.terminal_to_preview.insert(id, file_key);
                    }
                    state.pane_manager.replace_window(&old_window, window);
                } else {
                    state.pane_manager.push_window(window);
                }
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
    let height = ofs_buf.get_window_size().row_height.as_usize();
    (0..height).all(|r| {
        ofs_buf
            .get_row(r)
            .unwrap_or(&[])
            .iter()
            .all(|pc| !matches!(pc, PixelChar::PlainText { .. }))
    })
}

fn poll_terminal_output(app: &mut AppMain, state: &mut AppState) {
    while let Ok((id, event)) = app.terminal_event_rx.try_recv() {
        if let PtyOutputEvent::Exit(status) = event {
            let exit_code = Some(status.exit_code());
            let exit_signal = status.signal().map(String::from);
            let remove_now = state
                .with_terminal(id, |p| is_buffer_empty(&p.ofs_buf))
                .unwrap_or(false);

            if remove_now {
                // Send focus-lost before removing the pane.
                if let Some(pane) = state.terminal_panes.get(&id) {
                    let p = pane.lock();
                    if p.ofs_buf.terminal_mode.focus_events {
                        let _ = p
                            .pty_input_tx
                            .try_send(PtyInputEvent::Write(b"\x1b[O".to_vec()));
                    }
                }
                let old = state.pane_manager.focused_window;
                if let Some(pane) = state.terminal_panes.remove(&id) {
                    let mut p = pane.lock();
                    if let Some(mut killer) = p.child_killer.take() {
                        let _ = killer.kill();
                    }
                }
                if let Some(file_key) = state.terminal_to_preview.remove(&id) {
                    state
                        .pane_manager
                        .replace_window(&Window::Terminal(id), Window::FilePreview(file_key));
                } else {
                    state.pane_manager.remove_window(&Window::Terminal(id));
                }
                notify_terminal_focus_change(state, old, state.pane_manager.focused_window);
                state.mark_session_dirty();
                sync_terminal_grabbed(state);
            } else {
                state.with_terminal_mut(id, |p| {
                    p.exited = true;
                    p.exit_code = exit_code;
                    p.exit_signal = exit_signal;
                });
            }
        }
    }
}

/// Tells the main thread that the status of an MCP server changed.
pub(crate) fn mcp_notify(
    main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
) -> Notify {
    Arc::new(move |name| {
        let main_tx = main_tx.clone();
        let name = name.to_string();
        tokio::spawn(async move {
            let _ = main_tx
                .send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                    AppSignal::McpStatus { name },
                ))
                .await;
        });
    })
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

        if lsp::disabled() {
            tracing::info!("LSP worker disabled (EXPLORER_NO_LSP)");
        } else {
            match LSP_RRT.try_subscribe(lsp::LspConfig {
                root: root.clone(),
                files: Arc::clone(&files),
                app_tx: notify_tx.clone(),
                cargo_features: global_data.state.cargo_features.clone(),
            }) {
                Ok(guard) => {
                    self.lsp_guard = Some(guard);
                    tracing::info!("LSP worker started");
                }
                Err(e) => tracing::warn!("LSP worker failed to start: {e}"),
            }
        }

        let notify = mcp_notify(notify_tx.clone());
        for decl in &global_data.state.mcp_servers {
            global_data.state.mcp.start(decl, Arc::clone(&notify));
        }

        match WATCHER_RRT.try_subscribe(crate::watcher::WatcherConfig {
            root,
            app_tx: notify_tx.clone(),
            files: Arc::clone(&files),
        }) {
            Ok(guard) => {
                self.watcher_guard = Some(guard);
                tracing::info!("watcher started");
            }
            Err(e) => tracing::warn!("watcher failed to start: {e}"),
        }

        // Enable terminal focus reporting (DECSET ?1004h) so the host terminal emits
        // FocusGained/FocusLost. The r3bl setup doesn't turn this on, and without it the
        // "active" time counter can't pause the instant the window loses focus. Disabled
        // again in `run` on teardown. Safe here: runs before the first paint.
        global_data.output_device.write(|w| {
            let _ = w.write_all(b"\x1b[?1004h");
        });
        let _ = global_data.output_device.flush();

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

        // Run one dirty-status check immediately so the status-bar flag isn't wrong for
        // the first few seconds before the periodic Noop-tick recheck kicks in.
        if let Some(repo) = global_data.state.repo.clone() {
            let main_tx = notify_tx.clone();
            tokio::task::spawn_blocking(move || {
                let result = crate::git::is_dirty(&repo);
                send_signal!(
                    main_tx,
                    TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::DirtyStatusLoaded(
                        result
                    ))
                );
            });
        }

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
            let _ = self.open_terminal(global_data, cmd, cwd, None, Some(id), None);
            if !global_data.state.terminal_panes.contains_key(&id) {
                global_data.state.pane_manager.remove_window(&window);
                global_data.state.mark_session_dirty();
            }
        }

        for id in global_data.state.pending_diffs.drain(..) {
            if global_data.state.commit_diffs.contains_key(&id) {
                continue;
            }
            global_data
                .state
                .commit_diffs
                .insert(id, CommitDiffState::default());
            let main_tx = notify_tx.clone();
            match global_data.state.repo.clone() {
                Some(repo) => {
                    tokio::task::spawn_blocking(move || {
                        let result = crate::git::load_commit_diff(&repo, id)
                            .map(|text| std::sync::Arc::new(crate::git::classify_diff(&text)));
                        send_signal!(
                            main_tx,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::CommitDiffLoaded { id, result }
                            )
                        );
                    });
                }
                None => {
                    send_signal!(
                        main_tx,
                        TerminalWindowMainThreadSignal::ApplyAppSignal(
                            AppSignal::CommitDiffLoaded {
                                id,
                                result: Err("open repository: not a git repository".to_string()),
                            }
                        )
                    );
                }
            }
        }

        for id in global_data.state.pending_git_logs.drain(..) {
            let main_tx = notify_tx.clone();
            let source = global_data
                .state
                .git_logs
                .get(&id)
                .map(|win| win.source.clone())
                .unwrap_or_default();
            crate::tui::git_log_pane::GitLogPaneComponent::reload_log(
                global_data.state.repo.clone(),
                main_tx,
                id,
                &source,
            );
        }

        if let Some(dir) = crate::conversation::conversations_dir(&global_data.state.root) {
            let tree = Arc::clone(&global_data.state.conversations);
            let main_tx = notify_tx.clone();
            tokio::task::spawn_blocking(move || {
                *tree.write() = crate::conversation::ConversationTree::load(&dir);
                send_signal!(
                    main_tx,
                    TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::ConversationsLoaded)
                );
            });
        }

        for endpoint in &global_data.state.agent.endpoints {
            crate::tui::model_picker::fetch_models(endpoint, notify_tx.clone());
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

        // Terminal focus gained/lost pauses/resumes the "active" time counter immediately,
        // rather than waiting for the idle threshold. Focus events carry no other meaning
        // for the app, so consume and re-render (the status-bar counter reflects the pause).
        if let InputEvent::Focus(focus) = input_event {
            global_data
                .state
                .time
                .on_focus(matches!(focus, FocusEvent::Gained));
            return Ok(EventPropagation::ConsumedRender);
        }
        // Any other input event counts as activity for the "active" time counter.
        global_data.state.time.on_input();

        if after_agents::cancel_on_input(&mut global_data.state, &input_event) {
            return Ok(EventPropagation::ConsumedRender);
        }

        // The command line takes every key while it is open, ahead of the leader key.
        if let Some(command_line) = global_data.state.command_line.as_mut() {
            if !matches!(input_event, InputEvent::Mouse(_)) && command_line.input.selecting() {
                command_line.input.cancel_select();
                global_data.state.mouse_drag_active = false;
            }
            match &input_event {
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Enter),
                }) => {
                    run_command_line(global_data);
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Esc),
                }) => {
                    global_data.state.command_line = None;
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::WithModifiers {
                    key: Key::Character('c'),
                    mask:
                        ModifierKeysMask {
                            ctrl_key_state: KeyState::Pressed,
                            shift_key_state: KeyState::NotPressed,
                            alt_key_state: KeyState::NotPressed,
                        },
                }) => {
                    command_line.input.clear();
                    command_line.error = None;
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(KeyPress::WithModifiers {
                    key: Key::Character('d'),
                    mask:
                        ModifierKeysMask {
                            ctrl_key_state: KeyState::Pressed,
                            shift_key_state: KeyState::NotPressed,
                            alt_key_state: KeyState::NotPressed,
                        },
                }) if command_line.input.text().is_empty() => {
                    global_data.state.command_line = None;
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Keyboard(_) | InputEvent::BracketedPaste(_) => {
                    command_line.input.handle_key(&input_event);
                    command_line.error = None;
                    return Ok(EventPropagation::ConsumedRender);
                }
                InputEvent::Mouse(mouse) if command_line.input.handle_mouse(mouse) => {
                    global_data.state.mouse_drag_active = command_line.input.selecting();
                    return Ok(EventPropagation::ConsumedRender);
                }
                // A mouse event the input does not take means its release was lost.
                InputEvent::Mouse(_) if command_line.input.selecting() => {
                    command_line.input.cancel_select();
                    global_data.state.mouse_drag_active = false;
                }
                _ => {}
            }
        }

        // The single KeyPress driving all top-level (leader + global) shortcuts, resolved
        // against the configurable keymap below.
        let keyboard = match input_event {
            InputEvent::Keyboard(kp) => Some(kp),
            _ => None,
        };

        // Leader key activation.
        if let Some(kp) = keyboard
            && kp == global_data.state.keymap.leader_key
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
            match keyboard.and_then(|kp| global_data.state.keymap.leader_action(&kp)) {
                Some(Action::GrabTerminal) => {
                    // Pressing the leader key again grabs the focused terminal. Unlike the
                    // other arms this deliberately does not return: the event still falls
                    // through to the focused component (matching the original behavior).
                    if matches!(
                        global_data.state.pane_manager.focused_window,
                        Some(Window::Terminal(_))
                    ) {
                        global_data.state.terminal_grabbed = true;
                    }
                }
                Some(Action::OpenWindowPicker) => {
                    let state = &mut global_data.state;
                    let old = state.pane_manager.focused_window;
                    state.pane_manager.push_window(Window::WindowPicker);
                    state.pane_manager.focused_window = Some(Window::WindowPicker);
                    notify_terminal_focus_change(state, old, Some(Window::WindowPicker));
                    state.window_picker.selected = None;
                    state.window_picker.query.clear();
                    return Ok(EventPropagation::ConsumedRender);
                }
                Some(Action::OpenTerminal) => {
                    return self.open_terminal(global_data, None, None, None, None, None);
                }
                Some(Action::OpenCommandLine) => {
                    let state = &mut global_data.state;
                    state.command_line = Some(CommandLine::new(state.pane_manager.focused_window));
                    return Ok(EventPropagation::ConsumedRender);
                }
                Some(Action::OpenThemePicker) => {
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
                    state.theme_picker.query.clear();
                    return Ok(EventPropagation::ConsumedRender);
                }
                Some(Action::OpenGitLog) => {
                    let state = &mut global_data.state;
                    let id = state.next_git_log_id;
                    state.next_git_log_id += 1;
                    state.git_logs.insert(id, GitLogWindowState::default());
                    let window = Window::GitLog(id);
                    let old = state.pane_manager.focused_window;
                    state.pane_manager.push_window(window);
                    state.pane_manager.focused_window = Some(window);
                    notify_terminal_focus_change(state, old, Some(window));
                    let main_tx = global_data.main_thread_channel_sender.clone();
                    crate::tui::git_log_pane::GitLogPaneComponent::reload_log(
                        state.repo.clone(),
                        main_tx,
                        id,
                        &LogSource::Head,
                    );
                    return Ok(EventPropagation::ConsumedRender);
                }
                Some(Action::OpenConversations) => {
                    let state = &mut global_data.state;
                    let window = Window::ConversationTree;
                    let old = state.pane_manager.focused_window;
                    state.pane_manager.push_window(window);
                    state.pane_manager.focused_window = Some(window);
                    notify_terminal_focus_change(state, old, Some(window));
                    state.mark_session_dirty();
                    return Ok(EventPropagation::ConsumedRender);
                }
                Some(Action::ToggleFavorite) => {
                    let state = &mut global_data.state;
                    let node = match state.pane_manager.focused_window {
                        Some(Window::Discussion(id)) => state
                            .discussions
                            .get(&id)
                            .filter(|discussion| discussion.agent.is_none())
                            .and_then(|discussion| discussion.node.clone()),
                        Some(Window::ConversationTree) => state
                            .tree_view
                            .selected
                            .as_ref()
                            .and_then(RowTarget::node)
                            .cloned(),
                        _ => None,
                    };
                    if let Some(node) = node
                        && let Err(e) = state.conversations.write().toggle_favorite(&node)
                    {
                        tracing::warn!("failed to save favorite: {e}");
                        let notice = Some(format!("favorite not saved: {e}"));
                        match state.pane_manager.focused_window {
                            Some(Window::Discussion(id)) => {
                                if let Some(discussion) = state.discussions.get_mut(&id) {
                                    discussion.notice = notice;
                                }
                            }
                            _ => state.tree_view.notice = notice,
                        }
                    }
                    return Ok(EventPropagation::ConsumedRender);
                }
                Some(Action::OpenModelPicker) => {
                    if let Some(Window::Discussion(id)) =
                        global_data.state.pane_manager.focused_window
                        && global_data
                            .state
                            .discussions
                            .get(&id)
                            .is_some_and(|discussion| discussion.agent.is_none())
                    {
                        let main_tx = global_data.main_thread_channel_sender.clone();
                        let purpose = ModelPickerPurpose::Window(id);
                        ModelPickerComponent::open(&mut global_data.state, main_tx, purpose);
                    }
                    return Ok(EventPropagation::ConsumedRender);
                }
                Some(Action::Quit) => {
                    return Ok(EventPropagation::ExitMainEventLoop);
                }
                Some(Action::FocusNext) => {
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
                Some(Action::FocusPrev) => {
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
                Some(Action::ClosePane) => {
                    let state = &mut global_data.state;
                    if let Some(old) = state.pane_manager.focused_window {
                        close_window(state, old);
                    }
                    return Ok(EventPropagation::ConsumedRender);
                }
                Some(Action::SendToBack) => {
                    let state = &mut global_data.state;
                    if let Some(focused) = state.pane_manager.focused_window {
                        let old = Some(focused);
                        state.pane_manager.send_to_back(&focused);
                        notify_terminal_focus_change(state, old, state.pane_manager.focused_window);
                        sync_terminal_grabbed(state);
                        state.mark_session_dirty();
                    }
                    return Ok(EventPropagation::ConsumedRender);
                }
                Some(Action::DismissLeader) => {
                    return Ok(EventPropagation::ConsumedRender);
                }
                _ => {}
            }
        }

        // Global shortcuts: Tab/BackTab cycles focus; Ctrl+Up/Down resize; Ctrl+Left/Right reorder.
        // Skipped for Terminal panes so Tab reaches the PTY (e.g. shell completion).
        // When a terminal is ungrabbed Tab cycles focus.
        if (!matches!(
            global_data.state.pane_manager.focused_window,
            Some(Window::Terminal(_))
        ) || !global_data.state.terminal_grabbed)
            && !global_data.state.mouse_drag_active
        {
            match keyboard.and_then(|kp| global_data.state.keymap.global_action(&kp)) {
                Some(Action::FocusNext) => {
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
                Some(Action::FocusPrev) => {
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
                Some(Action::ResizeGrow) => {
                    global_data
                        .state
                        .pane_manager
                        .resize_focused(ResizeDelta::Grow);
                    global_data.state.mark_session_dirty();
                    return Ok(EventPropagation::ConsumedRender);
                }
                Some(Action::ResizeShrink) => {
                    global_data
                        .state
                        .pane_manager
                        .resize_focused(ResizeDelta::Shrink);
                    global_data.state.mark_session_dirty();
                    return Ok(EventPropagation::ConsumedRender);
                }
                Some(Action::MovePaneForward) => {
                    let Some(focused) = global_data.state.pane_manager.focused_window else {
                        return Ok(EventPropagation::ConsumedRender);
                    };
                    global_data.state.pane_manager.move_forward(&focused);
                    global_data.state.mark_session_dirty();
                    return Ok(EventPropagation::ConsumedRender);
                }
                Some(Action::MovePaneBackward) => {
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
                                    .with_terminal(id, |p| p.scroll_offset > 0)
                                    .unwrap_or(false);
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
            return self.open_terminal(global_data, cmd, Some(cwd), None, None, None);
        }
        if let AppSignal::OpenEditor { cmd, cwd, file_key } = action {
            let cmd = cmd.clone();
            let cwd = cwd.clone();
            let key = *file_key;
            return self.open_terminal(
                global_data,
                Some(cmd),
                Some(cwd),
                None,
                None,
                Some(Window::FilePreview(key)),
            );
        }
        if let AppSignal::SetCargoFeatures(selection) = action {
            // rust-analyzer doesn't hot-reload `cargo.*` init options, so applying a
            // new feature selection means killing the current worker and respawning
            // with fresh `initializationOptions`. Dropping the guard here only starts
            // the teardown; the actual resubscribe is deferred to the Noop tick
            // handler once `health_check()` confirms the old worker has exited (see
            // that handler for why resubscribing immediately would be racy).
            global_data.state.cargo_features = selection.clone();
            global_data.state.mark_session_dirty();
            self.lsp_guard = None;
            self.lsp_restart_pending = true;
            return Ok(EventPropagation::ConsumedRender);
        }
        if let AppSignal::ForwardOscToTerminal(bytes) = action {
            // Write the raw sequence (e.g. OSC 52 clipboard copy) to the real terminal.
            // Runs on the main thread, outside paint, so it won't corrupt the TUI; OSC 52
            // is display-neutral (no cursor/screen change).
            global_data.output_device.write(|w| {
                let _ = w.write_all(bytes);
            });
            let _ = global_data.output_device.flush();
            return Ok(EventPropagation::Propagate);
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

                    // Index path -> position once. A rescan (post-overflow)
                    // batch can carry hundreds of thousands of paths; a linear
                    // `snapshot.iter().find()` per path would be quadratic.
                    let index: std::collections::HashMap<&Utf8Path, usize> = snapshot
                        .iter()
                        .enumerate()
                        .map(|(i, f)| (f.path.as_path(), i))
                        .collect();

                    for path in &batch.removed {
                        if let Some(&file_idx) = index.get(path.as_path()) {
                            let file = &snapshot[file_idx];
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
                        match index.get(path.as_path()) {
                            Some(&file_idx)
                                if !snapshot[file_idx].removed.load(Ordering::Relaxed) =>
                            {
                                tracing::debug!(
                                    "FilesChanged: modified match idx={} path={}",
                                    file_idx,
                                    path
                                );
                                snapshot[file_idx].reload();
                                crate::lsp::send_file_request(file_idx);
                            }
                            _ => {
                                tracing::debug!(
                                    "FilesChanged: modified no match for path={}",
                                    path
                                );
                            }
                        }
                    }

                    let mut new_files: Vec<Arc<LoadedFile>> = vec![];
                    for path in &batch.created {
                        if let Some(&file_idx) = index.get(path.as_path()) {
                            let file = &snapshot[file_idx];
                            if file.removed.load(Ordering::Relaxed) {
                                tracing::debug!(
                                    "FilesChanged: created resurrect idx={} path={}",
                                    file_idx,
                                    path
                                );
                                file.removed.store(false, Ordering::Relaxed);
                            } else {
                                tracing::debug!(
                                    "FilesChanged: created replace idx={} path={}",
                                    file_idx,
                                    path
                                );
                            }
                            file.reload();
                            crate::lsp::send_file_request(file_idx);
                        } else if let Some(loaded) =
                            LoadedFile::load(path.clone().into_std_path_buf())
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
                        .contains(&Window::WindowPicker)
                        && !state.window_picker.query.text().is_empty()
                    {
                        WindowPickerComponent::spawn_match(
                            &*state,
                            Arc::clone(&self.picker_generation),
                            self.picker_results_tx.clone(),
                            global_data.main_thread_channel_sender.clone(),
                        );
                    }

                    // Invalidate symbol highlight groups when files change.
                    let mut idx = 0;
                    while idx < state.symbol_highlights.len() {
                        let origin_path = {
                            let group = &state.symbol_highlights[idx];
                            crate::loader::path_for_file_key(&snapshot, group.origin_file)
                                .map(|p| p.to_path_buf())
                        };
                        let Some(ref origin_path) = origin_path else {
                            idx += 1;
                            continue;
                        };

                        let origin_removed = batch
                            .removed
                            .iter()
                            .any(|p| p.as_path() == origin_path.as_path());
                        if origin_removed {
                            state.symbol_highlights.remove(idx);
                            state.mark_session_dirty();
                            continue;
                        }

                        let origin_changed = batch
                            .modified
                            .iter()
                            .any(|p| p.as_path() == origin_path.as_path());
                        if origin_changed {
                            if let Some(g) = state.symbol_highlights.get_mut(idx) {
                                g.needs_rebuild = true;
                                g.origin_byte_start = None;
                                g.origin_byte_end = None;
                                g.locations.clear();
                                crate::lsp::send_symbol_request(
                                    g.origin_file.0,
                                    g.origin_line,
                                    g.origin_char,
                                    g.origin_word.clone(),
                                    Some(idx),
                                );
                            }
                        } else {
                            let stale_paths: Vec<&Utf8Path> = batch
                                .modified
                                .iter()
                                .map(|p| p.as_path())
                                .chain(batch.removed.iter().map(|p| p.as_path()))
                                .collect();
                            if let Some(g) = state.symbol_highlights.get_mut(idx) {
                                g.locations.retain(|loc| {
                                    !stale_paths.iter().any(|p| {
                                        crate::loader::path_for_file_key(&snapshot, loc.file_key)
                                            .is_some_and(|fp| fp.as_str() == p.as_str())
                                    })
                                });
                            }
                        }
                        idx += 1;
                    }

                    state.bump_files_version();
                }
                AppSignal::OpenTerminal { .. } => {}
                AppSignal::ForwardOscToTerminal(_) => {
                    // Handled as early return above; unreachable here.
                }
                AppSignal::OpenEditor { .. } => {
                    // Handled as early return above; unreachable here.
                }
                AppSignal::SetCargoFeatures(_) => {
                    // Handled as early return above; unreachable here.
                }
                AppSignal::SymbolHighlightResult {
                    qualified_name,
                    group_id,
                    origin_file_idx,
                    origin_line,
                    origin_char,
                    origin_word,
                    origin_locations,
                    reference_locations,
                } => {
                    let qualified_name = qualified_name.clone();
                    let group_id = *group_id;
                    let origin_file_idx = *origin_file_idx;
                    let origin_line = *origin_line;
                    let origin_char = *origin_char;
                    let origin_word = origin_word.clone();
                    tracing::debug!(
                        "AppSignal::SymbolHighlightResult: qn={:?} group_id={:?} origin_locs={} ref_locs={}",
                        qualified_name,
                        group_id,
                        origin_locations.len(),
                        reference_locations.len(),
                    );
                    // Rebuild responses locate their group by origin position, not
                    // by the Vec index captured at send time — toggles that land
                    // while the query is in flight shift indices.
                    let target = state.symbol_highlights.iter().position(|g| {
                        g.origin_file.0 == origin_file_idx
                            && g.origin_line == origin_line
                            && g.origin_char == origin_char
                    });
                    if origin_locations.is_empty() && reference_locations.is_empty() {
                        if group_id.is_some()
                            && let Some(idx) = target
                        {
                            state.symbol_highlights.remove(idx);
                            state.mark_session_dirty();
                        }
                    } else if group_id.is_some() {
                        let Some(idx) = target else {
                            // Group was toggled off while the rebuild was in
                            // flight; nothing to update.
                            return Ok(EventPropagation::ConsumedRender);
                        };
                        let group = &mut state.symbol_highlights[idx];
                        group.qualified_name.clone_from(&qualified_name);
                        group.needs_rebuild = false;
                        if let Some(origin) = origin_locations.first() {
                            group.origin_byte_start = Some(origin.start_byte);
                            group.origin_byte_end = Some(origin.end_byte);
                        }
                        group.locations.clear();
                        group.locations.extend(origin_locations.iter().cloned());
                        group.locations.extend(reference_locations.iter().cloned());
                        let qn = group.qualified_name.clone();
                        let mut i = 0;
                        while i < state.symbol_highlights.len() {
                            if i != idx && state.symbol_highlights[i].qualified_name == qn {
                                state.symbol_highlights.remove(i);
                            } else {
                                i += 1;
                            }
                        }
                        state.mark_session_dirty();
                    } else {
                        let origin = origin_locations.first().cloned();
                        let palette = &crate::tui::state::SYMBOL_PALETTE;
                        // Prefer a color no active group is already using. Scan the
                        // whole palette starting at the round-robin cursor; only fall
                        // back to a reused color once every palette entry is taken.
                        let start = state.next_palette_index % palette.len();
                        let color_idx = (0..palette.len())
                            .map(|off| (start + off) % palette.len())
                            .find(|&i| {
                                !state
                                    .symbol_highlights
                                    .iter()
                                    .any(|g| g.color == palette[i])
                            })
                            .unwrap_or(start);
                        state.next_palette_index = (color_idx + 1) % palette.len();
                        let mut locations: Vec<crate::tui::state::SymbolRefLocation> =
                            origin_locations.clone();
                        locations.extend(reference_locations.iter().cloned());
                        state
                            .symbol_highlights
                            .push(crate::tui::state::SymbolHighlightGroup {
                                qualified_name: qualified_name.clone(),
                                origin_file: crate::loader::FileKey(origin_file_idx),
                                origin_line,
                                origin_char,
                                origin_word,
                                origin_byte_start: origin.as_ref().map(|l| l.start_byte),
                                origin_byte_end: origin.as_ref().map(|l| l.end_byte),
                                color: crate::tui::state::SYMBOL_PALETTE[color_idx],
                                locations,
                                needs_rebuild: false,
                            });
                        // Dedup: remove any previous group with the same qualified_name.
                        let qn = qualified_name;
                        let mut i = 0;
                        while i < state.symbol_highlights.len() - 1 {
                            if state.symbol_highlights[i].qualified_name == qn {
                                state.symbol_highlights.remove(i);
                            } else {
                                i += 1;
                            }
                        }
                        state.mark_session_dirty();
                    }
                }
                AppSignal::GitLogLoaded { id, result } => {
                    if let Some(win) = state.git_logs.get_mut(id) {
                        match result {
                            Ok(entries) => {
                                win.entries = Some(entries.clone());
                                win.error = None;
                                let count = win
                                    .entries
                                    .as_ref()
                                    .map_or(0, |history| history.entries.len());
                                if win.selected >= count {
                                    win.selected = count.saturating_sub(1);
                                }
                                crate::tui::git_log_pane::apply_filter(win);
                            }
                            Err(e) => win.error = Some(e.clone()),
                        }
                    }
                }
                AppSignal::GitHistoryChanged => {
                    // Every open log window now holds stale entries; reload each from its own source.
                    let main_tx = global_data.main_thread_channel_sender.clone();
                    for (id, source) in state
                        .git_logs
                        .iter()
                        .map(|(id, win)| (*id, win.source.clone()))
                        .collect::<Vec<_>>()
                    {
                        crate::tui::git_log_pane::GitLogPaneComponent::reload_log(
                            state.repo.clone(),
                            main_tx.clone(),
                            id,
                            &source,
                        );
                    }
                    if let Some(repo) = state.repo.clone() {
                        tokio::task::spawn_blocking(move || {
                            let result = crate::git::load_refs(&repo).map(Arc::new);
                            send_signal!(
                                main_tx,
                                TerminalWindowMainThreadSignal::ApplyAppSignal(
                                    AppSignal::GitRefsLoaded(result)
                                )
                            );
                        });
                    }
                }
                AppSignal::GitRefsLoaded(result) => {
                    if let Ok(refs) = result {
                        state.git_refs = Some(refs.clone());
                    }
                    let picking: Vec<usize> = state
                        .git_logs
                        .iter()
                        .filter(|(_, win)| win.branch_picking)
                        .map(|(id, _)| *id)
                        .collect();
                    for id in picking {
                        crate::tui::git_log_pane::GitLogPaneComponent::
                            refresh_branch_results_for_window(state, id);
                    }
                }
                AppSignal::CommitDiffLoaded { id, result } => {
                    let still_open = state
                        .pane_manager
                        .window_stack
                        .iter()
                        .any(|w| matches!(w, Window::CommitDiff(other) if other == id));
                    if still_open {
                        let entry = state.commit_diffs.entry(*id).or_default();
                        match result {
                            Ok(lines) => {
                                entry.lines = Some(lines.clone());
                                entry.error = None;
                            }
                            Err(e) => entry.error = Some(e.clone()),
                        }
                    }
                }
                AppSignal::ConversationsLoaded => {
                    state.tree_view.loaded = true;
                    let tree = Arc::clone(&state.conversations);
                    let tree = tree.read();
                    state
                        .tree_view
                        .expanded
                        .retain(|id| tree.node(id).is_some());
                    state
                        .discussion_highlights
                        .retain(|id, _| tree.node(id).is_some());
                    // Session-restored discussion windows: load their node, or close
                    // the ones whose node no longer exists.
                    let restored: Vec<(usize, bool)> = state
                        .discussions
                        .iter()
                        .filter_map(|(id, discussion)| {
                            let node = discussion.node.as_ref()?;
                            let exists = match discussion.agent {
                                Some(index) => tree.agent(node, index).is_some(),
                                None => tree.node(node).is_some(),
                            };
                            Some((*id, exists))
                        })
                        .collect();
                    for (id, exists) in restored {
                        if exists {
                            DiscussionComponent::load_content(
                                state,
                                global_data.main_thread_channel_sender.clone(),
                                id,
                            );
                        } else {
                            crate::tui::discussion_pane::drop_queue_entry(state, id);
                            state.discussions.remove(&id);
                            state.pane_manager.remove_window(&Window::Discussion(id));
                            state.mark_session_dirty();
                        }
                    }
                }
                AppSignal::NodeContentLoaded {
                    window_id,
                    node,
                    prompt,
                    reply,
                    work_log,
                } => {
                    if let Some(discussion) = state.discussions.get_mut(window_id)
                        && discussion.node.as_ref() == Some(node)
                    {
                        discussion.prompt = prompt.clone();
                        discussion.reply = reply.clone();
                        discussion.work_log = work_log.clone();
                        discussion.thinking.clear();
                        discussion.loaded = true;
                        if discussion.model.is_none() {
                            let tree = state.conversations.read();
                            discussion.model = match discussion.agent {
                                Some(index) => tree
                                    .agent(node, index)
                                    .and_then(|agent| agent.meta.model_spec()),
                                None => tree
                                    .node(node)
                                    .and_then(crate::tui::discussion_pane::node_model),
                            };
                        }
                    }
                }
                AppSignal::ModelsFetched { endpoint, result } => {
                    if let Err(e) = result {
                        tracing::warn!("model list of '{endpoint}' failed: {e}");
                    }
                    let agent = Arc::clone(&state.agent);
                    state.models.set_list(&agent, endpoint, result.clone());
                    if state
                        .pane_manager
                        .window_stack
                        .contains(&Window::ModelPicker)
                    {
                        let query = state.model_picker.query.text().to_string();
                        state.model_picker.results =
                            crate::tui::model_picker::run_model_match(&query, state);
                    }
                }
                AppSignal::UsageFetched { endpoint, usage } => {
                    state.usage.insert(endpoint.clone(), usage.clone());
                    crate::agent::usage::save(&state.usage);
                }
                AppSignal::TurnProgress => {}
                AppSignal::McpStatus { name } => {
                    let reported = match state.mcp.status(name) {
                        Some(McpStatus::Starting) | None => None,
                        Some(McpStatus::Failed(reason)) => {
                            Some(format!("{name}: failed: {reason}"))
                        }
                        Some(ready) => Some(format!("{name}: {}", ready.label())),
                    };
                    if let Some(notice) = reported
                        && state.tree_view.announced_starts.remove(name)
                    {
                        state.tree_view.notice = Some(notice);
                    }
                }
                AppSignal::SubagentStarted { node } => {
                    state.tree_view.shown_agents.insert(node.clone());
                }
                AppSignal::SubagentFinished { node, index } => {
                    DiscussionComponent::reload_agent_windows(
                        state,
                        global_data.main_thread_channel_sender.clone(),
                        node,
                        Some(*index),
                    );
                }
                AppSignal::TurnFinished { node, outcome } => {
                    if let Some(handle) = state.running_turns.remove(node) {
                        let (reply, work_log) = {
                            let live = handle.live.lock();
                            (live.reply.clone(), live.work_log.clone())
                        };
                        for (id, discussion) in state.discussions.iter_mut() {
                            if discussion.node.as_ref() == Some(node) && discussion.agent.is_none()
                            {
                                discussion.reply.clone_from(&reply);
                                discussion.work_log.clone_from(&work_log);
                                if outcome.status == crate::conversation::NodeStatus::Done {
                                    discussion.thinking.clear();
                                    state
                                        .pane_manager
                                        .set_window_scroll(&Window::Discussion(*id), 0);
                                }
                            }
                        }
                    }
                    let updated = state.conversations.write().update_meta(node, |meta| {
                        meta.status = Some(outcome.status);
                        meta.error = outcome.error.clone();
                        meta.secs = Some(outcome.secs);
                        meta.head_after = outcome.head_after.clone();
                        meta.tokens_in = outcome.tokens_in;
                        meta.cached_in = outcome.cached_in;
                        meta.commits = outcome.commits.clone();
                        meta.scratch_reset = outcome.reset;
                        meta.session_id = outcome.session_id.clone();
                    });
                    if let Err(e) = updated {
                        tracing::warn!("failed to save the outcome of {}: {e}", node.as_str());
                    }
                    if outcome.status == crate::conversation::NodeStatus::Failed {
                        DiscussionComponent::restore_prompt(state, node);
                    }
                    // A turn may have moved HEAD or discarded its work: refresh what shows git state.
                    let main_tx = global_data.main_thread_channel_sender.clone();
                    // A cancel cuts its sub-agent short, without the signal of its end.
                    DiscussionComponent::reload_agent_windows(state, main_tx.clone(), node, None);
                    for (id, win) in &state.git_logs {
                        crate::tui::git_log_pane::GitLogPaneComponent::reload_log(
                            state.repo.clone(),
                            main_tx.clone(),
                            *id,
                            &win.source,
                        );
                    }
                    DiscussionComponent::release_queue(state, main_tx.clone());
                    after_agents::arm(state, main_tx);
                    self.dirty_check_ticks = 4;
                }
                AppSignal::NodeTitleReady { node, title } => {
                    state.conversations.write().set_title(node, title.clone());
                }
                AppSignal::NodeSummaryReady { node, summary } => {
                    state
                        .conversations
                        .write()
                        .set_summary(node, summary.clone());
                }
                AppSignal::DirtyStatusLoaded(result) => {
                    // Errors are transient (e.g. a momentary git-lock during a commit);
                    // leave the previous value rather than surfacing them.
                    if let Ok(dirty) = result {
                        state.is_dirty = *dirty;
                    }
                }
                AppSignal::AfterAgentsExited { run } => after_agents::exited(state, *run),
                AppSignal::Noop | AppSignal::TerminalOutput => {
                    after_agents::tick(state, global_data.main_thread_channel_sender.clone());
                    // Streaming terminal output counts as activity when the option is on,
                    // so a running command keeps the active counter alive without input.
                    // Polled from per-pane `last_output` timestamps rather than tied to
                    // TerminalOutput signal delivery: the render backoff suppresses all
                    // signals during a sustained flood, while this 1s tick (and the
                    // persisted timestamps) keep working through it.
                    if state.count_render_as_active
                        && state.any_terminal_output_recently(Duration::from_secs(2))
                    {
                        state.time.note_render();
                    }
                    // Advance the time counters live (the status bar re-renders each tick).
                    state.time.tick();
                    // Flush the time counters to the session file roughly once a minute so a
                    // crash loses at most ~1 min; the on-exit save captures the final value.
                    self.time_flush_ticks = self.time_flush_ticks.wrapping_add(1);
                    if self.time_flush_ticks.is_multiple_of(60) {
                        state.mark_session_dirty();
                    }
                    // Recheck the working-tree dirty status every ~5s rather than every
                    // render, since it requires walking the index/worktree.
                    self.dirty_check_ticks = self.dirty_check_ticks.wrapping_add(1);
                    if self.dirty_check_ticks.is_multiple_of(5)
                        && let Some(repo) = state.repo.clone()
                    {
                        let main_tx = global_data.main_thread_channel_sender.clone();
                        tokio::task::spawn_blocking(move || {
                            let result = crate::git::is_dirty(&repo);
                            send_signal!(
                                main_tx,
                                TerminalWindowMainThreadSignal::ApplyAppSignal(
                                    AppSignal::DirtyStatusLoaded(result)
                                )
                            );
                        });
                    }
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
                        crate::lsp::LspHealth::NotRunning if crate::lsp::disabled() => {
                            tracing::debug!("LSP health: disabled");
                        }
                        crate::lsp::LspHealth::NotRunning => {
                            tracing::warn!("LSP health: NotRunning");
                        }
                        _ if self.lsp_health_skip.is_multiple_of(10) => {
                            tracing::debug!("LSP health: {:?}", health);
                        }
                        _ => {}
                    }

                    // A `:features`/`:all-features`/`:default-features` command dropped
                    // `lsp_guard` to kill the old rust-analyzer; only resubscribe once the
                    // old worker thread has actually exited (`NotRunning`). Resubscribing
                    // while it's merely `Running` with 0 receivers would take the RRT
                    // framework's FastPath and silently rejoin the still-running old
                    // worker instead of spawning a fresh one with the new init options.
                    if self.lsp_restart_pending
                        && !crate::lsp::disabled()
                        && matches!(health, crate::lsp::LspHealth::NotRunning)
                    {
                        for file in self.files.load().iter() {
                            if file.path.extension() == Some("rs") {
                                file.colored_lines.lock().clear();
                            }
                        }
                        match LSP_RRT.try_subscribe(lsp::LspConfig {
                            root: self.root.clone(),
                            files: Arc::clone(&self.files),
                            app_tx: global_data.main_thread_channel_sender.clone(),
                            cargo_features: state.cargo_features.clone(),
                        }) {
                            Ok(guard) => {
                                self.lsp_guard = Some(guard);
                                tracing::info!(
                                    "LSP worker restarted: cargo_features={:?}",
                                    state.cargo_features
                                );
                                for window in state.pane_manager.window_stack.iter() {
                                    if let Window::FilePreview(key) = window {
                                        crate::lsp::send_file_request(key.0);
                                    }
                                }
                                crate::lsp::try_drain_pending_requests();
                            }
                            Err(e) => tracing::warn!("LSP worker restart failed: {e}"),
                        }
                        self.lsp_restart_pending = false;
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
    ) -> CommonResult {
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
            global_data.state.window_picker.results = results;
        }

        poll_terminal_output(self, &mut global_data.state);

        throws!({
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

            // Build surface and render components. They push ops into global_data.pipeline.
            {
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
            }

            // Take the pipeline (components have already pushed into it) and add
            // background fill and status bar on top.
            let mut pipeline = std::mem::take(&mut global_data.pipeline);

            // Fill entire surface area with pane background (covers padding
            // between panes, which the FlexBox layout system does not fill).
            let bg_rgb = global_data
                .state
                .theme
                .ui_bg("ui.background")
                .unwrap_or(DEFAULT_BG);
            let bg = TuiColor::from(bg_rgb);
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

            if global_data.state.command_line.is_some() {
                render_command_line(&mut pipeline, window_size, &mut global_data.state);
            } else {
                render_status_bar(
                    &mut pipeline,
                    window_size,
                    &global_data.state,
                    &global_data.state.theme,
                );
            }

            global_data.pipeline = pipeline;
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

/// Closes `window` for good: kills a terminal's process and drops the window's state.
pub(super) fn close_window(state: &mut AppState, window: Window) {
    let old = state.pane_manager.focused_window;
    match window {
        Window::Terminal(id) => {
            if let Some(pane) = state.terminal_panes.remove(&id) {
                let mut p = pane.lock();
                if old == Some(window) && p.ofs_buf.terminal_mode.focus_events {
                    let _ = p
                        .pty_input_tx
                        .try_send(PtyInputEvent::Write(b"\x1b[O".to_vec()));
                }
                if let Some(mut killer) = p.child_killer.take() {
                    let _ = killer.kill();
                }
            }
            state.terminal_to_preview.remove(&id);
        }
        Window::GitLog(id) => {
            state.git_logs.remove(&id);
        }
        Window::CommitDiff(id) => {
            state.commit_diffs.remove(&id);
        }
        Window::Discussion(id) => {
            crate::tui::discussion_pane::drop_queue_entry(state, id);
            state.discussions.remove(&id);
        }
        Window::FilePreview(key) => {
            state.preview_inputs.remove(&key);
        }
        Window::ConversationTree => {
            state.tree_view.command = None;
        }
        Window::WindowPicker | Window::ThemePicker | Window::ModelPicker => {}
    }
    state.pane_manager.remove_window(&window);
    notify_terminal_focus_change(state, old, state.pane_manager.focused_window);
    sync_terminal_grabbed(state);
    state.mark_session_dirty();
}

pub(super) fn sync_terminal_grabbed(state: &mut AppState) {
    if !matches!(state.pane_manager.focused_window, Some(Window::Terminal(_))) {
        state.terminal_grabbed = false;
    }
}

pub(super) fn notify_terminal_focus_change(
    state: &AppState,
    old: Option<Window>,
    new: Option<Window>,
) {
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
        let p = pane.lock();
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
    let bg = theme.ui_bg("ui.background").unwrap_or(DEFAULT_BG);
    throws_with_return!({
        let mut styles = tui_stylesheet! {
            new_style!(
                id: {Id::Container}
                color_bg: {TuiColor::from(bg)}
            )
        };
        for col_idx in 0..MAX_PANES {
            let id = FlexBoxId::new(100 + col_idx as u8);
            styles.add_style(new_style!(
                id: {id}
                padding: {0}
                color_bg: {TuiColor::from(bg)}
            ))?;
        }
        for slot in 0..MAX_PANES {
            let id = Id::pane(slot);
            styles.add_style(new_style!(
                id: {id}
                padding: {0}
                color_bg: {TuiColor::from(bg)}
            ))?;
        }
        styles
    })
}

fn render_status_bar(
    pipeline: &mut RenderPipeline,
    size: Size,
    state: &AppState,
    theme: &HelixTheme,
) {
    let bg_rgb = theme
        .ui_bg("ui.statusline")
        .unwrap_or(DEFAULT_STATUSLINE_BG);
    let fg_rgb = theme
        .ui_fg("ui.statusline")
        .unwrap_or(DEFAULT_STATUSLINE_FG);
    let color_bg = TuiColor::from(bg_rgb);
    let color_fg = TuiColor::from(fg_rgb);

    let leader_style = new_style!(bold color_fg: {color_fg} color_bg: {color_bg});
    let normal_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});

    // Status-bar shortcut hints are derived from the live keymap so they reflect any
    // rebindings from config.kdl, rather than hardcoded key letters.
    let leader_lbl = |a: Action| {
        state
            .keymap
            .leader_key_for(a)
            .map(|k| crate::keymap::hint(&k))
            .unwrap_or_default()
    };
    // Two global keys sharing a modifier read as one chord: `Ctrl+↑/↓`, not `Ctrl+↑/Ctrl+↓`.
    let global_pair = |a: Action, b: Action| state.keymap.global_pair(a, b);

    let (leader_text, rest_text) = if state.leader_active {
        (
            " Leader ".to_string(),
            format!(
                "{}:Picker  {}:Term  {}:Cmd  {}:Theme  {}:Git  {}:Chat  {}:Favorite  {}:Model  {}:To back  {}:Close  {}:Quit  {}:Next/Prev  {}:Cancel",
                leader_lbl(Action::OpenWindowPicker),
                leader_lbl(Action::OpenTerminal),
                leader_lbl(Action::OpenCommandLine),
                leader_lbl(Action::OpenThemePicker),
                leader_lbl(Action::OpenGitLog),
                leader_lbl(Action::OpenConversations),
                leader_lbl(Action::ToggleFavorite),
                leader_lbl(Action::OpenModelPicker),
                leader_lbl(Action::SendToBack),
                leader_lbl(Action::ClosePane),
                leader_lbl(Action::Quit),
                state
                    .keymap
                    .leader_pair(Action::FocusNext, Action::FocusPrev),
                leader_lbl(Action::DismissLeader),
            ),
        )
    } else {
        let pane = {
            let rest = match state.pane_manager.focused_window.as_ref() {
                Some(Window::WindowPicker) => {
                    "Esc:Close picker  Enter:Open  Ctrl+X:Close window".to_string()
                }
                Some(Window::ThemePicker) => "Esc:Cancel  Enter:Save".to_string(),
                Some(Window::FilePreview(_)) => {
                    "e:Edit  Bksp:Send to back  ::Command".to_string()
                }
                Some(Window::GitLog(_)) => {
                    "Bksp:Send to back  b:Branch  ↑↓:Select  /:Search  Enter:Diff  Esc:Clear search".to_string()
                }
                Some(Window::CommitDiff(_)) => {
                    "Bksp:Send to back  Esc:Close  ↑↓PgUp/PgDn:scroll".to_string()
                }
                Some(Window::ConversationTree) => {
                    "Bksp:Send to back  /:Search  ::Command  n:New  m:Other model  e:Edit  c:Commits  f:Favorite  s:Sub-agents  Ctrl+D:Delete  Esc:Clear search"
                        .to_string()
                }
                Some(Window::Discussion(_)) => {
                    "Enter:Send  Shift+Enter:New line  PgUp/PgDn:scroll"
                        .to_string()
                }
                Some(Window::ModelPicker) => model_picker_hint(state),
                Some(Window::Terminal(_)) if !state.terminal_grabbed => {
                    "Bksp:Send to back  Enter:grab  ↑↓PgUp/PgDn:scroll".to_string()
                }
                Some(Window::Terminal(_)) | None => String::new(),
            };
            let alert = match state.pane_manager.focused_window {
                Some(Window::ConversationTree) => conversation_tree_alert(state),
                _ => None,
            };
            // Global shortcuts.
            if let Some(alert) = alert {
                alert
            } else if (!matches!(state.pane_manager.focused_window, Some(Window::Terminal(_)))
                || !state.terminal_grabbed)
                && !state.mouse_drag_active
            {
                format!(
                    "{}:Next/Prev  {}:Resize  {}:Move  {rest}",
                    global_pair(Action::FocusNext, Action::FocusPrev),
                    global_pair(Action::ResizeShrink, Action::ResizeGrow),
                    global_pair(Action::MovePaneForward, Action::MovePaneBackward),
                )
            } else {
                rest
            }
        };
        (
            format!(
                " {}: Leader ",
                crate::keymap::hint(&state.keymap.leader_key)
            ),
            pane,
        )
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

    // Right-aligned "dirty" flag (uncommitted staged/unstaged changes), shown in a
    // bright error-like theme color, followed by the usage of the focused conversation's
    // subscription and by the time counters. Each is independently optional: the flag
    // reflects `state.is_dirty` unconditionally, the counters are gated on the
    // `show-timers` config option.
    let error_fg = TuiColor::from(theme.ui_fg("error").unwrap_or(DEFAULT_ERROR_FG));
    let warning_fg = TuiColor::from(theme.ui_fg("warning").unwrap_or(DEFAULT_WARNING_FG));
    let dirty_style = new_style!(bold color_fg: {error_fg} color_bg: {color_bg});
    let warning_style = new_style!(color_fg: {warning_fg} color_bg: {color_bg});
    let error_style = new_style!(color_fg: {error_fg} color_bg: {color_bg});
    let dirty = state.is_dirty.then_some(("│ ", "dirty "));
    let usage = focused_usage(state);
    let after_agents = state
        .after_agents
        .as_ref()
        .map(|after| after_agents::status_text(after, Instant::now()));
    let counters = state.show_timers.then(|| {
        let t = &state.time;
        format!(
            "│ {}/{}  Σ {}/{} ",
            fmt_duration(t.session_active()),
            fmt_duration(t.session_total()),
            fmt_duration(t.project_active()),
            fmt_duration(t.project_total()),
        )
    });
    let clock = {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        clock_text(now, crate::conversation::local_offset_secs(now))
    };
    let width_of = |text: &str| GCStringOwned::from(text).display_width().as_usize();
    let dirty_width = dirty.map_or(0, |(bar, label)| width_of(bar) + width_of(label));
    let usage_width: usize = usage.iter().map(|(text, _)| width_of(text)).sum();
    let after_width = after_agents.as_ref().map_or(0, |(text, _)| width_of(text));
    let counters_width = counters.as_deref().map(width_of).unwrap_or(0);
    let clock_width = width_of(&clock);
    let total_width = size.col_width.as_usize();
    // Usage goes first, then after-agents: a narrow terminal keeps the flag, counters and clock.
    let base_width = dirty_width + counters_width + clock_width;
    let usage_fits = total_width > base_width + after_width + usage_width + 2;
    let after_fits = total_width > base_width + after_width + 2;
    let combined_width = base_width
        + if after_fits { after_width } else { 0 }
        + if usage_fits { usage_width } else { 0 };
    // Only draw when it fits alongside the left hints (leave a small gap), so narrow
    // terminals keep the shortcut hints rather than being overwritten.
    if combined_width > 0 && total_width > combined_width + 2 {
        let start_col = total_width - combined_width - 1;
        render_ops += RenderOpCommon::MoveCursorPositionAbs(col(start_col as u16) + row_idx);
        render_ops += RenderOpIR::PaintTextWithAttributes(" ".into(), Some(normal_style));
        if let Some((bar, label)) = dirty {
            render_ops += RenderOpIR::PaintTextWithAttributes(bar.into(), Some(normal_style));
            render_ops += RenderOpIR::PaintTextWithAttributes(label.into(), Some(dirty_style));
        }
        if after_fits && let Some((text, counting_down)) = after_agents {
            let style = if counting_down {
                warning_style
            } else {
                normal_style
            };
            render_ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
        }
        if usage_fits {
            for (text, percent) in usage {
                let style = match percent {
                    Some(USAGE_ERROR_PERCENT..) => error_style,
                    Some(USAGE_WARNING_PERCENT..) => warning_style,
                    _ => normal_style,
                };
                render_ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
            }
        }
        if let Some(counters) = counters {
            render_ops += RenderOpIR::PaintTextWithAttributes(counters.into(), Some(normal_style));
        }
        render_ops += RenderOpIR::PaintTextWithAttributes(clock.into(), Some(normal_style));
    }

    pipeline.push(ZOrder::Normal, render_ops);
}

/// The status-bar clock: local time of day, 24h, as a `│ HH:MM ` segment.
fn clock_text(now: i64, offset_secs: i32) -> String {
    let minutes = (now + i64::from(offset_secs)).rem_euclid(86_400) / 60;
    format!("│ {:02}:{:02} ", minutes / 60, minutes % 60)
}

/// Percent of a subscription window from which the status bar warns about it.
const USAGE_WARNING_PERCENT: u32 = 75;
const USAGE_ERROR_PERCENT: u32 = 90;

/// The endpoint the focused window spends: the model of a Discussion window, the
/// endpoint of the Conversation Tree's selected node or sub-agent run (for a queued
/// row, the model of the window holding the prompt), the selected row of the Model
/// Picker.
fn focused_endpoint(state: &AppState) -> Option<String> {
    let window_endpoint =
        |id: &usize| Some(state.discussions.get(id)?.model.as_ref()?.endpoint.clone());
    match state.pane_manager.focused_window.as_ref()? {
        Window::Discussion(id) => window_endpoint(id),
        Window::ConversationTree => match state.tree_view.selected.as_ref()? {
            RowTarget::Node(node) => state.conversations.read().node(node)?.meta.endpoint.clone(),
            RowTarget::Agent { node, index } => state
                .conversations
                .read()
                .agent(node, *index)?
                .meta
                .endpoint
                .clone(),
            RowTarget::Queued { window, .. } => window_endpoint(window),
        },
        Window::ModelPicker => {
            let picker = &state.model_picker;
            let selected = picker
                .selected
                .or_else(|| picker.results.first().map(|(idx, _)| *idx))?;
            Some(state.models.specs.get(selected)?.endpoint.clone())
        }
        _ => None,
    }
}

/// The status-bar entry for the subscription behind the focused window's endpoint;
/// empty when that endpoint has no subscription or its usage is not known yet.
fn focused_usage(state: &AppState) -> Vec<(String, Option<u32>)> {
    let entry = || {
        let endpoint = focused_endpoint(state)?;
        let label = state.agent.endpoint(&endpoint)?.kind.usage_label()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs() as i64);
        let usage = state.usage.get(&endpoint)?;
        Some(usage.pieces(label, now, crate::conversation::local_offset_secs))
    };
    entry().unwrap_or_default()
}

fn model_picker_hint(state: &AppState) -> String {
    let errors = state.models.errors();
    if !errors.is_empty() {
        return errors.join("  ");
    }
    if state.agent.endpoints.is_empty() {
        return "No endpoint: declare one in config.kdl and its token in auth.kdl".to_string();
    }
    "Esc:Cancel  Enter:Select".to_string()
}

/// A message that must not be cut off by the regular hints: a failure, or a pending
/// delete confirmation.
fn conversation_tree_alert(state: &AppState) -> Option<String> {
    if let Some(notice) = &state.tree_view.notice {
        return Some(notice.clone());
    }
    let id = state.tree_view.armed_delete.as_ref()?;
    let count = state.conversations.read().subtree_len(id);
    Some(format!(
        "Ctrl+D again: delete {count} node(s)  any other key: Cancel"
    ))
}

/// Formats a duration compactly for the status bar: `5s`, `12m`, `1h20m`, `3h`.
fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        if m > 0 {
            format!("{h}h{m}m")
        } else {
            format!("{h}h")
        }
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

pub fn build_state(
    files: Arc<ArcSwap<Vec<Arc<LoadedFile>>>>,
    root: Utf8PathBuf,
    theme: crate::tui::theme::HelixTheme,
    keymap: crate::keymap::Keymap,
) -> AppState {
    AppState::new(files, root, theme, keymap)
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

    // Disable terminal focus reporting enabled in `app_start_background_services`.
    global_data.output_device.write(|w| {
        let _ = w.write_all(b"\x1b[?1004l");
    });
    let _ = global_data.output_device.flush();

    // Stop running turns: keep what they produced. One with a reply or commits is
    // cancelled, part of the discussion; one without has nothing to show and fails.
    for (node, handle) in &global_data.state.running_turns {
        handle.cancel();
        let mut tree = global_data.state.conversations.write();
        if let Some(dir) = tree.node_dir(node) {
            let reply = handle.live.lock().reply.clone();
            let _ = crate::conversation::write_atomic(&dir.join("reply.md"), &reply);
            let status = if crate::conversation::has_content(&reply) {
                crate::conversation::NodeStatus::Cancelled
            } else {
                crate::conversation::NodeStatus::Failed
            };
            let _ = tree.update_meta(node, |meta| {
                meta.status = Some(status);
                if status == crate::conversation::NodeStatus::Failed {
                    meta.error = Some(crate::conversation::CANCELLED_ERROR.to_string());
                }
            });
        }
    }

    // Kill all terminal children gracefully.
    for pane in global_data.state.terminal_panes.values() {
        let mut pane = pane.lock();
        if let Some(mut killer) = pane.child_killer.take() {
            let _ = killer.kill(); // Sends SIGHUP.
        }
        let _ = pane.pty_input_tx.try_send(PtyInputEvent::Close);
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

#[cfg(test)]
mod tests {
    use super::clock_text;

    #[test]
    fn clock_text_is_local_24h() {
        assert_eq!(clock_text(0, 0), "│ 00:00 ");
        assert_eq!(clock_text(86_399, 0), "│ 23:59 ");
        assert_eq!(clock_text(0, 7_200), "│ 02:00 ");
        assert_eq!(clock_text(0, -3_600), "│ 23:00 ");
    }
}
