---
title: Testing a turn without a model
summary: The scripted local server that lets the agent loop run end to end in tests, and what the end-to-end tests pin.
refs: src/agent/fake_llm.rs, src/agent/turn/pin.rs, src/agent/child.rs
---

`src/agent/fake_llm.rs` is a scripted OpenAI-compatible server for tests that run the own agent loop end to end without a network or a real model. It listens on a random local port, answers each request with the next reply of a script over server-sent events, and records every request so the test can inspect it.

A scripted reply can carry thinking, text, tool calls and usage, and can hang instead of ending its stream, run something as the request arrives, or wait on a gate — which is how a test drives a cancel or a steering message at the right moment.

The child process the turn starts is the test binary re-executed as the agent (the `AS_AGENT` variable in `src/agent/child.rs`), so no `explorer` binary is needed either. `src/agent/turn/pin.rs` is the set of tests that use both: it pins what a turn records, the requests the model receives, cancellation, copies, compact turns and update turns.
