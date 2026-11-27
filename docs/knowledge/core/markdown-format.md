---
title: The markdown formatter
summary: The single markdown formatter and its four callers: fmt-md, the agent tool, commit turns and the knowledge store.
refs: src/markdown_format.rs, format_markdown, format_paths, format_committed
---

`src/markdown_format.rs` is the one markdown formatter of the repository: `dprint_plugin_markdown` with `TextWrap::Maintain` (the author's line breaks are kept) and `NewLineKind::Auto` (CRLF stays CRLF). Only `.md` files are touched, and `vendor`, `target` and `tests/fixtures` are excluded.

It is used four ways: the `fmt-md` subcommand, the `format_markdown` agent tool, the end of a commit turn (`format_committed`: formats the files the turn committed, then re-amends the commit keeping its message), and the knowledge store, which runs every entry body through it before writing. That last use is why a knowledge entry always reads back as Explorer wrote it: an entry whose layout is not the formatter's output is reported as malformed instead of read.

`format_committed` does nothing when the tree is dirty or HEAD did not move, and it re-amends rather than adding a second commit, so a commit turn leaves one clean commit per change.
