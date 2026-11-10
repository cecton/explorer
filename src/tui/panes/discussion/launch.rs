use std::sync::Arc;
/// What a turn is asked to do.
pub(crate) enum TurnKind {
    /// A prompt run in a mode: feedback, or commit for the built-in `/commit`.
    Prompt(crate::agent::Mode),
    /// A custom command sent without the mode's system prompt: nothing tells its turn
    /// that the tree is a lab, so its work is kept and closed like a commit turn.
    NoSystemPrompt,
    /// The built-in `/compact`: summarize the branch so far.
    Compact { instructions: String },
    /// The built-in `/continue`: resume the turn of an interrupted node in the mode it
    /// ran, replaying its transcript and adding a note of how it ended.
    Continue(crate::agent::Mode),
}

/// A turn to start: a new node under `parent`, or a new root.
pub(crate) struct Launch {
    pub parent: Option<crate::conversation::NodeId>,
    /// Text stored as the node's `prompt.md`.
    pub prompt: String,
    /// The prompt as typed, when a slash command expanded it into `prompt`.
    pub input: Option<String>,
    pub kind: TurnKind,
    /// Custom slash command the prompt was expanded from.
    pub command: Option<String>,
    pub spec: crate::model_spec::ModelSpec,
}

impl Launch {
    /// The turn of an existing node, to run again as it was sent: same prompt, mode and
    /// command. `prompt` is the node's `prompt.md`.
    pub(crate) fn replay(
        tree: &crate::conversation::ConversationTree,
        node: &crate::conversation::NodeId,
        prompt: String,
        spec: crate::model_spec::ModelSpec,
    ) -> Option<Self> {
        use crate::agent::commands::Expansion;
        use crate::conversation::NodeType;
        let source = tree.node(node)?;
        let kind = match source.meta.node_type {
            Some(NodeType::Compact) => match crate::agent::commands::expand(&prompt, &[]) {
                Expansion::Compact { instructions } => TurnKind::Compact { instructions },
                Expansion::Prompt { .. }
                | Expansion::Commit { .. }
                | Expansion::Continue { .. } => TurnKind::Compact {
                    instructions: String::new(),
                },
            },
            _ if source.meta.no_system_prompt => TurnKind::NoSystemPrompt,
            _ => TurnKind::Prompt(node_mode(source)),
        };
        Some(Self {
            parent: source.parent.clone(),
            prompt,
            input: None,
            kind,
            command: source.meta.command.clone(),
            spec,
        })
    }
}

/// Why a turn did not start.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// A turn is running: the prompt can start once it has ended.
    Busy(&'static str),
    /// The working tree has uncommitted changes.
    DirtyTree,
    /// The repository is not trusted yet: no agent runs in it before the user says so.
    Untrusted,
    Rejected(String),
}

impl Refusal {
    pub(crate) fn into_reason(self) -> String {
        match self {
            Self::Busy(reason) => reason.to_string(),
            Self::DirtyTree => {
                "the working tree has uncommitted changes; commit or stash them first".to_string()
            }
            Self::Untrusted => {
                "this repository is not trusted yet: run scan, read its report, then \
                 trust on the command line to allow agents here"
                    .to_string()
            }
            Self::Rejected(reason) => reason,
        }
    }
}

/// Composes the draft of window `id`, starts its turn and points the window at the new
/// node: `None` when there is nothing to send, `Ok(())` once the turn runs, the refusal
/// when it cannot. A refused window is left as it was.
pub(crate) fn launch_window(
    state: &mut crate::tui::state::AppState,
    main_tx: crate::signal::MainTx,
    id: usize,
) -> Option<Result<(), Refusal>> {
    let launch = match compose_launch(state, id)? {
        Ok(launch) => launch,
        Err(refusal) => return Some(Err(refusal)),
    };
    let prompt = launch.prompt.clone();
    match start_turn(state, main_tx, launch) {
        Ok(node) => {
            crate::tui::panes::discussion::DiscussionComponent::apply_sent(state, id, node, prompt);
            Some(Ok(()))
        }
        Err(refusal) => Some(Err(refusal)),
    }
}

/// The model a node was run with, when its metadata names one completely.
pub(crate) fn node_model(node: &crate::conversation::Node) -> Option<crate::model_spec::ModelSpec> {
    node.meta.model_spec()
}

/// Whether the node shown by Discussion window `id` has a turn running.
pub(crate) fn window_is_running(state: &crate::tui::state::AppState, id: usize) -> bool {
    state
        .discussions
        .get(&id)
        .and_then(|discussion| discussion.node.as_ref())
        .is_some_and(|node| state.running_turns.contains_key(node))
}

