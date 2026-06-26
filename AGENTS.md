# AGENTS.md — Explorer

This file provides guidance for agentic coding assistants operating in this repository.
The project is in very early development — don't hesitate to make breaking changes and
don't write migration or retro-compatibility code.

Every change you make must be committed with a clear title and description. Run
`cargo fmt` before every commit. Always use the git skill before committing.

When working on r3bl repository (vendor/r3bl-open-core), never try to compile
the code or format or use rustup.

---

## Project Overview

`explorer` is a Rust TUI file explorer that:
1. Locates the nearest `.git` root from the current directory.
2. Walks the entire repository tree in parallel (via `jwalk` + rayon), skipping `.git` and `target`.
3. Loads every UTF-8 text file into memory as a `LoadedFile` struct.
4. Displays files in an interactive TUI: left pane is the file list, right pane is a syntax-highlighted preview.

Source is organized as:
- `src/main.rs` — async entry point, wires CLI → loader → TUI
- `src/cli.rs` — CLI argument parsing via `pico-args`
- `src/config.rs` — KDL configuration file parsing (theme, future fields)
- `src/loader.rs` — parallel file walking and `LoadedFile` construction
- `src/session.rs` — session save/restore (pane stack, terminals, highlights)
- `src/lsp.rs` — LSP client (JSON-RPC over stdio); public API: `send_file_request`, `health_check`, `try_drain_pending_requests`, `LspHealth` enum
- `src/watcher.rs` — filesystem watcher via `notify`; debounces events into `BatchedWatchEvent` and broadcasts via `WATCHER_RRT`
- `src/tui/mod.rs` — module declarations
- `src/tui/title_row.rs` — `TitleRow` trait, `render_pane_title` utility, `title_bar_colors`
- `src/tui/state.rs` — `AppState` (with global `TextSelection`, `SelPoint`, `terminal_grabbed`, `PaneManager`), `AppSignal`, `TerminalPane`, `Window`, `WindowState`, and `FuzzyPickerState` types
- `src/tui/app.rs` — `App` trait impl, layout, `run()` entry point; handles global text selection, terminal grab/ungrab, mouse event routing, and PaneManager-driven layout
- `src/tui/pane_manager.rs` — `PaneManager` type: focus, resize, stacking, and layout of 16 pane slots; responds to `PaneCommand` enums for directional movement and resize operations
- `src/tui/pane_component.rs` — `PaneComponent` implementation: renders a stack of overlapping windows within a pane slot with tab-style headers; manages per-pane state and command routing to child components
- `src/tui/file_name_picker.rs` — fuzzy file-name picker overlay (exceptions → input → navigation)
- `src/tui/fuzzy_picker.rs` — shared fuzzy list picker component; navigation via flat `match` on `InputEvent`
- `src/tui/theme.rs` — `HelixTheme` type; loads bundled TOML files via `include!("../../themes/themes.rs")`
- `src/tui/theme_picker.rs` — theme picker overlay with fuzzy search and live preview (exceptions → input → navigation)
- `src/tui/preview.rs` — `FilePreviewComponent` with syntect syntax highlighting (right pane); text selection uses global `AppState`
- `src/tui/terminal_pane.rs` — `TerminalPaneComponent` with PTY-based terminal emulation; `render_ofs_buf_to_ir()` emits `ResetColor` for `Spacer` runs to prevent stale SGR inheritance; supports mouse selection, word/line text extraction, and clipboard copy; skips render when `synchronized_output` is active (DEC private mode 2026) to preserve previous frame during batch updates
- `src/tui/input_line.rs` — query input with Emacs-style key bindings; single flat `match` on `InputEvent`

---

## Build & Run Commands

```bash
# Build (dev) -- always use release even for development
cargo build --release

# Build (release)
cargo build --release

# Run
cargo run --release

# Format (required before every commit)
cargo fmt

# Lint (skips vendored r3bl dependencies)
cargo clippy --no-deps

# Check without producing a binary
cargo check
```

### xtask

The workspace includes an `xtask` crate aliased via `.cargo/config.toml`:

```bash
# Run with file-watching (auto-restart on change), using release-with-debug profile
cargo xtask start

# Watch source and run `cargo check` on change
cargo xtask watch

# Regenerate themes/themes.rs from themes/*.toml (run after adding/changing theme files)
cargo xtask update-themes
```

### Tests

Tests live in `src/tui/pane_manager.rs` (unit tests for focus, resize, stacking, and layout). When adding tests:

```bash
# Run all tests
cargo test

# Run a single test by name
cargo test <test_name>

# Run tests in a specific module
cargo test <module>::<test_name>
```

---

## Before Every Commit

1. Run `cargo fmt` — no exceptions. Commits must have formatted code.
2. Run `cargo build` (or `cargo check`) to confirm there are no compile errors.
3. Run `cargo clippy --no-deps` and address any warnings before merging.

---

## Dependencies

| Crate                  | Version | Purpose                                                        |
|------------------------|---------|----------------------------------------------------------------|
| `arc-swap`             | 1.x     | Lock-free atomic swap for the shared file list (`Arc<ArcSwap<Vec<Arc<LoadedFile>>>>`) |
| `camino`               | 1.x     | UTF-8–typed path types (`Utf8PathBuf`, etc.)                   |
| `directories`          | 5.x     | XDG Base Directory resolution for config and session storage   |
| `hex`                  | 0.4.x   | Hex encoding for session filename hashes                       |
| `jwalk`                | 0.8.x   | Parallel directory traversal (uses rayon)                      |
| `kdl`                  | 6.x     | KDL config file parsing                                        |
| `libc`                 | 0.2.x   | PTY/OS-level syscalls used by terminal emulation               |
| `lsp-types`            | 0.97.x  | Typed LSP protocol structs (with `proposed` feature)           |
| `miette`               | 7.x     | Error reporting                                                |
| `notify`               | 8.x     | Filesystem event watching (used in `watcher.rs`)               |
| `nucleo`               | 0.5.x   | Fuzzy matching for paths and file content                      |
| `pico-args`            | 0.5.x   | Lightweight CLI argument parsing (no proc macros)              |
| `r3bl_tui`             | 0.7.x   | TUI framework with Linux-native `direct_to_ansi` backend, PTY/terminal-multiplexer support |
| `serde`                | 1.x     | Derive macros for serialization (`derive` feature)             |
| `serde_json`           | 1.x     | JSON-RPC message serialization for LSP protocol                |
| `sha2`                 | 0.10.x  | Stable repo-root hashing for session filenames                 |
| `toml`                 | 0.8.x   | Theme file parsing (`themes/*.toml`)                           |
| `tokio`                | 1.x     | Async runtime required by `r3bl_tui`                           |
| `tracing`              | 0.1.x   | Structured logging macros (`debug!`, `info!`, etc.)            |
| `tracing-core`         | 0.1.x   | `LevelFilter` type used to configure `r3bl_tui` logger         |
| `unicode-segmentation` | 1.x     | Word boundary detection in `input_line`                        |
| `url`                  | 2.x     | URL parsing in `word_bounds` for cursor-based URL selection    |

Planned feature areas and their likely dependencies:
- **Git information** (blame, diff, status): `git2`

---

## Code Style

### Formatting

- `cargo fmt` is authoritative. Do not hand-format; let rustfmt decide.
- Edition 2024 is in use. Use its idioms (e.g., `let … else`, `if let` chains).

### Naming Conventions

- Types and traits: `UpperCamelCase`
- Functions, methods, variables, modules: `snake_case`
- Constants and statics: `SCREAMING_SNAKE_CASE`
- No Hungarian notation. Name by what a thing *is*, not its type.

### Imports

- Group `use` statements: standard library first, then external crates, then local modules.
- Prefer importing the type directly over aliasing (`use camino::Utf8PathBuf` not `use camino::Utf8PathBuf as UtfBuf`).
- Glob imports are fine when they reduce boilerplate for large, stable external APIs (e.g. `use r3bl_tui::*;` in a module that re-exports to its children).
- Function-scoped `use` is fine for rarely-used imports — no need to hoist them to module level.

### Paths

- **Always** use `camino::Utf8PathBuf` / `camino::Utf8Path` for paths stored in structs or passed between functions.
- Use `std::path::PathBuf` only at FFI/OS boundaries (e.g., values returned by `std::env::current_dir` or `jwalk`), and convert to `Utf8PathBuf` as early as possible via `Utf8PathBuf::from_path_buf(p).ok()?`.
- For filesystem *name comparisons* (not full paths), use `OsString` / `OsStr` directly — do not convert to `String` just to compare.

### Error Handling

- In functions that return `Option<T>`, propagate failures with `?` and return `None` for expected failure modes (file unreadable, non-UTF-8 path).
- Use `.expect("message")` only for conditions that are truly unrecoverable programmer errors (e.g., `current_dir()` failing). The message should state *what invariant was violated*.
- Never use `.unwrap()` except in tests or throwaway prototypes.
- Do not add error handling for scenarios that cannot happen given the surrounding invariants.

