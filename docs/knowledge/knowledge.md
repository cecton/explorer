---
title: The knowledge base
summary: The repository's knowledge base: the store layout under docs/knowledge/, the entry shape, and the page limits of the tool.
refs: src/knowledge.rs, Store, Entry, spec, run
---

`src/knowledge.rs` is the knowledge base of a repository: short markdown entries about its code, under `docs/knowledge/`, served to the agents through the `knowledge` tool in bounded pages, the way the `conversation` tool serves the tree. Entry `x.md` holds its children in a directory `x/`, so an id is a `/`-joined path and the store is a tree.

The files are the store: nothing is cached between calls, so an entry written by one turn is read by the next. An entry is a `---` metadata block (title, summary, refs) plus a body, and the body is run through `markdown_format` before writing; an entry that does not read back as Explorer writes it is reported as malformed instead of read, so the layout and the formatter stay one fixed point.

A limit per page keeps an agent from reading the whole base: `toc` lists 30 entries, `read` takes 10 ids, `search` returns 20 hits, `check` lists 40 problems.

The tool's full description and the rules it enforces are stated in its own spec (`crate::knowledge::spec`); this entry only says what the store is.
