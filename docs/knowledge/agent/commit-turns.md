---
title: How a commit turn is made to commit
summary: What makes a commit turn end in a commit: the review rounds the prompt names, and the hidden nudge that quotes git status at most twice.
refs: prompts/commit.md, src/agent/context.rs, next_commit_nudge, commit_nudge, mode_prompt, src/agent/mod.rs, src/agent/turn/mod.rs, src/agent/turn/close.rs, src/agent/driver.rs
---

A commit turn is held to two rules the code enforces after the agent has spoken, so neither depends on the model remembering them.

The first is the review: `prompts/commit.md` makes the agent review its own commits, at most `review-rounds` times per commit (3 by default, at least 1), and report each commit's short hash and title, the number of review rounds and what they changed. The number is substituted by `context::mode_prompt`, reached through `system_prompt_with` and `claude_system_prompt`, so it comes from the config and not from the model's judgment.

The second is the commit itself. When the turn stops while the working tree is still dirty, `context::next_commit_nudge` builds a hidden message (`injected: true`, never shown in a window) that quotes `git status --short` and says to commit; it is sent at most `MAX_COMMIT_NUDGES` (2) times, through the own loop's `Settle` and the Claude backend's resumed session, and after that the close discards what is left. The nudge applies only to commit turns that are neither compact nor knowledge updates, and is skipped when a user message is queued. This is the one place both backends close a commit turn through, which is why the nudge lives in `context.rs` rather than in either of them.
