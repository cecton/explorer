---
title: What a finished turn leaves behind
summary: What a turn does to the working tree when it ends, per mode, and the special close of an update turn.
refs: src/agent/turn/close.rs, close, commit_staged_knowledge, src/markdown_format.rs, format_committed
---

When a turn ends, `src/agent/turn/close.rs` decides what survives. A commit turn keeps its commits and loses what it left uncommitted; a feedback turn loses both. A feedback turn that worked in a copy of the repository leaves the repository exactly as it was — the copy is what goes — and it reports whether it left uncommitted work there, because the agent was told not to clean up after itself.

Before `close::close` runs, `spawn` (in `src/agent/turn/mod.rs`) finishes the turn's commits. A turn marked `update` has `commit_staged_knowledge` (defined in `close.rs`) run `git diff --cached` and `git commit` for what the knowledge tool staged under `docs/knowledge/`, as `docs(knowledge): update` with the added, changed and removed entries in the message; HEAD is read afterwards, so that commit counts as the turn's own. It is the update turn's agent that never runs git. If this commit fails, a turn otherwise done is marked `Failed` with the reason; one that had already failed keeps its own reason and only logs this one.

A commit turn is also formatted: `markdown_format::format_committed` formats the markdown the turn committed and re-amends the commit, so HEAD keeps one commit per change. It does nothing when the tree is dirty or HEAD did not move, and a failure is logged rather than failing an otherwise finished turn.
