# Git Log Pane + Commit Diff Panes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `<leader>g` opens a unique session-excluded Git Log pane (HEAD first-parent commits, datetime + summary); ENTER opens a per-commit Commit Diff pane with colored unified diff; Commit Diff panes persist in sessions.

**Architecture:** New `src/git.rs` data layer on `gix` (gitoxide) exposes `load_log` / `load_commit_diff`, run on `tokio::task::spawn_blocking` and delivered to the UI via two new `AppSignal` variants. Two new components (`GitLogPaneComponent`, `CommitDiffPaneComponent`) are dispatched by `PaneComponent` per active `Window` variant. Session persists `CommitDiff` panes as hex hashes and re-loads them via a `pending_diffs` queue drained in `app_start_background_services`.

**Tech Stack:** Rust (edition 2024), `gix = "0.87"` with `blob-diff` feature, r3bl_tui component/pipeline rendering, `hex`, `tokio::task::spawn_blocking`.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-09-21-git-log-pane-design.md`
- `Window` enum payloads must stay `Copy + Eq + Hash`; `CommitDiff([u8; 20])`.
- Every commit: run `cargo fmt` first, then `cargo clippy --no-deps` must be warning-free.
- No comments unless a non-obvious why; no `#[allow(dead_code)]`; no `.unwrap()` outside tests.
- No new datetime dependency — format `YYYY-MM-DD HH:MM` from unix seconds with a civil-from-days helper.
- Gix APIs used below are verified against gix 0.87.1 docs/source; do not substitute `git2` or CLI git.
- Every task must leave the workspace compiling (`cargo check` clean) before committing.
- Every intermediate commit that adds `Window` variants must also add arms to the exhaustive matches in `pane_component.rs` (5 sites), `session.rs::pane_entry_from_window`, and `app.rs::render_status_bar`.

---

### Task 1: `src/git.rs` — types + pure helpers (TDD)

**Files:**
- Create: `src/git.rs`
- Modify: `src/main.rs` (add `mod git;` after `mod config;`)

**Interfaces:**
- Produces (used by all later tasks):
  - `pub struct LogEntry { pub id: [u8; 20], pub time_secs: i64, pub summary: String }`
  - `pub enum DiffKind { FileHeader, File, Hunk, Add, Del, Context }` (derives `Clone, Copy, PartialEq, Eq, Debug`)
  - `pub struct DiffLine { pub kind: DiffKind, pub text: String }` (derives `Clone, PartialEq, Eq, Debug`)
  - `pub fn format_log_time(time_secs: i64) -> String` → `"YYYY-MM-DD HH:MM"` (UTC)
  - `pub fn classify_diff(text: &str) -> Vec<DiffLine>`
  - `pub fn short_id(id: &[u8; 20]) -> String` → 7-char hex prefix

- [ ] **Step 1: Add the module declaration**

In `src/main.rs`, after the line `mod config;` add:

```rust
mod git;
```

- [ ] **Step 2: Write the failing tests**