/// The sub-agent run window `discussion` shows, while it runs.
pub(crate) fn running_agent(
    state: &crate::tui::state::AppState,
    discussion: &crate::tui::state::DiscussionState,
) -> Option<crate::agent::turn::live::LiveSubagent> {
    let index = discussion.agent?;
    let handle = state.running_turns.get(discussion.node.as_ref()?)?;
    let running = handle.live.lock().subagent.clone()?;
    (running.index == index).then_some(running)
}

/// What the turn or the sub-agent run window `discussion` shows has produced so far,
/// while it runs.
pub(crate) fn running_output(
    state: &crate::tui::state::AppState,
    discussion: &crate::tui::state::DiscussionState,
) -> Option<Arc<parking_lot::Mutex<crate::agent::turn::live::LiveTurn>>> {
    match discussion.agent {
        Some(_) => running_agent(state, discussion).map(|running| running.live),
        None => {
            let handle = state.running_turns.get(discussion.node.as_ref()?)?;
            Some(Arc::clone(&handle.live))
        }
    }
}

/// Metadata of the node or the sub-agent run window `discussion` shows.
pub(crate) fn shown_meta<'a>(
    tree: &'a crate::conversation::ConversationTree,
    discussion: &crate::tui::state::DiscussionState,
) -> Option<&'a crate::conversation::metadata::Metadata> {
    let node = discussion.node.as_ref()?;
    match discussion.agent {
        Some(index) => tree.agent(node, index).map(|agent| &agent.meta),
        None => tree.node(node).map(|node| &node.meta),
    }
}

/// The mode a node was run in; a compact node counts as feedback.
pub(crate) fn node_mode(node: &crate::conversation::Node) -> crate::agent::Mode {
    use crate::agent::Mode;
    use crate::conversation::NodeType;
    match node.meta.node_type {
        Some(NodeType::Commit) => Mode::Commit,
        _ => Mode::Feedback,
    }
}

/// Why a prompt in `mode` cannot start while `running` turns are in progress: turns
/// run one at a time, since a feedback turn experiments freely in the working tree
/// and a commit turn must have it clean and to itself.
pub(crate) fn lock_refusal(running: &[crate::agent::Mode]) -> Option<&'static str> {
    (!running.is_empty()).then_some("another turn is running; wait for it to finish")
}

/// How the turn of `node` ended, for the note a resume sends: `None` when it ran to
/// completion, since nothing was interrupted then.
fn interrupted_of(node: &crate::conversation::Node) -> Option<String> {
    use crate::conversation::NodeStatus;
    match node.meta.status {
        Some(NodeStatus::Failed) => Some(
            node.meta
                .error
                .clone()
                .unwrap_or_else(|| "the turn ended without an error message".to_string()),
        ),
        Some(NodeStatus::Cancelled) => Some(crate::conversation::CANCELLED_ERROR.to_string()),
        _ => None,
    }
}

/// What a resume of `node` sends: how its turn ended, the mode it ran in, and whether
/// its uncommitted work was discarded.
fn resume_of(node: &crate::conversation::Node) -> (Option<String>, crate::agent::Mode, bool) {
    (
        interrupted_of(node),
        node_mode(node),
        node.meta.scratch_reset,
    )
}

/// Whether a plain message on the node window `id` shows resumes it, the way
/// `/continue` does: its turn stopped without finishing, so the reasoning on disk is
/// what the message builds on. A compact node is left out — it has no work to resume,
/// and a resume would replay the summary request and read a summary that is not there.
pub(crate) fn resumes_on_message(
    state: &crate::tui::state::AppState,
    discussion: &crate::tui::state::DiscussionState,
) -> bool {
    use crate::conversation::NodeStatus;
    use crate::conversation::NodeType;
    discussion.node.as_ref().is_some_and(|node| {
        state.conversations.read().node(node).is_some_and(|node| {
            matches!(
                node.meta.status,
                Some(NodeStatus::Failed | NodeStatus::Cancelled)
            ) && node.meta.node_type != Some(NodeType::Compact)
        })
    })
}

