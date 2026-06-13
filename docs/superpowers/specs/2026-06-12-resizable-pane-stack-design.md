# Resizable Pane Stack Design

## Summary

Add per-pane vertical sizes (full, half, third, quarter) so the window stack can be
tiled both horizontally and vertically. Introduce new global key bindings to grow,
shrink, and reorder panes. Refactor pane stack and layout logic into a dedicated
`PaneManager` module, and split `PaneComponent` out of `app.rs`.

## Motivation

Currently every pane is full-height. A wide terminal can show two columns, but each
pane still occupies the full vertical space. Supporting smaller vertical sizes lets
users see more panes at once (for example, one full-height pane on the left and two
half-height panes stacked on the right).

## Terminology

- **Window**: a logical pane (`FileNamePicker`, `ThemePicker`, `FilePreview(key)`,
  `Terminal(id)`).
- **Window stack**: ordered list of open windows, index `0` = front/leftmost.
- **Pane size**: vertical height of a window in the layout: `Full`, `Half`, `Third`,
  or `Quarter`.
- **Pane slot**: one of the component slots `0..MAX_PANES` used to render a visible
  window.

## Decisions

- Pane size is stored inside `WindowState.pane_size`. This keeps all per-window pane
  configuration in one map.
- `MAX_PANES` is increased from `5` to `16`. A dynamic component pool would require
  refactoring `ComponentRegistry`; a fixed cap of `16` covers three columns of
  quarter-height panes on very wide screens without that cost.
