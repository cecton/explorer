---
title: The Conversation Tree window
summary: The tree window: how rows are laid out and filtered, the keys it takes, and what the component does on its own.
refs: src/tui/panes/conversation_tree/mod.rs, src/tui/panes/conversation_tree/layout.rs, src/tui/panes/conversation_tree/filter.rs, src/tui/panes/conversation_tree/input.rs, src/tui/panes/conversation_tree/render.rs
---

`src/tui/panes/conversation_tree/` is the Conversation Tree window: every conversation of the repository, drawn as a tree with rails, folds and branches. `layout.rs` turns the tree and the set of expanded forks into rows; `filter.rs` keeps the nodes whose title or summary matches a query and reports the byte ranges that matched, so the renderer can highlight them; `input.rs` is the navigation (arrows, page keys, `/` to filter, `n` for a new root, `m` for another model, `e` to edit, `c` for commits, `f` for a favorite, `d d` to delete, double-click to open); `render.rs` paints the rows and the queue of waiting turns.

`mod.rs` is the component: it retitles a node the cursor rests on only when it is untitled, finished, has a reply to describe and has no retitle already running (a running turn is never retitled), reloads when the tree changes, and drops the state of nodes that vanished. Its title carries `(untrusted)` while the repository is not trusted, since no turn runs before it is.
