---
title: The report window
summary: The read-only markdown window that holds a generated report, saves nothing, and re-wraps only when the width or the theme moves.
refs: src/tui/panes/report_pane.rs, ReportComponent
---

`src/tui/panes/report_pane.rs` is a window that shows markdown to read and nothing else: no input, no model, nothing saved. A report is not part of the conversation, so it lives in no node and leaves nothing behind when it closes — which is what lets the command line's `scan` show its findings without them becoming a document.

The laid-out lines are cached with the window, keyed by the width and the theme name, so scrolling a long report does not re-wrap it; a resize or a theme change throws the cache away, and the cache counts its own renders so a test can show it is not laid out again per frame.
