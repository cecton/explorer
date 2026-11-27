---
title: The TUI layer
summary: The screen layer: what each part of `src/tui/` is for, how the state is built and torn down, and the rules a change there must respect.
refs: src/tui/mod.rs, src/tui/state.rs, src/tui/pane_manager.rs, src/tui/pane_component.rs, src/tui/panes/mod.rs, src/tui/modal/mod.rs, src/tui/widgets/mod.rs, src/tui/session.rs
---

`src/tui/` is everything on screen, built on r3bl's `TerminalWindow` and an `AppMain` that receives key, mouse and `AppSignal` events and paints one frame. Its parts: `app/` (the frame loop), `pane_manager.rs` (the window stack and the tiling), `pane_component.rs` (one dispatcher per slot), `panes/` (a component per kind of window), `modal/` (the pickers), `widgets/` (drawing primitives that may name nothing above them), `state.rs` (`AppState`, the only place UI state lives), `session.rs` (what is saved per repository), `command_line.rs`, `keymap.rs`, `theme.rs`, `selection.rs`, `symbols.rs`, `focus.rs` and `after_agents.rs`.

The rules AGENTS.md states are what a change here must respect: UI state lives in `AppState` and never in a component, a widget may import only other widgets, `crate::tui::theme` and `util`, and a modal is not a window — no `Window` variant, no session entry, no place in the focus cycle.

`main::run` builds the `AppState` before the loop starts: the theme comes from `--theme`, then the config, then the first bundled name, and the saved session's theme replaces the starting theme unless `--theme` was given; the keymap overrides are applied and a bad one fails startup; the saved session is applied and each restored symbol highlight is asked of rust-analyzer again. `tui::app::run` then cancels the running turns when the loop ends, hangs up the terminals, and saves the session one last time.
