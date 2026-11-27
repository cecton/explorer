---
title: AppState: the one place UI state lives
summary: AppState, the single place UI state lives: what it holds by area and the invariants (append-only keys, revision-bumped texts) a change must keep.
refs: src/tui/state.rs, AppState
---

`src/tui/state.rs` holds every piece of UI state in one `AppState`: the repository and its `ArcSwap` of files, the pane manager and focus, the theme and keymap, one map per kind of window (previews by `FileKey`, terminals, git logs, commit diffs, reports, discussions), the modal and its pickers, the conversation tree with its view state and running turns, the agent config and MCP servers, aliases, trust, the after-agents command, and the dirty flag.

It is one struct because the rules that span panes are the interesting ones — a highlight survives a preview closing, a theme preview is not saved, a project alias wins over a config one — and each is a field here rather than a copy in a component.

Two facts are worth knowing before adding to it. `DiscussionState::mirror` appends only what is new, because the texts grow and work-log entries are never rewritten; `content_revision` is bumped whenever a text is replaced as a whole, which no length tells apart. And a `FileKey` is only valid because the file vector is append-only.

A session is applied only to a fresh `AppState`: `apply` does not clear what is already there.
