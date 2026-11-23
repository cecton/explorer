#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

pub(super) fn handle(
    app: &mut crate::tui::app::AppMain,
    action: &crate::signal::AppSignal,
    global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
    has_focus: &mut HasFocus,
) -> CommonResult<EventPropagation> {
    use crate::agent::mcp::McpStatus;
    use crate::signal::AppSignal;
    use crate::tui::after_agents::AfterAgents;
    use crate::tui::modal::Modal;
    use crate::tui::modal::window_picker;
    use crate::tui::pane_manager::Window;
    use crate::tui::panes::discussion::DiscussionComponent;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    crate::tui::app::sync_has_focus(&global_data.state, has_focus);

    throws_with_return!({
        // These arms never touch `state`, so they may hand the whole `global_data` on.
        let state = &mut global_data.state;
        let mut propagation = EventPropagation::ConsumedRender;
        match action {
            AppSignal::OpenTerminal { cmd, cwd } => {
                return crate::tui::panes::terminal::spawn::open(
                    global_data,
                    crate::tui::panes::terminal::spawn::Request {
                        cmd: cmd.clone(),
                        cwd: Some(cwd.clone()),
                        pane_size: None,
                        id: None,
                        replace_window: None,
                        event_tx: &app.terminal_event_tx,
                    },
                );
            }
            AppSignal::OpenEditor { cmd, cwd, file_key } => {
                return crate::tui::panes::terminal::spawn::open(
                    global_data,
                    crate::tui::panes::terminal::spawn::Request {
                        cmd: Some(cmd.clone()),
                        cwd: Some(cwd.clone()),
                        pane_size: None,
                        id: None,
                        replace_window: Some(Window::FilePreview(*file_key)),
                        event_tx: &app.terminal_event_tx,
                    },
                );
            }
            AppSignal::SetCargoFeatures(selection) => {
                state.cargo_features = selection.clone();
                state.mark_session_dirty();
                crate::lsp::set_cargo_features(selection.clone());
                // The restart drops the colors of every file, and the new server holds none of
                // them open: each preview of the window stack asks again, queued behind the
                // restart it must follow, and the warmup colors the rest.
                for window in state.pane_manager.window_stack.iter() {
                    if let Window::FilePreview(key) = window {
                        crate::lsp::send_file_request(key.0);
                    }
                }
            }
            AppSignal::FilesChanged(changes) => {
                // The reads already happened on the watcher thread; this is the one turn that
                // publishes them, and it invalidates the offsets they make stale right after.
                let touched = crate::loader::watch::swap_in(&app.files, changes);
                tracing::info!(
                    "FilesChanged: {} modified, {} created, {} removed",
                    touched.modified.len(),
                    touched.created.len(),
                    touched.removed.len()
                );
                if matches!(state.modal, Some(Modal::WindowPicker))
                    && !state.window_picker.query.text().is_empty()
                {
                    window_picker::spawn_match(
                        &*state,
                        Arc::clone(&app.picker_generation),
                        app.picker_results_tx.clone(),
                        global_data.main_thread_channel_sender.clone(),
                    );
                }
                crate::tui::symbols::files_changed(state, &touched);
            }
            AppSignal::SymbolHighlight(answer) => {
                tracing::debug!(
                    "AppSignal::SymbolHighlight: qn={:?} group_id={:?} origin_locs={} ref_locs={}",
                    answer.qualified_name,
                    answer.group_id,
                    answer.origin_locations.len(),
                    answer.reference_locations.len(),
                );
                crate::tui::symbols::answered(state, answer);
            }
            AppSignal::GitLogLoaded { id, result } => {
                crate::tui::panes::git_log_pane::log_loaded(state, *id, result);
            }
            AppSignal::GitHistoryChanged => {
                crate::tui::panes::git_log_pane::history_changed(
                    state,
                    global_data.main_thread_channel_sender.clone(),
                );
            }
            AppSignal::GitRefsLoaded(result) => {
                crate::tui::panes::git_log_pane::refs_loaded(state, result);
            }
            AppSignal::CommitDiffLoaded { id, result } => {
                crate::tui::panes::commit_diff_pane::diff_loaded(state, *id, result);
            }
            AppSignal::ConversationsLoaded => {
                crate::tui::panes::conversation_tree::conversations_loaded(
                    state,
                    global_data.main_thread_channel_sender.clone(),
                );
            }
            AppSignal::NodeContentLoaded {
                window_id,
                node,
                prompt,
                reply,
                work_log,
            } => {
                crate::tui::panes::discussion::content_loaded(
                    state, *window_id, node, prompt, reply, work_log,
                );
            }
            AppSignal::ModelsFetched { endpoint, result } => {
                if let Err(e) = result {
                    tracing::warn!("model list of '{endpoint}' failed: {e}");
                }
                let agent = Arc::clone(&state.agent);
                state.models.set_list(&agent, endpoint, result.clone());
                if matches!(state.modal, Some(Modal::ModelPicker(_))) {
                    let query = state.model_picker.query.text().to_string();
                    state.model_picker.results =
                        crate::tui::modal::model_picker::run_model_match(&query, state);
                }
            }
            AppSignal::UsageFetched { endpoint, usage } => {
                state.usage.insert(endpoint.clone(), usage.clone());
                crate::usage::save(&state.usage);
            }
            AppSignal::TurnTick => {
                state.spinner_tick =
                    (state.spinner_tick + 1) % crate::agent::turn::SPINNER.len() as u8;
                let visible = state.pane_manager.visible();
                if !tick_repaints(!state.running_turns.is_empty(), visible) {
                    propagation = EventPropagation::Consumed;
                }
            }
            AppSignal::McpStatus { name } => {
                let reported = match state.mcp.status(name) {
                    Some(McpStatus::Starting) | None => None,
                    Some(McpStatus::Failed(reason)) => Some(format!("{name}: failed: {reason}")),
                    Some(ready) => Some(format!("{name}: {}", ready.label())),
                };
                if let Some(notice) = reported
                    && state.tree_view.announced_starts.remove(name)
                {
                    state.tree_view.notice = Some(notice);
                }
            }
            AppSignal::TurnFinished { node, outcome } => {
                DiscussionComponent::turn_finished(
                    state,
                    global_data.main_thread_channel_sender.clone(),
                    node,
                    outcome,
                );
            }
            AppSignal::NodeTitleReady { node, title } => {
                state.conversations.write().set_title(node, title.clone());
            }
            AppSignal::NodeRetitleEnded { node } => {
                state.tree_view.retitling.remove(node);
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
                match result {
                    Ok(dirty) if state.is_dirty != *dirty => state.is_dirty = *dirty,
                    // Unchanged, or an error that leaves the flag as it was: no repaint.
                    _ => propagation = EventPropagation::Consumed,
                }
            }
            AppSignal::AfterAgentsExited { run } => crate::tui::after_agents::exited(state, *run),
            AppSignal::Redraw | AppSignal::TerminalOutput => {}
            AppSignal::Tick => {
                // A countdown shows its seconds, and a firing or reverting one
                // changes the status text: all repaint.
                let countdown = matches!(state.after_agents, Some(AfterAgents::Countdown { .. }));
                crate::tui::after_agents::tick(
                    state,
                    global_data.main_thread_channel_sender.clone(),
                );
                let minute = crate::tui::app::tick::clock_minute();
                let minute_changed =
                    std::mem::replace(&mut app.last_clock_minute, minute) != minute;
                // Panes whose output no signal announced: swap each flag, no
                // short-circuit, so every flag is cleared.
                let flushed = state.terminal_panes.values().fold(false, |any, pane| {
                    let pending = pane.lock().flush_pending.swap(false, Ordering::Relaxed);
                    any | pending
                });
                if !(countdown || minute_changed || flushed) {
                    propagation = EventPropagation::Consumed;
                }
            }
            AppSignal::Housekeeping => {
                crate::tui::app::tick::maybe_save_session(state);
                state.spawn_dirty_check(global_data.main_thread_channel_sender.clone());
                app.log_lsp_health();
                propagation = EventPropagation::Consumed;
            }
            AppSignal::UsagePollDue => {
                // Their counters settle seconds after the requests that spent them.
                crate::tui::app::status_bar::poll_openrouter_usage(
                    state,
                    global_data.main_thread_channel_sender.clone(),
                );
                propagation = EventPropagation::Consumed;
            }
        }

        propagation
    });
}

/// Whether a `TurnTick` changes anything on screen: only a running turn animates, and
/// only the conversation tree and a Discussion show it.
fn tick_repaints(turn_running: bool, visible: &[crate::tui::pane_manager::PaneSlot]) -> bool {
    use crate::tui::pane_manager::Window;
    turn_running
        && visible.iter().any(|slot| {
            matches!(
                slot.window,
                Window::ConversationTree | Window::Discussion(_)
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::pane_manager::{PaneSlot, Window};

    fn slot(window: Window) -> PaneSlot {
        PaneSlot {
            slot: 0,
            window,
            box_: FlexBox::default(),
        }
    }

    #[test]
    fn a_tick_repaints_only_for_a_running_turn_shown_by_the_tree_or_a_discussion() {
        assert!(tick_repaints(true, &[slot(Window::Discussion(1))]));
        assert!(tick_repaints(
            true,
            &[slot(Window::GitLog(1)), slot(Window::ConversationTree)]
        ));
        assert!(!tick_repaints(
            true,
            &[slot(Window::GitLog(1)), slot(Window::Terminal(2))]
        ));
        assert!(!tick_repaints(true, &[]));
        assert!(!tick_repaints(false, &[slot(Window::Discussion(1))]));
    }
}