### Memory & Allocation

- Pre-allocate `Vec` capacity when a reasonable estimate is available, then call `.shrink_to_fit()` afterward if the estimate may have overshot.
- Prefer `&str` / `&Utf8Path` over owned types in function parameters when the function does not need ownership.

### Parallelism

- File walking and loading is parallelised via `jwalk` (backed by rayon). Do not add a separate serial walk phase.
- Directory subtrees to skip (`.git`, `target`) must be pruned in the `process_read_dir` callback — before rayon enqueues them — not filtered afterward.
- `nucleo::Matcher` is not `Send`; matching is done single-threaded per query invocation. If the corpus grows large enough to warrant parallel matching, create one `Matcher` per rayon thread via `thread_local!`.

### CLI Arguments

- Use `pico-args` for argument parsing. It has no proc macros and minimal overhead.
- `LevelFilter` from `tracing-core` does not implement `FromStr`. Parse log level as
  `Option<String>` via `pico-args`, then convert to `LevelFilter` with a `match`.

### Logging

- Logging uses `r3bl_tui::log` (`tracing`-based). Use `tracing::debug!`, `tracing::info!`,
  etc. — never the `log` crate macros.
- `r3bl_tui` provides `try_initialize_logging_global(config)` where config is a
  `TracingConfig { level_filter, writer_config }`.
- `WriterConfig::File(path)` logs to an exact file path (no rolling suffix).
- `DisplayPreference::Stdout` / `Stderr` must not be used in a TUI — it corrupts the display.
- Logging is **off by default**. It is enabled only when `--log-file <path>` is passed.
  An optional `--log-level <level>` controls verbosity (error/warn/info/debug/trace);
  default is `debug`.

### Terminal Pane Grab/Ungrab

- `AppState::terminal_grabbed` is a single global flag: when `true`, keyboard events go to the focused PTY; when `false`, they propagate to app-level shortcuts.
- Scrolling a terminal pane ungrabs it. Clicking the pane or pressing Enter/Esc re-grabs.
- When removing a terminal window via `remove_window`, only reset `terminal_grabbed` if the window being removed is the currently focused one — otherwise a background pane exit silently ungrabs an active terminal.

### Comments

- Write no comments by default.
- Add a comment only when the *why* is non-obvious: a hidden constraint, a subtle invariant, a workaround for a specific external bug.
- Never describe *what* the code does — well-named identifiers already do that.
- No multi-line comment blocks. One short line maximum.

### Pattern Matching

- Prefer a single flat `match` on the original value (e.g. `&InputEvent`) over a chain of `if let` blocks that unpack intermediates.
- Embed the full path in each arm: `InputEvent::Keyboard(KeyPress::Plain { key: Key::SpecialKey(SpecialKey::Esc) })` instead of `if let InputEvent::Keyboard(KeyPress::Plain { key }) = input_event { match key { Key::SpecialKey(SpecialKey::Esc) => ... } }`.
- Use `|` to share an arm body when two patterns are semantically equivalent (e.g. Down arrow and scroll-down).
- Destructure modifier masks directly in the pattern (`ModifierKeysMask { ctrl_key_state: KeyState::Pressed, .. }`) rather than match guards like `if mask == ModifierKeysMask::new().with_ctrl()`.
- **Never match on an outer enum to extract an inner value only to match on it again.** A two-level `match input_event { InputEvent::Keyboard(keypress) => { match keypress { ... } } }` is the same anti-pattern as `if let` + `match`. Every keyboard arm must be a top-level arm with the full path: `InputEvent::Keyboard(KeyPress::Plain { key: Key::SpecialKey(SpecialKey::Esc) })`.

### General

- Do not add features, abstractions, or error handling for hypothetical future requirements.
- A bug fix does not need surrounding cleanup.
- Three similar lines is better than a premature abstraction.
- No half-finished implementations.
- Do not add `#[allow(dead_code)]` or similar suppression attributes to ship around incomplete work.
- **No magic sentinel values** — never encode "invalid" or "absent" state by
  stuffing a sentinel number (e.g. `-1`) into a field that normally holds
  real data. This is a common slope AI pattern and it is broken by design:
  sentinels are invisible to downstream code — clamps, casts, arithmetic,
  and comparisons all treat them as ordinary values, creating hidden coupling
  between producer and every consumer. Use `Option<T>` or a dedicated `enum`
  variant instead.
