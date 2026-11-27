---
title: Reading and rewriting the repository's history
summary: The main module for git: gix for in-memory reads, `git` itself for writes and file listings, and what each function is for.
refs: src/git.rs, load_log, LogEntry, amend_all, discard_worktree, commits_since
---

`src/git.rs` is the main place that talks to git; a few other modules do their own (`src/knowledge.rs` runs `git grep`, and `src/agent/workspace.rs`, `src/agent/turn/pin.rs` and `src/agent/subagent.rs` open the repository themselves). Reads that fit in memory (log, refs, dirty check, commit diff) go through `gix` on a `ThreadSafeRepository` the UI opens at startup (`subagent.rs` opens another); anything that writes, or that needs git's own file listing, shells out through `git_output`. The split exists because `gix` gives a fast in-memory walk while `add`, `reset`, `clean` and the pathspec rules are easier to get right by asking git itself.

The log is first-parent history of HEAD or of a chosen ref, capped at `MAX_LOG_ENTRIES` (10 000, with a `truncated` flag the pane shows); an unborn HEAD gives an empty history. `LogEntry` carries the date preformatted because the log pane redraws every visible row on every frame. A dangling symref is dropped rather than failing the whole log.

The writing half exists for the agent: `amend_all` re-amends the last commit so a commit turn can format the files it just committed, `discard_worktree` resets and cleans what a turn left behind, and `commits_since` lists what a commit turn produced, falling back to the single commit it knows rather than losing it.
