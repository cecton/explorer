---
title: Focus and the terminal grab rule
summary: Who has the keyboard: the grab rule for terminals, why focus arrival never grabs, and what closing a window tears down.
refs: src/tui/focus.rs, focus_window, refocus, close_window, close_terminal, focus_along, push_window, replace_window
---

`src/tui/focus.rs` decides which window has the keyboard, and it is where the grab rule lives. `focus_window` brings a window to the front of the **view** (`PaneManager::push_window` inserts at `view_start`, so the view does not scroll), notifies terminals that they lost or gained focus, and grabs a terminal unless it is scrolled back. A terminal is also grabbed by pointing at it with the mouse, `Enter` or `q` in a scrolled-back one, the leader key then the grab action, the window picker, scrolling back to the bottom, and when it was just spawned.

`refocus` is different on purpose: it is the focus half of a stack change the caller already made, and in it the keyboard is only ever given back, never taken. So Tab can land on a terminal and Tab again leave it, and one terminal cannot inherit another's grab. `Tab`/`Shift+Tab` reach every window of the stack, on screen or not, and stop at either end; the view scrolls to show where focus landed (`focus_along`), and the frame never moves focus by itself — a window the view hides is one Tab can reach.

`close_window` delegates to `close_terminal` for a terminal, which does the full teardown: a terminal spawned over a file preview gives the preview back (`replace_window`, which keeps the preview's place in the stack), the focus escape sequence goes out through the PTY before the pane is dropped, and a background close leaves the focused terminal exactly as it was. Closing the focused window refocuses the window that took its place, not the front of the stack, which can be scrolled out of view.
