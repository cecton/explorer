# Git Log Pane Enhancements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Session-version tolerance, 3-space row gap, conventional-commit type coloring, a 10k-entry log cap, and `/` nucleo live filtering in the Git Log pane.

**Architecture:** Pure helpers land in `src/git.rs` (`LogHistory` + cap, `commit_type_prefix`). `GitLogState` gains `query`/`filtered`; filtering runs synchronously through nucleo (`run_log_match`/`apply_filter` in `src/tui/git_log_pane.rs`, also called from the `GitLogLoaded` signal handler). The component renders the visible list (filtered or all), colors type prefixes via theme scopes, and swaps its title row for an `InputLine` during search.

**Tech Stack:** nucleo (already a dependency), existing r3bl_tui component patterns, no new dependencies.

## Global Constraints

- Spec: `docs/superpowers/specs/2026-09-21-git-log-pane-enhancements-design.md`
- Every commit: `cargo fmt` first; `cargo clippy --no-deps` warning-free except explicitly noted transient dead-code; no `#[allow(...)]`; no `.unwrap()` outside tests; no comments unless non-obvious why.
- `cargo check` clean before committing. If rustc SIGSEGVs, `export RUST_MIN_STACK=16777216`.
- Type→scope map (verbatim): feat→`diff.plus`, fix→`diff.minus`, perf→`diff.delta`, docs→`comment`, test→`special`, refactor→`keyword`, style→`attribute`, build→`type`, ci→`constant`, chore→`operator`, revert→`operator`.
- `COMMIT_TYPES` (verbatim): `feat, fix, docs, style, refactor, perf, test, build, ci, chore, revert`.
- `MAX_LOG_ENTRIES = 10_000`; truncation flag correctness must hold when the repo has exactly 10_000 commits.
- Esc ladder (verbatim): clear non-empty query → else leave search mode → else close pane.
- Nucleo config verbatim: `Pattern::parse(query, CaseMatching::Smart, Normalization::Smart)`, `Matcher::new(Config::DEFAULT)`.

---

### Task 1: Session version tolerance

**Files:**
- Modify: `src/session.rs:422` (version check) + `mod tests`

