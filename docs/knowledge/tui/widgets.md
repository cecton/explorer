---
title: Widgets: the drawing layer
summary: The drawing layer and what each widget is for, including the byte offsets markdown spans keep for click mapping.
refs: src/tui/widgets/mod.rs, src/tui/widgets/markdown.rs, src/tui/widgets/text_layout.rs, src/tui/widgets/input_line.rs, src/tui/widgets/fuzzy.rs, src/tui/widgets/fuzzy_picker.rs, src/tui/widgets/title_row.rs, src/tui/widgets/contrast.rs
---

`src/tui/widgets/` is the drawing layer, and the only crate modules a widget may name are other widgets, `crate::tui::theme` and `util`. Nothing here knows about `AppState` or about a window: a widget is handed text, colors and a box and paints.

`markdown.rs` renders markdown into laid-out rows of styled spans, and each span keeps the byte offset it came from (`Span.src`, an `Option<usize>`, `None` for markers, padding, borders and rewritten entities), which is what lets a Discussion or a report map a click back to the text. `text_layout.rs` has the width, truncation and wrapping primitives everything else uses, and it is where a wide character is counted once. `input_line.rs` is the single-line editor with a cursor, scrolling, its own mouse selection and a double click. `fuzzy.rs` only paints matched runs and tests whether a character is matched; the ranking itself is `util::fuzzy`, and `fuzzy_picker.rs` is the picker state built on it. `title_row.rs` paints a window's title bar (`render_pane_title`) or its input title (`render_input_title`), and `contrast.rs` keeps a repainted foreground readable.
