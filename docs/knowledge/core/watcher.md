---
title: Keeping the repository in memory current
summary: The file watcher, the 50 ms debounce, rename, rescan and git-history events, and the two halves that read off-thread and publish on the main thread.
refs: src/watcher.rs, src/loader/watch.rs, read, swap_in
---

`src/watcher.rs` runs a `notify::RecommendedWatcher` on a thread of its own, with r3bl's RRT, and debounces 50 ms before reading anything. It translates what notify reports into created/modified/removed entries: a rename is one removal and one creation, a created directory expands into the files under it, and a create plus a remove in the same window, in either order, collapses into a modification — an atomic replace.

Git's own history files (`.git/HEAD`, `packed-refs`, `refs`) are recognized before the ignore rule, since the ignore rule would otherwise drop them; they do not read a file but raise one `GitHistoryChanged` signal, which is what reloads an open git log. An event-queue overflow sets a rescan, which re-walks the tree and diffs against the in-memory list, and also sets `pending_git`, so a git-history signal goes out with it.

Applying a batch takes two halves. `loader::watch::read` runs on the watcher thread and does all the I/O. `loader::watch::swap_in` runs on the main thread and publishes the result, in one turn with the invalidation of everything computed against the old content — a frame must never paint new content while the offsets naming into it still point at the old one.
