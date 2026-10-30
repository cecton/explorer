use super::claude_code::{self, ClaudeTarget, Invocation, Resume, RunError};
use super::llm::{self, LlmTarget};
use super::mcp::{McpHost, McpTools};
use super::subagent::Subagents;
use super::tools::ToolContext;
use super::usage::{self, Usage};
use super::{
    AgentConfig, Backend, Endpoint, EndpointKind, Message, Mode, ModelSpec, agent_loop, context,
};
use crate::conversation::{CANCELLED_ERROR, ConversationTree, NodeId, NodeStatus, write_atomic};
use crate::git::discard_worktree;
use crate::tui::AppSignal;
use camino::{Utf8Path, Utf8PathBuf};
use parking_lot::{Mutex, RwLock};
use r3bl_tui::TerminalWindowMainThreadSignal;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};

const REPLY_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
const PROGRESS_SIGNAL_INTERVAL: Duration = Duration::from_millis(80);
/// How often a running turn asks for a redraw, which is also one spinner frame.
pub const REDRAW_INTERVAL: Duration = Duration::from_millis(100);
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const TITLE_MAX_CHARS: usize = 100;
const REPLY_MAX_CHARS: usize = 8000;
const COMMITS_MAX: usize = 20;
/// Times a commit turn is asked to commit what it left behind before Explorer discards it.
const MAX_NUDGES: usize = 2;
/// Opens the line of a tool call in the work log.
pub const TOOL_LINE_PREFIX: &str = "- ";
/// Sets a sub-agent's tool calls apart in the work log of the turn that called it.
const SUBAGENT_INDENT: &str = "  ";

type MainTx = mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>;

/// What a running turn has produced so far, read by Discussion windows at render time.
#[derive(Default)]
pub struct LiveTurn {
    /// Text of the answers that called no tools.
    pub reply: String,
    /// What led to the reply, in order: the text of answers that called tools, and
    /// one [`TOOL_LINE_PREFIX`] line per tool call.
    pub work_log: Vec<String>,
    /// First work-log entry of the answer in progress; entries before this are from
    /// dropped tool-calling answers whose thinking is no longer shown.
    log_start: usize,
    /// The tool call in progress, as shown on the status line.
    pub activity: Option<String>,
    /// The thinking streamed so far in the answer in progress; shown dimmed in the
    /// discussion window and never recorded.
    pub thinking: String,
    /// Model this turn was sent to, shown next to its outcome.
    pub model: String,
    pub tokens_in: Option<u64>,
    /// Input tokens of the turn's first request the provider served from its prompt cache.
    pub cached_in: Option<u64>,
    /// Claude Code session holding this turn, once the CLI has announced it.
    pub session_id: Option<String>,
    /// What the CLI last reported of the subscription's limits.
    pub usage: Option<Usage>,
    /// The sub-agent this turn is waiting for.
    pub subagent: Option<LiveSubagent>,
}

/// A sub-agent run in progress, as the turn that called it sees it.
#[derive(Clone)]
pub struct LiveSubagent {
    pub index: u32,
    pub started: Instant,
    pub live: Arc<Mutex<LiveTurn>>,
}

/// The spinner frame of a turn that has been running for `elapsed`.
pub fn spinner_frame(elapsed: Duration) -> char {
    let frame = elapsed.as_millis() / REDRAW_INTERVAL.as_millis();
    SPINNER[frame as usize % SPINNER.len()]
}

/// Main-thread handle on a running turn.
#[derive(Clone)]
pub struct TurnHandle {
    pub live: Arc<Mutex<LiveTurn>>,
    pub started: Instant,
    pub mode: Mode,
    cancel: Arc<watch::Sender<bool>>,
}

impl TurnHandle {
    pub fn cancel(&self) {
        let _ = self.cancel.send(true);
    }
}

impl LiveTurn {
    /// Work log entries from the current answer in progress.
    pub fn visible_work_log(&self) -> &[String] {
        &self.work_log[self.log_start.min(self.work_log.len())..]
    }
}

/// How a turn ended, applied to the node's metadata on the main thread.
#[derive(Clone, Debug)]
pub struct TurnOutcome {
    pub status: NodeStatus,
    pub error: Option<String>,
    /// Seconds the turn took, wall clock.
    pub secs: u64,
    pub head_after: Option<String>,
    pub tokens_in: Option<u64>,
    pub cached_in: Option<u64>,
    /// Commits the turn produced, oldest first.
    pub commits: Vec<String>,
    /// True when the turn's uncommitted work was discarded at its end.
    pub reset: bool,
    /// Claude Code session of the turn; `None` for our own loop.
    pub session_id: Option<String>,
}

