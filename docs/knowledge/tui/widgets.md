---
title: Widgets: the drawing layer
summary: The drawing layer, what each widget is for, the markdown options enabled, and how `paint_row` and `row_pieces` share a row between panes.
refs: src/tui/widgets/mod.rs, src/tui/widgets/markdown.rs, Span, paint_row, row_pieces, src/tui/widgets/text_layout.rs, src/tui/widgets/input_line.rs, src/tui/widgets/fuzzy.rs, src/tui/widgets/fuzzy_picker.rs, src/tui/widgets/title_row.rs, src/tui/widgets/contrast.rs
---

`src/tui/widgets/` is the drawing layer, and the only crate modules a widget may name are other widgets, `crate::tui::theme` and `util`. Nothing here knows about `AppState` or about a window: a widget is handed text, colors and a box and paints.

`markdown.rs` renders markdown into laid-out rows of styled spans, and each span keeps the byte offset it came from (`Span.src`: `None` for every cell the renderer adds, such as markers, padding, borders and rewritten entities; `Some` only when the source holds the text verbatim), which is what lets a Discussion or a report map a click back to the text. Only strikethrough, task lists and tables are enabled, so anything else in a document is not rendered at all.

`paint_row` is the row painter both the Discussion and the report paint through: the stripe behind the row, then each span at its column, stopping at the row's width and advancing by what it painted. A marked range takes the mark's background and a readable foreground unless the caller keeps the rendered one, and `row_pieces` splits spans at those marks and only then — with nothing marked it returns the spans unsplit, so a plain row allocates nothing extra.

`text_layout.rs` has the width, truncation and wrapping primitives everything else uses, and it is where a wide character is counted once. `input_line.rs` is the single-line editor with a cursor, scrolling, its own mouse selection and a double click. `fuzzy.rs` only paints matched runs and tests whether a character is matched; the ranking itself is `core/fuzzy`. `title_row.rs` paints a window's title bar or its input title, and `contrast.rs` keeps a repainted foreground readable.
