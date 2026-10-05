use super::Message;
use super::llm::{self, LlmTarget};
use super::tools::{self, ToolContext};
use super::turn::Recorder;

/// Our own agent loop: ask the model, run the tools it asks for, feed the results
/// back, and stop when an answer asks for none.
pub async fn run(
    context: &ToolContext,
    target: &LlmTarget,
    system: &str,
    messages: &mut Vec<Message>,
    recorder: &mut Recorder,
) -> Result<(), String> {
    let tool_specs = tools::specs(context.mode);
    loop {
        recorder.begin_answer();
        let response = llm::stream_chat(target, system, messages, &tool_specs, |text| {
            recorder.text(text);
        })
        .await?;
        if let Some(tokens) = response.tokens_in {
            recorder.tokens_in(tokens);
        }
        let answer = Message::Assistant {
            content: response.text,
            tool_calls: response.tool_calls.clone(),
        };
        recorder.message(&answer);
        messages.push(answer);
        if response.tool_calls.is_empty() {
            return Ok(());
        }
        for call in &response.tool_calls {
            recorder.activity(Some(tools::describe(call)));
            let result = Message::Tool {
                call_id: call.id.clone(),
                name: call.name.clone(),
                content: tools::run(context, call).await,
            };
            recorder.message(&result);
            messages.push(result);
        }
        recorder.activity(None);
    }
}
