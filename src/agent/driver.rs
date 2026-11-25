//! The turn's side of `explorer agent`: starts the process, applies every event it writes
//! to the turn's recorder (which stays the only writer of the node), and feeds it what the
//! user sends while it works. A run stops the process and its group before it returns.

use std::sync::OnceLock;
use std::time::Duration;

/// How long a process that was told to stop has before it is killed.
pub(crate) const STOP_GRACE: Duration = Duration::from_secs(5);

/// How often a stop checks whether its process has gone.
const POLL: Duration = Duration::from_millis(5);

/// How long a cancelled run's output is still read, for what it wrote before it stopped.
pub(crate) const DRAIN: Duration = Duration::from_secs(1);

/// Where the parent logs, passed on to the agent processes it starts.
static LOG: OnceLock<(String, tracing_core::LevelFilter)> = OnceLock::new();

/// Records where the parent logs, so the agent processes it starts log to the same file.
pub fn set_log(file: &str, level: tracing_core::LevelFilter) {
    // Absolute: the agent runs in its turn's copy, where a relative path is another file.
    let file = std::path::absolute(file)
        .ok()
        .and_then(|path| camino::Utf8PathBuf::from_path_buf(path).ok())
        .map_or_else(|| file.to_string(), camino::Utf8PathBuf::into_string);
    let _ = LOG.set((file, level));
}

/// Whether `pid` has exited, without reaping it: its owner still holds the child.
pub(crate) fn exited(pid: u32) -> bool {
    // SAFETY: `info` is written by the call and `WNOWAIT` leaves the child unreaped.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        let rc = libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        );
        // An error is a child that is already reaped.
        rc != 0 || info.si_pid() != 0
    }
}

/// The process of one run: `stop` ends it and its group, a guard dropped without it kills the group.
pub(crate) struct ChildGuard {
    child: tokio::process::Child,
    pid: u32,
    reaped: bool,
}

impl ChildGuard {
    pub(crate) fn new(child: tokio::process::Child) -> Result<Self, String> {
        let Some(pid) = child.id() else {
            return Err("the agent process exited before the turn took hold of it".to_string());
        };
        Ok(Self {
            child,
            pid,
            reaped: false,
        })
    }

    /// The handle itself, for the pipes of a run that reads it while it works.
    pub(crate) fn child_mut(&mut self) -> &mut tokio::process::Child {
        &mut self.child
    }

