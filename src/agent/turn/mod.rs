//! The life of a turn, off the UI thread: `live` is what it shows and records while it
//! runs, `close` what it does to the working tree when it ends, `title` what a finished
//! node is described by, and this module what a turn is and how it runs it.

mod close;
pub(crate) mod live;
#[cfg(test)]
mod pin;
pub(crate) mod title;

use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often the spinners advance one frame.
pub const SPINNER_INTERVAL: Duration = Duration::from_millis(100);
pub const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// The spinner character for the shared animation step `tick`.
pub fn spinner_char(tick: u8) -> char {
    SPINNER[tick as usize % SPINNER.len()]
}

/// Main-thread handle on a running turn.
#[derive(Clone)]
pub struct TurnHandle {
    pub live: Arc<parking_lot::Mutex<live::LiveTurn>>,
    pub started: Instant,
    pub mode: crate::agent::Mode,
    /// HEAD when the prompt was sent: what a commit turn's commits are counted from.
    pub head_before: Option<String>,
    cancel: Arc<tokio::sync::watch::Sender<bool>>,
}

impl TurnHandle {
    pub fn cancel(&self) {
        let _ = self.cancel.send(true);
    }

    /// A handle on a turn that never ran, for a test that only needs one parked in
    /// `AppState::running_turns`.
    #[cfg(test)]
    pub(crate) fn for_test(mode: crate::agent::Mode, reply: &str) -> Self {
        let (cancel, _) = tokio::sync::watch::channel(false);
        Self {
            live: Arc::new(parking_lot::Mutex::new(live::LiveTurn::for_test(
                reply,
                &[],
                "",
                "",
            ))),
            started: Instant::now(),
            mode,
            head_before: None,
            cancel: Arc::new(cancel),
        }
    }
}

#[derive(Clone)]
pub struct TurnRequest {
    pub mode: crate::agent::Mode,
    /// False for a custom command sent without the mode's system prompt.
    pub system_prompt: bool,
    pub node: crate::conversation::NodeId,
    pub node_dir: camino::Utf8PathBuf,
    /// Directories of the ancestors to replay, root first.
    pub ancestor_dirs: Vec<camino::Utf8PathBuf>,
    /// HEAD when the parent's turn ended.
    pub parent_head: Option<String>,
    /// HEAD when this prompt was sent.
    pub head: Option<String>,
    pub root: camino::Utf8PathBuf,
    pub repo: Option<gix::ThreadSafeRepository>,
    pub prompt: String,
    /// `YYYY-MM-DD`, for the system prompt.
    pub date: String,
    pub backend: crate::agent::Backend,
    /// The endpoint of `backend`, whose subscription this turn spends.
    pub endpoint: crate::endpoint::Endpoint,
    /// The model this turn is sent to.
    pub spec: crate::model_spec::ModelSpec,
    /// Endpoints, roles and models the turn's sub-agents can run on.
    pub agent: Arc<crate::agent::AgentConfig>,
    /// Model that titles and summarizes the node once its turn is done.
    pub summary_backend: Option<crate::agent::Backend>,
    /// Id of the conversation's root node, sent to OpenAI-compatible servers as the
    /// session of the turn's requests; a sub-agent's requests carry it with its node and
    /// run number appended.
    pub session: String,
    /// Claude Code session of the parent node, to fork when the backend is the CLI and the
    /// turn is a feedback turn.
    pub parent_session: Option<String>,
    /// Claude Code session of the node itself, for a continue turn on it: the session
    /// holds everything its interrupted turn did.
    pub continue_session: Option<String>,
    /// Instructions of a `/compact` turn; such a turn asks for a summary instead of
    /// sending `prompt`.
    pub compact: Option<String>,
    /// Directory of the parent when it is a compact node: its reply opens the context.
    pub compacted_parent: Option<camino::Utf8PathBuf>,
    /// The conversation tree, for the `conversation` tool.
    pub tree: Arc<parking_lot::RwLock<crate::conversation::ConversationTree>>,
    /// The MCP servers of the repository; the turn takes the tools of those running
    /// when it starts.
    pub mcp: Arc<crate::agent::mcp::McpHost>,
}

