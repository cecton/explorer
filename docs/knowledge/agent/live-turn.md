---
title: A turn in flight: live state, recorder, injector
summary: What a running turn shows and records: the live state, the recorder that writes the node, and the injector that steers it.
refs: src/agent/turn/live.rs, LiveTurn, Recorder, Injector
---

`src/agent/turn/live.rs` is what a running turn shows and records while it runs. `LiveTurn` is the state a Discussion window reads at render time: the reply as it streams, the work log, the thinking, the counters, the model, the injected messages still to send. `tokens_in` is the latest request's, `cached_in` is set once, from the first request that reports it. A process-wide `REVISION` counter moves with every change, so a window re-reads texts only when it moved, and so a resumed turn can never reuse a number.

`Recorder` writes the node. It appends to the transcript, writes `reply.md` at most once a second while text streams (and also when an answer ends having called tools, and when the turn finishes), moves an answer that turned out to call tools into the activity log instead of the reply, and remembers the partial answer so a replay continues from where the model stopped. Thinking is shown but never recorded, and an answer's thinking is dropped when it calls tools.

`Injector` carries a message the user sends while the turn works: it queues it, gives it to the loop at its next round, and `Injector::sent` appends it to the prompt, to `prompt.md` and to the transcript — so the prompt is the whole conversation the turn was given, not just what was typed. It is the second writer of the node besides the `Recorder`.
