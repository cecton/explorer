---
title: The repository in memory
summary: The append-only file list behind the whole app: why a `FileKey` never moves, what counts as a text file, and how content is read.
refs: src/loader.rs, FileKey, LoadedFile, load_repo, FileData
---

The whole repository is held in memory as an append-only `Vec<Arc<LoadedFile>>`, behind an `arc_swap::ArcSwap`. The watcher publishes into it, and `src/tui/session.rs` also stores stubs for missing paths before the watcher is subscribed. A `FileKey(usize)` is an index into that vector and stays valid for the life of the `AppState`: files are never removed or reordered, only flagged `removed`. Preview, search and the symbol groups name files by key, which is why the vector may not be rebuilt.

`LoadedFile` holds the content behind an `RwLock` along with its line offsets and the colored spans rust-analyzer returned for it. A file counts as text when the whole file is valid UTF-8 (the first 1024 bytes are sniffed first as a cheap check); a NUL does not disqualify it, since a Rust test fixture legitimately holds one. The walk uses `jwalk`, prunes `target` and `.git`, and `find_git_root` only requires that `.git` exists, so a linked worktree (where it is a file) works.

`FileData::extract_text` clamps onto character boundaries: a stale selection or a symbol range from before an edit can land inside a character, and slicing there would panic. `line_bounds` clamps the line index, and `word_bounds` is delegated to `util::text`.
