/// Forgets the prompt queued by `window`, when it has one.
pub(crate) fn drop_queue_entry(state: &mut crate::tui::state::AppState, window: usize) {
    state.queued_turns.retain(|entry| entry.window != window);
}

/// Takes back the head-most prompt queued below `parent`, if any: it returns to the
/// draft of the window that queued it, and the queue moves up.
pub(crate) fn take_back(
    state: &mut crate::tui::state::AppState,
    main_tx: crate::signal::MainTx,
    parent: &crate::conversation::NodeId,
) {
    let Some(entry) = state
        .queued_turns
        .iter()
        .find(|entry| entry.parent.as_ref() == Some(parent))
        .cloned()
    else {
        return;
    };
    drop_queue_entry(state, entry.window);
    crate::tui::panes::discussion::DiscussionComponent::release_queue(state, main_tx);
}

/// Takes back the prompt window `window` queued, if any. Its text stays the draft of
/// that window. A window on a new conversation queues with no parent, so its entry is
/// found by window, not by the node it will follow.
pub(crate) fn take_back_window(
    state: &mut crate::tui::state::AppState,
    main_tx: crate::signal::MainTx,
    window: usize,
) {
    drop_queue_entry(state, window);
    crate::tui::panes::discussion::DiscussionComponent::release_queue(state, main_tx);
}

pub(crate) fn ordinal(rank: usize) -> String {
    let suffix = match (rank % 10, rank % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{rank}{suffix}")
}

/// Status line part of a window whose prompt is queued: its place and the keys that
/// act on it.
pub(crate) fn queue_hint(state: &crate::tui::state::AppState, window: usize) -> Option<String> {
    let index = state
        .queued_turns
        .iter()
        .position(|entry| entry.window == window)?;
    Some(format!(
        "queued · {} · Ctrl+Plus/Minus:Priority · Esc:Take back",
        ordinal(index + 1)
    ))
}

/// Moves the prompt queued by `window` one place earlier or later.
pub(crate) fn move_queued(state: &mut crate::tui::state::AppState, window: usize, earlier: bool) {
    let queue = &mut state.queued_turns;
    let Some(index) = queue.iter().position(|entry| entry.window == window) else {
        return;
    };
    let target = if earlier {
        index.checked_sub(1)
    } else {
        Some(index + 1).filter(|next| *next < queue.len())
    };
    if let Some(target) = target {
        queue.swap(index, target);
    }
}

impl crate::tui::panes::discussion::DiscussionComponent {
    /// Gives the messages the user sent that the turn never read back to the windows
    /// showing `node`, under whatever they hold now.
    pub(crate) fn restore_injected(
        state: &mut crate::tui::state::AppState,
        node: &crate::conversation::NodeId,
        handle: &crate::agent::turn::TurnHandle,
    ) {
        let pending = handle.live.lock().take_injected();
        if pending.is_empty() {
            return;
        }
        let shown: Vec<usize> = state
            .discussions
            .iter()
            .filter(|(_, discussion)| {
                discussion.node.as_ref() == Some(node) && discussion.agent.is_none()
            })
            .map(|(id, _)| *id)
            .collect();
        let Some(&first) = shown.first() else {
            return;
        };
        if let Some(discussion) = state.discussions.get_mut(&first) {
            let mut text = discussion.draft.text().to_string();
            for content in &pending {
                if !text.trim().is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(content);
            }
            discussion.draft.set_text(text);
        }
        state.mark_session_dirty();
    }

    /// Starts the queued prompts, next first, for as long as no turn is running. Each is
    /// composed from its window as it stands now. A prompt that can no longer be sent
    /// leaves the queue with a notice; a dirty working tree holds the whole queue back.
    pub(crate) fn release_queue(
        state: &mut crate::tui::state::AppState,
        main_tx: crate::signal::MainTx,
    ) {
        use crate::tui::panes::discussion::launch::Refusal;
        while state.running_turns.is_empty() {
            let Some(entry) = state.queued_turns.first().cloned() else {
                return;
            };
            let id = entry.window;
            let Some(discussion) = state.discussions.get_mut(&id) else {
                state.queued_turns.remove(0);
                continue;
            };
            if discussion.node != entry.parent {
                discussion.notice =
                    Some("this window moved on since the prompt was queued".to_string());
                state.queued_turns.remove(0);
                continue;
            }
            let Some(composed) = crate::tui::panes::discussion::launch::compose_launch(state, id)
            else {
                state.queued_turns.remove(0);
                continue;
            };
            let started = composed.and_then(|launch| {
                let prompt = launch.prompt.clone();
                crate::tui::panes::discussion::launch::start_turn(state, main_tx.clone(), launch)
                    .map(|node| (node, prompt))
            });
            let refusal = match started {
                Ok((node, prompt)) => {
                    state.queued_turns.remove(0);
                    crate::tui::panes::discussion::DiscussionComponent::apply_sent(
                        state, id, node, prompt,
                    );
                    continue;
                }
                Err(refusal) => refusal,
            };
            let held = matches!(refusal, Refusal::Busy(_) | Refusal::DirtyTree);
            let reason = refusal.into_reason();
            if !held {
                state.queued_turns.remove(0);
            }
            if let Some(discussion) = state.discussions.get_mut(&id) {
                discussion.notice = Some(if held {
                    format!("queue held: {reason} · Enter:Retry")
                } else {
                    reason
                });
            }
            if held {
                return;
            }
        }
    }
}
