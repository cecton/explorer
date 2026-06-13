# Resizable Pane Stack Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add per-pane vertical sizes (full/half/third/quarter), global and leader-key bindings to resize/reorder panes, and refactor pane stack/layout logic into a `PaneManager` module with `PaneComponent` split out of `app.rs`.

**Architecture:** Introduce `src/tui/pane_manager.rs` owning the window stack, pane sizes, layout engine, and focus helpers. Move `Window`, `SelPoint`, `TextSelection`, `WindowState`, and `MAX_PANES` there to avoid circular imports. Create `src/tui/pane_component.rs` containing the existing `PaneComponent`, its scrollbar helpers, and a shared `pane_slot` reverse mapping. Update `app.rs` to use the new layout engine for rendering and input handling, and bump visible pane slots from 5 to 16.

**Tech Stack:** Rust, `r3bl_tui` (TUI framework), `tokio`, `camino`.

---

## File Structure

| File | Responsibility |
|------|----------------|
| `src/tui/pane_manager.rs` (new) | `Window`, `SelPoint`, `TextSelection`, `PaneSize`, `ResizeDelta`, `WindowState`, `PaneSlot`, `PaneManager`, layout algorithm, focus helpers. |
| `src/tui/pane_component.rs` (new) | `PaneComponent`, scrollbar helpers, `pane_slot` reverse mapping, shared component state for drag/selection. |
| `src/tui/state.rs` (modify) | `TerminalPane`, `AppState` (now holds `PaneManager`), `AppSignal`. |
| `src/tui/app.rs` (modify) | `Id` enum expanded to 16 pane IDs, `AppMain`, `PanesRenderer`, global/leader key bindings, status bar. |
| `src/tui/mod.rs` (modify) | Declare new modules. |
| `src/tui/file_name_picker.rs` (modify) | Read `window_stack` through `state.pane_manager`. |
| `src/tui/theme_picker.rs` (modify) | Read stack/focus through `state.pane_manager`. |
| `src/tui/preview.rs` (modify) | Use shared `pane_slot` helper; read stack through `state.pane_manager`. |
| `src/tui/terminal_pane.rs` (modify) | Use shared `pane_slot` helper; read `pane_boxes` through layout. |

---

## Task 1: Create `src/tui/pane_manager.rs` with core types

**Files:**
- Create: `src/tui/pane_manager.rs`

This module becomes the home for everything related to the pane stack, sizes, and layout. It also hosts `Window`, `SelPoint`, and `TextSelection` so `state.rs` can hold a `PaneManager` without a circular import.

- [ ] **Step 1: Write `pane_manager.rs` with types and `PaneSize` methods**

```rust
use crate::loader::FileKey;
use r3bl_tui::{FlexBox, Size};
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};

pub const MAX_PANES: usize = 16;

/// A pane that can appear in the window stack.
///
/// Each variant is unique: there is at most one `FileNamePicker` and at most one
/// `FilePreview` per `FileKey` in the stack at any time.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Window {
    FilePreview(FileKey),
    FileNamePicker,
    ThemePicker,
    Terminal(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelPoint {
    Preview { line_idx: usize, byte_offset: usize },
    Terminal { viewport_row: usize, col: usize },
}

#[derive(Clone, Debug)]
pub struct TextSelection {
    pub window: Window,
    pub start: SelPoint,
    pub end: SelPoint,
    pub click_anchor: Option<SelPoint>,
    pub click_word: Option<(SelPoint, SelPoint)>,
    pub active: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaneSize {
    #[default]
    Full,
    Half,
    Third,
    Quarter,
}

impl PaneSize {
    /// Fraction of available vertical rows consumed by this size.
    pub fn height_factor(&self) -> f32 {
        match self {
            PaneSize::Full => 1.0,
            PaneSize::Half => 0.5,
            PaneSize::Third => 1.0 / 3.0,
            PaneSize::Quarter => 0.25,
        }
    }

    /// Next larger size, clamped at `Full`.
    pub fn grow(self) -> Self {
        match self {
            PaneSize::Quarter => PaneSize::Third,
            PaneSize::Third => PaneSize::Half,
            PaneSize::Half => PaneSize::Full,
            PaneSize::Full => PaneSize::Full,
        }
    }

    /// Next smaller size, clamped at `Quarter`.
    pub fn shrink(self) -> Self {
        match self {
            PaneSize::Full => PaneSize::Half,
            PaneSize::Half => PaneSize::Third,
            PaneSize::Third => PaneSize::Quarter,
            PaneSize::Quarter => PaneSize::Quarter,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeDelta {
    Grow,
    Shrink,
}

#[derive(Clone, Debug, Default)]
pub struct WindowState {
    pub scroll: usize,
    pub page_size: usize,
    pub scroll_max: usize,
    pub pane_size: PaneSize,
}

#[derive(Clone, Debug)]
pub struct PaneSlot {
    pub slot: usize,
    pub window: Window,
    pub box_: FlexBox,
}

pub struct PaneManager {
    /// Stack of open windows, most-recently-opened first (index 0 = leftmost pane).
    pub window_stack: Vec<Window>,
    /// The window that currently receives keyboard input.
    pub focused_window: Option<Window>,
    /// Per-window scroll, page-size, and pane-size state.
    pub window_states: HashMap<Window, WindowState>,
}
```

- [ ] **Step 2: Implement `PaneManager` stack and focus methods**

Append to `pane_manager.rs`:

```rust
impl PaneManager {
    pub fn new() -> Self {
        Self {
            window_stack: Vec::new(),
            focused_window: None,
            window_states: HashMap::new(),
        }
    }

    /// Moves `window` to the front of the stack (index 0). If it is not present, inserts it.
    pub fn push_window(&mut self, window: Window) {
        if let Some(pos) = self.window_stack.iter().position(|w| w == &window) {
            self.window_stack.remove(pos);
        }
        self.window_stack.insert(0, window);
    }

    /// Removes `window` from the stack entirely.
    pub fn remove_window(&mut self, window: &Window) {
        self.window_stack.retain(|w| w != window);
        self.window_states.remove(window);
        if self.focused_window.as_ref() == Some(window) {
            self.focused_window = self.window_stack.first().cloned();
        }
    }

    /// Moves `window` to the back of the stack (last position).
    pub fn send_to_back(&mut self, window: &Window) {
        if let Some(pos) = self.window_stack.iter().position(|w| w == window) {
            let w = self.window_stack.remove(pos);
            self.window_stack.push(w);
        }
        if self.focused_window.as_ref() == Some(window) {
            self.focused_window = self.window_stack.first().cloned();
        }
    }

    /// Swaps `window` with the element before it (toward index 0).
    pub fn move_forward(&mut self, window: &Window) {
        let pos = match self.window_stack.iter().position(|w| w == window) {
            Some(0) | None => return,
            Some(p) => p,
        };
        self.window_stack.swap(pos, pos - 1);
    }

    /// Swaps `window` with the element after it (toward the end).
    pub fn move_backward(&mut self, window: &Window) {
        let pos = match self.window_stack.iter().position(|w| w == window) {
            Some(p) if p + 1 < self.window_stack.len() => p,
            _ => return,
        };
        self.window_stack.swap(pos, pos + 1);
    }

    /// Grows or shrinks the focused window's pane size, clamped at the boundaries.
    pub fn resize_focused(&mut self, delta: ResizeDelta) {
        let Some(window) = self.focused_window.clone() else { return };
        let state = self.window_states.entry(window).or_default();
        state.pane_size = match delta {
            ResizeDelta::Grow => state.pane_size.grow(),
            ResizeDelta::Shrink => state.pane_size.shrink(),
        };
    }

    pub fn focused_slot(&self) -> Option<usize> {
        let focused = self.focused_window.as_ref()?;
        self.window_stack.iter().position(|w| w == focused)
    }

    pub fn cycle_focus(&mut self, visible: &[PaneSlot], direction: i32) {
        if visible.is_empty() {
            return;
        }
        let current_pos = self
            .focused_window
            .as_ref()
            .and_then(|f| visible.iter().position(|s| &s.window == f))
            .unwrap_or(0);
        let len = visible.len() as i32;
        let next_pos = ((current_pos as i32 + direction).rem_euclid(len)) as usize;
        self.focused_window = Some(visible[next_pos].window.clone());
    }
}
```

- [ ] **Step 3: Implement scroll/page helpers on `PaneManager`**

Append to `pane_manager.rs`:

```rust
impl PaneManager {
    pub fn window_scroll(&self, window: &Window) -> usize {
        self.window_states.get(window).map(|s| s.scroll).unwrap_or(0)
    }

    pub fn window_page_size(&self, window: &Window) -> usize {
        self.window_states.get(window).map(|s| s.page_size).unwrap_or(0)
    }

    pub fn window_scroll_max(&self, window: &Window) -> usize {
        self.window_states.get(window).map(|s| s.scroll_max).unwrap_or(0)
    }

    pub fn set_window_scroll(&mut self, window: &Window, scroll: usize) {
        self.window_states.entry(window.clone()).or_default().scroll = scroll;
    }

    pub fn set_window_page_size(&mut self, window: &Window, page_size: usize) {
        self.window_states.entry(window.clone()).or_default().page_size = page_size;
    }

    pub fn set_window_scroll_max(&mut self, window: &Window, scroll_max: usize) {
        self.window_states.entry(window.clone()).or_default().scroll_max = scroll_max;
    }

    pub fn clamp_scroll(&mut self, window: &Window) {
        let state = self.window_states.get(window);
        let (scroll, page_size, scroll_max) = match state {
            Some(s) => (s.scroll, s.page_size, s.scroll_max),
            None => return,
        };
        if scroll_max > page_size {
            let clamped = scroll.min(scroll_max - page_size);
            self.window_states.get_mut(window).unwrap().scroll = clamped;
        }
    }
}
```

- [ ] **Step 4: Commit the new module skeleton**

```bash
cd /home/deck/repos/explorer
git add src/tui/pane_manager.rs
git commit -m "feat: add PaneManager module skeleton"
```

---

## Task 2: Implement the pane layout algorithm in `PaneManager`

**Files:**
- Modify: `src/tui/pane_manager.rs`

- [ ] **Step 1: Add the `layout` method**

Append to `pane_manager.rs` inside `impl PaneManager`:

```rust
impl PaneManager {
    /// Lays out visible panes into columns and rows.
    ///
    /// Column count is derived from `surface_size.col_width / MIN_PANE_WIDTH`.
    /// Within each column, panes are stacked top-to-bottom using their requested
    /// `PaneSize`. The last visible pane in the rightmost column is shrunk to fill
    /// any remaining vertical space.
    pub fn layout(&self, surface_size: Size) -> Vec<PaneSlot> {
        const MIN_PANE_WIDTH: u16 = 100;

        let surface_cols = surface_size.col_width.as_u16();
        let surface_rows = surface_size.row_height.as_u16();
        let cols = (surface_cols / MIN_PANE_WIDTH).max(1) as usize;
        let base_col_width = surface_cols / cols as u16;
        let remainder = surface_cols % cols as u16;

        let mut slots = Vec::with_capacity(self.window_stack.len().min(MAX_PANES));
        let mut current_col: usize = 0;
        let mut used_rows_in_col: u16 = 0;

        for (window_idx, window) in self.window_stack.iter().enumerate() {
            if current_col >= cols || slots.len() >= MAX_PANES {
                break;
            }

            let mut remaining_rows = surface_rows.saturating_sub(used_rows_in_col);
            if remaining_rows == 0 {
                current_col += 1;
                if current_col >= cols {
                    break;
                }
                used_rows_in_col = 0;
                remaining_rows = surface_rows;
            }

            let pane_size = self
                .window_states
                .get(window)
                .map(|s| s.pane_size)
                .unwrap_or_default();
            let requested_rows = ((surface_rows as f32 * pane_size.height_factor()) as u16).max(1);

            let actual_rows = if requested_rows > remaining_rows {
                let is_last_col = current_col == cols - 1;
                let has_more_windows = window_idx + 1 < self.window_stack.len();
                if is_last_col && !has_more_windows {
                    remaining_rows
                } else if !is_last_col {
                    current_col += 1;
                    if current_col >= cols {
                        break;
                    }
                    used_rows_in_col = 0;
                    let new_remaining = surface_rows;
                    requested_rows.min(new_remaining)
                } else {
                    remaining_rows
                }
            } else {
                requested_rows
            };

            // Compute column origin by summing widths of previous columns.
            let mut origin_col: u16 = 0;
            for c in 0..current_col {
                let width = base_col_width + if c < remainder as usize { 1 } else { 0 };
                origin_col += width;
            }
            let width = base_col_width + if current_col < remainder as usize { 1 } else { 0 };

            let origin = col(origin_col) + row(used_rows_in_col);
            let size = col(width) + row(actual_rows);
            let box_ = FlexBox {
                style_adjusted_origin_pos: origin,
                style_adjusted_bounds_size: size,
                ..FlexBox::default()
            };

            slots.push(PaneSlot {
                slot: slots.len(),
                window: window.clone(),
                box_,
            });

            used_rows_in_col += actual_rows;
        }

        slots
    }
}
```

> The helper `col`/`row` come from `r3bl_tui::*`. Ensure `use r3bl_tui::*;` is present or use `r3bl_tui::col` / `r3bl_tui::row`.