/// The text to send for `request`: what the turn asks, preceded by the notes Explorer
/// adds. Reads files and runs git.
fn message_to_send(request: &TurnRequest, sandboxed: bool) -> String {
    use crate::agent::context::{Feedback, Notes};
    let notes = Notes {
        root: &request.root,
        head: request.head.as_deref(),
        date: &request.date,
        compacted_parent: request.compacted_parent.as_deref(),
        parent_head: request.parent_head.as_deref(),
        feedback: Feedback::of(request.mode, sandboxed),
    };
    let ask = match &request.compact {
        Some(instructions) => crate::agent::context::compact_request(instructions),
        None => request.prompt.clone(),
    };
    notes.message(&ask)
}

/// Tells the UI what the spending behind `endpoint` is: what the CLI last `reported`
/// for a `claude-code` endpoint, or what OpenCode Go, Kimi Code, Cline or OpenRouter
/// answer now. Called after every turn, and for an OpenRouter key at startup and every
/// minute between turns, since its counter settles seconds after the requests that
/// spent it.
pub fn report_usage(
    endpoint: crate::endpoint::Endpoint,
    reported: Option<crate::usage::Usage>,
    main_tx: crate::signal::MainTx,
) {
    use crate::endpoint::EndpointKind;
    use crate::signal::AppSignal;
    tokio::spawn(async move {
        let usage = match &endpoint.kind {
            // The CLI pushes its own spending while it runs; there is nothing to ask.
            EndpointKind::ClaudeCode { .. } => reported,
            EndpointKind::Http {
                url,
                token,
                provider,
            } => crate::usage::fetch(*provider, url, token)
                .await
                .map_err(|e| tracing::warn!("usage of '{}' failed: {e}", endpoint.name))
                .ok()
                .flatten(),
        };
        let Some(usage) = usage else {
            return;
        };
        crate::signal::send(
            &main_tx,
            AppSignal::UsageFetched {
                endpoint: endpoint.name,
                usage,
            },
        );
    });
}

/// Starts the turn of a node already created on disk and returns its handle. The
/// outcome arrives as `AppSignal::TurnFinished`.
pub fn spawn(request: TurnRequest, main_tx: crate::signal::MainTx) -> TurnHandle {
    use crate::agent::Mode;
    use crate::agent::turn::live::LiveTurn;
    use crate::conversation::NodeStatus;
    use crate::signal::AppSignal;
    let live = Arc::new(parking_lot::Mutex::new(LiveTurn::new()));
    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
    let handle = TurnHandle {
        live: Arc::clone(&live),
        started: Instant::now(),
        mode: request.mode,
        head_before: request.head.clone(),
        cancel: Arc::new(cancel_tx),
    };
    // Before the handle leaves, so a window that mirrors the turn in its first frame
    // paints the prompt it was just sent, never the empty one a live turn starts with.
    {
        let mut live = live.lock();
        live.model = request.spec.to_string();
        live.prompt = request.prompt.clone();
    }

    tokio::spawn(async move {
        let mut recorder = live::Recorder::new(Arc::clone(&live), request.node_dir.clone());
        let workspace = prepare_workspace(&request, &mut recorder).await;
        let sandbox = workspace
            .as_ref()
            .and_then(|workspace| workspace.sandbox(&request.root));
        let in_place = workspace
            .as_ref()
            .and_then(|workspace| workspace.in_place_reason())
            .map(str::to_string);
        let result = run(&request, sandbox.as_ref(), &mut recorder, cancel_rx.clone()).await;
        // However the run ended, a turn cancelled while it ran is cancelled.
        let cancelled = *cancel_rx.borrow();
        let (mut status, mut error) = match result {
            _ if cancelled => (NodeStatus::Cancelled, None),
            Ok(()) => (NodeStatus::Done, None),
            Err(error) => (NodeStatus::Failed, Some(with_compact_hint(error))),
        };
        recorder.flush_reply();
        let mut head_after = request.repo.as_ref().and_then(crate::git::head_id);
        // The markdown a commit turn committed is formatted into its newest commit.
        if request.mode == Mode::Commit {
            let root = request.root.clone();
            let before = request.head.clone();
            let after = head_after.clone();
            let amended = tokio::task::spawn_blocking(move || {
                crate::markdown_format::format_committed(&root, before.as_deref(), after.as_deref())
            })
            .await
            .unwrap_or(false);
            if amended {
                head_after = request.repo.as_ref().and_then(crate::git::head_id);
            }
        }
        // Whatever a turn left uncommitted is discarded, however it ended.
        let (commits, reset) = close::close(&request, head_after.clone(), workspace.as_ref()).await;
        if let Some(workspace) = workspace {
            workspace.release();
        }
        // A turn cut short before it said anything has nothing to show and nothing to
        // build on: it fails. One with a reply or commits stays cancelled, part of the
        // discussion.
        if status == NodeStatus::Cancelled {
            (status, error) = crate::conversation::metadata::cut_short_outcome(
                &recorder.live.lock().reply,
                &commits,
            );
        }
        let (tokens_in, cached_in, session_id, reported_usage) = {
            let live = live.lock();
            (
                live.tokens_in,
                live.cached_in,
                live.session_id.clone(),
                live.usage.clone(),
            )
        };
        let outcome = crate::conversation::metadata::TurnOutcome {
            status,
            error,
            secs: handle.started.elapsed().as_secs(),
            head_after: head_after.clone(),
            tokens_in,
            cached_in,
            commits: commits.clone(),
            reset,
            session_id,
            in_place,
        };
        crate::signal::send(
            &main_tx,
            AppSignal::TurnFinished {
                node: request.node.clone(),
                outcome,
            },
        );
        report_usage(request.endpoint.clone(), reported_usage, main_tx.clone());
        let reply_blank = recorder.live.lock().reply.trim().is_empty();
        if (status == NodeStatus::Done || !reply_blank || !commits.is_empty())
            && let Some(backend) = &request.summary_backend
        {
            title::retitle(
                &request.node,
                &request.node_dir,
                &request.session,
                &request.root,
                request.head.clone(),
                head_after.clone(),
                backend,
                &main_tx,
            )
            .await;
        }
    });

    handle
}