Create `src/git.rs` containing only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_log_time_renders_utc_datetime() {
        assert_eq!(format_log_time(0), "1970-01-01 00:00");
        assert_eq!(format_log_time(1_700_000_000), "2023-11-14 22:13");
        assert_eq!(format_log_time(-1), "1969-12-31 23:59");
    }

    #[test]
    fn classify_diff_assigns_kinds_by_prefix() {
        let lines = classify_diff(
            "diff --git a/src/lib.rs b/src/lib.rs\n\
             --- a/src/lib.rs\n\
             +++ b/src/lib.rs\n\
             @@ -1,3 +1,4 @@\n\
             \x20context line\n\
             -removed line\n\
             +added line\n\
             \\ No newline at end of file\n",
        );
        let kinds: Vec<DiffKind> = lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffKind::FileHeader,
                DiffKind::File,
                DiffKind::File,
                DiffKind::Hunk,
                DiffKind::Context,
                DiffKind::Del,
                DiffKind::Add,
                DiffKind::Context,
            ]
        );
        assert_eq!(lines[0].text, "diff --git a/src/lib.rs b/src/lib.rs");
        assert_eq!(lines[5].text, "-removed line");
    }

    #[test]
    fn classify_diff_handles_empty_and_added_files() {
        assert!(classify_diff("").is_empty());
        let lines = classify_diff("Binary files a/img.png and b/img.png\n");
        assert_eq!(lines[0].kind, DiffKind::FileHeader);
    }

    #[test]
    fn short_id_is_seven_hex_chars() {
        let id = [0xabu8; 20];
        assert_eq!(short_id(&id), "abababa");
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test git::`
Expected: compile error — `format_log_time`, `classify_diff`, `short_id` not found.

- [ ] **Step 4: Implement the types and helpers**

Add above the test module in `src/git.rs`:

```rust
use bstr-ish-free-zone-do-not-use;
```

No — use only std + hex. The actual content:

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffKind {
    FileHeader,
    File,
    Hunk,
    Add,
    Del,
    Context,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub id: [u8; 20],
    pub time_secs: i64,
    pub summary: String,
}

pub fn format_log_time(time_secs: i64) -> String {
    let days = time_secs.div_euclid(86_400);
    let secs_of_day = time_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

pub fn classify_diff(text: &str) -> Vec<DiffLine> {
    text.lines()
        .map(|line| {
            let kind = if line.starts_with("diff --git ") || line.starts_with("Binary files ") {
                DiffKind::FileHeader
            } else if line.starts_with("@@") {
                DiffKind::Hunk
            } else if line.starts_with("+++") || line.starts_with("---") {
                DiffKind::File
            } else if line.starts_with('+') {
                DiffKind::Add
            } else if line.starts_with('-') {
                DiffKind::Del
            } else {
                DiffKind::Context
            };
            DiffLine {
                kind,
                text: line.to_string(),
            }
        })
        .collect()
}

pub fn short_id(id: &[u8; 20]) -> String {
    format!("{}{:x}", hex::encode(&id[..3]), id[3] >> 4)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
```

Note: the test input uses `\x20` (a literal space) for the context line so the leading-space context prefix survives source formatting; `classify_diff` sees `" context line"`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test git::`
Expected: 4 tests pass.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt && cargo clippy --no-deps && git add src/git.rs src/main.rs
git commit -m "feat(git): add diff/log types and pure helpers"
```

---

### Task 2: gix dependency + `load_log` / `load_commit_diff`

**Files:**
- Modify: `Cargo.toml` (add `gix` dependency)
- Modify: `src/git.rs`

**Interfaces:**
- Consumes: `LogEntry`, `DiffKind`-free — `classify_diff` is applied by callers, not here.
- Produces:
  - `pub fn load_log(repo_root: &Utf8Path) -> Result<Vec<LogEntry>, String>` (newest first)
  - `pub fn load_commit_diff(repo_root: &Utf8Path, id: [u8; 20]) -> Result<String, String>` (unified-diff text)

- [ ] **Step 1: Add the dependency**

In `Cargo.toml`, under `[dependencies]`, after the `hex = "0.4"` line add:

```toml
gix = { version = "0.87", features = ["blob-diff"] }
```

- [ ] **Step 2: Write the failing test (uses this repo, which is a git repo)**

Add to the existing `mod tests` in `src/git.rs`:

```rust
    #[test]
    fn load_log_walks_this_repository_head() {
        let root = camino::Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let entries = load_log(&root).expect("load_log");
        assert!(!entries.is_empty(), "this repo has commits");
        assert!(
            entries.iter().all(|e| !e.summary.is_empty()),
            "every entry has a non-empty summary"
        );
        assert!(entries[0].id != [0u8; 20]);
    }

    #[test]
    fn load_commit_diff_renders_unified_text_for_head() {
        let root = camino::Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let entries = load_log(&root).expect("load_log");
        let diff = load_commit_diff(&root, entries[0].id).expect("load_commit_diff");
        let lines = classify_diff(&diff);
        if !lines.is_empty() {
            assert_eq!(lines[0].kind, DiffKind::FileHeader);
            assert!(lines[0].text.starts_with("diff --git"));
        }
    }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test git::`
Expected: compile error — `load_log`, `load_commit_diff` not found.

- [ ] **Step 4: Implement the loaders**

Add to `src/git.rs` (above the tests module), with these imports at the top of the file:

```rust
use camino::Utf8Path;
use gix::diff::blob::Algorithm;
use gix::ObjectId;
use gix::objs::tree::EntryMode;
use gix::object::tree::diff::ChangeDetached;
```

The functions:

```rust
pub fn load_log(repo_root: &Utf8Path) -> Result<Vec<LogEntry>, String> {
    let repo = gix::open(repo_root.as_std_path()).map_err(|e| format!("open repository: {e}"))?;
    let head = repo.head_commit().map_err(|e| format!("resolve HEAD: {e}"))?;
    let walk = head
        .ancestors()
        .first_parent_only()
        .all()
        .map_err(|e| format!("revision walk: {e}"))?;

    let mut entries = Vec::new();
    for info in walk {
        let info = info.map_err(|e| format!("revision walk: {e}"))?;
        let commit = info
            .object()
            .map_err(|e| format!("read commit {}: {e}", info.id))?;
        let time_secs = commit.committer().map(|sig| sig.seconds()).unwrap_or(0);
        let first_line = commit.message_raw_sloppy().split(|&b| b == b'\n').next().unwrap_or(b"");
        let mut id = [0u8; 20];
        id.copy_from_slice(info.id.as_bytes());
        entries.push(LogEntry {
            id,
            time_secs,
            summary: String::from_utf8_lossy(first_line).into_owned(),
        });
    }
    Ok(entries)
}

pub fn load_commit_diff(repo_root: &Utf8Path, id: [u8; 20]) -> Result<String, String> {
    let repo = gix::open(repo_root.as_std_path()).map_err(|e| format!("open repository: {e}"))?;
    let commit = repo
        .find_commit(ObjectId::from(id))
        .map_err(|e| format!("find commit: {e}"))?;

    let parent_tree = match commit.parent_ids().next() {
        Some(parent_id) => repo
            .find_commit(parent_id)
            .and_then(|parent| parent.tree())
            .map_err(|e| format!("parent tree: {e}"))?,
        None => repo.empty_tree().map_err(|e| format!("empty tree: {e}"))?,
    };
    let tree = commit.tree().map_err(|e| format!("commit tree: {e}"))?;

    let mut options = gix::diff::Options::default();
    options.track_rewrites(None);
    let changes = repo
        .diff_tree_to_tree(Some(&parent_tree), Some(&tree), Some(options))
        .map_err(|e| format!("tree diff: {e}"))?;
    let algorithm = repo
        .diff_algorithm()
        .map_err(|e| format!("diff algorithm: {e}"))?;

    let mut out = String::new();
    for change in changes {
        let (location, old_id, new_id, entry_mode) = match change {
            ChangeDetached::Modification {
                location,
                previous_id,
                id,
                entry_mode,
                ..
            } => (location, Some(previous_id), Some(id), entry_mode),
            ChangeDetached::Addition {
                location,
                id,
                entry_mode,
                ..
            } => (location, None, Some(id), entry_mode),
            ChangeDetached::Deletion {
                location,
                id,
                entry_mode,
                ..
            } => (location, Some(id), None, entry_mode),
            ChangeDetached::Rewrite {
                location,
                source_id,
                id,
                entry_mode,
                ..
            } => (location, Some(source_id), Some(id), entry_mode),
        };
        let path = String::from_utf8_lossy(&location).into_owned();
        if matches!(entry_mode, EntryMode::Blob | EntryMode::BlobExecutable) {
            file_diff(&repo, algorithm, &path, old_id, new_id, &mut out)?;
        }
    }
    Ok(out)
}

fn file_diff(
    repo: &gix::Repository,
    algorithm: Algorithm,
    path: &str,
    old_id: Option<ObjectId>,
    new_id: Option<ObjectId>,
    out: &mut String,
) -> Result<(), String> {
    let old_data = match old_id {
        Some(id) => blob_data(repo, id)?,
        None => Vec::new(),
    };
    let new_data = match new_id {
        Some(id) => blob_data(repo, id)?,
        None => Vec::new(),
    };

    let old_label = if old_id.is_none() {
        "/dev/null".to_string()
    } else {
        format!("a/{path}")
    };
    let new_label = if new_id.is_none() {
        "/dev/null".to_string()
    } else {
        format!("b/{path}")
    };
    out.push_str(&format!("diff --git a/{path} b/{path}\n"));
    out.push_str(&format!("--- {old_label}\n"));
    out.push_str(&format!("+++ {new_label}\n"));

    let binary = old_data.contains(&0)
        || new_data.contains(&0)
        || std::str::from_utf8(&old_data).is_err()
        || std::str::from_utf8(&new_data).is_err();
    if binary {
        out.push_str("Binary files differ\n");
        return Ok(());
    }

    let input = gix::diff::blob::InternedInput::new(old_data.as_slice(), new_data.as_slice());
    let diff = gix::diff::blob::diff_with_slider_heuristics(algorithm, &input);
    let unified = gix::diff::blob::UnifiedDiff::new(
        &diff,
        &input,
        gix::diff::blob::unified_diff::ConsumeBinaryHunk::new(String::new(), "\n"),
        gix::diff::blob::unified_diff::ContextSize::default(),
    );
    let hunks = unified.consume().map_err(|e| format!("render diff: {e}"))?;
    out.push_str(&hunks);
    Ok(())
}

fn blob_data(repo: &gix::Repository, id: ObjectId) -> Result<Vec<u8>, String> {
    Ok(repo
        .find_object(id)
        .map_err(|e| format!("find object {id}: {e}"))?
        .try_into_blob()
        .map_err(|e| format!("load blob {id}: {e}"))?
        .data)
}
```

Notes for the implementer:
- `repo.diff_algorithm()` returns `gix::diff::blob::Algorithm` (imara's `Algorithm` re-exported through `gix::diff::blob`, which is `pub use imara_diff::*`).
- `UnifiedDiff` is re-exported at `gix::diff::blob::UnifiedDiff`; `ConsumeBinaryHunk`/`ContextSize` live at `gix::diff::blob::unified_diff::*`. `ContextSize::default()` is 3 context lines. `consume()` returns `io::Result<String>`; the UTF-8 pre-check above guarantees the delegate's `to_str` cannot fail.
- If `repo.empty_tree()` has a different arity in the resolved version, the equivalent is `repo.find_object(ObjectId::empty_tree(repo.object_hash()))` unwrapped to a `Tree`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test git::`
Expected: 6 tests pass.

- [ ] **Step 6: Format, lint, commit**

```bash
cargo fmt && cargo clippy --no-deps && git add Cargo.toml Cargo.lock src/git.rs
git commit -m "feat(git): load head log and commit diffs via gix"
```

---

### Task 3: State layer + Window variants + session persistence

**Files:**
- Modify: `src/tui/pane_manager.rs` (Window enum)
- Modify: `src/tui/state.rs` (GitLogState, CommitDiffState, AppState fields, AppSignal variants)
- Modify: `src/session.rs` (PaneEntry variant, save filter, restore, version bump)
- Modify: `src/tui/pane_component.rs` (temporary compile-fix arms; replaced by Tasks 4–5)
- Modify: `src/tui/app.rs` (`render_status_bar` temporary arms)

**Interfaces:**
- Consumes: `crate::git::{LogEntry, DiffLine}` from Task 1.
- Produces (used by Tasks 4–5):
  - `Window::GitLog`, `Window::CommitDiff([u8; 20])`
  - `AppState.git_log: GitLogState` — `GitLogState { entries: Option<Arc<Vec<LogEntry>>>, error: Option<String>, selected: usize }`, all `pub`, derives `Clone, Default`
  - `AppState.commit_diffs: HashMap<[u8; 20], CommitDiffState>` — `CommitDiffState { lines: Option<Arc<Vec<DiffLine>>>, error: Option<String> }`, all `pub`, derives `Clone, Default`
  - `AppState.pending_diffs: Vec<[u8; 20]>`
  - `AppSignal::GitLogLoaded(Result<Arc<Vec<LogEntry>>, String>)`
  - `AppSignal::CommitDiffLoaded { id: [u8; 20], result: Result<Arc<Vec<DiffLine>>, String> }`
  - `PaneEntry::CommitDiff { hash: String, pane_size: SerializablePaneSize }` (serde `kind = "commit_diff"`)

- [ ] **Step 1: Extend `Window`**

In `src/tui/pane_manager.rs`, update the doc comment on the enum (line 8-11) to read:

```rust
/// A pane that can appear in the window stack.
///
/// Each variant is unique: there is at most one `FileNamePicker`, one `ThemePicker`, one
/// `GitLog`, and at most one `FilePreview`/`CommitDiff` per key (file/commit id) in the
/// stack at any time.
```

and add two variants at the end of the `Window` enum:

```rust
    /// Git log pane listing HEAD (first-parent) history. Unique.
    GitLog,
    /// Unified diff of a single commit, identified by its object id.
    CommitDiff([u8; 20]),
```

- [ ] **Step 2: Add state types and fields**

In `src/tui/state.rs`, add to the existing imports (top of file, after the `crate::loader` line):

```rust
use crate::git::{DiffLine, LogEntry};
```

Add the two state structs (after the `FuzzyPickerState` impl block, before `pub struct TerminalPane`):

```rust
#[derive(Clone, Default)]
pub struct GitLogState {
    pub entries: Option<Arc<Vec<LogEntry>>>,
    pub error: Option<String>,
    pub selected: usize,
}

#[derive(Clone, Default)]
pub struct CommitDiffState {
    pub lines: Option<Arc<Vec<DiffLine>>>,
    pub error: Option<String>,
}
```

Add three fields to `AppState` (after the `pub pending_terminals: ...` field):

```rust
    /// Git log entries for the (single, session-excluded) git log pane.
    pub git_log: GitLogState,
    /// Loaded commit diffs keyed by commit object id.
    pub commit_diffs: HashMap<[u8; 20], CommitDiffState>,
    /// Commit-diff windows restored from the session that still need a diff loaded.
    pub pending_diffs: Vec<[u8; 20]>,
```

and initialize them in `AppState::new`'s struct literal (after `pending_terminals: HashMap::new(),`):

```rust
            git_log: GitLogState::default(),
            commit_diffs: HashMap::new(),
            pending_diffs: Vec::new(),
```

Add the two `AppSignal` variants (after the `SetCargoFeatures` variant, before `#[default] Noop`):

```rust
    /// Result of an async `git::load_log` run for the git log pane.
    GitLogLoaded(Result<Arc<Vec<LogEntry>>, String>),
    /// Result of an async `git::load_commit_diff` run for `id`.
    CommitDiffLoaded {
        id: [u8; 20],
        result: Result<Arc<Vec<DiffLine>>, String>,
    },
```

- [ ] **Step 3: Compile-fix the exhaustive matches (temporary arms)**

`src/session.rs` — in `pane_entry_from_window`, change:

```rust
        Window::FileNamePicker | Window::ThemePicker => None,
```

to:

```rust
        Window::FileNamePicker | Window::ThemePicker | Window::GitLog => None,
        Window::CommitDiff(_) => None,
```

(The `CommitDiff` arm is completed in Step 5.)

`src/tui/pane_component.rs` — in `impl TitleRow for PaneComponent::render_title_row`, after the `Window::Terminal(_)` arm add:

```rust
            Window::GitLog | Window::CommitDiff(_) => 0,
```

in `Component::handle_event`'s tail `match active_window`, after the `Some(Window::Terminal(_))` arm add:

```rust
            Some(Window::GitLog | Window::CommitDiff(_)) => Ok(EventPropagation::Propagate),
```

in `Component::render`'s dispatch, after the `Some(Window::Terminal(_))` arm add:

```rust
                Some(Window::GitLog | Window::CommitDiff(_)) => {}
```

in `apply_scroll`, add a new arm after the `Window::FilePreview(_)` arm:

```rust
            Window::GitLog | Window::CommitDiff(_) => {
                state.pane_manager.set_window_scroll(window, target);
                state.pane_manager.clamp_scroll(window);
                EventPropagation::ConsumedRender
            }
```

`src/tui/app.rs` — in `render_status_bar`, the `rest` match (line ~1514) gets one arm added before the final `Some(Window::Terminal(_)) | None => ""` arm:

```rust
                Some(Window::GitLog) | Some(Window::CommitDiff(_)) => "",
```

- [ ] **Step 4: Verify compile + run full test suite**

Run: `cargo check && cargo test`
Expected: clean check; all tests pass (no behavior change yet).

- [ ] **Step 5: Persist CommitDiff panes**

`src/session.rs`:

1. Bump the version (line 14):

```rust
pub const SESSION_VERSION: u32 = 3;
```

2. Add the `PaneEntry` variant after the `Terminal` variant:

```rust
    #[serde(rename = "commit_diff")]
    CommitDiff {
        hash: String,
        pane_size: SerializablePaneSize,
    },
```

3. In `pane_entry_from_window`, replace the temporary `Window::CommitDiff(_) => None,` arm with:

```rust
        Window::CommitDiff(id) => Some(PaneEntry::CommitDiff {
            hash: hex::encode(id),
            pane_size: window_state.pane_size.into(),
        }),
```

4. In `Session::apply`'s `for entry in self.panes` loop, add a new arm after the `PaneEntry::Terminal` arm:

```rust
                PaneEntry::CommitDiff { hash, pane_size } => {
                    let Ok(bytes) = hex::decode(&hash) else {
                        continue;
                    };
                    let Ok(id) = <[u8; 20]>::try_from(bytes.as_slice()) else {
                        continue;
                    };
                    let window = Window::CommitDiff(id);
                    new_stack.push(window);
                    new_states.insert(
                        window,
                        WindowState {
                            pane_size: pane_size.into(),
                            ..Default::default()
                        },
                    );
                    state.pending_diffs.push(id);
                }
```

- [ ] **Step 6: Write session tests**

Add to `mod tests` in `src/session.rs`. First, the existing `session_from_state_preserves_window_stack_order` test maps pane kinds with a two-arm `match`; extend its `kinds` closure to:

```rust
                PaneEntry::CommitDiff { .. } => "commit_diff",
```

Then add:

```rust
    #[test]
    fn commit_diff_pane_round_trips_through_session() {
        let (_dir, root) = temp_root();
        let mut state = empty_state(root.clone());
        let id = [0x42u8; 20];
        state.pane_manager.window_stack = vec![Window::CommitDiff(id)];
        state.pane_manager.window_states.insert(
            Window::CommitDiff(id),
            WindowState {
                pane_size: PaneSize::Half,
                ..Default::default()
            },
        );

        let session = Session::from_state(&state);
        assert_eq!(session.version, 3);
        let kinds: Vec<_> = session
            .panes
            .iter()
            .map(|entry| match entry {
                PaneEntry::FilePreview { .. } => "file_preview",
                PaneEntry::Terminal { .. } => "terminal",
                PaneEntry::CommitDiff { .. } => "commit_diff",
            })
            .collect();
        assert_eq!(kinds, vec!["commit_diff"]);
        let PaneEntry::CommitDiff { hash, .. } = &session.panes[0] else {
            panic!("expected commit_diff entry");
        };
        assert_eq!(hash, &hex::encode([0x42u8; 20]));

        let mut restored = empty_state(root.clone());
        session.apply(&mut restored);
        assert_eq!(
            restored.pane_manager.window_stack,
            vec![Window::CommitDiff(id)]
        );
        assert_eq!(restored.pending_diffs, vec![id]);
    }

    #[test]
    fn git_log_pane_is_not_saved() {
        let (_dir, root) = temp_root();
        let mut state = empty_state(root.clone());
        state.pane_manager.window_stack = vec![Window::GitLog];

        let session = Session::from_state(&state);
        assert!(session.panes.is_empty());
    }
```

Note: `hex` is already a main dependency; `use` it via full path (`hex::encode`) — no new import needed.

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test session::`
Expected: all session tests pass including the two new ones.

- [ ] **Step 8: Format, lint, commit**

```bash
cargo fmt && cargo clippy --no-deps && git add src/session.rs src/tui/state.rs src/tui/pane_manager.rs src/tui/pane_component.rs src/tui/app.rs
git commit -m "feat(session): persist commit diff panes and add git window state"
```

---

### Task 4: Keymap action + Git Log pane component

**Files:**
- Modify: `src/keymap.rs`
- Create: `src/tui/git_log_pane.rs`
- Modify: `src/tui/mod.rs`
- Modify: `src/tui/pane_component.rs`
- Modify: `src/tui/app.rs` (leader arm + `GitLogLoaded` signal arm + status hints)

**Interfaces:**
- Consumes: everything from Tasks 1–3.
- Produces:
  - `Action::OpenGitLog` (slug `"open-git-log"`, default leader `g`)
  - `GitLogPaneComponent::new(id: FlexBoxId) -> Self` implementing `Component<AppState, AppSignal>` + `TitleRow`
  - ENTER opens `Window::CommitDiff(id)`, seeds `state.commit_diffs` and spawns `load_commit_diff` if absent (this is the Task 5 contract: the `CommitDiffLoaded` signal is already defined in Task 3).

- [ ] **Step 1: Keymap action**

In `src/keymap.rs`:

1. Add to the `Action` enum (after `OpenThemePicker`):

```rust
    OpenGitLog,
```

2. Add to `Action::from_slug` (after `"open-theme-picker"` arm):

```rust
            "open-git-log" => Action::OpenGitLog,
```

3. Add to `Keymap::default()`'s `leader` vec (after the `OpenThemePicker` binding):

```rust
                (plain_char('g'), Action::OpenGitLog),
```

4. Extend `defaults_match_historical_bindings` test with:

```rust
        assert_eq!(
            km.leader_action(&plain_char('g')),
            Some(Action::OpenGitLog)
        );
```

Run: `cargo test keymap::` — expected pass.

- [ ] **Step 2: Create `src/tui/git_log_pane.rs`**

Full file content:

```rust
use crate::git::LogEntry;
use crate::git::format_log_time;
use crate::tui::*;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct GitLogPaneComponent {
    id: FlexBoxId,
    content_origin_row: usize,
    content_row_count: usize,
}

impl GitLogPaneComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self {
            id,
            content_origin_row: 0,
            content_row_count: 0,
        }
    }

    pub(crate) fn open_commit_diff(
        state: &mut AppState,
        main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
        id: [u8; 20],
    ) {
        if !state.commit_diffs.contains_key(&id) {
            state.commit_diffs.insert(id, CommitDiffState::default());
            let root = state.root.clone();
            tokio::task::spawn_blocking(move || {
                let result = crate::git::load_commit_diff(&root, id)
                    .map(|text| Arc::new(crate::git::classify_diff(&text)));
                send_signal!(
                    main_tx,
                    TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::CommitDiffLoaded {
                        id,
                        result,
                    })
                );
            });
        }
        let old = state.pane_manager.focused_window;
        let window = Window::CommitDiff(id);
        state.pane_manager.push_window(window);
        state.pane_manager.focused_window = Some(window);
        crate::tui::app::notify_terminal_focus_change(state, old, Some(window));
    }

    fn move_selection(state: &mut AppState, target: usize, count: usize) -> bool {
        if count == 0 {
            return false;
        }
        let clamped = target.min(count - 1);
        if clamped != state.git_log.selected {
            state.git_log.selected = clamped;
            return true;
        }
        false
    }
}

