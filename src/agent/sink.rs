//! `Sink`: everything the own agent loop reports while it runs, so the loop does not
//! care whether it writes a node's files directly (`Recorder`) or sends them elsewhere.

use std::future::Future;
use std::pin::Pin;

/// What a run of `agent_loop::run` reports and asks of whoever hosts it.
pub trait Sink: Send {
    /// The queue of what the user sends while the turn works.
    fn injector(&self) -> crate::agent::turn::live::Injector;

    /// Records one message of the transcript.
    fn message(&mut self, message: &crate::agent::Message);

    /// A new model answer starts streaming.
    fn begin_answer(&mut self);

    /// One streamed chunk of an answer: thinking or reply text.
    fn chunk(&mut self, is_thinking: bool, text: &str);

    /// The answer in progress died mid-stream; what streamed is kept.
    fn flush_partial_answer(&mut self);

    /// The answer in progress calls tools: its text is work in progress, not the reply.
    fn answer_calls_tools(&mut self);

    /// The sizes one request reported, as far as it reported them.
    fn request_size(&mut self, tokens_in: Option<u64>, cached_in: Option<u64>);

    /// A tool call starts: its description joins the work log.
    fn tool_call(&mut self, description: String);

    /// What the turn is doing now; `None` once the round's tools are done.
    fn activity(&mut self, activity: Option<String>);

    /// Runs one call of the `agent` tool and returns its result for the model.
    fn run_agent<'a>(
        &'a mut self,
        context: &'a crate::agent::tools::ToolContext,
        subagents: &'a crate::agent::subagent::Subagents,
        call: &'a crate::agent::ToolCall,
    ) -> Pin<Box<dyn Future<Output = String> + Send + 'a>>;
}