    /// Waits until the process has exited, without reaping it; whether it did in time.
    async fn wait_exit(&self, grace: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + grace;
        while !exited(self.pid) {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
        true
    }

    /// SIGTERM if the leader runs, then SIGKILL to its group before the leader is reaped.
    pub(crate) async fn stop(&mut self) -> std::io::Result<std::process::ExitStatus> {
        if !self.reaped {
            if !exited(self.pid) {
                crate::util::process::kill_group(self.pid, libc::SIGTERM);
                self.wait_exit(STOP_GRACE).await;
            }
            crate::util::process::kill_group(self.pid, crate::util::process::KILL);
        }
        let status = self.child.wait().await;
        self.reaped = true;
        status
    }

    /// Like `stop`, for a process that was told to finish: it has `STOP_GRACE` to exit alone.
    pub(crate) async fn finish(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.wait_exit(STOP_GRACE).await;
        self.stop().await
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.reaped {
            crate::util::process::kill_group(self.pid, crate::util::process::KILL);
            // Reaps what has already exited; tokio reaps the rest in the background.
            let _ = self.child.try_wait();
        }
    }
}

/// The command that starts `explorer agent`.
fn command(sandbox: Option<&crate::agent::workspace::Sandbox>) -> tokio::process::Command {
    use std::process::Stdio;
    // The program of this process, even if the file was replaced since it started.
    #[cfg(not(test))]
    let (program, args) = (std::path::PathBuf::from("/proc/self/exe"), ["agent"]);
    // A test binary is not `explorer`: it runs the one test that stands in for it.
    #[cfg(test)]
    let (program, args) = (
        std::env::current_exe().expect("the test binary has a path"),
        ["--exact", "agent::child::tests::as_agent", "--nocapture"],
    );
    let mut command = match sandbox {
        Some(sandbox) => sandbox.command(&program),
        None => tokio::process::Command::new(&program),
    };
    command.args(args);
    #[cfg(test)]
    command.env(crate::agent::child::AS_AGENT, "1");
    if let Some((file, level)) = LOG.get() {
        command.args([
            "--log-file",
            file,
            "--log-level",
            &level.to_string().to_lowercase(),
        ]);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    command
}

/// The error of a run that stopped because its turn was cancelled.
pub(crate) const CANCELLED: &str = crate::conversation::CANCELLED_ERROR;

/// What the turn asks of a run besides what it records.
pub struct Closing<'a> {
    pub sandbox: Option<&'a crate::agent::workspace::Sandbox>,
    /// A commit turn: the repository to ask for a commit while its tree is dirty.
    pub commit_root: Option<&'a camino::Utf8Path>,
    /// Set by the turn to cancel the run.
    pub cancel: tokio::sync::watch::Receiver<bool>,
}

/// Runs `start` in a child process, recording into `recorder`; the process is gone on return.
pub async fn run(
    start: &crate::agent::wire::Start,
    closing: Closing<'_>,
    recorder: &mut crate::agent::turn::live::Recorder,
) -> Result<(), String> {
    // A turn cancelled before its run started starts none.
    if *closing.cancel.borrow() {
        return Err(CANCELLED.to_string());
    }
    let mut child = ChildGuard::new(
        command(closing.sandbox)
            .spawn()
            .map_err(|e| format!("failed to start the agent process: {e}"))?,
    )?;
    let result = drive(&mut child, start, closing, recorder).await;
    let _ = child.stop().await;
    result
}

/// Feeds the process `start`, then applies what it reports until it finishes.
async fn drive(
    child: &mut ChildGuard,
    start: &crate::agent::wire::Start,
    closing: Closing<'_>,
    recorder: &mut crate::agent::turn::live::Recorder,
) -> Result<(), String> {
    use crate::agent::wire::{Command, Event};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    let injector = recorder.injector();
    // The recorder may come from a run that closed it when it finished.
    injector.open();
    let mut stdin = child
        .child
        .stdin
        .take()
        .expect("command() pipes the child's stdin");
    let stdout = child
        .child
        .stdout
        .take()
        .expect("command() pipes the child's stdout");
    let stderr = child.child.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            text
        })
    });
    // A child that refuses its start says why on stderr, which is the reason reported.
    let start_failed = stdin
        .write_all(crate::agent::wire::line(start).as_bytes())
        .await
        .err();

    let mut lines = tokio::io::BufReader::new(stdout).split(b'\n');
    let mut cancel = closing.cancel;
    // Set once cancelled: the process is stopped, and its output is read until then.
    let mut drain_until: Option<tokio::time::Instant> = None;
    let mut nudges = 0;
    // Whether an answer is streaming: its text is recorded once the child says so.
    let mut streaming = false;
    let mut finished: Option<Option<String>> = None;
    // A JSON line that is no event: the child may wait for a reply to it, so the run fails.
    let mut garbled: Option<String> = None;
    while start_failed.is_none() && finished.is_none() {
        let next = match drain_until {
            Some(deadline) => match tokio::time::timeout_at(deadline, lines.next_segment()).await {
                Ok(next) => next,
                Err(_) => break,
            },
            None => tokio::select! {
                biased;
                _ = cancel.wait_for(|cancelled| *cancelled) => {
                    crate::util::process::kill_group(child.pid, libc::SIGTERM);
                    drain_until = Some(tokio::time::Instant::now() + DRAIN);
                    continue;
                }
                next = lines.next_segment() => next,
            },
        };
        let Ok(Some(line)) = next else {
            break;
        };
        let event = match serde_json::from_slice::<Event>(&line) {
            Ok(event) => event,
            Err(error) if line.first() == Some(&b'{') => {
                garbled = Some(format!("the agent process wrote a bad event: {error}"));
                break;
            }
            // Other output, like a test harness's, is no business of this turn.
            Err(_) => continue,
        };
        let reply = match event {
            Event::Message { message } => {
                streaming = false;
                recorder.message(&message);
                None
            }
            Event::BeginAnswer => {
                streaming = true;
                recorder.begin_answer();
                None
            }
            Event::Chunk { thinking, text } => {
                recorder.chunk(thinking, &text);
                None
            }
            Event::FlushPartialAnswer => {
                streaming = false;
                recorder.flush_partial_answer();
                None
            }
            Event::AnswerCallsTools => {
                streaming = false;
                recorder.answer_calls_tools();
                None
            }
            Event::RequestSize {
                tokens_in,
                cached_in,
            } => {
                recorder.request_size(tokens_in, cached_in);
                None
            }
            Event::ToolCall { description } => {
                recorder.tool_call(description);
                None
            }
            Event::Activity { activity } => {
                recorder.activity(activity);
                None
            }
            // A cancelled run starts no round and asks for no commit.
            Event::Poll | Event::Settle if drain_until.is_some() => None,
            // The child reports what it read, so the queue gives it up only once read.
            Event::Poll => Some(Command::Messages {
                contents: injector.queued(),
            }),
            Event::Injected { content } => {
                injector.wrote(&content);
                None
            }
            Event::Settle => {
                // Looked for before the turn closes; a message sent meanwhile drops it.
                let nudge = match closing.commit_root {
                    Some(root) if injector.queued().is_empty() => {
                        crate::agent::context::next_commit_nudge(root, nudges).await?
                    }
                    _ => None,
                };
                // A cancel that landed during the wait above still asks for no commit.
                if *cancel.borrow() {
                    return Err(CANCELLED.to_string());
                }
                // Looked at and closed under one lock, so no message slips in between.
                match injector.settle(nudge.is_none()) {
                    Some(contents) => Some(Command::Messages { contents }),
                    None => Some(match nudge {
                        Some(message) => {
                            nudges += 1;
                            recorder.message(&message);
                            Command::Note { message }
                        }
                        None => Command::Finish,
                    }),
                }
            }
            Event::Finished { error } => {
                finished = Some(error);
                None
            }
        };
        if let Some(reply) = reply {
            // A child that is gone shows as the end of its output.
            if stdin
                .write_all(crate::agent::wire::line(&reply).as_bytes())
                .await
                .is_err()
            {
                break;
            }
            let _ = stdin.flush().await;
        }
    }
    // Only `streaming` holds text the transcript lacks: a recorded answer is not repeated.
    if streaming {
        recorder.flush_partial_answer();
    }
    // A line cut short by the stop is no bad event, so the cancel is checked first.
    if drain_until.is_some() {
        return Err(CANCELLED.to_string());
    }
    if let Some(error) = garbled {
        // The child may have said why on stderr (a panic, say): that reason comes first.
        drop(stdin);
        let _ = child.stop().await;
        let stderr = match stderr {
            Some(reader) => tokio::time::timeout(DRAIN, reader)
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default(),
            None => String::new(),
        };
        return Err(stderr_reason(&stderr).map_or(error, str::to_string));
    }
    // Closes the child's input: it has nothing more to wait for.
    drop(stdin);
    let status = child.finish().await;
    let stderr = match stderr {
        Some(reader) => reader.await.unwrap_or_default(),
        None => String::new(),
    };
    match finished {
        Some(None) => Ok(()),
        Some(Some(error)) => Err(error),
        None => Err(match (stderr_reason(&stderr), start_failed, status) {
            (Some(reason), _, _) => reason.to_string(),
            (None, Some(error), _) => {
                format!("the agent process did not take its start: {error}")
            }
            (None, None, Ok(status)) => {
                format!("the agent process ended without a result ({status})")
            }
            (None, None, Err(e)) => {
                format!("the agent process could not be waited for: {e}")
            }
        }),
    }
}

