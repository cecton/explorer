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

Navigate by summaries, and read in full only the entries that look relevant. Search before
concluding that something is undocumented. The `refs` of an entry name the code it
describes; when an entry and the code disagree, the code is right.
