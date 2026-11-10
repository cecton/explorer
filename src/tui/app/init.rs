#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

pub(super) fn components(
    app: &crate::tui::app::AppMain,
    component_registry_map: &mut ComponentRegistryMap<
        crate::tui::state::AppState,
        crate::signal::AppSignal,
    >,
    has_focus: &mut HasFocus,
) {
    use crate::tui::pane_component::PaneComponent;
    use std::sync::Arc;
    for slot in 0..crate::tui::pane_manager::MAX_PANES {
        let pane_id = crate::tui::pane_component::pane_id(slot);
        if let ContainsResult::DoesNotContain =
            ComponentRegistry::contains(component_registry_map, pane_id)
        {
            ComponentRegistry::put(
                component_registry_map,
                pane_id,
                PaneComponent::new_boxed(
                    slot,
                    pane_id,
                    app.picker_results_tx.clone(),
                    Arc::clone(&app.picker_generation),
                ),
            );
        }
    }

    if has_focus.get_id().is_none() {
        has_focus.set_id(crate::tui::pane_component::pane_id(0));
    }
}
pub(super) fn background_services(
    app: &mut crate::tui::app::AppMain,
    global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
) {
    use crate::conversation::ConversationTree;
    use crate::lsp::LspConfig;
    use crate::signal::AppSignal;
    use crate::tui::pane_manager::Window;
    use crate::tui::panes::git_log_pane::GitLogPaneComponent;
    use crate::tui::state::CommitDiffState;
    use crate::watcher::WatcherConfig;
    use std::sync::Arc;
    use std::time::Duration;
    let notify_tx = global_data.main_thread_channel_sender.clone();

    // Publish the channel sender so the SIGTERM handler can request a clean exit.
    let _ = app.exit_tx.set(notify_tx.clone());
    let files = Arc::clone(&app.files);
    let root = app.root.clone();

    if crate::lsp::disabled() {
        tracing::info!("LSP worker disabled (EXPLORER_NO_LSP)");
    } else {
        match crate::lsp::LSP_RRT.try_subscribe(LspConfig {
            root: root.clone(),
            files: Arc::clone(&files),
            app_tx: notify_tx.clone(),
            cargo_features: global_data.state.cargo_features.clone(),
        }) {
            Ok(guard) => {
                app.lsp_guard = Some(guard);
                tracing::info!("LSP worker started");
            }
            Err(e) => tracing::warn!("LSP worker failed to start: {e}"),
        }
    }

    let notify = crate::agent::mcp::notify(notify_tx.clone());
    for decl in &global_data.state.mcp_servers {
        global_data.state.mcp.start(decl, Arc::clone(&notify));
    }

    match crate::watcher::WATCHER_RRT.try_subscribe(WatcherConfig {
        root,
        app_tx: notify_tx.clone(),
        files: Arc::clone(&files),
    }) {
        Ok(guard) => {
            app.watcher_guard = Some(guard);
            tracing::info!("watcher started");
        }
        Err(e) => tracing::warn!("watcher failed to start: {e}"),
    }

    // Each period gets its own signal. Only `Tick` and `TurnTick` (while a turn runs)
    // may repaint, so timers that fire together cost at most two renders. The first
    // tick of each interval is immediate: the dirty flag and the OpenRouter spending
    // are loaded at startup.
    crate::tui::app::tick::every(&notify_tx, Duration::from_secs(1), AppSignal::Tick);
    crate::tui::app::tick::every(&notify_tx, Duration::from_secs(5), AppSignal::Housekeeping);
    crate::tui::app::tick::every(&notify_tx, Duration::from_secs(60), AppSignal::UsagePollDue);
    crate::tui::app::tick::every(
        &notify_tx,
        crate::agent::turn::SPINNER_INTERVAL,
        AppSignal::TurnTick,
    );

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
        let _ = app.open_terminal(global_data, cmd, cwd, None, Some(id), None);
        if !global_data.state.terminal_panes.contains_key(&id) {
            crate::tui::focus::close_terminal(&mut global_data.state, id);
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
                    crate::signal::send(&main_tx, AppSignal::CommitDiffLoaded { id, result });
                });
            }
            None => {
                crate::signal::send(
                    &main_tx,
                    AppSignal::CommitDiffLoaded {
                        id,
                        result: Err("open repository: not a git repository".to_string()),
                    },
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
        GitLogPaneComponent::reload_log(global_data.state.repo.clone(), main_tx, id, &source);
    }

    if let Some(dir) = crate::conversation::conversations_dir(&global_data.state.root) {
        let tree = Arc::clone(&global_data.state.conversations);
        let main_tx = notify_tx.clone();
        tokio::task::spawn_blocking(move || {
            *tree.write() = ConversationTree::load(&dir);
            crate::signal::send(&main_tx, AppSignal::ConversationsLoaded);
        });
    }

    for endpoint in &global_data.state.agent.endpoints {
        crate::tui::panes::pickers::model_picker::fetch_models(endpoint, notify_tx.clone());
    }

    // Request full tokens for session-restored file previews.
    for window in global_data.state.pane_manager.window_stack.iter() {
        if let Window::FilePreview(key) = window {
            crate::lsp::send_file_request(key.0);
        }
    }
    crate::lsp::try_drain_pending_requests();
}