async fn prepare_workspace(
    request: &TurnRequest,
    recorder: &mut live::Recorder,
) -> Option<crate::agent::workspace::Workspace> {
    use crate::agent::Mode;
    use crate::agent::workspace::Workspace;
    if request.mode != Mode::Feedback || request.compact.is_some() {
        return None;
    }
    recorder.activity(Some("preparing workspace".to_string()));
    let (root, node) = (request.root.clone(), request.node.as_str().to_string());
    let workspace = tokio::task::spawn_blocking(move || Workspace::prepare(&root, &node))
        .await
        .unwrap_or_else(|e| Workspace::InPlace {
            reason: crate::util::text::single_line(&format!("the copy failed: {e}")),
        });
    recorder.activity(None);
    recorder.live.lock().in_place = workspace.in_place_reason().map(str::to_string);
    Some(workspace)
}

/// Runs the backend; cancelled, it returns early and the caller reads the cancel as the outcome.
async fn run(
    request: &TurnRequest,
    sandbox: Option<&crate::agent::workspace::Sandbox>,
    recorder: &mut live::Recorder,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    use crate::agent::Backend;
    let mcp = tokio::select! {
        mcp = request.mcp.snapshot() => mcp,
        _ = cancel.wait_for(|cancelled| *cancelled) => return Ok(()),
    };
    match &request.backend {
        Backend::OpenAi(target) => {
            run_own_loop(
                request,
                sandbox,
                &mcp,
                target,
                &request.spec,
                recorder,
                cancel,
            )
            .await
        }
        Backend::ClaudeCode(target) => {
            tokio::select! {
                result = run_claude_code(request, sandbox, &mcp, target, recorder) => result,
                _ = cancel.wait_for(|cancelled| *cancelled) => Ok(()),
            }
        }
    }
}

/// The parent session a turn forks. Only feedback turns fork: a fork replays the parent's
/// earlier user messages, which carry the feedback note.
fn fork_source(mode: crate::agent::Mode, parent_session: Option<&str>) -> Option<&str> {
    use crate::agent::Mode;
    parent_session.filter(|_| mode == Mode::Feedback)
}

/// The session a turn's first run starts from.
///
/// A continue turn starts from the node's own session, which holds its thinking and its
/// tool state. It forks rather than appending in place: two resumes of one interrupted
/// node are two independent branches of the tree, and a fork keeps them from reading each
/// other's work. Feedback turns fork the parent's session when there is one; commit turns
/// start fresh so they never see the replayed feedback history. Every fork that is gone,
/// and every fresh start, opens with the ancestors as plain text (`Run::from`).
fn start_session(request: &TurnRequest) -> crate::agent::claude_code::Resume<'_> {
    use crate::agent::claude_code::Resume;
    match &request.continue_session {
        Some(session) => Resume::Fork(session),
        None => fork_source(request.mode, request.parent_session.as_deref())
            .map_or(Resume::Fresh, Resume::Fork),
    }
}

