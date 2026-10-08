use super::Effort;
use super::turn::Recorder;
use camino::Utf8Path;
use serde_json::Value;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// Removes Claude Code's own trailer from the commits it makes.
const NO_ATTRIBUTION: &str = r#"{"attribution":{"commit":"","pr":""}}"#;

/// A model of the local `claude` CLI and the effort to run it with.
#[derive(Clone, Debug)]
pub struct ClaudeTarget {
    pub model: String,
    pub effort: Effort,
}

/// The session a run starts from.
pub enum Resume<'a> {
    /// A new session with no history.
    Fresh,
    /// A copy of the parent node's session, which is left as it was.
    Fork(&'a str),
    /// The node's own session, appended to in place.
    Continue(&'a str),
}

pub struct Invocation<'a> {
    pub target: &'a ClaudeTarget,
    pub root: &'a Utf8Path,
    /// Appended to Claude Code's own system prompt.
    pub system: &'a str,
    pub prompt: &'a str,
    pub resume: Resume<'a>,
}

pub enum RunError {
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
    /// The run is over. `no_turn` is set when it ended before the model was called.
    Result {
        error: Option<String>,
        no_turn: bool,
    },
}

fn tool_activity(name: &str, input: &Value) -> String {
    let detail = ["command", "file_path", "pattern", "description"]
        .iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
        .unwrap_or("");
    let detail = detail.lines().next().unwrap_or("");
    format!("{name} {detail}").trim_end().to_string()
}

fn context_tokens(usage: &Value) -> Option<u64> {
    let field = |key: &str| usage.get(key).and_then(Value::as_u64);
    Some(
        field("input_tokens")?
            + field("cache_creation_input_tokens").unwrap_or(0)
            + field("cache_read_input_tokens").unwrap_or(0),
    )
}

/// Translates one output line. Lines of subagents and of kinds we do not use yield
/// nothing.
fn parse_line(line: &str) -> Vec<StreamEvent> {
    let Ok(message) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    if message
        .get("parent_tool_use_id")
        .is_some_and(|parent| !parent.is_null())
    {
        return Vec::new();
    }
    let text =
        |value: &Value, key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "system" if message.get("subtype").and_then(Value::as_str) == Some("init") => {
            text(&message, "session_id")
                .map(StreamEvent::Session)
                .into_iter()
                .collect()
        }
        "stream_event" => {
            let event = &message["event"];
            match event.get("type").and_then(Value::as_str).unwrap_or("") {
                "message_start" => {
                    let mut events = vec![StreamEvent::AnswerStart];
                    events.extend(
                        context_tokens(&event["message"]["usage"]).map(StreamEvent::TokensIn),
                    );
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
                "message_delta" => context_tokens(&event["usage"])
                    .map(StreamEvent::TokensIn)
                    .into_iter()
                    .collect(),
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

fn arguments(invocation: &Invocation<'_>) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-mode",
        "bypassPermissions",
        "--settings",
        NO_ATTRIBUTION,
        // A resumed session otherwise keeps the prompt of its first request, and with
        // it the mode of the node it was forked from.
        "--system-prompt-snapshot",
        "off",
        "--append-system-prompt",
        invocation.system,
        "--model",
        &invocation.target.model,
    ]
    .iter()
    .map(|arg| arg.to_string())
    .collect();
    if invocation.target.effort != Effort::Default {
        args.push("--effort".to_string());
        args.push(invocation.target.effort.as_str().to_string());
    }
    match invocation.resume {
        Resume::Fresh => {}
        Resume::Fork(session) => {
            args.extend(["--resume", session, "--fork-session"].map(str::to_string));
        }
        Resume::Continue(session) => args.extend(["--resume", session].map(str::to_string)),
    }
    args
}

/// Terminates the CLI's process group unless disarmed: a cancelled turn drops the
/// future mid-run, and SIGTERM lets Claude Code stop its own commands.
struct GroupTerminator(Option<u32>);

impl Drop for GroupTerminator {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            // SAFETY: plain syscall on a process group this process created.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGTERM);
            }
        }
    }
}

fn spawn_error(error: std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => "the `claude` command was not found on PATH".to_string(),
        _ => format!("failed to start claude: {error}"),
    }
}

/// Runs the CLI once and feeds its output to `recorder`. The session id is recorded as
/// soon as it is known, so it survives a failure or a cancellation.
pub async fn run(invocation: &Invocation<'_>, recorder: &mut Recorder) -> Result<(), RunError> {
    let mut child = tokio::process::Command::new("claude")
        .args(arguments(invocation))
        .current_dir(invocation.root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| RunError::Failed(spawn_error(e)))?;
    let mut terminator = GroupTerminator(child.id());

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(invocation.prompt.as_bytes()).await;
    }
    let stderr = child.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text).await;
            text
        })
    });

    let mut outcome = None;
    if let Some(stdout) = child.stdout.take() {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            for event in parse_line(&line) {
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
                    StreamEvent::Result { error, no_turn } => outcome = Some((error, no_turn)),
                }
            }
        }
    }
    let status = child.wait().await;
    terminator.0 = None;
    let stderr = match stderr {
        Some(reader) => reader.await.unwrap_or_default(),
        None => String::new(),
    };

    let resumed = !matches!(invocation.resume, Resume::Fresh);
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
    let mut command = tokio::process::Command::new("claude");
    command
        .args(["-p", "--output-format", "text", "--no-session-persistence"])
        .args(["--tools", "", "--system-prompt", system])
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
                StreamEvent::TokensIn(2 + 10352 + 8368)
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
    fn arguments_follow_effort_and_session() {
        let target = ClaudeTarget {
            model: "opus".to_string(),
            effort: Effort::High,
        };
        let invocation = |resume| Invocation {
            target: &target,
            root: Utf8Path::new("/repo"),
            system: "SYSTEM",
            prompt: "hi",
            resume,
        };
        let joined = |invocation: &Invocation<'_>| arguments(invocation).join(" ");

        let forked = joined(&invocation(Resume::Fork("abc")));
        assert!(forked.contains("--model opus --effort high"));
        assert!(!forked.contains("--disallowedTools"));
        assert!(forked.ends_with("--resume abc --fork-session"));
        assert!(forked.contains("--append-system-prompt SYSTEM"));
        assert!(forked.contains("--system-prompt-snapshot off"));
        assert!(!forked.contains("--bare"));

        let continued = joined(&invocation(Resume::Continue("abc")));
        assert!(continued.ends_with("--resume abc"));

        let default_effort = ClaudeTarget {
            model: "haiku".to_string(),
            effort: Effort::Default,
        };
        let fresh = Invocation {
            target: &default_effort,
            ..invocation(Resume::Fresh)
        };
        let fresh = joined(&fresh);
        assert!(!fresh.contains("--effort"));
        assert!(!fresh.contains("--resume"));
    }
}
