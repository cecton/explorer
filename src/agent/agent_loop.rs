use super::Message;
use super::llm::{self, LlmTarget};
use super::subagent;
use super::tools::{self, ToolContext};
use super::turn::Recorder;
use std::future::Future;
use std::pin::Pin;

/// Our own agent loop: ask the model, run the tools it asks for, feed the results
/// back, and stop when an answer asks for none. Boxed: a sub-agent call runs it in turn.
pub fn run<'a>(
    context: &'a ToolContext,
    target: &'a LlmTarget,
    session: &'a str,
    system: &'a str,
    messages: &'a mut Vec<Message>,
    recorder: &'a mut Recorder,
) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
    Box::pin(run_loop(
        context, target, session, system, messages, recorder,
    ))
}

async fn run_loop(
    context: &ToolContext,
    target: &LlmTarget,
    session: &str,
    system: &str,
    messages: &mut Vec<Message>,
    recorder: &mut Recorder,
) -> Result<(), String> {
    let tool_specs = tools::specs(context);
    loop {
        recorder.begin_answer();
        let response = llm::stream_chat(
            target,
            session,
            system,
            messages,
            &tool_specs,
            |is_thinking, text| {
                if is_thinking {
                    recorder.reasoning(text);
                } else {
                    recorder.text(text);
                }
            },
        )
        .await?;
        if let Some(tokens) = response.tokens_in {
            recorder.tokens_in(tokens);
        }
        recorder.cached_in(response.cached_in);
        let answer = Message::Assistant {
            content: response.text,
            tool_calls: response.tool_calls.clone(),
        };
        recorder.message(&answer);
        messages.push(answer);
        if response.tool_calls.is_empty() {
            return Ok(());
        }
        recorder.answer_calls_tools();
        for call in &response.tool_calls {
            recorder.tool_call(tools::describe(call));
            let content = match &context.subagents {
                Some(subagents) if call.name == subagent::TOOL => {
                    subagent::run(context, subagents, call, recorder).await
                }
                _ => tools::run(context, call).await,
            };
            let result = Message::Tool {
                call_id: call.id.clone(),
                name: call.name.clone(),
                content,
            };
            recorder.message(&result);
            messages.push(result);
        }
        recorder.activity(None);
    }
}
