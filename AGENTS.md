# AGENTS.md — Explorer

This file provides guidance for agentic coding assistants operating in this repository.
The project is in very early development — don't hesitate to make breaking changes and
don't write migration or retro-compatibility code.

Every change you make must be committed with a clear title and description. Run
`cargo fmt` before every commit.

---

## Project Overview

`explorer` is a Rust TUI file explorer that:
1. Locates the nearest `.git` root from the current directory.
2. Walks the entire repository tree in parallel (via `jwalk` + rayon), skipping `.git` and `target`.
3. Loads every UTF-8 text file into memory as a `LoadedFile` struct.
4. Displays files in an interactive TUI: left pane is the file list, right pane is a syntax-highlighted preview.

Source is organized as:
- `src/main.rs` — file loading, async entry point
- `src/tui/mod.rs` — module declarations
- `src/tui/state.rs` — `State` and `AppSignal` types
- `src/tui/app.rs` — `App` trait impl, layout, `run()` entry point
- `src/tui/file_list.rs` — `FileListComponent` (left pane)
- `src/tui/preview.rs` — `FilePreviewComponent` with syntect syntax highlighting (right pane)

---

## Build & Run Commands

```bash
# Build (dev)
cargo build

# Build (release)
cargo build --release

# Run without a search query (lists all loaded files)
cargo run

# Run with a fuzzy search query
cargo run -- <query>

# Format (required before every commit)
cargo fmt

# Lint
cargo clippy

# Check without producing a binary
cargo check
```

### Tests

There are no tests yet. When adding tests:

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
3. Run `cargo clippy` and address any warnings before merging.

---

## Dependencies

| Crate      | Version | Purpose                                           |
|------------|---------|---------------------------------------------------|
| `camino`      | 1.x     | UTF-8–typed path types (`Utf8PathBuf`, etc.)      |
| `jwalk`       | 0.8.x   | Parallel directory traversal (uses rayon)         |
| `nucleo`      | 0.5.x   | Fuzzy matching for paths and file content         |
| `r3bl_tui`   | 0.7.x   | TUI framework with Linux-native `direct_to_ansi` backend, PTY/terminal-multiplexer support |
| `serde_json`  | 1.x     | JSON-RPC message serialization for LSP protocol   |
| `tokio`       | 1.x     | Async runtime required by `r3bl_tui`              |

Add a dependency when it provides substantial value that would take significant effort to replicate correctly — covering performance, correctness, or capability. Prefer `std` for trivial things. Each dep must have a concrete, stated purpose in this table.

Planned feature areas and their likely dependencies:
- **File watching**: `notify-rust` (already a transitive dep of `r3bl_tui`)
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
- Avoid glob imports (`use std::io::*`) except in test modules where `use super::*` is conventional.

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

- Parse arguments with `std::env::args()` directly. Do not add a CLI-parsing crate (`clap`, `argh`, etc.) unless the argument surface grows substantially more complex.

### Comments

- Write no comments by default.
- Add a comment only when the *why* is non-obvious: a hidden constraint, a subtle invariant, a workaround for a specific external bug.
- Never describe *what* the code does — well-named identifiers already do that.
- No multi-line comment blocks. One short line maximum.

### General

- Do not add features, abstractions, or error handling for hypothetical future requirements.
- A bug fix does not need surrounding cleanup.
- Three similar lines is better than a premature abstraction.
- No half-finished implementations.
- Do not add `#[allow(dead_code)]` or similar suppression attributes to ship around incomplete work.