/// The first line of a child's stderr that says why it failed.
fn stderr_reason(stderr: &str) -> Option<&str> {
    // A panic's header names where it happened; its message is the line after.
    stderr
        .lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with("thread '"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn_sh(script: &str) -> (ChildGuard, u32) {
        let child = tokio::process::Command::new("sh")
            .args(["-c", script])
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        (ChildGuard::new(child).unwrap(), pid)
    }

    /// What `pgrep` finds in the process group `pid`; empty once the group is gone.
    fn group_members(pid: u32) -> String {
        let out = std::process::Command::new("pgrep")
            .args(["-g", &pid.to_string()])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[tokio::test]
    async fn stopping_the_guard_waits_for_the_process_to_stop_alone() {
        // Exits a second after SIGTERM: the stop must wait for that, not return at once.
        let (mut guard, pid) = spawn_sh("trap 'sleep 1; exit 0' TERM; sleep 300 & wait");
        // Lets the shell install its trap.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let started = std::time::Instant::now();
        guard.stop().await.unwrap();
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert!(exited(pid), "the process {pid} still runs after the stop");
    }

    #[tokio::test]
    async fn stopping_the_guard_kills_what_a_finished_leader_left_in_its_group() {
        // The leader exits at once; its background `sleep` stays in the group.
        let (mut guard, pid) = spawn_sh("sleep 300 & echo $! ; exit 0");
        guard.wait_exit(STOP_GRACE).await;
        guard.stop().await.unwrap();
        let leftover = group_members(pid);
        assert!(leftover.is_empty(), "group {pid} still has {leftover}");
    }

    #[tokio::test]
    async fn dropping_the_guard_kills_the_group_without_waiting() {
        // Ignores SIGTERM: only a kill ends it, and the drop must not wait on a grace period.
        let (guard, pid) = spawn_sh("trap '' TERM; sleep 300 & wait");
        tokio::time::sleep(Duration::from_millis(300)).await;
        let started = std::time::Instant::now();
        drop(guard);
        assert!(started.elapsed() < Duration::from_secs(1));
        // The leader is reaped in the background, and its `sleep` dies with the group.
        for _ in 0..200 {
            if exited(pid) && group_members(pid).is_empty() {
                return;
            }
            tokio::time::sleep(POLL).await;
        }
        panic!("group {pid} still has {}", group_members(pid));
    }

    fn start(system: &str) -> crate::agent::wire::Start {
        use crate::agent::llm::LlmTarget;
        use crate::agent::wire::Start;
        use crate::model_spec::Effort;
        Start {
            target: LlmTarget {
                url: "http://127.0.0.1:1".to_string(),
                token: String::new(),
                model: "model".to_string(),
                effort: Effort::Default,
            },
            session: "root".to_string(),
            system: system.to_string(),
            messages: Vec::new(),
            root: camino::Utf8PathBuf::from("/"),
            node: "n".to_string(),
            node_dir: camino::Utf8PathBuf::from("/"),
            mcp: Vec::new(),
            socket: None,
            agents: None,
            repeat_limit: 0,
            compact: false,
        }
    }

    fn recorder() -> crate::agent::turn::live::Recorder {
        use crate::agent::turn::live::{LiveTurn, Recorder};
        let live = std::sync::Arc::new(parking_lot::Mutex::new(LiveTurn::new()));
        Recorder::new(live, camino::Utf8PathBuf::from("/"))
    }

    #[tokio::test]
    async fn a_run_cancelled_before_it_starts_starts_nothing() {
        let mut recorder = recorder();
        recorder.live.lock().closed = true;
        let (_tx, cancel) = tokio::sync::watch::channel(true);
        let closing = Closing {
            sandbox: None,
            commit_root: None,
            cancel,
        };
        let result = run(&start(""), closing, &mut recorder).await;
        assert_eq!(result.err().as_deref(), Some(CANCELLED));
        // Starting a run reopens the turn, so a closed one never started.
        assert!(recorder.live.lock().closed);
    }

    #[tokio::test]
    async fn a_garbled_event_fails_the_run_with_the_reason_the_child_gave() {
        for (stderr, expected) in [
            ("", "the agent process wrote a bad event"),
            ("it panicked: boom", "it panicked: boom"),
        ] {
            let mut recorder = recorder();
            let (_tx, cancel) = tokio::sync::watch::channel(false);
            let closing = Closing {
                sandbox: None,
                commit_root: None,
                cancel,
            };
            let system = format!("{}{stderr}", crate::agent::child::GARBLE);
            let result = run(&start(&system), closing, &mut recorder).await;
            let error = result.expect_err("a garbled line fails the run");
            assert!(error.starts_with(expected), "{error}");
        }
    }
}
