//! What a running turn shows and records: the `LiveTurn` a Discussion window reads at
//! render time, the `Recorder` that feeds it and its files, and the `Injector` a message
//! from the user travels through.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

const REPLY_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
/// Opens the line of a tool call in the work log.
pub const TOOL_LINE_PREFIX: &str = "- ";
/// Sets a sub-agent's tool calls apart in the work log of the turn that called it.
const SUBAGENT_INDENT: &str = "  ";

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
    pub usage: Option<crate::usage::Usage>,
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
pub(crate) struct Injector {
    live: Arc<parking_lot::Mutex<LiveTurn>>,
    node_dir: camino::Utf8PathBuf,
}

impl Injector {
    /// The message at the front of the queue, which stays there until it is written.
    pub(crate) fn front(&self) -> Option<String> {
        self.live.lock().injected.front().cloned()
    }

    /// Whether a message is waiting.
    pub(crate) fn pending(&self) -> bool {
        !self.live.lock().injected.is_empty()
    }

    /// Closes the turn to new messages when none waits and `settled` (a result came
    /// after the last write). The check and the flag share one lock, so a message
    /// either is seen here or is refused by the window.
    pub(crate) fn close_if_idle(&self, settled: bool) -> bool {
        let mut live = self.live.lock();
        let idle = live.injected.is_empty() && settled;
        if idle {
            live.closed = true;
        }
        idle
    }

    /// Closes the turn to new messages whatever waits: the CLI's stdin is gone.
    pub(crate) fn close(&self) {
        self.live.lock().closed = true;
    }

    /// Records one as written and takes it out of the queue. It leaves the queue only
    /// here, once the model has it: a write that fails, or a task dropped mid-write,
    /// leaves the message for the window to give back.
    pub(crate) fn wrote(&self, content: &str) -> crate::agent::Message {
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
            .open(self.node_dir.join(crate::conversation::files::PROMPT))
            .and_then(|mut file| write!(file, "\n\n{content}"));
        if let Err(e) = appended {
            tracing::warn!("failed to append to the prompt of {}: {e}", self.node_dir);
        }
        let message = Message::User {
            content: content.to_string(),
            injected: false,
        };
        append_transcript(&self.node_dir, &message);
        message
    }
}

/// Collects a turn's output: live reply and work log for the UI, `reply.md`,
/// `activity.md` and `transcript.jsonl` on disk.
pub struct Recorder {
    pub(crate) live: Arc<parking_lot::Mutex<LiveTurn>>,
    node_dir: camino::Utf8PathBuf,
    main_tx: crate::signal::MainTx,
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
                .open(dir.join(crate::conversation::files::TRANSCRIPT))?;
            writeln!(file, "{line}")
        });
    if let Err(e) = appended {
        tracing::warn!("failed to append to the transcript of {dir}: {e}");
    }
}

/// Appends one entry to a work log and to the `activity.md` of `dir`.
fn append_work_log(dir: &camino::Utf8Path, live: &parking_lot::Mutex<LiveTurn>, entry: String) {
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(crate::conversation::files::ACTIVITY))
        .and_then(|mut file| writeln!(file, "{entry}"));
    if let Err(e) = appended {
        tracing::warn!("failed to append to the work log of {dir}: {e}");
    }
    live.lock().work_log.push(entry);
}

impl Recorder {
    pub(super) fn new(
        live: Arc<parking_lot::Mutex<LiveTurn>>,
        node_dir: camino::Utf8PathBuf,
        main_tx: crate::signal::MainTx,
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
    pub(crate) fn subagent(&self, dir: camino::Utf8PathBuf) -> Self {
        Self {
            caller: Some(Caller {
                live: Arc::clone(&self.live),
                node_dir: self.node_dir.clone(),
            }),
            ..Self::new(Arc::default(), dir, self.main_tx.clone())
        }
    }

    pub(crate) fn main_tx(&self) -> crate::signal::MainTx {
        self.main_tx.clone()
    }

    pub(crate) fn send(&self, signal: crate::signal::AppSignal) {
        crate::signal::send(&self.main_tx, signal);
    }

    pub(crate) fn flush_reply(&mut self) {
        self.last_flush = Instant::now();
        let reply = self.live.lock().reply.clone();
        if let Err(e) = crate::conversation::files::write_reply(&self.node_dir, &reply) {
            tracing::warn!("failed to write the reply of {}: {e}", self.node_dir);
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
    pub(crate) fn log(&mut self, entry: String) {
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
    pub(crate) fn injector(&self) -> Injector {
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

    pub fn usage(&mut self, usage: crate::usage::Usage) {
        self.live.lock().usage = Some(usage);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        (tmp, dir)
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
        let (_tmp, dir) = temp_dir();
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
        let (_tmp, dir) = temp_dir();
        std::fs::write(
            dir.join(crate::conversation::files::PROMPT),
            "the first prompt",
        )
        .unwrap();
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
            std::fs::read_to_string(dir.join(crate::conversation::files::PROMPT)).unwrap(),
            "the first prompt\n\nalso handle the parser"
        );
        let transcript =
            std::fs::read_to_string(dir.join(crate::conversation::files::TRANSCRIPT)).unwrap();
        assert_eq!(
            transcript,
            "{\"role\":\"user\",\"content\":\"also handle the parser\"}\n"
        );
    }

    #[test]
    fn cache_hits_are_those_of_the_first_request() {
        let (_tmp, node_dir) = temp_dir();
        let (main_tx, _main_rx) = tokio::sync::mpsc::channel(8);
        let live = Arc::default();
        let mut recorder = Recorder::new(Arc::clone(&live), node_dir, main_tx);

        recorder.cached_in(Some(0));
        recorder.cached_in(Some(20_000));
        assert_eq!(live.lock().cached_in, Some(0));
    }

    #[test]
    fn text_of_an_answer_that_calls_tools_moves_to_the_work_log() {
        let (_tmp, node_dir) = temp_dir();
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
        assert_eq!(
            read(crate::conversation::files::REPLY),
            "First part.\n\nCommitted."
        );
        assert_eq!(
            read(crate::conversation::files::ACTIVITY),
            "Let me look.\n- shell ls\n- read a.txt\n- shell git commit\n"
        );
        let live = recorder.live.lock();
        assert_eq!(live.reply, "First part.\n\nCommitted.");
        assert_eq!(live.work_log.len(), 4);
        assert_eq!(live.activity.as_deref(), Some("shell git commit"));
    }

    #[test]
    fn a_sub_agent_logs_its_tool_calls_in_the_work_log_of_its_caller_too() {
        let (_tmp, node_dir) = temp_dir();
        let agent_dir = node_dir
            .join(crate::conversation::files::AGENTS_DIR)
            .join("1");
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
        assert_eq!(
            read(&agent_dir, crate::conversation::files::ACTIVITY),
            "Reading.\n- read a.txt\n"
        );
        assert_eq!(
            read(&agent_dir, crate::conversation::files::REPLY),
            "Looks fine."
        );
        assert_eq!(
            read(&node_dir, crate::conversation::files::ACTIVITY),
            "- agent reviewer: Review\n  - read a.txt\n"
        );
        assert!(recorder.live.lock().reply.is_empty());
    }

    #[test]
    fn thinking_of_tool_calling_answers_is_dropped_and_the_last_one_kept() {
        let (_tmp, node_dir) = temp_dir();
        let (main_tx, _main_rx) = tokio::sync::mpsc::channel(8);
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
        let (_tmp, node_dir) = temp_dir();
        let (main_tx, _main_rx) = tokio::sync::mpsc::channel(8);
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
}