#[derive(Clone)]
pub struct TurnRequest {
    pub mode: Mode,
    /// False for a custom command sent without the mode's system prompt.
    pub system_prompt: bool,
    pub node: NodeId,
    pub node_dir: Utf8PathBuf,
    /// Directories of the ancestors to replay, root first.
    pub ancestor_dirs: Vec<Utf8PathBuf>,
    /// HEAD when the parent's turn ended.
    pub parent_head: Option<String>,
    /// HEAD when this prompt was sent.
    pub head: Option<String>,
    pub root: Utf8PathBuf,
    pub repo: Option<gix::ThreadSafeRepository>,
    pub prompt: String,
    /// `YYYY-MM-DD`, for the system prompt.
    pub date: String,
    pub backend: Backend,
    /// The endpoint of `backend`, whose subscription this turn spends.
    pub endpoint: Endpoint,
    /// The model this turn is sent to.
    pub spec: ModelSpec,
    /// Endpoints, roles and models the turn's sub-agents can run on.
    pub agent: Arc<AgentConfig>,
    /// Model that titles and summarizes the node once its turn is done.
    pub summary_backend: Option<Backend>,
    /// Id of the conversation's root node, sent to OpenAI-compatible servers as the
    /// session of every request.
    pub session: String,
    /// Claude Code session of the parent node, to fork when the backend is the CLI.
    pub parent_session: Option<String>,
    /// Claude Code session of the node itself, for a continue turn on it: the session
    /// holds everything its interrupted turn did.
    pub continue_session: Option<String>,
    /// Instructions of a `/compact` turn; such a turn asks for a summary instead of
    /// sending `prompt`.
    pub compact: Option<String>,
    /// Directory of the parent when it is a compact node: its reply opens the context.
    pub compacted_parent: Option<Utf8PathBuf>,
    /// The conversation tree, for the `conversation` tool.
    pub tree: Arc<RwLock<ConversationTree>>,
    /// The MCP servers of the repository; the turn takes the tools of those running
    /// when it starts.
    pub mcp: Arc<McpHost>,
}

/// The text to send for `request`: what the turn asks, preceded by the notes Explorer
/// adds — the state of the repository, the summary of a compacted parent, and what
/// moved since the parent's turn. Reads files and runs git.
fn message_to_send(request: &TurnRequest) -> String {
    let environment = Some(context::environment_note(
        &request.root,
        request.head.as_deref(),
        &request.date,
    ));
    let summary = request.compacted_parent.as_ref().map(|dir| {
        let (_, summary) = crate::conversation::read_prompt_and_reply(dir);
        context::compaction_note(&summary)
    });
    let moved = moved_note(
        &request.root,
        request.parent_head.as_deref(),
        request.head.as_deref(),
    );
    let workspace = (request.mode == Mode::Feedback).then(|| context::WORKSPACE_NOTE.to_string());
    let ask = match &request.compact {
        Some(instructions) => context::compact_request(instructions),
        None => request.prompt.clone(),
    };
    context::with_notes(&[environment, summary, moved, workspace], &ask)
}

/// Collects a turn's output: live reply and work log for the UI, `reply.md`,
/// `activity.md` and `transcript.jsonl` on disk.
pub struct Recorder {
    pub(super) live: Arc<Mutex<LiveTurn>>,
    node_dir: Utf8PathBuf,
    main_tx: MainTx,
    /// The recorder of the turn that called this sub-agent, which logs its tool calls
    /// too.
    caller: Option<Caller>,
    /// Where the text of the current answer starts in the live reply.
    answer_start: usize,
    last_flush: Instant,
    last_signal: Instant,
    /// Chars of thinking recorded for the answer in progress; lets the recorder drop
    /// the live thinking when the answer turns out to call tools.
    thinking_start: usize,
}

/// Where a sub-agent's tool calls are copied: the work log of the calling turn.
struct Caller {
    live: Arc<Mutex<LiveTurn>>,
    node_dir: Utf8PathBuf,
}

/// Appends one entry to a work log and to the `activity.md` of `dir`.
fn append_work_log(dir: &Utf8Path, live: &Mutex<LiveTurn>, entry: String) {
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("activity.md"))
        .and_then(|mut file| writeln!(file, "{entry}"));
    if let Err(e) = appended {
        tracing::warn!("failed to append to activity.md: {e}");
    }
    live.lock().work_log.push(entry);
}

impl Recorder {
    fn new(live: Arc<Mutex<LiveTurn>>, node_dir: Utf8PathBuf, main_tx: MainTx) -> Self {
        Self {
            live,
            node_dir,
            main_tx,
            caller: None,
            answer_start: 0,
            last_flush: Instant::now(),
            last_signal: Instant::now(),
            thinking_start: 0,
        }
    }

    /// The recorder of a sub-agent run kept in `dir`, whose tool calls also go to this
    /// turn's work log.
    pub(super) fn subagent(&self, dir: Utf8PathBuf) -> Self {
        Self {
            caller: Some(Caller {
                live: Arc::clone(&self.live),
                node_dir: self.node_dir.clone(),
            }),
            ..Self::new(Arc::default(), dir, self.main_tx.clone())
        }
    }

    pub(super) fn main_tx(&self) -> MainTx {
        self.main_tx.clone()
    }

    pub(super) async fn send(&self, signal: AppSignal) {
        let _ = self
            .main_tx
            .send(TerminalWindowMainThreadSignal::ApplyAppSignal(signal))
            .await;
    }

