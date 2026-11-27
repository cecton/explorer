---
title: The file preview
summary: The read-only file view: search, highlights, selection, click counting, and the once-per-line overlays the paint loop rests on.
refs: src/tui/panes/preview.rs, FilePreviewComponent, ClickTracker, line_overlays, overlay_interval, segment_end, paint_line_segment
---

`src/tui/panes/preview.rs` is the file reader: a file's text with a gutter of line numbers, read-only, wrapped at the pane width. It is where most of the reading happens, so it carries the marks that make reading work — `/` and `?` search by regular expression with `n`/`N` moving between matches, line highlights, symbol highlight overlays and text selection.

A drag with `Shift` adds a highlight range and with `Ctrl` removes one; a plain drag selects text. A click on the title bar scrolls to the highlight range under it; it does not select it. A single click selects the word and a triple click the line; a double click toggles a symbol highlight. Clicks are counted per cell with a 300 ms window, which is why the component keeps a `ClickTracker` rather than asking the terminal.

The paint loop has one shape worth keeping. Symbol ranges are gathered once per frame, and a line's overlays are built once per line in `line_overlays`, in priority order — symbols, then search matches, then the selection — because `overlay_interval` lets a later one win over an earlier one on the same bytes. The sub-rows of a wrapped line are then walked with a byte cursor (`segment_end`: a byte count on an ASCII line, `fit_end` on any other), never by re-running the search over each sub-row: that was the quadratic case, where a minified line was scanned once per screen row it filled. `paint_line_segment` takes line-local byte ranges, and clamps them onto character boundaries before slicing, since a highlight computed before an external edit can name the middle of a character.

Closing the window drops its search, its title-bar input and its error, and keeps the highlights: they are notes the user left, and they are saved with the session.

The content is followed by `page_size - 1` blank rows, so `End` brings the last line to the top of the pane instead of to the bottom; a file that fits the page does not scroll at all.
