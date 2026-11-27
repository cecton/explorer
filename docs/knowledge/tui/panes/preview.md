---
title: The file preview
summary: The read-only file view: search, line and symbol highlights, selection, the click counting behind double clicks, and what closing keeps.
refs: src/tui/panes/preview.rs, FilePreviewComponent, ClickTracker
---

`src/tui/panes/preview.rs` is the file reader: a file's text with a gutter of line numbers, read-only, wrapped at the pane width. It is where most of the reading happens, so it carries the marks that make reading work — `/` and `?` search by regular expression with `n`/`N` moving between matches, line highlights, symbol highlight overlays and text selection.

A drag with `Shift` adds a highlight range and with `Ctrl` removes one; a plain drag selects text. A click on the title bar scrolls to the highlight range under it; it does not select it. A single click selects the word and a triple click the line; a double click toggles a symbol highlight. Clicks are counted per cell with a 300 ms window, which is why the component keeps a `ClickTracker` rather than asking the terminal.

Closing the window drops its search, its title-bar input and its error, and keeps the highlights: they are notes the user left, and they are saved with the session.

The content is followed by `page_size - 1` blank rows, so `End` brings the last line to the top of the pane instead of to the bottom; a file that fits the page does not scroll at all.
