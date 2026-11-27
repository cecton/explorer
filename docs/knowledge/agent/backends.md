---
title: The two model backends
summary: The two model backends: the genai-based HTTP client with its retries and session header, and the `claude` CLI driver with its settings and resumes.
refs: src/agent/llm.rs, src/agent/claude_code.rs, src/agent/mod.rs, Backend, Run, chat_options, Resume
---

There are two backends behind `agent::Backend`, and the turn picks one from the endpoint's kind. `src/agent/llm.rs` speaks to any OpenAI-compatible HTTP server and is the only file in the crate that imports `genai`; `src/agent/claude_code.rs` drives the local `claude` CLI as a subprocess, streaming its JSON output.

The HTTP backend streams, makes at most four attempts at one answer and retries only when nothing has streamed yet and the error is transient (408, 429, 5xx, a connection error). Its streaming client has a 30 s connect timeout and a 120 s read timeout but no total timeout, so a long answer that keeps streaming is never cut. It drops nameless tool calls, because replaying one makes the next request a 400. Every request carries an `x-opencode-session` header holding the conversation's session id: opencode rejects a request without one, and the other servers ignore it.

The Claude backend is started with a settings blob holding only `attribution` and `disableAllHooks` (hooks are code a repository supplies and the CLI runs before the model is called), with `--setting-sources user` so the project's own settings are never read, with `--system-prompt-snapshot off` (a resumed session would otherwise keep the mode of the node it was forked from), and with `docs/knowledge/**` disallowed for editing so the store only grows through the tool.

`Resume::{Fresh, Fork, Continue}` decide whether a run replays a session: two resumes of one interrupted node are two independent branches, so a fork is used rather than a continue. `Continue` is used only for the commit nudges. A fork whose session is gone is retried once as a fresh run, with the ancestors handed over as text; a message that has to continue a session the CLI no longer has fails the turn instead, since it only makes sense to a model that read the turn.
