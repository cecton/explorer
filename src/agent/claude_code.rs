use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Settings every run gets: no trailer on the commits it makes, and no hooks, which are
/// code a repository supplies and which Claude Code runs before the model is called.
const SETTINGS: &str = r#"{"attribution":{"commit":"","pr":""},"disableAllHooks":true}"#;

/// How often the steering task looks for a message the user sent while the CLI works.
const STEERING_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

/// A model of the local `claude` CLI and the effort to run it with.
#[derive(Clone, Debug)]
pub struct ClaudeTarget {
    pub model: String,
    pub effort: crate::model_spec::Effort,
}

/// The session a run starts from.
#[derive(Clone, Copy)]
pub enum Resume<'a> {
    /// A new session with no history.
    Fresh,
    /// A copy of the parent node's session, which is left as it was.
    Fork(&'a str),
    /// The node's own session, appended to in place.
    Continue(&'a str),
}

/// What every run of one turn shares: only its prompt and session differ.
#[derive(Clone, Copy)]
pub struct Run<'a> {
    pub target: &'a ClaudeTarget,
    pub root: &'a camino::Utf8Path,
    pub system: &'a str,
    pub mcp: &'a crate::agent::mcp::McpTools,
    /// The turn's tool socket, which Explorer's tool server asks for what only the turn knows.
    pub privileged_socket: Option<&'a camino::Utf8Path>,
    pub sandbox: Option<&'a crate::agent::workspace::Sandbox>,
}

impl Run<'_> {
    /// Runs `prompt` from `start`. A `start` whose session is gone is not an error: the
    /// run opens a fresh session instead. A fresh session opens with `ancestors` as plain
    /// text, since it has none of their history; they are read only when such a run is
    /// about to happen, so a fork that works costs no file read.
    pub async fn from(
        self,
        prompt: &str,
        start: Resume<'_>,
        ancestors: &[camino::Utf8PathBuf],
        recorder: &mut crate::agent::turn::live::Recorder,
    ) -> Result<(), String> {
        let mut opening = prompt.to_string();
        let mut resume = start;
        let mut anchored = false;
        loop {
            if !anchored && matches!(resume, Resume::Fresh) {
                opening = open_fresh(ancestors, &opening).await?;
                anchored = true;
            }
            let attempt = run(&self, &opening, resume, recorder).await;
            match attempt {
                Ok(()) => return Ok(()),
                Err(RunError::Failed(error)) => return Err(error),
                // A fresh run never reports a missing session: this retries at most once.
                Err(RunError::SessionGone) => resume = Resume::Fresh,
            }
        }
    }

    /// Runs `prompt` as a new message of `session`. A session that is gone is an error
    /// here: the message only makes sense to a model that has read the turn.
    pub async fn resume(
        self,
        prompt: &str,
        session: &str,
        recorder: &mut crate::agent::turn::live::Recorder,
    ) -> Result<(), String> {
        run(&self, prompt, Resume::Continue(session), recorder)
            .await
            .map_err(|error| match error {
                RunError::SessionGone => "the Claude Code session of this turn is gone".to_string(),
                RunError::Failed(error) => error,
            })
    }
}

/// `prompt`, preceded by the ancestors as plain text when any of them has something to
/// say.
async fn open_fresh(ancestors: &[camino::Utf8PathBuf], prompt: &str) -> Result<String, String> {
    let ancestors = ancestors.to_vec();
    let preamble =
        tokio::task::spawn_blocking(move || crate::agent::context::handover_preamble(&ancestors))
            .await
            .map_err(|e| e.to_string())?;
    Ok(match preamble {
        Some(preamble) => format!("{preamble}\n\n{prompt}"),
        None => prompt.to_string(),
    })
}

enum RunError {
    /// The session to resume no longer exists; nothing was run.
    SessionGone,
    Failed(String),
}

/// What one line of the CLI's `stream-json` output means for a turn.
#[derive(Debug, PartialEq)]
enum StreamEvent {
    Session(String),
    /// The model starts a new answer; its text follows.
    AnswerStart,
    Text(String),
    /// The current answer starts a call to the named tool.
    ToolStart(String),
    /// A complete tool call, described for the work log.
    ToolCall(String),
    /// The results of the answer's tool calls are back.
    ToolsDone,
    /// Size of the context of the request that just completed.
    TokensIn(u64),
    /// Part of that context the provider served from its prompt cache.
    CachedIn(u64),
    /// How much of the subscription is used after the request that just completed.
    Usage(crate::usage::Usage),
    /// A task the CLI runs on the side (a sub-agent, a background command) started.
    TaskStarted(String),
    /// That task is over; the CLI tells the model, which may answer again.
    TaskDone(String),
    /// The model's answer is over. `no_turn` is set when it ended before the model was called.
    Result {
        error: Option<String>,
        no_turn: bool,
    },
}

