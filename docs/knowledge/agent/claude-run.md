---
title: A Claude Code run, start to finish
summary: One Claude Code run: the steering task on standard input, the side tasks that keep a run open, and how a run ends, errors or loses its session.
refs: src/agent/claude_code.rs, run, Run, Settling, parse_line, StreamEvent, src/agent/turn/live.rs, Injector
---

A Claude Code run is `run` in `src/agent/claude_code.rs`: the CLI is spawned in its own process group, its `stream-json` output is read line by line, and each line becomes a `StreamEvent` through `parse_line`. `Run` holds what every run of one turn shares (the sandbox, the root, the model); `Run::from` and `Run::resume` are the two ways in.

Standard input stays open while the run streams, because steering needs it: a task polls the `Injector` every 250 ms, writes each queued message as an `input_frame`, and calls `injector.wrote` only once the write succeeded, so a failed write leaves the message for the window to give back. The run ends when that task closes stdin, which it does only when `close_if_idle` sees no queued message and `settled`.

`settled` is not "a `result` line arrived". A `task_started` line adds its `task_id` to `Settling`, a `task_notification` removes it, and a `result` settles the run only when nothing is outstanding — Claude Code starts another model turn after a background task reports in. `task_updated` and `background_tasks_changed` are ignored. `Injector::open` runs at the start of every run, since a commit nudge or a retry after a lost session reuses a recorder the last run closed.

A resume that ends with a result carrying an error and `no_turn` (the CLI's `num_turns` of 0) is `SessionGone`; a fresh run never reports it. Without a result at all, the first non-empty stderr line becomes the error, since that is the only place the CLI explains itself.
