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

Source is organized as:

- `src/main.rs` — async entry point, wires CLI → loader → TUI
- `src/cli.rs` — CLI argument parsing via `pico-args` (`--scan` prints the scan report and exits before the UI starts; `fmt-md [paths]`, the first argument only, formats markdown and exits, see `src/markdown_format.rs`)
- `src/config.rs` — KDL configuration file parsing (theme, `keybindings` block, `agent` block with endpoints, `summary-model`, `subagent-model` and `review-rounds` (the most review rounds a commit turn runs on each commit, at least 1, default 3, filled into the `{review_rounds}` placeholder of `prompts/commit.md`) and `repeat-limit` (how many times one tool call may return the same result before the agent loop nudges the model, and again after that nudge before it fails the run; 0 turns the check off, 1 is refused, default 5); an endpoint's `kind` is `openai` when absent, `opencode-go` (its `url` defaults to OpenCode Go's), `kimi-code`, `cline` (their `url` is required), `openrouter` (its `url` defaults to OpenRouter's) or `claude-code`); an `after-agents { grace-seconds <n> }` block (`AfterAgentsConfig`: grace default 5, 0 runs the command at once) and top-level `alias <name> "<command>"` lines (`Alias`, `Config.aliases`: a one-word, unique name; also the type of the project aliases of the session); the five `*_KIND` consts are the endpoint kind strings the parser matches on and `EndpointKind::label` writes; `load_auth` reads endpoint tokens from `<data_dir>/auth.kdl`
- `src/conversation/` — the agent conversation tree: `mod.rs` (`NodeId`, a node's directory name, `NodeType`/`NodeStatus`, `Node`, `TurnContext`, `ConversationTree`, which holds metadata, titles and summaries only — prompts, replies and transcripts stay on disk — and a `revision` it hands out (`revision()`) and moves on every change, from a counter shared by the whole process so a tree reloaded with `load` never takes the number of the one it replaces, so what a reader cached of the tree can tell when it went stale; node creation/delete, `turn_context` (what a node's turn replays, cut off at the nearest compact node), `mark_cut_short` (writes the reply and records the commits and the HEAD it stopped at, for a turn quit or killed) and the fold queries `thread_below`, `thread_end` and `folds_on_the_way`), `metadata.rs` (`Metadata`, the flat `key: value` of `metadata.md`, with `record_outcome` and `mark_cut_short`; `TurnOutcome`; `cut_short_outcome`, the one rule for what a turn cut short records), `files.rs` (the file names a node and a run keep, and `read_prompt`, `read_reply`, `read_prompt_and_reply`, `read_input`, `read_typed_input`, `read_work_log`, `read_title`, `read_summary`, `write_title`, `write_summary`, `write_reply`, so no caller spells a node file name), `agents.rs` (`AgentRecord`, a sub-agent run of a node's turn, in `<node dir>/agents/<index>/`, never a node: `load_children` skips that directory, as it skips `processes/`, the logs of background commands; `agent_id` is `<node>/<index>`, `resumable_agent` accepts only runs of the asking node's lineage, `agent_chain` follows `resumed-from`; `title` prefers the run's own `title.md` and falls back to `agent <role>: <description>`, the caller's words; `set_agent_title` applies the model's answer in memory, and the task that asked writes the file) and `toc.rs` (table-of-contents pages and commit lookup for the agent's `conversation` tool); the tree pane lays the rows out with the fold queries and a turn expands them
- `src/markdown_format.rs` — the markdown formatter (`dprint-plugin-markdown` as a library, line breaks of prose kept): `format_markdown`, `is_formatted` (`.md` outside `vendor/`, `target/` and `tests/fixtures/`), `expand_directories` (a directory argument becomes the files git lists under it, ignored ones left out), `format_files` (in place, returns the files rewritten), `format_committed` (run when a commit turn ends: formats the markdown of the commits made and amends it into the newest, skipped while the tree is dirty); also behind the `explorer fmt-md [paths]` subcommand (default: the markdown changed in the working tree) and the `format_markdown` agent tool; a directory argument formats recursively
- `src/model_spec.rs` — `Effort` (`Default`, `Low`, `Medium`, `High`; `Default` sends no effort parameter) and `ModelSpec` (`endpoint/model/effort`: `parse` lets the effort be left out and the model id contain `/`, `short_label` for discussion windows); it names no other module
- `src/agent/mod.rs` — `Mode` (feedback/commit: how a turn runs and closes, never a setting of a window), `AgentConfig` (`resolve` joins the settings with the tokens and the two directory loaders; an endpoint without a token is logged and skipped), `Endpoint`/`EndpointKind` (`OpenAi`, `OpenCodeGo`, `KimiCode`, `Cline`, `OpenRouter`, `ClaudeCode`; `usage_label` names the spending behind a kind, `None` for a plain OpenAI-compatible server), `Backend`, transcript `Message`
- `src/agent/turn/` — turn lifecycle off the UI thread: `mod.rs` (`TurnRequest`, what a turn is asked, and `message_to_send`, the notes it opens with; `spawn`, whose task runs the turn, marks its running sub-agent records cancelled and closes the working tree, sends `TurnFinished` and then keeps `run`/`run_own_loop`/`run_claude_code`; `TurnHandle`, which `TurnHandle::cancel` stops; `SPINNER_INTERVAL` and `spinner_char`, the frame of the shared `AppState::spinner_tick`, advanced by `AppSignal::TurnTick` and shown by the tree marker and the Discussion status line; `report_usage`, after `TurnFinished` whatever the status, and at startup and every minute for an OpenRouter key: it sends `AppSignal::UsageFetched` with what the CLI last reported or what OpenCode Go, Kimi Code, Cline or OpenRouter answers; `run` opens with `McpHost::snapshot`, the MCP tools of the turn and of its sub-agents), `live.rs` (`LiveTurn`, the reply, work log, thinking and activity a Discussion window reads at render time; `Recorder`, which feeds it and writes `reply.md`, `activity.md` and `transcript.jsonl`, a sub-agent's also logging its tool calls, indented, in its caller's work log; `Injector`, what the user sends to a running turn goes through; `LiveSubagent`, the run the turn is waiting for), `close.rs` (what a turn does to the working tree when it ends: `close`, the commits of a commit turn and the discard of what it left uncommitted; `uncommitted`, the status the hidden nudge asks the model to commit), `title.rs` (the post-turn title and summary, one commit-message-style call, `retitle`, also run by the tree pane)
- `src/agent/llm.rs` — the only file importing `genai`: streaming chat with tool calls (missing or empty arguments become `{}`, so the transcript replays an object), retried with jittered backoff while nothing has streamed (`is_transient`), the client's connect and read timeouts, one-shot completion
- `src/agent/agent_loop.rs` — our own agent loop for OpenAI-compatible endpoints; `run` returns a boxed future, since an `agent` call runs the loop again for the sub-agent, and takes the run's `Limits`: its time budget (start and duration, `None` for turns), injecting wrap-up nudges like the language correction: one soft at deadline − min(2 minutes, half the budget), then a firm one every round past the deadline; and the `repeat-limit`, which `StallDetector` checks after every tool result: a call (name, arguments, hash of the result) found that many times among the last twice that many calls gets a recorded nudge quoting the call as sent, and the window starts over; the run fails only when a call already warned about reaches the limit again; `repair_tail` answers the dangling tool calls of a transcript cut inside a round, so a `/continue` can replay it; a round that dies mid-stream flushes what had streamed into the transcript as an incomplete assistant message
- `src/agent/subagent.rs` — sub-agents of own-loop turns: the `agent` tool (`spec` lists the `Role`s, `describe` is its work-log line), `Role` (`<config_dir>/agents/<name>.md`: metadata block with a required `description` and an optional `model` and `budget-minutes`, the soft time budget of one run, then the system prompt), `Subagents` (what a turn lends its sub-agents), `run` (records the run, runs it with no context of the caller on the role's model, then `subagent-model`, then the caller's; a resumed run keeps its model and replays its chain (without the feedback note when the caller is a commit turn), or forks its Claude Code session unless a commit caller resumes a run made under a feedback node; returns `agent_id: <id>` and the final answer, behind an over-budget note when the run reached its budget), `conclude` (the one funnel every run ends through, success, failure and both halves of a fallback retry: writes the record's status and sends `SubagentFinished`), `title_run` (asks the summary model what a finished run with a non-empty `reply.md` came to, in a detached task so the caller waiting on the run's answer never pays for it; writes the run's `title.md` and sends `AppSignal::AgentTitleReady`; failures are only logged), `title_backend` (that gate), `run_title_input` (the task and the _end_ of the answer, where a run's verdict is); `load_roles` reads the roles from `<config_dir>/agents/<name>.md` the same way
- `src/agent/tools.rs` — agent tools (`conversation`, `read`, `format_markdown`, `glob`, `grep`, `shell`, `write`, `edit`, the same in both modes, plus `agent` when `ToolContext.subagents` is set: own-loop turns, never sub-agents, then the tools of `ToolContext.mcp`), confined to the repository root; `read` pages by line (`offset`/`limit`, the footer naming the line to resume at), cuts a line over 2000 characters, and stops at the first line that would push its output past 30 KB, so a window is never spliced at a byte in its middle; `describe` falls back to the arguments as compact JSON; `shell` with `background` returns at once with the pid and the log of the command's output, `<node dir>/processes/<n>.log` (the first free `n`); the turn's `Background` (shared with its sub-agents) holds the group leaders, never waited for so a group id cannot be reused, and kills their process groups when it is dropped at the end of the turn
- `src/agent/mcp.rs` — MCP servers of the repository (`McpServerDecl`: a name and a program, no arguments, saved in the session): `McpHost` (in `AppState.mcp`) starts them all with Explorer and one on `mcp add`, keeps them running until `mcp remove` or exit (each in its own process group, killed when its `Client` drops), and reports each status change through a `Notify` (`AppSignal::McpStatus`); a hand-written stdio client on `tokio::process` (protocol `2025-06-18`, a reader task routing replies by id to the waiting call, a reply to a cancelled call dropped, a 120 s call timeout); `snapshot` is what a turn takes when it starts: the tools of the servers running, after waiting up to 10 s for those still starting; `McpTools` advertises them as `mcp__<server>__<tool>`, servers by name (a tool whose prefixed name the endpoints would refuse, with a character other than ASCII letters, digits, `_` and `-` or over 64 bytes, is skipped when its server starts), runs a call (arguments that are not an object refused; a server found dead is restarted once, in a task of its own so that a cancelled turn never leaves it starting, and the call returns an error telling the model its state is lost; a removed server is never started again, even by a call still holding it), turns a result into text (text blocks, else `structuredContent`, `[<mime> omitted]` for images, `isError` an error, cut with `truncate_middle`) and builds the `--mcp-config` of Claude Code runs
- `src/agent/claude_code.rs` — `claude -p` driver: arguments (always `--strict-mcp-config`, with `--mcp-config` from the turn's `McpTools`, so a run only sees the servers of the repository; `complete` passes `--strict-mcp-config` alone), takes the same `--setting-sources user` and settings, without tools or a saved session), `stream-json` parser (its `rate_limit_event` carries the subscription's usage), session forking
- `src/agent/context.rs` — system prompts (from `prompts/*.md`), ancestor replay, handover preamble, `budget_note` (the run's budget, start and expiry, closing a budgeted run's system prompt; composed in `execute`, so every run names its own times), the notes Explorer injects (repository state, compaction, repository-moved), `strip_workspace_note` (what a commit turn replays carries no feedback note), `SUBAGENT_TITLE_PROMPT` (`prompts/subagent_title.md`: the one-shot that titles a finished sub-agent run by its outcome, not by the task it was handed)
- `src/agent/commands.rs` — slash commands: `Command` (a custom command file: optional metadata block between two `---` lines with `system-prompt: on | off`, `argument`, `description`, then the base prompt), expansion (`/compact`, `/commit` and `/continue` built in; `commit_prompt` is the go-ahead a `/commit` sends), `placeholder` (the hint for a draft that is a command name with nothing after it); `load` reads the custom slash commands from `<config_dir>/commands/<name>.md` (a file that cannot make a command is logged and skipped)
- `src/agent/models.rs` — `GET <url>/models`
- `src/usage.rs` — subscription usage: `Usage` (its `UsageWindow`s: five hours, week, month; each a percent and a reset time, `None` for a provider that reports no reset until the window is spent; and `KeyMoney`, what an OpenRouter key reports of its own spending in micro-dollars, with `Credit`, what is left of the account's credit), `fetch` (`GET <url>/usage` of OpenCode Go), `fetch_kimi` (`GET <url>/usages` of Kimi Code), `fetch_cline` (`GET <url>/users/me/plan/usage-limits` of Cline), `fetch_openrouter` (`GET <url>/key` of OpenRouter: the credit the key has spent and the limit it enforces, and `GET <url>/credits` for a key without a limit, what is left of the account's credit), `pieces` (the status-bar entry, `Claude 22% 17:00 · 18% Thu 01:00`: local 24h reset times, a day of the month as an ordinal, `0%` and no time for a window past its reset), `money_pieces` (the entry of an OpenRouter key, `OpenRouter $0.50 · $1.00 daily`: the credit spent and its limit, colored by the percent of the limit spent; `OpenRouter $0.00 · $19.31 left` for a key without a limit, colored by the percent of the account credit spent), `load`/`save` of `<data_dir>/usage.json` (the last known usage by endpoint name, shared by every repository)
- `src/git.rs` — gix-backed git data: `load_log` (first-parent walk of HEAD or a resolved branch/tag ref, capped at `MAX_LOG_ENTRIES` entries in a `LogHistory`, plus `refs`: commit → branches/remote branches/tags map built by peeling every ref), `load_refs` (flat branch/tag list for the branch picker), `load_commit_diff` (unified text), `LogEntry`, `GitRef`/`RefKind`, `DiffLine`/`DiffKind`, `commit_type_prefix` (conventional-commit parsing), short-id helpers
- `src/tui/keymap.rs` — configurable top-level shortcuts: `Action` enum, `Keymap` mapping `KeyPress` → action per leader/global context, key-spec parsing, status-bar hint rendering; `<leader>Backspace` (`Action::SendToBack`, dispatched in `tui/app/input.rs`) sends the focused window to the back; `<leader>f` (`Action::ToggleFavorite`, dispatched in `tui/app/input.rs`) toggles the favorite of the node in focus: the one a Discussion window shows or the selected row of the Conversation Tree; `<leader>m` (`Action::OpenModelPicker`, dispatched in `tui/app/input.rs`) opens the Model Picker for the Discussion window in focus; `<leader>:` (`Action::OpenCommandLine`, dispatched in `tui/app/input.rs`) opens the global command line
- `src/util/` — the helpers every layer below shares: `time.rs` (`now_secs`, `parse_utc_stamp`, `utc_stamp`, `utc_date`, `local_hhmm`, `local_offset_secs`, `civil_from_days`, `days_from_civil`, `format_log_time`), `fs.rs` (`write_atomic`, `repo_hash`, `data_dir`, `config_dir`, `resolve_in`), `text.rs` (`word_bounds`, `drag_range`, `single_line`, `split_metadata`, `key_values`) and `process.rs` (`KILL`, `kill_group`, `GroupGuard`, the one place that calls `killpg`)
- `src/loader.rs` — parallel file walking and `LoadedFile` construction; `IGNORED_DIRS` and `is_ignored` (the one prune rule, shared by the walk, the watcher and the trigger check); `find_git_root`, `path_for_file_key` and `file_key_for_path`
- `src/loader/watch.rs` — `apply` (the shared file list, a `signal::FilesChanged` batch): marks the removed, reloads the modified and the created, appends the created (the list stays append-only, so `FileKey`s stay valid)
- `src/tui/session.rs` — session save/restore (whether the user trusted the repository, the repository's MCP servers and project aliases, pane stack incl. git log windows with branch + query, Discussion windows with their sub-agent run, terminals, highlights of files and of conversation nodes, the Conversation Tree's expanded folds and filter)
- `src/lsp/` — the rust-analyzer client, JSON-RPC over its stdio in a process of its own on a thread of its own (`LSP_RRT`, an `r3bl_tui::RRT`): `mod.rs` (public API: `send_file_request`, `send_symbol_request`, `set_cargo_features`, `health_check` with its `LspHealth`, `disabled` — the `EXPLORER_NO_LSP` environment variable, read once, so no rust-analyzer is spawned and every request is dropped — the `LspInput` vocabulary, `LspConfig`, `cargo_initialization_options` and the UTF-16 ↔ byte offset helpers; requests made while no worker runs are buffered and the worker takes them itself when it dispatches, so nothing on the main thread retries them), `rpc.rs` (the `Content-Length` framing, `RpcRequest`/`RpcNotification`/`RpcResponse`, `file_uri` and `poll_readable`), `worker.rs` (`LspWorker`: the handshake and the token legend it learns, the dispatch loop — the channel and the buffer, the `experimental/serverStatus` readiness gate that holds hover requests until the workspace is indexed, warmup of the Rust files one at a time and yielding to user requests, and the server's own requests answered with a null — and the restart a `features` command asks for, which the worker performs itself so no subscriber is dropped and no `Tick` polls for the old thread), `tokens.rs` (`request_tokens`, `close_file`, `apply_tokens` and `decode_tokens` into the preview's `ColoredLine`s), `symbols.rs` (the hover → references round-trip's `PendingRefQuery` and `qualified_name`, the name a hover answer states)
- `src/watcher.rs` — filesystem watcher via `notify`; debounces events into a `signal::FilesChanged` batch and broadcasts via `WATCHER_RRT`
- `src/signal.rs` — the event vocabulary every thread speaks: `AppSignal` (its payloads `FilesChanged`, `SymbolHighlight` and `SymbolRefLocation`), `MainTx` (the main loop's channel), `send` (awaits the send in a task of its own) and `try_send` (drops the signal rather than wait)
- `src/scan.rs` — deterministic scanner for text in the repository aimed at an agent rather than a reader: `scan` (walks with its own pruning rather than the loader's: hidden files are scanned, and `.git`, `target`, `node_modules`, `vendor`, `bower_components` are pruned as directories; never follows a symlink, modifies and executes nothing) returns a `Report` of `files`/`binary`/`not_utf8`/`unreadable` coverage (`not_utf8` a subset of `files`, `unreadable` counted apart) and `Finding`s of a `Severity::Danger` (hidden from a human or not readable as text: `Kind::TagPayload` decoded into the excerpt, `BidiControl`, `InvisibleRun` of 8+, `VariationRun` of 8+, `EscapeSequence`, `InvalidUtf8`) and `Severity::Review` (`TagCharacters` of a subdivision flag or a lone cancel tag, a single `InvisibleCharacter`, `ControlCharacter` but a tab, `PrivateUse`, `InstructionProse`, `SymlinkOutside`); `to_markdown` renders the counts and a capped evidence table; `--scan` prints it and exits before the UI
- `src/tui/panes/report_pane.rs` — report window (`Window::Report(id)`, state in `AppState.reports`: a title and markdown, in memory only). Shows markdown and nothing else: no input, no model, no node, and nothing saved, so a report is read and gone. Esc closes the window and drops the report, Backspace sends it to back, PgUp/PgDn and the mouse wheel scroll it; `show` replaces the text of an existing report instead of opening a second window
- `src/tui/mod.rs` — module declarations
- `src/tui/symbols.rs` — the symbol highlight groups' bookkeeping: `find` (the group at an origin, since toggles in flight shift indices), `dedup` (one group per qualified name, keeping the one it is given through removals of earlier ones) and `files_changed` (a group whose origin went away is dropped, one whose origin changed asks for its uses again, one whose uses vanished loses them)
- `src/tui/command_line.rs` — the global command line (`<leader>:`): `CommandLine` (in `AppState.command_line`: its `InputLine`, `origin`, the window focused when it opened, the `error` of the last run, shown as its placeholder until the next key, and `selected`, the palette's highlight, reset whenever the text changes), painted by `render_command_line` in place of the status bar and growing upward over the panes without changing `surface_size` (no PTY is resized while typing); `app_handle_input_event` hands it every key ahead of the leader key, and its mouse events; `COMMANDS`, the one table of what the line runs and what the palette lists (`Command { name, argument, description, run: fn(&mut CommandRun) -> Result<(), String> }`), so a command cannot be listed without being runnable nor runnable without being listed: `after-agents <shell command>`, which stores the command with its `%` placeholders expanded now (`%project`, the repository directory's name, needs no file), replaces any earlier one, and clears it when bare; `alias <name> <command>`, which sets or replaces a project alias with the command raw, removes it when typed bare (an alias of `config.kdl` cannot be removed), and does nothing for a bare `alias`; `mcp add <name> <program>`, which starts an MCP server of the repository (a unique name of ASCII letters, digits, `_` and `-`, without `__`, as the endpoints require of a tool name; the tree's notice reports the start once `AppSignal::McpStatus` arrives, through `announced_starts`) and `mcp remove <name>`, which stops it, with `command_hint` listing the installed servers after a bare `mcp`; `highlight <lines>`, which marks lines of the file preview the line opened from (`12`, `3-7`, `1-3,8`, clamped to the file; bare, it clears them), scrolls to them and shows the current ones as its hint (the preview's `:` opens the line with `highlight ` typed, `CommandLine::prefilled`); `features [no-default] [feature...]`, `all-features` and `default-features`, which restart rust-analyzer on that Cargo feature selection (refused with `EXPLORER_NO_LSP`); `scan`, which runs the repository scanner and shows its report in a report window; `trust` and `untrust`, which allow or forbid agent turns in this repository and are saved in its session; `run_command_line` dispatches on that table, and on `!<shell command>` (multiline allowed) and on an alias name, which opens the alias's command in a terminal since typing its name is how it runs; the palette lists the matching `COMMANDS` and then the aliases of `alias_rows` (the project's, then `config.kdl`'s shadowed ones) while the first word is being typed and is not already an exact name, filtered by prefix, `Tab`/`Shift+Tab` walking it (wrapping), `Enter` writing the highlighted name into the input (with a space when it takes an argument) so a second `Enter` runs it, the arrows left to the input's cursor, and the list capped at `MAX_LIST_ROWS` and a third of the screen and scrolled to keep the highlight in view, `palette_entries` being what the keys walk and the render paints; `command_hint` is the placeholder of each typed alone: what a `COMMANDS` entry does, the current `after-agents` command, what an alias expands to, the aliases (`resolve_alias` and `aliases_suffix`), what `alias <name>` would remove; it appends the palette's keys while that is up; `resolve_alias` replaces the first word of any shell command (`!`, `after-agents`) by the alias it names, the project's before the config's, appends the typed remainder unquoted, never recurses, shadows a program of the same name and leaves the text unchanged on a miss; `shell_signal`, expands the placeholders (`%%` a literal `%`; any other placeholder needs `origin` to be a file preview) and opens the terminal; `sh_quote`
- `src/tui/after_agents.rs` — the `after-agents` command: `AfterAgents` (`Pending` until a turn has run and the queue is empty, then `Countdown` for the configured grace period during which any key, paste or mouse event cancels it (keys and paste are consumed; focus and resize are ignored), then `Running` headless: `sh -c` on `tokio::process`, null stdio, own process group, `kill_on_drop`), one-shot (a cancelled or run command is gone, never re-armed); `Group` kills the process group when the last holder drops it, so replacing or clearing the command, or quitting, kills a running one; `arm` (after `release_queue` in the `TurnFinished` handler) and `tick` (the 1s `Tick`) drive it, `Running.run` makes a late `AppSignal::AfterAgentsExited` harmless to a newer run; failures are only logged; `status_text` is the status-bar entry, `hint` the command line's placeholder (the current command)
- `src/tui/widgets/title_row.rs` — `TitleRow` trait (`render_title_row` takes `&mut AppState`: an input records where it is painted), `render_pane_title` utility, `title_bar_colors`, `title_selection_bg` (the background of a selection in a title-bar input, the other title bar's since a focused one already wears `ui.selection`)
- `src/tui/state.rs` — `AppState` (with `trusted`: the user allowed agent turns in this repository, saved in its session; global `TextSelection`, `SelPoint`, `terminal_grabbed`, `PaneManager`, `discussion_highlights`: `NodeHighlights` by node, `queued_turns`: `PendingTurn`s waiting for the running turn to end, in memory only, `after_agents`: the `AfterAgents` command, in memory only, `config_aliases` and `project_aliases` (saved with the session, win over the config's), `usage`: the last known usage of the spending behind each endpoint, by endpoint name, `mcp_servers` and `mcp`: the MCP servers of the repository as saved and as running, `reports` and `next_report_id`: the title and markdown of the report windows by id, in memory only, `preview_inputs`: the `PreviewInput` open in a file preview's title bar by file, the `/` `?` search with its direction, dropped in `close_window`, `open_input`: the `InputLine` a window shows right now, `input_mouse`: hands a mouse event to it, `input_drag`: the window whose input holds the mouse selection in progress, set with `mouse_drag_active` and dropped by `cancel_input_drag`), `RowTarget` (what a Conversation Tree row points at: a node, a sub-agent run of a node, or a queued prompt by its window and place in the queue), `TreeViewState` (the Conversation Tree's selection, folds, `shown_agents` (the nodes whose sub-agent runs are listed, in memory only: hidden at startup) and `query`, its `/` filter, with `search_active` and `before_search` while the input is open; `filtering` is true once the query holds a word), `TerminalPane`, `Window`, `WindowState`, and `FuzzyPickerState` types
- `src/tui/app/` — `mod.rs` keeps `AppMain`, the `App` trait impl (one dispatcher per method, since a trait cannot be implemented in two `impl` blocks), `run()` and the helpers they call; `init.rs` wires the components and the background services; `input.rs` routes keys and mouse; `signals.rs` handles the signals; `render.rs` composes the frame; `status_bar.rs` owns the status bar; `tick.rs` holds the timers and the housekeeping they drive; handles global text selection, terminal grab/ungrab, mouse event routing, and PaneManager-driven layout; starts the session's MCP servers in `app_start_background_services` with `crate::agent::mcp::notify` (the `Notify` sending `AppSignal::McpStatus`, whose handler reports the start of a server added from the status bar's command line in the tree's notice); the status bar shows the usage of the endpoint the focused window spends (`focused_endpoint`: a Discussion window's model, the Conversation Tree's selected node or sub-agent run, the Model Picker's selected row; nothing elsewhere), its percents in the theme's `warning` color from 75% and `error` from 90% (an OpenRouter key states the credit it has spent and its limit instead, or the account's credit when the key has none, polled by `poll_openrouter_usage` at startup and every minute between turns), and drops it first when the terminal is too narrow; between the dirty flag and the usage it shows the `after-agents` entry (`after-agents`, `after-agents <n>s` in the `warning` color while counting down, `after-agents running`), dropped after the usage when the terminal is too narrow, and any key, paste or mouse event during the countdown cancels the command; the last right-aligned segment is the local time `│ HH:MM` (24h, `clock_text`), always shown
- `src/tui/pane_manager.rs` — `PaneManager` type: focus, resize, stacking, and layout of 16 pane slots (`push_window`, `send_to_back`, `replace_window`, `move_forward`/`move_backward`, `resize_focused`, `cycle_focus`). It owns the frame's one layout (`relayout`, run once per event and once per frame), which nothing else recomputes: a pane slot is the index of its window in `window_stack`, so `slot_window` reads the stack (true before any frame) while `slot_box` reads that layout, and `visible` is what the renderer and the mouse path walk
- `src/tui/pane_component.rs` — `PaneComponent` implementation: renders a stack of overlapping windows within a pane slot with tab-style headers; manages per-pane state and command routing to child components, which read the window of their own slot (`slot_window`) instead of being told it; hands every mouse event to the active window's input first (`AppState::input_mouse`), ahead of the scrollbar and of the preview's title clicks, and cancels an input selection on any event that is not the mouse on that window
- `src/tui/panes/pickers/window_picker.rs` — window picker overlay (`Window::WindowPicker`, `<leader>space`): while the query is empty, every open window but the pickers in stack order (`open_windows_results`, rebuilt at each render; holds only `Window` values, titles are resolved for the visible rows), each row titled like its window's title bar and colored by kind (`row_accents`: terminal, agent, git); fuzzy file-name matches once a query is typed; `Ctrl+X` closes the selected row's window through `close_window` in `tui/focus.rs`, shared with `<leader>x` (exceptions → input → navigation)
- `src/tui/widgets/fuzzy.rs` — the one fuzzy-matching loop every picker and filter runs through: `Kind` (`Plain`, `Paths` for a segment-boundary bonus, `Substring` for every word in any order), `Query` (a parsed pattern with its `Matcher` and haystack, `set` reparsing only a changed text, `rank` for scoring a candidate, `match_indices` for the chars a painted row highlights, `score_and_occurrences` for a filter that needs both), `paint_runs` (one paint op per run of one style instead of one per char) and `matched` (a binary search of sorted indices, so no `HashSet` per row); a haystack is the text's chars, never `Utf32Str::new`, which indexes by grapheme and as bytes for any text whose graphemes all start with an ASCII char, so its positions drift from the chars painted
- `src/tui/widgets/fuzzy_picker.rs` — shared fuzzy list picker component; navigation via flat `match` on `InputEvent`; owns the `Query` its rows are highlighted with, so a frame reuses its matcher
- `src/tui/panes/git_log_pane.rs` — git log pane component; multiple windows (`Window::GitLog(id)`, state in `AppState.git_logs`); renders async-loaded log entries for HEAD or the window's chosen branch, arrow/page/home/end/mouse selection, `/` live filter (`run_log_match` ranks the log and `apply_filter` keeps the entries, while `painted_entries` names the rows a frame draws and `visible_matches` matches only those again, so a keystroke costs one score per commit and a frame one per painted row; ENTER validates, Esc clears search, Esc outside search is a noop), `b` in-window branch picker (fuzzy `run_ref_match` over all branches and tags, prompt `Show branch:`; ENTER loads that branch, Esc returns to HEAD), conventional-commit type coloring, 3-space date gap, right-aligned ref decoration per row (`layout_refs`: branches then tags, drop from the right with `...` in the dropped group's color), `Logs` title with `(branch)` segment, `(10000+)` when truncated, ` — <query>` suffix while a filter is validated), Enter opens a `CommitDiff` window (seeding `state.commit_diffs` and spawning `load_commit_diff` if absent)
- `src/tui/panes/commit_diff_pane.rs` — commit diff pane component; renders a colored unified diff for one commit (`+` green, `-` red, `@@` yellow, headers cyan); Backspace sends the window to back, Esc closes it
- `src/tui/panes/conversation_tree/` — Conversation Tree window (`Window::ConversationTree`, `<leader>c`; its title reads `Conversations (untrusted)` until the user trusts the repository): `mod.rs` (the component, its title row and `retitle_selected`), `layout.rs` (`rows`, the one place the row list and the filter's matches are built, taking the component's `FilterCache` so a frame that changed neither the query nor the tree does not filter again; `TreeRow`, `Fold`, `Branch`, `selected_index`, `shown_summary`), `filter.rs` (`filter_nodes`, `FilterMatch`, and `FilterCache`, which hands back the answer of the last query for as long as neither the query nor the tree's `revision` moved, since a filter run costs about a millisecond over a large tree and an ordinary frame would otherwise pay it again), `input.rs` (`handle_input` over `handle_search_key`, `handle_key` and `handle_click`, and one `move_selection`; `Shown`, the rows with the row the selection sits on and the page height, is what `handle_key` is answered against), `render.rs` (the frame and the row prefixes); `layout_rows` (roots oldest first, the newest selected by default; a thread, the straight run of single replies below a root or a branch, folds into its head and is collapsed until expanded, while the fork ending it stays visible; fork-only indentation; the first row below a root gets `└─`; `TreeRow.rails` records which forks above still have branches below, driving the `│`/`├─`/`└─` prefix and its continuation under the selected row), `with_queued` (a provisional dimmed row per queued prompt, after the row of the node it will follow, holding the prompt's first line and its place in the queue; left out when that node is folded away or the window shows no node yet), `with_agents` (a dimmed row per sub-agent run, the run's own title once the summary model has described it and `agent <role>: <description>` until then, with its state marker, right after its node's row and one level below it, for the nodes in `shown_agents`: `s` toggles the selected node's, a turn starting a sub-agent shows them without moving the selection; never listed by the filter; `Enter`/double click opens the run read-only, `Left` selects its node), `RowTarget` as the row's identity (`Node`, `Agent { node, index }` or `Queued { window, position }`), fold column (`+`/`-`, leaf `─`; a fork's arms and rail sit in its column), `state_marker` in the gutter after the date (braille spinner while running, `✗` failed, `■` cancelled, `≡` compact; `ui.linenr` rails, `ui.linenr.selected` on the selected row), selected node expanded with title and summary (the scroll re-anchors onto the selection only when it changes, and otherwise a view already at the bottom of the tree stays there as rows appear below it, so the sub-agent rows of a running turn scroll into sight), `Right` on a thread head opens it and selects its last reply (`thread_end`), `folds_on_the_way` (what `start_turn` expands), `f` favorite, `d` twice to delete, `Enter`/double click open the node (on a queued row, focus its window), `n` new root, `m` replay with another model, `e` edit as sibling, `c` commit diffs, `s` sub-agent runs, `Esc` takes back the queued prompt of the selected row (`take_back_window`) or the head-most one queued below its node (`take_back`); the other keys do nothing on a queued row, Backspace sends the window to back; `/` live filter in the title bar (`filter_nodes`: every typed word must be in the node's title or summary, in any order, smart case, through nucleo substring atoms; best match first, then newest first; `FilterMatch` holds the matched byte ranges of the title and of the summary, painted in `ui.cursor.match` and underlined through `segments`): while `TreeViewState.query` holds a word the window lists the matches flat (`flat_rows`, no queued rows) and says `No matches` when there are none, the open input takes every key (arrows and pages move the selection, each edit selects the best match), ENTER validates (` — <query>` title suffix, the tree's keys work on the matches), Esc in the input clears the filter and keeps the selected match selected in the tree with the folds above it opened (`reveal`), or gives back the row selected before `/` when nothing matched; `start_turn` clears the filter; `handle_event` wraps `handle_input` and, when the selection lands on a node without `title.md` (`needs_retitle`: turn over, something to describe, not already in `TreeViewState.retitling`) and a `summary-model` resolves, runs `retitle` again, `AppSignal::NodeRetitleEnded` clearing the guard
- `src/tui/panes/discussion/` — `mod.rs` (the `Component`, the window lifecycle and the title bar), `launch.rs` (what a turn is, `compose_launch`, `start_turn`, and the `launch_window` that `send` and `release_queue` share), `queue.rs`, `document.rs` (the document a window shows: `Doc`, its four sections laid out on their own, `DocCache` and `doc_lines`), `selection.rs` (the drag methods, the marks and `locate`) and `status.rs`; Discussion window (`Window::Discussion(id)`, state in `AppState.discussions`; `DiscussionState.agent` makes it show a sub-agent run of its node, read-only: no input, no send, no cancel, no highlights, its live output followed while it runs through `LiveTurn.subagent` and read again from disk on `AppSignal::SubagentFinished` or when the turn ends; `open` takes the run's index): prompt as plain wrapped text, reply as markdown, dimmed work log above the reply (shown while the node has no finished reply, `Ctrl+O` toggles it), status line (led by the spinner while the turn runs), multi-row input without a label, every prompt a feedback turn unless it is the built-in `/commit [instructions]` (a go-ahead run as a commit turn; refused bare in a new conversation) or the built-in `/continue [instructions]` (a child of the node shown that resumes its interrupted turn in the node's mode, a resumed commit turn without the mode prompt, replaying its transcript; allowed on an interrupted node whatever its children, refused on a compact node and in a new conversation), the input's placeholder hinting at what a command typed alone expects and does, `<leader>m` model (`Action::OpenModelPicker`, dispatched in `tui/app/input.rs`), `Esc` takes a queued prompt back (shared with the tree's, via `take_back`) or else cancels the running turn, a running turn's texts are mirrored into the window through `DiscussionState::mirror` on `LiveTurn.revision` and a drag through the work log is dropped when the entries shown change (`Ctrl+C` only clears the input), `Ctrl+Plus`/`Ctrl+Minus` move the queued prompt in the queue, `<leader>f` toggles the favorite of the node shown (`title` prefixes a favorite with `* `, in the title bar and the window picker); `<leader>Backspace` sends the window to the back (plain Backspace edits the input, so the chord goes through the keymap); mouse drags over the document, from word to word across rows (`locate`, `drag_range`): plain drag selects and copies the source text on release, `Shift`+drag adds a highlight and `Ctrl`+drag removes one (`AppState.discussion_highlights`, byte ranges of the prompt and the reply; refused while the turn runs); `start_turn` checks the guards (the repository trusted, one turn at a time, clean tree), creates the node and spawns the turn, or returns a typed `Refusal`; `send` hands the draft to the running turn of the node the window shows (`inject`: the text joins `LiveTurn.injected` and the draft clears), and queues the window on `Refusal::Busy` (`AppState.queued_turns`) otherwise, `compose_launch` builds the turn from the window as it stands (a plain message on a failed or cancelled node resumes it the way `/continue` does, `resumes_on_message`; anything else is a new child, and a `/continue` resumes the node itself), `release_queue` starts the queue when a turn ends, `restore_injected` gives a message the turn never read back to the input (also `Esc`'s first effect while a turn runs), `drop_queue_entry` must be called wherever a Discussion window is removed. Opening a node is passive: it starts no turn, deletes nothing, and the draft starts empty.
- `src/tui/widgets/markdown.rs` — `render`: markdown (`pulldown-cmark`) to theme-colored `MdLine`s of styled `Span`s fitted to a width; headings, emphasis, code, quotes, lists, links, rules, and tables (aligned columns with wrapped cells, stacked `Header: value` records when the columns cannot fit); each `Span` carries `src`, the byte offset of its text in the source, `None` for what the renderer adds or rewrites; `span_style` styles one span over a background, shared by the report pane and the Discussion, and `paint_row` paints one row of spans for a pane (the background stripe, then each span at its column, cut where the row runs out of width)
- `src/tui/panes/pickers/model_picker.rs` — Model Picker window (`Window::ModelPicker`): fuzzy list of `endpoint/model/effort`, for a window's next prompt or to duplicate a node; `open` refuses `ModelPickerPurpose::Window` for a window whose turn is running (its model is settled for the whole turn), never `Duplicate`; `fetch_models`
- `src/tui/widgets/text_layout.rs` — `wrap` (each line with its byte offset in the text), `truncate`, `display_width`
- `src/tui/theme.rs` — `HelixTheme` type; loads bundled TOML files via `include!("../../themes/themes.rs")`
- `src/tui/panes/pickers/theme_picker.rs` — theme picker overlay with fuzzy search and live preview (exceptions → input → navigation)
- `src/tui/panes/preview.rs` — `FilePreviewComponent` with syntect syntax highlighting; text selection uses global `AppState`; its `/` `?` input lives in `AppState.preview_inputs` (`:` opens the global command line with `highlight ` typed), by file
- `src/tui/panes/terminal/spawn.rs` — starting a terminal pane: `open` (a `Request` of what to start: `cmd`, `cwd`, `pane_size`, `id` and `replace_window`, plus the app shell's `event_tx`; a `cwd` falls back to `AppState.root`), its reader task (feeding `OfsBufVT100`, scrolling, osc titles, PTY responses, `Redraw` on exit), `Throttle` (a chunk is announced unless the gate shut on a full channel, reopened by a `GAP` of 100 ms; a chunk it holds back sets the pane's `flush_pending`, which the 1 s `Tick` repaints) with five tests, and `drain_exits` (an exited pane with an empty screen closes at once, an occupied one shows as exited)
- `src/tui/panes/terminal/mod.rs` — `TerminalPaneComponent` with PTY-based terminal emulation; `render_ofs_buf_to_ir()` emits `ResetColor` for `Spacer` runs to prevent stale SGR inheritance; supports mouse selection, word/line text extraction, and clipboard copy; synchronized output (DEC private mode 2026) is buffered natively by `OfsBufVT100` in r3bl-open-core
- `src/tui/widgets/input_line.rs` — `InputLine`, a text input that owns its text and its cursor (`text`, `with_text`, `set_text` with the cursor to the end, `clear`), where it was last painted (`Painted`, recorded by `render`) and its mouse selection (a byte range) and lives in `AppState` beside what it edits (`DiscussionState.draft`, `FuzzyPickerState.query`, `GitLogWindowState.query`, `TreeViewState.query`, `TreeViewState.command`, `PreviewInput`, `CommandLine.input`), never in a component: a component belongs to a pane slot, and a window changes slot whenever the stack moves; Emacs-style key bindings; single flat `match` on `InputEvent`; soft-wraps over several rows, `Up`/`Down` between rows, `Ctrl+A`/`Ctrl+E` to the start and end of the line the cursor is on (newline to newline, whatever the wrapping), `Ctrl+K`/`Ctrl+U` kill within that line (`Ctrl+K` at the end of a line joins the next one), `Home`/`End` to the start and end of the whole text, height cap with internal scroll; optional placeholder passed to `render` and `height` at each frame: weak text laid out after the typed text, over as many rows as it needs, which the cursor and the keys never enter; `handle_mouse` (wired once, in `pane_component.rs`): a press on the painted cells puts the cursor there, a drag from it selects whole words across rows (`locate`, `word_bounds`, `drag_range`; a position outside the input stands for its nearest cell, no auto-scroll), the release copies the selection to the clipboard and drops it, a double click takes the word; the placeholder is never selected; `render` paints the selection on the third color it is given

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
- **`r3bl_tui` is the one glob**, in tui code and in `keymap.rs`, because it is the UI's shared vocabulary: `GlobalData`, `InputEvent`, `FlexBox`, `RenderOpCommon`, the `throws!` family and `new_style!` appear in nearly every signature. Mark each of those lines `#[allow(clippy::wildcard_imports)]`. It brings `HashMap`/`HashSet` with it, aliased to the Fx hashers: a module that wants the std ones keeps its `use std::collections::{HashMap, HashSet};`.
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
