---
title: Modals: the pickers
summary: The four pickers, why a modal is not a window, and the rules their code follows: every event, and no matcher per frame.
refs: src/tui/modal/mod.rs, src/tui/modal/list.rs, src/tui/modal/window_picker.rs, src/tui/modal/theme_picker.rs, src/tui/modal/model_picker.rs, src/tui/modal/branch_picker.rs, Modal
---

`src/tui/modal/` holds the four pickers and the frame they share. A modal is not a window — no `Window` variant, no session entry, no place in the window stack: it paints above every pane and takes every event while it is open, so the panes see none.

`Modal::open` records the window that had the keyboard only when no modal was already open, and drops the terminal grab, any drag and the leader state. It reverts an unconfirmed theme preview only when another modal replaces the open theme picker; `close` reverts an unconfirmed theme preview and gives the keyboard back, ungrabbed.

`list.rs` keeps the painters across frames on purpose: one fuzzy picker owns a matcher of about 135 KB, so building one per frame would be a stutter. It also answers which rows are visible, so a long list paints only what is on screen.

The window picker lists the open windows while its query is empty and the repository's files by fuzzy path once something is typed; the match runs off the main thread and an answer carrying an older generation is dropped. The model picker refuses while the window's turn is running, since the turn's model is fixed until it ends, and retries an endpoint whose model list failed.