- [ ] **Step 2: Verify the module compiles standalone**

```bash
cd /home/deck/repos/explorer
cargo check
```

Expected: errors in other files because `Window` moved, but `pane_manager.rs` itself should have no syntax errors. It is OK if the crate does not compile yet.

- [ ] **Step 3: Commit layout algorithm**

```bash
cd /home/deck/repos/explorer
git add src/tui/pane_manager.rs
git commit -m "feat: implement pane layout algorithm"
```

---

## Task 3: Update `src/tui/state.rs` to use `PaneManager`

**Files:**
- Modify: `src/tui/state.rs`

- [ ] **Step 1: Remove moved types and old stack fields from `state.rs`**

Remove from `src/tui/state.rs`:
- `MAX_PANES` constant.
- `Window` enum.
- `SelPoint` enum.
- `TextSelection` struct.
- `WindowState` struct.
- `AppState` fields: `window_stack`, `focused_window`, `window_states`, `pane_boxes`.
- All `impl AppState` methods that moved to `PaneManager`: `push_window`, `remove_window`, `send_to_back`, `visible_windows`, `window_scroll`, `window_page_size`, `set_window_scroll`, `set_window_page_size`, `window_scroll_max`, `set_window_scroll_max`, `clamp_scroll`.

Replace the imports at the top of `state.rs` with:

```rust
use crate::loader::{FileKey, LoadedFile};
use crate::tui::pane_manager::{PaneManager, TextSelection};
use crate::tui::theme::HelixTheme;
use crate::watcher::BatchedWatchEvent;
use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use r3bl_tui::{OffscreenBuffer, Size};
use std::fmt::{Debug, Display, Formatter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
```

- [ ] **Step 2: Update `AppState` struct definition**

```rust
#[derive(Clone, Default)]
pub struct AppState {
    pub files: Arc<ArcSwap<Vec<LoadedFile>>>,
    pub files_version: u64,
    pub root: Utf8PathBuf,
    /// Pane stack, sizes, layout, and focus state.
    pub pane_manager: PaneManager,
    /// Per-file highlight ranges (1-indexed, inclusive).
    pub highlight_ranges: HashMap<FileKey, Vec<(usize, usize)>>,
    pub leader_active: bool,
    pub command_mode_active: bool,
    pub file_name_picker: FuzzyPickerState<FileKey>,
    pub theme_picker: FuzzyPickerState<String>,
    pub theme: HelixTheme,
    pub saved_theme: HelixTheme,
    /// Terminal panes keyed by their unique ID.
    pub terminal_panes: HashMap<usize, Arc<Mutex<TerminalPane>>>,
    /// Next available terminal pane ID.
    pub next_terminal_id: usize,
    pub mouse_drag_active: bool,
    pub terminal_grabbed: bool,
    pub text_selection: Option<TextSelection>,
}
```

- [ ] **Step 3: Update `AppState::bump_files_version` and `AppState::new`**

`bump_files_version` stays unchanged.

Update `AppState::new`:

```rust
impl AppState {
    pub fn new(
        files: Arc<ArcSwap<Vec<LoadedFile>>>,
        root: Utf8PathBuf,
        theme: HelixTheme,
    ) -> Self {
        let snapshot = files.load();
        let saved_theme = theme.clone();
        let mut pane_manager = PaneManager::new();
        pane_manager.push_window(Window::FileNamePicker);
        pane_manager.focused_window = Some(Window::FileNamePicker);
        pane_manager
            .window_states
            .insert(Window::FileNamePicker, WindowState::default());

        let mut state = Self {
            files,
            files_version: 0,
            root,
            pane_manager,
            highlight_ranges: HashMap::new(),
            leader_active: false,
            command_mode_active: false,
            file_name_picker: FuzzyPickerState::default(),
            theme_picker: FuzzyPickerState::default(),
            theme,
            saved_theme,
            terminal_panes: HashMap::new(),
            next_terminal_id: 0,
            mouse_drag_active: false,
            terminal_grabbed: false,
            text_selection: None,
        };
        state.file_name_picker.results =
            crate::tui::file_name_picker::FileNamePickerComponent::all_files_results(
                &snapshot,
                &state.pane_manager.window_stack,
            );
        state
    }
}
```

> Note: `Window`, `WindowState`, and `PaneManager` are imported from `pane_manager.rs`.

- [ ] **Step 4: Update `Debug` and `Display` impls for `AppState`**

```rust
impl Debug for AppState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let count = self.files.load().len();
        write!(
            f,
            "AppState {{ files: {}, stack: {:?}, focused: {:?} }}",
            count, self.pane_manager.window_stack, self.pane_manager.focused_window
        )
    }
}

impl Display for AppState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "AppState[files={}]", self.files.load().len())
    }
}
```

- [ ] **Step 5: Commit state refactor**

```bash
cd /home/deck/repos/explorer
git add src/tui/state.rs
git commit -m "refactor: AppState uses PaneManager"
```

---

## Task 4: Create `src/tui/pane_component.rs`

**Files:**
- Create: `src/tui/pane_component.rs`
- Modify: `src/tui/app.rs` (remove the moved code)

This task moves the existing `PaneComponent` (and its private helpers) out of `app.rs`.

- [ ] **Step 1: Create `src/tui/pane_component.rs` with module imports**

```rust
use crate::tui::*;
use std::time::Instant;

pub struct PaneComponent {
    pub id: FlexBoxId,
    pub slot: usize,
    picker: FileNamePickerComponent,
    theme_picker: ThemePickerComponent,
    preview: FilePreviewComponent,
    terminal: TerminalPaneComponent,
    /// Origin row of the content area (below title bar), used for scrollbar mouse events.
    pub content_origin_row: u16,
    /// Total columns in the content area (full width including scrollbar column).
    pub content_col_count: u16,
    /// Total rows in the content area.
    pub content_row_count: u16,
    /// Origin column of the content area, used for absolute scrollbar column calculation.
    pub content_origin_col: u16,
    /// Whether the user is currently dragging the scrollbar thumb.
    pub scrollbar_dragging: bool,
    /// (scroll, rel_y) at thumb grab time (None if drag started on track).
    pub scrollbar_grab_state: Option<(usize, usize)>,
    pub preview_drag_active: bool,
    pub text_drag_active: bool,
    pub last_click: Option<(Instant, Pos)>,
    pub consecutive_clicks: u8,
}
```

> Many fields become `pub` because `app.rs` may need them during event routing (currently not, but keep public to match future access). Actually keep them as they are in the original (private is fine if app.rs does not access them). The original had them private.

