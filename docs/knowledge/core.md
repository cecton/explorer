---
title: The core modules
summary: The middle layer of the crate: the modules that touch git, the disk, config, models and the OS, with an entry each.
refs: src/git.rs, src/loader.rs, src/loader/watch.rs, src/watcher.rs, src/config.rs, src/endpoint.rs, src/model_spec.rs, src/usage.rs, src/markdown_format.rs, src/scan.rs, src/sandbox.rs, src/signal.rs, src/util/mod.rs, src/conversation/mod.rs
---

These are the modules that sit between the file list and the screen: `git.rs` (history and writes), `loader.rs` with its watcher (the repository in memory), `config.rs` (the two config files), `endpoint.rs` and `model_spec.rs` (which server a model runs on), `usage.rs` (subscription spend), `markdown_format.rs` (the one formatter), `scan.rs` (`--scan`), `sandbox.rs` (`sandbox-exec`), `signal.rs` (how threads reach the main loop) and `conversation/` (the tree on disk).

Each has an entry of its own. The rule they share: none of them imports `tui` or `agent`, and the ones that spawn processes do it so the caller keeps its signal and process-group handling.
