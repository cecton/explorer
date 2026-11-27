---
title: Semantic tokens and symbol references
summary: The two round trips: semantic tokens decoded into colored spans, and hover plus references turning a click into a symbol group.
refs: src/lsp/tokens.rs, src/lsp/symbols.rs, src/highlight.rs, TokenKind, ColoredSpan
---

`src/lsp/tokens.rs` opens a file on the server, asks for its semantic tokens and decodes the answer into the per-line spans the preview paints. `src/lsp/symbols.rs` is the other round trip: a hover names the symbol under a click, then references find every place that name appears, and the pair becomes a `SymbolHighlight` for the main loop.

The decoding is where the care is: semantic tokens arrive as deltas in UTF-16 columns, so they are converted to byte offsets and clamped, and every line is covered end to end with `Other`, since a line with no token still has to be colored as plain text. The server's legends also differ between versions, so `TokenKind::from_name` maps only the names Explorer styles and falls back to `Other` for every other.

A changed file is never sent as an edit: the file is closed and opened again with the new content, so no document version can drift from what is in memory.
