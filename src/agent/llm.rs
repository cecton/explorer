use super::{Effort, Message, ToolCall};
use futures_util::StreamExt;
use genai::adapter::AdapterKind;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, ContentPart, MessageContent,
    ReasoningEffort, Tool, ToolResponse,
};
use genai::resolver::{AuthData, Endpoint};
use genai::{Client, ModelIden, ServiceTarget};
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::Duration;

/// Where and how to run one model: an OpenAI-compatible server, a model and an effort.
#[derive(Clone, Debug)]
pub struct LlmTarget {
    pub url: String,
    pub token: String,
    pub model: String,
    pub effort: Effort,
}

/// A tool as advertised to the model.
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub schema: serde_json::Value,
}

/// One complete answer of the model: its text, the tools it asks for, and the size
/// of the request that produced it.
#[derive(Debug, Default)]
pub struct LlmResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    /// Tool calls left out of `tool_calls` because they arrived without a name.
    pub dropped_calls: usize,
    pub tokens_in: Option<u64>,
    /// Input tokens of this request the provider served from its prompt cache;
    /// `None` when the provider does not report caching at all.
    pub cached_in: Option<u64>,
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const READ_TIMEOUT: Duration = Duration::from_secs(120);
/// Requests made at most for one answer: the first one and its retries.
const MAX_ATTEMPTS: u32 = 4;
const BACKOFF_CAP: Duration = Duration::from_secs(30);

fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    // No total timeout, so a long answer that keeps streaming is never cut.
    CLIENT.get_or_init(|| web_client(Some(READ_TIMEOUT)))
}

/// A complete answer arrives only once it is ready, so it gets no read timeout.
fn complete_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| web_client(None))
}

fn web_client(read_timeout: Option<Duration>) -> Client {
    Client::builder()
        .with_web_config(genai::WebConfig {
            connect_timeout: Some(CONNECT_TIMEOUT),
            read_timeout,
            ..Default::default()
        })
        .build()
}

fn service_target(target: &LlmTarget) -> ServiceTarget {
    // The adapter joins `chat/completions` onto this URL, which needs the final slash.
    let url = format!("{}/", target.url.trim_end_matches('/'));
    ServiceTarget {
        endpoint: Endpoint::from_owned(url),
        auth: AuthData::from_single(target.token.clone()),
        model: ModelIden::new(AdapterKind::OpenAI, target.model.as_str()),
    }
}

fn chat_options(effort: Effort, session: &str) -> ChatOptions {
    // opencode rejects requests without a session id; other servers ignore the header.
    let options = ChatOptions::default()
        .with_capture_usage(true)
        .with_capture_content(true)
        .with_capture_tool_calls(true)
        .with_extra_headers([("x-opencode-session", session)]);
    match effort {
        Effort::Default => options,
        Effort::Low => options.with_reasoning_effort(ReasoningEffort::Low),
        Effort::Medium => options.with_reasoning_effort(ReasoningEffort::Medium),
        Effort::High => options.with_reasoning_effort(ReasoningEffort::High),
    }
}

fn to_genai(message: &Message) -> ChatMessage {
    match message {
        Message::User { content, .. } => ChatMessage::user(content.as_str()),
        Message::Assistant {
            content,
            tool_calls,
        } => {
            let mut parts: Vec<ContentPart> = Vec::with_capacity(tool_calls.len() + 1);
            if !content.is_empty() {
                parts.push(ContentPart::Text(content.clone()));
            }
            for call in tool_calls {
                parts.push(ContentPart::ToolCall(genai::chat::ToolCall {
                    call_id: call.id.clone(),
                    fn_name: call.name.clone(),
                    fn_arguments: call.arguments.clone(),
                    thought_signatures: None,
                }));
            }
            ChatMessage::assistant(MessageContent::from_parts(parts))
        }
        Message::Tool {
            call_id,
            name,
            content,
        } => ChatMessage::from(
            ToolResponse::new(call_id.as_str(), content.as_str()).with_fn_name(name.as_str()),
        ),
    }
}

fn request(system: &str, messages: &[Message], tools: &[ToolSpec]) -> ChatRequest {
    let request = ChatRequest::new(messages.iter().map(to_genai).collect()).with_system(system);
    if tools.is_empty() {
        return request;
    }
    request.with_tools(tools.iter().map(|tool| {
        Tool::new(tool.name.as_str())
            .with_description(tool.description.as_str())
            .with_schema(tool.schema.clone())
    }))
}

const ERROR_MAX_CHARS: usize = 300;

/// A provider error on one line, cut short: the tail is usually a dump of the response.
fn error_text(error: genai::Error) -> String {
    one_line(&error.to_string())
}

