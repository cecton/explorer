use std::collections::VecDeque;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

const REPLY_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
/// How often the spinners advance one frame.
pub const SPINNER_INTERVAL: Duration = Duration::from_millis(100);
pub const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const TITLE_MAX_CHARS: usize = 100;
pub(super) const REPLY_MAX_CHARS: usize = 8000;
const COMMITS_MAX: usize = 20;
/// Times a commit turn is asked to commit what it left behind before Explorer discards it.
const MAX_NUDGES: usize = 2;
/// Opens the line of a tool call in the work log.
pub const TOOL_LINE_PREFIX: &str = "- ";
/// Sets a sub-agent's tool calls apart in the work log of the turn that called it.
const SUBAGENT_INDENT: &str = "  ";

pub(super) type MainTx = tokio::sync::mpsc::Sender<
    r3bl_tui::TerminalWindowMainThreadSignal<crate::tui::state::AppSignal>,
>;

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
    /// The prompt block shown above the reply: the prompt as sent, with the messages
    /// the user sent while the turn was working appended.
    pub prompt: String,
    pub tokens_in: Option<u64>,
    /// Input tokens of the turn's first request the provider served from its prompt cache.
    pub cached_in: Option<u64>,
    /// Claude Code session holding this turn, once the CLI has announced it.
    pub session_id: Option<String>,
    /// What the CLI last reported of the subscription's limits.
    pub usage: Option<crate::agent::usage::Usage>,
    /// The sub-agent this turn is waiting for.
    pub subagent: Option<LiveSubagent>,
    /// What the user sent while the turn was working, waiting to join the loop at its
    /// next round.
    pub injected: VecDeque<String>,
    /// The turn takes no more messages: the Claude Code driver closed the CLI's stdin.
    pub closed: bool,
}

/// A sub-agent run in progress, as the turn that called it sees it.
#[derive(Clone)]
pub struct LiveSubagent {
    pub index: u32,
    pub started: Instant,
    pub live: Arc<parking_lot::Mutex<LiveTurn>>,
}

/// The spinner character for the shared animation step `tick`.
pub fn spinner_char(tick: u8) -> char {
    SPINNER[tick as usize % SPINNER.len()]
}

/// Main-thread handle on a running turn.
#[derive(Clone)]
pub struct TurnHandle {
    pub live: Arc<parking_lot::Mutex<LiveTurn>>,
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

impl LiveTurn {
    /// Index of the first work log entry of the current answer in progress.
    pub fn log_start(&self) -> usize {
        self.log_start.min(self.work_log.len())
    }

    /// Takes what the user sent while the turn was working.
    pub fn take_injected(&mut self) -> Vec<String> {
        self.injected.drain(..).collect()
    }
}

/// What the user sends while a turn is working. The own loop drains it; the Claude
/// Code driver writes it to the CLI's stdin. Either way it joins the prompt block,
/// the node's `prompt.md` and the transcript when it is sent.
#[derive(Clone)]
pub(super) struct Injector {
    live: Arc<parking_lot::Mutex<LiveTurn>>,
    node_dir: camino::Utf8PathBuf,
}

impl Injector {
    /// The message at the front of the queue, which stays there until it is written.
    pub(super) fn front(&self) -> Option<String> {
        self.live.lock().injected.front().cloned()
    }

    /// Whether a message is waiting.
    pub(super) fn pending(&self) -> bool {
        !self.live.lock().injected.is_empty()
    }

    /// Closes the turn to new messages when none waits and `settled` (a result came
    /// after the last write). The check and the flag share one lock, so a message
    /// either is seen here or is refused by the window.
    pub(super) fn close_if_idle(&self, settled: bool) -> bool {
        let mut live = self.live.lock();
        let idle = live.injected.is_empty() && settled;
        if idle {
            live.closed = true;
        }
        idle
    }

    /// Closes the turn to new messages whatever waits: the CLI's stdin is gone.
    pub(super) fn close(&self) {
        self.live.lock().closed = true;
    }

    /// Records one as written and takes it out of the queue. It leaves the queue only
    /// here, once the model has it: a write that fails, or a task dropped mid-write,
    /// leaves the message for the window to give back.
    pub(super) fn wrote(&self, content: &str) -> crate::agent::Message {
        {
            let mut live = self.live.lock();
            if live.injected.front().map(String::as_str) == Some(content) {
                live.injected.pop_front();
            }
        }
        self.sent(content)
    }

