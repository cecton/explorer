---
title: The windows: one component per kind
summary: One component per kind of window, and the dispatcher that picks the one a slot holds.
refs: src/tui/panes/mod.rs, src/tui/panes/preview.rs, src/tui/panes/git_log_pane.rs, src/tui/panes/commit_diff_pane.rs, src/tui/panes/report_pane.rs, src/tui/pane_component.rs
---

`src/tui/panes/` holds one component per kind of window: `preview.rs` (a file), `terminal/` (a PTY), `git_log_pane.rs`, `commit_diff_pane.rs`, `report_pane.rs`, `conversation_tree/` (the tree of conversations) and `discussion/` (one node of it).

Each is an r3bl component that only renders and handles events; what it shows lives in `AppState`, keyed by the window. `pane_component.rs` owns one of each and hands an event to the one its slot currently holds, so a slot can switch between windows without losing their state.
