# Code review, November 2026: second pass

A fresh review of the refactor that `docs/review-2026-11.md` planned, from `1d073ff` (before) to `7cb25ee` (after), plus a diagnosis of Claude Code steering. Behavior is meant to stay the same; a small behavior change is worth proposing only when it buys a large simplification.

Each item below is meant to be one commit. Tick it off here in the commit that does it, and say in the entry what was done and what was deliberately left undone. Line numbers are from 2026-11-13 and will move; find the code by name.

**Decided, not to be reopened:**

- The import style stays as it is: module level spells crate paths out in full, as `AGENTS.md` states. No import-convention change.
- No new type aliases (e.g. none for `GlobalData<AppState, AppSignal>`).

## Order

1. Regressions R1–R5, then the steering fix S1–S2: one commit each, small and independent.
2. The half-done plan steps (section "Left half done").
3. The fresh findings (section "Fresh findings").
4. The documentation fixes last.

## Steering with Claude Code

- [ ] **S1. Close a Claude Code run only when its background tasks are done.** `src/agent/claude_code.rs` (`run`, `parse_line`), `src/agent/turn/live.rs` (`LiveTurn.closed`). The driver treats the CLI's first `result` line as the end of the run: within 250 ms it closes stdin and sets `live.closed = true`. Current Claude Code often keeps going: background tasks (agents, background Bash) finish later, the CLI gets a `<task-notification>` and starts another model turn. The window shows the turn as running for minutes, but `inject` refuses every message (`if live.closed { return false; }`) and `send` queues it as a new turn. Seen in session `327a9054`: first `end_turn` 30 s in, task-notification turns for 3 more minutes. It predates the refactor (`062b513`, `865d93d`, `414a4e9`). Fix: `parse_line` maps `{"type":"system","subtype":"task_started","task_id":…}` to `StreamEvent::TaskStarted(id)` and `{"type":"system","subtype":"task_notification","task_id":…}` to `StreamEvent::TaskDone(id)`; `run` keeps the set of outstanding task ids; `TaskDone` also clears `settled`, since the CLI answers the notification with another `result`; close only when `settled && outstanding.is_empty()`. Test the parsing and the close rule.
- [ ] **S2. Reopen the injector at the start of every run.** `LiveTurn.closed` is never reset. The commit-nudge runs (`turn/mod.rs`, around `:357`) and the retry after `SessionGone` (`claude_code.rs`, around `:75`) reuse the recorder of the previous run, already closed, so a commit turn cannot be steered during its nudges. Fix: add `Injector::open()` (it does not exist yet), which sets `live.closed = false`, and call it at the start of `run`, before the first write.

## Regressions from the refactor

