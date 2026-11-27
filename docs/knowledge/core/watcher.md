---
title: Keeping the repository in memory current
summary: The file watcher and its debounce, the two halves that read off-thread and publish on the main thread, and how two overlapping batches stay correct.
refs: src/watcher.rs, src/loader/watch.rs, read, swap_in, resolve_key, src/lsp/mod.rs, send_file_request
---

`src/watcher.rs` runs a `notify::RecommendedWatcher` on a thread of its own, with r3bl's RRT, and debounces 50 ms before reading anything. It translates what notify reports into created/modified/removed entries: a rename is one removal and one creation, a created directory expands into the files under it, and a create plus a remove in the same window, in either order, collapses into a modification — an atomic replace.

Git's own history files (`.git/HEAD`, `packed-refs`, `refs`) are recognized before the ignore rule, since the ignore rule would otherwise drop them; they do not read a file but raise one `GitHistoryChanged` signal, which is what reloads an open git log. An event-queue overflow sets a rescan, which re-walks the tree and diffs against the in-memory list, and also sets `pending_git`, so a git-history signal goes out with it.

Applying a batch takes two halves. `loader::watch::read` runs on the watcher thread and does all the I/O. `loader::watch::swap_in` runs on the main thread and publishes the result, in one turn with the invalidation of everything computed against the old content — a frame must never paint new content while the offsets naming into it still point at the old one.

The two halves exist for a second reason: a batch can be read while an earlier one waits to be published, and then the list grows under it. So the `FileKey`s `read` found are only a hint. It carries each path and the list's length at the read (`read_len`), and `swap_in` resolves every path again through `resolve_key` — the read's key when there was one, since the list is append-only, else the first match among the files appended since `read_len`. Without that, an overlapping batch loses an edit, leaves a deleted file behind or appends one path twice.

`loader/watch.rs` is also the one place outside the UI that asks the LSP for colors: each key it touches gets an `lsp::send_file_request`.
