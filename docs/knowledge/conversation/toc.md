---
title: Paging the tree for the agent
summary: The bounded pages the `conversation` tool serves: roots, the path to a node with its line and forks, and the commit lookup.
refs: src/conversation/toc.rs, toc_roots, toc_node, src/agent/tools.rs
---

`src/conversation/toc.rs` is the paging behind the agent's `conversation` tool: the whole tree is too large to hand over at once, so every page is bounded and names the ids of the pages and nodes around it. `toc_roots` lists the root conversations newest first, `TOC_ROOTS_PER_PAGE` (30) at a time. `toc_node` lists the path from the root down to the node, the straight line below it up to `TOC_SEGMENT_MAX` (20), and then the forks where the line splits. `node_of_commit` finds the node that produced a commit, from a hash or a prefix of seven or more.

A line marks the asking turn with `<- this turn` and its own branch with `<- your branch`, so an agent sees where it stands in the tree it is reading.

The tool's description and schema live with the other agent tools in `src/agent/tools.rs`; the page shapes are here, next to the tree they walk.
