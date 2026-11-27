---
title: Selection and contrast
summary: The single app-wide text selection, the drag modifiers, and why a repainted foreground may have to give way.
refs: src/tui/selection.rs, TextSelection, SelPoint, DragModifier, src/tui/widgets/contrast.rs, ensure_readable_fg
---

There is one text selection in the application, in `src/tui/selection.rs`, shared by the file preview, the terminal and the Discussion: which window it is in and the two points it spans. A point is spelled per kind of window — a line and a byte offset in a preview, a viewport row and column in a terminal, a `DocPart` and an offset in a Discussion — so the copy and the highlight code handle one type instead of three.

A drag modifier is a separate thing: exactly one of `Shift` (add a highlight) or `Ctrl` (remove one), never `Alt`.

`src/tui/widgets/contrast.rs` exists for the same reason a selection needs care: text repainted over a marker color or a symbol color was picked against the pane background, so the foreground has to give way when the new background is close in brightness. It leaves the foreground alone when either side is a palette color or the default, since the terminal decides those, and it only ever swaps to near-black or near-white rather than inventing a tint.
