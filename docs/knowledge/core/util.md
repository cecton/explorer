---
title: The shared helpers
summary: The bottom layer: files and directories, text handling, time, fuzzy matching, one HTTP call and process-group kills.
refs: src/util/text.rs, word_bounds, url_start, drag_range, single_line, split_metadata, key_values, truncate_middle, src/util/fuzzy.rs
---

`src/util/` is the shared helpers, and it may import nothing else from the crate. `fs.rs` has the atomic write, the data and config directories, the repository hash that names the per-repository directories, and the join that refuses to leave the root. `text.rs` has the word bounds, the drag range, the single-line flattening, the metadata block splitter and the middle truncation that everything from work logs to commit summaries uses. `time.rs` has the UTC stamps of node ids, the local offset and the `HH:MM` of the clock.

`fuzzy.rs` wraps nucleo with one pattern and matcher reused per query and three kinds of match (plain, path, substring); see core/fuzzy for what each kind reads. `http.rs` is one bearer-token GET with a 20 s timeout, one number for every non-model request. `process.rs` is `killpg` and the group guard that kills a command's whole group on drop.

The one rule in `text.rs` that looks like an optimisation and is not: `word_bounds` finds the URL case through `url_start`, which tries at most one candidate per scheme class for each `:` of a whitespace-free run. Parsing per character instead makes a click quadratic, and a minified line or a base64 blob freezes the UI on every mouse move of a drag.

`test_support.rs` builds temporary repositories and commits for the tests of every layer, and each layer's own test module has its own builders beside it.
