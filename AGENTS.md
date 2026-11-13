# AGENTS.md — Explorer

This file provides guidance for agentic coding assistants operating in this repository.
The project is in very early development — don't hesitate to make breaking changes and
don't write migration or retro-compatibility code.

Every change you make must be committed with a clear title and description. Run
`cargo fmt` before every commit. Markdown layout belongs to Explorer's own formatter
(`src/markdown_format.rs`): never format markdown by hand, run `explorer fmt-md [paths]`
(or the `format_markdown` tool) if you want it formatted before the turn ends. Always use
the git skill before committing.

When working on r3bl repository (vendor/r3bl-open-core), never try to compile
the code or format or use rustup.

---

## Project Goal

Explorer is a **personal, repo-scoped terminal workspace** — a keyboard- and
mouse-driven cockpit for working inside a single git repository. For its owner it is
meant to replace the everyday combination of a tiling window manager, a terminal
multiplexer (tmux), a TUI file manager (yazi/nnn), and — eventually — `tig`.

Guiding principles:

- **Get back to work fast, every day.** Session persistence is the north star:
  Explorer saves and restores _everything that was running_ — pane layout, open files,
  live terminals, and highlights (see `src/tui/session.rs`) — so a work session resumes
  instantly.
- **Speed via whole-repo-in-memory.** The entire repository is walked in parallel and
  held in memory (`src/loader.rs`), making navigation, preview, and search feel
  instantaneous. Do not trade this away.
- **Mouse and keyboard are both first-class.** This is not a keyboard-only TUI; mouse
  interaction (selection, focus, resize) is a supported, well-developed input path.
- **Editing is delegated, permanently.** Explorer orchestrates; real editing happens in
  the owner's editor (helix/vim/…) spawned in a pane (`e` in the file preview). Explorer
  will **not** grow its own native text editor — do not propose one.
- **Personal tool.** Decisions are shaped by the owner's workflow, not by
  general-purpose adoption. Prefer the direct solution over configurable generality.

Direction (partly built — log and diff panes and the conversation agent shipped;
status and blame are not — do not scaffold ahead of need): deeper **AI tooling**
integration, and native **git tooling** to replace `tig` (status, blame).

---

## Project Overview

`explorer` is a Rust TUI workspace built around a git repository. Its core:

1. Locates the nearest `.git` root from the current directory.
2. Walks the entire repository tree in parallel (via `jwalk` + rayon), skipping `.git` and `target`.
3. Loads every UTF-8 text file into memory as a `LoadedFile` struct, so preview and search need no disk read.
4. Shows it as a stack of windows laid out by `PaneManager` (`src/tui/pane_manager.rs`): a `Window` is a file preview, a terminal, a git log, a commit diff, the conversation tree, a discussion, a report, or a picker. Files are not listed anywhere — `WindowPicker` (`<leader>space`) opens one by fuzzy name.

Source is organized in layers. A module imports only from its own layer or a lower one, never from one above (`config.rs` names `tui::keymap` in a doc comment only):

| Layer                               | Modules                                                                                                                                                                                                                                                                 |
| ----------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1. `util/`                          | `time`, `fs`, `text`, `process`, `http`: helpers every layer shares, importing no other module of the crate                                                                                                                                                             |
| 2. Core data, services and `signal` | `cli`, `git`, `loader`, `scan`, `markdown_format`, `config`, `conversation/`, `endpoint`, `model_spec`, `usage`, `highlight`, `lsp/`, `watcher`, `signal` (`AppSignal`, the event vocabulary every thread speaks; core threads send it, so its payloads are core types) |
| 3. `agent/`                         | turns, the agent loop, sub-agents, tools, MCP, slash commands                                                                                                                                                                                                           |
| 4. `tui/`                           | everything on screen, plus `session`                                                                                                                                                                                                                                    |

`loader/watch.rs` is the one module outside `tui/` and `main.rs` that calls into `lsp` (`send_file_request`, to tell it a file changed); `lsp/tokens.rs` reads `loader::LoadedFile`. Keep it that way.

