---
title: The app loop, file by file
summary: The frame loop by file, and which signals repaint: the spinner only over a running turn the tree or a Discussion shows.
refs: src/tui/app/mod.rs, src/tui/app/init.rs, src/tui/app/input.rs, src/tui/app/signals.rs, tick_repaints, src/tui/app/render.rs, src/tui/app/status_bar.rs, src/tui/app/tick.rs, AppMain
---

`src/tui/app/` is the frame loop, split by what each kind of event is:

`init.rs` registers one `PaneComponent` per slot, publishes the main-thread sender, starts the LSP worker, the MCP servers, the file watcher and the four timers, then restores terminals, git logs and commit diffs, loads the conversation tree off the main thread, and fetches the model list of every endpoint. `input.rs` routes one event: an open modal first, then the after-agents cancel, the command line, the leader key, global shortcuts, focus-follows-mouse, and finally the focused component. `signals.rs` is the `AppSignal` match — the one place where what another thread reported becomes a change in `AppState`. `render.rs` drains the picker generation and terminal exits, relayouts, paints the panes, then the command line or the status bar, then the modal. `status_bar.rs` paints the one bottom row: the hints of what has focus on the left, and on the right the dirty flag, the after-agents countdown, the usage of the focused endpoint (warning at 75 %, error at 90 %) and the clock. `tick.rs` holds the timers, the clock minute and `maybe_save_session`.

Which signals repaint is a rule, not an accident. `Tick` (1 s) repaints for the after-agents countdown, a changed clock minute, or a terminal with output nothing announced; `TurnTick` (100 ms) advances the spinner on every tick but repaints only while a turn runs **and** the Conversation Tree or a Discussion is visible, since those are the only things that animate; `Housekeeping` (5 s) never repaints — its results arrive as their own signals.

`AppMain` owns what is not UI state: the file `ArcSwap`, the window-picker channels, the LSP and watcher guards and the cached modal painters.