impl TitleRow for GitLogPaneComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        theme: &HelixTheme,
        _state: &AppState,
    ) -> usize {
        render_pane_title(ops, pane_box, "Git Log", false, theme, focused);
        1
    }
}

impl Component<AppState, AppSignal> for GitLogPaneComponent {
    fn reset(&mut self) {}

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        input_event: InputEvent,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        let state = &mut global_data.state;
        let count = state.git_log.entries.as_ref().map_or(0, |e| e.len());

        match &input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }) => {
                state.pane_manager.remove_window(&Window::GitLog);
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Enter),
            }) => {
                let id = state
                    .git_log
                    .entries
                    .as_ref()
                    .and_then(|e| e.get(state.git_log.selected))
                    .map(|entry| entry.id);
                if let Some(id) = id {
                    let main_tx = global_data.main_thread_channel_sender.clone();
                    Self::open_commit_diff(state, main_tx, id);
                    return Ok(EventPropagation::ConsumedRender);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Up),
            }) => {
                if Self::move_selection(state, state.git_log.selected.saturating_sub(1), count) {
                    return Ok(EventPropagation::ConsumedRender);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Down),
            }) => {
                if Self::move_selection(state, state.git_log.selected + 1, count) {
                    return Ok(EventPropagation::ConsumedRender);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::PageUp),
            }) => {
                let page = state
                    .pane_manager
                    .window_page_size(&Window::GitLog)
                    .saturating_sub(1)
                    .max(1);
                if Self::move_selection(state, state.git_log.selected.saturating_sub(page), count) {
                    return Ok(EventPropagation::ConsumedRender);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::PageDown),
            }) => {
                let page = state
                    .pane_manager
                    .window_page_size(&Window::GitLog)
                    .saturating_sub(1)
                    .max(1);
                if Self::move_selection(state, state.git_log.selected + page, count) {
                    return Ok(EventPropagation::ConsumedRender);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Home),
            }) => {
                if Self::move_selection(state, 0, count) {
                    return Ok(EventPropagation::ConsumedRender);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::End),
            }) => {
                if Self::move_selection(state, count.saturating_sub(1), count) {
                    return Ok(EventPropagation::ConsumedRender);
                }
            }
            InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::MouseDown(Button::Left),
                pos,
                ..
            }) => {
                let row = pos.row_index.as_usize();
                if row >= self.content_origin_row
                    && row < self.content_origin_row + self.content_row_count
                {
                    let rel = row - self.content_origin_row;
                    let scroll = state.pane_manager.window_scroll(&Window::GitLog);
                    if Self::move_selection(state, scroll + rel, count) {
                        return Ok(EventPropagation::ConsumedRender);
                    }
                }
            }
            _ => {}
        }

        Ok(EventPropagation::Propagate)
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        current_box: FlexBox,
        _surface_bounds: SurfaceBounds,
        _has_focus: &mut HasFocus,
    ) -> CommonResult {
        throws!({
            let origin = current_box.style_adjusted_origin_pos;
            let bounds = current_box.style_adjusted_bounds_size;
            let total_rows = bounds.row_height.as_usize();
            let pane_width = bounds.col_width.as_usize();
            self.content_origin_row = origin.row_index.as_usize();
            self.content_row_count = total_rows;

            let window = Window::GitLog;
            let state = &mut global_data.state;
            state.pane_manager.set_window_page_size(&window, total_rows);

            let entries = state.git_log.entries.clone();
            let count = entries.as_ref().map_or(0, |e| e.len());
            state.pane_manager.set_window_scroll_max(&window, count);
            state.pane_manager.clamp_scroll(&window);

            let mut scroll = state.pane_manager.window_scroll(&window);
            if count > 0 && total_rows > 0 {
                let selected = state.git_log.selected;
                if selected < scroll {
                    scroll = selected;
                } else if selected >= scroll + total_rows {
                    scroll = selected + 1 - total_rows;
                }
                state.pane_manager.set_window_scroll(&window, scroll);
            }

            let pane_bg = state.theme.ui_bg("ui.background").unwrap_or(DEFAULT_BG);
            let bg = TuiColor::from(pane_bg);
            let bg_style = new_style!(color_bg: {bg});
            let selected_bg = TuiColor::from(
                state
                    .theme
                    .ui_bg("ui.selection")
                    .unwrap_or(DEFAULT_SELECTION_BG),
            );
            let normal_fg = TuiColor::from(
                state
                    .theme
                    .ui_fg("ui.text")
                    .unwrap_or(RgbValue::from_u8(170, 170, 200)),
            );
            let error_fg = TuiColor::from(
                state
                    .theme
                    .ui_fg("error")
                    .unwrap_or(RgbValue::from_u8(220, 80, 80)),
            );
            let normal_text_style = new_style!(color_fg: {normal_fg} color_bg: {bg});
            let selected_text_style = new_style!(color_fg: {normal_fg} color_bg: {selected_bg});

            let mut render_ops = RenderOpIRVec::new();
            let selected = state.git_log.selected;
            let error = state.git_log.error.clone();

            if count == 0 {
                let message = if let Some(err) = error {
                    (format!("error: {err}"), error_fg)
                } else if entries.is_none() {
                    ("Loading git log…".to_string(), normal_fg)
                } else {
                    ("(no commits)".to_string(), normal_fg)
                };
                let message_style = new_style!(color_fg: {message.1} color_bg: {bg});
                for row_offset in 0..total_rows {
                    render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        col(0) + row(row_offset as u16),
                    );
                    render_ops += RenderOpCommon::ApplyColors(Some(bg_style));
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        " ".repeat(pane_width).into(),
                        Some(bg_style),
                    );
                    if row_offset == 0 {
                        render_ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
                        render_ops += RenderOpCommon::ApplyColors(Some(message_style));
                        render_ops +=
                            RenderOpIR::PaintTextWithAttributes(message.0.clone().into(), Some(message_style));
                    }
                }
            } else {
                let list = entries.as_ref().expect("count > 0 implies entries");
                for row_offset in 0..total_rows {
                    let entry_idx = scroll + row_offset;
                    render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        col(0) + row(row_offset as u16),
                    );
                    let is_selected = entry_idx == selected;
                    let row_style = if is_selected { selected_text_style } else { bg_style };
                    render_ops += RenderOpCommon::ApplyColors(Some(row_style));
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        " ".repeat(pane_width).into(),
                        Some(row_style),
                    );
                    let Some(entry) = list.get(entry_idx) else {
                        continue;
                    };
                    render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        col(0) + row(row_offset as u16),
                    );
                    render_ops += RenderOpCommon::ApplyColors(Some(row_style));
                    let text = log_row_text(entry, pane_width);
                    render_ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(row_style));
                }
            }

            global_data.pipeline.push(ZOrder::Normal, render_ops);
        });
    }
}

