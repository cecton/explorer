---
title: The cargo xtask commands
summary: The crate's own tooling: theme regeneration, the import-rule checker, and the watch loop.
refs: xtask/src/main.rs, xtask/src/check_imports.rs, xtask/src/update_themes.rs, themes/themes.rs
---

`xtask` is the crate's own tooling, run as `cargo xtask <command>`.

`update-themes` lists the themes of the Helix repository through the GitHub API, downloads the ones that are not skipped (copyleft-licensed ones are), writes `themes/<name>.toml` and regenerates `themes/themes.rs`, the table of `include_str!` entries the binary compiles in. That file says not to edit it and `cargo xtask update-themes` is how it changes.

`check-imports` lists files with `git ls-files --cached --others --exclude-standard -- src`, so untracked files count, and fails on what the import rules forbid: a module-level `use` that is not `std`/`core`/`alloc` or `use r3bl_tui::*;`, `super::`/`self::` outside a test module, a single-variant enum import, a full variant path inside a function body, and a widget naming anything but widgets, the theme and `util`. Test modules are exempt from the `use` rule. It parses the source as rustfmt writes it, so a module-level item is one that starts at column zero.

`start` rebuilds and reruns the app with the `release-with-debug` profile under `xtask-watch`; `watch` only reruns `cargo check`.
