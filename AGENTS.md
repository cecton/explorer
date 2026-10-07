# AGENTS.md — Explorer

This file provides guidance for agentic coding assistants operating in this repository.
The project is in very early development — don't hesitate to make breaking changes and
don't write migration or retro-compatibility code.

Every change you make must be committed with a clear title and description. Run
`cargo fmt` before every commit. Always use the git skill before committing.

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
  Explorer saves and restores *everything that was running* — pane layout, open files,
  live terminals, and highlights (see `src/session.rs`) — so a work session resumes
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

`explorer` is a Rust TUI workspace built around a git repository. Its file-explorer
core:
1. Locates the nearest `.git` root from the current directory.
2. Walks the entire repository tree in parallel (via `jwalk` + rayon), skipping `.git` and `target`.
3. Loads every UTF-8 text file into memory as a `LoadedFile` struct.
4. Displays files in an interactive TUI: left pane is the file list, right pane is a syntax-highlighted preview.

Source is organized as:
- `src/main.rs` — async entry point, wires CLI → loader → TUI
- `src/cli.rs` — CLI argument parsing via `pico-args`
- `src/config.rs` — KDL configuration file parsing (theme, `keybindings` block, `counters` block, `agent` block with endpoints, `summary-model` and custom slash commands); `load_auth` reads endpoint tokens from `<data_dir>/auth.kdl` and `resolve_agent` joins them (an endpoint without a token is logged and skipped)
- `src/conversation.rs` — the agent conversation tree: `NodeId` (a node's directory name), `Node`, `Metadata` (flat `key: value` `metadata.md`), `ConversationTree` (metadata, titles and summaries only; prompts, replies and transcripts stay on disk), node creation/restart/delete, `turn_context` (what a node's turn replays, with the compact cut-off), table-of-contents pages and commit lookup for the agent's `conversation` tool
- `src/agent/mod.rs` — `Mode` (feedback/commit), `ModelSpec` (`endpoint/model/effort`), `Endpoint`/`EndpointKind`, `Backend`, transcript `Message`
- `src/agent/turn.rs` — turn lifecycle off the UI thread: `spawn` (with a `TurnProgress` redraw every `REDRAW_INTERVAL` while the turn runs), `spinner_frame` (shared by the tree marker and the Discussion status line), `Recorder` (live reply and work log, `reply.md`, `activity.md`, `transcript.jsonl`), commit-turn closing (commit range, hidden nudge, stash), the post-turn title and summary (one commit-message-style call)
- `src/agent/llm.rs` — the only file importing `genai`: streaming chat with tool calls, one-shot completion
- `src/agent/agent_loop.rs` — our own agent loop for OpenAI-compatible endpoints
- `src/agent/tools.rs` — agent tools (`conversation`, `read`, `glob`, `grep`, `shell`, plus `write`/`edit` in commit mode), confined to the repository root
- `src/agent/claude_code.rs` — `claude -p` driver: arguments, `stream-json` parser, session forking
- `src/agent/context.rs` — system prompts (from `prompts/*.md`), ancestor replay, handover preamble, the notes Explorer injects
- `src/agent/commands.rs` — slash command expansion (`/compact` built in)
- `src/agent/models.rs` — `GET <url>/models`
- `src/git.rs` — gix-backed git data: `load_log` (first-parent walk of HEAD or a resolved branch/tag ref, capped at `MAX_LOG_ENTRIES` entries in a `LogHistory`, plus `refs`: commit → branches/remote branches/tags map built by peeling every ref), `load_refs` (flat branch/tag list for the branch picker), `load_commit_diff` (unified text), `LogEntry`, `GitRef`/`RefKind`, `DiffLine`/`DiffKind`, `commit_type_prefix` (conventional-commit parsing), datetime + short-id helpers
- `src/keymap.rs` — configurable top-level shortcuts: `Action` enum, `Keymap` mapping `KeyPress` → action per leader/global context, key-spec parsing, status-bar hint rendering; `<leader>Backspace` (`Action::SendToBack`, dispatched in `app.rs`) sends the focused window to the back; `<leader>f` (`Action::ToggleFavorite`, dispatched in `app.rs`) toggles the favorite of the node in focus: the one a Discussion window shows or the selected row of the Conversation Tree
- `src/loader.rs` — parallel file walking and `LoadedFile` construction
- `src/session.rs` — session save/restore (pane stack incl. git log windows with branch + query, terminals, highlights of files and of conversation nodes)
- `src/lsp.rs` — LSP client (JSON-RPC over stdio); public API: `send_file_request`, `health_check`, `try_drain_pending_requests`, `LspHealth` enum
- `src/watcher.rs` — filesystem watcher via `notify`; debounces events into `BatchedWatchEvent` and broadcasts via `WATCHER_RRT`
- `src/tui/mod.rs` — module declarations
- `src/tui/title_row.rs` — `TitleRow` trait, `render_pane_title` utility, `title_bar_colors`
- `src/tui/state.rs` — `AppState` (with global `TextSelection`, `SelPoint`, `terminal_grabbed`, `PaneManager`, `discussion_highlights`: `NodeHighlights` by node, `queued_turns`: `PendingTurn`s waiting for the running turn to end, in memory only), `AppSignal`, `TerminalPane`, `Window`, `WindowState`, and `FuzzyPickerState` types
- `src/tui/app.rs` — `App` trait impl, layout, `run()` entry point; handles global text selection, terminal grab/ungrab, mouse event routing, and PaneManager-driven layout
- `src/tui/pane_manager.rs` — `PaneManager` type: focus, resize, stacking, and layout of 16 pane slots; responds to `PaneCommand` enums for directional movement and resize operations
- `src/tui/pane_component.rs` — `PaneComponent` implementation: renders a stack of overlapping windows within a pane slot with tab-style headers; manages per-pane state and command routing to child components
- `src/tui/window_picker.rs` — window picker overlay (`Window::WindowPicker`, `<leader>space`): while the query is empty, every open window but the pickers in stack order (`open_windows_results`, rebuilt at each render; holds only `Window` values, titles are resolved for the visible rows), each row titled like its window's title bar and colored by kind (`row_accents`: terminal, agent, git); fuzzy file-name matches once a query is typed; `Ctrl+X` closes the selected row's window through `close_window` in `app.rs`, shared with `<leader>x` (exceptions → input → navigation)
- `src/tui/fuzzy_picker.rs` — shared fuzzy list picker component; navigation via flat `match` on `InputEvent`
- `src/tui/git_log_pane.rs` — git log pane component; multiple windows (`Window::GitLog(id)`, state in `AppState.git_logs`); renders async-loaded log entries for HEAD or the window's chosen branch, arrow/page/home/end/mouse selection, `/` live filter via nucleo (`run_log_match`/`apply_filter`; ENTER validates, Esc clears search, Esc outside search is a noop), `b` in-window branch picker (fuzzy `run_ref_match` over all branches and tags, prompt `Show branch:`; ENTER loads that branch, Esc returns to HEAD), conventional-commit type coloring, 3-space date gap, right-aligned ref decoration per row (`layout_refs`: branches then tags, drop from the right with `...` in the dropped group's color), `Logs` title with `(branch)` segment, `(10000+)` when truncated, ` — <query>` suffix while a filter is validated), Enter opens a `CommitDiff` window (seeding `state.commit_diffs` and spawning `load_commit_diff` if absent)
- `src/tui/commit_diff_pane.rs` — commit diff pane component; renders a colored unified diff for one commit (`+` green, `-` red, `@@` yellow, headers cyan); Backspace sends the window to back, Esc closes it
- `src/tui/conversation_tree_pane.rs` — Conversation Tree window (`Window::ConversationTree`, `<leader>c`): `layout_rows` (roots oldest first, the newest selected by default; a thread, the straight run of single replies below a root or a branch, folds into its head and is collapsed until expanded, while the fork ending it stays visible; fork-only indentation; the first row below a root gets `└─`; `TreeRow.rails` records which forks above still have branches below, driving the `│`/`├─`/`└─` prefix and its continuation under the selected row), fold column (`+`/`-`, leaf `─`; a fork's arms and rail sit in its column), `state_marker` in the gutter after the date (braille spinner while running, `✗` failed, `■` cancelled, `±` stashed, `≡` compact; `ui.linenr` rails, `ui.linenr.selected` on the selected row), selected node expanded with title and summary, `Right` on a thread head opens it and selects its last reply (`thread_end`), `folds_on_the_way` (what `start_turn` expands), `f` favorite, `Ctrl+D` twice to delete, `Enter`/double click open-and-recover, `n` new root, `m` replay with another model, `e` edit as sibling, `c` commit diffs, Backspace sends the window to back
- `src/tui/discussion_pane.rs` — Discussion window (`Window::Discussion(id)`, state in `AppState.discussions`): prompt as plain wrapped text, reply as markdown, dimmed work log above the reply (shown while the node has no finished reply, `Ctrl+O` toggles it), status line (led by the spinner while the turn runs), multi-row input, `Shift+Tab` mode toggle, `Ctrl+P` model, `Esc` takes a queued prompt back or else cancels the running turn (`Ctrl+C` only clears the input), `Ctrl+Plus`/`Ctrl+Minus` move the queued prompt in the queue, `<leader>f` toggles the favorite of the node shown (`title` prefixes a favorite with `* `, in the title bar and the window picker); `<leader>Backspace` sends the window to the back (plain Backspace edits the input, so the chord goes through the keymap); mouse drags over the document, from word to word across rows (`locate`, `drag_range`): plain drag selects and copies the source text on release, `Shift`+drag adds a highlight and `Ctrl`+drag removes one (`AppState.discussion_highlights`, byte ranges of the prompt and the reply; refused while the turn runs); `start_turn` checks the guards (one turn at a time, clean tree), creates the node and spawns the turn, or returns a typed `Refusal`; `send` queues the window on `Refusal::Busy` (`AppState.queued_turns`), `compose_launch` builds the turn from the window as it stands (sibling of a failed or cancelled node), `release_queue` starts the queue when a turn ends, `restore_prompt` gives a failed or cancelled prompt back to the input, `drop_queue_entry` must be called wherever a Discussion window is removed
- `src/tui/markdown.rs` — `render`: markdown (`pulldown-cmark`) to theme-colored `MdLine`s of styled `Span`s fitted to a width; headings, emphasis, code, quotes, lists, links, rules, and tables (aligned columns with wrapped cells, stacked `Header: value` records when the columns cannot fit); each `Span` carries `src`, the byte offset of its text in the source, `None` for what the renderer adds or rewrites
- `src/tui/model_picker.rs` — Model Picker window (`Window::ModelPicker`): fuzzy list of `endpoint/model/effort`, for a window's next prompt or to duplicate a node; `fetch_models`
- `src/tui/text_layout.rs` — `wrap` (each line with its byte offset in the text), `truncate`, `display_width`
- `src/tui/theme.rs` — `HelixTheme` type; loads bundled TOML files via `include!("../../themes/themes.rs")`
- `src/tui/theme_picker.rs` — theme picker overlay with fuzzy search and live preview (exceptions → input → navigation)
- `src/tui/preview.rs` — `FilePreviewComponent` with syntect syntax highlighting (right pane); text selection uses global `AppState`
- `src/tui/terminal_pane.rs` — `TerminalPaneComponent` with PTY-based terminal emulation; `render_ofs_buf_to_ir()` emits `ResetColor` for `Spacer` runs to prevent stale SGR inheritance; supports mouse selection, word/line text extraction, and clipboard copy; synchronized output (DEC private mode 2026) is buffered natively by `OfsBufVT100` in r3bl-open-core
- `src/tui/input_line.rs` — query input with Emacs-style key bindings; single flat `match` on `InputEvent`; soft-wraps over several rows, `Up`/`Down` between rows, height cap with internal scroll

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
| `futures-util`         | 0.3.x   | Reading `genai`'s chat stream                                  |
| `genai`                | 0.6.x   | LLM wire client for OpenAI-compatible endpoints (only in `src/agent/llm.rs`) |
| `gix`                  | 0.87.x  | Git data (log walk, tree diff, blob unified diff); pure Rust   |
| `hex`                  | 0.4.x   | Hex encoding for session filename hashes                       |
| `jwalk`                | 0.8.x   | Parallel directory traversal (uses rayon)                      |
| `kdl`                  | 6.x     | KDL config file parsing                                        |
| `libc`                 | 0.2.x   | PTY/OS-level syscalls used by terminal emulation               |
| `lsp-types`            | 0.97.x  | Typed LSP protocol structs (with `proposed` feature)           |
| `miette`               | 7.x     | Error reporting                                                |
| `notify`               | 8.x     | Filesystem event watching (used in `watcher.rs`)               |
| `nucleo`               | 0.5.x   | Fuzzy matching for paths and file content                      |
| `parking_lot`          | 0.12.x  | Poison-free `Mutex`/`RwLock` primitives (used for shared panes, file data) |
| `pico-args`            | 0.5.x   | Lightweight CLI argument parsing (no proc macros)              |
| `r3bl_tui`             | 0.7.x   | TUI framework with Linux-native `direct_to_ansi` backend, PTY/terminal-multiplexer support |
| `reqwest`              | 0.12.x  | Model list fetch from OpenAI-compatible endpoints              |
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
- **Git tooling** (remaining: status, blame — intended to replace `tig`; log and diff have shipped). See `docs/superpowers/specs/2026-09-21-git-log-pane-design.md`
- **AI tooling** integration: the conversation-tree agent chat has shipped (see `docs/superpowers/specs/2026-10-05-conversation-agent-design.md`); its Future work section lists what remains (architecture-walking tool, tree filter, in-repo export)

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
- Scrolling a terminal pane ungrabs it. Clicking the pane or pressing Enter or `q` re-grabs.
- When removing a terminal window via `remove_window`, only reset `terminal_grabbed` if the window being removed is the currently focused one — otherwise a background pane exit silently ungrabs an active terminal.

### Conversation Agent

- A running turn shares its `LiveTurn` behind a `parking_lot::Mutex` with the render path. Never lock it twice in one expression (two `live.lock()` in the same struct literal deadlock the turn task and then the UI); take what you need under a single guard.
- `transcript.jsonl` is what child nodes replay. Anything sent to the model must be recorded there, including the notes and nudges Explorer injects, even when the user never sees them. Only own-loop nodes have one: `claude-code` nodes replay through their `session-id`.
- Selections and highlights in a Discussion window are byte ranges of the node's `prompt.md` and `reply.md`, never screen positions: rows are laid out again at every width. A span of the document maps a cell to the source only through its `src`; keep it `None` for text that is not in the source as is.
- `prompt.md` is the prompt as sent to the model, after slash-command expansion; `input.md` holds the text as typed when a custom command was expanded. Give the user their text back through `conversation::read_input`, never from `prompt.md`, and never send `input.md` to a model.
- `reply.md` holds only the text of answers that called no tools. The text of an answer that called tools, and one line per tool call, go to `activity.md`, the work log. It is display-only: never replay it and never send it to a model.
- Never pass `--bare` to `claude`: it disables the subscription login.
- A queued prompt is never copied: the queue holds the window, and the text stays that window's `draft` until its turn starts. Nothing reaches the conversation tree or the disk before `start_turn`. The queue is released only from the main thread, after the finished turn's status is written to its node.
- Every prompt requires a clean working tree, and only one turn runs at a time; a prompt sent meanwhile is queued. Feedback turns end with a hard reset of the tree, and a commit they make is not recorded to the conversation, so they must not commit; commit turns end with the agent's leftovers stashed.

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
