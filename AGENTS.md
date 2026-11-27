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
4. Shows it as a stack of windows laid out by `PaneManager` (`src/tui/pane_manager.rs`): a `Window` is a file preview, a terminal, a git log, a commit diff, the conversation tree, a discussion or a report. File or window is not listed anywhere — the Window Picker (`<leader>space`) opens one by fuzzy name. Pickers are modals, painted above the panes, not windows.

Source is organized in layers. A module imports only from its own layer or a lower one, never from one above (`config.rs` names `tui::keymap` in a doc comment only):

| Layer                               | Modules                                                                                                                                                                                                                                                                                         |
| ----------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1. `util/`                          | `time`, `fs`, `text`, `fuzzy`, `process`, `http`, `test_support`: helpers every layer shares, importing no other module of the crate                                                                                                                                                            |
| 2. Core data, services and `signal` | `cli`, `git`, `loader`, `scan`, `sandbox`, `markdown_format`, `knowledge`, `config`, `conversation/`, `endpoint`, `model_spec`, `usage`, `highlight`, `lsp/`, `watcher`, `signal` (`AppSignal`, the event vocabulary every thread speaks; core threads send it, so its payloads are core types) |
| 3. `agent/`                         | turns, the agent loop, sub-agents, tools, MCP, slash commands                                                                                                                                                                                                                                   |
| 4. `tui/`                           | everything on screen, plus `session`                                                                                                                                                                                                                                                            |

`loader/watch.rs` is the one module outside `tui/` and `main.rs` that calls into `lsp` (`send_file_request`, to tell it a file changed); `lsp/tokens.rs` reads `loader::LoadedFile`. Keep it that way.

