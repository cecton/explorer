---
title: Tests and their fixtures
summary: How the tests are set up: the shared builders per layer, the scripted model, and the fixture directory that must not be touched.
refs: src/util/test_support.rs, src/tui/test_support.rs, src/conversation/test_support.rs, src/agent/fake_llm.rs, tests/fixtures/jqwik
---

Tests run with `cargo test --release`, need `git` on `PATH`, and touch no network and no real model: the agent tests run the own loop against a scripted local server (`src/agent/fake_llm.rs`) and re-execute the test binary as the agent process, so no `explorer` binary is needed either.

Each layer has its own builders beside its tests: `src/util/test_support.rs` makes temporary repositories and commits (`temp_repo`, `temp_root`, `commit`, `git`), `src/tui/test_support.rs` provides `global_data(state)`, `key` and `frame_box` for the panes (it does not build an `AppState`), and `src/conversation/test_support.rs` writes node directories.

`tests/fixtures/jqwik/` is a byte-for-byte scan fixture, not documentation: its text is plain visible anti-AI notice, not a hidden payload (its `PROVENANCE.md` says so), and `the_jqwik_payload_is_review_and_never_danger` in `src/scan.rs` asserts its exact findings. `fmt-md` skips the directory; changing or reformatting the files would break that test.
