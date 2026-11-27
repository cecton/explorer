---
title: The terminal window
summary: The terminal window: the PTY and scrollback, the grab rule with the mouse, how a terminal is started or restored, and what an exit does.
refs: src/tui/panes/terminal/mod.rs, src/tui/panes/terminal/spawn.rs, TerminalPaneComponent, Request
---

`src/tui/panes/terminal/mod.rs` is a terminal window: an `OfsBufVT100` fed by a real PTY, with a scrollback and mouse selection. Keys go to the shell while the terminal is grabbed; wheel-up into the primary screen's scrollback releases the grab, and the ways to take it back are listed in tui/focus. A grabbed terminal whose program asked for mouse tracking keeps the mouse for the program, so no selection is taken; an ungrabbed one selects even then.

`spawn.rs` starts one. A `Request` says what for: a command (`sh -c`), an interactive shell, a saved terminal restored from the session, or a terminal replacing a file preview (the `$EDITOR` window). A spawned task reads the PTY events and feeds the buffer directly, and a flood gate throttles the repaint signals, since a command that prints megabytes must not paint a frame per chunk.

Processes do not survive quitting: only exit events reach the app, through `drain_exits` on every frame; an empty screen closes straight away, and an occupied one stays open showing that it exited, with its status.