    fn signal(&mut self) {
        self.last_signal = Instant::now();
        let _ = self
            .main_tx
            .try_send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                AppSignal::TurnProgress,
            ));
    }

    pub(super) fn flush_reply(&mut self) {
        self.last_flush = Instant::now();
        let reply = self.live.lock().reply.clone();
        if let Err(e) = write_atomic(&self.node_dir.join("reply.md"), &reply) {
            tracing::warn!("failed to write reply.md: {e}");
        }
    }

    /// Separates the text of successive model answers within one turn.
    pub fn begin_answer(&mut self) {
        let mut live = self.live.lock();
        if !live.reply.is_empty() && !live.reply.ends_with("\n\n") {
            let missing = if live.reply.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            live.reply.push_str(missing);
        }
        self.answer_start = live.reply.len();
        self.thinking_start = live.thinking.len();
    }

    /// The current answer calls tools: its text is not part of the reply, and moves
    /// to the work log.
    pub fn answer_calls_tools(&mut self) {
        {
            let mut live = self.live.lock();
            live.thinking.truncate(self.thinking_start);
        }
        let text = {
            let mut live = self.live.lock();
            let start = self.answer_start.min(live.reply.len());
            live.reply.split_off(start)
        };
        let text = text.trim();
        if !text.is_empty() {
            self.log(text.to_string());
            self.flush_reply();
        }
        {
            let mut live = self.live.lock();
            live.log_start = live.work_log.len();
        }
    }

    /// The answer in progress died mid-stream: what it streamed stays in the reply and
    /// joins the transcript as an assistant message without tool calls, so a replay
    /// continues from it instead of repeating it. A second call records nothing.
    pub fn flush_partial_answer(&mut self) {
        let text = {
            let live = self.live.lock();
            let start = self.answer_start.min(live.reply.len());
            self.answer_start = live.reply.len();
            live.reply[start..].to_string()
        };
        if text.trim().is_empty() {
            return;
        }
        self.message(&Message::Assistant {
            content: text,
            tool_calls: Vec::new(),
        });
    }

    /// Adds a tool call to the work log and shows it as the activity in progress.
    pub fn tool_call(&mut self, description: String) {
        if let Some(caller) = &self.caller {
            let entry = format!("{SUBAGENT_INDENT}{TOOL_LINE_PREFIX}{description}");
            append_work_log(&caller.node_dir, &caller.live, entry);
        }
        self.log(format!("{TOOL_LINE_PREFIX}{description}"));
        self.activity(Some(description));
    }

    /// Appends one entry to the work log and to `activity.md`.
    pub(super) fn log(&mut self, entry: String) {
        append_work_log(&self.node_dir, &self.live, entry);
    }

    pub fn text(&mut self, chunk: &str) {
        self.live.lock().reply.push_str(chunk);
        if self.last_flush.elapsed() >= REPLY_FLUSH_INTERVAL {
            self.flush_reply();
        }
        if self.last_signal.elapsed() >= PROGRESS_SIGNAL_INTERVAL {
            self.signal();
        }
    }

    /// Thinking of the answer in progress; shown dimmed and never recorded.
    pub fn reasoning(&mut self, chunk: &str) {
        self.live.lock().thinking.push_str(chunk);
        if self.last_signal.elapsed() >= PROGRESS_SIGNAL_INTERVAL {
            self.signal();
        }
    }

    /// Appends one message to `transcript.jsonl`.
    pub fn message(&mut self, message: &Message) {
        let appended = serde_json::to_string(message)
            .map_err(std::io::Error::other)
            .and_then(|line| {
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.node_dir.join("transcript.jsonl"))?;
                writeln!(file, "{line}")
            });
        if let Err(e) = appended {
            tracing::warn!("failed to append to transcript.jsonl: {e}");
        }
    }

    pub fn activity(&mut self, activity: Option<String>) {
        self.live.lock().activity = activity;
        self.signal();
    }

    pub fn tokens_in(&mut self, tokens: u64) {
        self.live.lock().tokens_in = Some(tokens);
    }

    /// Cache hits of the first request of the turn that reported them: how much of
    /// the context was already cached when the prompt was sent. Later requests find
    /// the whole previous round cached, which says nothing about the conversation.
    pub fn cached_in(&mut self, cached: Option<u64>) {
        let mut live = self.live.lock();
        if live.cached_in.is_none() {
            live.cached_in = cached;
        }
    }

    pub fn session(&mut self, session_id: String) {
        self.live.lock().session_id = Some(session_id);
    }

    pub fn usage(&mut self, usage: Usage) {
        self.live.lock().usage = Some(usage);
    }
}

/// Tells the UI how much of the subscription behind `endpoint` is used once a turn has
/// spent some of it: what the CLI last `reported`, or what OpenCode Go answers now.
pub(super) fn report_usage(endpoint: Endpoint, reported: Option<Usage>, main_tx: MainTx) {
    tokio::spawn(async move {
        let usage = match &endpoint.kind {
            EndpointKind::ClaudeCode { .. } => reported,
            EndpointKind::OpenCodeGo { url, token } => usage::fetch(url, token)
                .await
                .map_err(|e| tracing::warn!("usage of '{}' failed: {e}", endpoint.name))
                .ok(),
            EndpointKind::OpenAi { .. } => None,
        };
        let Some(usage) = usage else {
            return;
        };
        let _ = main_tx
            .send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                AppSignal::UsageFetched {
                    endpoint: endpoint.name,
                    usage,
                },
            ))
            .await;
    });
}

/// What a turn cut short records: one with content stays cancelled, part of the
/// discussion; one with none has nothing to show and fails.
fn cancelled_status(reply: &str, commits: &[String]) -> (NodeStatus, Option<String>) {
    if !crate::conversation::has_content(reply) && commits.is_empty() {
        (NodeStatus::Failed, Some(CANCELLED_ERROR.to_string()))
    } else {
        (NodeStatus::Cancelled, None)
    }
}

