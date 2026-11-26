//! What `explorer agent` and the turn that runs it say to each other, one JSON line at a
//! time: the child reads a `Start` and then `Command`s on its standard input, and writes
//! `Event`s on its standard output. Both ends are the same binary, so the format is
//! private and changes whenever the code does; the parent stays the only writer of the
//! conversation tree and applies every event to its own recorder.

/// The first line the child reads: all it needs, token included, so it never travels in argv.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Start {
    pub target: crate::agent::llm::LlmTarget,
    pub session: String,
    pub system: String,
    /// The conversation so far, ending on the message to answer; already recorded.
    pub messages: Vec<crate::agent::Message>,
    pub root: camino::Utf8PathBuf,
    pub node: String,
    /// Where background commands write their output.
    pub node_dir: camino::Utf8PathBuf,
    /// The MCP servers the run starts for itself and takes the tools of.
    pub mcp: Vec<crate::agent::mcp::McpServerDecl>,
    /// The turn's socket, which answers the tools only Explorer can serve.
    pub socket: Option<camino::Utf8PathBuf>,
    /// What the run needs to call sub-agents, which run inside this process.
    pub agents: Option<crate::agent::subagent::Agents>,
    pub repeat_limit: u32,
    /// A single answer without tools, asking for a summary.
    pub compact: bool,
    /// What the turn may do with the knowledge base.
    pub knowledge: crate::knowledge::Access,
}

/// What the child reports, in the order the run produces it.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Message {
        message: crate::agent::Message,
    },
    BeginAnswer,
    Chunk {
        thinking: bool,
        text: String,
    },
    FlushPartialAnswer,
    AnswerCallsTools,
    RequestSize {
        tokens_in: Option<u64>,
        cached_in: Option<u64>,
    },
    ToolCall {
        description: String,
    },
    Activity {
        activity: Option<String>,
    },
    /// A round starts: the run waits to be told what the user sent since the last one.
    Poll,
    /// A message of the user joined the conversation: it leaves the parent's queue now.
    Injected {
        content: String,
    },
    /// The last answer called no tool: the run waits to be told whether it goes on.
    Settle,
    /// The run is over; `error` is why it failed.
    Finished {
        error: Option<String>,
    },
}

/// What the parent tells a running child.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Command {
    /// Unread user messages, oldest first: the answer to a `Poll`, or to a `Settle` with some.
    Messages { contents: Vec<String> },
    /// A note of Explorer's, already recorded by the parent, to answer a `Settle` with.
    Note { message: crate::agent::Message },
    /// Nothing more is coming, to answer a `Settle` with: the run ends.
    Finish,
}

/// Writes `value` as one line.
pub fn line<T: serde::Serialize>(value: &T) -> String {
    let mut line = serde_json::to_string(value).expect("wire types serialize");
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_is_one_line_that_reads_back() {
        use crate::agent::Message;
        let event = Event::Message {
            message: Message::User {
                content: "line one\nline two".to_string(),
                injected: true,
            },
        };
        let written = line(&event);
        assert_eq!(written.matches('\n').count(), 1);
        let Ok(Event::Message { message }) = serde_json::from_str::<Event>(&written) else {
            panic!("the event did not read back");
        };
        assert_eq!(
            message,
            Message::User {
                content: "line one\nline two".to_string(),
                injected: true
            }
        );
    }
}
