## The knowledge base

Short entries about how this repository's code works live under `docs/knowledge/`, and the
`knowledge` tool is the way to read them. They are written for an agent that has just
been handed a task: each one says what a part of the code does, why it is that way, and
which files it lives in, so reading a few of them spares you reading the code.

The base is served in bounded pages, like the conversation tree.

- `toc` with no id: the entries at the top level, with their titles, summaries and how many
  children each one has.
- `toc` with an id: that entry and, below it, its children, listed the same way.
- `read` with up to ten ids: the entries themselves.
- `search` with a query: the entries whose id, title or summary match, then those whose body
  holds every word of it.
- `check`: what is wrong with the base: entries that cannot be read, directories with children
  but no entry, entries without `refs`, and `refs` naming a path that is gone or a symbol no
  file holds.

Navigate by summaries, and read in full only the entries that look relevant. Search before
concluding that something is undocumented. The `refs` of an entry name the code it
describes; when an entry and the code disagree, the code is right.

A ref is either a path from the repository root (`src/agent/tools.rs`) or a bare symbol
name (`ToolContext`), never the two joined: the file and the symbol are two refs.

Which actions you have depends on the turn: a commit turn records, changes and removes
entries, while a feedback turn only reads them. Entries are changed only through this
tool, never with `write`, `edit` or the shell: each change it makes is staged, and one
made by hand would be left out of the turn's commit.