fn log_row_text(entry: &LogEntry, pane_width: usize) -> String {
    let mut text = format!("{} {}", format_log_time(entry.time_secs), entry.summary);
    text.truncate(pane_width.saturating_sub(1).max(0));
    text
}
```


- [ ] **Step 3: Register the module**

In `src/tui/mod.rs`: add `mod git_log_pane;` to the mod list (alphabetical, after `mod fuzzy_picker;`) and `use self::git_log_pane::*;` to the use list (after `use self::fuzzy_picker::*;`).

- [ ] **Step 4: Wire into `PaneComponent`**

In `src/tui/pane_component.rs`:

1. Add field `git_log: GitLogPaneComponent,` after `theme_picker: ThemePickerComponent,`.
2. In `new_boxed`, after the `theme_picker:` initializer line add: `git_log: GitLogPaneComponent::new(id),`.
3. In `reset()`, add `self.git_log.reset();` after `self.theme_picker.reset();`.
4. Replace the temporary `Window::GitLog | Window::CommitDiff(_) => 0,` title arm with:

```rust
            Window::GitLog => self
                .git_log
                .render_title_row(ops, pane_box, focused, theme, state),
            Window::CommitDiff(_) => 0,
```

5. Replace the temporary tail-event arm `Some(Window::GitLog | Window::CommitDiff(_)) => Ok(EventPropagation::Propagate),` with:

```rust
            Some(Window::GitLog) => {
                self.git_log
                    .handle_event(global_data, input_event, has_focus)
            }
            Some(Window::CommitDiff(_)) => Ok(EventPropagation::Propagate),
