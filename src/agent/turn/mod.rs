//! The life of a turn, off the UI thread: `live` is what it shows and records while it
//! runs, `close` what it does to the working tree when it ends, `title` what a finished
//! node is described by, and this module what a turn is and how it runs it.

mod close;
pub(crate) mod live;
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
    pub endpoint: crate::agent::Endpoint,
    /// The model this turn is sent to.
    pub spec: crate::model_spec::ModelSpec,
    /// Endpoints, roles and models the turn's sub-agents can run on.
    pub agent: Arc<crate::agent::AgentConfig>,
    /// Model that titles and summarizes the node once its turn is done.
    pub summary_backend: Option<crate::agent::Backend>,
    /// Id of the conversation's root node, sent to OpenAI-compatible servers as the
    /// session of every request.
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
/// adds — the state of the repository, the summary of a compacted parent, and what
/// moved since the parent's turn. Reads files and runs git.
fn message_to_send(request: &TurnRequest) -> String {
    use crate::agent::Mode;
    let environment = Some(crate::agent::context::environment_note(
        &request.root,
        request.head.as_deref(),
        &request.date,
    ));
    let summary = request.compacted_parent.as_ref().map(|dir| {
        let (_, summary) = crate::conversation::files::read_prompt_and_reply(dir);
        crate::agent::context::compaction_note(&summary)
    });
    let moved = moved_note(
        &request.root,
        request.parent_head.as_deref(),
        request.head.as_deref(),
    );
    let workspace =
        (request.mode == Mode::Feedback).then(|| crate::agent::context::WORKSPACE_NOTE.to_string());
    let ask = match &request.compact {
        Some(instructions) => crate::agent::context::compact_request(instructions),
        None => request.prompt.clone(),
    };
    crate::agent::context::with_notes(&[environment, summary, moved, workspace], &ask)
}

