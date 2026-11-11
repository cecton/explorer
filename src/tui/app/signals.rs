#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

pub(super) fn handle(
    app: &mut crate::tui::app::AppMain,
    action: &crate::signal::AppSignal,
    global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
    has_focus: &mut HasFocus,
) -> CommonResult<EventPropagation> {
    use crate::agent::mcp::McpStatus;
    use crate::conversation::NodeStatus;
    use crate::loader::FileKey;
    use crate::signal::AppSignal;
    use crate::signal::SymbolRefLocation;
    use crate::tui::after_agents::AfterAgents;
    use crate::tui::pane_manager::Window;
    use crate::tui::panes::discussion::DiscussionComponent;
    use crate::tui::panes::git_log_pane::GitLogPaneComponent;
    use crate::tui::panes::pickers::window_picker::WindowPickerComponent;
    use crate::tui::state::SymbolHighlightGroup;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    crate::tui::app::sync_has_focus(&global_data.state, has_focus);

    if let AppSignal::OpenTerminal { cmd, cwd } = action {
        let cmd = cmd.clone();
        let cwd = cwd.clone();
        return crate::tui::panes::terminal::spawn::open(
            global_data,
            crate::tui::panes::terminal::spawn::Request {
                cmd,
                cwd: Some(cwd),
                pane_size: None,
                id: None,
                replace_window: None,
                event_tx: &app.terminal_event_tx,
            },
        );
    }
    if let AppSignal::OpenEditor { cmd, cwd, file_key } = action {
        let cmd = cmd.clone();
        let cwd = cwd.clone();
        let key = *file_key;
        return crate::tui::panes::terminal::spawn::open(
            global_data,
            crate::tui::panes::terminal::spawn::Request {
                cmd: Some(cmd),
                cwd: Some(cwd),
                pane_size: None,
                id: None,
                replace_window: Some(Window::FilePreview(key)),
                event_tx: &app.terminal_event_tx,
            },
        );
    }
    if let AppSignal::SetCargoFeatures(selection) = action {
        global_data.state.cargo_features = selection.clone();
        global_data.state.mark_session_dirty();
        crate::lsp::set_cargo_features(selection.clone());
        return Ok(EventPropagation::ConsumedRender);
    }
    throws_with_return!({
        let state = &mut global_data.state;
        let mut propagation = EventPropagation::ConsumedRender;
        match action {
            AppSignal::FilesChanged(batch) => {
                tracing::info!(
                    "FilesChanged: {} modified, {} created, {} removed",
                    batch.modified.len(),
                    batch.created.len(),
                    batch.removed.len()
                );
                crate::loader::watch::apply(&app.files, batch);
                if state
                    .pane_manager
                    .window_stack
                    .contains(&Window::WindowPicker)
                    && !state.window_picker.query.text().is_empty()
                {
                    WindowPickerComponent::spawn_match(
                        &*state,
                        Arc::clone(&app.picker_generation),
                        app.picker_results_tx.clone(),
                        global_data.main_thread_channel_sender.clone(),
                    );
                }
                crate::tui::symbols::files_changed(state, batch);
                state.bump_files_version();
            }
            AppSignal::OpenTerminal { .. } => {}
            AppSignal::OpenEditor { .. } => {
                // Handled as early return above; unreachable here.
            }
            AppSignal::SetCargoFeatures(_) => {
                // Handled as early return above; unreachable here.
            }
            AppSignal::SymbolHighlight(highlight) => {
                let qualified_name = highlight.qualified_name.clone();
                let group_id = highlight.group_id;
                let origin_file_idx = highlight.origin.0;
                let origin_line = highlight.origin_line;
                let origin_char = highlight.origin_char;
                let origin_word = highlight.origin_word.clone();
                let origin_locations = &highlight.origin_locations;
                let reference_locations = &highlight.reference_locations;
                tracing::debug!(
                    "AppSignal::SymbolHighlight: qn={:?} group_id={:?} origin_locs={} ref_locs={}",
                    qualified_name,
                    group_id,
                    origin_locations.len(),
                    reference_locations.len(),
                );
                let target = crate::tui::symbols::find(
                    &state.symbol_highlights,
                    FileKey(origin_file_idx),
                    origin_line,
                    origin_char,
                );
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
                    crate::tui::symbols::dedup(&mut state.symbol_highlights, &qn, Some(idx));
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
                    let mut locations: Vec<SymbolRefLocation> = origin_locations.clone();
                    locations.extend(reference_locations.iter().cloned());
                    state.symbol_highlights.push(SymbolHighlightGroup {
                        qualified_name: qualified_name.clone(),
                        origin_file: FileKey(origin_file_idx),
                        origin_line,
                        origin_char,
                        origin_word,
                        origin_byte_start: origin.as_ref().map(|l| l.start_byte),
                        origin_byte_end: origin.as_ref().map(|l| l.end_byte),
                        color: crate::tui::state::SYMBOL_PALETTE[color_idx],
                        locations,
                        needs_rebuild: false,
                    });
                    let keep = state.symbol_highlights.len() - 1;
                    let qn = qualified_name;
                    crate::tui::symbols::dedup(&mut state.symbol_highlights, &qn, Some(keep));
                    state.mark_session_dirty();
                }
            }
            AppSignal::GitLogLoaded { id, result } => {
                if let Some(win) = state.git_logs.get_mut(id) {
                    match result {
                        Ok(entries) => {
                            win.entries = Some(std::sync::Arc::clone(entries));
                            win.error = None;
                            let count = win
                                .entries
                                .as_ref()
                                .map_or(0, |history| history.entries.len());
                            if win.selected >= count {
                                win.selected = count.saturating_sub(1);
                            }
                            crate::tui::panes::git_log_pane::apply_filter(win);
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
                    GitLogPaneComponent::reload_log(
                        state.repo.clone(),
                        main_tx.clone(),
                        id,
                        &source,
                    );
                }
                if let Some(repo) = state.repo.clone() {
                    tokio::task::spawn_blocking(move || {
                        let result = crate::git::load_refs(&repo).map(Arc::new);
                        crate::signal::send(&main_tx, AppSignal::GitRefsLoaded(result));
                    });
                }
            }
            AppSignal::GitRefsLoaded(result) => {
                if let Ok(refs) = result {
                    state.git_refs = Some(std::sync::Arc::clone(refs));
                }
                let picking: Vec<usize> = state
                    .git_logs
                    .iter()
                    .filter(|(_, win)| win.branch_picking)
                    .map(|(id, _)| *id)
                    .collect();
                for id in picking {
                    GitLogPaneComponent::refresh_branch_results_for_window(state, id);
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
                            entry.lines = Some(std::sync::Arc::clone(lines));
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
                        crate::tui::focus::close_window(state, Window::Discussion(id));
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
                    discussion.show_content(prompt.clone(), reply.clone(), work_log.clone());
                    discussion.loaded = true;
                    if discussion.model.is_none() {
                        let tree = state.conversations.read();
                        discussion.model = match discussion.agent {
                            Some(index) => tree
                                .agent(node, index)
                                .and_then(|agent| agent.meta.model_spec()),
                            None => tree
                                .node(node)
                                .and_then(crate::tui::panes::discussion::launch::node_model),
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
                        crate::tui::panes::pickers::model_picker::run_model_match(&query, state);
                }
            }
            AppSignal::UsageFetched { endpoint, usage } => {
                state.usage.insert(endpoint.clone(), usage.clone());
                crate::usage::save(&state.usage);
            }
            AppSignal::TurnTick => {
                state.spinner_tick =
                    (state.spinner_tick + 1) % crate::agent::turn::SPINNER.len() as u8;
                if state.running_turns.is_empty() {
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
                let finished = state.running_turns.remove(node);
                if let Some(handle) = &finished {
                    let (reply, work_log) = {
                        let live = handle.live.lock();
                        (live.reply.clone(), live.work_log.clone())
                    };
                    for (id, discussion) in state.discussions.iter_mut() {
                        if discussion.node.as_ref() == Some(node) && discussion.agent.is_none() {
                            discussion.show_finished_turn(&reply, &work_log);
                            if outcome.status == NodeStatus::Done {
                                discussion.thinking.clear();
                                state
                                    .pane_manager
                                    .set_window_scroll(&Window::Discussion(*id), 0);
                            }
                        }
                    }
                }
                let updated = state
                    .conversations
                    .write()
                    .update_meta(node, |meta| meta.record_outcome(outcome));
                if let Err(e) = updated {
                    tracing::warn!("failed to save the outcome of {}: {e}", node.as_str());
                }
                // A failed turn does not hand its prompt back: the input stays as the
                // user left it, and a message sent there resumes the turn.
                //
                // What the user sent that the turn never read goes back to the input
                // rather than vanishing with it.
                if let Some(handle) = &finished {
                    DiscussionComponent::restore_injected(state, node, handle);
                }
                // A turn may have moved HEAD or discarded its work: refresh what shows git state.
                let main_tx = global_data.main_thread_channel_sender.clone();
                // A cancel cuts its sub-agent short, without the signal of its end.
                DiscussionComponent::reload_agent_windows(state, main_tx.clone(), node, None);
                for (id, win) in &state.git_logs {
                    GitLogPaneComponent::reload_log(
                        state.repo.clone(),
                        main_tx.clone(),
                        *id,
                        &win.source,
                    );
                }
                DiscussionComponent::release_queue(state, main_tx.clone());
                crate::tui::after_agents::arm(state, main_tx);
                crate::tui::app::tick::spawn_dirty_check(
                    state,
                    global_data.main_thread_channel_sender.clone(),
                );
            }
            AppSignal::NodeTitleReady { node, title } => {
                state.conversations.write().set_title(node, title.clone());
            }
            AppSignal::NodeRetitleEnded { node } => {
                state.tree_view.retitling.remove(node);
            }
            AppSignal::AgentTitleReady { node, index, title } => {
                state
                    .conversations
                    .write()
                    .set_agent_title(node, *index, title.clone());
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
                crate::tui::app::tick::maybe_save_session(state, &app.root);
                crate::tui::app::tick::spawn_dirty_check(
                    state,
                    global_data.main_thread_channel_sender.clone(),
                );
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