- [ ] **Step 2: Copy `PaneComponent::new_boxed`, `active_window`, `handle_scrollbar`, `apply_scroll`, `TitleRow` impl, scrollbar helpers, and `Component` impl from `app.rs`**

Copy lines `58..1062` from `src/tui/app.rs` into `src/tui/pane_component.rs`, preserving order and imports.

The helpers `thumb_size`, `thumb_position`, `scroll_from_y`, and `drag_modifier_from_mouse` move with `PaneComponent`.

- [ ] **Step 3: Add a shared `pub(crate) fn pane_slot` helper**

At the bottom of `pane_component.rs`, add:

```rust
pub(crate) fn pane_slot(id: FlexBoxId) -> Option<usize> {
    match id.inner {
        x if x == Id::Pane0 as u8 => Some(0),
        x if x == Id::Pane1 as u8 => Some(1),
        x if x == Id::Pane2 as u8 => Some(2),
        x if x == Id::Pane3 as u8 => Some(3),
        x if x == Id::Pane4 as u8 => Some(4),
        x if x == Id::Pane5 as u8 => Some(5),
        x if x == Id::Pane6 as u8 => Some(6),
        x if x == Id::Pane7 as u8 => Some(7),
        x if x == Id::Pane8 as u8 => Some(8),
        x if x == Id::Pane9 as u8 => Some(9),
        x if x == Id::Pane10 as u8 => Some(10),
        x if x == Id::Pane11 as u8 => Some(11),
        x if x == Id::Pane12 as u8 => Some(12),
        x if x == Id::Pane13 as u8 => Some(13),
        x if x == Id::Pane14 as u8 => Some(14),
        x if x == Id::Pane15 as u8 => Some(15),
        _ => None,
    }
}
```

- [ ] **Step 4: Remove the copied code from `app.rs`**

Delete from `src/tui/app.rs`:
- `struct PaneComponent` and its `impl` blocks.
- `thumb_size`, `thumb_position`, `scroll_from_y`, `drag_modifier_from_mouse`.
- `impl TitleRow for PaneComponent`.

Leave `Id` enum and `Id::pane` in `app.rs` for now.

- [ ] **Step 5: Update `app.rs` imports**

Add `use crate::tui::pane_component::PaneComponent;` to `app.rs` imports.

- [ ] **Step 6: Commit component extraction**

```bash
cd /home/deck/repos/explorer
git add src/tui/pane_component.rs src/tui/app.rs
git commit -m "refactor: move PaneComponent to its own module"
```

---

## Task 5: Update `src/tui/mod.rs`

**Files:**
- Modify: `src/tui/mod.rs`

- [ ] **Step 1: Declare new modules and update re-exports**

```rust
mod app;
mod file_name_picker;
mod fuzzy_picker;
mod input_line;
mod pane_component;
mod pane_manager;
mod preview;
mod state;
mod terminal_pane;
mod theme;
mod theme_picker;
mod title_row;

pub use app::{build_state, run};
pub use state::{AppSignal, AppState};
pub use theme::HelixTheme;

use self::app::*;
use self::file_name_picker::*;
use self::fuzzy_picker::*;
use self::input_line::*;
use self::pane_manager::*;
use self::preview::*;
use self::state::*;
use self::terminal_pane::*;
use self::theme_picker::*;
use self::title_row::*;
use r3bl_tui::*;
```

- [ ] **Step 2: Commit module declaration**

```bash
cd /home/deck/repos/explorer
git add src/tui/mod.rs
git commit -m "chore: declare pane_manager and pane_component modules"
```

---

## Task 6: Update `Id` enum for 16 pane slots

**Files:**
- Modify: `src/tui/app.rs`

- [ ] **Step 1: Expand `Id` enum to support 16 panes**

```rust
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Id {
    Container = 1,
    Pane0 = 2,
    Pane1 = 3,
    Pane2 = 4,
    Pane3 = 5,
    Pane4 = 6,
    Pane5 = 7,
    Pane6 = 8,
    Pane7 = 9,
    Pane8 = 10,
    Pane9 = 11,
    Pane10 = 12,
    Pane11 = 13,
    Pane12 = 14,
    Pane13 = 15,
    Pane14 = 16,
    Pane15 = 17,
}

impl Id {
    pub fn pane(slot: usize) -> Self {
        match slot {
            0 => Id::Pane0,
            1 => Id::Pane1,
            2 => Id::Pane2,
            3 => Id::Pane3,
            4 => Id::Pane4,
            5 => Id::Pane5,
            6 => Id::Pane6,
            7 => Id::Pane7,
            8 => Id::Pane8,
            9 => Id::Pane9,
            10 => Id::Pane10,
            11 => Id::Pane11,
            12 => Id::Pane12,
            13 => Id::Pane13,
            14 => Id::Pane14,
            _ => Id::Pane15,
        }
    }
}
```

- [ ] **Step 2: Update `create_stylesheet` to style all 16 pane IDs**

Replace the stylesheet entries for panes with a generated list:

```rust
fn create_stylesheet(theme: &HelixTheme) -> CommonResult<TuiStylesheet> {
    let bg = theme.ui_bg("ui.background").unwrap_or([15, 15, 25]);
    throws_with_return!({
        let mut styles = tui_stylesheet! {
            new_style!(
                id: {Id::Container}
                color_bg: {tui_color!(bg[0], bg[1], bg[2])}
            ),
        };
        for slot in 0..MAX_PANES {
            let id = Id::pane(slot);
            styles += new_style!(
                id: {id}
                padding: {0}
                color_bg: {tui_color!(bg[0], bg[1], bg[2])}
            );
        }
        styles
    })
}
```

> `MAX_PANES` is imported from `pane_manager.rs` via `use crate::tui::*;`.

- [ ] **Step 3: Update `app_init_components` to create 16 pane components**

```rust
fn app_init_components(
    &mut self,
    component_registry_map: &mut ComponentRegistryMap<Self::S, Self::AS>,
    has_focus: &mut HasFocus,
) {
    for slot in 0..MAX_PANES {
        let pane_id = FlexBoxId::from(Id::pane(slot));
        if let ContainsResult::DoesNotContain =
            ComponentRegistry::contains(component_registry_map, pane_id)
        {
            ComponentRegistry::put(
                component_registry_map,
                pane_id,
                PaneComponent::new_boxed(
                    slot,
                    pane_id,
                    self.picker_results_tx.clone(),
                    Arc::clone(&self.picker_generation),
                ),
            );
        }
    }

    if has_focus.get_id().is_none() {
        has_focus.set_id(FlexBoxId::from(Id::Pane0));
    }
}
```

- [ ] **Step 4: Commit ID expansion**

```bash
cd /home/deck/repos/explorer
git add src/tui/app.rs
git commit -m "feat: support 16 pane slots"
```