async fn run_claude_code(
    request: &TurnRequest,
    sandbox: Option<&crate::agent::workspace::Sandbox>,
    mcp: &crate::agent::mcp::McpTools,
    target: &crate::agent::claude_code::ClaudeTarget,
    recorder: &mut live::Recorder,
) -> Result<(), String> {
    use crate::agent::Message;
    use crate::agent::Mode;
    use crate::agent::claude_code::Run;
    use crate::agent::mcp::McpTools;
    use crate::agent::privileged::PrivilegedSocket;
    use crate::agent::tools::ToolContext;
    // Open until the run is over: the CLI's tool server asks it for the conversation.
    let socket = PrivilegedSocket::start(ToolContext {
        root: request.root.clone(),
        tree: Arc::clone(&request.tree),
        node: request.node.clone(),
        node_dir: None,
        background: Arc::default(),
        mcp: McpTools::default(),
        privileged: None,
    })?;
    let socket_path = socket.path();
    let sandboxed = sandbox.is_some();
    let (system, message) = {
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            let system = crate::agent::context::claude_system_prompt(
                request.system_prompt.then_some(request.mode),
                request.agent.review_rounds,
            );
            (system, message_to_send(&request, sandboxed))
        })
        .await
        .map_err(|e| e.to_string())?
    };
    let run = Run {
        target,
        root: &request.root,
        system: &system,
        mcp,
        privileged_socket: Some(&socket_path),
        sandbox,
    };
    run.from(
        &message,
        start_session(request),
        &request.ancestor_dirs,
        recorder,
    )
    .await?;
    if request.mode == Mode::Feedback {
        return Ok(());
    }
    // A commit turn is not over while the working tree is dirty: stay in the loop and
    // ask for the commit. The nudge joins the turn's own CLI session, which is the only
    // place this backend keeps it, so a session that is gone fails the turn rather than
    // opening one that has never read what the nudge refers to.
    let mut sent = 0;
    while let Some(nudge) = crate::agent::context::next_commit_nudge(&request.root, sent).await? {
        sent += 1;
        let Some(session) = recorder.live.lock().session_id.clone() else {
            break;
        };
        let Message::User { content: nudge, .. } = nudge else {
            break;
        };
        run.resume(&nudge, &session, recorder).await?;
    }
    Ok(())
}

async fn run_own_loop(
    request: &TurnRequest,
    sandbox: Option<&crate::agent::workspace::Sandbox>,
    mcp: &crate::agent::mcp::McpTools,
    target: &crate::agent::llm::LlmTarget,
    spec: &crate::model_spec::ModelSpec,
    recorder: &mut live::Recorder,
    cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    use crate::agent::Message;
    use crate::agent::Mode;
    use crate::agent::context::Feedback;
    use crate::agent::driver::Closing;
    use crate::agent::privileged::PrivilegedSocket;
    use crate::agent::tools::ToolContext;
    use crate::agent::wire::Start;
    let sandboxed = sandbox.is_some();
    let (system, messages) = {
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            let system = crate::agent::context::system_prompt(
                request.system_prompt.then_some(request.mode),
                request.agent.review_rounds,
                &request.root,
            );
            let mut messages = crate::agent::context::replay(&request.ancestor_dirs, request.mode);
            messages.push(Message::User {
                content: message_to_send(&request, sandboxed),
                injected: false,
            });
            (system, messages)
        })
        .await
        .map_err(|e| e.to_string())?
    };
    if let Some(sent) = messages.last() {
        recorder.message(sent);
    }
    let compact = request.compact.is_some();
    // Open until the run is over, for the agent process's tools; none if compact.
    let socket = if compact {
        None
    } else {
        Some(PrivilegedSocket::start(ToolContext {
            root: request.root.clone(),
            tree: Arc::clone(&request.tree),
            node: request.node.clone(),
            node_dir: Some(request.node_dir.clone()),
            background: Arc::default(),
            mcp: mcp.clone(),
            privileged: None,
        })?)
    };
    let start = Start {
        target: target.clone(),
        session: request.session.clone(),
        system,
        messages,
        root: request.root.clone(),
        node: request.node.as_str().to_string(),
        node_dir: request.node_dir.clone(),
        mcp: mcp.decls(),
        socket: socket.as_ref().map(PrivilegedSocket::path),
        agents: (!compact).then(|| {
            crate::agent::subagent::agents(
                &request.agent,
                spec,
                target,
                &request.date,
                Feedback::of(request.mode, sandboxed),
            )
        }),
        repeat_limit: request.agent.repeat_limit,
        compact,
    };
    // A commit turn is asked to commit while its tree is dirty; the nudge is never shown.
    let closing = Closing {
        sandbox,
        commit_root: (request.mode == Mode::Commit && !compact).then_some(request.root.as_path()),
        cancel,
    };
    crate::agent::driver::run(&start, closing, recorder).await
}

