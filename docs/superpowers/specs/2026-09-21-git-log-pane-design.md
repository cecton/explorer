# Git Log Pane + Commit Diff Panes — Design

Date: 2026-09-21
Status: approved

## Goal

`<leader>g` opens a unique, session-excluded **Git Log pane**: one line per commit of
HEAD (first-parent), datetime on the left, first line of the commit message next to it.
Up/Down navigates; ENTER opens a **Commit Diff pane** for that commit (one pane per
commit). Commit Diff panes are saved in sessions; the Git Log pane is not.

Git data is read with `gix` (gitoxide) — pure Rust, no `libgit2`.

## 1. Data layer — new `src/git.rs`

New dependency: `gix`.

```rust
pub struct LogEntry {
    pub id: [u8; 20],
    pub time_secs: i64,
    pub summary: String, // first line of commit message
}

pub fn load_log(repo_root: &Utf8Path) -> Result<Vec<LogEntry>, String>;
pub fn load_commit_diff(repo_root: &Utf8Path, id: [u8; 20]) -> Result<String, String>;
```

- `load_log`: open repo at root, peel HEAD, revwalk with `first_parent()`, newest
  first, all commits (no cap). Empty repo → `Ok(vec![])`.
- `load_commit_diff`: tree diff of the commit against its first parent; unified-diff
  text including `diff --git` / `---` / `+++` / `@@` headers. Empty diff (root commit
  with no files, merge-style) → `Ok(String::new())`; pane renders `(no diff)`.
- Both are pure functions suitable for `tokio::task::spawn_blocking`.
- Datetime rendering (`YYYY-MM-DD HH:MM`) is derived from `time_secs` without a new
  chrono dependency.

## 2. State — `src/tui/state.rs`

- `Window` gains variants:
  - `GitLog` — unique (at most one); `push_window` dedup handles this.
  - `CommitDiff([u8; 20])` — at most one per commit id; payload is `Copy + Eq + Hash`.
- `AppState` gains:
  - `git_log: GitLogState { entries: Option<Arc<Vec<LogEntry>>>, error: Option<String>, selected: usize }`
  - `commit_diffs: HashMap<[u8; 20], CommitDiffState { lines: Option<Arc<Vec<DiffLine>>>, error: Option<String> }>`
  - `pending_diffs: Vec<[u8; 20]>` — session-restore queue (mirrors `pending_terminals`).
- Diff text is split and classified once at load time:

```rust
pub enum DiffKind { FileHeader, File, Hunk, Add, Del, Context }
pub struct DiffLine { pub kind: DiffKind, pub text: String }
```

`FileHeader` covers `diff --git` and `Binary files` lines; `File` covers the
`---`/`+++` labels; the rest match their unified-diff prefixes.

- New `AppSignal` variants:
  - `GitLogLoaded(Result<Arc<Vec<LogEntry>>, String>)`
  - `CommitDiffLoaded { id: [u8; 20], result: Result<Arc<Vec<DiffLine>>, String> }`

## 3. Keymap — `src/keymap.rs`

- `Action::OpenGitLog`, slug `"open-git-log"`, default leader binding `g`.
- Handler arm in `app.rs` mirrors `OpenFilePicker`: ensure-unique
  `push_window(Window::GitLog)`, focus it, then spawn `load_log` via
  `spawn_blocking` (entries refresh on every open so a `git pull` in a terminal pane
  is reflected).

## 4. Components

### `src/tui/git_log_pane.rs` — `GitLogPaneComponent`

- Renders one line per entry: fixed-width datetime column (`YYYY-MM-DD HH:MM`), then
  summary. Selected row uses selection style.
- Loading / error / empty states render as single-line placeholder text.
- Scroll is PaneManager-owned (`scroll_max = entries.len()`), same as FilePreview —
  scrollbar, mouse drag, and wheel come from the parent `PaneComponent` for free.
- Input: single flat `match` on `InputEvent`.
  - Up/Down move selection with autoscroll; PgUp/PgDn/Home/End supported.
  - ENTER: ensure-unique `push_window(Window::CommitDiff(id))`, focus, spawn
    `load_commit_diff` via `spawn_blocking` if content absent; pane shows a loading
    placeholder until the signal arrives.
  - Esc: `remove_window(Window::GitLog)`.
  - Mouse click on a row selects it.
- One instance per pane (field on `PaneComponent`), like the pickers.

### `src/tui/commit_diff_pane.rs` — `CommitDiffPaneComponent`

- One instance per pane; resolves the active commit id from the pane's current
  `Window`, the same way FilePreview resolves its `FileKey`.
- Colors by `DiffKind`: FileHeader/File cyan, Hunk yellow, Add green, Del red, Context
  plain. Colors come from the theme's `diff.plus`/`diff.minus`/`diff.delta` scopes where
  defined, with hardcoded fallbacks.
- PaneManager-owned scroll, identical key handling to FilePreview
  (arrows/PgUp/PgDn/Home/End); Backspace sends the window to back (preview
  convention).

### `src/tui/pane_component.rs`

- New fields `git_log`, `commit_diff`; new arms in the five dispatch sites:
  `render_title_row`, scrollbar `apply_scroll`, `handle_event` tail, `render`,
  `has_scrollbar`.

### Titles and status bar

- Git Log pane title: `Git Log`. Commit Diff title: `commit <7-hex> <summary>`.
- Status-bar hint strings added for both window kinds (`render_status_bar`).

## 5. Session — `src/session.rs`

- `pane_entry_from_window`:
  - `Window::GitLog => None` (not saved).
  - `Window::CommitDiff(id) => Some(PaneEntry::CommitDiff { hash: hex(id), pane_size })`.
- New serde-tagged `PaneEntry::CommitDiff` variant (`kind = "commit_diff"`).
- `SESSION_VERSION` 2 → 3 (breaking change acceptable; no migration code).
- `Session::apply`: re-insert `CommitDiff` windows with a loading state and enqueue
  ids into `AppState.pending_diffs`; `app_start_background_services` drains the
  queue → `spawn_blocking` load → `CommitDiffLoaded` signal fills content.
- The Git Log pane itself is never restored.

## 6. Error handling

- Load functions return `Result<_, String>`; panes render the error inline. No
  panics on unusual repos (empty repo, unreadable objects).

## 7. Testing

- Hermetic unit tests:
  - `DiffLine` classification over a sample unified diff.
  - Datetime formatting from `time_secs`.
  - `PaneEntry::CommitDiff` serde roundtrip; hex id encode/decode.
- `load_log` / `load_commit_diff` verified manually via `cargo run --release` in
  this repository.