- [x] **R1. A `features` change reopens every file ever opened.** `src/lsp/worker.rs` (`restart_server`, around `:270-300`, step 13). After the change it sends `didOpen` and full token requests for every file the warmup opened (every `.rs` file, never closed), back to back without draining stdout: the pipe deadlock `send_warmup` exists to avoid, and a duplicate of the paced warmup re-queued just above. Fix: delete the reopen loop, and re-request only the open `FilePreview` windows instead, as `1d073ff:src/tui/app.rs` (around `:1785-1815`) did after its restart: `lsp::send_file_request` for each `Window::FilePreview` in the window stack, sent once the new worker was subscribed. At HEAD nothing does this: the `SetCargoFeatures` arm in `src/tui/app/signals.rs` only stores the selection, so the new code has to add it, ordered after the restart. Done: `restart_server` drops the `reopened` collection and its loop, leaving the warmup re-queue as the only path re-requesting what it opened; the `SetCargoFeatures` arm asks again for every `Window::FilePreview` of the window stack right after `lsp::set_cargo_features`, so the restart precedes them in the worker's queue (or in the buffer, which is replayed in order). Left undone: no test covers a restart itself (it needs a real Cargo project and would flake, and driving `signals::handle` would leak entries into the process-wide request buffer); a manual `features` run should confirm the new server gets one paced warmup and no `didOpen` burst.
- [x] **R2. An engine restart forgets the selected cargo features.** `src/lsp/mod.rs` (around `:38-44`), `src/tui/app/signals.rs` (around `:56-60`) (step 13). Any engine-level restart (rust-analyzer crash, read error) rebuilds the worker from the startup config; the features revert while the UI still shows the new selection, and a code comment admits it (`src/lsp/worker.rs` around `:296-298`). Done: a process-wide store in `src/lsp/mod.rs` (`CARGO_FEATURES`, `cargo_features`, `set_cargo_features`), read by `create_and_register_os_sources`; `restart_server` needed no change since it spawns from the selection it is given. Three deviations from the plan: **an `Option` behind a `parking_lot::const_mutex`, not an `ArcSwap`** — `None` says "no command has selected anything yet" and makes the seed fall back to the config the worker was subscribed with, so a fresh start still honours the session's saved selection without seeding the store at subscription; **the store is written when the command is issued, not when a spawn succeeds** — a selection rust-analyzer refuses to start under is kept (as `restart_server` keeps the old process) and retried once more by the next engine restart, within the engine's budget of 3, where storing on success would keep the last working selection at the cost of a `remember` call in `restart_server`; and **`LspConfig::cargo_features` stays** as the seed, so `src/tui/app/init.rs` is untouched.
- [x] **R3. Overlapping watcher batches lose or duplicate files.** `src/watcher.rs` (around `:206`), `src/loader/watch.rs` (around `:63-190`) (step 22). Paths are resolved to keys on the watcher thread against the list as it was, and swapped in later on the main thread. When two batches overlap (the main thread takes more than 50 ms), a file created in batch 1 and modified, removed or re-created in batch 2 loses the edit, outlives its deletion, or is appended twice. Fix: keep reading on the watcher thread, but resolve paths to keys in `swap_in` against the current list, and check that an appended path is not already present. Test the overlap. Done: the read keeps the key it found only as a hint and carries the path, and `swap_in` resolves every path again through `resolve_key` — the read's key when it found one, since the list is append-only, else the first match in the tail grown since `Pending::read_len`, so an overlapping batch appends nothing twice. `resurrected` and `appended` merged into one `created` list, decided at publication since only the list then knows whether a path is new; `read` now reads through `read_file_data` everywhere, never `LoadedFile::load`, so a file is read once (`LoadedFile::from_data`). Three tests cover the overlap: an edit lands, a deletion marks removed, two `created` batches append once.
- [x] **R4. Tab escapes the command line when its palette is empty.** `src/tui/command_line.rs` (`CommandLine::handle_event`, Tab and BackTab arms, commit `5dda7ee`). With no palette entries (after any space, after `!`, or on an exact command name) the arms fall out of the `match`, and the global keymap cycles pane focus behind the open line. Fix: return `true` after the `if !entries.is_empty()` block in both arms. Done: both arms return `true` whatever the palette holds, as the catch-all arm did before `5dda7ee` took the guard off the arm; `tab_stays_with_the_line_when_the_palette_is_empty` covers Tab and Shift+Tab.
- [ ] **R5. A Discussion opened on a running turn shows stale content.** `src/tui/state.rs` (around `:223`, `:250-257`) (step 16). The first render copies the live turn and records `live_revision`; then `NodeContentLoaded` → `show_content` replaces the texts with the disk copy without resetting it, so `mirror` skips copying until the turn next changes, which can be minutes during a long tool call. Meanwhile there is no thinking, dropped rounds reappear, and the reply can be up to 1 s old. Fix: `self.live_revision = 0` in `show_content` and in `apply_sent`.

## Behavior changes to keep or revert

These looked fine to the review; the owner decides. Nothing to do unless one is reverted.

- [ ] **C1. Idle wakeups.** The 100 ms spinner tick (step 15) runs for the whole session and renders only while a turn runs, so an idle app wakes 11 times a second instead of once. Proposed: run the tick only while `running_turns` is non-empty.
- [ ] **C2. `fmt-md .` still formats the whole repository.** The guard from `2352de8` refuses `""` only. Either refuse any path that resolves to the repository root, or drop the guard.

Accepted as they are:

- LSP token colors (B4, intended): `comment`, `modifier`, `const`/`static`, `namespace`, `parameter`, `property`/`enumMember` and `lifetime` now use their more specific scopes (`comment` used to be painted in the string color).
- Session save runs on the 5 s housekeeping tick instead of 2 s after the last change; the save on exit is unchanged.
- The report pane gained Up/Down/Home/End/Ctrl+A/E, the commit diff Ctrl+A/E.
- Focus repair during a render releases the terminal grab and sends focus-in/out bytes, consistent with the grab rule.
- The watcher drops an unreadable file of a batch instead of keeping its old content.
- Config and auth paths need a UTF-8 home directory, as conversations already did.

