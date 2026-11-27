---
title: The shared helpers
summary: The bottom layer: files and directories, text handling, time, fuzzy matching, one HTTP call and process-group kills.
refs: src/util/mod.rs, src/util/fs.rs, src/util/text.rs, src/util/time.rs, src/util/fuzzy.rs, src/util/http.rs, src/util/process.rs, src/util/test_support.rs
---

`src/util/` is the shared helpers, and it may import nothing else from the crate. `fs.rs` has the atomic write, the data and config directories, the repository hash that names the per-repository directories, and the join that refuses to leave the root. `text.rs` has the word bounds, the drag range, the single-line flattening, the metadata block splitter and the middle truncation that everything from work logs to commit summaries uses. `time.rs` has the UTC stamps of node ids, the local offset and the `HH:MM` of the clock.

`fuzzy.rs` wraps nucleo with one pattern and matcher reused per query and three kinds of match (plain, path, substring). `http.rs` is one bearer-token GET with a 20 s timeout, one number for every non-model request. `process.rs` is `killpg` and the group guard that kills a command's whole group on drop.

`test_support.rs` builds temporary repositories and commits for the tests of every layer, and each layer's own test module has its own builders beside it.
