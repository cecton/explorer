use crate::loader::FileKey;
use r3bl_tui::{FlexBox, Size, col, height, row, width};
use std::collections::HashMap;

/// Maximum number of panes the layout engine may render at once.
pub const MAX_PANES: usize = 16;

/// A pane that can appear in the window stack.
///
/// Each variant is unique: there is at most one `FileNamePicker` and at most one
/// `FilePreview` per `FileKey` in the stack at any time.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Window {
    /// Preview of the file identified by `FileKey`.
    FilePreview(FileKey),
    /// Fuzzy file-name picker overlay.
    FileNamePicker,
    /// Theme picker overlay with live preview.
    ThemePicker,
    /// Embedded terminal pane identified by its session id.
    Terminal(usize),
}

/// A point in a window's content used as a text-selection anchor or endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelPoint {
    /// Point inside a file preview.
    Preview {
        /// Zero-based line index in the preview content.
        line_idx: usize,
        /// Byte offset within that line.
        byte_offset: usize,
    },
    /// Point inside a terminal pane viewport.
    Terminal {
        /// Zero-based row within the visible terminal viewport.
        viewport_row: usize,
        /// Zero-based column within that row.
        col: usize,
    },
}

/// Active text selection within a single window.
#[derive(Clone, Debug)]
pub struct TextSelection {
    /// Window that owns this selection.
    pub window: Window,
    /// Selection start point.
    pub start: SelPoint,
    /// Selection end point.
    pub end: SelPoint,
    /// Point where the current click/drag sequence began.
    pub click_anchor: Option<SelPoint>,
    /// Word boundary pair detected from a single click.
    pub click_word: Option<(SelPoint, SelPoint)>,
    /// Whether the selection is still being dragged.
    pub active: bool,
}

/// Relative height of a pane within the available vertical space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PaneSize {
    /// Occupies the full available vertical height.
    #[default]
    Full,
    /// Occupies half of the available vertical height.
    Half,
    /// Occupies one third of the available vertical height.
    Third,
    /// Occupies one quarter of the available vertical height.
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

/// Direction in which to resize the focused pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeDelta {
    /// Increase the focused pane's vertical size.
    Grow,
    /// Decrease the focused pane's vertical size.
    Shrink,
}

/// Scroll, paging, and sizing state for a single open window.
#[derive(Clone, Debug, Default)]
pub struct WindowState {
    /// Current scroll offset in content rows.
    pub scroll: usize,
    /// Number of rows visible in the pane at once.
    pub page_size: usize,
    /// Total content height in rows.
    pub scroll_max: usize,
    /// Relative vertical size of the pane.
    pub pane_size: PaneSize,
}

/// A positioned pane produced by the layout engine.
#[derive(Clone, Debug)]
pub struct PaneSlot {
    /// Zero-based position in the visible pane layout.
    pub slot: usize,
    /// Window rendered in this slot.
    pub window: Window,
    /// Computed layout box for the pane.
    pub box_: FlexBox,
}

/// Owns the ordered window stack, per-window state, and focus.
#[derive(Clone, Default)]
pub struct PaneManager {
    /// Stack of open windows, most-recently-opened first (index 0 = leftmost pane).
    pub window_stack: Vec<Window>,
    /// The window that currently receives keyboard input.
    pub focused_window: Option<Window>,
    /// Per-window scroll, page-size, and pane-size state.
    pub window_states: HashMap<Window, WindowState>,
}

