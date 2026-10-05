use super::llm::{self, LlmTarget};
use super::{Message, Mode, agent_loop, context};
use crate::conversation::{NodeId, NodeStatus, write_atomic};
use crate::tui::AppSignal;
use camino::{Utf8Path, Utf8PathBuf};
use parking_lot::Mutex;
use r3bl_tui::TerminalWindowMainThreadSignal;
use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

const REPLY_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
const PROGRESS_SIGNAL_INTERVAL: Duration = Duration::from_millis(80);
const TITLE_MAX_CHARS: usize = 100;
const SUMMARY_MAX_WORDS: usize = 50;
/// Times a commit turn is asked to commit what it left behind before Explorer stashes it.
const MAX_NUDGES: usize = 2;

type MainTx = mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>;

/// What a running turn has produced so far, read by Discussion windows at render time.
#[derive(Default)]
pub struct LiveTurn {
    pub reply: String,
    /// The tool call in progress, as shown on the status line.
    pub activity: Option<String>,
    pub tokens_in: Option<u64>,
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

/// How a turn ended, applied to the node's metadata on the main thread.
#[derive(Clone, Debug)]
pub struct TurnOutcome {
    pub status: NodeStatus,
    pub error: Option<String>,
    pub head_after: Option<String>,
    pub tokens_in: Option<u64>,
    /// Commits the turn produced, oldest first.
    pub commits: Vec<String>,
    /// Stash holding what the turn left uncommitted.
    pub stash: Option<String>,
}

pub struct TurnRequest {
    pub mode: Mode,
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
    pub target: LlmTarget,
}

/// Collects a turn's output: live reply for the UI, `reply.md` and `transcript.jsonl`
/// on disk.
pub struct Recorder {
    live: Arc<Mutex<LiveTurn>>,
    node_dir: Utf8PathBuf,
    main_tx: MainTx,
    last_flush: Instant,
    last_signal: Instant,
}

impl Recorder {
    fn signal(&mut self) {
        self.last_signal = Instant::now();
        let _ = self
            .main_tx
            .try_send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                AppSignal::TurnProgress,
            ));
    }

    fn flush_reply(&mut self) {
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

    tokio::spawn(async move {
        let mut recorder = Recorder {
            live: Arc::clone(&live),
            node_dir: request.node_dir.clone(),
            main_tx: main_tx.clone(),
            last_flush: Instant::now(),
            last_signal: Instant::now(),
        };
        let (status, error) = tokio::select! {
            result = run(&request, &mut recorder) => match result {
                Ok(()) => (NodeStatus::Done, None),
                Err(error) => (NodeStatus::Failed, Some(error)),
            },
            _ = cancel_rx.wait_for(|cancelled| *cancelled) => (NodeStatus::Cancelled, None),
        };
        recorder.flush_reply();
        let head_after = request.repo.as_ref().and_then(crate::git::head_id);
        let (commits, stash) = match request.mode {
            Mode::Feedback => (Vec::new(), None),
            Mode::Commit => {
                let root = request.root.clone();
                let node = request.node.clone();
                let before = request.head.clone();
                let after = head_after.clone();
                tokio::task::spawn_blocking(move || {
                    close_commit_turn(&root, node.as_str(), before.as_deref(), after.as_deref())
                })
                .await
                .unwrap_or_default()
            }
        };
        let outcome = TurnOutcome {
            status,
            error,
            head_after,
            tokens_in: live.lock().tokens_in,
            commits,
            stash,
        };
        let _ = main_tx
            .send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                AppSignal::TurnFinished {
                    node: request.node.clone(),
                    outcome,
                },
            ))
            .await;
    });

    handle
}