fn tool_activity(name: &str, input: &serde_json::Value) -> String {
    let detail = ["command", "file_path", "pattern", "description"]
        .iter()
        .find_map(|key| input.get(key).and_then(serde_json::Value::as_str))
        .map(str::to_string);
    crate::agent::tools::describe_with(name, detail, input)
}

/// The sizes of one request's context: its total, and how much of it the provider
/// served from its prompt cache. The cache write is part of the context but not a hit.
fn context_tokens(usage: &serde_json::Value) -> Option<(u64, u64)> {
    let field = |key: &str| usage.get(key).and_then(serde_json::Value::as_u64);
    let cached = field("cache_read_input_tokens").unwrap_or(0);
    Some((
        field("input_tokens")? + field("cache_creation_input_tokens").unwrap_or(0) + cached,
        cached,
    ))
}

fn context_sizes(usage: &serde_json::Value) -> Vec<StreamEvent> {
    let Some((total, cached)) = context_tokens(usage) else {
        return Vec::new();
    };
    vec![StreamEvent::TokensIn(total), StreamEvent::CachedIn(cached)]
}

/// The subscription's limits in a `rate_limit_event`; a login without a subscription
/// reports none.
fn subscription_usage(info: &serde_json::Value) -> Option<crate::usage::Usage> {
    use crate::usage::Span;
    use crate::usage::Usage;
    use crate::usage::UsageWindow;
    let reported = info.get("unifiedWindows")?;
    let window = |key: &str, span: Span| {
        let window = reported.get(key)?;
        Some(UsageWindow {
            span,
            percent: (window.get("utilization")?.as_f64()? * 100.0).round() as u32,
            resets_at: Some(window.get("resetsAt")?.as_i64()?),
        })
    };
    let windows: Vec<UsageWindow> = [
        window("five_hour", Span::FiveHours),
        window("seven_day", Span::Week),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!windows.is_empty()).then_some(Usage { windows, key: None })
}

/// Translates one output line. Lines of subagents and of kinds we do not use yield
/// nothing.
fn parse_line(line: &str) -> Vec<StreamEvent> {
    let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
        return Vec::new();
    };
    if message
        .get("parent_tool_use_id")
        .is_some_and(|parent| !parent.is_null())
    {
        return Vec::new();
    }
    let text = |value: &serde_json::Value, key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let kind = message
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match kind {
        "system" => {
            let event: fn(String) -> StreamEvent =
                match message.get("subtype").and_then(serde_json::Value::as_str) {
                    Some("init") => {
                        return text(&message, "session_id")
                            .map(StreamEvent::Session)
                            .into_iter()
                            .collect();
                    }
                    Some("task_started") => StreamEvent::TaskStarted,
                    Some("task_notification") => StreamEvent::TaskDone,
                    _ => return Vec::new(),
                };
            text(&message, "task_id").map(event).into_iter().collect()
        }
        "stream_event" => {
            let event = &message["event"];
            match event
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
            {
                "message_start" => {
                    let mut events = vec![StreamEvent::AnswerStart];
                    events.extend(context_sizes(&event["message"]["usage"]));
                    events
                }
                "content_block_start" if event["content_block"]["type"] == "tool_use" => {
                    let name = text(&event["content_block"], "name");
                    vec![StreamEvent::ToolStart(name.unwrap_or_default())]
                }
                "content_block_delta" if event["delta"]["type"] == "text_delta" => {
                    text(&event["delta"], "text")
                        .map(StreamEvent::Text)
                        .into_iter()
                        .collect()
                }
                "message_delta" => context_sizes(&event["usage"]),
                _ => Vec::new(),
            }
        }
        "assistant" => message["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|block| block["type"] == "tool_use")
            .map(|block| {
                let name = block["name"].as_str().unwrap_or("tool");
                StreamEvent::ToolCall(tool_activity(name, &block["input"]))
            })
            .collect(),
        "user" => vec![StreamEvent::ToolsDone],
        "rate_limit_event" => subscription_usage(&message["rate_limit_info"])
            .map(StreamEvent::Usage)
            .into_iter()
            .collect(),
        "result" => {
            let failed = message["is_error"].as_bool().unwrap_or(false);
            let error = failed.then(|| {
                text(&message, "result")
                    .filter(|result| !result.is_empty())
                    .or_else(|| text(&message, "subtype"))
                    .unwrap_or_else(|| "claude reported an error".to_string())
            });
            let mut events: Vec<StreamEvent> = text(&message, "session_id")
                .map(StreamEvent::Session)
                .into_iter()
                .collect();
            events.push(StreamEvent::Result {
                error,
                no_turn: message["num_turns"].as_u64() == Some(0),
            });
            events
        }
        _ => Vec::new(),
    }
}

