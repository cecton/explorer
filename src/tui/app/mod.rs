#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, OnceLock};

mod init;
mod input;
mod render;
mod signals;
mod status_bar;
mod tick;

pub struct AppMain {
    files: Arc<arc_swap::ArcSwap<Vec<Arc<crate::loader::LoadedFile>>>>,
    root: camino::Utf8PathBuf,
    picker_results_tx: tokio::sync::mpsc::Sender<crate::tui::modal::window_picker::PickerResultMsg>,
    picker_results_rx:
        tokio::sync::mpsc::Receiver<crate::tui::modal::window_picker::PickerResultMsg>,
    picker_generation: Arc<AtomicU64>,
    exit_tx: Arc<OnceLock<crate::signal::MainTx>>,
    terminal_event_tx: tokio::sync::mpsc::UnboundedSender<(usize, PtyOutputEvent)>,
    terminal_event_rx: tokio::sync::mpsc::UnboundedReceiver<(usize, PtyOutputEvent)>,
    lsp_guard: Option<SubscriberGuard<crate::lsp::worker::LspWorker>>,
    lsp_last_gen: u8,
    /// Counts `Housekeeping` calls so the LSP health is logged only every few of them.
    lsp_health_skip: u32,
    watcher_guard: Option<SubscriberGuard<crate::watcher::WatcherWorker>>,
    /// Local minute (since the epoch) the status-bar clock was last painted at.
    last_clock_minute: i64,
    /// The modal lists' painters, kept so a frame builds no matcher.
    modal_painters: crate::tui::modal::list::ModalPainters,
}

impl AppMain {
    fn new_boxed(
        files: Arc<arc_swap::ArcSwap<Vec<Arc<crate::loader::LoadedFile>>>>,
        root: camino::Utf8PathBuf,
        exit_tx: Arc<OnceLock<crate::signal::MainTx>>,
    ) -> BoxedSafeApp<crate::tui::state::AppState, crate::signal::AppSignal> {
        use crate::tui::modal::list::ModalPainters;
        let (picker_results_tx, picker_results_rx) = tokio::sync::mpsc::channel(32);
        let (terminal_event_tx, terminal_event_rx) = tokio::sync::mpsc::unbounded_channel();
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
            last_clock_minute: crate::tui::app::tick::clock_minute(),
            modal_painters: ModalPainters::new(),
        })
    }

    /// Logs the LSP worker's health: on a change of generation, and otherwise every
    /// [`crate::tui::app::tick::LSP_HEALTH_LOG_EVERY`] calls so a steady state does not fill
    /// the log.
    fn log_lsp_health(&mut self) {
        use crate::lsp::LspHealth;
        let health = crate::lsp::health_check();
        let generation = match &health {
            LspHealth::Running { generation, .. } => *generation,
            LspHealth::NotRunning => 0,
        };
        self.lsp_health_skip = self.lsp_health_skip.wrapping_add(1);
        if generation != self.lsp_last_gen {
            tracing::info!("LSP health: {:?}", health);
            self.lsp_last_gen = generation;
        }
        match &health {
            LspHealth::Running {
                input_receivers: 0, ..
            } if self
                .lsp_health_skip
                .is_multiple_of(crate::tui::app::tick::LSP_HEALTH_LOG_EVERY) =>
            {
                tracing::warn!("LSP health: {:?}", health);
            }
            LspHealth::NotRunning if crate::lsp::disabled() => {
                tracing::debug!("LSP health: disabled");
            }
            LspHealth::NotRunning => {
                tracing::warn!("LSP health: NotRunning");
            }
            _ if self
                .lsp_health_skip
                .is_multiple_of(crate::tui::app::tick::LSP_HEALTH_LOG_EVERY) =>
            {
                tracing::debug!("LSP health: {:?}", health);
            }
            _ => {}
        }
    }
}

impl App for AppMain {
    type S = crate::tui::state::AppState;
    type AS = crate::signal::AppSignal;

    fn app_init_components(
        &mut self,
        component_registry_map: &mut ComponentRegistryMap<Self::S, Self::AS>,
        has_focus: &mut HasFocus,
    ) {
        crate::tui::app::init::components(self, component_registry_map, has_focus);
    }

    fn app_start_background_services(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
    ) {
        crate::tui::app::init::background_services(self, global_data);
    }

    fn app_handle_input_event(
        &mut self,
        input_event: InputEvent,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<
            crate::tui::state::AppState,
            crate::signal::AppSignal,
        >,
        has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        crate::tui::app::input::handle(
            self,
            input_event,
            global_data,
            component_registry_map,
            has_focus,
        )
    }

    fn app_handle_signal(
        &mut self,
        action: &crate::signal::AppSignal,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        _component_registry_map: &mut ComponentRegistryMap<
            crate::tui::state::AppState,
            crate::signal::AppSignal,
        >,
        has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        crate::tui::app::signals::handle(self, action, global_data, has_focus)
    }

    fn app_render(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<
            crate::tui::state::AppState,
            crate::signal::AppSignal,
        >,
        has_focus: &mut HasFocus,
    ) -> CommonResult {
        crate::tui::app::render::render(self, global_data, component_registry_map, has_focus)
    }
}

/// Returns the `FlexBoxId` for the pane slot that corresponds to the focused window.
pub(super) fn focused_pane_id(state: &crate::tui::state::AppState) -> FlexBoxId {
    let Some(slot) = state.pane_manager.focused_slot() else {
        return crate::tui::pane_component::pane_id(0);
    };
    crate::tui::pane_component::pane_id(slot)
}

