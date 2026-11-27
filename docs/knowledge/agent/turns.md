---
title: A turn, and when it may run
summary: What a turn is, what its task does around the loop (panic report, copy claim, compact branch), and when it may run beside another.
refs: src/agent/turn/mod.rs, src/agent/mod.rs, Mode, TurnRequest, Claim, admit, spawn, report_panic, with_compact_hint, CopyMade
---

A turn is one prompt and everything that follows from it, run on a task of its own so the UI thread never waits. `Mode` is `Feedback` (the agent explains and advises; what it changes is discarded) or `Commit` (it carries out what was decided and commits).

A `TurnRequest` is everything the turn needs and nothing it could look up later: the mode, the node and its directory, the ancestor directories to replay, HEAD when the prompt was sent and HEAD when the parent's turn ended, the workspace plan, the backend, endpoint and model spec, the agent config for sub-agents, the tree handle and the MCP host.

`spawn` sets the task's shape: it pre-sets the live model and prompt _before_ returning, so the window's first frame paints what it was just sent; it prepares the workspace before the sandbox; and it wraps the body in `report_panic`, which sends `TurnFinished` with `Failed("the turn panicked")` when the task panicked and nothing was sent yet, so a node is never left `running` by a panic. `with_compact_hint` appends "use /compact from the parent node" when the failure looks like a context-length refusal.

A `/compact` turn is a `TurnRequest` with `compact` set: it asks for a summary instead of sending the prompt, gets no feedback note, and its claim is `Compact`.

`Claim` is what the turn needs of the repository: `Exclusive`, `Copy`, `Compact` or `Copying` (a `Copy` whose copy is still being made). `admit` decides what may start beside it: an `Exclusive` claim is admitted beside copies but refused beside another `Exclusive` turn or while a copy is still being made, since the walk reads the tree such a turn would write. Copies are capped by `parallel-copies`, a copy still being made taking a slot too. `TurnHandle::claim` is not fixed at spawn: a copy turn starts as `Copying` and reports `Copy` once the walk ends, through an atomic `CopyMade` sets on drop, which also sends `TurnCopyMade` to release the queue.
