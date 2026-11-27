---
title: Windows, the stack and the tiling
summary: The window stack, the tiling it implies, and the invariant everything rests on: a pane slot is its window's index in the stack.
refs: src/tui/pane_manager.rs, Window, PaneManager, PaneSize, MAX_PANES, relayout, slot_window, slot_box, src/tui/app/render.rs, src/tui/app/input.rs
---

`src/tui/pane_manager.rs` keeps the windows in one stack, most recent first, at index 0, and the layout rests on one invariant: a pane slot is the index of its window in `window_stack`. Every pane draws the window and the box of its own slot, so a slot that was not its index would draw one window into another's box. `layout` keeps it by pushing in stack order, `slot_window` reads the stack (correct before any frame), and `slot_box` reads the layout (only ever asked for during a frame). `a_slot_is_the_index_of_its_window_in_the_stack` pins it over 27 200 cases.

`relayout` is the only writer of the layout, and it runs once per event (`app/input.rs`) and once per frame (`app/render.rs`) — nowhere else.

Every variant of `Window` is unique: at most one `ConversationTree`, one `FilePreview` or `CommitDiff` per file or commit, and the others (terminal, git log, discussion, report) by id. Pickers are modals, so they are not windows.

Explorer tiles the top of the stack by itself. The column count is the surface width divided by `TARGET_PANE_WIDTH` (100), rounded to the nearest, so 100 cells is the ideal pane width rather than a minimum; widths split evenly with the first columns one cell wider. Panes stack top to bottom in a column by their `PaneSize` (`Full`, `Half`, `Third`, `Quarter`), the last pane in a column absorbs the leftover rows, and the layout wraps when less than a quarter of the surface is left. `MAX_PANES` is 16, the most the layout renders at once; the windows that do not fit stay open behind the others.