fn one_line(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= ERROR_MAX_CHARS {
        return flat;
    }
    let mut cut: String = flat.chars().take(ERROR_MAX_CHARS).collect();
    cut.push('…');
    cut
}

/// Runs one provider turn for `session`; retried only while nothing has streamed to `on_chunk`.
pub async fn stream_chat(
    target: &LlmTarget,
    session: &str,
    system: &str,
    messages: &[Message],
    tools: &[ToolSpec],
    mut on_chunk: impl FnMut(bool, &str),
) -> Result<LlmResponse, String> {
    let mut attempt = 1;
    loop {
        let mut streamed = false;
        let error = match attempt_chat(
            target,
            session,
            system,
            messages,
            tools,
            &mut streamed,
            &mut on_chunk,
        )
        .await
        {
            Ok(response) => return Ok(response),
            Err(error) => error,
        };
        if streamed || attempt >= MAX_ATTEMPTS || !is_transient(&error) {
            let mut text = error_text(error);
            if attempt > 1 {
                text.push_str(&format!(" (after {attempt} attempts)"));
            }
            return Err(text);
        }
        tokio::time::sleep(backoff(attempt, jitter())).await;
        attempt += 1;
    }
}

async fn attempt_chat(
    target: &LlmTarget,
    session: &str,
    system: &str,
    messages: &[Message],
    tools: &[ToolSpec],
    streamed: &mut bool,
    on_chunk: &mut impl FnMut(bool, &str),
) -> Result<LlmResponse, genai::Error> {
    let options = chat_options(target.effort, session);
    let mut stream = client()
        .exec_chat_stream(
            service_target(target),
            request(system, messages, tools),
            Some(&options),
        )
        .await?
        .stream;

    let mut response = LlmResponse::default();
    while let Some(event) = stream.next().await {
        match event? {
            ChatStreamEvent::Chunk(chunk) => {
                *streamed = true;
                on_chunk(false, &chunk.content);
                response.text.push_str(&chunk.content);
            }
            ChatStreamEvent::ReasoningChunk(chunk) => {
                *streamed = true;
                on_chunk(true, &chunk.content);
            }
            ChatStreamEvent::End(end) => {
                let usage = end.captured_usage.as_ref();
                response.tokens_in = usage
                    .and_then(|usage| usage.prompt_tokens)
                    .and_then(|tokens| u64::try_from(tokens).ok());
                response.cached_in = usage
                    .and_then(|usage| usage.prompt_tokens_details.as_ref())
                    .and_then(|details| details.cached_tokens)
                    .and_then(|tokens| u64::try_from(tokens).ok());
                (response.tool_calls, response.dropped_calls) =
                    named_calls(end.captured_into_tool_calls().unwrap_or_default());
            }
            _ => {}
        }
    }
    Ok(response)
}

/// The calls with a name, and how many were left out. A provider that splits a call
/// across chunks can hand over one with no name; replayed in the transcript it makes
/// the next request a 400.
fn named_calls(calls: Vec<genai::chat::ToolCall>) -> (Vec<ToolCall>, usize) {
    let received = calls.len();
    let named: Vec<ToolCall> = calls
        .into_iter()
        .filter(|call| !call.fn_name.trim().is_empty())
        .map(|call| ToolCall {
            id: call.call_id,
            name: call.fn_name,
            arguments: tool_arguments(call.fn_arguments),
        })
        .collect();
    let dropped = received - named.len();
    (named, dropped)
}

/// Whether the same request may succeed when made again (server or network failure, not a refusal).
fn is_transient(error: &genai::Error) -> bool {
    use genai::Error;
    use genai::webc::Error as Web;
    fn status_is_transient(status: u16) -> bool {
        matches!(status, 408 | 429 | 500..=599)
    }
    // A malformed URL or header fails the request before it is sent: retrying cannot help.
    fn reqwest_is_transient(error: &reqwest::Error) -> bool {
        !error.is_builder() && !error.is_redirect()
    }
    match error {
        Error::HttpError { status, .. } => status_is_transient(status.as_u16()),
        Error::WebModelCall { webc_error, .. } | Error::WebAdapterCall { webc_error, .. } => {
            match webc_error {
                Web::Reqwest(error) => reqwest_is_transient(error),
                Web::ResponseFailedStatus { status, .. } => status_is_transient(status.as_u16()),
                _ => false,
            }
        }
        // genai wraps status errors in the stream error; other stream failures are dropped connections.
        Error::WebStream { error, .. } => {
            if let Some(inner) = error.downcast_ref::<Error>() {
                is_transient(inner)
            } else if let Some(error) = error.downcast_ref::<reqwest::Error>() {
                reqwest_is_transient(error)
            } else {
                true
            }
        }
        Error::StreamParse { .. } => true,
        // A failure the provider reported inside a stream that was answering.
        _ => false,
    }
}