---

## Task 7: Update `PanesRenderer` and rendering in `app.rs`

**Files:**
- Modify: `src/tui/app.rs`

- [ ] **Step 1: Replace `visible_windows` usage with `PaneManager::layout`**

In `app_render`, replace:

```rust
let visible = global_data.state.visible_windows(surface_cols);
```

with:

```rust
let surface_size = {
    let col_count = window_size.col_width;
    let row_count = window_size.row_height - height(1);
    col_count + row_count
};
let visible = global_data.state.pane_manager.layout(surface_size);
```

> Use this `surface_size` for both layout and the `SurfaceProps` below.

- [ ] **Step 2: Update focused-window visibility sync**

Replace the focused visibility check with:

```rust
let focused = global_data.state.pane_manager.focused_window.clone();
let focused_is_visible = focused
    .as_ref()
    .map(|f| visible.iter().any(|s| &s.window == f))
    .unwrap_or(false);
if !global_data.state.mouse_drag_active
    && !focused_is_visible
    && let Some(first) = visible.first()
{
    global_data.state.pane_manager.focused_window = Some(first.window.clone());
}
```

- [ ] **Step 3: Rewrite `PanesRenderer` to render columns with vertical stacks**

Replace the existing `PanesRenderer` impl with:

```rust
struct PanesRenderer<'a> {
    visible: &'a [PaneSlot],
}

impl SurfaceRender<AppState, AppSignal> for PanesRenderer<'_> {
    fn render_in_surface(
        &mut self,
        surface: &mut Surface,
        global_data: &mut GlobalData<AppState, AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<AppState, AppSignal>,
        has_focus: &mut HasFocus,
    ) -> CommonResult<()> {
        throws!({
            let container_id = FlexBoxId::from(Id::Container);
            box_start!(
                in: surface,
                id: container_id,
                dir: LayoutDirection::Horizontal,
                requested_size_percent: req_size_pc!(width: 100, height: 100),
                styles: [container_id],
            );

            let window_size = global_data.window_size;
            let surface_rows = window_size.row_height.as_u16().saturating_sub(1);
            let mut current_col_origin: Option<u16> = None;
            let mut column_id = FlexBoxId::from(Id::Container);

            for slot in self.visible {
                let slot_origin_col = slot.box_.style_adjusted_origin_pos.col_index.as_u16();
                let slot_width = slot.box_.style_adjusted_bounds_size.col_width.as_u16();
                let slot_height = slot.box_.style_adjusted_bounds_size.row_height.as_u16();

                if current_col_origin != Some(slot_origin_col) {
                    if current_col_origin.is_some() {
                        box_end!(in: surface);
                    }
                    current_col_origin = Some(slot_origin_col);
                    column_id = FlexBoxId::from(Id::Container);
                    let width_pc = (slot_width as i32 * 100)
                        .div_euclid(window_size.col_width.as_u16() as i32);
                    box_start!(
                        in: surface,
                        id: column_id,
                        dir: LayoutDirection::Vertical,
                        requested_size_percent: req_size_pc!(width: {width_pc}, height: 100),
                        styles: [column_id],
                    );
                }

                let pane_id = FlexBoxId::from(Id::pane(slot.slot));
                let height_pc = (slot_height as i32 * 100)
                    .div_euclid(surface_rows as i32)
                    .max(1);
                box_start!(
                    in: surface,
                    id: pane_id,
                    dir: LayoutDirection::Vertical,
                    requested_size_percent: req_size_pc!(width: 100, height: {height_pc}),
                    styles: [pane_id],
                );
                render_component_in_current_box!(
                    in: surface,
                    component_id: pane_id,
                    from: component_registry_map,
                    global_data: global_data,
                    has_focus: has_focus
                );
                box_end!(in: surface);
            }

            if current_col_origin.is_some() {
                box_end!(in: surface);
            }
            box_end!(in: surface);
        });
    }
}
```

> The renderer walks slots in layout order. Whenever the column origin changes it closes the previous vertical column box and opens a new one.

- [ ] **Step 4: Remove `PanesRenderer` mutation of `window_stack`**

The old code did `global_data.state.window_stack[slot] = window.clone();` inside the render loop. Delete that line. The component reads the window from the layout result instead.

- [ ] **Step 5: Commit renderer update**

```bash
cd /home/deck/repos/explorer
git add src/tui/app.rs
git commit -m "feat: render panes using PaneManager layout"
```

---

## Task 8: Update focus helpers and key bindings in `app.rs`

**Files:**
- Modify: `src/tui/app.rs`

- [ ] **Step 1: Replace `focused_pane_id` and `sync_has_focus`**

```rust
pub(super) fn focused_pane_id(state: &AppState) -> FlexBoxId {
    let Some(slot) = state.pane_manager.focused_slot() else {
        return FlexBoxId::from(Id::Pane0);
    };
    FlexBoxId::from(Id::pane(slot))
}

fn sync_has_focus(state: &AppState, has_focus: &mut HasFocus) {
    has_focus.set_id(focused_pane_id(state));
}
```

- [ ] **Step 2: Remove the old `cycle_focus` free function**

`PaneManager::cycle_focus` replaces it. Delete:

```rust
fn cycle_focus(state: &mut AppState, visible: &[(Window, u16)], direction: i32) { ... }
```

- [ ] **Step 3: Update leader-key dispatch for pane resize/reorder**

In the leader-key dispatch block, after the existing arms, add:

```rust
InputEvent::Keyboard(KeyPress::Plain {
    key: Key::Character('='),
}) => {
    global_data.state.pane_manager.resize_focused(ResizeDelta::Grow);
    return Ok(EventPropagation::ConsumedRender);
}
InputEvent::Keyboard(KeyPress::Plain {
    key: Key::Character('-'),
}) => {
    global_data.state.pane_manager.resize_focused(ResizeDelta::Shrink);
    return Ok(EventPropagation::ConsumedRender);
}
InputEvent::Keyboard(KeyPress::Plain {
    key: Key::SpecialKey(SpecialKey::Up),
}) => {
    let Some(focused) = global_data.state.pane_manager.focused_window.clone() else {
        return Ok(EventPropagation::ConsumedRender);
    };
    global_data.state.pane_manager.move_forward(&focused);
    return Ok(EventPropagation::ConsumedRender);
}
InputEvent::Keyboard(KeyPress::Plain {
    key: Key::SpecialKey(SpecialKey::Down),
}) => {
    let Some(focused) = global_data.state.pane_manager.focused_window.clone() else {
        return Ok(EventPropagation::ConsumedRender);
    };
    global_data.state.pane_manager.move_backward(&focused);
    return Ok(EventPropagation::ConsumedRender);
}
```

