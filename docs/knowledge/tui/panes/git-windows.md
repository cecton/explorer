---
title: The git log and the commit diff
summary: The git log and the commit diff windows: the first-parent history, the filter that scores once and highlights per row, and when they reload.
refs: src/tui/panes/git_log_pane.rs, src/tui/panes/commit_diff_pane.rs, GitLogPaneComponent, CommitDiffPaneComponent, LogSource, src/util/fuzzy.rs, Kind
---

`src/tui/panes/git_log_pane.rs` shows the first-parent history of HEAD, a branch or tag, or a given commit (`LogSource` is `Head`, `Branch` or `Commit`), loaded with `git::load_log`. `/` filters it live by fuzzy match and highlights what matched in the rows on screen — only those, since a filter of a 10 000-commit history repaints on every keystroke. Enter opens the commit's diff, which `commit_diff_pane.rs` shows as a unified diff colored by `DiffKind`, from the theme's diff scopes.

The filter keeps the entries' indices rather than a copy of each match: the whole list is scored once per keystroke, and a frame then asks for the char indices of the rows it paints only. `Kind::Plain` is what it ranks with, which is why a commit summary matches the way it always did.

`b` opens the branch picker, and several logs can be open at once; each remembers its source, its filter and its scroll position in the session.

The log knows when it is stale: every log reloads when any agent turn ends, and when `.git/HEAD` or a ref changes on disk, without the user asking. When it is showing a truncated history it says so in the title.