/// Starts the turn of a node already created on disk and returns its handle. The
/// outcome arrives as `AppSignal::TurnFinished`.
pub fn spawn(request: TurnRequest, main_tx: MainTx) -> TurnHandle {
    let live = Arc::new(Mutex::new(LiveTurn::default()));
    let (cancel_tx, mut cancel_rx) = watch::channel(false);
    let handle = TurnHandle {
        live: Arc::clone(&live),
        started: Instant::now(),
        mode: request.mode,
        cancel: Arc::new(cancel_tx),
    };

    let redraw_tx = main_tx.clone();
    let redraw = tokio::spawn(async move {
        let mut interval = tokio::time::interval(REDRAW_INTERVAL);
        loop {
            interval.tick().await;
            let _ = redraw_tx.try_send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                AppSignal::TurnProgress,
            ));
        }
    });

    tokio::spawn(async move {
        let mut recorder =
            Recorder::new(Arc::clone(&live), request.node_dir.clone(), main_tx.clone());
        live.lock().model = request.spec.to_string();
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
            .cancel_running_agents(&request.node, now_secs());
        let head_after = request.repo.as_ref().and_then(crate::git::head_id);
        // Whatever a turn left uncommitted is discarded, however it ended.
        let (commits, reset) = match request.mode {
            Mode::Feedback => {
                let root = request.root.clone();
                let reset = tokio::task::spawn_blocking(move || discard_turn_work(&root))
                    .await
                    .unwrap_or(false);
                (Vec::new(), reset)
            }
            Mode::Commit => {
                let root = request.root.clone();
                let node = request.node.clone();
                let before = request.head.clone();
                let after = head_after.clone();
                let (commits, reset) = tokio::task::spawn_blocking(move || {
                    close_commit_turn(&root, node.as_str(), before.as_deref(), after.as_deref())
                })
                .await
                .unwrap_or_default();
                (commits, reset)
            }
        };
        // A turn cut short before it said anything has nothing to show and nothing to
        // build on: it fails. One with a reply or commits stays cancelled, part of the
        // discussion.
        if status == NodeStatus::Cancelled {
            (status, error) = cancelled_status(&recorder.live.lock().reply, &commits);
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
        let outcome = TurnOutcome {
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
        let _ = main_tx
            .send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                AppSignal::TurnFinished {
                    node: request.node.clone(),
                    outcome,
                },
            ))
            .await;
        redraw.abort();
        report_usage(request.endpoint.clone(), reported_usage, main_tx.clone());
        let reply_blank = recorder.live.lock().reply.trim().is_empty();
        if (status == NodeStatus::Done || !reply_blank || !commits.is_empty())
            && let Some(backend) = &request.summary_backend
        {
            let root = request.root.clone();
            let (before, after) = (request.head.clone(), head_after.clone());
            let commits = tokio::task::spawn_blocking(move || match (before, after) {
                (Some(before), Some(after)) => {
                    crate::git::log_oneline(&root, &before, &after, COMMITS_MAX)
                }
                _ => Vec::new(),
            })
            .await
            .unwrap_or_default();
            let (prompt, reply) = crate::conversation::read_prompt_and_reply(&request.node_dir);
            let input = title_input(&prompt, &reply, &commits);
            title_and_summarize(&request, backend, &input, &main_tx).await;
        }
    });

    handle
}

async fn run(request: &TurnRequest, recorder: &mut Recorder) -> Result<(), String> {
    let mcp = request.mcp.snapshot().await;
    match &request.backend {
        Backend::OpenAi(target) => {
            run_own_loop(request, &mcp, target, &request.spec, recorder).await
        }
        Backend::ClaudeCode(target) => run_claude_code(request, &mcp, target, recorder).await,
    }
}

/// The working tree status when a commit turn still has something to commit.
async fn uncommitted(request: &TurnRequest) -> Result<Option<String>, String> {
    let root = request.root.clone();
    let status = tokio::task::spawn_blocking(move || crate::git::status_short(&root))
        .await
        .map_err(|e| e.to_string())??;
    Ok((!status.trim().is_empty()).then_some(status))
}