```

6. Replace the temporary render arm `Some(Window::GitLog | Window::CommitDiff(_)) => {}` with:

```rust
                Some(Window::GitLog) => {
                    self.git_log
                        .render(global_data, inner_bounds, surface_bounds, has_focus)?
                }
                Some(Window::CommitDiff(_)) => {}
```

- [ ] **Step 5: Dispatch `<leader>g` and the `GitLogLoaded` signal**

In `src/tui/app.rs`:

1. In the leader dispatch `match` (after the `Some(Action::OpenThemePicker)` arm), add:

```rust
                Some(Action::OpenGitLog) => {
                    let state = &mut global_data.state;
                    let old = state.pane_manager.focused_window;
                    state.pane_manager.push_window(Window::GitLog);
                    state.pane_manager.focused_window = Some(Window::GitLog);
                    notify_terminal_focus_change(state, old, Some(Window::GitLog));
                    let main_tx = global_data.main_thread_channel_sender.clone();
                    let root = state.root.clone();
                    tokio::task::spawn_blocking(move || {
                        let result = crate::git::load_log(&root).map(Arc::new);
                        send_signal!(
                            main_tx,
                            TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::GitLogLoaded(
                                result
                            ))
                        );
                    });
                    return Ok(EventPropagation::ConsumedRender);
                }
