//! `explorer agent`: the own agent loop in a process of its own, so that everything the
//! agent does (its tools, its shell, its MCP servers) happens where this process runs and
//! can be confined there. It reads one `Start` and then `Command`s on its standard input,
//! writes `Event`s on its standard output (nothing else may go there) and never touches
//! the conversation tree: the turn that started it applies the events.
//!
//! SIGTERM, or the end of its standard input, stops it: the run is dropped, which kills
//! the background commands and the MCP servers it started.

use std::future::Future;
use std::pin::Pin;

/// The sink of the loop: events out on standard output, commands in from the parent.
struct WireSink {
    commands: tokio::sync::mpsc::UnboundedReceiver<crate::agent::wire::Command>,
    /// What came with the answer to a `Settle`, for the next round to take.
    arrived: Vec<crate::agent::Message>,
    /// The sub-agents the run may call, set once it has its tools.
    agents: Option<crate::agent::subagent::Subagents>,
}

impl WireSink {
    fn emit(event: &crate::agent::wire::Event) {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        // A parent that stopped reading is gone: its end of stdin closes and stops the run.
        let _ = out
            .write_all(crate::agent::wire::line(event).as_bytes())
            .and_then(|()| out.flush());
    }

    /// The user's messages as they join the conversation; the parent dequeues each now.
    fn read(contents: Vec<String>) -> impl Iterator<Item = crate::agent::Message> {
        use crate::agent::Message;
        use crate::agent::wire::Event;
        contents.into_iter().map(|content| {
            Self::emit(&Event::Injected {
                content: content.clone(),
            });
            Message::User {
                content,
                injected: false,
            }
        })
    }
}

impl crate::agent::sink::Sink for WireSink {
    fn injected(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Vec<crate::agent::Message>> + Send + '_>> {
        use crate::agent::wire::{Command, Event};
        Box::pin(async move {
            let mut messages = std::mem::take(&mut self.arrived);
            Self::emit(&Event::Poll);
            if let Some(Command::Messages { contents }) = self.commands.recv().await {
                messages.extend(Self::read(contents));
            }
            messages
        })
    }

    fn settle(&mut self) -> Pin<Box<dyn Future<Output = bool> + Send + '_>> {
        use crate::agent::wire::{Command, Event};
        Box::pin(async move {
            Self::emit(&Event::Settle);
            match self.commands.recv().await {
                Some(Command::Messages { contents }) => self.arrived.extend(Self::read(contents)),
                Some(Command::Note { message }) => self.arrived.push(message),
                Some(Command::Finish) | None => return false,
            }
            true
        })
    }

    fn message(&mut self, message: &crate::agent::Message) {
        use crate::agent::wire::Event;
        Self::emit(&Event::Message {
            message: message.clone(),
        });
    }

    fn begin_answer(&mut self) {
        use crate::agent::wire::Event;
        Self::emit(&Event::BeginAnswer);
    }

    fn chunk(&mut self, is_thinking: bool, text: &str) {
        use crate::agent::wire::Event;
        Self::emit(&Event::Chunk {
            thinking: is_thinking,
            text: text.to_string(),
        });
    }

    fn flush_partial_answer(&mut self) {
        use crate::agent::wire::Event;
        Self::emit(&Event::FlushPartialAnswer);
    }

    fn answer_calls_tools(&mut self) {
        use crate::agent::wire::Event;
        Self::emit(&Event::AnswerCallsTools);
    }

    fn request_size(&mut self, tokens_in: Option<u64>, cached_in: Option<u64>) {
        use crate::agent::wire::Event;
        Self::emit(&Event::RequestSize {
            tokens_in,
            cached_in,
        });
    }

    fn tool_call(&mut self, description: String) {
        use crate::agent::wire::Event;
        Self::emit(&Event::ToolCall { description });
    }

    fn activity(&mut self, activity: Option<String>) {
        use crate::agent::wire::Event;
        Self::emit(&Event::Activity { activity });
    }

    fn run_agent<'a>(
        &'a mut self,
        call: &'a crate::agent::ToolCall,
    ) -> Pin<Box<dyn Future<Output = String> + Send + 'a>> {
        Box::pin(async move {
            match &mut self.agents {
                Some(agents) => agents.run(call).await,
                None => "error: this run may not start sub-agents".to_string(),
            }
        })
    }
}