async fn run(request: &TurnRequest, recorder: &mut Recorder) -> Result<(), String> {
    let (system, mut messages) = {
        let root = request.root.clone();
        let ancestor_dirs = request.ancestor_dirs.clone();
        let parent_head = request.parent_head.clone();
        let head = request.head.clone();
        let prompt = request.prompt.clone();
        let date = request.date.clone();
        let mode = request.mode;
        tokio::task::spawn_blocking(move || {
            let system = context::system_prompt(mode, &root, head.as_deref(), &date);
            let mut messages: Vec<Message> = ancestor_dirs
                .iter()
                .flat_map(|dir| context::ancestor_messages(dir))
                .collect();
            let note = moved_note(&root, parent_head.as_deref(), head.as_deref());
            messages.push(context::user_message(note.as_deref(), &prompt));
            (system, messages)
        })
        .await
        .map_err(|e| e.to_string())?
    };
    if let Some(sent) = messages.last() {
        recorder.message(sent);
    }
    agent_loop::run(
        request.mode,
        &request.target,
        &system,
        &mut messages,
        &request.root,
        recorder,
    )
    .await?;
    if request.mode == Mode::Feedback {
        return Ok(());
    }
    // A commit turn is not over while the working tree is dirty: stay in the loop and
    // ask for the commit. The nudge goes in the transcript but is never shown.
    for _ in 0..MAX_NUDGES {
        let root = request.root.clone();
        let status = tokio::task::spawn_blocking(move || crate::git::status_short(&root))
            .await
            .map_err(|e| e.to_string())??;
        if status.trim().is_empty() {
            break;
        }
        let nudge = context::commit_nudge(&status);
        recorder.message(&nudge);
        messages.push(nudge);
        agent_loop::run(
            request.mode,
            &request.target,
            &system,
            &mut messages,
            &request.root,
            recorder,
        )
        .await?;
    }
    Ok(())
}

/// What a commit turn leaves in git once it has ended, however it ended: the commits
/// it made, and a stash of anything still uncommitted.
fn close_commit_turn(
    root: &Utf8Path,
    node: &str,
    head_before: Option<&str>,
    head_after: Option<&str>,
) -> (Vec<String>, Option<String>) {
    let commits = match head_after {
        Some(after) if head_before != Some(after) => {
            crate::git::commits_between(root, head_before, after).unwrap_or_else(|e| {
                tracing::warn!("cannot list the commits of {node}: {e}");
                vec![after.to_string()]
            })
        }
        _ => Vec::new(),
    };
    let dirty = crate::git::status_short(root).is_ok_and(|status| !status.trim().is_empty());
    let stash = dirty
        .then(|| crate::git::stash_push(root, &format!("explorer: left uncommitted by {node}")))
        .and_then(|stashed| {
            stashed
                .map_err(|e| tracing::warn!("cannot stash the leftovers of {node}: {e}"))
                .ok()
        });
    (commits, stash)
}

