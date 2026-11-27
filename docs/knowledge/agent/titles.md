---
title: Titles and summaries
summary: How a node gets its title and summary, what the summary model is shown, and how a node with no title is repaired.
refs: src/agent/turn/title.rs, retitle, TITLE_MAX_CHARS, src/tui/panes/conversation_tree/mod.rs, needs_retitle
---

`src/agent/turn/title.rs` gives a finished node its title and summary with one commit-message-style call to the summary model: it sends the prompt, the end of the reply (`REPLY_MAX_CHARS` of it, because the end is where the verdict is) and the commits made, and splits the answer into a first line (the title, capped and made single-line) and the rest (the summary). The title is written to `title.md` and sent to the tree pane as `NodeTitleReady`. Without a `summary-model` no title or summary is generated, and the node shows the first line of its prompt.

The same call repairs a node that has no title: the Conversation Tree window asks for one when the cursor rests on a finished node it finds undescribed, with a reply to describe and no retitle already running; the title is generated from what is on disk. A running node is never retitled.

The call has no tools and no history, and nothing of it is recorded in a transcript; the root node id is passed as the request's session key.