/// Points to `/compact` when an endpoint refuses a request for being too long.
fn with_compact_hint(error: String) -> String {
    let lower = error.to_lowercase();
    let too_long = lower.contains("context")
        && ["length", "window", "too long", "maximum", "exceed"]
            .iter()
            .any(|word| lower.contains(word));
    if too_long {
        format!("{error} — the conversation is too long: use /compact from the parent node")
    } else {
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_char_follows_the_tick_and_wraps() {
        assert_eq!(spinner_char(0), '⠋');
        assert_eq!(spinner_char(1), '⠙');
        assert_eq!(spinner_char(9), '⠏');
        assert_eq!(spinner_char(10), '⠋');
    }

    #[test]
    fn only_feedback_turns_fork_the_parent_session() {
        use crate::agent::Mode;
        assert_eq!(fork_source(Mode::Feedback, Some("abc")), Some("abc"));
        assert_eq!(fork_source(Mode::Commit, Some("abc")), None);
        assert_eq!(fork_source(Mode::Feedback, None), None);
        assert_eq!(fork_source(Mode::Commit, None), None);
    }

    #[test]
    fn a_continue_turn_starts_from_the_node_and_anything_else_from_its_parent() {
        use crate::agent::Backend;
        use crate::agent::Mode;
        use crate::agent::claude_code::Resume;
        use crate::conversation::NodeId;
        use crate::endpoint::Endpoint;
        use crate::endpoint::EndpointKind;
        use crate::model_spec::Effort;
        let request =
            |mode, parent_session: Option<&str>, continue_session: Option<&str>| TurnRequest {
                mode,
                system_prompt: true,
                node: NodeId::new("1"),
                node_dir: camino::Utf8PathBuf::new(),
                ancestor_dirs: Vec::new(),
                parent_head: None,
                head: None,
                root: camino::Utf8PathBuf::new(),
                repo: None,
                prompt: "go".to_string(),
                date: "2026-11-13".to_string(),
                backend: Backend::ClaudeCode(crate::agent::claude_code::ClaudeTarget {
                    model: "opus".to_string(),
                    effort: Effort::Default,
                }),
                endpoint: Endpoint {
                    name: "claude".to_string(),
                    kind: EndpointKind::ClaudeCode { models: vec![] },
                },
                spec: crate::model_spec::ModelSpec::parse("claude/opus").unwrap(),
                agent: std::sync::Arc::new(crate::agent::AgentConfig::default()),
                summary_backend: None,
                session: "root".to_string(),
                parent_session: parent_session.map(str::to_string),
                continue_session: continue_session.map(str::to_string),
                compact: None,
                compacted_parent: None,
                tree: std::sync::Arc::new(parking_lot::RwLock::new(
                    crate::conversation::ConversationTree::default(),
                )),
                mcp: std::sync::Arc::new(crate::agent::mcp::McpHost::default()),
            };
        // A continue turn must fork its node's session: continuing it in place would make
        // two resumes of one node read each other's work.
        let forked = |resume: Resume<'_>| match resume {
            Resume::Fresh => None,
            Resume::Continue(session) => panic!("continued {session} in place"),
            Resume::Fork(session) => Some(session.to_string()),
        };

        // A continue turn replays the node's own session, whatever its parent and mode.
        let continuing = request(Mode::Feedback, Some("parent"), Some("node"));
        assert_eq!(forked(start_session(&continuing)), Some("node".to_string()));
        let continuing = request(Mode::Commit, None, Some("node"));
        assert_eq!(forked(start_session(&continuing)), Some("node".to_string()));
        // A fresh child forks its parent's session only in a feedback turn.
        let child = request(Mode::Feedback, Some("parent"), None);
        assert_eq!(forked(start_session(&child)), Some("parent".to_string()));
        let commit = request(Mode::Commit, Some("parent"), None);
        assert_eq!(forked(start_session(&commit)), None);
        let root = request(Mode::Feedback, None, None);
        assert_eq!(forked(start_session(&root)), None);
    }

    #[test]
    fn context_length_errors_suggest_compacting() {
        let hinted = with_compact_hint("This model's maximum context length is 128000".into());
        assert!(hinted.ends_with("use /compact from the parent node"));
        assert_eq!(with_compact_hint("rate limited".into()), "rate limited");
    }
}
