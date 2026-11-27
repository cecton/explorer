---
title: Symbol highlight groups
summary: How a symbol gets one group and one color, how groups survive file changes and in-flight requests, and why locations are not saved.
refs: src/tui/symbols.rs, src/signal.rs, SymbolHighlightGroup, SymbolHighlight, SymbolRefLocation
---

A symbol highlight group ties a qualified name to every place it appears, colored so the origin and its uses can be read at a glance. `src/tui/symbols.rs` holds the bookkeeping, deliberately outside both the file-change handler and the symbol-answer handler, so the two share one rule for keeping a name unique: a group is dropped when another group for the same name survives.

Groups are looked up by their origin (file, line, column; the word is not part of the lookup) rather than by index, because a request still in flight shifts indices when an earlier one is toggled off. The file-change handler deletes a group whose origin file was removed, re-requests one whose origin file changed, and prunes uses in touched files. Only a fresh answer takes the next unused palette color; an answer for an existing group refreshes its locations, an empty answer removes the group, and an answer for a group that was removed in the meantime changes nothing.

The span locations themselves are byte offsets from `src/lsp/symbols.rs`, which the preview paints, and they are never saved: only the origin identity is, and the locations are asked of rust-analyzer again at startup.