/// The turn the draft of window `id` asks for, as the window stands now: a new node
/// below the node shown, or a sibling of it when its turn failed. A cancelled node
/// cut short with something to show, so follow-ups build on it. `None` without a
/// draft or a model; a refusal when the draft cannot make a turn.
pub(crate) fn compose_launch(
    state: &crate::tui::state::AppState,
    id: usize,
) -> Option<Result<Launch, Refusal>> {
    use crate::agent::Mode;
    use crate::agent::commands::Command;
    use crate::agent::commands::Expansion;
    use crate::conversation::Node;
    use crate::conversation::NodeStatus;
    use crate::conversation::NodeType;
    use crate::conversation::metadata::Metadata;
    let discussion = state.discussions.get(&id)?;
    let typed = discussion.draft.text().trim().to_string();
    if typed.is_empty() {
        return None;
    }
    let expansion = crate::agent::commands::expand(&typed, &state.agent.commands);
    // A message sent to a node whose turn stopped for any reason resumes that turn, the
    // way `/continue` does, rather than starting a sibling that knows nothing of it: the
    // reasoning is on disk, and this is what builds on it. A slash command keeps its own
    // meaning, since it says what to run rather than that the turn goes on.
    let expansion = match expansion {
        Expansion::Prompt { command: None, .. } if resumes_on_message(state, discussion) => {
            Expansion::Continue {
                instructions: typed.clone(),
            }
        }
        other => other,
    };
    let spec = discussion.model.clone()?;
    let continue_on_node = expansion.matches_command(crate::agent::commands::CONTINUE);
    let parent = {
        let tree = state.conversations.read();
        match discussion.node.as_ref().and_then(|node| tree.node(node)) {
            Some(Node {
                parent,
                meta:
                    Metadata {
                        status: Some(NodeStatus::Failed),
                        ..
                    },
                ..
            }) if !continue_on_node => parent.clone(),
            _ => discussion.node.clone(),
        }
    };
    let (prompt, input, kind, command) = match expansion {
        Expansion::Compact { instructions } => {
            (typed, None, TurnKind::Compact { instructions }, None)
        }
        Expansion::Continue { instructions } => {
            let Some(node) = &discussion.node else {
                return Some(Err(Refusal::Rejected(
                    "/continue has nothing to resume in a new conversation".to_string(),
                )));
            };
            let (interrupted, mode, discarded) = {
                let tree = state.conversations.read();
                let Some(resumed) = tree.node(node) else {
                    return Some(Err(Refusal::Rejected(
                        "the node to resume is gone".to_string(),
                    )));
                };
                if resumed.meta.node_type == Some(NodeType::Compact) {
                    return Some(Err(Refusal::Rejected(
                        "a compact turn has no work to resume; send the message again".to_string(),
                    )));
                }
                resume_of(resumed)
            };
            (
                crate::agent::context::continue_request(
                    interrupted.as_deref(),
                    discarded,
                    &instructions,
                ),
                Some(typed),
                TurnKind::Continue(mode),
                None,
            )
        }
        Expansion::Commit { instructions } if instructions.is_empty() && parent.is_none() => {
            return Some(Err(Refusal::Rejected(
                "/commit has nothing to go ahead with in a new conversation".to_string(),
            )));
        }
        Expansion::Commit { instructions } => (
            crate::agent::commands::commit_prompt(&instructions),
            Some(typed),
            TurnKind::Prompt(Mode::Commit),
            None,
        ),
        Expansion::Prompt { text, command } => {
            let kind = match command {
                Some(Command {
                    system_prompt: false,
                    ..
                }) => TurnKind::NoSystemPrompt,
                _ => TurnKind::Prompt(Mode::Feedback),
            };
            let input = command.is_some().then_some(typed);
            (text, input, kind, command.map(|c| c.name.clone()))
        }
    };
    Some(Ok(Launch {
        parent,
        prompt,
        input,
        kind,
        command,
        spec,
    }))
}

