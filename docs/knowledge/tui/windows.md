---
title: Windows, the stack and the tiling
summary: The window stack and the tiling it implies: unique windows, columns at 100 cells, the four pane sizes and the 16-slot limit.
refs: src/tui/pane_manager.rs, Window, PaneManager, PaneSize, MAX_PANES
---

Everything is a window, and `src/tui/pane_manager.rs` keeps them in one stack, most recent first, at index 0. Every variant of `Window` is unique: at most one `ConversationTree`, one `FilePreview` or `CommitDiff` per file or commit, and the others (terminal, git log, discussion, report) by id. Pickers are modals, so they are not windows.

Explorer tiles the top of the stack by itself. The column count is the surface width divided by `TARGET_PANE_WIDTH` (100), rounded to the nearest, so 100 cells is the ideal pane width rather than a minimum; widths split evenly with the first columns one cell wider. Panes stack top to bottom in a column by their `PaneSize` (`Full`, `Half`, `Third`, `Quarter`), the last pane in a column absorbs the leftover rows, and the layout wraps when less than a quarter of the surface is left. `MAX_PANES` is 16, the most the layout renders at once; the windows that do not fit stay open behind the others.

`slot_window` reads the stack and `slot_box` reads the last layout: before the first frame, the stack is the only truth.