async fn run_claude_code(
    request: &TurnRequest,
    mcp: &McpTools,
    target: &ClaudeTarget,
    recorder: &mut Recorder,
) -> Result<(), String> {
    let (system, message, preamble) = {
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            let system = context::claude_system_prompt(
                request.system_prompt.then_some(request.mode),
                request.agent.review_rounds,
            );
            let preamble = context::handover_preamble(&request.ancestor_dirs);
            (system, message_to_send(&request), preamble)
        })
        .await
        .map_err(|e| e.to_string())?
    };
    // A continue turn owns the node's own session: the CLI kept its thinking and its
    // tool state, so resuming it is the whole point. When it is gone, a fresh session
    // opens with the ancestors as plain text.
    if let Some(continue_session) = &request.continue_session {
        let run = claude_code::run(
            &invocation(
                request,
                mcp,
                target,
                &system,
                &message,
                Resume::Continue(continue_session),
            ),
            recorder,
        )
        .await;
        match run {
            Ok(()) => {}
            Err(RunError::SessionGone) => {
                let opening = match preamble {
                    Some(preamble) => format!("{preamble}\n\n{message}"),
                    None => message,
                };
                claude_code::run(
                    &invocation(request, mcp, target, &system, &opening, Resume::Fresh),
                    recorder,
                )
                .await
                .map_err(run_error_text)?;
            }
            Err(RunError::Failed(error)) => return Err(error),
        }
    } else {
        // Fork the parent's session when there is one; otherwise, or when it is gone, start
        // a fresh session that opens with the ancestors as plain text.
        let forked = match request.parent_session.as_deref() {
            Some(session) => {
                match claude_code::run(
                    &invocation(
                        request,
                        mcp,
                        target,
                        &system,
                        &message,
                        Resume::Fork(session),
                    ),
                    recorder,
                )
                .await
                {
                    Ok(()) => true,
                    Err(RunError::SessionGone) => false,
                    Err(RunError::Failed(error)) => return Err(error),
                }
            }
            None => false,
        };
        if !forked {
            let opening = match preamble {
                Some(preamble) => format!("{preamble}\n\n{message}"),
                None => message,
            };
            claude_code::run(
                &invocation(request, mcp, target, &system, &opening, Resume::Fresh),
                recorder,
            )
            .await
            .map_err(run_error_text)?;
        }
    }
    if request.mode == Mode::Feedback {
        return Ok(());
    }
    for _ in 0..MAX_NUDGES {
        let Some(status) = uncommitted(request).await? else {
            break;
        };
        let Some(session) = recorder.live.lock().session_id.clone() else {
            break;
        };
        let Message::User { content: nudge, .. } = context::commit_nudge(&status) else {
            break;
        };
        claude_code::run(
            &invocation(
                request,
                mcp,
                target,
                &system,
                &nudge,
                Resume::Continue(&session),
            ),
            recorder,
        )
        .await
        .map_err(run_error_text)?;
    }
    Ok(())
}

fn invocation<'a>(
    request: &'a TurnRequest,
    mcp: &'a McpTools,
    target: &'a ClaudeTarget,
    system: &'a str,
    prompt: &'a str,
    resume: Resume<'a>,
) -> Invocation<'a> {
    Invocation {
        target,
        root: &request.root,
        system,
        prompt,
        resume,
        mcp,
    }
}

fn run_error_text(error: RunError) -> String {
    match error {
        RunError::SessionGone => "the Claude Code session of this turn is gone".to_string(),
        RunError::Failed(error) => error,
    }
}