- [ ] **Step 4: Update global shortcut handlers for focus cycling and pane resize/reorder**

Replace the existing Tab/BackTab global shortcut block with:

```rust
if (!matches!(global_data.state.pane_manager.focused_window, Some(Window::Terminal(_)))
    || !global_data.state.terminal_grabbed)
    && !global_data.state.mouse_drag_active
{
    match &input_event {
        InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(SpecialKey::Tab),
        }) => {
            let visible = global_data.state.pane_manager.layout(global_data.window_size);
            global_data.state.pane_manager.cycle_focus(&visible, 1);
            return Ok(EventPropagation::ConsumedRender);
        }
        InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(SpecialKey::BackTab),
        }) => {
            let visible = global_data.state.pane_manager.layout(global_data.window_size);
            global_data.state.pane_manager.cycle_focus(&visible, -1);
            return Ok(EventPropagation::ConsumedRender);
        }
        InputEvent::Keyboard(KeyPress::WithModifiers {
            key: Key::Character('='),
            mask:
                ModifierKeysMask {
                    ctrl_key_state: KeyState::Pressed,
                    ..
                },
        }) => {
            global_data.state.pane_manager.resize_focused(ResizeDelta::Grow);
            return Ok(EventPropagation::ConsumedRender);
        }
        InputEvent::Keyboard(KeyPress::WithModifiers {
            key: Key::Character('-'),
            mask:
                ModifierKeysMask {
                    ctrl_key_state: KeyState::Pressed,
                    ..
                },
        }) => {
            global_data.state.pane_manager.resize_focused(ResizeDelta::Shrink);
            return Ok(EventPropagation::ConsumedRender);
        }
        InputEvent::Keyboard(KeyPress::WithModifiers {
            key: Key::SpecialKey(SpecialKey::Up),
            mask:
                ModifierKeysMask {
                    ctrl_key_state: KeyState::Pressed,
                    ..
                },
        }) => {
            let Some(focused) = global_data.state.pane_manager.focused_window.clone() else {
                return Ok(EventPropagation::ConsumedRender);
            };
            global_data.state.pane_manager.move_forward(&focused);
            return Ok(EventPropagation::ConsumedRender);
        }
        InputEvent::Keyboard(KeyPress::WithModifiers {
            key: Key::SpecialKey(SpecialKey::Down),
            mask:
                ModifierKeysMask {
                    ctrl_key_state: KeyState::Pressed,
                    ..
                },
        }) => {
            let Some(focused) = global_data.state.pane_manager.focused_window.clone() else {
                return Ok(EventPropagation::ConsumedRender);
            };
            global_data.state.pane_manager.move_backward(&focused);
            return Ok(EventPropagation::ConsumedRender);
        }
        _ => {}
    }
}
```

- [ ] **Step 5: Update leader Tab/BackTab cycle focus calls**

Replace the existing leader Tab/BackTab arms with:

```rust
InputEvent::Keyboard(KeyPress::Plain {
    key: Key::SpecialKey(SpecialKey::Tab),
}) => {
    let visible = global_data.state.pane_manager.layout(global_data.window_size);
    global_data.state.pane_manager.cycle_focus(&visible, 1);
    return Ok(EventPropagation::ConsumedRender);
}
InputEvent::Keyboard(KeyPress::Plain {
    key: Key::SpecialKey(SpecialKey::BackTab),
}) => {
    let visible = global_data.state.pane_manager.layout(global_data.window_size);
    global_data.state.pane_manager.cycle_focus(&visible, -1);
    return Ok(EventPropagation::ConsumedRender);
}
```

- [ ] **Step 6: Update focus-follows-mouse**

Replace the existing focus-follows-mouse block with:

```rust
if let InputEvent::Mouse(MouseInput {
    kind: MouseInputKind::MouseMove,
    maybe_modifier_keys: None,
    pos,
}) = &input_event
    && !global_data.state.mouse_drag_active
{
    let layout = global_data.state.pane_manager.layout(global_data.window_size);
    for slot in &layout {
        let ox = slot.box_.style_adjusted_origin_pos.col_index;
        let oy = slot.box_.style_adjusted_origin_pos.row_index;
        let w = slot.box_.style_adjusted_bounds_size.col_width;
        let h = slot.box_.style_adjusted_bounds_size.row_height;
        if pos.col_index >= ox
            && pos.col_index < ox + w
            && pos.row_index >= oy
            && pos.row_index < oy + h
        {
            if global_data.state.pane_manager.focused_window.as_ref() != Some(&slot.window) {
                global_data.state.pane_manager.focused_window = Some(slot.window.clone());
                return Ok(EventPropagation::ConsumedRender);
            }
            break;
        }
    }
}
```

- [ ] **Step 7: Update remaining `window_stack` / `focused_window` / `window_states` accesses in `app.rs`**

Find/replace the following patterns in `app.rs`:

- `state.window_stack` → `state.pane_manager.window_stack`
- `state.focused_window` → `state.pane_manager.focused_window`
- `state.window_states` → `state.pane_manager.window_states`
- `state.push_window(...)` → `state.pane_manager.push_window(...)`
- `state.remove_window(...)` → `state.pane_manager.remove_window(...)`
- `state.send_to_back(...)` → `state.pane_manager.send_to_back(...)`
- `state.set_window_scroll(...)` → `state.pane_manager.set_window_scroll(...)`
- `state.window_scroll(...)` → `state.pane_manager.window_scroll(...)`
- `state.window_page_size(...)` → `state.pane_manager.window_page_size(...)`
- `state.window_scroll_max(...)` → `state.pane_manager.window_scroll_max(...)`
- `state.clamp_scroll(...)` → `state.pane_manager.clamp_scroll(...)`
- `state.visible_windows(...)` → `state.pane_manager.layout(...)`

Also remove the `visible_count` width estimate in `open_terminal` and use the current layout's first column width, or simply `window_size.col_width` with a note that the first render will resize.

- [ ] **Step 8: Commit key binding and focus updates**

```bash
cd /home/deck/repos/explorer
git add src/tui/app.rs
git commit -m "feat: add pane resize/reorder bindings and update focus handling"
```

---

## Task 9: Update `PaneComponent` to read windows from the layout result

**Files:**
- Modify: `src/tui/pane_component.rs`
- Modify: `src/tui/state.rs`
- Modify: `src/tui/app.rs`

`PaneComponent::active_window` currently reads `state.window_stack.get(self.slot)`. After the refactor, the component is rendered for a specific `PaneSlot`, but it still needs to know which window is in its slot.

To avoid a stale cache, `PaneComponent::active_window` recomputes the layout on demand using the last known surface size stored in `AppState`. The layout computation is cheap and guarantees the component always sees the current stack state.