- `src/main.rs` — entry point (sync `main` dispatches the sandbox subcommands, async `run` is the app), wires CLI → loader → TUI
- `src/cli.rs` — CLI argument parsing via `pico-args`: `agent` runs the own agent loop of the turn that started it (see `src/agent/child.rs`; `main` dispatches it before any repository lookup), `--scan` prints the scan report and exits, `fmt-md [paths]` (first argument only) formats markdown and exits, `mcp-serve [root] [--own-tools-only] [--privileged-socket <path>] [--knowledge-read-only]` serves Explorer's own agent tools over stdio and exits (see `src/agent/serve.rs`), `sandbox-exec --root <repo> [--workspace <dir>] [--ro <path>]... [--check] -- <cmd>` runs a command (`--check` only sets the namespaces up and exits, which is how a turn learns the sandbox does not work here) with the workspace, if any, mounted over the repository and each `--ro` path read-only (`EROFS`) (see `src/sandbox.rs`; `main` handles it and its `sandbox-child` re-exec before building the tokio runtime, so `unshare` runs before any thread exists)
- `src/config.rs` — KDL configuration (theme, `keybindings`, the `agent` block with endpoints and models, `review-rounds`, `repeat-limit`, `parallel-copies`, the `after-agents` block, top-level `alias` lines); a bad `config.kdl` value is refused at load, never defaulted silently; `load_auth` reads endpoint tokens from `<data_dir>/auth.kdl`, where a bad file is logged and yields no token
- `src/endpoint.rs` — `Provider`, the kind of server behind an endpoint and its default URL; the one place that knows which providers report usage
- `src/model_spec.rs` — `Effort` and `ModelSpec` (`endpoint/model/effort`); names no other module
- `src/usage.rs` — subscription usage: `Usage` and its windows, one `fetch` per provider; sends nothing itself, the turn's `report_usage` does
- `src/git.rs` — gix-backed git data: the log (first-parent walk, capped at `MAX_LOG_ENTRIES`), refs, commit diffs, conventional-commit prefixes; the turn helpers shell out to `git` through `git_output`
- `src/loader.rs` — parallel walk and read of the whole repository into memory as `LoadedFile`s, so preview and search never touch the disk (do not trade this away); `IGNORED_DIRS` and `is_ignored` are the one prune rule, shared by the walk, the watcher and the trigger check; `FileKey`s stay valid because the list is append-only; `find_git_root_from` returns `None` where `find_git_root` panics, for a subcommand that must report a missing repository
- `src/loader/watch.rs` — a changed batch in two halves: `read` on the watcher thread (touches no file on the UI thread), `swap_in` on the main thread; publishing new content in the same turn that invalidates the offsets naming into it is what stops a frame painting one against the other
- `src/watcher.rs` — `notify` watcher, debounces into a `loader::watch::Batch`, reads it and sends `AppSignal::FilesChanged`
- `src/scan.rs` — deterministic scanner for hidden or misleading text aimed at an agent; its own pruning (hidden files are scanned), follows no symlink, modifies and executes nothing
- `src/sandbox.rs` — `explorer sandbox-exec`, the workspace sandbox: a user and mount namespace with the workspace bind-mounted over the repository path, so an agent's writes land in the workspace, not the repository. A guard against accidents, not confinement: the agent still reaches the rest of the host. The `--ro` remount keeps the flags the mount already has (`statvfs`), since an unprivileged remount cannot drop a locked one, and a missing path is a setup failure. `driver::command` and `claude_code::run` start the agent and the CLI through it when the turn runs in a copy of the repository, and for a commit turn, which works in the repository itself with `docs/knowledge` read-only (`Sandbox::commit`)
- `src/markdown_format.rs` — the markdown formatter (`dprint-plugin-markdown` as a library) behind `explorer fmt-md`, the `format_markdown` agent tool and the end of a commit turn; markdown is never formatted by hand
- `src/highlight.rs` — `TokenKind` and the theme-scope fallbacks of semantic tokens; here so `loader` and `lsp` share the color types without either importing the other for them
- `src/lsp/` — the rust-analyzer client on a thread of its own (`LSP_RRT`): `mod.rs` (public API, `disabled` via `EXPLORER_NO_LSP`, UTF-16 ↔ byte offsets; requests made while no worker runs are buffered), `rpc.rs` (framing), `worker.rs` (handshake, dispatch, readiness gate, restart), `tokens.rs`, `symbols.rs`; nothing on the main thread retries a request
- `src/knowledge.rs` — the knowledge base: short entries about the code, `docs/knowledge/x.md` holding its children in `x/`, served by the `knowledge` tool in bounded pages. The files are the store, read afresh on every call; a file Explorer did not lay out (its `render` through the markdown formatter does not reproduce it) is reported as malformed rather than read, and an id is only ever a path inside the store, so names that are not `[a-z0-9-]` segments (never starting or ending with a dash) and symlinks are never followed. `Access` is what a turn may do with it: a commit turn records, changes and removes entries (`write`, `delete`, `move`, `rename`, under one process-wide mutex, with the limits refused rather than trimmed and a body that would not survive the formatter refused), and every change is staged with `git add -A -- <paths>` so that the turn's own commit carries it; a feedback turn only reads, since its work is discarded when it ends and its copy stands for a repository the tool would otherwise write. `spec` is the tool's schema for each access, and `check` (in both) reports what is wrong with the base: entries that cannot be read, directories that hold children but no entry of their own, entries without `refs`, and `refs` naming a path that does not exist or a symbol no tracked file outside the store holds (looked up by the last `::` segment, all of them in one `git grep` that skips `docs/knowledge`, whose refs name every symbol)
- `src/conversation/` — the agent conversation tree: `mod.rs` (`ConversationTree`, `Node`, `TurnContext`; holds metadata, titles and summaries only, prompts, replies and transcripts stay on disk; `revision()` comes from one process-wide counter so a reloaded tree never reuses a number), `metadata.rs`, `files.rs` (the only place a node file name is spelled; an `agents` directory, which sub-agent runs were once recorded in, is never read as a node), `toc.rs`, `test_support.rs` (`write_node`, for the tests); imports nothing from `agent/` or `tui/`
- `src/signal.rs` — `AppSignal`, `MainTx`, `send` (awaits in a task of its own) and `try_send` (drops rather than wait); every thread reaches the main loop through it
- `src/util/` — `time.rs` (UTC stamps, civil-date math, local offset), `fs.rs` (`write_atomic`, `repo_hash`, `data_dir`, `config_dir`), `text.rs` (word bounds, `split_metadata`, `key_values`, `truncate_middle`), `fuzzy.rs` (`Kind`, `Query`, `ranked`: the one fuzzy-matching loop, here so a core module can rank without reaching into `tui/`; a haystack is the text's chars, never `Utf32Str::new`), `process.rs` (`kill_group`, `GroupGuard`: the one place that calls `killpg`), `http.rs` (`get`, the one request timeout), `test_support.rs` (`temp_root`, `temp_root_in_target`, `temp_repo`, `init_repo`, `commit`, `git`, for the tests of every layer)
- `src/agent/mod.rs` — `Mode` (feedback/commit, how a turn runs and closes, never a window setting), `AgentConfig` (`resolve` joins settings with tokens; an endpoint without a token is logged and skipped), `Backend`, transcript `Message`
- `src/agent/turn/` — a turn off the UI thread: `mod.rs` (`TurnRequest`, `spawn`, `TurnHandle`, `report_usage`, the spinner frame), `live.rs` (`LiveTurn`, `Recorder`, `Injector`: what a window reads at render time and what is written to disk), `close.rs` (what a turn does to the working tree when it ends), `title.rs` (post-turn title and summary), `pin.rs` (tests: what an own-loop turn records, run against the scripted server of `agent/fake_llm.rs`)
- `src/agent/llm.rs` — the only file importing `genai`: streaming chat with tool calls, retried with backoff while nothing has streamed
- `src/agent/agent_loop.rs` — our own agent loop for OpenAI-compatible endpoints: time-budget nudges, `StallDetector` for repeated tool results
- `src/agent/sink.rs` — `Sink`, what `agent_loop::run` reports and asks of its host (messages, chunks, request sizes, tool calls, the messages that joined, settling, the `agent` tool); `child.rs` and `Quiet` in `subagent.rs` implement it
- `src/agent/wire.rs` — `Start` (with `Agents`, what the run needs to call sub-agents), `Event` and `Command`, the private JSON lines `explorer agent` and its turn exchange; both ends are one binary, so the format changes with the code
- `src/agent/child.rs` — `explorer agent`: the own loop in a process of its own, reading a `Start` then `Command`s on stdin and writing `Event`s on stdout (nothing else goes there), starting its own MCP servers, running its sub-agents as nested loops of the same process, and never touching the conversation tree; SIGTERM or the end of its stdin drops the run, which kills its background commands and servers, sub-agents' included. Tests start the test binary in its place (`as_agent`)
- `src/agent/driver.rs` — the turn's side of it: spawns the process (`/proc/self/exe agent`, through `explorer sandbox-exec` when the turn is sandboxed (a feedback turn in a copy, or a commit turn with `docs/knowledge` read-only), with the parent's `--log-file` and `--log-level`, in its own process group; `ChildGuard::stop` ends it and its group, and a guard dropped without it kills the group without waiting), applies each event to the turn's `Recorder`, answers `Poll` (what the user queued, which leaves the queue only on `Injected`) and `Settle` (more messages, a commit nudge, or `Finish`)
- `src/agent/subagent.rs` — sub-agents of own-loop turns: the `agent` tool, `Role` (`<config_dir>/agents/<name>.md`), `agents` (the turn resolves each role to a model and a target, so the process needs no config; a role on a `claude-code` endpoint or one that is not configured is left out), `Subagents` (in the agent process: each call is a nested `agent_loop::run` on a sink that records nothing and keeps the final answer, with the turn's tools and background commands, rerun on the caller's model if the first run fails); nothing of a sub-agent is recorded or shown, only its answer comes back as the tool result, and it never starts another
- `src/agent/tools.rs` — agent tools, same in both modes; `knowledge` reads from `context.root`, so a sandboxed turn reads its copy; a call `privileged::serves` names (`conversation`, and a knowledge change) goes to the turn's socket when `ToolContext.privileged` is set, while a knowledge read stays local, in the view the process has; file paths are checked lexically against the repository root (`util::fs::resolve_in`, symlinks not resolved) and `shell` only starts there, unconfined; `read` never splices a window at a byte in its middle; `write` and `edit` refuse a path under `docs/knowledge/`, which only the knowledge tool writes, its changes staged
- `src/agent/mcp.rs` — MCP servers of the repository: `McpHost`, a hand-written stdio client, `snapshot` for a turn's tools, `decls` for the processes that start their own copies; each server in its own process group; `claude_config` gives every `claude-code` run the repository's servers plus Explorer's own
- `src/agent/privileged.rs` — the per-turn Unix socket (private directory under `$XDG_RUNTIME_DIR`, mode `0600`, removed with its guard, which also stops every call still running) that answers the tools only Explorer can serve, from the turn's in-memory tree or its real store: `serves` (`conversation`, and a `knowledge` change), any other call is refused, and the store of a knowledge read is the view the asking process has, so reads stay there. A tool that reads or writes Explorer's own state is privileged, one that touches the repository or the system is not; the check is the one that matters, since an agent can find the socket and call it directly
- `src/agent/serve.rs` — the other side of that protocol: `mcp-serve` answers a client outside Explorer with the `conversation`, `format_markdown` and `knowledge` tools, which it has no other way to reach, and forwards the MCP servers `main` hands it; it reads neither the repository walk nor any TUI state (given a turn's `--privileged-socket`, it asks that turn for the `conversation` tool instead of reading the tree from disk, and `--knowledge-read-only` makes it serve a feedback turn), and never serves `read`, `glob`, `grep`, `shell`, `write` or `edit`, which the client already has
- `src/agent/claude_code.rs` — the `claude -p` driver: arguments (always `--strict-mcp-config`, so a run sees only the repository's MCP servers and Explorer's own tool server; `--disallowedTools "Edit(docs/knowledge/**)"` on every run with tools, since the flag is variadic and must be followed by another flag), `stream-json` parser, session forking
- `src/agent/context.rs` — system prompts (from `prompts/*.md`), ancestor replay and every note Explorer injects; the user's own instructions (`src/agent/fragments.rs`, from `<config_dir>/prompts/` and `<config_dir>/prompts/<mode>/`) are appended last so they win on a conflict; whatever is sent to the model must be recorded in `transcript.jsonl`
- `src/agent/commands.rs` — slash commands (`/compact`, `/commit`, `/continue`, `/update` built in, custom ones from `<config_dir>/commands/`)
- `src/agent/fragments.rs` — the user's own system-prompt fragments: every `.md` in `<config_dir>/prompts/` applies to both modes and those in `<config_dir>/prompts/<commit|feedback>/` to one, in name order, appended last; an optional `---` block turns one off with `enabled: false` (only `true` or `false`, so a typo is refused), an unknown key is ignored, and a file that is disabled, empty or unreadable is logged and skipped
- `src/agent/workspace.rs` — the copy a feedback turn works in: `workspace::plan` decides at the start of a turn (`Plan::Copy` or `Plan::InPlace` with the reason; `warm` runs the sandbox check at startup, on any repository: it needs no `target/`) and `Workspace::copy` reflink-copies the repository, `.git` and `target/` included and `target/explorer` left out, into `target/explorer/<pid>-<node>` (built as `.building-…` and renamed, mtimes kept to the nanosecond so cargo finds every artifact fresh), and only where `target/` already holds a build, is a real directory, neither `CARGO_TARGET_DIR` nor `CARGO_BUILD_TARGET_DIR` is set and no cargo config redirects `build.target-dir`, no `.git` file, `core.worktree`, `objects/info/alternates` or `commondir` names a tree outside the copy, and `sandbox-exec --check` works; any other case runs the turn in place with the reason (the node's `in-place`). A copy that fails after admission, the walk's refusals of shared git data included, fails the turn and plans the next one in place (`copy_failed`): it cannot fall back to the tree. `Sandbox` builds the `sandbox-exec` prefix (a copy as workspace, or no workspace and a read-only list); `Sandbox::commit` is a commit turn's, which creates `docs/knowledge` if absent and returns `None`, so the turn runs unprotected and unblocked, when the namespaces do not work, that path is not a directory, `docs/` is not a real directory, or the directory is on another filesystem than the repository (a mount of its own is not remounted by a bind of the root); `release` deletes the copy on a thread of its own, and `sweep` (at startup, and before each copy) deletes what is stale: a copy whose owner is dead, or whose owner is this process and which no running turn holds (`IN_USE`)
- `src/agent/models.rs` — `GET <url>/models`
- `src/agent/fixtures/` — `claude_stream.jsonl`, a recorded `claude -p` stream that the parser's tests in `claude_code.rs` read
- `src/agent/fake_llm.rs` — tests only: a scripted OpenAI-compatible server on raw tokio; the n-th request gets the n-th `Reply` as SSE, and every request body is recorded for `turn/pin.rs`
- `src/tui/mod.rs` — module declarations; `test_support.rs` is the shared `AppState` and `GlobalData` test setup, with `key` and `frame_box`
- `src/tui/app/` — the `r3bl_tui` `App` impl: `mod.rs` (`AppMain`, one dispatcher per trait method), `init.rs` (components, background services), `input.rs` (key and mouse routing), `signals.rs`, `render.rs`, `status_bar.rs`, `tick.rs` (timers and housekeeping)
- `src/tui/state.rs` — `AppState`, the one state every component reads; inputs and selections live here beside what they edit, never in a component (a component belongs to a pane slot and a window changes slot whenever the stack moves)
- `src/tui/focus.rs` — `focus_window`, `close_window` and terminal focus notification: the shared helpers panes call instead of importing `app/`
- `src/tui/pane_manager.rs` — `PaneManager` and `Window`: focus, resize, stacking and the frame's one layout (`relayout`, run once per event and once per frame; nothing else recomputes it)
- `src/tui/selection.rs` — `TextSelection`, the one text selection of `AppState`, and the `SelPoint`s and `DocPart`s that address it in a file preview, a terminal or a Discussion; `DragModifier` and `drag_modifier_from_mouse` (the Shift or Ctrl drag that marks a highlight range)
- `src/tui/pane_component.rs` — the component of a pane slot: a stack of windows with tab headers, routing mouse and commands to the active window
- `src/tui/keymap.rs` — configurable shortcuts: `Action`, `Keymap`, key-spec parsing, status-bar hints
- `src/tui/command_line.rs` — the global command line (`<leader>:`) and `COMMANDS`, the one table of what it runs and what the palette lists
- `src/tui/after_agents.rs` — the `after-agents` command: a one-shot shell command after the queue empties, cancelled by any input; killed with its process group when dropped
- `src/tui/session.rs` — session save/restore: trust, MCP servers, aliases, the window stack and each window's own state; the one place that reaches into every pane; `session_mcp_servers` reads that list alone, which is all `mcp-serve` needs of a session, and `main` reads it before handing the servers to `agent::serve` so that `agent/` never names `tui/`
- `src/tui/symbols.rs` — symbol highlight groups' bookkeeping, in `FileKey`s alone: `answered` (a language server's answer folded into the groups), `find`, `group_at`, `dedup`, `files_changed`
- `src/tui/theme.rs` — `HelixTheme`, loaded from the bundled TOML files via `include!("../../themes/themes.rs")`; `Color` (`Rgb`, `Ansi` for the terminal's palette, `Default` for its own color: Helix's 16 color names reach the screen as the terminal's palette, and an unset background or text as the terminal paints it) and `Ui`, every role a pane paints, resolved once at load with one fallback chain each. Panes read `theme.ui` fields, never a UI key by string; theme colors are painted with `ApplyColors(Some(fg.on(bg)))`, never `SetBgColor`, which cannot ask for the terminal's default; color arithmetic runs only on `Color::rgb`
- `src/tui/panes/` — one window kind each: `preview.rs` (file preview with syntect highlighting and LSP tokens), `git_log_pane.rs` (log with live filter), `commit_diff_pane.rs`, `report_pane.rs` (markdown in memory, nothing saved), `terminal/` (`mod.rs` PTY emulation and selection, `spawn.rs` start, reader task and `Throttle`); a pane never imports `app/`; what panes share lives in `focus` and `state`
- `src/tui/modal/` — the pickers, painted over the panes: `mod.rs` (`Modal`, the open one in `AppState::modal`; `ModalBox`, its centred box and frame; `render`, the one place a modal is painted, at `ZOrder::High`; `Modal::open`/`close`, which release the keyboard and give it back; `dispatch`, which every event goes through while a modal is open), `window_picker.rs`, `theme_picker.rs`, `model_picker.rs`, `branch_picker.rs` (the git log's branches: a `HEAD` row first, then every branch and tag). A modal is not a window: no pane slot, no session entry, no place in the focus cycle — never give one a `Window` variant or route an event to a pane while one is open
- `src/tui/panes/discussion/` — a conversation window (`Window::Discussion(id)`): `mod.rs` (component, lifecycle, title bar), `launch.rs` (start a turn), `queue.rs`, `document.rs` (the laid-out document and its `DocCache`), `selection.rs` (marks and `locate`), `status.rs`; `drop_queue_entry` must be called wherever a Discussion window is removed
- `src/tui/panes/conversation_tree/` — the Conversation Tree window: `mod.rs`, `layout.rs` (`rows`, the one place rows and filter matches are built), `filter.rs` (`FilterCache`, keyed by query and tree `revision`), `input.rs`, `render.rs`, `test_support.rs` (the trees the tests build)
- `src/tui/widgets/` — reusable pieces with no window of their own. A widget takes values and names only other widgets, `crate::tui::theme` and `crate::util`, never `state`, `pane_manager` or a pane; `cargo xtask check-imports` enforces it. `contrast.rs` (`ensure_readable_fg`: the foreground swap that keeps text readable over a marker or highlight colour, a no-op when either colour is the terminal's), `fuzzy.rs` (`paint_runs`, `matched`: painting the fuzzy matches `util::fuzzy` ranks), `fuzzy_picker.rs` (`FuzzyPicker`, which paints rows and keeps nothing but its query, `PickerRow`, `PickerColors`, and the `FuzzyPickerState` a picker's query, results, selection and scroll offset live in), `input_line.rs` (`InputLine`, text and cursor, soft-wrapped), `markdown.rs` (markdown to themed `MdLine`s; each `Span` carries its source byte offset; `span_style`, `row_pieces` and `paint_row` paint one row of spans with the marked ranges, which the report pane and the Discussion both do), `text_layout.rs` (`wrap`, `truncate`, `display_width`), `title_row.rs`

---

## Build & Run Commands

```bash
# Build -- always use release, even for development
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
- **Test helpers are the exception to the function rule**: the shared helpers of `util::test_support` (`temp_root`, `git`, `commit`, `temp_repo`) are imported by name in a test module.
- **Always spell paths from `crate::`**, never `super::` or `self::`, so one grep finds every dependency on a module. The exception is `use super::*;` in a `#[cfg(test)]` module.
- **`r3bl_tui` is the one glob**, in tui code, because it is the UI's shared vocabulary: `GlobalData`, `InputEvent`, `FlexBox`, `RenderOpCommon`, the `throws!` family and `new_style!` appear in nearly every signature. Mark each of those lines `#[allow(clippy::wildcard_imports)]`. It brings `HashMap`/`HashSet` with it, aliased to the Fx hashers: a module that wants the std ones keeps its `use std::collections::{HashMap, HashSet};`.
- **Never a glob otherwise**, including `use Window::*` for enum variants: a misspelled variant would bind instead of failing to compile.
- **No re-exports.** Each item has one canonical path; `crate::tui::state::AppState`, never `crate::tui::AppState`.
- **Call this crate's trait methods by full path** with an explicit receiver, and r3bl's `Component` methods too when a parent dispatches to a child: `r3bl_tui::Component::handle_event(&mut self.preview, global_data, input_event, has_focus)`. std and other external traits keep method syntax: `Arc::clone(&x)`, never `x.clone()` on a shared pointer.
- `cargo xtask check-imports` enforces the module-level rule, the `super::` rule, the allowed globs, the full-path variant inside a body, the single-variant import and the widgets' dependencies. `clippy::wildcard_imports` and `clippy::clone_on_ref_ptr` are warnings in `src/main.rs`.

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
- `prompt.md` is the prompt as sent to the model, after slash-command expansion; `input.md` holds the text as typed when a custom command, `/commit` or `/update` was expanded. Give the user their text back through `conversation::files::read_input`, never from `prompt.md`, and never send `input.md` to a model.
- `reply.md` holds only the text of answers that called no tools. The text of an answer that called tools, and one line per tool call, go to `activity.md`, the work log. It is display-only: never replay it and never send it to a model.
- Never pass `--bare` to `claude`: it disables the subscription login.
- A queued prompt is never copied: the queue holds the window, and the text stays that window's `draft` until its turn starts. Nothing reaches the conversation tree or the disk before `start_turn`. The queue is released only from the main thread, after the finished turn's status is written to its node.
- A message sent to the window of a turn that is running is not queued: it joins that turn (`LiveTurn.injected`: an own-loop turn's agent process sends a `Poll` at the top of every round and a `Settle` when an answer calls no tool, and `driver::run` answers each with what waits; a message leaves the queue only on the `Injected` the agent sends once it has read it. `claude_code::run` writes them to the CLI's stdin), and the turn's prompt block, `prompt.md` and `transcript.jsonl` carry it as a user message that is not `injected`. It enters the conversation at a round boundary, so a turn whose answer called no tools keeps going to read it. What the turn never read goes back to the input when it ends (`restore_injected`), and `Esc` takes it back before it cancels. A Claude Code turn whose stdin is closed (`LiveTurn.closed`, set by `Injector::close_if_idle` once a result followed the last write and no task of the CLI is still running, or by `Injector::close` when a write to it fails) takes no more messages; an own-loop turn is closed the same way: `driver::run` sets `LiveTurn.closed` at a `Settle` that finds nothing waiting and no commit nudge to send; a commit nudge keeps the turn open. Only a message for another turn — another window's node, or a turn that no longer takes them — is queued.
- A window has no mode: a prompt is a feedback turn unless it is `/commit`, `/update` or a custom command with `system-prompt: off`, so the mode travels with the typed text (queue, `input.md`, restored prompts).
- A custom command with `system-prompt: off` gets no mode prompt and no feedback note, so its turn must never be reset: it runs and closes as a commit turn (`TurnKind::NoSystemPrompt`), and its node records `system-prompt: off` for replays. Both modes have the same tools; what differs is the system prompt and how the turn is closed. `/update` (`TurnKind::Update`, `command: update` in the node) is the same shape with its own prompt: a commit turn that is never nudged to commit (`TurnRequest.update`), because what it staged under `docs/knowledge/` is committed by the close step.
- However an update turn ends, `close::commit_staged_knowledge` commits what is staged under `docs/knowledge/` as `docs(knowledge): update` before HEAD is read, so the commit counts as the turn's: such a turn stages entries and never runs git, so nothing else would. A rejected commit fails a turn that had finished and says the entries were discarded with the rest; a turn that had already failed or was cancelled keeps its status and only logs it. Any other commit turn leaves its staged knowledge to the agent's own commit, like the code it describes: a plain `git commit` takes the index, so the entry travels with the change that explains it, and whatever the agent left uncommitted is discarded with the rest.
- A sub-agent starts from its task alone: never give it the caller's context. It runs inside its caller's agent process, so cancelling the turn ends it with the rest, and it leaves no record of its own. It has every tool but `agent` and leaves the closing to its caller: the sub-agent of a feedback turn gets the feedback note and must not commit either.
- Agent-side child processes (the `claude` CLI, `shell`, MCP servers) run on `tokio::process`, never on RRT: the agent awaits their replies, and RRT suits blocking sources broadcasting to the UI (LSP, watcher).
- MCP servers outlive turns in the app's host, so what a server holds (egui-mcp's `attach`) carries over; an own-loop turn's `explorer agent` starts its own copies, which do not share it. A turn sees the tools of the servers running when it started, and never forwards images to the model.
- A background `shell` command never outlives its turn: an own-loop turn's `explorer agent` drops its `Background`, killing its process groups, when its run ends, and the turn stops and waits for that process (`driver::ChildGuard::stop`, async: SIGTERM, then a kill of the group as soon as the leader is gone or `STOP_GRACE` has passed) before the turn is closed (the tree is reset), and the `read` tool stays confined to the repository, so the agent reads `<node dir>/processes/<n>.log` through `shell`. A `claude-code` turn holds its CLI in the same `ChildGuard` and takes the cancel receiver, so a cancelled run stops the CLI's group before it returns: a copy of the repository is never released under a process still writing it.
- No turn runs in a repository the user has not trusted (`AppState.trusted`, saved in the session, set by the command line's `trust`/`untrust`, and shown in the Conversation Tree's title). A refused turn says so and names `scan` then `trust`. A report is not a turn and never touches the conversation tree.
- Every prompt requires a clean working tree. A turn holds the repository one of three ways (`turn::Claim`, `turn::admit`, decided in `start_turn`): `Exclusive` (a commit turn, or a feedback turn planned in place, `workspace::plan`) needs it alone, `Compact` touches nothing, and `Copy` (a feedback turn that will work in a copy) runs beside other copies, at most `parallel-copies` (default 4) of them. A prompt that may not start yet is queued, in order, and `release_queue` starts it when `admit` allows (a finished turn calls it, and so does the next tick when the head's window was closed: `AppState.release_pending`); a turn task that panics still ends with a failed `TurnFinished` (`report_panic`), so its claim is released; a prompt for the window of the turn that is running joins it instead. Every turn that works in the repository itself ends with a hard reset of the tree: a feedback turn's scratch work and a commit turn's leftovers are both discarded, so nothing of a turn survives outside the commits it made. A feedback turn that runs in a copy (`agent/workspace.rs`) never touched the tree, so it is not reset, and neither is a compact turn; a copy that cannot be made fails the turn (it was admitted beside others, so it cannot fall back to the tree) and the next turn is planned in place (`reset` records whether its copy held uncommitted work, which the next `/continue` is told was discarded); its note (`SANDBOX_NOTE`) says so, and sub-agents get the same one; a compact turn has no tools, so it gets no feedback note at all. A commit they make in a feedback turn is not recorded to the conversation, so feedback turns must not commit.
- Subscription usage is only known for `claude-code` (pushed by the CLI while a turn runs), `opencode-go`, `kimi-code` and `cline` (asked once a turn has ended); an OpenRouter key's spending is polled at startup and every minute between turns, with the account's credit when the key has no limit. OpenCode Zen is another product with another accounting: do not treat an endpoint as `opencode-go` because of its host. A new provider gets its own `EndpointKind`, never a probe of `<url>/usage` on plain `openai` endpoints.
- The system prompt holds nothing that changes between turns. The repository state (root, date, HEAD) opens the user message as an Explorer note, together with the compaction and repository-moved notes: HEAD moves on every commit, and the endpoints cache on the longest common prefix, so a change ahead of the replayed ancestors throws the cache of the whole transcript away.
- `mcp-serve` is a short circuit, not a second Explorer: it starts no TUI, walks no repository, reads no configuration beyond what a tool call needs. It serves the tools a client cannot have otherwise (`conversation`, `format_markdown`, `knowledge`) and forwards the MCP servers the repository's session lists; its `knowledge` writes under `docs/knowledge/` and stages them, but only through the turn's socket when it has one, and directly when it runs outside Explorer; it never serves `read`, `glob`, `grep`, `shell`, `write` or `edit`, which the client already has and which would put arbitrary commands behind anything able to start this process. Its stdout carries JSON-RPC and nothing else: any `println!` reachable from it corrupts the protocol, so diagnostics go to stderr.

### Comments

- Write no comments by default.
- Add a comment only when the _why_ is non-obvious: a hidden constraint, a subtle invariant, a workaround for a specific external bug.
- Never describe _what_ the code does — well-named identifiers already do that.
- No multi-line comment blocks. One short line maximum. A file's `//!` module doc is not a block in this sense: it is where the _why_ of the file belongs.
- Doc comments written before this rule stay as they are until a commit touches that function; then it shortens them.

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