impl PaneManager {
    /// Creates a new, empty pane manager.
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
        self.window_states.entry(window).or_default();
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
        let Some(pos) = self.window_stack.iter().position(|w| w == window) else {
            return;
        };
        let w = self.window_stack.remove(pos);
        self.window_stack.push(w);
        if self.focused_window.as_ref() == Some(window) {
            // Focus the pane that took the sent pane's place.
            self.focused_window = self
                .window_stack
                .get(pos)
                .filter(|&&w| w != *window)
                .cloned()
                .or_else(|| self.window_stack.first().cloned());
        }
    }

    /// Swaps `window` toward index 0 (the front / left).
    pub fn move_forward(&mut self, window: &Window) {
        let pos = match self.window_stack.iter().position(|w| w == window) {
            Some(0) | None => return,
            Some(p) => p,
        };
        self.window_stack.swap(pos, pos - 1);
    }

    /// Swaps `window` toward the end (the back / right).
    pub fn move_backward(&mut self, window: &Window) {
        let pos = match self.window_stack.iter().position(|w| w == window) {
            Some(p) if p + 1 < self.window_stack.len() => p,
            _ => return,
        };
        self.window_stack.swap(pos, pos + 1);
    }

    /// Grows or shrinks the focused window's pane size, clamped at the boundaries.
    pub fn resize_focused(&mut self, delta: ResizeDelta) {
        let Some(window) = self.focused_window else {
            return;
        };
        let state = self.window_states.entry(window).or_default();
        state.pane_size = match delta {
            ResizeDelta::Grow => state.pane_size.grow(),
            ResizeDelta::Shrink => state.pane_size.shrink(),
        };
    }

    /// Position of the focused window in the window stack, if any.
    pub fn focused_slot(&self) -> Option<usize> {
        let focused = self.focused_window.as_ref()?;
        self.window_stack.iter().position(|w| w == focused)
    }

    /// Cycles focus through the visible panes.
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
        self.focused_window = Some(visible[next_pos].window);
    }

    /// Lays out visible panes into columns and rows.
    ///
    /// Column count is derived from `surface_size.col_width / MIN_PANE_WIDTH`.
    /// Within each column, panes are stacked top-to-bottom using their requested
    /// `PaneSize`. The last pane in each column is stretched to fill any remaining
    /// vertical space in that column.
    pub fn layout(&self, surface_size: Size) -> Vec<PaneSlot> {
        const MIN_PANE_WIDTH: u16 = 100;

        let surface_cols = surface_size.col_width.as_u16();
        let surface_rows = surface_size.row_height.as_u16();
        if surface_rows == 0 || surface_cols == 0 {
            return Vec::new();
        }

        let cols = (surface_cols / MIN_PANE_WIDTH).max(1) as usize;
        let base_col_width = surface_cols / cols as u16;
        let remainder = surface_cols % cols as u16;

        let mut slots: Vec<PaneSlot> = Vec::with_capacity(self.window_stack.len().min(MAX_PANES));
        let mut current_col: usize = 0;
        let mut used_rows_in_col: u16 = 0;
        let mut origin_col: u16 = 0;
        let mut col_last_slot: Vec<usize> = vec![0; cols];
        let mut col_used_rows: Vec<u16> = vec![0; cols];

        for window in self.window_stack.iter() {
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
                origin_col += base_col_width
                    + if current_col - 1 < remainder as usize {
                        1
                    } else {
                        0
                    };
                remaining_rows = surface_rows;
            }

            let pane_size = self
                .window_states
                .get(window)
                .map(|s| s.pane_size)
                .unwrap_or_default();
            let requested_rows = ((surface_rows as f32 * pane_size.height_factor()) as u16).max(1);

            let actual_rows = if requested_rows > remaining_rows {
                let remaining_pct = remaining_rows * 100 / surface_rows;
                if remaining_pct >= 25 {
                    // Meaningful space — shrink the pane to fit in this column.
                    remaining_rows
                } else {
                    // Small leftover (< 25%) — absorb into the last placed
                    // pane, then wrap (or drop if this is the last column).
                    if let Some(last) = slots.last_mut() {
                        last.box_.style_adjusted_bounds_size.row_height += remaining_rows;
                    }
                    col_used_rows[current_col] += remaining_rows;
                    used_rows_in_col += remaining_rows;
                    if current_col == cols - 1 {
                        continue;
                    }
                    current_col += 1;
                    if current_col >= cols {
                        break;
                    }
                    used_rows_in_col = 0;
                    origin_col += base_col_width
                        + if current_col - 1 < remainder as usize {
                            1
                        } else {
                            0
                        };
                    requested_rows
                }
            } else {
                requested_rows
            };

            let pane_width = base_col_width
                + if current_col < remainder as usize {
                    1
                } else {
                    0
                };

            let origin = col(origin_col) + row(used_rows_in_col);
            let size = width(pane_width) + height(actual_rows);
            let box_ = FlexBox {
                style_adjusted_origin_pos: origin,
                style_adjusted_bounds_size: size,
                ..FlexBox::default()
            };

            slots.push(PaneSlot {
                slot: slots.len(),
                window: *window,
                box_,
            });

            col_last_slot[current_col] = slots.len() - 1;
            col_used_rows[current_col] += actual_rows;
            used_rows_in_col += actual_rows;
        }

        // Stretch the last pane in each column to absorb any rounding leftovers.
        for col in 0..cols {
            let used = col_used_rows[col];
            if used == 0 {
                continue;
            }
            let remaining = surface_rows.saturating_sub(used);
            if remaining > 0 {
                let last_idx = col_last_slot[col];
                slots[last_idx].box_.style_adjusted_bounds_size.row_height += remaining;
            }
        }

        slots
    }
}