/// Runs what `start` describes; the error is the run's failure.
async fn run(start: crate::agent::wire::Start, sink: &mut WireSink) -> Result<(), String> {
    use crate::agent::Message;
    use crate::agent::agent_loop::Limits;
    use crate::agent::mcp::{McpHost, McpTools, Notify};
    use crate::agent::sink::Sink;
    use crate::agent::subagent::Subagents;
    use crate::agent::tools::ToolContext;
    use crate::agent::wire::Start;
    use crate::conversation::NodeId;
    use std::sync::Arc;
    let Start {
        target,
        session,
        system,
        mut messages,
        root,
        node,
        node_dir,
        mcp,
        socket,
        agents,
        repeat_limit,
        compact,
        knowledge,
    } = start;
    // A test child that writes a JSON line which is no event, then waits for its parent.
    #[cfg(test)]
    if let Some(stderr) = system.strip_prefix(GARBLE) {
        use std::io::Write;
        // The reason goes first: the parent stops this child as soon as it reads the line.
        if !stderr.is_empty() {
            eprintln!("{stderr}");
        }
        println!("{{\"not\": \"an event\"");
        let _ = std::io::stdout().flush();
        std::future::pending::<()>().await;
    }
    if compact {
        // A compact turn is a single answer, without tools.
        sink.begin_answer();
        let response = match crate::agent::llm::stream_chat(
            &target,
            &session,
            &system,
            &messages,
            &[],
            |is_thinking, text| sink.chunk(is_thinking, text),
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                sink.flush_partial_answer();
                return Err(error);
            }
        };
        sink.request_size(response.tokens_in, response.cached_in);
        sink.message(&Message::Assistant {
            content: response.text,
            tool_calls: Vec::new(),
        });
        return Ok(());
    }
    let host = Arc::new(McpHost::new(root.clone()));
    let notify: Notify = Arc::new(|_| {});
    for decl in &mcp {
        host.start(decl, Arc::clone(&notify));
    }
    let mcp = if mcp.is_empty() {
        McpTools::default()
    } else {
        host.snapshot().await
    };
    let context = Arc::new(ToolContext {
        root,
        tree: Arc::default(),
        node: NodeId::new(&node),
        node_dir: Some(node_dir),
        background: Arc::default(),
        mcp,
        privileged: socket,
        knowledge,
    });
    let agent_tool = agents.as_ref().map(|agents| agents.tool.clone());
    sink.agents = agents.map(|agents| {
        Subagents::new(
            agents,
            Arc::clone(&context),
            format!("{session}-{node}"),
            repeat_limit,
        )
    });
    let limits = Limits {
        budget: None,
        repeat_limit,
    };
    crate::agent::agent_loop::run(
        &context,
        agent_tool,
        &target,
        &session,
        &system,
        &mut messages,
        sink,
        limits,
    )
    .await
}

/// The `agent` subcommand; returns the exit code.
pub async fn main() -> i32 {
    use crate::agent::wire::{Command, Event, Start};
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).split(b'\n');
    let start = match lines.next_segment().await {
        Ok(Some(line)) => match serde_json::from_slice::<Start>(&line) {
            Ok(start) => start,
            Err(error) => {
                eprintln!("the first line is not a start: {error}");
                return 2;
            }
        },
        _ => {
            eprintln!("no start on the standard input");
            return 2;
        }
    };
    let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    else {
        eprintln!("could not listen for SIGTERM");
        return 2;
    };
    let (commands_tx, commands) = tokio::sync::mpsc::unbounded_channel();
    // Ends with the standard input: the parent is gone.
    let mut parent = tokio::spawn(async move {
        while let Ok(Some(line)) = lines.next_segment().await {
            if let Ok(command) = serde_json::from_slice::<Command>(&line)
                && commands_tx.send(command).is_err()
            {
                break;
            }
        }
    });
    let mut sink = WireSink {
        commands,
        arrived: Vec::new(),
        agents: None,
    };
    let finished = tokio::select! {
        result = run(start, &mut sink) => result,
        _ = terminate.recv() => return 143,
        _ = &mut parent => return 1,
    };
    // Kills the background commands before the parent sees the run end.
    sink.agents = None;
    WireSink::emit(&Event::Finished {
        error: finished.err(),
    });
    0
}

/// Set for the process a test starts in place of `explorer agent`.
#[cfg(test)]
pub(crate) const AS_AGENT: &str = "EXPLORER_TEST_AS_AGENT";

/// The system prompt of a test run that garbles its output; the rest is written to stderr.
#[cfg(test)]
pub(crate) const GARBLE: &str = "EXPLORER_TEST_GARBLE:";

#[cfg(test)]
mod tests {
    use super::*;

    /// Not a test: the process a test starts in place of `explorer agent` runs it.
    #[test]
    fn as_agent() {
        if std::env::var_os(AS_AGENT).is_none() {
            return;
        }
        // Dropping the runtime first would wait on the stdin reader, and the child would linger.
        let runtime = tokio::runtime::Runtime::new().expect("a runtime");
        let code = runtime.block_on(main());
        std::process::exit(code);
    }

    #[tokio::test]
    async fn a_child_given_no_start_refuses_and_says_why() {
        use std::process::Stdio;
        use tokio::io::AsyncWriteExt;
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "agent::child::tests::as_agent", "--nocapture"])
            .env(AS_AGENT, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"not a start\n")
            .await
            .unwrap();
        let output = child.wait_with_output().await.unwrap();
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("the first line is not a start"), "{stderr}");
    }
}
