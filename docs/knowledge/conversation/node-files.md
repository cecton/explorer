---
title: The files of a node
summary: The files of one node, what each is for, and the rule that what a model sees is always in the transcript.
refs: src/conversation/files.rs, PROMPT, TRANSCRIPT, read_input
---

A node's directory holds a few files, and `src/conversation/files.rs` names them: `prompt.md` (the prompt as sent, after a slash command expanded it), `input.md` (the prompt as typed, kept only when it differs), `reply.md` (the answer, only for a turn that called no tools), `activity.md` (one line per tool call, plus the text of answers that called tools; display only), `transcript.jsonl` (the messages a child replays), `metadata.md`, and `title.md` / `summary.md` from the summary model. `processes/` holds the output logs of the node's background commands and is never a node; the stale `agents/` directory, from when sub-agent runs were recorded on disk, is skipped by the walk for the same reason.

The split between `prompt.md` and `input.md` is what lets a node be edited: the window gives back what was typed, while what the model saw stays.

`activity.md` and `input.md` are display only and never sent to a model; everything sent to a model must also be in `transcript.jsonl`. That is the one invariant of these files, and it is what makes replaying an ancestor faithful.
