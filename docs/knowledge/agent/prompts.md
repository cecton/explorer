---
title: Prompts, notes and replay
summary: What a model is told: the compiled prompts, the system prompt's composition, the notes a turn carries and the transcript replay.
refs: src/agent/context.rs, system_prompt_with, claude_system_prompt, subagent_system_prompt, environment_note, replay, Notes, prompts/feedback.md, prompts/commit.md
---

`src/agent/context.rs` builds everything a model is told, and the prompts themselves are `prompts/*.md`, compiled into the binary with `include_str!`, so editing one changes agent behavior and is not a documentation change.

The Explorer backend's system prompt (`system_prompt_with`) is the mode's prompt (`feedback.md` or `commit.md`, absent when the command sets `system-prompt: off`), the conversation-tool and knowledge-tool prompts, `AGENTS.md` under a project-instructions heading, and the user's own fragments last so they win on a conflict. The Claude Code backend builds a shorter one in `claude_system_prompt`, without `AGENTS.md`, and sub-agents use `subagent_system_prompt`. None holds anything that changes between turns — no HEAD, no date — because HEAD moves on every commit and anything changing ahead of the replayed ancestors throws away the endpoint's prefix cache. That state travels in the first message instead, as an environment note.

The rest of the file is the notes a turn carries: the replay of the ancestors' transcripts (oldest first, with missing tool results repaired so the sequence stays valid), a handover preamble for a fresh session that cannot replay, the feedback note that tells the agent what will be discarded, a repository-moved note when HEAD changed under it, the compaction note, and the compact, continue and update requests.