fn arguments(config: &Run<'_>, resume: Resume<'_>) -> Vec<String> {
    use crate::model_spec::Effort;
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        // The prompt and everything the user sends while the run works arrive on stdin
        // as user messages, which is how the CLI takes steering mid-run.
        "--input-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-mode",
        "bypassPermissions",
        "--settings",
        SETTINGS,
        // The repository's own settings file, and the hooks it declares, are code it
        // supplies: read the user's, never the project's.
        "--setting-sources",
        "user",
        // A resumed session otherwise keeps the prompt of its first request, and with
        // it the mode of the node it was forked from.
        "--system-prompt-snapshot",
        "off",
        "--append-system-prompt",
        config.system,
        "--model",
        &config.target.model,
    ]
    .iter()
    .map(|arg| arg.to_string())
    .collect();
    if config.target.effort != Effort::Default {
        args.push("--effort".to_string());
        args.push(config.target.effort.as_str().to_string());
    }
    match resume {
        Resume::Fresh => {}
        Resume::Fork(session) => {
            args.extend(["--resume", session, "--fork-session"].map(str::to_string));
        }
        Resume::Continue(session) => args.extend(["--resume", session].map(str::to_string)),
    }
    let config = config
        .mcp
        .claude_config(config.root, config.privileged_socket);
    args.push("--mcp-config".to_string());
    args.push(config);
    // Only the servers of this repository, never those of the user's own setup.
    args.push("--strict-mcp-config".to_string());
    args
}

fn spawn_error(error: std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "the `claude` command was not found on PATH".to_string(),
        _ => format!("failed to start claude: {error}"),
    }
}

/// Side tasks that have not reported back yet: a result settles the run only when none is left.
#[derive(Default)]
struct Settling {
    outstanding: std::collections::HashSet<String>,
}

impl Settling {
    /// What `event` makes of `settled`, when it changes it.
    fn after(&mut self, event: &StreamEvent) -> Option<bool> {
        match event {
            StreamEvent::TaskStarted(id) => {
                self.outstanding.insert(id.clone());
                Some(false)
            }
            StreamEvent::TaskDone(id) => {
                self.outstanding.remove(id);
                Some(false)
            }
            StreamEvent::Result { .. } => Some(self.outstanding.is_empty()),
            _ => None,
        }
    }
}

/// One user message, in the `stream-json` input format of the CLI.
fn input_frame(content: &str) -> String {
    let frame = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": content },
    });
    format!("{frame}\n")
}