```

2. In `app_handle_signal`'s big `match action`, add after the `AppSignal::SetCargoFeatures(_)` arm:

```rust
                AppSignal::GitLogLoaded(result) => match result {
                    Ok(entries) => {
                        state.git_log.entries = Some(entries);
                        state.git_log.error = None;
                        let count = state
                            .git_log
                            .entries
                            .as_ref()
                            .map_or(0, |entries| entries.len());
                        if state.git_log.selected >= count {
                            state.git_log.selected = count.saturating_sub(1);
                        }
                    }
                    Err(e) => state.git_log.error = Some(e),
                },
                AppSignal::CommitDiffLoaded { id, result } => {
                    let entry = state.commit_diffs.entry(*id).or_default();
                    match result {
                        Ok(lines) => {
                            entry.lines = Some(lines);
                            entry.error = None;
                        }
                        Err(e) => entry.error = Some(e),
                    }
                }
```

3. In `render_status_bar`, extend the leader-mode hint format string: change

```rust
                "{}:Picker  {}:Term  {}:Theme  {}:Close  {}:Quit  {}:Next  {}:Prev  {}:Cancel",
```

arguments to include `leader_lbl(Action::OpenGitLog)` after the Theme label, e.g. `...  {}:Theme  {}:Git  {}:Close ...` with the new argument inserted after `leader_lbl(Action::OpenThemePicker),`.

4. Replace the temporary status-bar arm `Some(Window::GitLog) | Some(Window::CommitDiff(_)) => "",` with:

```rust
                Some(Window::GitLog) => "↑↓:Select  Enter:Diff  Esc:Close",
                Some(Window::CommitDiff(_)) => "Backspace:Send to back  ↑↓PgUp/PgDn:scroll",
```

- [ ] **Step 6: Build, test, format, lint, commit**

Run: `cargo test && cargo clippy --no-deps`
Expected: all pass, no warnings. (`git_log_pane.rs` has no unit tests — UI component; verification is build + manual smoke in Task 6.)

```bash
cargo fmt && git add src/keymap.rs src/tui/git_log_pane.rs src/tui/mod.rs src/tui/pane_component.rs src/tui/app.rs
git commit -m "feat(tui): git log pane with async head log loading"
```

---

### Task 5: Commit Diff pane component + session restore drain

**Files:**
- Create: `src/tui/commit_diff_pane.rs`
- Modify: `src/tui/mod.rs`
- Modify: `src/tui/pane_component.rs`
- Modify: `src/tui/app.rs` (background-services drain)

**Interfaces:**
- Consumes: `CommitDiffState`, `AppSignal::CommitDiffLoaded`, `DiffLine`/`DiffKind`, `GitLogPaneComponent::open_commit_diff` (already spawns loads).
- Produces:
  - `CommitDiffPaneComponent::new(id: FlexBoxId) -> Self` implementing `Component` + `TitleRow`
  - Session-restored `pending_diffs` drained in `app_start_background_services`.

- [ ] **Step 1: Create `src/tui/commit_diff_pane.rs`**

Full file content:

```rust
use crate::git::{DiffKind, short_id};
use crate::tui::*;

pub struct CommitDiffPaneComponent {
    id: FlexBoxId,
}

