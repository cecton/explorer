---
title: Fuzzy matching: one matcher, three kinds
summary: The one fuzzy matcher: the three kinds and why they read bytes or chars, why `Utf32Str::new` is avoided, and the query reused across calls.
refs: src/util/fuzzy.rs, Kind, Query, ranked, match_indices, score_and_occurrences, haystack, src/tui/panes/conversation_tree/filter.rs, src/tui/modal/window_picker.rs
---

`src/util/fuzzy.rs` is the one fuzzy matcher, and its `Kind` says how a query reads its candidates — not cosmetically, but because nucleo scores the same text differently over bytes and over chars, so each kind keeps the ranking its callers always had.

`Kind::Plain` and `Kind::Paths` parse the query one way and read an ASCII candidate as bytes (`Utf32Str::Ascii`), any other as chars. `Kind::Paths` gives the matcher a path configuration, so a segment boundary scores higher — this is the window picker's file names and the git log's filter. `Kind::Substring` parses the query with substring atoms, wants every word in any order, and pads the char haystack with a newline, because a substring atom otherwise misses a match that ends a non-ASCII text; the conversation tree's filter uses it.

`Utf32Str::new` is never used: it indexes by grapheme, and as bytes for text whose graphemes all begin with an ASCII character, so its indices drift from the chars a row paints and a combining accent makes a highlight land on the wrong letters.

`Query` holds the pattern, the matcher and the haystack across calls, and `set` reparses only a text that changed, which is what lets a frame reuse it. `ranked` is the whole loop for a caller that wants only the keys; `match_indices` and `score_and_occurrences` also answer the char indices a highlighted row paints.