/// Checks the guards, creates the node on disk and starts its turn. On refusal nothing
/// is created and the reason is returned.
pub(crate) fn start_turn(
    state: &mut crate::tui::state::AppState,
    main_tx: crate::signal::MainTx,
    launch: Launch,
) -> Result<crate::conversation::NodeId, Refusal> {
    use crate::agent::Mode;
    use crate::agent::turn::TurnRequest;
    use crate::conversation::NodeStatus;
    use crate::conversation::NodeType;
    use crate::conversation::metadata::Metadata;
    use crate::tui::state::RowTarget;
    let Launch {
        parent,
        prompt,
        input,
        kind,
        command,
        spec,
    } = launch;
    // A resumed commit turn is closed as a commit but gets no mode prompt, like a custom
    // command with `system-prompt: off`; a resumed feedback turn keeps its prompt.
    let no_system_prompt = matches!(
        kind,
        TurnKind::NoSystemPrompt | TurnKind::Continue(Mode::Commit)
    );
    let continuing = matches!(kind, TurnKind::Continue(_));
    // Nothing an agent says or does runs in a repository the user has not allowed it in,
    // and the scan is how they come to know whether they should.
    if !state.trusted {
        return Err(Refusal::Untrusted);
    }
    // A compact turn changes nothing, so it runs under the shared lock like feedback.
    let (mode, node_type, compact) = match kind {
        TurnKind::Prompt(Mode::Feedback) | TurnKind::Continue(Mode::Feedback) => {
            (Mode::Feedback, NodeType::Feedback, None)
        }
        TurnKind::Prompt(Mode::Commit)
        | TurnKind::NoSystemPrompt
        | TurnKind::Continue(Mode::Commit) => (Mode::Commit, NodeType::Commit, None),
        TurnKind::Compact { instructions } => {
            (Mode::Feedback, NodeType::Compact, Some(instructions))
        }
    };
    let endpoint = state.agent.endpoint(&spec.endpoint).cloned();
    let (Some(endpoint), Some(backend)) = (endpoint, state.agent.backend(&spec)) else {
        return Err(Refusal::Rejected(format!(
            "endpoint '{}' is not configured or has no token",
            spec.endpoint
        )));
    };
    let parent_status = parent
        .as_ref()
        .and_then(|parent| state.conversations.read().node(parent)?.meta.status);
    let parent_has_children = parent.as_ref().is_some_and(|parent| {
        state
            .conversations
            .read()
            .node(parent)
            .is_some_and(|node| !node.children.is_empty())
    });
    if parent_status == Some(NodeStatus::Running) {
        return Err(Refusal::Busy("this node is still running"));
    }
    // A continue turn replays the interrupted node's transcript: it is the one prompt
    // that may build on an incomplete node. A cancelled node cut short with something to
    // show is complete like a done one, so it takes children, and it takes a resume
    // whatever it already has — a resume forks the node's session, so the branches stay
    // independent. A failed node is only built on by a resume, never by a fresh child.
    let allowed = match parent_status {
        None | Some(NodeStatus::Done) => !continuing || !parent_has_children,
        Some(NodeStatus::Failed) => continuing,
        Some(NodeStatus::Cancelled) => true,
        Some(NodeStatus::Running) => false,
    };
    if !allowed {
        return Err(Refusal::Rejected(if continuing {
            "this node already has follow-ups; continue from its last reply".to_string()
        } else {
            "this node is not complete; continue from its parent".to_string()
        }));
    }
    let running: Vec<Mode> = state
        .running_turns
        .values()
        .map(|handle| handle.mode)
        .collect();
    if let Some(reason) = lock_refusal(&running) {
        return Err(Refusal::Busy(reason));
    }
    if crate::git::status_short(&state.root).is_ok_and(|status| !status.trim().is_empty()) {
        return Err(Refusal::DirtyTree);
    }

    let head = state.repo.as_ref().and_then(crate::git::head_id);
    let now = crate::util::time::now_secs();
    let meta = Metadata {
        node_type: Some(node_type),
        endpoint: Some(spec.endpoint.clone()),
        kind: Some(endpoint.kind.label().to_string()),
        model: Some(spec.model.clone()),
        effort: Some(spec.effort.as_str().to_string()),
        status: Some(NodeStatus::Running),
        head_before: head.clone(),
        command,
        no_system_prompt,
        ..Default::default()
    };
    let (node, lineage, folds, context) = {
        let mut tree = state.conversations.write();
        let node = tree
            .create_node(parent.as_ref(), now, meta, &prompt, input.as_deref())
            .map_err(|e| Refusal::Rejected(format!("could not create the node: {e}")))?;
        let context = tree
            .turn_context(&node)
            .ok_or_else(|| Refusal::Rejected("the new node has no directory".to_string()))?;
        let folds = tree.folds_on_the_way(&node);
        (node.clone(), tree.lineage(&node), folds, context)
    };
    let session = lineage.first().unwrap_or(&node).as_str().to_string();
    state.tree_view.expanded.extend(folds);
    state.tree_view.selected = Some(RowTarget::Node(node.clone()));
    // The new node has no title yet: no filter would list it.
    state.tree_view.query.clear();
    state.mark_session_dirty();

    let summary_backend = state
        .agent
        .summary_model
        .as_ref()
        .and_then(|summary| state.agent.backend(summary));
    let request = TurnRequest {
        spec,
        agent: Arc::clone(&state.agent),
        mode,
        system_prompt: !no_system_prompt,
        node: node.clone(),
        node_dir: context.node_dir.clone(),
        ancestor_dirs: context.ancestor_dirs,
        parent_head: context.parent_head,
        head,
        root: state.root.clone(),
        repo: state.repo.clone(),
        prompt: prompt.clone(),
        date: crate::util::time::utc_date(now),
        backend,
        endpoint,
        summary_backend,
        session,
        parent_session: context.parent_session.clone(),
        // A continue turn on a claude-code node resumes the node's own session, which
        // turn_context reports as the parent's.
        continue_session: continuing.then_some(context.parent_session).flatten(),
        compact,
        compacted_parent: context.compacted_parent,
        tree: Arc::clone(&state.conversations),
        mcp: Arc::clone(&state.mcp),
    };
    let handle = crate::agent::turn::spawn(request, main_tx.clone());
    state.running_turns.insert(node.clone(), handle);
    state.mark_session_dirty();
    Ok(node)
}