**Interfaces:**
- Produces: `load_session` accepts `version <= SESSION_VERSION`, discards only `version > SESSION_VERSION`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/session.rs`:

```rust
    #[test]
    fn load_session_accepts_older_versions() {
        let (_dir, root) = temp_root();
        let path = session_file_path(&root).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!(
                "{{\"version\": {}}}",
                SESSION_VERSION - 1
            ),
        )
        .unwrap();

        let session = load_session(&root).expect("older version must load");
        assert_eq!(session.version, SESSION_VERSION - 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_session_discards_newer_versions() {
        let (_dir, root) = temp_root();
        let path = session_file_path(&root).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{{\"version\": {}}}", SESSION_VERSION + 1)).unwrap();

        assert!(load_session(&root).is_none());

        let _ = std::fs::remove_file(&path);
    }
```

Note: `Session`'s other fields all have `#[serde(default)]`, so a minimal `{"version": N}` JSON deserializes.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test session::tests::load_session`
Expected: `load_session_accepts_older_versions` FAILS (None returned); the discard test passes already.

- [ ] **Step 3: Fix the check**

In `src/session.rs`, replace (line ~422):

```rust
    if session.version != SESSION_VERSION {
```

with:

```rust
    if session.version > SESSION_VERSION {
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test session::`
Expected: all session tests pass.

- [ ] **Step 5: Format, lint, commit**

```bash
cargo fmt && cargo clippy --no-deps && git add src/session.rs
git commit -m "fix(session): accept older session versions"
```

---

### Task 2: `git.rs` — LogHistory, cap, commit-type parser (TDD)

**Files:**
- Modify: `src/git.rs`

**Interfaces:**
- Consumes: existing `load_log`, `LogEntry`.
- Produces (used by Tasks 3–4):
  - `pub const MAX_LOG_ENTRIES: usize = 10_000;`
  - `pub struct LogHistory { pub entries: Vec<LogEntry>, pub truncated: bool }` (derives `Clone, Debug`)
  - `pub fn load_log(repo_root: &Utf8Path) -> Result<LogHistory, String>` (return type changes)
  - `pub const COMMIT_TYPES: &[&str]`
  - `pub fn commit_type_prefix(summary: &str) -> Option<(&'static str, usize)>` — type plus byte length of the `type(scope)!` prefix

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/git.rs`:

```rust
    #[test]
    fn commit_type_prefix_parses_conventional_summaries() {
        assert_eq!(commit_type_prefix("feat: add pane"), Some(("feat", 4)));
        assert_eq!(
            commit_type_prefix("feat(tui)!: add pane"),
            Some(("feat", 10))
        );
        assert_eq!(commit_type_prefix("fix(scope): x"), Some(("fix", 10)));
        assert_eq!(commit_type_prefix("chore!: x"), Some(("chore", 6)));
        assert_eq!(commit_type_prefix("weird: x"), None);
        assert_eq!(commit_type_prefix("notatype: x"), None);
        assert_eq!(commit_type_prefix("no colon here"), None);
        assert_eq!(commit_type_prefix("feat (x): y"), None);
        assert_eq!(commit_type_prefix(": empty type"), None);
    }

    #[test]
    fn load_log_reports_no_truncation_for_this_repo() {
        let root = camino::Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let history = load_log(&root).expect("load_log");
        assert!(!history.truncated);
        assert!(history.entries.len() < MAX_LOG_ENTRIES);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test git::`
Expected: compile errors — `commit_type_prefix`, `MAX_LOG_ENTRIES` not found; `load_log` returns `Vec<LogEntry>` not `LogHistory`.

- [ ] **Step 3: Implement**

In `src/git.rs`, after the `LogEntry` struct add:

```rust
#[derive(Clone, Debug)]
pub struct LogHistory {
    pub entries: Vec<LogEntry>,
    pub truncated: bool,
}

pub const MAX_LOG_ENTRIES: usize = 10_000;

pub const COMMIT_TYPES: &[&str] = &[
    "feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore", "revert",
];

pub fn commit_type_prefix(summary: &str) -> Option<(&'static str, usize)> {
    let colon = summary.find(':')?;
    let head = &summary[..colon];
    if head.contains(' ') {
        return None;
    }
    let head = head.strip_suffix('!').unwrap_or(head);
    if let Some(open) = head.find('(') {
        if !head.ends_with(')') {
            return None;
        }
        let ty = head.get(..open)?;
        let ty = COMMIT_TYPES.iter().find(|candidate| **candidate == ty)?;
        Some((ty, colon))
    } else {
        let ty = COMMIT_TYPES.iter().find(|candidate| **candidate == head)?;
        Some((ty, colon))
    }
}
```

Change `load_log`'s signature to `pub fn load_log(repo_root: &Utf8Path) -> Result<LogHistory, String>`, the unborn-HEAD early return to `return Ok(LogHistory { entries: Vec::new(), truncated: false });`, and the walk loop + tail to:

```rust
    let mut entries = Vec::new();
    let mut truncated = false;
    for info in walk {
        if entries.len() == MAX_LOG_ENTRIES {
            truncated = true;
            break;
        }
        let info = info.map_err(|e| format!("revision walk: {e}"))?;
        let commit = info
            .object()
            .map_err(|e| format!("read commit {}: {e}", info.id))?;
        let time_secs = commit.committer().map(|sig| sig.seconds()).unwrap_or(0);
        let first_line = commit
            .message_raw_sloppy()
            .split(|&b| b == b'\n')
            .next()
            .unwrap_or(b"");
        let mut id = [0u8; 20];
        id.copy_from_slice(info.id.as_bytes());
        entries.push(LogEntry {
            id,
            time_secs,
            summary: String::from_utf8_lossy(first_line).into_owned(),
        });
    }
    Ok(LogHistory { entries, truncated })
```

Cap semantics: if the repo has exactly `MAX_LOG_ENTRIES` commits, the iterator exhausts naturally and `truncated` stays false; a 10_001st commit enters the loop body, flips the flag, and is dropped.

Fix the two existing repo tests that index `entries[0]`: they now read `load_log(&root).expect(..).entries[0]` (via a `let history = ...;` binding).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test git::`
Expected: all git tests pass. NOTE: the crate will NOT compile yet — `src/tui/state.rs` and `src/tui/app.rs` still assign `Vec<LogEntry>` to `git_log.entries` / `GitLogLoaded`. If `cargo test git::` cannot run due to those errors, this task also updates exactly two sites: `src/tui/state.rs` — `use crate::git::{DiffLine, LogEntry};` gains `LogHistory`, `GitLogState.entries: Option<Arc<LogHistory>>`; `src/tui/app.rs` — `AppSignal::GitLogLoaded(Result<Arc<Vec<LogEntry>>, String>)` becomes `AppSignal::GitLogLoaded(Result<Arc<LogHistory>, String>)` with import updated, and in the `GitLogLoaded` handler arm the selected-clamp line `state.git_log.entries.as_ref().map_or(0, |entries| entries.len())` becomes `...map_or(0, |history| history.entries.len())`. No other behavior changes.

- [ ] **Step 5: Full suite + commit**

Run: `cargo test && cargo clippy --no-deps`
Expected: 109+ tests pass, clippy clean.

```bash
cargo fmt && git add src/git.rs src/tui/state.rs src/tui/app.rs
git commit -m "feat(git): cap log history and parse conventional commit types"
```

---

### Task 3: Filter engine — `run_log_match` / `apply_filter` + state fields (TDD)

**Files:**
- Modify: `src/tui/state.rs` (`GitLogState` fields)
- Modify: `src/tui/git_log_pane.rs` (two pure/pub(crate) functions + tests)

**Interfaces:**
- Consumes: `LogHistory.entries` from Task 2.
- Produces (used by Task 4 and by app.rs):
  - `GitLogState.query: String`, `GitLogState.filtered: Option<Vec<(usize, Vec<u32>)>>`
  - `pub(crate) fn run_log_match(query: &str, entries: &[LogEntry]) -> Vec<(usize, Vec<u32>)>` (relevance-sorted; empty query → identity order, empty match vecs)
  - `pub(crate) fn apply_filter(state: &mut AppState)` (sets/clears `filtered`; clamps `selected`)

- [ ] **Step 1: State fields**

In `src/tui/state.rs`, extend `GitLogState`:

```rust
#[derive(Clone, Default)]
pub struct GitLogState {
    pub entries: Option<Arc<LogHistory>>,
    pub error: Option<String>,
    pub selected: usize,
    pub query: String,
    /// Some = query active: (entry index, nucleo match positions), relevance order.
    pub filtered: Option<Vec<(usize, Vec<u32>)>>,
}
```

- [ ] **Step 2: Write the failing test**

Add to `mod tests` in `src/tui/git_log_pane.rs` (the module already exists):

```rust
    fn entry_with_summary(summary: &str) -> LogEntry {
        LogEntry {
            id: [0u8; 20],
            time_secs: 0,
            summary: summary.to_string(),
        }
    }

    #[test]
    fn run_log_match_empty_query_lists_everything_in_order() {
        let entries = vec![
            entry_with_summary("feat: one"),
            entry_with_summary("fix: two"),
        ];
        let results = run_log_match("", &entries);
        assert_eq!(results, vec![(0, vec![]), (1, vec![])]);
    }

    #[test]
    fn run_log_match_ranks_and_records_positions() {
        let entries = vec![
            entry_with_summary("docs: update readme"),
            entry_with_summary("feat: readme reader"),
        ];
        let results = run_log_match("readme", &entries);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|&(idx, _)| idx == 0 || idx == 1));
        let (_, positions) = results[0];
        assert!(!positions.is_empty());
        let summary = entries[results[0].0].summary.clone();
        assert!(positions.iter().all(|&p| {
            summary.chars().nth(p as usize).is_some()
        }));
    }
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test git_log_pane::`
Expected: compile errors — `run_log_match` not found.

- [ ] **Step 4: Implement**

In `src/tui/git_log_pane.rs`, extend the imports at the top:

```rust
use nucleo::Matcher;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Utf32Str};
```

Add these two functions (below the component impl, above `log_row_text`):

```rust
pub(crate) fn run_log_match(query: &str, entries: &[LogEntry]) -> Vec<(usize, Vec<u32>)> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    if pattern.atoms.is_empty() {
        return (0..entries.len()).map(|i| (i, Vec::new())).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(usize, u32, Vec<u32>)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| {
            let haystack = Utf32Str::new(&entry.summary, &mut buf);
            let mut indices = Vec::new();
            pattern
                .indices(haystack, &mut matcher, &mut indices)
                .map(|score| {
                    indices.sort_unstable();
                    indices.dedup();
                    (i, score, indices)
                })
        })
        .collect();
    scored.sort_by_key(|&(_, score, _)| std::cmp::Reverse(score));
    scored
        .into_iter()
        .map(|(i, _, indices)| (i, indices))
        .collect()
}

pub(crate) fn apply_filter(state: &mut AppState) {
    let total = state
        .git_log
        .entries
        .as_ref()
        .map_or(0, |history| history.entries.len());
    if state.git_log.query.is_empty() {
        state.git_log.filtered = None;
    } else if total > 0 {
        let entries: &[LogEntry] = &state.git_log.entries.as_ref().expect("total > 0").entries;
        let matches = run_log_match(&state.git_log.query, entries);
        state.git_log.filtered = Some(matches);
    }
    let visible = state
        .git_log
        .filtered
        .as_ref()
        .map_or(total, |matches| matches.len());
    if state.git_log.selected >= visible {
        state.git_log.selected = visible.saturating_sub(1);
    }
}
```

- [ ] **Step 5: Wire the signal handler**

In `src/tui/app.rs`, the `AppSignal::GitLogLoaded` handler arm: after the existing selected-clamp block, add `crate::tui::git_log_pane::apply_filter(state);` so a `<leader>g` reload re-filters a live query. The arm becomes:

```rust
                AppSignal::GitLogLoaded(result) => match result {
                    Ok(history) => {
                        state.git_log.entries = Some(history);
                        state.git_log.error = None;
                        let count = state
                            .git_log
                            .entries
                            .as_ref()
                            .map_or(0, |history| history.entries.len());
                        if state.git_log.selected >= count {
                            state.git_log.selected = count.saturating_sub(1);
                        }
                        crate::tui::git_log_pane::apply_filter(state);
                    }
                    Err(e) => state.git_log.error = Some(e),
                },
```

- [ ] **Step 6: Run to verify pass**

Run: `cargo test && cargo clippy --no-deps`
Expected: all tests pass; clippy clean except transient dead-code warnings for `query`, `filtered`, `run_log_match`, `apply_filter` (Task 4 consumes them).

- [ ] **Step 7: Commit**

```bash
cargo fmt && git add src/tui/state.rs src/tui/git_log_pane.rs src/tui/app.rs
git commit -m "feat(tui): nucleo filter engine for git log entries"
```

---

### Task 4: Component UI — spacing, type colors, visible list, search

**Files:**
- Modify: `src/tui/git_log_pane.rs` (component rework)
- Modify: `src/tui/app.rs` (status-bar hint only)

**Interfaces:**
- Consumes: everything from Tasks 2–3.
- Produces: full user-visible behavior per spec §2–§5.

- [ ] **Step 1: Rework `log_row_text` into parts with a 3-space gap**

Replace `log_row_text` and its two tests with:

```rust
fn log_row_parts(entry: &LogEntry, pane_width: usize) -> (String, String) {
    let date = format_log_time(entry.time_secs);
    let gap = "   ";
    let max_chars = pane_width.saturating_sub(1);
    let summary_max = max_chars.saturating_sub(date.chars().count() + gap.len());
    let summary: String = entry.summary.chars().take(summary_max).collect();
    (format!("{date}{gap}"), summary)
}
```

```rust
    #[test]
    fn log_row_parts_separates_date_with_three_spaces() {
        let entry = entry_with_summary("feat: x");
        let (left, summary) = log_row_parts(&entry, 60);
        assert_eq!(left, "1970-01-01 00:00   ");
        assert_eq!(summary, "feat: x");
    }

    #[test]
    fn log_row_parts_truncates_summary_on_char_boundaries() {
        let entry = entry_with_summary("éééééééééé éééééééééé");
        let (_, summary) = log_row_parts(&entry, 40);
        assert!(summary.chars().count() <= 40 - 1 - 19);
        assert_eq!(
            summary,
            entry.summary.chars().take(40 - 1 - 19).collect::<String>()
        );
    }
```

Delete the old `log_row_text`, `log_row_text_truncates_on_char_boundaries`, and `log_row_text_truncation_index_hits_multibyte_char` (render now paints segments; `log_row_parts` is its replacement). `entry_with_summary` comes from Task 3's test module.

- [ ] **Step 2: Component fields + type colors**

Extend `GitLogPaneComponent`:

```rust
pub struct GitLogPaneComponent {
    id: FlexBoxId,
    content_origin_row: usize,
    content_row_count: usize,
    search_active: bool,
    input_line: InputLine,
}
```

`new` initializes `search_active: false, input_line: InputLine::new()`. `reset()` clears `search_active` (does NOT clear `state.git_log.query` — that lives in app state; nothing clears it except Esc).

Add the type-color mapper (below `run_log_match`):

```rust
fn commit_type_color(theme: &HelixTheme, ty: &str) -> RgbValue {
    let scope = match ty {
        "feat" => "diff.plus",
        "fix" => "diff.minus",
        "perf" => "diff.delta",
        "docs" => "comment",
        "test" => "special",
        "refactor" => "keyword",
        "style" => "attribute",
        "build" => "type",
        "ci" => "constant",
        "chore" | "revert" => "operator",
        _ => "ui.text",
    };
    theme.color_for_scope(scope).unwrap_or_else(|| {
        theme.ui_fg("ui.text").unwrap_or(RgbValue::from_u8(170, 170, 200))
    })
}
```

Add imports: `use crate::git::commit_type_prefix;` (alongside the existing `use crate::git::...` lines).

- [ ] **Step 3: Visible-list helpers**

Add below `apply_filter`:

```rust
fn visible_len(state: &AppState) -> usize {
    let total = state
        .git_log
        .entries
        .as_ref()
        .map_or(0, |history| history.entries.len());
    state
        .git_log
        .filtered
        .as_ref()
        .map_or(total, |matches| matches.len())
}

fn visible_entry_id(state: &AppState) -> Option<[u8; 20]> {
    let history = state.git_log.entries.as_ref()?;
    let entry_idx = match state.git_log.filtered.as_ref() {
        Some(matches) => matches.get(state.git_log.selected).map(|&(idx, _)| idx)?,
        None => {
            if state.git_log.selected < history.entries.len() {
                state.git_log.selected
            } else {
                return None;
            }
        }
    };
    history.entries.get(entry_idx).map(|entry| entry.id)
}
```

- [ ] **Step 4: Rework `handle_event`**

Full replacement of the `handle_event` body's logic (keep the signature):

```rust
        let state = &mut global_data.state;
        let count = visible_len(state);

        if let InputEvent::Keyboard(KeyPress::Plain {
            key: Key::Character('/'),
        }) = input_event
            && !self.search_active
        {
            self.search_active = true;
            self.input_line.set_cursor_end(&state.git_log.query);
            if state.git_log.query.is_empty() {
                state.git_log.filtered = None;
            } else {
                apply_filter(state);
            }
            return Ok(EventPropagation::ConsumedRender);
        }

        match &input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }) => {
                if !state.git_log.query.is_empty() {
                    state.git_log.query.clear();
                    apply_filter(state);
                    return Ok(EventPropagation::ConsumedRender);
                }
                if self.search_active {
                    self.search_active = false;
                    return Ok(EventPropagation::ConsumedRender);
                }
                state.pane_manager.remove_window(&Window::GitLog);
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Enter),
            }) => {
                if let Some(id) = visible_entry_id(state) {
                    let main_tx = global_data.main_thread_channel_sender.clone();
                    Self::open_commit_diff(state, main_tx, id);
                    return Ok(EventPropagation::ConsumedRender);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Up),
            }) if Self::move_selection(state, state.git_log.selected.saturating_sub(1), count) => {
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Down),
            }) if Self::move_selection(state, state.git_log.selected + 1, count) => {
                return Ok(EventPropagation::ConsumedRender);
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
            }) if Self::move_selection(state, 0, count) => {
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::End),
            }) if Self::move_selection(state, count.saturating_sub(1), count) => {
                return Ok(EventPropagation::ConsumedRender);
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

        if self.search_active
            && self
                .input_line
                .handle_key(&input_event, &mut state.git_log.query)
        {
            apply_filter(state);
            return Ok(EventPropagation::ConsumedRender);
        }

        Ok(EventPropagation::Propagate)
```

Key routing note: the `/` arm runs BEFORE the `Esc`/navigation match and is guarded by `!self.search_active`, so `/` typed while searching edits the query (falls through to `input_line.handle_key`). Special keys (Esc/Enter/arrows) are matched before `input_line.handle_key`, so navigation still works during search, matching the picker pattern.

- [ ] **Step 5: Rework `render`**

Changes inside `render` (keep the surrounding structure and placeholder handling):

1. Scroll bookkeeping uses the visible length instead of `count`:

```rust
            let visible = visible_len(state);
            state.pane_manager.set_window_scroll_max(&window, visible);
            state.pane_manager.clamp_scroll(&window);

            let mut scroll = state.pane_manager.window_scroll(&window);
            if visible > 0 && total_rows > 0 {
                let selected = state.git_log.selected;
                if selected < scroll {
                    scroll = selected;
                } else if selected >= scroll + total_rows {
                    scroll = selected + 1 - total_rows;
                }
                state.pane_manager.set_window_scroll(&window, scroll);
            }
```

2. In the `count == 0` placeholder branch, insert a "No matches" state between loading and empty: it applies when entries exist but the filter is active and empty:

```rust
            let filtering = state.git_log.filtered.is_some();
            if count == 0 {
                let message = if let Some(err) = error {
                    (format!("error: {err}"), error_fg)
                } else if entries.is_none() {
                    ("Loading git log…".to_string(), normal_fg)
                } else if filtering {
                    ("No matches".to_string(), normal_fg)
                } else {
                    ("(no commits)".to_string(), normal_fg)
                };
```

(with `let entries = state.git_log.entries.clone();` still present above; `count` here means `visible_len`.)

3. Replace the row loop (the `else` branch) with segment painting:

```rust
            } else {
                let history = entries.as_ref().expect("visible > 0 implies entries");
                let match_rgb = state
                    .theme
                    .ui_fg("ui.cursor.match")
                    .unwrap_or(RgbValue::from_u8(255, 200, 60));
                let match_fg = TuiColor::from(match_rgb);
                let muted_fg = TuiColor::from(
                    state
                        .theme
                        .ui_fg("ui.text")
                        .unwrap_or(RgbValue::from_u8(170, 170, 200)),
                );
                for row_offset in 0..total_rows {
                    let vis_idx = scroll + row_offset;
                    let slot = match state.git_log.filtered.as_ref() {
                        Some(matches) => matches.get(vis_idx).map(|&(idx, _)| (idx, None)),
                        None => history.entries.get(vis_idx).map(|e| (vis_idx, None)),
                    };
                    render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        col(0) + row(row_offset as u16),
                    );
                    let is_selected = vis_idx == selected;
                    let row_style = if is_selected {
                        selected_text_style
                    } else {
                        bg_style
                    };
                    render_ops += RenderOpCommon::ApplyColors(Some(row_style));
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        " ".repeat(pane_width).into(),
                        Some(row_style),
                    );
                    let Some((entry_idx, _)) = slot else {
                        continue;
                    };
                    let Some(entry) = history.entries.get(entry_idx) else {
                        continue;
                    };
                    let (left, summary) = log_row_parts(entry, pane_width);
                    render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        col(0) + row(row_offset as u16),
                    );
                    let left_style = if is_selected {
                        new_style!(color_fg: {muted_fg} color_bg: {selected_bg})
                    } else {
                        new_style!(color_fg: {muted_fg} color_bg: {bg})
                    };
                    render_ops += RenderOpCommon::ApplyColors(Some(left_style));
                    render_ops += RenderOpIR::PaintTextWithAttributes(left.into(), Some(left_style));

                    let type_prefix = commit_type_prefix(&summary).map(|(_, len)| {
                        summary.char_indices().take_while(|(b, _)| *b < len).count()
                    });
                    let match_set: std::collections::HashSet<u32> = state
                        .git_log
                        .filtered
                        .as_ref()
                        .and_then(|matches| matches.get(vis_idx))
                        .map(|(_, positions)| positions.iter().copied().collect())
                        .unwrap_or_default();

                    for (char_i, ch) in summary.chars().enumerate() {
                        let fg = if type_prefix.is_some_and(|len| char_i < len) {
                            let (ty, _) = commit_type_prefix(&summary).expect("checked above");
                            TuiColor::from(commit_type_color(&state.theme, ty))
                        } else if match_set.contains(&(char_i as u32)) {
                            match_fg
                        } else {
                            normal_fg
                        };
                        let style = if is_selected {
                            new_style!(color_fg: {fg} color_bg: {selected_bg})
                        } else {
                            new_style!(color_fg: {fg} color_bg: {bg})
                        };
                        let mut buf = [0u8; 4];
                        render_ops += RenderOpIR::PaintTextWithAttributes(
                            ch.encode_utf8(&mut buf).to_string().into(),
                            Some(style),
                        );
                    }
                }
            }
```

Implementation notes:
- `type_prefix` converts the byte-length from `commit_type_prefix` into a char count so the per-char loop compares char indices (boundary-safe by construction).
- `selected` must be read before the loop (`let selected = state.git_log.selected;` already exists).
- Match positions from nucleo index the SUMMARY (`Utf32Str::new(&entry.summary, ..)`), and the per-char loop paints `summary` — indices align. The prefix (`type`/scope chars) may overlap match positions; type color wins (checked first), which is intended.
- `muted_fg` reuses `ui.text` (date column); `normal_fg` already exists in the function.

4. Title row: search input vs. static title with truncation marker. Replace the `TitleRow` impl:

```rust
impl TitleRow for GitLogPaneComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        theme: &HelixTheme,
        state: &AppState,
    ) -> usize {
        if self.search_active || !state.git_log.query.is_empty() {
            let origin = pane_box.style_adjusted_origin_pos;
            let width = pane_box.style_adjusted_bounds_size.col_width.as_u16();
            let query = &state.git_log.query;
            let (bg_rgb, fg_rgb) = title_bar_colors(focused, theme);
            let color_bg = TuiColor::from(bg_rgb);
            let color_fg = TuiColor::from(fg_rgb);
            let bg_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});
            *ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
            *ops += RenderOpCommon::SetBgColor(color_bg);
            *ops += RenderOpIR::PaintTextWithAttributes(
                " ".repeat(width as usize).as_str().into(),
                Some(bg_style),
            );
            self.input_line.render(
                ops,
                query,
                "",
                origin,
                width,
                focused,
                (color_bg, color_fg),
            );
            1
        } else {
            let title = match state
                .git_log
                .entries
                .as_ref()
                .map(|history| history.truncated)
            {
                Some(true) => "Git Log (10000+)",
                _ => "Git Log",
            };
            render_pane_title(ops, pane_box, title, false, theme, focused);
            1
        }
    }
}
```

- [ ] **Step 6: Status-bar hint**

In `src/tui/app.rs`, `render_status_bar`, change the `Some(Window::GitLog)` hint to:

```rust
                Some(Window::GitLog) => "↑↓:Select  /:Search  Enter:Diff  Esc:Close",
```

- [ ] **Step 7: Full suite + lint**

Run: `cargo test && cargo clippy --no-deps && cargo build --release`
Expected: all tests pass, zero clippy warnings (all Task 3 items now consumed), release build ok.

- [ ] **Step 8: Commit**

```bash
cargo fmt && git add src/tui/git_log_pane.rs src/tui/app.rs
git commit -m "feat(tui): git log search, type colors, spacing and truncation title"
```

---

### Task 5: Verification + docs

**Files:**
- Modify: `AGENTS.md`

- [ ] **Step 1: Full verification**

```bash
cargo fmt --check && cargo test && cargo clippy --no-deps && cargo build --release
```

All clean/passing.

- [ ] **Step 2: AGENTS.md**

1. `src/git.rs` entry: extend to mention `LogHistory`/`MAX_LOG_ENTRIES` cap and `commit_type_prefix` (conventional-commit parsing).
2. `src/tui/git_log_pane.rs` entry: extend to mention `/` nucleo live filter (`run_log_match`/`apply_filter`), conventional-commit type coloring, 3-space date gap, and the `Git Log (10000+)` truncation title.
3. Nothing else changes.

- [ ] **Step 3: Commit**

```bash
cargo fmt && git add AGENTS.md && git commit -m "docs: describe git log search and type colors in agents guide"
```

---

### Task 6: Manual smoke (user, interactive)

`cargo run --release` then:
1. `<leader>g` → rows show `YYYY-MM-DD HH:MM<3 spaces><summary>`; `feat`/`fix`/etc prefixes tinted per theme.
2. `/` → title becomes query input; typing narrows list with fuzzy relevance; matched chars highlighted; selection clamps.
3. ENTER on a match opens that commit's diff; Esc clears query (list restored), Esc leaves search, Esc closes pane.
4. `<leader>g` again with query still active → reload re-filters.
5. On a huge repo the title shows `Git Log (10000+)`.
6. Old v2 sessions (other repos) load instead of being discarded.

## Self-Review

- **Spec coverage:** §1→Task 1; §2→Task 4 Step 1; §3→Tasks 2+4; §4→Tasks 2+4 (title); §5→Tasks 3+4. ✔
- **Placeholder scan:** no TBDs; every changed function is fully specified; Task 2 Step 4 explicitly lists the two compile-fix sites outside `git.rs`. ✔
- **Type consistency:** `LogHistory { entries, truncated }` used identically in Tasks 2/3/4; `filtered: Option<Vec<(usize, Vec<u32>)>>` matches `run_log_match`/`apply_filter`/render. `commit_type_prefix` returns byte length; render converts to char count once. ✔