    /// Records one as sent: it joins the prompt block, `prompt.md` and the transcript,
    /// and is handed back for the loop to push onto its messages.
    fn sent(&self, content: &str) -> crate::agent::Message {
        use crate::agent::Message;
        {
            let mut live = self.live.lock();
            if !live.prompt.is_empty() && !live.prompt.ends_with("\n\n") {
                live.prompt.push_str("\n\n");
            }
            live.prompt.push_str(content);
        }
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.node_dir.join("prompt.md"))
            .and_then(|mut file| write!(file, "\n\n{content}"));
        if let Err(e) = appended {
            tracing::warn!("failed to append to prompt.md: {e}");
        }
        let message = Message::User {
            content: content.to_string(),
            injected: false,
        };
        append_transcript(&self.node_dir, &message);
        message
    }
}

/// How a turn ended, applied to the node's metadata on the main thread.
#[derive(Clone, Debug)]
pub struct TurnOutcome {
    pub status: crate::conversation::NodeStatus,
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
    pub spec: crate::agent::ModelSpec,
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
        let (_, summary) = crate::conversation::read_prompt_and_reply(dir);
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

/// Collects a turn's output: live reply and work log for the UI, `reply.md`,
/// `activity.md` and `transcript.jsonl` on disk.
pub struct Recorder {
    pub(super) live: Arc<parking_lot::Mutex<LiveTurn>>,
    node_dir: camino::Utf8PathBuf,
    main_tx: MainTx,
    /// The recorder of the turn that called this sub-agent, which logs its tool calls
    /// too.
    caller: Option<Caller>,
    /// Where the text of the current answer starts in the live reply.
    answer_start: usize,
    last_flush: Instant,
    /// Chars of thinking recorded for the answer in progress; lets the recorder drop
    /// the live thinking when the answer turns out to call tools.
    thinking_start: usize,
}

/// Where a sub-agent's tool calls are copied: the work log of the calling turn.
struct Caller {
    live: Arc<parking_lot::Mutex<LiveTurn>>,
    node_dir: camino::Utf8PathBuf,
}

/// Appends one message to the `transcript.jsonl` of `dir`, which is what a child node
/// replays.
fn append_transcript(dir: &camino::Utf8Path, message: &crate::agent::Message) {
    let appended = serde_json::to_string(message)
        .map_err(std::io::Error::other)
        .and_then(|line| {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("transcript.jsonl"))?;
            writeln!(file, "{line}")
        });
    if let Err(e) = appended {
        tracing::warn!("failed to append to transcript.jsonl: {e}");
    }
}