impl CommitDiffPaneComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self { id }
    }

    fn active_commit(&self, state: &AppState) -> Option<[u8; 20]> {
        let slot = pane_slot(self.id)?;
        match state.pane_manager.window_stack.get(slot)? {
            Window::CommitDiff(id) => Some(*id),
            _ => None,
        }
    }

    fn commit_summary(state: &AppState, id: [u8; 20]) -> Option<String> {
        state
            .git_log
            .entries
            .as_ref()?
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.summary.clone())
    }

    fn kind_color(theme: &HelixTheme, kind: DiffKind) -> RgbValue {
        let fallback = |r: u8, g: u8, b: u8| RgbValue::from_u8(r, g, b);
        match kind {
            DiffKind::FileHeader => theme
                .color_for_scope("function")
                .unwrap_or(fallback(137, 180, 250)),
            DiffKind::File => theme
                .color_for_scope("function")
                .unwrap_or(fallback(110, 130, 170)),
            DiffKind::Hunk => theme
                .color_for_scope("diff.delta")
                .unwrap_or(fallback(250, 200, 90)),
            DiffKind::Add => theme
                .color_for_scope("diff.plus")
                .unwrap_or(fallback(150, 220, 120)),
            DiffKind::Del => theme
                .color_for_scope("diff.minus")
                .unwrap_or(fallback(230, 120, 120)),
            DiffKind::Context => theme
                .ui_fg("ui.text")
                .unwrap_or(fallback(170, 170, 200)),
        }
    }
}

impl TitleRow for CommitDiffPaneComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        theme: &HelixTheme,
        state: &AppState,
    ) -> usize {
        let Some(id) = self.active_commit(state) else {
            return 0;
        };
        let short = short_id(&id);
        let title = match Self::commit_summary(state, id) {
            Some(summary) => format!("commit {short} {summary}"),
            None => format!("commit {short}"),
        };
        render_pane_title(ops, pane_box, &title, false, theme, focused);
        1
    }
}

impl Component<AppState, AppSignal> for CommitDiffPaneComponent {
    fn reset(&mut self) {}

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        input_event: InputEvent,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        throws_with_return!({
            let Some(id) = self.active_commit(&global_data.state) else {
                return Ok(EventPropagation::Propagate);
            };
            let window = Window::CommitDiff(id);
            let state = &mut global_data.state;

            if let InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Backspace),
            }) = input_event
            {
                state.pane_manager.send_to_back(&window);
                state.mark_session_dirty();
                return Ok(EventPropagation::ConsumedRender);
            }

            let mut consumed = false;
            if let InputEvent::Keyboard(KeyPress::Plain { key: kb_key }) = input_event {
                match kb_key {
                    Key::SpecialKey(SpecialKey::PageUp) => {
                        consumed = true;
                        let page = state.pane_manager.window_page_size(&window);
                        let current = state.pane_manager.window_scroll(&window);
                        state
                            .pane_manager
                            .set_window_scroll(&window, current.saturating_sub(page));
                        state.pane_manager.clamp_scroll(&window);
                    }
                    Key::SpecialKey(SpecialKey::PageDown) => {
                        consumed = true;
                        let page = state.pane_manager.window_page_size(&window);
                        let current = state.pane_manager.window_scroll(&window);
                        state
                            .pane_manager
                            .set_window_scroll(&window, current.saturating_add(page));
                        state.pane_manager.clamp_scroll(&window);
                    }
                    Key::SpecialKey(SpecialKey::Up) => {
                        consumed = true;
                        let current = state.pane_manager.window_scroll(&window);
                        state
                            .pane_manager
                            .set_window_scroll(&window, current.saturating_sub(1));
                        state.pane_manager.clamp_scroll(&window);
                    }
                    Key::SpecialKey(SpecialKey::Down) => {
                        consumed = true;
                        let current = state.pane_manager.window_scroll(&window);
                        state
                            .pane_manager
                            .set_window_scroll(&window, current.saturating_add(1));
                        state.pane_manager.clamp_scroll(&window);
                    }
                    Key::SpecialKey(SpecialKey::Home) => {
                        consumed = true;
                        state.pane_manager.set_window_scroll(&window, 0);
                    }
                    Key::SpecialKey(SpecialKey::End) => {
                        consumed = true;
                        let max = state.pane_manager.window_scroll_max(&window);
                        state.pane_manager.set_window_scroll(&window, max);
                        state.pane_manager.clamp_scroll(&window);
                    }
                    _ => {}
                }
            }
            if consumed {
                Ok(EventPropagation::ConsumedRender)
            } else {
                Ok(EventPropagation::Propagate)
            }
        });
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        current_box: FlexBox,
        _surface_bounds: SurfaceBounds,
        _has_focus: &mut HasFocus,
    ) -> CommonResult {
        throws!({
            let origin = current_box.style_adjusted_origin_pos;
            let bounds = current_box.style_adjusted_bounds_size;
            let total_rows = bounds.row_height.as_usize();
            let pane_width = bounds.col_width.as_usize();

            let Some(id) = self.active_commit(&global_data.state) else {
                return Ok(());
            };
            let window = Window::CommitDiff(id);
            let state = &mut global_data.state;
            state.pane_manager.set_window_page_size(&window, total_rows);

            let diff = state.commit_diffs.get(&id).cloned();
            let lines = diff.as_ref().and_then(|d| d.lines.clone());
            let error = diff.as_ref().and_then(|d| d.error.clone());

            let line_count = lines.as_ref().map_or(0, |l| l.len());
            state.pane_manager.set_window_scroll_max(&window, line_count);
            state.pane_manager.clamp_scroll(&window);
            let scroll = state.pane_manager.window_scroll(&window);

            let pane_bg = state.theme.ui_bg("ui.background").unwrap_or(DEFAULT_BG);
            let bg = TuiColor::from(pane_bg);
            let bg_style = new_style!(color_bg: {bg});

            let mut render_ops = RenderOpIRVec::new();
            match (&lines, &error) {
                (None, _) => {
                    let fg = state.theme.ui_fg("ui.text").unwrap_or(RgbValue::from_u8(170, 170, 200));
                    paint_centered(&mut render_ops, &origin, pane_width, "Loading diff…", fg, pane_bg);
                }
                (Some(_), Some(err)) => {
                    let fg = state.theme.ui_fg("error").unwrap_or(RgbValue::from_u8(220, 80, 80));
                    let text = format!("error: {err}");
                    paint_centered(&mut render_ops, &origin, pane_width, &text, fg, pane_bg);
                }
                (Some(lines), _) if lines.is_empty() => {
                    let fg = state.theme.ui_fg("ui.text").unwrap_or(RgbValue::from_u8(170, 170, 200));
                    paint_centered(&mut render_ops, &origin, pane_width, "(no diff)", fg, pane_bg);
                }
                (Some(lines), _) => {
                    for row_offset in 0..total_rows {
                        let line_idx = scroll + row_offset;
                        let Some(line) = lines.get(line_idx) else {
                            break;
                        };
                        render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                            origin,
                            col(0) + row(row_offset as u16),
                        );
                        render_ops += RenderOpCommon::ApplyColors(Some(bg_style));
                        render_ops += RenderOpIR::PaintTextWithAttributes(
                            " ".repeat(pane_width).into(),
                            Some(bg_style),
                        );
                        render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                            origin,
                            col(0) + row(row_offset as u16),
                        );
                        let fg = Self::kind_color(&state.theme, line.kind);
                        let style = new_style!(color_fg: {TuiColor::from(fg)} color_bg: {bg});
                        let mut text = line.text.clone();
                        if text.chars().count() > pane_width.saturating_sub(1) {
                            text = text.chars().take(pane_width.saturating_sub(1)).collect();
                        }
                        render_ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
                    }
                }
            }

            global_data.pipeline.push(ZOrder::Normal, render_ops);
        });
    }
}