/// Runs the CLI once and feeds its output to `recorder`. The session id is recorded as
/// soon as it is known, so it survives a failure or a cancellation.
async fn run(
    config: &Run<'_>,
    prompt: &str,
    resume: Resume<'_>,
    recorder: &mut crate::agent::turn::live::Recorder,
) -> Result<(), RunError> {
    use crate::agent::turn::live::Injector;
    use tokio::io::AsyncBufReadExt;
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    let injector: Injector = recorder.injector();
    // The recorder may come from a run that closed it: a commit nudge, a lost session.
    injector.open();
    let mut command = match config.sandbox {
        Some(sandbox) => sandbox.command("claude"),
        None => {
            let mut command = tokio::process::Command::new("claude");
            command.current_dir(config.root);
            command
        }
    };
    let mut child = command
        .args(arguments(config, resume))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| RunError::Failed(spawn_error(e)))?;
    // SIGTERM rather than SIGKILL: it lets Claude Code stop the commands it started.
    let mut terminator = child
        .id()
        .map(|pid| crate::util::process::GroupGuard::new(pid, libc::SIGTERM));

    // stdin stays open for steering; the run ends once `Settling` lets stdin close.
    let first = input_frame(prompt);
    let settled = Arc::new(AtomicBool::new(false));
    let steering = child.stdin.take().map(|mut stdin| {
        let injector = injector.clone();
        let settled = Arc::clone(&settled);
        tokio::spawn(async move {
            settled.store(false, Ordering::SeqCst);
            if stdin.write_all(first.as_bytes()).await.is_err() {
                injector.close();
                return;
            }
            let _ = stdin.flush().await;
            loop {
                tokio::time::sleep(STEERING_INTERVAL).await;
                // One at a time, taken only once written: a write that fails, or a task
                // dropped mid-write, leaves the message for the window to give back.
                while let Some(content) = injector.front() {
                    settled.store(false, Ordering::SeqCst);
                    if stdin
                        .write_all(input_frame(&content).as_bytes())
                        .await
                        .is_err()
                    {
                        injector.close();
                        return;
                    }
                    let _ = stdin.flush().await;
                    injector.wrote(&content);
                }
                if injector.close_if_idle(settled.load(Ordering::SeqCst)) {
                    // The run has settled: closing stdin ends it.
                    return;
                }
            }
        })
    });
    let stderr = child.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            text
        })
    });

    let mut outcome = None;
    let mut settling = Settling::default();
    if let Some(stdout) = child.stdout.take() {
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            for event in parse_line(&line) {
                if let Some(now) = settling.after(&event) {
                    settled.store(now, Ordering::SeqCst);
                }
                match event {
                    StreamEvent::Session(session) => recorder.session(session),
                    StreamEvent::AnswerStart => recorder.begin_answer(),
                    StreamEvent::Text(text) => recorder.text(&text),
                    StreamEvent::ToolStart(name) => {
                        recorder.answer_calls_tools();
                        recorder.activity(Some(name));
                    }
                    StreamEvent::ToolCall(call) => recorder.tool_call(call),
                    StreamEvent::ToolsDone => recorder.activity(None),
                    StreamEvent::TokensIn(tokens) => recorder.tokens_in(tokens),
                    StreamEvent::CachedIn(tokens) => recorder.cached_in(Some(tokens)),
                    StreamEvent::Usage(usage) => recorder.usage(usage),
                    StreamEvent::TaskStarted(_) | StreamEvent::TaskDone(_) => {}
                    StreamEvent::Result { error, no_turn } => outcome = Some((error, no_turn)),
                }
            }
        }
    }
    if let Some(steering) = steering {
        // Output ended without a settled result: the steering task would poll forever.
        steering.abort();
    }
    let status = child.wait().await;
    if let Some(terminator) = terminator.as_mut() {
        terminator.disarm();
    }
    let stderr = match stderr {
        Some(reader) => reader.await.unwrap_or_default(),
        None => String::new(),
    };

    let resumed = !matches!(resume, Resume::Fresh);
    match outcome {
        Some((None, _)) => Ok(()),
        Some((Some(_), true)) if resumed => Err(RunError::SessionGone),
        Some((Some(error), _)) => Err(RunError::Failed(error)),
        None => {
            let reason = stderr.lines().find(|line| !line.trim().is_empty());
            Err(RunError::Failed(match (reason, status) {
                (Some(reason), _) => reason.to_string(),
                (None, Ok(status)) => format!("claude exited without a result ({status})"),
                (None, Err(e)) => format!("claude could not be waited for: {e}"),
            }))
        }
    }
}