/// Pause before the next attempt: 1s, 2s, 4s, ... capped, scaled by `jitter` in `0.5..1.5`.
fn backoff(attempt: u32, jitter: f64) -> Duration {
    let base = Duration::from_secs(1 << attempt.saturating_sub(1).min(5)).min(BACKOFF_CAP);
    base.mul_f64(jitter)
}

fn jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    0.5 + f64::from(nanos % 1000) / 1000.0
}

/// genai hands over unparsable arguments as a string, `""` for a call without any.
fn tool_arguments(arguments: Value) -> Value {
    match arguments {
        Value::Null => json!({}),
        Value::String(text) if text.trim().is_empty() => json!({}),
        other => other,
    }
}

/// One non-streaming call without tools; returns the answer text.
pub async fn complete(
    target: &LlmTarget,
    session: &str,
    system: &str,
    user: &str,
) -> Result<String, String> {
    let options = chat_options(target.effort, session);
    let messages = [Message::User {
        content: user.to_string(),
        injected: false,
    }];
    let response = complete_client()
        .exec_chat(
            service_target(target),
            request(system, &messages, &[]),
            Some(&options),
        )
        .await
        .map_err(error_text)?;
    response
        .first_text()
        .map(str::to_string)
        .ok_or_else(|| "empty answer".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_without_arguments_get_an_empty_object() {
        assert_eq!(tool_arguments(json!("")), json!({}));
        assert_eq!(tool_arguments(Value::Null), json!({}));
        assert_eq!(tool_arguments(json!({"id": 4})), json!({"id": 4}));
        assert_eq!(tool_arguments(json!("{oops")), json!("{oops"));
    }

    #[test]
    fn calls_without_a_name_are_left_out_and_counted() {
        let call = |name: &str| genai::chat::ToolCall {
            call_id: "c".into(),
            fn_name: name.into(),
            fn_arguments: json!({}),
            thought_signatures: None,
        };
        let (kept, dropped) = named_calls(vec![call("read"), call(""), call(" \t"), call("grep")]);
        let names: Vec<&str> = kept.iter().map(|call| call.name.as_str()).collect();
        assert_eq!(names, ["read", "grep"]);
        assert_eq!(dropped, 2);
    }

    #[test]
    fn one_line_flattens_and_caps_error_text() {
        assert_eq!(
            one_line("Web error.\n  Cause: HTTP 500\n"),
            "Web error. Cause: HTTP 500"
        );
        let long = one_line(&"x".repeat(400));
        assert_eq!(long.chars().count(), ERROR_MAX_CHARS + 1);
        assert!(long.ends_with('…'));
    }

    fn http(status: u16) -> genai::Error {
        genai::Error::HttpError {
            status: reqwest::StatusCode::from_u16(status).unwrap(),
            canonical_reason: String::new(),
            body: String::new(),
        }
    }

    fn stream_error(error: impl Into<genai::BoxError>) -> genai::Error {
        genai::Error::WebStream {
            model_iden: ModelIden::new(AdapterKind::OpenAI, "m"),
            cause: String::new(),
            error: error.into(),
        }
    }

    #[test]
    fn server_failures_are_transient_and_refusals_are_not() {
        for status in [408, 429, 500, 502, 503, 504] {
            assert!(is_transient(&http(status)), "{status}");
        }
        for status in [400, 401, 403, 404] {
            assert!(!is_transient(&http(status)), "{status}");
        }
    }

    #[test]
    fn stream_errors_follow_the_status_they_wrap() {
        assert!(is_transient(&stream_error(http(502))));
        assert!(!is_transient(&stream_error(http(401))));
        assert!(is_transient(&stream_error("error decoding response body")));
    }

    #[test]
    fn a_request_that_cannot_be_built_is_not_retried() {
        let error = reqwest::Client::new().get("not a url").build().unwrap_err();
        let failed = genai::Error::WebModelCall {
            model_iden: ModelIden::new(AdapterKind::OpenAI, "m"),
            webc_error: genai::webc::Error::Reqwest(error),
        };
        assert!(!is_transient(&failed));
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        assert_eq!(backoff(1, 1.0), Duration::from_secs(1));
        assert_eq!(backoff(3, 1.0), Duration::from_secs(4));
        assert_eq!(backoff(9, 1.0), BACKOFF_CAP);
        assert_eq!(backoff(2, 0.5), Duration::from_secs(1));
    }
}
