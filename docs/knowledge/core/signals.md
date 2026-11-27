---
title: Threads report through AppSignal
summary: How every other thread reaches the main loop: the AppSignal enum, send versus try_send, and the four timers.
refs: src/signal.rs, AppSignal, send, try_send, src/tui/app/tick.rs, every
---

Every thread that is not the main loop reports to it through `AppSignal` (`src/signal.rs`), a non-exhaustive enum carried on `MainTx`. The watcher, the LSP worker, terminal output, git loads, turn ends, title generation, usage fetches and the timers all use it; nothing touches `AppState` from another thread.

`signal::send` clones the sender and awaits the send in a task of its own, so a caller never blocks on a full channel and a signal is never lost; `signal::try_send` drops the signal instead, for periodic and best-effort ones (`Tick`, `Housekeeping`, `UsagePollDue`, `TurnTick`, `Redraw`). The LSP worker and the watcher call `blocking_send` directly where a dropped result would strand state.

The timers are spawned in `tui::app::tick::every`: 1 s `Tick`, 5 s `Housekeeping`, 60 s `UsagePollDue`, and 100 ms `TurnTick` for the spinner. Each has its own period so that timers firing together cost at most two renders, the first tick is immediate, and a stall costs one late signal rather than a burst of catch-up ones.