/// One answer without tools and without a saved session, for titles and summaries.
pub async fn complete(target: &ClaudeTarget, system: &str, user: &str) -> Result<String, String> {
    use crate::model_spec::Effort;
    let mut command = tokio::process::Command::new("claude");
    command
        .args(["-p", "--output-format", "text", "--no-session-persistence"])
        .args([
            "--tools",
            "",
            "--strict-mcp-config",
            "--setting-sources",
            "user",
            "--settings",
            SETTINGS,
            "--system-prompt",
            system,
        ])
        .args(["--model", &target.model]);
    if target.effort != Effort::Default {
        command.args(["--effort", target.effort.as_str()]);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(spawn_error)?;
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(user.as_bytes()).await;
    }
    let output = child.wait_with_output().await.map_err(|e| e.to_string())?;
    let answer = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || answer.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr.lines().find(|line| !line.trim().is_empty());
        return Err(reason.unwrap_or("claude gave no answer").to_string());
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("fixtures/claude_stream.jsonl");

    fn fixture_events() -> Vec<StreamEvent> {
        FIXTURE.lines().flat_map(parse_line).collect()
    }

    #[test]
    fn recorded_stream_yields_session_text_activity_and_result() {
        let events = fixture_events();
        let session = "11111111-2222-4333-8444-555555555555".to_string();
        assert_eq!(events.first(), Some(&StreamEvent::Session(session.clone())));

        let text: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "pong");

        assert!(events.contains(&StreamEvent::ToolStart("Bash".to_string())));
        assert!(events.contains(&StreamEvent::ToolCall("Bash echo pong".to_string())));
        assert!(events.contains(&StreamEvent::ToolsDone));
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == StreamEvent::AnswerStart)
                .count(),
            2
        );
        assert!(events.contains(&StreamEvent::TokensIn(8 + 170 + 22_382)));
        assert!(events.contains(&StreamEvent::CachedIn(22_382)));
        assert_eq!(
            &events[events.len() - 2..],
            [
                StreamEvent::Session(session),
                StreamEvent::Result {
                    error: None,
                    no_turn: false
                }
            ]
        );
    }

    #[test]
    fn message_start_reports_the_context_before_the_answer_streams() {
        let start = r#"{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"message_start","message":{"usage":{"input_tokens":2,"cache_creation_input_tokens":10352,"cache_read_input_tokens":8368,"output_tokens":16}}},"session_id":"s"}"#;
        assert_eq!(
            parse_line(start),
            [
                StreamEvent::AnswerStart,
                StreamEvent::TokensIn(2 + 10352 + 8368),
                StreamEvent::CachedIn(8368)
            ]
        );
        let no_usage = r#"{"type":"stream_event","parent_tool_use_id":null,"event":{"type":"message_start"},"session_id":"s"}"#;
        assert_eq!(parse_line(no_usage), [StreamEvent::AnswerStart]);
    }

    #[test]
    fn failed_result_carries_its_reason_and_whether_a_turn_ran() {
        let gone = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"num_turns":0,"session_id":"s"}"#;
        assert_eq!(
            parse_line(gone).last(),
            Some(&StreamEvent::Result {
                error: Some("error_during_execution".to_string()),
                no_turn: true
            })
        );
        let login = r#"{"type":"result","subtype":"success","is_error":true,"num_turns":1,"result":"Please run /login"}"#;
        assert_eq!(
            parse_line(login),
            [StreamEvent::Result {
                error: Some("Please run /login".to_string()),
                no_turn: false
            }]
        );
    }

    #[test]
    fn subagent_and_unknown_lines_are_ignored() {
        let subagent = r#"{"type":"assistant","parent_tool_use_id":"toolu_1","message":{"content":[{"type":"tool_use","name":"Read","input":{}}]}}"#;
        assert!(parse_line(subagent).is_empty());
        assert!(parse_line(r#"{"type":"rate_limit_event"}"#).is_empty());
        assert!(parse_line("not json").is_empty());
    }

    #[test]
    fn rate_limit_event_gives_the_usage_of_the_subscription() {
        use crate::usage::Span;
        use crate::usage::Usage;
        use crate::usage::UsageWindow;
        let line = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1791817200,"rateLimitType":"five_hour","overageStatus":"rejected","overageDisabledReason":"out_of_credits","isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.22,"resetsAt":1791817200},"seven_day":{"utilization":0.18,"resetsAt":1792018800}}},"uuid":"8c092c44-667f-4329-af9b-b6a6803cb3ba","session_id":"s"}"#;
        assert_eq!(
            parse_line(line),
            [StreamEvent::Usage(Usage {
                windows: vec![
                    UsageWindow {
                        span: Span::FiveHours,
                        percent: 22,
                        resets_at: Some(1_791_817_200)
                    },
                    UsageWindow {
                        span: Span::Week,
                        percent: 18,
                        resets_at: Some(1_792_018_800)
                    },
                ],
                key: None,
            })]
        );
    }

    #[test]
    fn arguments_follow_effort_and_session() {
        use crate::agent::mcp::McpTools;
        use crate::model_spec::Effort;
        let target = ClaudeTarget {
            model: "opus".to_string(),
            effort: Effort::High,
        };
        let mcp = McpTools::default();
        let config = Run {
            target: &target,
            root: camino::Utf8Path::new("/repo"),
            system: "SYSTEM",
            mcp: &mcp,
            privileged_socket: None,
            sandbox: None,
        };
        let joined = |config: &Run<'_>, resume| arguments(config, resume).join(" ");

        let forked = joined(&config, Resume::Fork("abc"));
        assert!(forked.contains("--model opus --effort high"));
        assert!(!forked.contains("--disallowedTools"));
        assert!(forked.contains("--resume abc --fork-session --mcp-config"));
        assert!(forked.ends_with("--strict-mcp-config"));
        assert!(forked.contains("--append-system-prompt SYSTEM"));
        assert!(forked.contains("--system-prompt-snapshot off"));
        assert!(!forked.contains("--bare"));
        // A repository's settings file, and the hooks it declares, are code it supplies.
        assert!(forked.contains("--setting-sources user"));
        assert!(forked.contains(r#"{"attribution":{"commit":"","pr":""},"disableAllHooks":true}"#));

        let continued = joined(&config, Resume::Continue("abc"));
        assert!(continued.contains("--resume abc --mcp-config"));

        let default_effort = ClaudeTarget {
            model: "haiku".to_string(),
            effort: Effort::Default,
        };
        let fresh = Run {
            target: &default_effort,
            ..config
        };
        let fresh = joined(&fresh, Resume::Fresh);
        assert!(!fresh.contains("--effort"));
        assert!(!fresh.contains("--resume"));
        assert!(fresh.contains("--setting-sources user"));
        assert!(fresh.contains("--input-format stream-json"));
        // Every run carries Explorer's own tools, even one whose repository has no MCP
        // server of its own: a claude-code turn reads the conversation tree through them.
        assert!(fresh.contains("--mcp-config"));
        assert!(fresh.contains("explorer"));
    }

    #[test]
    fn a_tool_is_shown_with_its_first_detail_line() {
        let shown = |name: &str, input: serde_json::Value| tool_activity(name, &input);
        assert_eq!(
            shown("Bash", serde_json::json!({"command": "cargo check\necho"})),
            "Bash cargo check"
        );
        assert_eq!(
            shown("Read", serde_json::json!({"file_path": "src/main.rs"})),
            "Read src/main.rs"
        );
        assert_eq!(shown("Idle", serde_json::json!({})), "Idle");
        assert_eq!(
            shown("Odd", serde_json::json!({"id": 4})),
            r#"Odd {"id":4}"#
        );
    }

    #[test]
    fn task_lines_give_the_task_id() {
        let started = r#"{"type":"system","subtype":"task_started","task_id":"b8zpuyvzl","tool_use_id":"toolu_1","description":"Sleep","is_backgrounded":true,"task_type":"local_bash","session_id":"s"}"#;
        assert_eq!(
            parse_line(started),
            [StreamEvent::TaskStarted("b8zpuyvzl".to_string())]
        );
        let done = r#"{"type":"system","subtype":"task_notification","task_id":"b8zpuyvzl","tool_use_id":"toolu_1","status":"completed","output_file":"","summary":"done","session_id":"s"}"#;
        assert_eq!(
            parse_line(done),
            [StreamEvent::TaskDone("b8zpuyvzl".to_string())]
        );
        let updated = r#"{"type":"system","subtype":"task_updated","task_id":"b8zpuyvzl","patch":{"status":"completed"},"session_id":"s"}"#;
        assert!(parse_line(updated).is_empty());
    }

    #[test]
    fn a_run_settles_only_once_its_tasks_are_done() {
        let result = || StreamEvent::Result {
            error: None,
            no_turn: false,
        };
        let task = |id: &str| id.to_string();
        let mut settling = Settling::default();
        // No task: the first result settles the run.
        assert_eq!(settling.after(&result()), Some(true));

        let mut settling = Settling::default();
        assert_eq!(
            settling.after(&StreamEvent::TaskStarted(task("a"))),
            Some(false)
        );
        assert_eq!(
            settling.after(&StreamEvent::TaskStarted(task("b"))),
            Some(false)
        );
        // A background task outlives the first answer.
        assert_eq!(settling.after(&result()), Some(false));
        // Its end starts another answer, so the run is unsettled until that one's result.
        assert_eq!(
            settling.after(&StreamEvent::TaskDone(task("a"))),
            Some(false)
        );
        assert_eq!(settling.after(&result()), Some(false));
        assert_eq!(
            settling.after(&StreamEvent::TaskDone(task("b"))),
            Some(false)
        );
        assert_eq!(settling.after(&result()), Some(true));
        assert_eq!(settling.after(&StreamEvent::Text("hi".to_string())), None);
    }

    #[test]
    fn a_steering_message_is_a_user_frame_of_the_stream_json_input() {
        assert_eq!(
            input_frame("also handle the parser"),
            "{\"message\":{\"content\":\"also handle the parser\",\"role\":\"user\"},\
             \"type\":\"user\"}\n"
        );
    }
}