- [ ] **Step 1: Add `pub last_surface_size: Size` to `AppState`**

In `state.rs`, add to `AppState`:

```rust
pub last_surface_size: Size,
```

Initialize it to `Size::default()` in `AppState::new`.

- [ ] **Step 2: Update `app_handle_input_event` and `app_render` to refresh `last_surface_size`**

In `app_handle_input_event`, after `sync_has_focus`, set:

```rust
global_data.state.last_surface_size = surface_size(global_data.window_size);
```

In `app_render`, after computing `surface_size`, set:

```rust
global_data.state.last_surface_size = surface_size;
```

- [ ] **Step 3: Update `PaneComponent::active_window`**

Replace:

```rust
fn active_window<'s>(&self, state: &'s AppState) -> Option<&'s Window> {
    state.window_stack.get(self.slot)
}
```

with:

```rust
fn active_window(&self, state: &AppState) -> Option<Window> {
    state
        .pane_manager
        .layout(state.last_surface_size)
        .iter()
        .find(|slot| slot.slot == self.slot)
        .map(|slot| slot.window.clone())
}
```

Remove redundant `.cloned()` calls at the call sites since `active_window` now returns an owned `Option<Window>`.

- [ ] **Step 4: Remove `global_data.state.pane_boxes[self.slot] = current_box;`**

`pane_boxes` no longer exists. If the component needs its box for events, it already stores `content_origin_*` and `content_col_count` / `content_row_count` from render.

- [ ] **Step 5: Commit layout-on-demand read**

```bash
cd /home/deck/repos/explorer
git add src/tui/state.rs src/tui/app.rs src/tui/pane_component.rs
git commit -m "feat: read active window from layout on demand"
```

---

## Task 10: Update remaining TUI modules to use `PaneManager`

**Files:**
- Modify: `src/tui/file_name_picker.rs`
- Modify: `src/tui/theme_picker.rs`
- Modify: `src/tui/preview.rs`
- Modify: `src/tui/terminal_pane.rs`

- [ ] **Step 1: Update `file_name_picker.rs`**

Replace:
- `state.window_stack.clone()` → `state.pane_manager.window_stack.clone()`
- `state.window_states.contains_key(...)` → `state.pane_manager.window_states.contains_key(...)`
- `state.set_window_scroll(...)` → `state.pane_manager.set_window_scroll(...)`
- `state.push_window(...)` → `state.pane_manager.push_window(...)`
- `state.focused_window = ...` → `state.pane_manager.focused_window = ...`
- `state.remove_window(...)` → `state.pane_manager.remove_window(...)`
- `&state.window_stack` → `&state.pane_manager.window_stack`

- [ ] **Step 2: Update `theme_picker.rs`**

Replace `state.remove_window(...)` with `state.pane_manager.remove_window(...)`.

- [ ] **Step 3: Update `preview.rs`**

Replace:
- `state.window_stack.get(slot)?` → read from `state.current_layout` or use `state.pane_manager.window_stack.get(slot)?`
- `state.send_to_back(...)` → `state.pane_manager.send_to_back(...)`
- `state.window_scroll(...)` → `state.pane_manager.window_scroll(...)`
- `state.window_page_size(...)` → `state.pane_manager.window_page_size(...)`
- `state.set_window_scroll(...)` → `state.pane_manager.set_window_scroll(...)`
- `state.clamp_scroll(...)` → `state.pane_manager.clamp_scroll(...)`
- Remove the local `pane_slot` function and import `crate::tui::pane_component::pane_slot`.

- [ ] **Step 4: Update `terminal_pane.rs`**

Replace:
- `state.window_stack.get(slot)?` → `state.pane_manager.window_stack.get(slot)?`
- `state.remove_window(...)` → `state.pane_manager.remove_window(...)`
- Remove the local `pane_slot` function and import `crate::tui::pane_component::pane_slot`.

The `global_data.state.pane_boxes[slot]` accesses for `PageUp`/`PageDown` height need to use the current layout instead:

```rust
let pane_height = state
    .pane_manager
    .layout(state.last_surface_size)
    .iter()
    .find(|s| s.slot == slot)
    .map(|s| s.box_.style_adjusted_bounds_size.row_height.as_usize())
    .unwrap_or(0);
```

- [ ] **Step 5: Commit module updates**

```bash
cd /home/deck/repos/explorer
git add src/tui/file_name_picker.rs src/tui/theme_picker.rs src/tui/preview.rs src/tui/terminal_pane.rs
git commit -m "refactor: update picker/preview/terminal for PaneManager"
```

---

## Task 11: Update status bar hints

**Files:**
- Modify: `src/tui/app.rs`

- [ ] **Step 1: Add pane resize/reorder hints to the leader status bar text**

Update the leader `rest_text` in `render_status_bar`:

```rust
(
    " Leader ".to_string(),
    "f:Picker  t:Term  T:Theme  x:Close  q:Quit  Tab:Next  Shift+Tab:Prev  =:Grow  -:Shrink  ↑:Forward  ↓:Backward  Esc:Cancel"
        .to_string(),
)
```

- [ ] **Step 2: Add global hints when a non-terminal pane is focused**

Update the non-leader hints to include the new global shortcuts when not in a terminal:

```rust
let pane = match state.pane_manager.focused_window.as_ref() {
    Some(w) => {
        let base = w.pane_key_hints();
        if matches!(w, Window::Terminal(_)) && !state.terminal_grabbed {
            "Enter:grab  ↑↓PgUp/PgDn:scroll"
        } else {
            base
        }
    }
    None => "",
};
let mut rest = String::new();
rest.push_str("Ctrl+=:Grow  Ctrl+-:Shrink  Ctrl+↑:Forward  Ctrl+↓:Backward");
if !pane.is_empty() {
    rest.push_str("  ");
    rest.push_str(pane);
}
```

- [ ] **Step 3: Commit status bar update**

```bash
cd /home/deck/repos/explorer
git add src/tui/app.rs
git commit -m "feat: show pane resize/reorder hints in status bar"
```

---

## Task 12: Add unit tests for layout

**Files:**
- Modify: `src/tui/pane_manager.rs`

- [ ] **Step 1: Add a test module for `PaneManager::layout`**