- `src/main.rs` — async entry point, wires CLI → loader → TUI
- `src/cli.rs` — CLI argument parsing via `pico-args`: `--scan` prints the scan report and exits, `fmt-md [paths]` (first argument only) formats markdown and exits
- `src/config.rs` — KDL configuration (theme, `keybindings`, the `agent` block with endpoints and models, `review-rounds`, `repeat-limit`, the `after-agents` block, top-level `alias` lines); a bad `config.kdl` value is refused at load, never defaulted silently; `load_auth` reads endpoint tokens from `<data_dir>/auth.kdl`, where a bad file is logged and yields no token
- `src/endpoint.rs` — `Provider`, the kind of server behind an endpoint and its default URL; the one place that knows which providers report usage
- `src/model_spec.rs` — `Effort` and `ModelSpec` (`endpoint/model/effort`); names no other module
- `src/usage.rs` — subscription usage: `Usage` and its windows, one `fetch` per provider; sends nothing itself, the turn's `report_usage` does
- `src/git.rs` — gix-backed git data: the log (first-parent walk, capped at `MAX_LOG_ENTRIES`), refs, commit diffs, conventional-commit prefixes; the turn helpers shell out to `git` through `git_output`
- `src/loader.rs` — parallel walk and read of the whole repository into memory as `LoadedFile`s, so preview and search never touch the disk (do not trade this away); `IGNORED_DIRS` and `is_ignored` are the one prune rule, shared by the walk, the watcher and the trigger check; `FileKey`s stay valid because the list is append-only
- `src/loader/watch.rs` — a changed batch in two halves: `read` on the watcher thread (touches no file on the UI thread), `swap_in` on the main thread; publishing new content in the same turn that invalidates the offsets naming into it is what stops a frame painting one against the other
- `src/watcher.rs` — `notify` watcher, debounces into a `loader::watch::Batch`, reads it and sends `AppSignal::FilesChanged`
- `src/scan.rs` — deterministic scanner for hidden or misleading text aimed at an agent; its own pruning (hidden files are scanned), follows no symlink, modifies and executes nothing
- `src/markdown_format.rs` — the markdown formatter (`dprint-plugin-markdown` as a library) behind `explorer fmt-md`, the `format_markdown` agent tool and the end of a commit turn; markdown is never formatted by hand
- `src/highlight.rs` — `TokenKind` and the theme-scope fallbacks of semantic tokens; here so `loader` and `lsp` share the color types without either importing the other for them
- `src/lsp/` — the rust-analyzer client on a thread of its own (`LSP_RRT`): `mod.rs` (public API, `disabled` via `EXPLORER_NO_LSP`, UTF-16 ↔ byte offsets; requests made while no worker runs are buffered), `rpc.rs` (framing), `worker.rs` (handshake, dispatch, readiness gate, restart), `tokens.rs`, `symbols.rs`; nothing on the main thread retries a request
- `src/conversation/` — the agent conversation tree: `mod.rs` (`ConversationTree`, `Node`, `TurnContext`; holds metadata, titles and summaries only, prompts, replies and transcripts stay on disk; `revision()` comes from one process-wide counter so a reloaded tree never reuses a number), `metadata.rs`, `files.rs` (the only place a node file name is spelled), `agents.rs` (sub-agent runs, in `<node dir>/agents/<index>/`, never nodes), `toc.rs`, `test_support.rs` (`temp_root`, `write_node`, `tree_with_node`, for the tests); imports nothing from `agent/` or `tui/`
- `src/signal.rs` — `AppSignal`, `MainTx`, `send` (awaits in a task of its own) and `try_send` (drops rather than wait); every thread reaches the main loop through it
- `src/util/` — `time.rs` (UTC stamps, civil-date math, local offset), `fs.rs` (`write_atomic`, `repo_hash`, `data_dir`, `config_dir`), `text.rs` (word bounds, `split_metadata`, `key_values`), `process.rs` (`kill_group`, `GroupGuard`: the one place that calls `killpg`), `http.rs` (`get`, the one request timeout)
- `src/agent/mod.rs` — `Mode` (feedback/commit, how a turn runs and closes, never a window setting), `AgentConfig` (`resolve` joins settings with tokens; an endpoint without a token is logged and skipped), `Backend`, transcript `Message`
- `src/agent/turn/` — a turn off the UI thread: `mod.rs` (`TurnRequest`, `spawn`, `TurnHandle`, `report_usage`, the spinner frame), `live.rs` (`LiveTurn`, `Recorder`, `Injector`: what a window reads at render time and what is written to disk), `close.rs` (what a turn does to the working tree when it ends), `title.rs` (post-turn title and summary)
- `src/agent/llm.rs` — the only file importing `genai`: streaming chat with tool calls, retried with backoff while nothing has streamed
- `src/agent/agent_loop.rs` — our own agent loop for OpenAI-compatible endpoints: time-budget nudges, `StallDetector` for repeated tool results; `run` returns a boxed future since the `agent` tool runs it again
- `src/agent/subagent.rs` — sub-agents of own-loop turns: the `agent` tool, `Role` (`<config_dir>/agents/<name>.md`), `Subagents`, `run`; `conclude` is the one funnel every run ends through; a sub-agent never starts another
- `src/agent/tools.rs` — agent tools, same in both modes; file paths are checked lexically against the repository root (`util::fs::resolve_in`, symlinks not resolved) and `shell` only starts there, unconfined; `read` never splices a window at a byte in its middle
- `src/agent/mcp.rs` — MCP servers of the repository: `McpHost`, a hand-written stdio client, `snapshot` for a turn's tools; each server in its own process group
- `src/agent/claude_code.rs` — the `claude -p` driver: arguments (always `--strict-mcp-config`, so a run sees only the repository's MCP servers), `stream-json` parser, session forking
- `src/agent/context.rs` — system prompts (from `prompts/*.md`), ancestor replay and every note Explorer injects; whatever is sent to the model must be recorded in `transcript.jsonl`
- `src/agent/commands.rs` — slash commands (`/compact`, `/commit`, `/continue` built in, custom ones from `<config_dir>/commands/`)
- `src/agent/models.rs` — `GET <url>/models`
- `src/agent/fixtures/` — `claude_stream.jsonl`, a recorded `claude -p` stream that the parser's tests in `claude_code.rs` read
- `src/tui/mod.rs` — module declarations; `test_support.rs` is the shared `AppState` and `GlobalData` test setup
- `src/tui/app/` — the `r3bl_tui` `App` impl: `mod.rs` (`AppMain`, one dispatcher per trait method), `init.rs` (components, background services), `input.rs` (key and mouse routing), `signals.rs`, `render.rs`, `status_bar.rs`, `tick.rs` (timers and housekeeping)
- `src/tui/state.rs` — `AppState`, the one state every component reads; inputs and selections live here beside what they edit, never in a component (a component belongs to a pane slot and a window changes slot whenever the stack moves)
- `src/tui/focus.rs` — `focus_window`, `close_window` and terminal focus notification: the shared helpers panes call instead of importing `app/`
- `src/tui/pane_manager.rs` — `PaneManager` and `Window`: focus, resize, stacking and the frame's one layout (`relayout`, run once per event and once per frame; nothing else recomputes it)
- `src/tui/pane_component.rs` — the component of a pane slot: a stack of windows with tab headers, routing mouse and commands to the active window
- `src/tui/panes_renderer.rs` — paints the visible panes of the frame
- `src/tui/keymap.rs` — configurable shortcuts: `Action`, `Keymap`, key-spec parsing, status-bar hints
- `src/tui/command_line.rs` — the global command line (`<leader>:`) and `COMMANDS`, the one table of what it runs and what the palette lists
- `src/tui/after_agents.rs` — the `after-agents` command: a one-shot shell command after the queue empties, cancelled by any input; killed with its process group when dropped
- `src/tui/session.rs` — session save/restore: trust, MCP servers, aliases, the window stack and each window's own state; the one place that reaches into every pane
- `src/tui/symbols.rs` — symbol highlight groups' bookkeeping, in `FileKey`s alone
- `src/tui/theme.rs` — `HelixTheme`, loaded from the bundled TOML files via `include!("../../themes/themes.rs")`
- `src/tui/panes/` — one window kind each: `preview.rs` (file preview with syntect highlighting and LSP tokens), `git_log_pane.rs` (log with live filter and branch picker), `commit_diff_pane.rs`, `report_pane.rs` (markdown in memory, nothing saved), `terminal/` (`mod.rs` PTY emulation and selection, `spawn.rs` start, reader task and `Throttle`), `pickers/` (`window_picker`, `model_picker`, `theme_picker`); a pane never imports `app/`; what panes share lives in `focus` and `state`
- `src/tui/panes/discussion/` — a conversation window (`Window::Discussion(id)`; with `DiscussionState.agent` a read-only view of a sub-agent run): `mod.rs` (component, lifecycle, title bar), `launch.rs` (start a turn), `queue.rs`, `document.rs` (the laid-out document and its `DocCache`), `selection.rs` (marks and `locate`), `status.rs`; `drop_queue_entry` must be called wherever a Discussion window is removed
- `src/tui/panes/conversation_tree/` — the Conversation Tree window: `mod.rs`, `layout.rs` (`rows`, the one place rows and filter matches are built), `filter.rs` (`FilterCache`, keyed by query and tree `revision`), `input.rs`, `render.rs`, `test_support.rs` (the trees the tests build)
- `src/tui/widgets/` — reusable pieces with no window of their own: `fuzzy.rs` (the one fuzzy-matching loop; a haystack is the text's chars, never `Utf32Str::new`), `fuzzy_picker.rs`, `input_line.rs` (`InputLine`, text and cursor, soft-wrapped), `markdown.rs` (markdown to themed `MdLine`s; each `Span` carries its source byte offset), `text_layout.rs` (`wrap`, `truncate`, `display_width`), `title_row.rs`

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

# Lint (skips vendored r3bl dependencies; --all-targets covers the tests too)
cargo clippy --no-deps --all-targets

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

### Running without rust-analyzer

Every Explorer instance spawns its own rust-analyzer. When you start Explorer to test
something that does not need syntax colors or symbol highlights, set `EXPLORER_NO_LSP`
(any value but `0`, `false` or `off`) so the run does not stack another one:

```bash
EXPLORER_NO_LSP=1 cargo run --release
```

Previews then stay plain text, symbol highlights do nothing, and `features`,
`all-features` and `default-features` are refused. Prefix every run command with it:
a shell tool call does not keep exported variables for the next one.

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
3. Run `cargo clippy --no-deps --all-targets` and address any warnings before merging. Without `--all-targets` the test modules are never linted, so a dead import there goes unnoticed.

---

## Dependencies

| Crate                      | Version | Purpose                                                                                    |
| -------------------------- | ------- | ------------------------------------------------------------------------------------------ |
| `arc-swap`                 | 1.x     | Lock-free atomic swap for the shared file list (`Arc<ArcSwap<Vec<Arc<LoadedFile>>>>`)      |
| `camino`                   | 1.x     | UTF-8–typed path types (`Utf8PathBuf`, etc.)                                               |
| `directories`              | 5.x     | XDG Base Directory resolution for config and session storage                               |
| `dprint-core`              | 0.71.x  | Newline configuration of the markdown formatter                                            |
| `dprint-plugin-markdown`   | 0.28.x  | In-process markdown formatting (`src/markdown_format.rs`)                                  |
| `futures-util`             | 0.3.x   | Reading `genai`'s chat stream                                                              |
| `genai`                    | 0.6.x   | LLM wire client for OpenAI-compatible endpoints (only in `src/agent/llm.rs`)               |
| `gix`                      | 0.87.x  | Git data (log walk, tree diff, blob unified diff); pure Rust                               |
| `hex`                      | 0.4.x   | Hex encoding for session filename hashes                                                   |
| `jwalk`                    | 0.8.x   | Parallel directory traversal (uses rayon)                                                  |
| `kdl`                      | 6.x     | KDL config file parsing                                                                    |
| `libc`                     | 0.2.x   | PTY/OS-level syscalls used by terminal emulation                                           |
| `lsp-types`                | 0.97.x  | Typed LSP protocol structs (with `proposed` feature)                                       |
| `miette`                   | 7.x     | Error reporting                                                                            |
| `notify`                   | 8.x     | Filesystem event watching (used in `watcher.rs`)                                           |
| `nucleo`                   | 0.5.x   | Fuzzy matching for paths and file content                                                  |
| `parking_lot`              | 0.12.x  | Poison-free `Mutex`/`RwLock` primitives (used for shared panes, file data)                 |
| `pico-args`                | 0.5.x   | Lightweight CLI argument parsing (no proc macros)                                          |
| `r3bl_tui`                 | 0.7.x   | TUI framework with Linux-native `direct_to_ansi` backend, PTY/terminal-multiplexer support |
| `reqwest`                  | 0.13.x  | Model list fetch from OpenAI-compatible endpoints                                          |
| `serde`                    | 1.x     | Derive macros for serialization (`derive` feature)                                         |
| `serde_json`               | 1.x     | JSON-RPC message serialization for LSP protocol                                            |
| `sha2`                     | 0.10.x  | Stable repo-root hashing for session filenames                                             |
| `toml`                     | 0.8.x   | Theme file parsing (`themes/*.toml`)                                                       |
| `tokio`                    | 1.x     | Async runtime required by `r3bl_tui`                                                       |
| `tracing`                  | 0.1.x   | Structured logging macros (`debug!`, `info!`, etc.)                                        |
| `tracing-core`             | 0.1.x   | `LevelFilter` type used to configure `r3bl_tui` logger                                     |
| `unicode-general-category` | 1.x     | General category lookup for the scan rules                                                 |
| `unicode-segmentation`     | 1.x     | Word boundary detection in `input_line`                                                    |
| `url`                      | 2.x     | URL parsing in `word_bounds` for cursor-based URL selection                                |

Planned feature areas and their likely dependencies:

- **Git tooling** (remaining: status, blame — intended to replace `tig`; log and diff have shipped)
- **AI tooling** integration: the conversation-tree agent chat has shipped; what remains is an architecture-walking tool and an in-repo export of the tree

---

## Code Style

### Formatting

- `cargo fmt` is authoritative. Do not hand-format; let rustfmt decide.
- Edition 2024 is in use. Use its idioms (e.g., `let … else`, `if let` chains).

### Naming Conventions

- Types and traits: `UpperCamelCase`
- Functions, methods, variables, modules: `snake_case`
- Constants and statics: `SCREAMING_SNAKE_CASE`
- No Hungarian notation. Name by what a thing _is_, not its type.

### Imports

A name is imported as close as possible to where it is used, never listed at the top of a file. This keeps the dependency visible in the code that has it, and out of sight elsewhere.

- **At module level** (function signatures, `impl` headers, struct and enum fields, type aliases, constants, statics), `use` is not available: spell the path out in full. `crate::…` for this crate, `<crate>::…` for a dependency. The three exceptions, which may stay at the top of a file, are `use std::…`, `use core::…`/`use alloc::…` and `use r3bl_tui::*;`.
- **Inside a function or a block**, `use` freely, at the start of the narrowest block that needs the names. A trait's methods need the trait in scope: import it in the function that calls them, e.g. `use std::io::Write;`.
- **Types are imported, functions are not.** A type of this crate (a struct, enum, trait, type alias or union) is imported inside the body that names it and written short from there on — `Endpoint`, `EndpointKind`, `NodeStatus`, including in a struct literal, a pattern or an associated call. An enum is a type like any other: import it and write `Window::FilePreview`, not the full path. Write the variant alone (`FilePreview`) only where the body imports two enums that share it; there the pair `Mode::Commit` / `NodeType::Commit` is what tells them apart. Never import a single variant (`use crate::git::RefKind::Branch;`): import the enum and write `RefKind::Branch`, which cannot be confused with another enum's `Branch`. A function, method or constant keeps its `crate::…::` prefix at the call site: that is where the dependency on a module has to stay visible, and where one grep for `crate::tui::app::` still finds it. So `crate::tui::state::AppState` may appear in a signature and `AppState` in the body right below it: that difference is the rule, not an accident.
- **Always spell paths from `crate::`**, never `super::` or `self::`, so one grep finds every dependency on a module. The exception is `use super::*;` in a `#[cfg(test)]` module.
- **`r3bl_tui` is the one glob**, in tui code, because it is the UI's shared vocabulary: `GlobalData`, `InputEvent`, `FlexBox`, `RenderOpCommon`, the `throws!` family and `new_style!` appear in nearly every signature. Mark each of those lines `#[allow(clippy::wildcard_imports)]`. It brings `HashMap`/`HashSet` with it, aliased to the Fx hashers: a module that wants the std ones keeps its `use std::collections::{HashMap, HashSet};`.
- **Never a glob otherwise**, including `use Window::*` for enum variants: a misspelled variant would bind instead of failing to compile.
- **No re-exports.** Each item has one canonical path; `crate::tui::state::AppState`, never `crate::tui::AppState`.
- **Call this crate's trait methods by full path** with an explicit receiver: `crate::tui::title_row::TitleRow::render_title_row(&mut pane, …)`. Do the same for r3bl's `Component` when a parent dispatches to a child. std and other external traits keep method syntax: `Arc::clone(&x)`, never `x.clone()` on a shared pointer.
- `cargo xtask check-imports` enforces the module-level rule, the `super::` rule, the allowed globs, the full-path variant inside a body and the single-variant import. `clippy::wildcard_imports` and `clippy::clone_on_ref_ptr` are warnings in `src/main.rs`.

### Paths

- **Always** use `camino::Utf8PathBuf` / `camino::Utf8Path` for paths stored in structs or passed between functions.
- Use `std::path::PathBuf` only at FFI/OS boundaries (e.g., values returned by `std::env::current_dir` or `jwalk`), and convert to `Utf8PathBuf` as early as possible via `Utf8PathBuf::from_path_buf(p).ok()?`.
- For filesystem _name comparisons_ (not full paths), use `OsString` / `OsStr` directly — do not convert to `String` just to compare.

### Error Handling

- In functions that return `Option<T>`, propagate failures with `?` and return `None` for expected failure modes (file unreadable, non-UTF-8 path).
- Use `.expect("message")` only for conditions that are truly unrecoverable programmer errors (e.g., `current_dir()` failing). The message should state _what invariant was violated_.
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
- Scrolling a terminal pane ungrabs it. Clicking the pane or pressing Enter or `q` re-grabs.
- When removing a terminal window via `remove_window`, only reset `terminal_grabbed` if the window being removed is the currently focused one — otherwise a background pane exit silently ungrabs an active terminal.

### Conversation Agent

- A running turn shares its `LiveTurn` behind a `parking_lot::Mutex` with the render path. Never lock it twice in one expression (two `live.lock()` in the same struct literal deadlock the turn task and then the UI); take what you need under a single guard.
- `transcript.jsonl` is what child nodes replay. Anything sent to the model must be recorded there, including the notes and nudges Explorer injects, even when the user never sees them. Only own-loop nodes have one: a `claude-code` node replays through its `session-id` (a feedback child forks it, a commit child opens with the ancestors as plain text, a continue child forks it, so two resumes of one node are independent branches).
- Selections and highlights in a Discussion window are byte ranges of the node's `prompt.md` and `reply.md`, never screen positions: rows are laid out again at every width. A span of the document maps a cell to the source only through its `src`; keep it `None` for text that is not in the source as is.
- `prompt.md` is the prompt as sent to the model, after slash-command expansion; `input.md` holds the text as typed when a custom command or `/commit` was expanded. Give the user their text back through `conversation::files::read_input`, never from `prompt.md`, and never send `input.md` to a model.
- `reply.md` holds only the text of answers that called no tools. The text of an answer that called tools, and one line per tool call, go to `activity.md`, the work log. It is display-only: never replay it and never send it to a model.
- Never pass `--bare` to `claude`: it disables the subscription login.
- A queued prompt is never copied: the queue holds the window, and the text stays that window's `draft` until its turn starts. Nothing reaches the conversation tree or the disk before `start_turn`. The queue is released only from the main thread, after the finished turn's status is written to its node.
- A message sent to the window of a turn that is running is not queued: it joins that turn (`LiveTurn.injected`, drained by `agent_loop::run_loop` at the top of every round, or written to the CLI's stdin by `claude_code::run`), and the turn's prompt block, `prompt.md` and `transcript.jsonl` carry it as a user message that is not `injected`. It enters the conversation at a round boundary, so a turn whose answer called no tools keeps going to read it. What the turn never read goes back to the input when it ends (`restore_injected`), and `Esc` takes it back before it cancels. A Claude Code turn whose stdin is closed (`LiveTurn.closed`, set by `Injector::close_if_idle` once a result followed the last write, or by `Injector::close` when a write to it fails) takes no more messages. Only a message for another turn — another window's node, or a turn that no longer takes them — is queued.
- A window has no mode: a prompt is a feedback turn unless it is `/commit` or a custom command with `system-prompt: off`, so the mode travels with the typed text (queue, `input.md`, restored prompts).
- A custom command with `system-prompt: off` gets no mode prompt and no workspace note, so its turn must never be reset: it runs and closes as a commit turn (`TurnKind::NoSystemPrompt`), and its node records `system-prompt: off` for replays. Both modes have the same tools; what differs is the system prompt and how the turn is closed.
- A sub-agent starts from its task alone: never give it the caller's context, and never replay a sub-agent run as part of a node (its `transcript.jsonl` serves only a resumed run of the same chain). It has every tool but `agent`, runs inside its caller's turn, and leaves the closing to it: the sub-agent of a feedback turn gets the workspace note and must not commit either.
- Agent-side child processes (the `claude` CLI, `shell`, MCP servers) run on `tokio::process`, never on RRT: the agent awaits their replies, and RRT suits blocking sources broadcasting to the UI (LSP, watcher).
- MCP servers outlive turns, so what a server holds (egui-mcp's `attach`) carries over; a turn sees the tools of the servers running when it started, and never forwards images to the model.
- A background `shell` command never outlives its turn: the turn's `Background` is dropped, killing its process groups, before the turn is closed (the tree is reset), and the `read` tool stays confined to the repository, so the agent reads `<node dir>/processes/<n>.log` through `shell`.
- No turn runs in a repository the user has not trusted (`AppState.trusted`, saved in the session, set by the command line's `trust`/`untrust`, and shown in the Conversation Tree's title). A refused turn says so and names `scan` then `trust`. A report is not a turn and never touches the conversation tree.
- Every prompt requires a clean working tree, and only one turn runs at a time; a prompt sent meanwhile is queued, unless it is for the window of the turn that is running, which takes it into the loop. Every turn ends with a hard reset of the tree: a feedback turn's scratch work and a commit turn's leftovers are both discarded, so nothing of a turn survives outside the commits it made. A commit they make in a feedback turn is not recorded to the conversation, so feedback turns must not commit.
- Subscription usage is only known for `claude-code` (pushed by the CLI while a turn runs), `opencode-go`, `kimi-code` and `cline` (asked once a turn has ended); an OpenRouter key's spending is polled at startup and every minute between turns, with the account's credit when the key has no limit. OpenCode Zen is another product with another accounting: do not treat an endpoint as `opencode-go` because of its host. A new provider gets its own `EndpointKind`, never a probe of `<url>/usage` on plain `openai` endpoints.
- The system prompt holds nothing that changes between turns. The repository state (root, date, HEAD) opens the user message as an Explorer note, together with the compaction and repository-moved notes: HEAD moves on every commit, and the endpoints cache on the longest common prefix, so a change ahead of the replayed ancestors throws the cache of the whole transcript away.

### Comments

- Write no comments by default.
- Add a comment only when the _why_ is non-obvious: a hidden constraint, a subtle invariant, a workaround for a specific external bug.
- Never describe _what_ the code does — well-named identifiers already do that.
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
