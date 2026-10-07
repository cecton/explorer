use super::{Effort, Message, ToolCall};
use futures_util::StreamExt;
use genai::adapter::AdapterKind;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, ContentPart, MessageContent,
    ReasoningEffort, Tool, ToolResponse,
};
use genai::resolver::{AuthData, Endpoint};
use genai::{Client, ModelIden, ServiceTarget};
use std::sync::OnceLock;

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
    pub name: &'static str,
    pub description: &'static str,
    pub schema: serde_json::Value,
}

/// One complete answer of the model: its text, the tools it asks for, and the size
/// of the request that produced it.
#[derive(Debug, Default)]
pub struct LlmResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub tokens_in: Option<u64>,
}

fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(Client::default)
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
        Tool::new(tool.name)
            .with_description(tool.description)
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

/// Runs one provider turn, calling `on_text` for every piece of text as it arrives.
/// `session` names the conversation the request belongs to.
pub async fn stream_chat(
    target: &LlmTarget,
    session: &str,
    system: &str,
    messages: &[Message],
    tools: &[ToolSpec],
    mut on_chunk: impl FnMut(bool, &str),
) -> Result<LlmResponse, String> {
    let options = chat_options(target.effort, session);
    let mut stream = client()
        .exec_chat_stream(
            service_target(target),
            request(system, messages, tools),
            Some(&options),
        )
        .await
        .map_err(error_text)?
        .stream;

    let mut response = LlmResponse::default();
    while let Some(event) = stream.next().await {
        match event.map_err(error_text)? {
            ChatStreamEvent::Chunk(chunk) => {
                on_chunk(false, &chunk.content);
                response.text.push_str(&chunk.content);
            }
            ChatStreamEvent::ReasoningChunk(chunk) => on_chunk(true, &chunk.content),
            ChatStreamEvent::End(end) => {
                response.tokens_in = end
                    .captured_usage
                    .as_ref()
                    .and_then(|usage| usage.prompt_tokens)
                    .and_then(|tokens| u64::try_from(tokens).ok());
                response.tool_calls = end
                    .captured_into_tool_calls()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|call| ToolCall {
                        id: call.call_id,
                        name: call.fn_name,
                        arguments: call.fn_arguments,
                    })
                    .collect();
            }
            _ => {}
        }
    }
    Ok(response)
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
    let response = client()
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
    fn one_line_flattens_and_caps_error_text() {
        assert_eq!(
            one_line("Web error.\n  Cause: HTTP 500\n"),
            "Web error. Cause: HTTP 500"
        );
        let long = one_line(&"x".repeat(400));
        assert_eq!(long.chars().count(), ERROR_MAX_CHARS + 1);
        assert!(long.ends_with('…'));
    }
}