/// The repository-moved note, when HEAD is no longer where the parent's turn left it.
fn moved_note(root: &Utf8Path, parent_head: Option<&str>, head: Option<&str>) -> Option<String> {
    let (parent_head, head) = (parent_head?, head?);
    if parent_head == head {
        return None;
    }
    let commits = crate::git::log_oneline(root, parent_head, head, context::NOTE_MAX_COMMITS + 1);
    Some(context::repository_moved_note(&commits))
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A prompt short enough to be its own title: one line of at most 100 characters.
fn title_from_prompt(prompt: &str) -> Option<String> {
    let trimmed = prompt.trim();
    (!trimmed.contains('\n') && trimmed.chars().count() <= TITLE_MAX_CHARS)
        .then(|| trimmed.to_string())
}

/// A prompt short enough to be its own summary: at most 50 words.
fn summary_from_prompt(prompt: &str) -> Option<String> {
    (prompt.split_whitespace().count() <= SUMMARY_MAX_WORDS).then(|| prompt.trim().to_string())
}

fn clean_title(answer: &str) -> String {
    single_line(answer).chars().take(TITLE_MAX_CHARS).collect()
}

async fn title_for(prompt: &str, target: Option<&LlmTarget>) -> Option<String> {
    if let Some(title) = title_from_prompt(prompt) {
        return Some(title);
    }
    match llm::complete(target?, context::TITLE_PROMPT, prompt).await {
        Ok(answer) => Some(clean_title(&answer)).filter(|title| !title.is_empty()),
        Err(e) => {
            tracing::warn!("title generation failed: {e}");
            None
        }
    }
}

async fn summary_for(prompt: &str, target: Option<&LlmTarget>) -> Option<String> {
    if let Some(summary) = summary_from_prompt(prompt) {
        return Some(summary);
    }
    match llm::complete(target?, context::SUMMARY_PROMPT, prompt).await {
        Ok(answer) => Some(answer.trim().to_string()).filter(|summary| !summary.is_empty()),
        Err(e) => {
            tracing::warn!("summary generation failed: {e}");
            None
        }
    }
}

/// Produces `title.md` and `summary.md` of a node from its prompt. Short prompts are
/// reused as they are; longer ones need `target`, and stay without when it is absent
/// or the call fails.
pub fn spawn_title_and_summary(
    node: NodeId,
    node_dir: Utf8PathBuf,
    prompt: String,
    target: Option<LlmTarget>,
    main_tx: MainTx,
) {
    tokio::spawn(async move {
        let (title, summary) = tokio::join!(
            title_for(&prompt, target.as_ref()),
            summary_for(&prompt, target.as_ref())
        );
        if let Some(title) = title {
            if let Err(e) = write_atomic(&node_dir.join("title.md"), &title) {
                tracing::warn!("failed to write title.md: {e}");
            }
            let signal = AppSignal::NodeTitleReady {
                node: node.clone(),
                title,
            };
            let _ = main_tx
                .send(TerminalWindowMainThreadSignal::ApplyAppSignal(signal))
                .await;
        }
        if let Some(summary) = summary {
            if let Err(e) = write_atomic(&node_dir.join("summary.md"), &summary) {
                tracing::warn!("failed to write summary.md: {e}");
            }
            let signal = AppSignal::NodeSummaryReady { node, summary };
            let _ = main_tx
                .send(TerminalWindowMainThreadSignal::ApplyAppSignal(signal))
                .await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn closing_a_clean_turn_lists_its_commits_oldest_first() {
        let (_tmp, root) = temp_repo();
        let before = commit(&root, "a.txt", "1", "init");
        let first = commit(&root, "a.txt", "2", "feat: one");
        let second = commit(&root, "b.txt", "x", "feat: two");

        let (commits, stash) = close_commit_turn(&root, "node", Some(&before), Some(&second));
        assert_eq!(commits, [first, second]);
        assert!(stash.is_none());
    }

    #[test]
    fn closing_a_turn_that_changed_nothing_records_nothing() {
        let (_tmp, root) = temp_repo();
        let head = commit(&root, "a.txt", "1", "init");
        let (commits, stash) = close_commit_turn(&root, "node", Some(&head), Some(&head));
        assert!(commits.is_empty());
        assert!(stash.is_none());
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
    fn closing_a_dirty_turn_stashes_tracked_and_untracked_leftovers() {
        let (_tmp, root) = temp_repo();
        let before = commit(&root, "a.txt", "1", "init");
        std::fs::write(root.join("a.txt"), "changed").unwrap();
        std::fs::write(root.join("new.txt"), "untracked").unwrap();

        let (commits, stash) = close_commit_turn(&root, "node-7", Some(&before), Some(&before));
        assert!(commits.is_empty());
        let stash = stash.expect("leftovers are stashed");
        assert_eq!(stash, git(&root, &["rev-parse", "refs/stash"]));
        assert_eq!(crate::git::status_short(&root).unwrap(), "");
        assert!(git(&root, &["stash", "list"]).contains("left uncommitted by node-7"));
    }

    #[test]
    fn closing_the_first_commit_of_a_repository_lists_it() {
        let (_tmp, root) = temp_repo();
        let first = commit(&root, "a.txt", "1", "init");
        let (commits, _) = close_commit_turn(&root, "node", None, Some(&first));
        assert_eq!(commits, [first]);
    }

    #[test]
    fn short_single_line_prompt_is_its_own_title() {
        assert_eq!(
            title_from_prompt("  fix the tests \n").as_deref(),
            Some("fix the tests")
        );
        assert_eq!(
            title_from_prompt(&"x".repeat(100)).map(|t| t.len()),
            Some(100)
        );
        assert!(title_from_prompt(&"x".repeat(101)).is_none());
        assert!(title_from_prompt("two\nlines").is_none());
    }

    #[test]
    fn prompt_of_fifty_words_or_fewer_is_its_own_summary() {
        let fifty = vec!["word"; 50].join(" ");
        assert_eq!(summary_from_prompt(&fifty).as_deref(), Some(fifty.as_str()));
        let more = vec!["word"; 51].join(" ");
        assert!(summary_from_prompt(&more).is_none());
    }

    #[test]
    fn model_title_is_flattened_and_cut_at_the_limit() {
        assert_eq!(clean_title(" Fix\n the  tests "), "Fix the tests");
        assert_eq!(clean_title(&"é".repeat(150)).chars().count(), 100);
    }
}