async fn run_own_loop(
    request: &TurnRequest,
    mcp: &McpTools,
    target: &LlmTarget,
    spec: &ModelSpec,
    recorder: &mut Recorder,
) -> Result<(), String> {
    let (system, mut messages) = {
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            let system = context::system_prompt(
                request.system_prompt.then_some(request.mode),
                request.agent.review_rounds,
                &request.root,
            );
            let mut messages: Vec<Message> = request
                .ancestor_dirs
                .iter()
                .flat_map(|dir| context::ancestor_messages(dir))
                .collect();
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
        let response = match llm::stream_chat(
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
    agent_loop::run(
        &tool_context,
        target,
        &request.session,
        &system,
        &mut messages,
        recorder,
        None,
    )
    .await?;
    if request.mode == Mode::Feedback {
        return Ok(());
    }
    // A commit turn is not over while the working tree is dirty: stay in the loop and
    // ask for the commit. The nudge goes in the transcript but is never shown.
    for _ in 0..MAX_NUDGES {
        let Some(status) = uncommitted(request).await? else {
            break;
        };
        let nudge = context::commit_nudge(&status);
        recorder.message(&nudge);
        messages.push(nudge);
        agent_loop::run(
            &tool_context,
            target,
            &request.session,
            &system,
            &mut messages,
            recorder,
            None,
        )
        .await?;
    }
    Ok(())
}

/// The commits a commit turn made; what it left uncommitted is discarded.
fn close_commit_turn(
    root: &Utf8Path,
    node: &str,
    head_before: Option<&str>,
    head_after: Option<&str>,
) -> (Vec<String>, bool) {
    let commits = match head_after {
        Some(after) if head_before != Some(after) => {
            crate::git::commits_between(root, head_before, after).unwrap_or_else(|e| {
                tracing::warn!("cannot list the commits of {node}: {e}");
                vec![after.to_string()]
            })
        }
        _ => Vec::new(),
    };
    (commits, discard_turn_work(root))
}

/// Discards what the turn left uncommitted; the tree was clean when the turn started.
fn discard_turn_work(root: &Utf8Path) -> bool {
    match crate::git::status_short(root) {
        Ok(status) if !status.trim().is_empty() => discard_worktree(root)
            .map_err(|e| tracing::warn!("cannot reset the tree after the turn: {e}"))
            .is_ok(),
        Ok(_) => false,
        Err(e) => {
            tracing::warn!("cannot check the tree after the turn: {e}");
            false
        }
    }
}

/// The repository-moved note, when HEAD is no longer where the parent's turn left it.
pub(super) fn moved_note(
    root: &Utf8Path,
    parent_head: Option<&str>,
    head: Option<&str>,
) -> Option<String> {
    let (parent_head, head) = (parent_head?, head?);
    if parent_head == head {
        return None;
    }
    let commits = crate::git::log_oneline(root, parent_head, head, context::NOTE_MAX_COMMITS + 1);
    Some(context::repository_moved_note(&commits))
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

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |e| e.as_secs() as i64)
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// What the summary model reads: the prompt, the end of the reply and the commits made.
fn title_input(prompt: &str, reply: &str, commits: &[String]) -> String {
    let reply = match reply.char_indices().nth(REPLY_MAX_CHARS) {
        Some((end, _)) => &reply[..end],
        None => reply,
    };
    let mut input = format!(
        "<prompt>\n{}\n</prompt>\n\n<reply>\n{}\n</reply>\n",
        prompt.trim(),
        reply.trim()
    );
    if !commits.is_empty() {
        input.push_str(&format!(
            "\n<commits>\n{}\n</commits>\n",
            commits.join("\n")
        ));
    }
    input.push_str("\nWrite the title and the summary of the turn recorded above.\n");
    input
}

/// Splits the model's answer into the title (first line) and the summary (the rest).
fn parse_title_and_summary(answer: &str) -> Option<(String, String)> {
    let answer = answer.trim();
    let (first, rest) = answer.split_once('\n').unwrap_or((answer, ""));
    let title: String = single_line(first).chars().take(TITLE_MAX_CHARS).collect();
    (!title.is_empty()).then(|| (title, single_line(rest)))
}

/// One answer without tools from the summary model.
async fn complete(
    backend: &Backend,
    session: &str,
    system: &str,
    user: &str,
) -> Result<String, String> {
    match backend {
        Backend::OpenAi(target) => llm::complete(target, session, system, user).await,
        Backend::ClaudeCode(target) => claude_code::complete(target, system, user).await,
    }
}

/// Replaces `title.md` and `summary.md` of a finished node with what the model makes of
/// its turn. Failures are logged and leave the prompt-based text in place.
async fn title_and_summarize(
    request: &TurnRequest,
    backend: &Backend,
    input: &str,
    main_tx: &MainTx,
) {
    let answer = match complete(
        backend,
        &request.session,
        context::TITLE_SUMMARY_PROMPT,
        input,
    )
    .await
    {
        Ok(answer) => answer,
        Err(e) => {
            tracing::warn!("title generation failed: {e}");
            return;
        }
    };
    let Some((title, summary)) = parse_title_and_summary(&answer) else {
        tracing::warn!("title generation returned no title");
        return;
    };
    if let Err(e) = write_atomic(&request.node_dir.join("title.md"), &title) {
        tracing::warn!("failed to write title.md: {e}");
    }
    let signal = AppSignal::NodeTitleReady {
        node: request.node.clone(),
        title,
    };
    let _ = main_tx
        .send(TerminalWindowMainThreadSignal::ApplyAppSignal(signal))
        .await;
    if summary.is_empty() {
        return;
    }
    if let Err(e) = write_atomic(&request.node_dir.join("summary.md"), &summary) {
        tracing::warn!("failed to write summary.md: {e}");
    }
    let signal = AppSignal::NodeSummaryReady {
        node: request.node.clone(),
        summary,
    };
    let _ = main_tx
        .send(TerminalWindowMainThreadSignal::ApplyAppSignal(signal))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_advances_with_the_elapsed_time_and_wraps() {
        assert_eq!(spinner_frame(Duration::ZERO), '⠋');
        assert_eq!(
            spinner_frame(REDRAW_INTERVAL - Duration::from_millis(1)),
            '⠋'
        );
        assert_eq!(spinner_frame(REDRAW_INTERVAL), '⠙');
        assert_eq!(spinner_frame(REDRAW_INTERVAL * 9), '⠏');
        assert_eq!(spinner_frame(REDRAW_INTERVAL * 10), '⠋');
    }

    #[test]
    fn a_cancel_with_content_stays_cancelled() {
        let (status, error) = cancelled_status("a partial answer", &[]);
        assert_eq!(status, NodeStatus::Cancelled);
        assert_eq!(error, None);
        let (status, error) = cancelled_status("", &["abc1234".to_string()]);
        assert_eq!(status, NodeStatus::Cancelled);
        assert_eq!(error, None);
        let (status, error) = cancelled_status("  \n  ", &[]);
        assert_eq!(status, NodeStatus::Failed);
        assert_eq!(error.as_deref(), Some(crate::conversation::CANCELLED_ERROR));
    }

    fn git(root: &Utf8Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn temp_repo() -> (tempfile::TempDir, Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "test"]);
        git(&root, &["config", "user.email", "test@example.com"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        (tmp, root)
    }

    fn commit(root: &Utf8Path, file: &str, content: &str, message: &str) -> String {
        std::fs::write(root.join(file), content).unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", message]);
        git(root, &["rev-parse", "HEAD"])
    }

    #[test]
    fn cache_hits_are_those_of_the_first_request() {
        let tmp = tempfile::tempdir().unwrap();
        let node_dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let (main_tx, _main_rx) = mpsc::channel(8);
        let live = Arc::default();
        let mut recorder = Recorder::new(Arc::clone(&live), node_dir, main_tx);

        recorder.cached_in(Some(0));
        recorder.cached_in(Some(20_000));
        assert_eq!(live.lock().cached_in, Some(0));
    }

    #[test]
    fn text_of_an_answer_that_calls_tools_moves_to_the_work_log() {
        let tmp = tempfile::tempdir().unwrap();
        let node_dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let (main_tx, _main_rx) = mpsc::channel(8);
        let mut recorder = Recorder::new(Arc::default(), node_dir.clone(), main_tx);

        recorder.begin_answer();
        recorder.text("Let me look.\n");
        recorder.answer_calls_tools();
        recorder.tool_call("shell ls".to_string());
        recorder.tool_call("read a.txt".to_string());
        recorder.begin_answer();
        recorder.text("First part.");
        recorder.begin_answer();
        recorder.answer_calls_tools();
        recorder.tool_call("shell git commit".to_string());
        recorder.begin_answer();
        recorder.text("Committed.");
        recorder.flush_reply();

        let read = |name: &str| std::fs::read_to_string(node_dir.join(name)).unwrap();
        assert_eq!(read("reply.md"), "First part.\n\nCommitted.");
        assert_eq!(
            read("activity.md"),
            "Let me look.\n- shell ls\n- read a.txt\n- shell git commit\n"
        );
        let live = recorder.live.lock();
        assert_eq!(live.reply, "First part.\n\nCommitted.");
        assert_eq!(live.work_log.len(), 4);
        assert_eq!(live.activity.as_deref(), Some("shell git commit"));
    }

    #[test]
    fn a_sub_agent_logs_its_tool_calls_in_the_work_log_of_its_caller_too() {
        let tmp = tempfile::tempdir().unwrap();
        let node_dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let agent_dir = node_dir.join("agents/1");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let (main_tx, _main_rx) = mpsc::channel(8);
        let mut recorder = Recorder::new(Arc::default(), node_dir.clone(), main_tx);
        recorder.tool_call("agent reviewer: Review".to_string());
        let mut child = recorder.subagent(agent_dir.clone());

        child.begin_answer();
        child.text("Reading.");
        child.answer_calls_tools();
        child.tool_call("read a.txt".to_string());
        child.begin_answer();
        child.text("Looks fine.");
        child.flush_reply();

        let read = |dir: &Utf8Path, name: &str| std::fs::read_to_string(dir.join(name)).unwrap();
        assert_eq!(read(&agent_dir, "activity.md"), "Reading.\n- read a.txt\n");
        assert_eq!(read(&agent_dir, "reply.md"), "Looks fine.");
        assert_eq!(
            read(&node_dir, "activity.md"),
            "- agent reviewer: Review\n  - read a.txt\n"
        );
        assert!(recorder.live.lock().reply.is_empty());
    }

    #[test]
    fn thinking_of_tool_calling_answers_is_dropped_and_the_last_one_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let (main_tx, _main_rx) = mpsc::channel(8);
        let node_dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let mut recorder = Recorder::new(Arc::default(), node_dir, main_tx);

        recorder.begin_answer();
        recorder.reasoning("should I look?");
        recorder.text("Let me look.\n");
        recorder.answer_calls_tools();
        recorder.tool_call("read a.txt".to_string());
        recorder.begin_answer();
        recorder.reasoning("deep thought");
        recorder.text("Done.");
        recorder.flush_reply();

        let live = recorder.live.lock();
        assert_eq!(live.thinking, "deep thought");
        assert_eq!(live.reply, "Done.");
        assert_eq!(live.visible_work_log().len(), 1);
        assert_eq!(live.log_start, 1);
    }

    #[test]
    fn the_running_log_hides_the_dropped_rounds() {
        let tmp = tempfile::tempdir().unwrap();
        let (main_tx, _main_rx) = mpsc::channel(8);
        let node_dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let mut recorder = Recorder::new(Arc::default(), node_dir, main_tx);

        // First tool-calling round
        recorder.begin_answer();
        recorder.reasoning("think 1");
        recorder.text("answer 1");
        recorder.answer_calls_tools();
        recorder.tool_call("tool 1".to_string());
        // Second tool-calling round
        recorder.begin_answer();
        recorder.reasoning("think 2");
        recorder.text("answer 2");
        recorder.answer_calls_tools();
        recorder.tool_call("tool 2".to_string());
        // Final answer
        recorder.begin_answer();
        recorder.reasoning("think 3");
        recorder.text("Done.");
        recorder.flush_reply();

        let live = recorder.live.lock();
        assert_eq!(live.work_log.len(), 4);
        assert_eq!(live.log_start, 3);
        assert_eq!(live.visible_work_log().len(), 1);
    }

    #[test]
    fn closing_a_clean_turn_lists_its_commits_oldest_first() {
        let (_tmp, root) = temp_repo();
        let before = commit(&root, "a.txt", "1", "init");
        let first = commit(&root, "a.txt", "2", "feat: one");
        let second = commit(&root, "b.txt", "x", "feat: two");

        let (commits, reset) = close_commit_turn(&root, "node", Some(&before), Some(&second));
        assert_eq!(commits, [first, second]);
        assert!(!reset);
    }

    #[test]
    fn closing_a_turn_that_changed_nothing_records_nothing() {
        let (_tmp, root) = temp_repo();
        let head = commit(&root, "a.txt", "1", "init");
        let (commits, reset) = close_commit_turn(&root, "node", Some(&head), Some(&head));
        assert!(commits.is_empty());
        assert!(!reset);
    }

    #[test]
    fn closing_records_the_new_commit_of_an_amend() {
        let (_tmp, root) = temp_repo();
        let base = commit(&root, "a.txt", "1", "init");
        let before = commit(&root, "a.txt", "2", "feat: one");
        std::fs::write(root.join("a.txt"), "3").unwrap();
        git(
            &root,
            &["commit", "-q", "-a", "--amend", "-m", "feat: one, amended"],
        );
        let amended = git(&root, &["rev-parse", "HEAD"]);
        assert_ne!(amended, before);

        let (commits, _) = close_commit_turn(&root, "node", Some(&before), Some(&amended));
        assert_eq!(commits, [amended]);
        assert_eq!(git(&root, &["rev-parse", "HEAD~1"]), base);
    }

    #[test]
    fn closing_a_clean_feedback_turn_touches_nothing() {
        let (_tmp, root) = temp_repo();
        let head = commit(&root, "a.txt", "1", "init");

        assert!(!discard_turn_work(&root));
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "1");
        assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    }

    #[test]
    fn closing_a_dirty_feedback_turn_discards_its_scratch() {
        let (_tmp, root) = temp_repo();
        let head = commit(&root, "a.txt", "1", "init");
        std::fs::write(root.join("a.txt"), "scratch").unwrap();
        std::fs::write(root.join("note.txt"), "untracked").unwrap();

        assert!(discard_turn_work(&root));
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "1");
        assert!(!root.join("note.txt").exists());
        assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
        assert_eq!(crate::git::status_short(&root).unwrap(), "");
    }

    #[test]
    fn closing_a_dirty_turn_discards_tracked_and_untracked_leftovers() {
        let (_tmp, root) = temp_repo();
        let before = commit(&root, "a.txt", "1", "init");
        std::fs::write(root.join("a.txt"), "changed").unwrap();
        std::fs::write(root.join("new.txt"), "untracked").unwrap();

        let (commits, reset) = close_commit_turn(&root, "node-7", Some(&before), Some(&before));
        assert!(commits.is_empty());
        assert!(reset);
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "1");
        assert!(!root.join("new.txt").exists());
        assert_eq!(git(&root, &["rev-parse", "HEAD"]), before);
        assert_eq!(crate::git::status_short(&root).unwrap(), "");
        assert!(git(&root, &["stash", "list"]).is_empty());
    }

    #[test]
    fn closing_a_turn_that_committed_keeps_the_commit_and_discards_the_rest() {
        let (_tmp, root) = temp_repo();
        let before = commit(&root, "a.txt", "1", "init");
        std::fs::write(root.join("a.txt"), "2").unwrap();
        git(&root, &["commit", "-q", "-a", "-m", "feat: one"]);
        let after = git(&root, &["rev-parse", "HEAD"]);
        std::fs::write(root.join("a.txt"), "3").unwrap();
        std::fs::write(root.join("new.txt"), "untracked").unwrap();

        let (commits, reset) = close_commit_turn(&root, "node", Some(&before), Some(&after));
        assert_eq!(commits, std::slice::from_ref(&after));
        assert!(reset);
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "2");
        assert!(!root.join("new.txt").exists());
        assert_eq!(git(&root, &["rev-parse", "HEAD"]), after);
        assert_eq!(crate::git::status_short(&root).unwrap(), "");
    }

    #[test]
    fn closing_the_first_commit_of_a_repository_lists_it() {
        let (_tmp, root) = temp_repo();
        let first = commit(&root, "a.txt", "1", "init");
        let (commits, _) = close_commit_turn(&root, "node", None, Some(&first));
        assert_eq!(commits, [first]);
    }

    #[test]
    fn context_length_errors_suggest_compacting() {
        let hinted = with_compact_hint("This model's maximum context length is 128000".into());
        assert!(hinted.ends_with("use /compact from the parent node"));
        assert_eq!(with_compact_hint("rate limited".into()), "rate limited");
    }

    #[test]
    fn parses_title_and_summary() {
        let (title, summary) =
            parse_title_and_summary("  Add a thing\n\nIt does this\nand that.\n").unwrap();
        assert_eq!(title, "Add a thing");
        assert_eq!(summary, "It does this and that.");
    }

    #[test]
    fn title_only_answer_has_empty_summary() {
        let (title, summary) = parse_title_and_summary("Just a title").unwrap();
        assert_eq!(title, "Just a title");
        assert_eq!(summary, "");
    }

    #[test]
    fn title_is_capped_and_empty_answer_rejected() {
        let (title, _) = parse_title_and_summary(&"x".repeat(300)).unwrap();
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS);
        assert!(parse_title_and_summary("  \n ").is_none());
    }

    #[test]
    fn title_input_truncates_reply_and_lists_commits() {
        let reply = "é".repeat(REPLY_MAX_CHARS + 10);
        let input = title_input("do it", &reply, &["abc1234 feat: x".to_string()]);
        assert!(input.starts_with("<prompt>\ndo it\n</prompt>\n\n<reply>\n"));
        assert_eq!(input.matches('é').count(), REPLY_MAX_CHARS);
        assert!(input.contains("\n<commits>\nabc1234 feat: x\n</commits>\n"));
        assert!(input.ends_with("recorded above.\n"));
    }
}
