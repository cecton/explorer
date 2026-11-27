---
title: What a turn replays of its branch
summary: What a turn replays of the branch above it: the cut at the nearest compact node, the parent's session and HEAD, and the repair of a half-finished transcript.
refs: src/conversation/mod.rs, turn_context, src/agent/context.rs, replay, compaction_note
---

A turn does not replay the whole branch: `ConversationTree::turn_context` walks the lineage and keeps only the ancestors after the nearest compact node. A `/compact` node is a summary standing in for everything before it, so the turns below it replay the summary and what came after, and the earlier messages are never sent again.

The context also carries what a turn needs to know about its parent: the parent's HEAD (to notice the repository moved), the parent's endpoint session (carried over only when the parent ran on a `claude-code` backend and is not a compact node, to resume it instead of starting over), and the parent's own directory when it was compact.

`src/agent/context.rs` then turns those directories into messages, oldest first, preferring each node's transcript and falling back to its `prompt.md` and `reply.md`; a transcript that ends inside a round gets a note for every tool call whose result never arrived, so the replay is a valid sequence. A compact turn's summary goes in as a note that says the earlier messages were replaced; the note goes only to the first turn directly below a compact node.