impl PaneManager {
    /// Current scroll offset for `window`.
    pub fn window_scroll(&self, window: &Window) -> usize {
        self.window_states
            .get(window)
            .map(|s| s.scroll)
            .unwrap_or(0)
    }

    /// Rendered page size for `window`.
    pub fn window_page_size(&self, window: &Window) -> usize {
        self.window_states
            .get(window)
            .map(|s| s.page_size)
            .unwrap_or(0)
    }

    /// Total content height for `window`.
    pub fn window_scroll_max(&self, window: &Window) -> usize {
        self.window_states
            .get(window)
            .map(|s| s.scroll_max)
            .unwrap_or(0)
    }

    /// Sets the scroll offset for `window`.
    pub fn set_window_scroll(&mut self, window: &Window, scroll: usize) {
        self.window_states.entry(*window).or_default().scroll = scroll;
    }

    /// Sets the rendered page size for `window`.
    pub fn set_window_page_size(&mut self, window: &Window, page_size: usize) {
        self.window_states.entry(*window).or_default().page_size = page_size;
    }

    /// Sets the total content height for `window`.
    pub fn set_window_scroll_max(&mut self, window: &Window, scroll_max: usize) {
        self.window_states.entry(*window).or_default().scroll_max = scroll_max;
    }

    /// Clamps `window`'s scroll offset so the visible page stays within the content.
    pub fn clamp_scroll(&mut self, window: &Window) {
        let Some(state) = self.window_states.get_mut(window) else {
            return;
        };
        if state.scroll_max > state.page_size {
            state.scroll = state.scroll.min(state.scroll_max - state.page_size);
        } else {
            state.scroll = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manager(stack: &[Window]) -> PaneManager {
        let mut manager = PaneManager::new();
        manager.window_stack = stack.to_vec();
        for window in stack {
            manager.window_states.entry(window.clone()).or_default();
        }
        if let Some(first) = manager.window_stack.first() {
            manager.focused_window = Some(first.clone());
        }
        manager
    }

    fn size(cols: u16, rows: u16) -> Size {
        width(cols) + height(rows)
    }

    #[test]
    fn single_full_pane_uses_full_height() {
        let manager = make_manager(&[Window::FileNamePicker]);
        let slots = manager.layout(size(80, 24));
        assert_eq!(slots.len(), 1);
        assert_eq!(
            slots[0].box_.style_adjusted_bounds_size.row_height.as_u16(),
            24
        );
    }

    #[test]
    fn two_columns_for_wide_surface() {
        let manager = make_manager(&[Window::FileNamePicker, Window::ThemePicker]);
        let slots = manager.layout(size(240, 24));
        assert_eq!(slots.len(), 2);
        assert_eq!(
            slots[0].box_.style_adjusted_origin_pos.col_index.as_u16(),
            0
        );
        assert_eq!(
            slots[1].box_.style_adjusted_origin_pos.col_index.as_u16(),
            120
        );
    }

    #[test]
    fn half_panes_stack_in_one_column() {
        let mut manager = make_manager(&[Window::FileNamePicker, Window::ThemePicker]);
        for state in manager.window_states.values_mut() {
            state.pane_size = PaneSize::Half;
        }
        let slots = manager.layout(size(80, 24));
        assert_eq!(slots.len(), 2);
        assert_eq!(
            slots[0].box_.style_adjusted_bounds_size.row_height.as_u16(),
            12
        );
        assert_eq!(
            slots[1].box_.style_adjusted_bounds_size.row_height.as_u16(),
            12
        );
    }

    #[test]
    fn last_full_pane_shrinks_to_fill_remaining_space() {
        let mut manager = make_manager(&[
            Window::FileNamePicker,
            Window::ThemePicker,
            Window::Terminal(0),
        ]);
        manager
            .window_states
            .get_mut(&Window::FileNamePicker)
            .unwrap()
            .pane_size = PaneSize::Full;
        manager
            .window_states
            .get_mut(&Window::ThemePicker)
            .unwrap()
            .pane_size = PaneSize::Half;
        manager
            .window_states
            .get_mut(&Window::Terminal(0))
            .unwrap()
            .pane_size = PaneSize::Full;
        let slots = manager.layout(size(200, 24));
        assert_eq!(slots.len(), 3);
        // First column: full height.
        assert_eq!(
            slots[0].box_.style_adjusted_bounds_size.row_height.as_u16(),
            24
        );
        // Second column top: half height.
        assert_eq!(
            slots[1].box_.style_adjusted_bounds_size.row_height.as_u16(),
            12
        );
        // Second column bottom: shrunk to remaining 12 rows.
        assert_eq!(
            slots[2].box_.style_adjusted_bounds_size.row_height.as_u16(),
            12
        );
        assert_eq!(
            slots[2].box_.style_adjusted_origin_pos.row_index.as_u16(),
            12
        );
    }

    #[test]
    fn four_quarters_fill_column() {
        let mut manager = PaneManager::new();
        for i in 0..4 {
            manager.push_window(Window::Terminal(i));
            manager
                .window_states
                .get_mut(&Window::Terminal(i))
                .unwrap()
                .pane_size = PaneSize::Quarter;
        }
        let slots = manager.layout(size(80, 24));
        assert_eq!(slots.len(), 4);
        for slot in &slots {
            assert_eq!(slot.box_.style_adjusted_bounds_size.row_height.as_u16(), 6);
            assert_eq!(slot.box_.style_adjusted_origin_pos.col_index.as_u16(), 0);
        }
    }

    #[test]
    fn quarter_and_half_leftover_absorbed_into_last_pane() {
        let mut manager = make_manager(&[Window::FileNamePicker, Window::ThemePicker]);
        manager
            .window_states
            .get_mut(&Window::FileNamePicker)
            .unwrap()
            .pane_size = PaneSize::Quarter;
        manager
            .window_states
            .get_mut(&Window::ThemePicker)
            .unwrap()
            .pane_size = PaneSize::Half;
        let slots = manager.layout(size(80, 24));
        assert_eq!(slots.len(), 2);
        assert_eq!(
            slots[0].box_.style_adjusted_bounds_size.row_height.as_u16(),
            6
        );
        // 25% leftover (6 rows) absorbed into the last pane.
        assert_eq!(
            slots[1].box_.style_adjusted_bounds_size.row_height.as_u16(),
            18
        );
    }

    #[test]
    fn meaningful_leftover_stays_in_same_column() {
        let mut manager = make_manager(&[
            Window::FileNamePicker,
            Window::ThemePicker,
            Window::Terminal(0),
            Window::Terminal(1),
        ]);
        for state in manager.window_states.values_mut() {
            state.pane_size = PaneSize::Quarter;
        }
        // 4 × 25% on a 2-column surface: all 4 panes fit in column 0 at
        // 3 rows each, leaving column 1 empty.
        let slots = manager.layout(size(200, 12));
        assert_eq!(slots.len(), 4);
        for slot in &slots {
            assert_eq!(slot.box_.style_adjusted_bounds_size.row_height.as_u16(), 3);
            assert_eq!(slot.box_.style_adjusted_origin_pos.col_index.as_u16(), 0);
        }
    }

    #[test]
    fn leftover_always_absorbed_into_last_pane() {
        let mut manager = make_manager(&[
            Window::FileNamePicker,
            Window::ThemePicker,
            Window::Terminal(0),
        ]);
        manager
            .window_states
            .get_mut(&Window::FileNamePicker)
            .unwrap()
            .pane_size = PaneSize::Quarter;
        manager
            .window_states
            .get_mut(&Window::ThemePicker)
            .unwrap()
            .pane_size = PaneSize::Quarter;
        manager
            .window_states
            .get_mut(&Window::Terminal(0))
            .unwrap()
            .pane_size = PaneSize::Half;
        // 4% leftover (2 rows) on 47-row surface — absorbed into last pane.
        let slots = manager.layout(size(80, 47));
        assert_eq!(slots.len(), 3);
        assert_eq!(
            slots[0].box_.style_adjusted_bounds_size.row_height.as_u16(),
            11
        );
        assert_eq!(
            slots[1].box_.style_adjusted_bounds_size.row_height.as_u16(),
            11
        );
        assert_eq!(
            slots[2].box_.style_adjusted_bounds_size.row_height.as_u16(),
            25
        );
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
            vec![
                Window::ThemePicker,
                Window::FileNamePicker,
                Window::Terminal(0)
            ]
        );
        manager.move_forward(&Window::FileNamePicker);
        assert_eq!(
            manager.window_stack,
            vec![
                Window::FileNamePicker,
                Window::ThemePicker,
                Window::Terminal(0)
            ]
        );
    }

    fn set_all_pane_sizes(manager: &mut PaneManager, pane_size: PaneSize) {
        for state in manager.window_states.values_mut() {
            state.pane_size = pane_size;
        }
    }

    #[test]
    fn cycle_focus_forward_wraps() {
        let mut manager = make_manager(&[
            Window::FileNamePicker,
            Window::ThemePicker,
            Window::Terminal(0),
        ]);
        set_all_pane_sizes(&mut manager, PaneSize::Quarter);
        manager.focused_window = Some(Window::Terminal(0));
        let visible = manager.layout(size(80, 24));
        manager.cycle_focus(&visible, 1);
        assert_eq!(manager.focused_window, Some(Window::FileNamePicker));
    }

    #[test]
    fn cycle_focus_backward_wraps() {
        let mut manager = make_manager(&[
            Window::FileNamePicker,
            Window::ThemePicker,
            Window::Terminal(0),
        ]);
        set_all_pane_sizes(&mut manager, PaneSize::Quarter);
        manager.focused_window = Some(Window::FileNamePicker);
        let visible = manager.layout(size(80, 24));
        manager.cycle_focus(&visible, -1);
        assert_eq!(manager.focused_window, Some(Window::Terminal(0)));
    }

    #[test]
    fn cycle_focus_empty_visible_is_noop() {
        let mut manager = make_manager(&[Window::FileNamePicker]);
        manager.cycle_focus(&[], 1);
        assert_eq!(manager.focused_window, Some(Window::FileNamePicker));
    }

    #[test]
    fn cycle_focus_wraps_to_second_when_focused_not_visible() {
        let mut manager = make_manager(&[
            Window::FileNamePicker,
            Window::ThemePicker,
            Window::Terminal(0),
        ]);
        set_all_pane_sizes(&mut manager, PaneSize::Quarter);
        manager.focused_window = Some(Window::Terminal(99));
        let visible = manager.layout(size(80, 24));
        manager.cycle_focus(&visible, 1);
        assert_eq!(manager.focused_window, Some(Window::ThemePicker));
    }

    #[test]
    fn resize_focused_grows_and_shrinks() {
        let mut manager = make_manager(&[Window::FileNamePicker]);
        manager.focused_window = Some(Window::FileNamePicker);
        manager
            .window_states
            .get_mut(&Window::FileNamePicker)
            .unwrap()
            .pane_size = PaneSize::Half;
        manager.resize_focused(ResizeDelta::Shrink);
        manager.resize_focused(ResizeDelta::Shrink);
        assert_eq!(
            manager
                .window_states
                .get(&Window::FileNamePicker)
                .unwrap()
                .pane_size,
            PaneSize::Quarter
        );
        manager.resize_focused(ResizeDelta::Grow);
        manager.resize_focused(ResizeDelta::Grow);
        assert_eq!(
            manager
                .window_states
                .get(&Window::FileNamePicker)
                .unwrap()
                .pane_size,
            PaneSize::Half
        );
    }

    #[test]
    fn resize_focused_noop_when_nothing_focused() {
        let mut manager = PaneManager::new();
        manager.resize_focused(ResizeDelta::Grow);
        assert!(manager.focused_window.is_none());
    }

    #[test]
    fn push_window_moves_existing_to_front() {
        let mut manager = PaneManager::new();
        manager.push_window(Window::Terminal(0));
        manager.push_window(Window::Terminal(1));
        manager.push_window(Window::Terminal(2));
        manager.push_window(Window::Terminal(0));
        assert_eq!(
            manager.window_stack,
            vec![
                Window::Terminal(0),
                Window::Terminal(2),
                Window::Terminal(1)
            ]
        );
    }

    #[test]
    fn remove_window_deletes_state_and_updates_focus() {
        let mut manager = make_manager(&[
            Window::Terminal(0),
            Window::Terminal(1),
            Window::Terminal(2),
        ]);
        manager.focused_window = Some(Window::Terminal(1));
        manager
            .window_states
            .get_mut(&Window::Terminal(1))
            .unwrap()
            .pane_size = PaneSize::Half;
        manager.remove_window(&Window::Terminal(1));
        assert_eq!(
            manager.window_stack,
            vec![Window::Terminal(0), Window::Terminal(2)]
        );
        assert_eq!(manager.focused_window, Some(Window::Terminal(0)));
        assert!(!manager.window_states.contains_key(&Window::Terminal(1)));
    }

    #[test]
    fn send_to_back_moves_window_to_end() {
        let mut manager = make_manager(&[
            Window::Terminal(0),
            Window::Terminal(1),
            Window::Terminal(2),
        ]);
        manager.send_to_back(&Window::Terminal(0));
        assert_eq!(
            manager.window_stack,
            vec![
                Window::Terminal(1),
                Window::Terminal(2),
                Window::Terminal(0)
            ]
        );
    }

    #[test]
    fn send_to_back_refocuses_next_pane() {
        let mut manager = PaneManager::new();
        manager.push_window(Window::Terminal(1));
        manager.push_window(Window::Terminal(2));
        manager.push_window(Window::FilePreview(FileKey::default()));
        // push_window places each new window at the front, so reverse to the
        // desired order before setting focus.
        manager.window_stack = vec![
            Window::Terminal(1),
            Window::Terminal(2),
            Window::FilePreview(FileKey::default()),
        ];
        manager.focused_window = Some(Window::Terminal(2));

        manager.send_to_back(&Window::Terminal(2));

        assert_eq!(
            manager.window_stack,
            vec![
                Window::Terminal(1),
                Window::FilePreview(FileKey::default()),
                Window::Terminal(2)
            ]
        );
        assert_eq!(
            manager.focused_window,
            Some(Window::FilePreview(FileKey::default()))
        );
    }

    #[test]
    fn layout_zero_surface_returns_empty() {
        let manager = make_manager(&[Window::FileNamePicker, Window::ThemePicker]);
        let slots = manager.layout(size(0, 0));
        assert!(slots.is_empty());
    }

    #[test]
    fn layout_narrow_surface_uses_one_column() {
        let mut manager = make_manager(&[Window::FileNamePicker, Window::ThemePicker]);
        set_all_pane_sizes(&mut manager, PaneSize::Half);
        let slots = manager.layout(size(80, 24));
        assert_eq!(slots.len(), 2);
        for slot in &slots {
            assert_eq!(slot.box_.style_adjusted_origin_pos.col_index.as_u16(), 0);
        }
    }

    #[test]
    fn layout_respects_max_panes() {
        let mut manager = PaneManager::new();
        for i in 0..20 {
            manager.push_window(Window::Terminal(i));
            manager
                .window_states
                .get_mut(&Window::Terminal(i))
                .unwrap()
                .pane_size = PaneSize::Quarter;
        }
        let slots = manager.layout(size(400, 64));
        assert_eq!(slots.len(), MAX_PANES);
    }
}
