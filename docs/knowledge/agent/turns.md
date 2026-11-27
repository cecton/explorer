---
title: A turn, and when it may run
summary: What a turn is: the two modes, the request that carries everything it needs, the claim it makes on the repository, and when it may run beside another turn.
refs: src/agent/turn/mod.rs, src/agent/mod.rs, Mode, TurnRequest, Claim, admit, spawn, TurnHandle::claim
---

A `TurnRequest` is everything the turn needs and nothing it could look up later: the mode, the node and its directory, the ancestor directories to replay, HEAD when the prompt was sent and HEAD when the parent's turn ended, the workspace plan, the backend, endpoint and model spec, the agent config for sub-agents, the tree handle and the MCP host. `spawn` returns a `TurnHandle` (the live state the window reads, plus a cancel channel) and starts the task.

`Claim` is what the turn needs of the repository: `Exclusive` (a commit turn, or a feedback turn working in place), `Copy` (a feedback turn in its own copy), `Compact` (no tools) or `Copying` (a `Copy` whose copy is still being made — a copy turn is admitted as `Copying`, and `TurnHandle::claim` reports `Copy` once the walk is over, through an atomic that `CopyMade` sets on drop). `admit` (`src/agent/turn/mod.rs`) decides whether it may start beside the running turns: an `Exclusive` claim is admitted beside `Copy` and `Compact` turns, and refused only beside another `Exclusive` turn or while a copy is still being made, since the walk over the repository reads the tree the exclusive turn would write. Any turn is refused while an `Exclusive` turn runs. Copies are capped by `parallel-copies`, a copy still being made taking a slot too. `CopyMade` also sends `AppSignal::TurnCopyMade`, which releases the queue, so a prompt held only for a copy starts as soon as the walk ends rather than waiting for the turn.

Only a commit turn moves HEAD, and only one runs at a time, so a feedback turn — in a copy, in place, or compact — records the HEAD it started from as `head_after`; were it to read HEAD again, a commit landing beside it would be recorded as its own and the follow-up turn would lose the note that the repository moved.
