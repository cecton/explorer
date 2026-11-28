---
title: Windows, the stack and the tiling
summary: The window stack, the scrolling view over it, and the invariant everything rests on: a pane slot is its index in the view.
refs: src/tui/pane_manager.rs, Window, PaneManager, PaneSize, MAX_PANES, relayout, slot_window, slot_box, cycle_focus, focused_slot, restore_view_start, src/tui/app/render.rs, src/tui/app/input.rs
---

`src/tui/pane_manager.rs` keeps the windows in one stack and a `view_start`: the stack index of the window at the top of the leftmost visible column, so the view scrolls over the stack by columns. The invariant: a pane slot is its index **in the view**, so slot `s` holds `window_stack[view_start + s]`, because every pane draws the box of its own slot. `slot_window` reads the stack (correct before any frame), `slot_box` reads the layout (only asked during a frame).

`relayout` is the only writer of the layout: once per event (`app/input.rs`) and once per frame (`app/render.rs`). It reveals the focused window first, scrolling the view back onto it if a resize or a pane moved off left it hidden. The render pass used to move focus into the frame instead; it no longer does, because a hidden window is one `Tab` can reach.

`cycle_focus` walks the whole stack, wrapping at both ends, and calls `reveal` to scroll by whole columns onto wherever focus landed: past the right edge, its column goes to the right edge; past the left, to the left. `clamp_view_start` snaps the view to a column start. `push_window` inserts at `view_start`, so a picked or new window comes to the front of the view while the view holds still. `remove_window` refocuses the window that took the closed one's place, since the front can be scrolled away.

The session saves and restores the view through `view_start`/`restore_view_start` (see `tui/session`), which names the window rather than its index.

Every variant of `Window` is unique: one `ConversationTree`, one `FilePreview` or `CommitDiff` per key, the others by id.

Explorer tiles with columns: the surface width over `TARGET_PANE_WIDTH` (100), rounded to the nearest, split evenly. Panes stack by `PaneSize` (`Full`, `Half`, `Third`, `Quarter`), the last absorbing the leftover rows. `MAX_PANES` is 16, drawn whole columns only.
