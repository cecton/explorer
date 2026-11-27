---
title: The crate's modules and import rules
summary: The module tree, the layer order below `tui`, and the unusual import style that `cargo xtask check-imports` enforces.
refs: src/main.rs, xtask/src/check_imports.rs, xtask/src/update_themes.rs, themes/themes.rs, AGENTS.md
---

`src/main.rs` declares every module of the crate, alphabetically. The layer order comes from `AGENTS.md`: `util` at the bottom, then the core modules (`git`, `loader`, `config`, `endpoint`, `conversation`, `lsp`, `highlight`, `knowledge`, `sandbox`, `scan`, `signal`, `usage`, `watcher`, `markdown_format`, `model_spec`), then `agent`, then `tui`. Each layer may import only from itself or below; a dependency upward is a bug, not a style choice.

The import style is unusual. `xtask/src/check_imports.rs` enforces: no module-level `use` except `std`/`core`/`alloc` and `use r3bl_tui::*;`, no `super::` or `self::` (outside a test module), no single-variant enum imports, no variant spelled as a long `crate::` path inside a function body, and a widget naming only other widgets, `crate::tui::theme` and `util`. Spelling `crate::` paths out for every function and constant call is only a convention, not checked. `cargo xtask update-themes` downloads the Helix themes from GitHub, skips copyleft ones, and regenerates `themes/themes.rs`.

`AGENTS.md` carries the same rules plus the build commands; when this entry and it disagree, it is right.