## Left half done

- [ ] **H1. Step 9: move the `SymbolHighlight` handler.** The 85-line handler is still in `src/tui/app/signals.rs` (around `:98-182`); only `find`/`dedup` moved to `tui/symbols.rs`. Move it there.
- [ ] **H2. Step 8: make nine `tui` modules private.** `after_agents`, `command_line`, `focus`, `pane_component`, `pane_manager`, `panes`, `panes_renderer`, `widgets` and `symbols` are `pub(crate)` though nothing outside `tui` names them.
- [ ] **H3. Step 10: the Discussion still imports a pane.** It uses `panes::preview::DragModifier` and `ensure_readable_fg`; move both to `widgets/`. Its 60-line row painter duplicates `widgets::markdown::paint_row`; use that one.
- [ ] **H4. Step 12 / B5: cancel running sub-agents on quit.** The exit loop never calls `cancel_running_agents`, so a sub-agent running at quit loads as `Failed "interrupted"` next time. Predates the refactor; one line next to `mark_cut_short` in the exit loop (`src/tui/app/mod.rs` around `:250`), called as `agent/turn/mod.rs` does around `:179`.
- [ ] **H5. Step 23: `retitle_selected` reads `reply.md` before the cheap checks.** Do the checks first.
- [ ] **H6. Step 16: clear the work-log selection when its text changes.** It is not cleared when the text is replaced from disk, a prompt is sent, or Ctrl+O toggles dropped rounds, so B2-style offset mismatches are still possible.

## Fresh findings

- [ ] **F1. One shared HTTP client.** `src/util/http.rs::get` builds a `reqwest::Client` per call (two a minute for OpenRouter, one per turn end). Use a static `OnceLock` client, as `llm.rs` does.
- [ ] **F2. Preview.** Its word-snapping drag duplicates `util::text::drag_range` (about 25 lines). Wrapping is implemented twice (render and `screen_pos_to_line_char`), and the second recounts every line's chars on each drag event. `ui_fg("ui.text")` is looked up per interval; hoist it out of the loop.
- [ ] **F3. `mirror` compares the whole work log.** `work_log.starts_with` compares every entry's bytes on every revision; entries are never rewritten, so length plus the last entry is enough, but only while the window shows the turn the live copy belongs to: a node swapped under the window (`show_content`, `src/tui/state.rs` around `:250`) keeps the full comparison. `append_str` (`src/tui/state.rs` around `:274`) makes a needless `to_string()`.
- [ ] **F4. The report pane paints every visible row twice** (about 20% of its draw ops).
- [ ] **F5. Small tidy-ups.** Dead arms in `src/tui/app/signals.rs` (around `:91-97`). `GroupGuard::new` (`src/util/process.rs` around `:22`) takes a `u32`, so its callers wrap `child.id()` in `.map`; let it take the `Option<u32>` that `child.id()` returns. `Run::from` (`src/agent/claude_code.rs` around `:55`) retries once after `SessionGone` with a `loop` and an `anchored` flag; restructure it without them, still retrying at most once. `src/lsp/tokens.rs` (around `:72`) counts lines with a full scan on every token request; `line_starts.len()` already has the count, except for an empty file: `compute_line_starts` gives `[0]` for `""`, where `lines().count()` is 0, so keep that case.

## Documentation

- [ ] **D1. `AGENTS.md`.** It still says (line 329) "a new provider gets its own `EndpointKind`"; a new provider is a `Provider` variant (`src/endpoint.rs`), and `EndpointKind` stays the transport of an endpoint. The other claim, that the app "handles global text selection", was already removed by `7cb25ee`: nothing to do there.
- [ ] **D2. `docs/review-2026-11.md`.** The step 4 and B1 notes describe code that later commits changed; bring them in line or mark them superseded. Step 4 says the lsp legend is a `OnceLock<Vec<TokenKind>>`; it is now a `Vec<TokenKind>` field of `LspWorker` (`src/lsp/worker.rs` around `:43`). B1 says only `Tick` repaints; `TurnTick` repaints too while a turn runs (`src/tui/app/init.rs` around `:89-101`).
