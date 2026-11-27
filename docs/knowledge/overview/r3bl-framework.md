---
title: The vendored TUI framework
summary: The vendored r3bl TUI framework: where it lives, why it is a fork, and the rules for touching it.
refs: Cargo.toml, vendor/r3bl-open-core, src/tui/app/mod.rs, src/lsp/worker.rs, src/watcher.rs
---

The TUI framework is r3bl's `tui` crate, vendored as a git submodule at `vendor/r3bl-open-core` and patched on the `fix-rrt-with-sane-types` branch of a fork. Everything Explorer draws or routes goes through it: `TerminalWindow` runs the event loop, `App` is the trait `AppMain` implements, `RRT` drives the worker threads (the LSP worker and the file watcher), and `OfsBufVT100` is the terminal emulator.

`Cargo.toml` also patches `proc-macro2` from git for a nightly breakage; that patch is not to be removed or "fixed".

Work on the submodule only when it is asked for, and never build, format or lint it on its own: `cargo clippy --no-deps` skips it, and a command that builds all targets builds it as a dependency anyway. `use r3bl_tui::*;` is the one wildcard import allowed at module level, and it brings Fx-hashed `HashMap`/`HashSet`, so std's must be imported explicitly where they are wanted.
