---
title: A turn, and when it may run
summary: What a turn is: the two modes, the request that carries everything it needs, the claim it makes on the repository, and when it may start.
refs: src/agent/turn/mod.rs, src/agent/mod.rs, Mode, TurnRequest, Claim, admit, spawn
---

A turn is one prompt and everything that follows from it, run on a task of its own so the UI thread never waits. `Mode` is `Feedback` (the agent explains and advises; what it changes is discarded) or `Commit` (it carries out what was decided and commits).

A `TurnRequest` is everything the turn needs and nothing it could look up later: the mode, the node and its directory, the ancestor directories to replay, HEAD when the prompt was sent and HEAD when the parent's turn ended, the workspace plan, the backend, endpoint and model spec, the agent config for sub-agents, the tree handle and the MCP host. `spawn` returns a `TurnHandle` (the live state the window reads, plus a cancel channel) and starts the task.

`Claim` is what the turn needs of the repository: `Exclusive` (a commit turn, or a feedback turn working in place), `Copy` (a feedback turn in its own copy) or `Compact` (no tools). `admit` (`src/agent/turn/mod.rs`) decides whether it may start beside the running turns: anything running exclusively blocks every other turn, a compact one included; an exclusive turn starts only when nothing else is running; copies are capped by `parallel-copies`.
