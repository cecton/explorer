---
title: Who writes the base, and how a change is staged
summary: Who may change the base, why every change is staged at once, and what `check` reports.
refs: src/knowledge.rs, Access, settle, is_mutation, missing_symbols, src/agent/tools.rs
---

What a turn may do with the store follows its mode. A commit turn records, changes and removes entries, and each change is staged (`git add` on the exact path, retried while another git process holds `index.lock`) so the turn's own commit carries it. A feedback turn only reads: its work is discarded when it ends, and its copy of the repository is not the repository. `Access` carries this, and `spec(Access)` offers a reading turn only the read actions.

Because the change is staged immediately, every change's answer ends by telling the turn not to unstage it (the `toc`, `read`, `search` and `check` answers do not), and `knowledge::settle` takes and drops the change mutex so a turn's close cannot race a change still in flight. The `write` and `edit` tools refuse `docs/knowledge/` so that the store only grows through the tool.

`check` is the pass that keeps the base honest: entries that cannot be read, entries without `refs`, `refs` naming a path that is gone, and `refs` naming a symbol no file holds, found in one `git grep --untracked` over the tree with the store itself excluded, so a turn that writes code and its entry together finds its own symbols. A symbol is looked up by its last `::` segment, since a method is rarely written as a literal. A ref is either a path or a bare symbol name, never both joined: a missing ref that also holds `::` is reported as a path and a symbol written as one, and the prompts say the two belong in two refs.
