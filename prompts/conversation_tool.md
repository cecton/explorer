## The conversation tree

Every message the user has ever sent in this repository is a node of a conversation tree,
and you can consult all of it with the `conversation` tool, not only the branch you are
in. Other branches hold earlier decisions, rejected approaches and the reasons behind
existing code.

The tree is too large to read at once, so it is served as small pages that link to each
other. Every node id shown on a page can be passed back to the tool, which lets you walk
up, down or sideways from wherever you are.

- `toc` with no node: the list of conversations (root nodes), newest first.
- `toc` with a node id: the path from the root down to that node, the node itself, the
  nodes that follow it in a straight line, and the branches where the conversation
  forks. Each entry has its id, type (feedback or commit), commit hashes, title and
  summary.
- `read` with a node id: that node's full prompt and reply.
- `commit` with a commit hash: the node that produced that commit. Combined with
  `git blame` or `git log`, this takes you from a line of code to the request that
  caused it.

Use it when the user refers to something discussed elsewhere, when you are about to
recommend or make a design choice that may already have been settled, or when you need to
know why a piece of code is the way it is. Navigate by titles and summaries, and read in
full only the nodes that look relevant.
