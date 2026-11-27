---
title: The conversation tree
summary: The conversation tree on disk: one directory per node, outside the repository, and what the in-memory tree keeps of it.
refs: src/conversation/mod.rs, ConversationTree, NodeId, conversations_dir
---

The conversation tree is a directory tree of markdown files, kept outside the repository, at `<data_dir>/conversations/<repo-hash>/`. One directory per node, named by its creation stamp (`2026-10-05T14-32-07Z`, uniquified with `-2`, `-3` across the tree), and its children are subdirectories. There is no index file: the directory _is_ the tree, so it can be read, searched and backed up without Explorer.

That is deliberate, and it costs one thing: the tree is loaded at startup and every change is made in memory and on disk at once. `ConversationTree` keeps the metadata, titles and summaries in memory and leaves the prompts and replies on disk, read when a window shows them.

`revision()` is a process-wide counter that moves with every change, so whatever caches something about the tree can tell when it is stale. `create_node` refuses while the tree has not loaded yet — a node created then would land in the working directory instead of the store.