fn sync_has_focus(state: &crate::tui::state::AppState, has_focus: &mut HasFocus) {
    has_focus.set_id(focused_pane_id(state));
}

fn surface_size(window_size: VPSize) -> VPSize {
    let col_count = window_size.col_width;
    let row_count = window_size.row_height - vp_height(1);
    col_count + row_count
}

/// Stops the turns still running, keeping what they produced: a turn with a reply or
/// commits is cancelled, part of the discussion, and one with neither fails, both
/// through the rule a turn that ends on its own goes through, so a commit turn's
/// commits count. Its sub-agent runs end with it: left running they would load as
/// "interrupted" and lose the duration a cancelled one records.
fn stop_running_turns(state: &crate::tui::state::AppState) {
    use crate::agent::Mode;
    let head_now = state.repo.as_ref().and_then(crate::git::head_id);
    let now_secs = crate::util::time::now_secs();
    for (node, handle) in &state.running_turns {
        handle.cancel();
        let reply = handle.live.lock().reply.clone();
        let commits = match handle.mode {
            Mode::Commit => crate::git::commits_since(
                &state.root,
                handle.head_before.as_deref(),
                head_now.as_deref(),
            ),
            Mode::Feedback => Vec::new(),
        };
        let mut tree = state.conversations.write();
        if let Err(e) = tree.mark_cut_short(node, &reply, &commits, head_now.as_deref()) {
            tracing::warn!(
                "failed to record the turn of {} cut short at exit: {e}",
                node.as_str()
            );
        }
        tree.cancel_running_agents(node, now_secs);
    }
}

pub async fn run(
    initial_state: crate::tui::state::AppState,
    files: Arc<arc_swap::ArcSwap<Vec<Arc<crate::loader::LoadedFile>>>>,
    root: camino::Utf8PathBuf,
) -> CommonResult<()> {
    let exit_tx: Arc<OnceLock<crate::signal::MainTx>> = Arc::new(OnceLock::new());
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

    // Nothing to restore means the first key of the run has no window to land on: the window
    // picker opens over the empty frame, as the panes it would list are none.
    let mut initial_state = initial_state;
    if initial_state.pane_manager.window_stack.is_empty() {
        crate::tui::modal::window_picker::open(&mut initial_state);
    }

    let app = AppMain::new_boxed(files, root, exit_tx);
    let exit_keys: &[InputEvent] = &[];
    let (global_data, _, _): (GlobalData<_, _>, _, _) =
        match TerminalWindow::main_event_loop(app, exit_keys, initial_state) {
            TuiAvailability::Available(future) => future.await?,
            it => return it.into_err(),
        };

    stop_running_turns(&global_data.state);

    // Kill all terminal children gracefully.
    for pane in global_data.state.terminal_panes.values() {
        let mut pane = pane.lock();
        if let Some(mut killer) = pane.child_killer.take() {
            let _ = killer.kill(); // Sends SIGHUP.
        }
        let _ = pane.pty_input_tx.try_send(PtyInputEvent::Close);
    }
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    if let Err(e) = crate::tui::session::save_session(&global_data.state.root, &global_data.state) {
        tracing::warn!("Failed to save session on exit: {e}");
    }

    if let Some(msg) = exit_message.get() {
        eprintln!("{msg}");
    }
    ok!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_a_turn_at_exit_ends_its_running_sub_agents_with_it() {
        use crate::agent::Mode;
        use crate::agent::turn::TurnHandle;
        use crate::conversation::ConversationTree;
        use crate::conversation::NodeStatus;
        use crate::conversation::files;
        use crate::conversation::metadata::Metadata;
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let mut tree = ConversationTree::load(&root);
        let node = tree
            .create_node(None, 86_401, Metadata::default(), "p", None)
            .unwrap();
        let running = Metadata {
            status: Some(NodeStatus::Running),
            ..Default::default()
        };
        tree.create_agent(&node, 86_402, running, "task").unwrap();
        let run_dir = tree.agent_dir(&node, 1).unwrap();
        files::write_reply(&run_dir, "a partial answer").unwrap();

        let mut state = crate::tui::state::AppState::for_test(root);
        *state.conversations.write() = tree;
        state.running_turns.insert(
            node.clone(),
            TurnHandle::for_test(Mode::Commit, "half an answer"),
        );

        let before = crate::util::time::now_secs();
        stop_running_turns(&state);
        let after = crate::util::time::now_secs();

        let tree = state.conversations.read();
        let cut_short = &tree.node(&node).unwrap().meta;
        assert_eq!(cut_short.status, Some(NodeStatus::Cancelled));
        assert_eq!(cut_short.commits, Vec::<String>::new());
        let run = tree.agent(&node, 1).unwrap();
        assert_eq!(run.meta.status, Some(NodeStatus::Cancelled));
        assert!(run.meta.error.is_none());
        // A run cut short records its duration from its creation stamp; left running it
        // would reload as "interrupted" with none. The clock the function reads sits
        // between these two.
        let secs = run.meta.secs.expect("a cancelled run records its duration");
        assert!(
            (before - 86_402..=after - 86_402).contains(&(secs as i64)),
            "{secs} is not between {} and {}",
            before - 86_402,
            after - 86_402
        );
        assert_eq!(files::read_reply(&run_dir), "a partial answer");
    }
}
