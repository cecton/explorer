---
title: The session, saved per repository
summary: What is saved per repository, where, when it is written, and the two things that are deliberately not saved.
refs: src/tui/session.rs, Session, PaneEntry, load_session, src/tui/app/tick.rs, maybe_save_session
---

The session is what makes the next start continuous: the window stack with each window's size and what it shows, the file highlights, the symbol highlight identities, the chosen cargo features, the theme, the conversation tree's expanded forks and filter, the highlights in prompts and replies, the repository's MCP servers, its aliases, and whether the user trusted it. It is saved per repository as `<data_dir>/sessions/<16-hex repo hash>.json`, written atomically.

Saving is synchronous on the main thread and is the 5 s `Housekeeping` pass's only debounce; it also happens on exit. A theme being previewed in the picker is not saved: only `saved_theme` is persisted, so Esc leaves the previous one.

Two things are deliberately left out. Symbol highlight locations would be stale the moment a file changed, so only the origin identity (path, line, column, word) is saved and the locations are asked of rust-analyzer again at startup. Report windows and exited terminals are not saved at all, and an editor terminal is saved as the file preview it replaced.