/// Tells the UI what the spending behind `endpoint` is: what the CLI last `reported`
/// for a `claude-code` endpoint, or what OpenCode Go, Kimi Code, Cline or OpenRouter
/// answer now. Called after every turn, and for an OpenRouter key at startup and every
/// minute between turns, since its counter settles seconds after the requests that
/// spent it.
pub fn report_usage(
    endpoint: crate::agent::Endpoint,
    reported: Option<crate::usage::Usage>,
    main_tx: crate::signal::MainTx,
) {
    use crate::agent::EndpointKind;
    use crate::signal::AppSignal;
    tokio::spawn(async move {
        let usage = match &endpoint.kind {
            EndpointKind::ClaudeCode { .. } => reported,
            EndpointKind::OpenCodeGo { url, token } => crate::usage::fetch(url, token)
                .await
                .map_err(|e| tracing::warn!("usage of '{}' failed: {e}", endpoint.name))
                .ok(),
            EndpointKind::KimiCode { url, token } => crate::usage::fetch_kimi(url, token)
                .await
                .map_err(|e| tracing::warn!("usage of '{}' failed: {e}", endpoint.name))
                .ok(),
            EndpointKind::Cline { url, token } => crate::usage::fetch_cline(url, token)
                .await
                .map_err(|e| tracing::warn!("usage of '{}' failed: {e}", endpoint.name))
                .ok(),
            EndpointKind::OpenRouter { url, token } => crate::usage::fetch_openrouter(url, token)
                .await
                .map_err(|e| tracing::warn!("usage of '{}' failed: {e}", endpoint.name))
                .ok(),
            EndpointKind::OpenAi { .. } => None,
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
    use crate::conversation::NodeStatus;
    use crate::signal::AppSignal;
    let live = Arc::new(parking_lot::Mutex::new(live::LiveTurn::default()));
    let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
    let handle = TurnHandle {
        live: Arc::clone(&live),
        started: Instant::now(),
        mode: request.mode,
        head_before: request.head.clone(),
        cancel: Arc::new(cancel_tx),
    };

    tokio::spawn(async move {
        let mut recorder =
            live::Recorder::new(Arc::clone(&live), request.node_dir.clone(), main_tx.clone());
        {
            let mut live = live.lock();
            live.model = request.spec.to_string();
            live.prompt = request.prompt.clone();
        }
        let (mut status, mut error) = tokio::select! {
            result = run(&request, &mut recorder) => match result {
                Ok(()) => (NodeStatus::Done, None),
                Err(error) => (NodeStatus::Failed, Some(with_compact_hint(error))),
            },
            _ = cancel_rx.wait_for(|cancelled| *cancelled) => (NodeStatus::Cancelled, None),
        };
        recorder.flush_reply();
        request
            .tree
            .write()
            .cancel_running_agents(&request.node, crate::util::time::now_secs());
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
        let (commits, reset) = close::close(&request, head_after.clone()).await;
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

async fn run(request: &TurnRequest, recorder: &mut live::Recorder) -> Result<(), String> {
    use crate::agent::Backend;
    let mcp = request.mcp.snapshot().await;
    match &request.backend {
        Backend::OpenAi(target) => {
            run_own_loop(request, &mcp, target, &request.spec, recorder).await
        }
        Backend::ClaudeCode(target) => run_claude_code(request, &mcp, target, recorder).await,
    }
}

/// The parent session a turn forks. Only feedback turns fork: a fork replays the parent's
/// earlier user messages, which carry the feedback-turn workspace note.
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
    mcp: &crate::agent::mcp::McpTools,
    target: &crate::agent::claude_code::ClaudeTarget,
    recorder: &mut live::Recorder,
) -> Result<(), String> {
    use crate::agent::Message;
    use crate::agent::Mode;
    use crate::agent::claude_code::Run;
    let (system, message) = {
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            let system = crate::agent::context::claude_system_prompt(
                request.system_prompt.then_some(request.mode),
                request.agent.review_rounds,
            );
            (system, message_to_send(&request))
        })
        .await
        .map_err(|e| e.to_string())?
    };
    let run = Run {
        target,
        root: &request.root,
        system: &system,
        mcp,
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
    mcp: &crate::agent::mcp::McpTools,
    target: &crate::agent::llm::LlmTarget,
    spec: &crate::model_spec::ModelSpec,
    recorder: &mut live::Recorder,
) -> Result<(), String> {
    use crate::agent::Message;
    use crate::agent::Mode;
    use crate::agent::agent_loop::Limits;
    use crate::agent::subagent::Subagents;
    use crate::agent::tools::ToolContext;
    let (system, mut messages) = {
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            let system = crate::agent::context::system_prompt(
                request.system_prompt.then_some(request.mode),
                request.agent.review_rounds,
                &request.root,
            );
            let mut messages: Vec<Message> = request
                .ancestor_dirs
                .iter()
                .flat_map(|dir| crate::agent::context::ancestor_messages(dir))
                .collect();
            if request.mode == Mode::Commit {
                crate::agent::context::strip_workspace_note(&mut messages);
            }
            messages.push(Message::User {
                content: message_to_send(&request),
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
    if request.compact.is_some() {
        // A compact turn is a single answer, without tools.
        recorder.begin_answer();
        let response = match crate::agent::llm::stream_chat(
            target,
            &request.session,
            &system,
            &messages,
            &[],
            |is_thinking, text| {
                if is_thinking {
                    recorder.reasoning(text);
                } else {
                    recorder.text(text);
                }
            },
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                recorder.flush_partial_answer();
                return Err(error);
            }
        };
        if let Some(tokens) = response.tokens_in {
            recorder.tokens_in(tokens);
        }
        recorder.cached_in(response.cached_in);
        recorder.message(&Message::Assistant {
            content: response.text,
            tool_calls: Vec::new(),
        });
        return Ok(());
    }
    let tool_context = ToolContext {
        root: request.root.clone(),
        tree: Arc::clone(&request.tree),
        node: request.node.clone(),
        subagents: Some(Subagents {
            config: Arc::clone(&request.agent),
            mode: request.mode,
            date: request.date.clone(),
            repo: request.repo.clone(),
            spec: spec.clone(),
        }),
        background: Arc::default(),
        mcp: mcp.clone(),
    };
    let limits = Limits {
        budget: None,
        repeat_limit: request.agent.repeat_limit,
    };
    crate::agent::agent_loop::run(
        &tool_context,
        target,
        &request.session,
        &system,
        &mut messages,
        recorder,
        limits,
    )
    .await?;
    if request.mode == Mode::Feedback {
        return Ok(());
    }
    // A commit turn is not over while the working tree is dirty: stay in the loop and
    // ask for the commit. The nudge goes in the transcript but is never shown.
    let mut sent = 0;
    while let Some(nudge) = crate::agent::context::next_commit_nudge(&request.root, sent).await? {
        sent += 1;
        recorder.message(&nudge);
        messages.push(nudge);
        crate::agent::agent_loop::run(
            &tool_context,
            target,
            &request.session,
            &system,
            &mut messages,
            recorder,
            limits,
        )
        .await?;
    }
    Ok(())
}

/// The repository-moved note, when HEAD is no longer where the parent's turn left it.
pub(super) fn moved_note(
    root: &camino::Utf8Path,
    parent_head: Option<&str>,
    head: Option<&str>,
) -> Option<String> {
    let (parent_head, head) = (parent_head?, head?);
    if parent_head == head {
        return None;
    }
    let commits = crate::git::log_oneline(
        root,
        parent_head,
        head,
        crate::agent::context::NOTE_MAX_COMMITS + 1,
    );
    Some(crate::agent::context::repository_moved_note(&commits))
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
        use crate::agent::Endpoint;
        use crate::agent::EndpointKind;
        use crate::agent::Mode;
        use crate::agent::claude_code::Resume;
        use crate::conversation::NodeId;
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