fn paint_centered(
    render_ops: &mut RenderOpIRVec,
    origin: &Pos,
    pane_width: usize,
    text: &str,
    fg: RgbValue,
    pane_bg: RgbValue,
) {
    let pad = pane_width.saturating_sub(text.len()) / 2;
    let padded = format!("{:pad$}{}", "", text, pad = pad);
    let style = new_style!(color_fg: {TuiColor::from(fg)} color_bg: {TuiColor::from(pane_bg)});
    render_ops += RenderOpCommon::MoveCursorPositionRelTo(*origin, col(0) + row(0));
    render_ops += RenderOpCommon::ApplyColors(Some(style));
    render_ops += RenderOpIR::PaintTextWithAttributes(padded.into(), Some(style));
}
```

Note: `paint_centered` receives an explicit `fg` per call site (`ui.text` for loading/empty, `error` for failures) so messages are always readable.

- [ ] **Step 2: Register the module**

In `src/tui/mod.rs`: add `mod commit_diff_pane;` (after `mod app;`) and `use self::commit_diff_pane::*;` (after `use self::app::*;`).

- [ ] **Step 3: Wire into `PaneComponent`**

In `src/tui/pane_component.rs`:

1. Add field `commit_diff: CommitDiffPaneComponent,` after `git_log: GitLogPaneComponent,`.
2. In `new_boxed`, add `commit_diff: CommitDiffPaneComponent::new(id),` after the `git_log:` line.
3. In `reset()`, add `self.commit_diff.reset();` after `self.git_log.reset();`.
4. Replace the title arm `Window::CommitDiff(_) => 0,` with:

```rust
            Window::CommitDiff(_) => self
                .commit_diff
                .render_title_row(ops, pane_box, focused, theme, state),
```

5. Replace the tail-event arm `Some(Window::CommitDiff(_)) => Ok(EventPropagation::Propagate),` with:

```rust
            Some(Window::CommitDiff(_)) => {
                self.commit_diff
                    .handle_event(global_data, input_event, has_focus)
            }
```

6. Replace the render arm `Some(Window::CommitDiff(_)) => {}` with:

```rust
                Some(Window::CommitDiff(_)) => {
                    self.commit_diff
                        .render(global_data, inner_bounds, surface_bounds, has_focus)?
                }
```

- [ ] **Step 4: Drain `pending_diffs` on startup**

In `src/tui/app.rs`, in `app_start_background_services`, add after the pending-terminals drain loop (after the closing brace of `for window in stack { ... }`):

```rust
        for id in global_data.state.pending_diffs.drain(..) {
            if global_data.state.commit_diffs.contains_key(&id) {
                continue;
            }
            global_data.state.commit_diffs.insert(id, CommitDiffState::default());
            let root = global_data.state.root.clone();
            let main_tx = notify_tx.clone();
            tokio::task::spawn_blocking(move || {
                let result = crate::git::load_commit_diff(&root, id)
                    .map(|text| std::sync::Arc::new(crate::git::classify_diff(&text)));
                send_signal!(
                    main_tx,
                    TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::CommitDiffLoaded {
                        id,
                        result,
                    })
                );
            });
        }
```

- [ ] **Step 5: Build, test, format, lint, commit**

Run: `cargo test && cargo clippy --no-deps`
Expected: all pass, no warnings.

```bash
cargo fmt && git add src/tui/commit_diff_pane.rs src/tui/mod.rs src/tui/pane_component.rs src/tui/app.rs
git commit -m "feat(tui): commit diff pane with session restore"
```

---

### Task 6: End-to-end verification + docs

**Files:**
- Modify: `AGENTS.md` (dependency table + overview list)

- [ ] **Step 1: Full verification**

```bash
cargo fmt --check
cargo test
cargo clippy --no-deps
cargo build --release
```

Expected: formatting clean, all tests pass, no clippy warnings, release build succeeds.

- [ ] **Step 2: Manual smoke test**

Run `cargo run --release` inside this repo and verify:
1. `<leader>` then `g` opens the Git Log pane; entries show `YYYY-MM-DD HH:MM <summary>`; a loading placeholder may flash first.
2. Up/Down move the selection with autoscroll; mouse scroll works; clicking a row selects it.
3. ENTER on a commit opens a colored diff pane (`+` green, `-` red, `@@` yellow, headers cyan); the title shows `commit <7hex> <summary>`.
4. ENTER on a different commit opens a second, distinct diff pane; ENTER on the same commit focuses the existing one.
5. Esc closes the Git Log pane; Backspace sends a diff pane to back; `<leader>x` still closes terminals only.
6. Quit and relaunch: restored session contains the commit-diff panes (diffs reload async), and no Git Log pane.
7. Status bar shows `g:Git` in leader mode and per-pane hints for both new panes.

- [ ] **Step 3: Update AGENTS.md**

1. Source-organization list: add entries in the correct alphabetical position:

```markdown
- `src/git.rs` — gix-backed git data: `load_log` (HEAD first-parent), `load_commit_diff` (unified text), `LogEntry`, `DiffLine`/`DiffKind`, datetime + short-id helpers
```

2. `src/tui/mod.rs` list: add `git_log_pane.rs`, `commit_diff_pane.rs` entries describing the two components (log list navigation + ENTER→diff pane; colored unified diff rendering, Backspace send-to-back).
3. Dependency table: add row `| `gix` | 0.87.x | Git data (log walk, tree diff, blob unified diff); pure Rust |` and remove the "Planned feature areas" git-tooling bullet's "likely dependencies" mention of `gix` (keep the spec reference).

- [ ] **Step 4: Commit**

```bash
cargo fmt && git add AGENTS.md && git commit -m "docs: register git log and commit diff panes in agents guide"
```

---

## Self-Review

- **Spec coverage:** data layer (§1→Tasks 1–2), state/signals (§2→Task 3), keymap (§3→Task 4), log + diff components incl. titles/hints/mouse (§4→Tasks 4–5), session save/restore incl. version bump (§5→Task 3 + Task 5 drain), error handling (§6→Tasks 2/4/5 error arms rendered inline), testing (§7→Tasks 1–3 tests + Task 6 smoke). ✔
- **Placeholder scan:** no TBDs, no scaffolding; every code step is final code. ✔
- **Type consistency:** `LogEntry.id: [u8; 20]`, `Window::CommitDiff([u8; 20])`, `AppSignal::CommitDiffLoaded { id: [u8; 20], result }`, `PaneEntry::CommitDiff { hash: String, pane_size }` — consistent across Tasks 3–5. `GitLogPaneComponent::open_commit_diff(state, main_tx, id)` signature matches its Task 4 call site. ✔
