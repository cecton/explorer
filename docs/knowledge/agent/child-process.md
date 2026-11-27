---
title: `explorer agent`: the loop in its own process
summary: The `explorer agent` process: why the loop runs in a child, the line protocol, which file is which side, and the compact turn's branch.
refs: src/agent/child.rs, src/agent/wire.rs, src/agent/driver.rs, src/agent/sink.rs, Start, Event, Sink
---

The own agent loop runs in a process of its own, `explorer agent`, so that everything the agent does — its tools, its shell, the MCP servers it starts — happens where the process runs and can be confined there, and so that a cancel can kill one process group instead of unwinding a loop.

The protocol is private and JSON lines on the child's standard streams (`src/agent/wire.rs`): the child reads one `Start` and then `Command`s on standard input and writes `Event`s on standard output; nothing else may go to that stream. Both ends are the same binary, so the format changes whenever the code does.

`driver.rs` is the parent's side: it starts the process from `/proc/self/exe` (so a rebuilt binary still runs the matching loop), applies every event to the recorder, answers `Poll` with the messages the user injected and `Settle` with a commit nudge or a finish. `sink.rs` is the loop's side: the trait is the loop's only way out, and the child's implementation turns every call into a wire event. `child.rs` is the process: it reads the start line, builds the tool context, MCP host and sub-agents, runs the loop, and dies on SIGTERM or the end of its input, killing its background commands and servers.

A compact turn takes a short branch there instead of the loop: it is one answer with no tools, so it cannot use the loop's tool calls, stall detector or nudges, and it gets no MCP host.

The token of the endpoint travels in `Start` rather than in `argv`, so it never shows in a process listing.
