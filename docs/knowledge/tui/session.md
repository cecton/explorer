---
title: The session, saved per repository
summary: What is saved per repository, where, when it is written, and the two things that are deliberately not saved.
refs: src/tui/session.rs, Session, PaneEntry, PaneRecord, load_session, src/tui/app/tick.rs, maybe_save_session, restore_view_start
---

The session is what makes the next start continuous: the window stack with each window's size and what it shows, where the view started (see `tui/windows`), the file highlights, the symbol highlight identities, the chosen cargo features, the theme, the conversation tree's expanded forks and filter, the highlights in prompts and replies, the repository's MCP servers, its aliases, and whether the user trusted it. It is saved per repository as `<data_dir>/sessions/<16-hex repo hash>.json`, written atomically.

Saving is synchronous on the main thread and is the 5 s `Housekeeping` pass's only debounce; it also happens on exit. A theme being previewed in the picker is not saved: only `saved_theme` is persisted, so Esc leaves the previous one.

`view_start` is saved as the `PaneEntry` of the window the view started at, not as an index. Restore looks that entry up among the windows that came back, so a window that failed to restore (a deleted file, an exited terminal) does not shift the view onto the wrong one — and a surface that packs the stack into other columns still starts the view at the same window. `PaneRecord`s are saved in stack order, so a restored stack keeps the saved shape.

Two things are deliberately left out. Symbol highlight locations would be stale the moment a file changed, so only the origin identity (path, line, column, word) is saved and the locations are asked of rust-analyzer again at startup. Report windows and exited terminals are not saved at all, and an editor terminal is saved as the file preview it replaced.
