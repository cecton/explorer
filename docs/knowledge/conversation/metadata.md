---
title: A node's metadata
summary: A node's metadata.md: the type, the status, the commits, the endpoint session, and the outcome a closed turn records.
refs: src/conversation/metadata.rs, Metadata, TurnOutcome, cut_short_outcome
---

`src/conversation/metadata.rs` reads and writes `metadata.md`, a flat `key: value` block: the node's `NodeType` (`feedback`, `commit`, `compact`), its `NodeStatus` (`running`, `done`, `failed`, `cancelled`), the commits it produced, the endpoint's `session_id`, and the `TurnOutcome` a closed turn records (what it left uncommitted, whether it ran in place and why).

A node still marked `running` when the tree is loaded is rewritten as failed and interrupted: a turn does not survive the process that ran it, so a `running` marker on disk is always a crash.

`cut_short_outcome` decides how a turn cut short by the user is recorded: with a reply or commits it stays `cancelled`; with neither it is recorded as `failed` with the error "cancelled by the user", so the node reads as stopped rather than empty.