/// Appends one entry to a work log and to the `activity.md` of `dir`.
fn append_work_log(dir: &camino::Utf8Path, live: &parking_lot::Mutex<LiveTurn>, entry: String) {
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
    fn new(
        live: Arc<parking_lot::Mutex<LiveTurn>>,
        node_dir: camino::Utf8PathBuf,
        main_tx: MainTx,
    ) -> Self {
        Self {
            live,
            node_dir,
            main_tx,
            caller: None,
            answer_start: 0,
            last_flush: Instant::now(),
            thinking_start: 0,
        }
    }

    /// The recorder of a sub-agent run kept in `dir`, whose tool calls also go to this
    /// turn's work log.
    pub(super) fn subagent(&self, dir: camino::Utf8PathBuf) -> Self {
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

    pub(super) async fn send(&self, signal: crate::tui::state::AppSignal) {
        let _ = self
            .main_tx
            .send(r3bl_tui::TerminalWindowMainThreadSignal::ApplyAppSignal(
                signal,
            ))
            .await;
    }

    pub(super) fn flush_reply(&mut self) {
        self.last_flush = Instant::now();
        let reply = self.live.lock().reply.clone();
        if let Err(e) = crate::util::fs::write_atomic(&self.node_dir.join("reply.md"), &reply) {
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
        use crate::agent::Message;
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
    }

    /// Thinking of the answer in progress; shown dimmed and never recorded.
    pub fn reasoning(&mut self, chunk: &str) {
        self.live.lock().thinking.push_str(chunk);
    }

    /// Appends one message to `transcript.jsonl`.
    pub fn message(&mut self, message: &crate::agent::Message) {
        append_transcript(&self.node_dir, message);
    }

    /// The injector of this turn, for what the user sends while it works.
    pub(super) fn injector(&self) -> Injector {
        Injector {
            live: Arc::clone(&self.live),
            node_dir: self.node_dir.clone(),
        }
    }

    pub fn activity(&mut self, activity: Option<String>) {
        self.live.lock().activity = activity;
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

    pub fn usage(&mut self, usage: crate::agent::usage::Usage) {
        self.live.lock().usage = Some(usage);
    }
}

/// Tells the UI what the spending behind `endpoint` is: what the CLI last `reported`
/// for a `claude-code` endpoint, or what OpenCode Go, Kimi Code, Cline or OpenRouter
/// answer now. Called after every turn, and for an OpenRouter key at startup and every
/// minute between turns, since its counter settles seconds after the requests that
/// spent it.
pub fn report_usage(
    endpoint: crate::agent::Endpoint,
    reported: Option<crate::agent::usage::Usage>,
    main_tx: MainTx,
) {
    use crate::agent::EndpointKind;
    use crate::tui::state::AppSignal;
    tokio::spawn(async move {
        let usage = match &endpoint.kind {
            EndpointKind::ClaudeCode { .. } => reported,
            EndpointKind::OpenCodeGo { url, token } => crate::agent::usage::fetch(url, token)
                .await
                .map_err(|e| tracing::warn!("usage of '{}' failed: {e}", endpoint.name))
                .ok(),
            EndpointKind::KimiCode { url, token } => crate::agent::usage::fetch_kimi(url, token)
                .await
                .map_err(|e| tracing::warn!("usage of '{}' failed: {e}", endpoint.name))
                .ok(),
            EndpointKind::Cline { url, token } => crate::agent::usage::fetch_cline(url, token)
                .await
                .map_err(|e| tracing::warn!("usage of '{}' failed: {e}", endpoint.name))
                .ok(),
            EndpointKind::OpenRouter { url, token } => {
                crate::agent::usage::fetch_openrouter(url, token)
                    .await
                    .map_err(|e| tracing::warn!("usage of '{}' failed: {e}", endpoint.name))
                    .ok()
            }
            EndpointKind::OpenAi { .. } => None,
        };
        let Some(usage) = usage else {
            return;
        };
        let _ = main_tx
            .send(r3bl_tui::TerminalWindowMainThreadSignal::ApplyAppSignal(
                AppSignal::UsageFetched {
                    endpoint: endpoint.name,
                    usage,
                },
            ))
            .await;
    });
}

/// Starts the turn of a node already created on disk and returns its handle. The
/// outcome arrives as `AppSignal::TurnFinished`.
pub fn spawn(request: TurnRequest, main_tx: MainTx) -> TurnHandle {
    use crate::agent::Mode;
    use crate::conversation::NodeStatus;
    use crate::tui::state::AppSignal;
    let live = Arc::new(parking_lot::Mutex::new(LiveTurn::default()));
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
            Recorder::new(Arc::clone(&live), request.node_dir.clone(), main_tx.clone());
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
            (status, error) =
                crate::conversation::cut_short_outcome(&recorder.live.lock().reply, &commits);
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
            .send(r3bl_tui::TerminalWindowMainThreadSignal::ApplyAppSignal(
                AppSignal::TurnFinished {
                    node: request.node.clone(),
                    outcome,
                },
            ))
            .await;
        report_usage(request.endpoint.clone(), reported_usage, main_tx.clone());
        let reply_blank = recorder.live.lock().reply.trim().is_empty();
        if (status == NodeStatus::Done || !reply_blank || !commits.is_empty())
            && let Some(backend) = &request.summary_backend
        {
            retitle(
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

async fn run(request: &TurnRequest, recorder: &mut Recorder) -> Result<(), String> {
    use crate::agent::Backend;
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

/// The parent session a turn forks. Only feedback turns fork: a fork replays the parent's
/// earlier user messages, which carry the feedback-turn workspace note.
fn fork_source(mode: crate::agent::Mode, parent_session: Option<&str>) -> Option<&str> {
    use crate::agent::Mode;
    parent_session.filter(|_| mode == Mode::Feedback)
}

async fn run_claude_code(
    request: &TurnRequest,
    mcp: &crate::agent::mcp::McpTools,
    target: &crate::agent::claude_code::ClaudeTarget,
    recorder: &mut Recorder,
) -> Result<(), String> {
    use crate::agent::Message;
    use crate::agent::Mode;
    use crate::agent::claude_code::Resume;
    use crate::agent::claude_code::RunError;
    let (system, message, preamble) = {
        let request = request.clone();
        tokio::task::spawn_blocking(move || {
            let system = crate::agent::context::claude_system_prompt(
                request.system_prompt.then_some(request.mode),
                request.agent.review_rounds,
            );
            let preamble = crate::agent::context::handover_preamble(&request.ancestor_dirs);
            (system, message_to_send(&request), preamble)
        })
        .await
        .map_err(|e| e.to_string())?
    };
    // A continue turn starts from the node's own session, which holds its thinking and
    // its tool state. It forks rather than appending in place: two resumes of one
    // interrupted node are two independent branches of the tree, and a fork keeps them
    // from reading each other's work. When the session is gone, a fresh one opens with
    // the ancestors as plain text.
    if let Some(continue_session) = &request.continue_session {
        let run = crate::agent::claude_code::run(
            &invocation(
                request,
                mcp,
                target,
                &system,
                &message,
                Resume::Fork(continue_session),
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
                crate::agent::claude_code::run(
                    &invocation(request, mcp, target, &system, &opening, Resume::Fresh),
                    recorder,
                )
                .await
                .map_err(run_error_text)?;
            }
            Err(RunError::Failed(error)) => return Err(error),
        }
    } else {
        // Feedback turns fork the parent's session when there is one; commit turns start
        // fresh so they never see the replayed feedback history. A fork that is gone, or
        // no parent, starts a fresh session that opens with the ancestors as plain text.
        let forked = match fork_source(request.mode, request.parent_session.as_deref()) {
            Some(session) => {
                match crate::agent::claude_code::run(
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
            crate::agent::claude_code::run(
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
        let Message::User { content: nudge, .. } = crate::agent::context::commit_nudge(&status)
        else {
            break;
        };
        crate::agent::claude_code::run(
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
    mcp: &'a crate::agent::mcp::McpTools,
    target: &'a crate::agent::claude_code::ClaudeTarget,
    system: &'a str,
    prompt: &'a str,
    resume: crate::agent::claude_code::Resume<'a>,
) -> crate::agent::claude_code::Invocation<'a> {
    use crate::agent::claude_code::Invocation;
    Invocation {
        target,
        root: &request.root,
        system,
        prompt,
        resume,
        mcp,
    }
}

fn run_error_text(error: crate::agent::claude_code::RunError) -> String {
    use crate::agent::claude_code::RunError;
    match error {
        RunError::SessionGone => "the Claude Code session of this turn is gone".to_string(),
        RunError::Failed(error) => error,
    }
}

async fn run_own_loop(
    request: &TurnRequest,
    mcp: &crate::agent::mcp::McpTools,
    target: &crate::agent::llm::LlmTarget,
    spec: &crate::agent::ModelSpec,
    recorder: &mut Recorder,
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
    for _ in 0..MAX_NUDGES {
        let Some(status) = uncommitted(request).await? else {
            break;
        };
        let nudge = crate::agent::context::commit_nudge(&status);
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

/// The commits a commit turn made; what it left uncommitted is discarded.
fn close_commit_turn(
    root: &camino::Utf8Path,
    node: &str,
    head_before: Option<&str>,
    head_after: Option<&str>,
) -> (Vec<String>, bool) {
    let commits = crate::git::commits_since(root, head_before, head_after);
    if commits.is_empty() && head_before != head_after {
        tracing::warn!("commit turn {node} moved HEAD but listed no commits");
    }
    (commits, discard_turn_work(root))
}

/// Discards what the turn left uncommitted; the tree was clean when the turn started.
fn discard_turn_work(root: &camino::Utf8Path) -> bool {
    match crate::git::status_short(root) {
        Ok(status) if !status.trim().is_empty() => crate::git::discard_worktree(root)
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
pub(super) fn parse_title_and_summary(answer: &str) -> Option<(String, String)> {
    let answer = answer.trim();
    let (first, rest) = answer.split_once('\n').unwrap_or((answer, ""));
    let title: String = crate::util::text::single_line(first)
        .chars()
        .take(TITLE_MAX_CHARS)
        .collect();
    (!title.is_empty()).then(|| (title, crate::util::text::single_line(rest)))
}

/// One answer without tools from the summary model.
pub(super) async fn complete(
    backend: &crate::agent::Backend,
    session: &str,
    system: &str,
    user: &str,
) -> Result<String, String> {
    use crate::agent::Backend;
    match backend {
        Backend::OpenAi(target) => crate::agent::llm::complete(target, session, system, user).await,
        Backend::ClaudeCode(target) => {
            crate::agent::claude_code::complete(target, system, user).await
        }
    }
}

/// Writes the title and summary of a node from its prompt, reply and commits.
#[allow(clippy::too_many_arguments)]
pub async fn retitle(
    node: &crate::conversation::NodeId,
    node_dir: &camino::Utf8Path,
    session: &str,
    root: &camino::Utf8Path,
    head_before: Option<String>,
    head_after: Option<String>,
    backend: &crate::agent::Backend,
    main_tx: &MainTx,
) {
    let root = root.to_path_buf();
    let commits = tokio::task::spawn_blocking(move || match (head_before, head_after) {
        (Some(before), Some(after)) => crate::git::log_oneline(&root, &before, &after, COMMITS_MAX),
        _ => Vec::new(),
    })
    .await
    .unwrap_or_default();
    let (prompt, reply) = crate::conversation::read_prompt_and_reply(node_dir);
    let input = title_input(&prompt, &reply, &commits);
    title_and_summarize(node, node_dir, session, backend, &input, main_tx).await;
}

/// Replaces `title.md` and `summary.md` of a finished node with what the model makes of
/// its turn.
async fn title_and_summarize(
    node: &crate::conversation::NodeId,
    node_dir: &camino::Utf8Path,
    session: &str,
    backend: &crate::agent::Backend,
    input: &str,
    main_tx: &MainTx,
) {
    use crate::tui::state::AppSignal;
    let answer = match complete(
        backend,
        session,
        crate::agent::context::TITLE_SUMMARY_PROMPT,
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
    if let Err(e) = crate::util::fs::write_atomic(&node_dir.join("title.md"), &title) {
        tracing::warn!("failed to write title.md: {e}");
    }
    let signal = AppSignal::NodeTitleReady {
        node: node.clone(),
        title,
    };
    let _ = main_tx
        .send(r3bl_tui::TerminalWindowMainThreadSignal::ApplyAppSignal(
            signal,
        ))
        .await;
    if summary.is_empty() {
        return;
    }
    if let Err(e) = crate::util::fs::write_atomic(&node_dir.join("summary.md"), &summary) {
        tracing::warn!("failed to write summary.md: {e}");
    }
    let signal = AppSignal::NodeSummaryReady {
        node: node.clone(),
        summary,
    };
    let _ = main_tx
        .send(r3bl_tui::TerminalWindowMainThreadSignal::ApplyAppSignal(
            signal,
        ))
        .await;
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
    fn a_turn_closes_only_when_idle_and_settled() {
        let live = Arc::new(parking_lot::Mutex::new(LiveTurn::default()));
        let injector = Injector {
            live: Arc::clone(&live),
            node_dir: camino::Utf8PathBuf::new(),
        };
        assert!(!injector.close_if_idle(false));
        assert!(!live.lock().closed);
        live.lock().injected.push_back("late".to_string());
        assert!(!injector.close_if_idle(true));
        assert!(!live.lock().closed);
        live.lock().injected.clear();
        assert!(injector.close_if_idle(true));
        assert!(live.lock().closed);
    }

    #[test]
    fn a_message_leaves_the_queue_only_once_it_is_written() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let live = Arc::new(parking_lot::Mutex::new(LiveTurn::default()));
        let injector = Injector {
            live: Arc::clone(&live),
            node_dir: dir,
        };
        live.lock().injected.push_back("one".to_string());
        live.lock().injected.push_back("two".to_string());

        assert_eq!(injector.front().as_deref(), Some("one"));
        injector.wrote("one");
        assert_eq!(injector.front().as_deref(), Some("two"));
        // What was never written is still there for the window to give back.
        assert_eq!(live.lock().take_injected(), ["two"]);
    }

    #[test]
    fn a_dead_stdin_closes_the_turn_whatever_waits() {
        let live = Arc::new(parking_lot::Mutex::new(LiveTurn::default()));
        let injector = Injector {
            live: Arc::clone(&live),
            node_dir: camino::Utf8PathBuf::new(),
        };
        // A message still waiting must not keep the turn open to more: the CLI is gone.
        live.lock().injected.push_back("unsent".to_string());
        injector.close();
        assert!(live.lock().closed);
    }

    #[test]
    fn an_injected_message_joins_the_prompt_block_and_the_transcript() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        std::fs::write(dir.join("prompt.md"), "the first prompt").unwrap();
        let live = Arc::new(parking_lot::Mutex::new(LiveTurn {
            prompt: "the first prompt".to_string(),
            ..Default::default()
        }));
        let injector = Injector {
            live: Arc::clone(&live),
            node_dir: dir.clone(),
        };

        injector.sent("also handle the parser");

        assert_eq!(
            live.lock().prompt,
            "the first prompt\n\nalso handle the parser"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("prompt.md")).unwrap(),
            "the first prompt\n\nalso handle the parser"
        );
        let transcript = std::fs::read_to_string(dir.join("transcript.jsonl")).unwrap();
        assert_eq!(
            transcript,
            "{\"role\":\"user\",\"content\":\"also handle the parser\"}\n"
        );
    }

    #[test]
    fn a_cancel_with_content_stays_cancelled() {
        use crate::conversation::NodeStatus;
        let (status, error) = crate::conversation::cut_short_outcome("a partial answer", &[]);
        assert_eq!(status, NodeStatus::Cancelled);
        assert_eq!(error, None);
        let (status, error) = crate::conversation::cut_short_outcome("", &["abc1234".to_string()]);
        assert_eq!(status, NodeStatus::Cancelled);
        assert_eq!(error, None);
        let (status, error) = crate::conversation::cut_short_outcome("  \n  ", &[]);
        assert_eq!(status, NodeStatus::Failed);
        assert_eq!(error.as_deref(), Some(crate::conversation::CANCELLED_ERROR));
    }

    fn git(root: &camino::Utf8Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn temp_repo() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "test"]);
        git(&root, &["config", "user.email", "test@example.com"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        (tmp, root)
    }

    fn commit(root: &camino::Utf8Path, file: &str, content: &str, message: &str) -> String {
        std::fs::write(root.join(file), content).unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", message]);
        git(root, &["rev-parse", "HEAD"])
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
    fn cache_hits_are_those_of_the_first_request() {
        let tmp = tempfile::tempdir().unwrap();
        let node_dir = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let (main_tx, _main_rx) = tokio::sync::mpsc::channel(8);
        let live = Arc::default();
        let mut recorder = Recorder::new(Arc::clone(&live), node_dir, main_tx);

        recorder.cached_in(Some(0));
        recorder.cached_in(Some(20_000));
        assert_eq!(live.lock().cached_in, Some(0));
    }

    #[test]
    fn text_of_an_answer_that_calls_tools_moves_to_the_work_log() {
        let tmp = tempfile::tempdir().unwrap();
        let node_dir = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let (main_tx, _main_rx) = tokio::sync::mpsc::channel(8);
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
        let node_dir = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let agent_dir = node_dir.join("agents/1");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let (main_tx, _main_rx) = tokio::sync::mpsc::channel(8);
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

        let read =
            |dir: &camino::Utf8Path, name: &str| std::fs::read_to_string(dir.join(name)).unwrap();
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
        let (main_tx, _main_rx) = tokio::sync::mpsc::channel(8);
        let node_dir = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
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
        assert_eq!(live.work_log.len() - live.log_start(), 1);
        assert_eq!(live.log_start, 1);
    }

    #[test]
    fn the_running_log_hides_the_dropped_rounds() {
        let tmp = tempfile::tempdir().unwrap();
        let (main_tx, _main_rx) = tokio::sync::mpsc::channel(8);
        let node_dir = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
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
        assert_eq!(live.work_log.len() - live.log_start(), 1);
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
    fn a_commit_turn_cut_short_keeps_its_commits() {
        use crate::conversation::NodeStatus;
        let (_tmp, root) = temp_repo();
        let before = commit(&root, "a.txt", "1", "init");
        let made = commit(&root, "a.txt", "2", "feat: one");

        let (commits, _) = close_commit_turn(&root, "node", Some(&before), Some(&made));
        let (status, error) = crate::conversation::cut_short_outcome("", &commits);
        assert_eq!(commits, [made]);
        assert_eq!(status, NodeStatus::Cancelled);
        assert_eq!(error, None);
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
