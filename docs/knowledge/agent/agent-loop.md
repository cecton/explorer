---
title: The agent loop
summary: One run of the own loop: rounds, tool execution, the stall detector, the time-budget nudges and the correction notes.
refs: src/agent/agent_loop.rs, run, Limits, StallDetector, time_nudge
---

`src/agent/agent_loop.rs` is one run of the own loop: it sends the messages, streams the answer, runs the tool calls, records everything, and starts the next round until an answer asks for no tool and nothing joined while it streamed.

The loop also keeps a run honest. A stall detector keys a call by its name and arguments, hashes only its result and, when the same call repeats `repeat-limit` times, first nudges the model and then fails the run; the arguments are quoted as the model sent them, not as it meant them. A run given a time budget gets a soft nudge once as the budget nears and a firm one every round past it, and a run that answers in Chinese gets a correction note placed after its tool results, never between a call and its result.

A nameless tool call is dropped and noted, up to three rounds, because replaying it in the transcript makes the next request invalid. A failed request flushes the partial answer first, so a later `/continue` shows the model where it stopped.
