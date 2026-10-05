# Git Log Pane Enhancements — Design

Date: 2026-09-21
Status: approved
Parent: `2026-09-21-git-log-pane-design.md`

Five changes to the Git Log pane and session loader:

## 1. Session version tolerance

`load_session` currently discards any file whose `version != SESSION_VERSION`.
The 2 → 3 bump was structurally additive (v2 JSON parses as v3), so the
equality check destroyed compatible sessions. New rule: discard only when
`version > SESSION_VERSION` (a file newer than this binary). Older files load
and are re-saved in the current format on the next write. Tests: a
`version: 2` session applies; `version: 4` is discarded.

## 2. Row spacing

Three spaces between the datetime and the summary in Git Log rows (was one).

## 3. Conventional-commit type coloring

`git.rs` gains a pure parser:

```rust
pub const COMMIT_TYPES: &[&str] = &[
    "feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore", "revert",
];
pub fn commit_type_prefix(summary: &str) -> Option<(&'static str, usize)>
```

matching `^<type>(<scope>)?!?:` at the start of a summary and returning the
type plus the byte length of the full `type(scope)!` prefix (excluding the
colon). `reactor` is not a type (user typo for `refactor`).

Rendering (git_log_pane): the summary's prefix run is painted in a per-type
color, the rest normally. Colors come from `HelixTheme::color_for_scope`
(which always resolves, falling back to `ui.text`):

| type | scope |
|---|---|
| feat | `diff.plus` |
| fix | `diff.minus` |
| perf | `diff.delta` |
| docs | `comment` |
| test | `special` |
| refactor | `keyword` |
| style | `attribute` |
| build | `type` |
| ci | `constant` |
| chore, revert | `operator` |

`diagnostic.*` scopes are excluded (underline-only, no fg in most themes).
Selection/row background applies to both segments.

## 4. Gigantic-repo safeguard

`git.rs`: `pub const MAX_LOG_ENTRIES: usize = 10_000;` — the first-parent walk
stops at the cap. `load_log` returns:

```rust
pub struct LogHistory {
    pub entries: Vec<LogEntry>,
    pub truncated: bool,
}
```

The pane title shows `Git Log (10000+)` when `truncated`. No config knob
(personal tool). `AppState.git_log.entries` becomes
`Option<Arc<LogHistory>>`; `AppSignal::GitLogLoaded` carries
`Result<Arc<LogHistory>, String>`.

## 5. `/` live fuzzy filter (nucleo)

`GitLogState` gains:

```rust
pub query: String,                            // search text
pub filtered: Option<Vec<(usize, Vec<u32>)>>, // (entry index, match positions); None = unfiltered
```

- `/` enters search mode: the title bar becomes an `InputLine` editing
  `git_log.query` (picker-style).
- Filtering uses `nucleo` (`Pattern::parse` with `CaseMatching::Smart` +
  `Normalization::Smart`, same as the pickers) over summaries, recomputed
  synchronously on every query change — 10k rows is trivial.
- Visible list = `filtered` when `Some`, else all entries. `selected` indexes
  the visible list; Up/Down/PgUp/PgDn/Home/End, click-to-select, scrollbar,
  and autoscroll operate on it. Matched characters are highlighted in rows
  (match positions from nucleo).
- ENTER opens the selected commit's diff (search mode stays as-is).
- Esc: clears a non-empty query first; otherwise leaves search mode; only a
  third Esc (no query, not searching) closes the pane.
- `<leader>g` reload keeps the query and re-filters after entries arrive.
- Status hint: `↑↓:Select  /:Search  Enter:Diff  Esc:Close`.

Rejected alternatives: plain substring filter (nucleo gives highlighting and
consistency with the pickers for free), regex filter, n/N navigation.

## Testing

Hermetic unit tests: session v2-loads/v4-discarded, `commit_type_prefix`
parsing (with/without scope, bang, unknown type, no prefix), `MAX_LOG_ENTRIES`
cap via a synthetic walk is impractical — cap logic covered by a direct unit
test on the collect loop's break condition if extractable, else verified by
review. Filter matching shares nucleo config already proven by the pickers.
UI behavior verified by manual smoke.