Append to `pane_manager.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_manager(stack: &[Window]) -> PaneManager {
        let mut manager = PaneManager::new();
        for window in stack {
            manager.push_window(window.clone());
        }
        if let Some(first) = manager.window_stack.first() {
            manager.focused_window = Some(first.clone());
        }
        manager
    }

    fn size(cols: u16, rows: u16) -> Size {
        r3bl_tui::col(cols) + r3bl_tui::row(rows)
    }

    #[test]
    fn single_full_pane_uses_full_height() {
        let manager = make_manager(&[Window::FileNamePicker]);
        let slots = manager.layout(size(80, 24));
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].box_.style_adjusted_bounds_size.row_height.as_u16(), 24);
    }

    #[test]
    fn two_columns_for_wide_surface() {
        let manager = make_manager(&[Window::FileNamePicker, Window::ThemePicker]);
        let slots = manager.layout(size(240, 24));
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].box_.style_adjusted_origin_pos.col_index.as_u16(), 0);
        assert_eq!(slots[1].box_.style_adjusted_origin_pos.col_index.as_u16(), 120);
    }

    #[test]
    fn half_panes_stack_in_one_column() {
        let mut manager = make_manager(&[Window::FileNamePicker, Window::ThemePicker]);
        for state in manager.window_states.values_mut() {
            state.pane_size = PaneSize::Half;
        }
        let slots = manager.layout(size(80, 24));
        assert_eq!(slots.len(), 2);
        assert_eq!(slots[0].box_.style_adjusted_bounds_size.row_height.as_u16(), 12);
        assert_eq!(slots[1].box_.style_adjusted_bounds_size.row_height.as_u16(), 12);
    }

    #[test]
    fn last_full_pane_shrinks_to_fill_remaining_space() {
        let mut manager = make_manager(&[
            Window::FileNamePicker,
            Window::ThemePicker,
            Window::Terminal(0),
        ]);
        manager.window_states.get_mut(&Window::FileNamePicker).unwrap().pane_size = PaneSize::Full;
        manager.window_states.get_mut(&Window::ThemePicker).unwrap().pane_size = PaneSize::Half;
        manager.window_states.get_mut(&Window::Terminal(0)).unwrap().pane_size = PaneSize::Full;
        let slots = manager.layout(size(200, 24));
        assert_eq!(slots.len(), 3);
        // First column: full height.
        assert_eq!(slots[0].box_.style_adjusted_bounds_size.row_height.as_u16(), 24);
        // Second column top: half height.
        assert_eq!(slots[1].box_.style_adjusted_bounds_size.row_height.as_u16(), 12);
        // Second column bottom: shrunk to remaining 12 rows.
        assert_eq!(slots[2].box_.style_adjusted_bounds_size.row_height.as_u16(), 12);
        assert_eq!(slots[2].box_.style_adjusted_origin_pos.row_index.as_u16(), 12);
    }

    #[test]
    fn four_quarters_fill_column() {
        let mut manager = PaneManager::new();
        for i in 0..4 {
            manager.push_window(Window::Terminal(i));
            manager.window_states.get_mut(&Window::Terminal(i)).unwrap().pane_size = PaneSize::Quarter;
        }
        let slots = manager.layout(size(80, 24));
        assert_eq!(slots.len(), 4);
        for slot in &slots {
            assert_eq!(slot.box_.style_adjusted_bounds_size.row_height.as_u16(), 6);
        }
    }

    #[test]
    fn grow_and_shrink_clamp() {
        assert_eq!(PaneSize::Full.grow(), PaneSize::Full);
        assert_eq!(PaneSize::Quarter.shrink(), PaneSize::Quarter);
        assert_eq!(PaneSize::Quarter.grow(), PaneSize::Third);
        assert_eq!(PaneSize::Full.shrink(), PaneSize::Half);
    }

    #[test]
    fn move_forward_and_backward() {
        let mut manager = make_manager(&[
            Window::FileNamePicker,
            Window::ThemePicker,
            Window::Terminal(0),
        ]);
        manager.move_backward(&Window::FileNamePicker);
        assert_eq!(
            manager.window_stack,
            vec![Window::ThemePicker, Window::FileNamePicker, Window::Terminal(0)]
        );
        manager.move_forward(&Window::FileNamePicker);
        assert_eq!(
            manager.window_stack,
            vec![Window::FileNamePicker, Window::ThemePicker, Window::Terminal(0)]
        );
    }
}
```

> Some helpers like `r3bl_tui::col` / `r3bl_tui::row` may need to be imported. Adjust imports as needed.

- [ ] **Step 2: Run the new tests**

```bash
cd /home/deck/repos/explorer
cargo test pane_manager::tests -- --nocapture
```

Expected: all tests pass once the crate compiles.

- [ ] **Step 3: Commit tests**

```bash
cd /home/deck/repos/explorer
git add src/tui/pane_manager.rs
git commit -m "test: add PaneManager layout and stack tests"
```

---

## Task 13: Build, format, and lint

**Files:**
- All modified `.rs` files.

- [ ] **Step 1: Format code**

```bash
cd /home/deck/repos/explorer
cargo fmt
```

Expected: no output means success.

- [ ] **Step 2: Build the project**

```bash
cd /home/deck/repos/explorer
cargo build --release
```

Expected: successful build with no errors.

- [ ] **Step 3: Run clippy**

```bash
cd /home/deck/repos/explorer
cargo clippy --no-deps
```

Expected: no warnings. Address any warnings before proceeding.

- [ ] **Step 4: Run all tests**

```bash
cd /home/deck/repos/explorer
cargo test
```

Expected: all tests pass.

- [ ] **Step 5: Commit formatting and fixes**

```bash
cd /home/deck/repos/explorer
git add -A
git commit -m "style: format and address clippy warnings"
```

---

## Self-Review Checklist

- [ ] **Spec coverage:** Every requirement in the design doc maps to a task:
  - `PaneSize` enum and grow/shrink methods → Task 1.
  - `WindowState.pane_size` → Task 1.
  - `PaneManager` with stack/focus/layout → Tasks 1–2.
  - `MAX_PANES = 16` → Tasks 3, 6.
  - Global `Ctrl+=/-` and `Ctrl+Up/Down` → Task 8.
  - Leader `=/-` and `Up/Down` → Task 8.
  - Layout algorithm with last-pane shrink → Task 2.
  - `PaneComponent` split → Task 4.
  - Module reorganization → Tasks 4–5.
  - Update pickers/preview/terminal → Task 10.
  - Status bar hints → Task 11.
  - Tests → Task 12.
- [ ] **Placeholder scan:** No `TBD`, `TODO`, or "add appropriate error handling" remain.
- [ ] **Type consistency:** `PaneSize`, `ResizeDelta`, `PaneSlot`, `PaneManager` names are consistent across all tasks.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-06-12-resizable-pane-stack.md`.**

Two execution options:

1. **Subagent-Driven (recommended)** — Dispatch a fresh subagent per task, review between tasks, fast iteration. Use the `superpowers:subagent-driven-development` skill.

2. **Inline Execution** — Execute tasks in this session using the `superpowers:executing-plans` skill, with batch execution and checkpoints for review.

Which approach would you like?