- New bindings are available both as global shortcuts and as leader commands:
  - Global: `Ctrl + =` grows, `Ctrl + -` shrinks, `Ctrl + Up` moves forward, `Ctrl + Down`
    moves backward.
  - Leader (`Alt + `` then): `=` grows, `-` shrinks, `Up` moves forward, `Down` moves
    backward.
- Leader bindings intentionally work even when a terminal pane has grabbed the
  keyboard, because entering leader mode ungrabs the terminal.
- Grow/shrink clamp at `Full` / `Quarter`; no wrap-around.
- The last visible pane in the rightmost column is shrunk to fill any remaining
  vertical space so there is never an empty gap.

## Module Layout

```
src/tui/
  pane_manager.rs   # PaneManager, PaneSize, layout, focus helpers
  pane_component.rs # PaneComponent and scrollbar helpers
  state.rs          # AppState, Window, TextSelection, TerminalPane, AppSignal
  app.rs            # AppMain, PanesRenderer, status bar, lifecycle
  mod.rs            # module declarations
```

## Data Model

### `PaneSize`

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaneSize {
    #[default]
    Full,
    Half,
    Third,
    Quarter,
}

impl PaneSize {
    /// Vertical space consumed as a fraction of the available content rows.
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
```

### `WindowState`

`WindowState` moves from `state.rs` to `pane_manager.rs` and gains `pane_size`:

```rust
#[derive(Clone, Debug, Default)]
pub struct WindowState {
    pub scroll: usize,
    pub page_size: usize,
    pub scroll_max: usize,
    pub pane_size: PaneSize,
}
```

### `PaneManager`

```rust
pub struct PaneManager {
    pub window_stack: Vec<Window>,
    pub focused_window: Option<Window>,
    pub window_states: HashMap<Window, WindowState>,
}

#[derive(Clone, Debug)]
pub struct PaneSlot {
    pub slot: usize,       // component slot 0..MAX_PANES
    pub window: Window,
    pub box_: FlexBox,     // computed screen position and size
}
```

Methods on `PaneManager`:

- `push_window(&mut self, window: Window)`
- `remove_window(&mut self, window: &Window)`
- `send_to_back(&mut self, window: &Window)`
- `move_forward(&mut self, window: &Window)` — swap with previous element.
- `move_backward(&mut self, window: &Window)` — swap with next element.
- `resize_focused(&mut self, delta: ResizeDelta)` — grow or shrink focused window.
- `layout(&self, surface_size: Size) -> Vec<PaneSlot>`
- `focused_pane_id(&self) -> FlexBoxId`
- `cycle_focus(&mut self, visible: &[PaneSlot], direction: i32)`

```rust
pub enum ResizeDelta {
    Grow,
    Shrink,
}
```

`AppState` will hold `pub pane_manager: PaneManager` and drop the separate
`window_stack`, `focused_window`, and `window_states` fields. `pane_boxes` is also
removed; layout is computed on demand.

## Layout Algorithm

Inputs: `window_stack`, `window_states`, `surface_size`.

`surface_size` is the size passed to `PanesRenderer`, which is the terminal height
minus the status bar row.

```text
content_rows = surface_rows
cols = max(1, surface_cols / MIN_PANE_WIDTH)
column_width = surface_cols / cols
remainder = surface_cols % cols

result = []
current_col = 0
used_rows_in_col = 0

for window in window_stack:
    if current_col >= cols:
        break

    remaining_rows = content_rows - used_rows_in_col
    if remaining_rows == 0:
        current_col += 1
        used_rows_in_col = 0
        if current_col >= cols:
            break
        remaining_rows = content_rows

    requested_rows = (content_rows as f32 * window.pane_size.height_factor()) as usize
    requested_rows = max(1, requested_rows)

    if requested_rows > remaining_rows:
        // Last column, last visible pane: shrink to fill.
        // Otherwise try next column.
        is_last_col = current_col == cols - 1
        more_windows = there are windows after this one
        if is_last_col && !more_windows:
            actual_rows = remaining_rows
        else if !is_last_col:
            current_col += 1
            used_rows_in_col = 0
            remaining_rows = content_rows
            if requested_rows > remaining_rows:
                actual_rows = remaining_rows
            else:
                actual_rows = requested_rows
        else:
            actual_rows = remaining_rows
    else:
        actual_rows = requested_rows

    origin_col = sum(widths of columns before current_col)
    origin_row = used_rows_in_col
    width = column_width + 1 if current_col < remainder else column_width

    emit PaneSlot { slot: result.len(), window, box_ }

    used_rows_in_col += actual_rows

    if result.len() == MAX_PANES:
        break
```

Panes are emitted in stack order. Column widths distribute the remainder pixels
left-to-right, matching the current `visible_windows` behavior.

### Layout examples

Surface: `2` columns, `24` content rows.

- `[Full]` → one full-height pane in column 0.
- `[Full, Half, Half]` → full in column 0; two half panes stacked in column 1.
- `[Full, Half, Full]` → full in column 0; half in column 1 top; last full does not
  fit, so it is shrunk to the remaining `12` rows and placed below the half pane.
- `[Quarter, Quarter, Quarter, Quarter, Quarter, Quarter, Quarter, Quarter]` → four
  quarters in column 0 and four in column 1.

## Rendering

`PanesRenderer::render_in_surface` calls `state.pane_manager.layout(surface_size)`
and renders nested boxes:

1. Start an outer horizontal container covering the full content area.
2. For each column, start a vertical sub-container with the column width.
3. For each `PaneSlot` in that column, start a vertical box with the computed
   height and render the `PaneComponent` for `slot`.

`PaneComponent` already renders whichever window is assigned to its slot by reading
from the layout result via `active_window`. No change to its rendering logic is
required beyond making sure it reads the correct window for its slot.

## Focus and Input

### Global shortcuts

In `AppMain::app_handle_input_event`, add handlers before routing to the focused
component:

- `Ctrl + =` → `state.pane_manager.resize_focused(ResizeDelta::Grow)`.
- `Ctrl + -` → `state.pane_manager.resize_focused(ResizeDelta::Shrink)`.
- `Ctrl + Up` → `state.pane_manager.move_forward(focused)`.
- `Ctrl + Down` → `state.pane_manager.move_backward(focused)`.

These are consumed and trigger a re-render. If no window is focused, the
resize/move handlers do nothing.

### Leader commands

In the leader-key dispatch block, add:

- `=` → `state.pane_manager.resize_focused(ResizeDelta::Grow)`.
- `-` → `state.pane_manager.resize_focused(ResizeDelta::Shrink)`.
- `Up` → `state.pane_manager.move_forward(focused)`.
- `Down` → `state.pane_manager.move_backward(focused)`.

These reuse the same `PaneManager` methods as the global shortcuts.

### Focus cycling

`cycle_focus` takes the visible `PaneSlot`s and steps to the next/previous slot.
The focused window becomes the window at that slot.

### Focus-follows-mouse

On mouse move, compute the current layout and find the `PaneSlot` whose `box_`
contains the cursor. If it differs from the focused window, update focus.

`AppState.pane_boxes` is removed because layout is computed on demand.

## Terminal PTY Sizing

`TerminalPaneComponent::render` already resizes the PTY from `current_box`, so
terminals adapt automatically when the layout changes.

`AppMain::open_terminal` currently estimates PTY columns as
`window_size.col_width / visible_count`. Replace this with the width the new
terminal would occupy under the current layout, or simply start with the full
window width; the first render will resize it correctly.

## Stack Operations

### `move_forward`

Swap the window with the element before it in `window_stack`. If it is already at
index `0`, do nothing.

### `move_backward`

Swap the window with the element after it in `window_stack`. If it is already at
the last index, do nothing.

### `remove_window`

Keep current behavior: remove the window from the stack, drop its `WindowState`,
reset `terminal_grabbed` if it was a terminal, and move focus to the new front
window if the removed window was focused.

## Defaults and Cleanup

- New windows get `WindowState::default()`, so `PaneSize::Full`.
- When a window is removed, its `WindowState` entry is removed.

## Testing

Add unit tests for `PaneManager::layout` in `pane_manager.rs` covering:

- Single `Full` pane on small and wide surfaces.
- `[Full, Half, Full]` with two columns → last pane shrunk to half on bottom-right.
- Eight `Quarter` panes with two columns → four stacked per column.
- Mixed sizes: `[Full, Half, Half]` with two columns.
- Layout respects `MAX_PANES`: stops emitting after 16 slots.
- Grow/shrink clamp at boundaries.
- `move_forward` and `move_backward` preserve order of other windows.

Also run `cargo build`, `cargo fmt`, and `cargo clippy --no-deps` before committing.
