---
title: Focus and the terminal grab rule
summary: Who has the keyboard: the grab rule for terminals, why focus arrival never grabs, and what closing a window tears down.
refs: src/tui/focus.rs, focus_window, refocus, close_window, close_terminal
---

`src/tui/focus.rs` decides which window has the keyboard, and it is where the grab rule lives. `focus_window` brings a window to the front, notifies terminals that they lost or gained focus, and grabs a terminal unless it is scrolled back. A terminal is also grabbed by pointing at it with the mouse, `Enter` or `q` in a scrolled-back one, the leader key then the grab action, the window picker, scrolling back to the bottom, and when it was just spawned.

`refocus` is different on purpose: it is the focus half of a stack change the caller already made, and in it the keyboard is only ever given back, never taken. So Tab can land on a terminal and Tab again leave it, and one terminal cannot inherit another's grab.

`close_window` delegates to `close_terminal` for a terminal, which does the full teardown: a terminal spawned over a file preview gives the preview back, the focus escape sequence goes out through the PTY before the pane is dropped, and a background close leaves the focused terminal exactly as it was.
