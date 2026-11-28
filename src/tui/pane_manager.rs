#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::collections::HashMap;

/// Maximum number of panes the layout engine may render at once.
pub const MAX_PANES: usize = 16;

/// Rows one wheel notch scrolls.
pub const WHEEL_STEP: usize = 3;

/// A pane that can appear in the window stack.
///
/// Each variant is unique: there is at most one `ConversationTree`, and at most one
/// `FilePreview`/`CommitDiff` per key (file/commit id) in the stack at any time. The pickers
/// are not here: they are modals, painted over the panes and taking every event while open.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Window {
    /// Preview of the file identified by `FileKey`.
    FilePreview(crate::loader::FileKey),
    /// Embedded terminal pane identified by its session id.
    Terminal(usize),
    /// Git log pane listing first-parent history of HEAD or a chosen ref.
    /// Identified by a unique id; `<leader>g` opens additional instances.
    GitLog(usize),
    /// Unified diff of a single commit, identified by its object id.
    CommitDiff([u8; 20]),
    /// Tree of every agent conversation of the repository.
    ConversationTree,
    /// One conversation node: its prompt and reply. Identified by a window id.
    Discussion(usize),
    /// Markdown to read and nothing else: not part of the conversation, never saved.
    Report(usize),
}

/// Relative height of a pane within the available vertical space.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
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
    /// Number of columns of the pane at its last paint.
    pub page_width: usize,
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
    /// Stack of open windows, most-recently-opened first at the front of the view.
    pub window_stack: Vec<Window>,
    /// The window that currently receives keyboard input.
    pub focused_window: Option<Window>,
    /// Per-window scroll, page-size, and pane-size state.
    pub window_states: HashMap<Window, WindowState>,
    /// Index in `window_stack` of the window at the top of the leftmost visible column: the
    /// view scrolls over the stack by columns, as a scrolling tiling window manager does.
    /// Zero means the view shows the front of the stack.
    view_start: usize,
    /// Rows and columns of the surface the last `relayout` was asked for, so focus can be
    /// moved to a column before the next layout runs.
    surface_rows: u16,
    surface_cols: u16,
    /// The panes of the last `relayout`, empty until the first one.
    visible: Vec<PaneSlot>,
}

impl PaneManager {
    /// Creates a new, empty pane manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Moves `window` to the front of the view. If it is not present, inserts it there.
    ///
    /// The view holds still: the window comes to `view_start`, the top of the leftmost
    /// visible column, so picking a window never scrolls the stack out from under you.
    /// With the view at the front this is the front of the stack, as it always was.
    pub fn push_window(&mut self, window: Window) {
        if let Some(pos) = self.window_stack.iter().position(|w| w == &window) {
            self.window_stack.remove(pos);
            if pos < self.view_start {
                self.view_start = self.view_start.saturating_sub(1);
            }
        }
        let at = self.view_start.min(self.window_stack.len());
        self.window_stack.insert(at, window);
        self.window_states.entry(window).or_default();
    }

    /// The index in the stack of the top of the leftmost visible column, which the session
    /// saves so the view comes back where it was.
    pub fn view_start(&self) -> usize {
        self.view_start
    }

    /// Puts the view at or before `start`, in the column that window falls in: the surface it
    /// was saved on may no longer pack the stack the same way.
    pub fn restore_view_start(&mut self, start: usize) {
        self.view_start = start;
        self.clamp_view_start();
    }

    /// Removes `window` from the stack entirely. Focus falls on the window that takes its
    /// place rather than on the front of the stack: the front can be scrolled out of view.
    pub fn remove_window(&mut self, window: &Window) {
        let Some(pos) = self.window_stack.iter().position(|w| w == window) else {
            return;
        };
        self.window_stack.remove(pos);
        self.window_states.remove(window);
        if pos < self.view_start {
            self.view_start = self.view_start.saturating_sub(1);
        }
        if self.focused_window.as_ref() == Some(window) {
            self.focused_window = self
                .window_stack
                .get(pos)
                .or_else(|| pos.checked_sub(1).and_then(|p| self.window_stack.get(p)))
                .copied();
        }
        self.clamp_view_start();
    }

    /// Moves `window` to the back of the stack (last position).
    pub fn send_to_back(&mut self, window: &Window) {
        let Some(pos) = self.window_stack.iter().position(|w| w == window) else {
            return;
        };
        let w = self.window_stack.remove(pos);
        self.window_stack.push(w);
        if pos < self.view_start {
            self.view_start = self.view_start.saturating_sub(1);
        }
        if self.focused_window.as_ref() == Some(window) {
            // Focus the pane that took the sent pane's place.
            self.focused_window = self
                .window_stack
                .get(pos)
                .filter(|&&w| w != *window)
                .or_else(|| pos.checked_sub(1).and_then(|p| self.window_stack.get(p)))
                .copied();
        }
        self.clamp_view_start();
    }

    /// Replaces `old` with `new` at the same position in the stack, preserving window state
    /// and focus. Does nothing if `old` is not in the stack.
    ///
    /// A terminal closing gives back the preview it replaced, and that preview can be open
    /// elsewhere in the stack: it moves into `old`'s slot and keeps its own state, where
    /// `push_window` would have brought it to the front of the view instead.
    pub fn replace_window(&mut self, old: &Window, new: Window) {
        let Some(pos) = self.window_stack.iter().position(|w| w == old) else {
            return;
        };
        if old == &new {
            return;
        }
        let pos = match self.window_stack.iter().position(|w| w == &new) {
            Some(elsewhere) => {
                self.window_stack.remove(elsewhere);
                if elsewhere < self.view_start {
                    self.view_start = self.view_start.saturating_sub(1);
                }
                if elsewhere < pos { pos - 1 } else { pos }
            }
            None => pos,
        };
        self.window_stack[pos] = new;
        let moved = self.window_states.remove(old);
        match (moved, self.window_states.contains_key(&new)) {
            (Some(_), true) => {}
            (Some(state), false) => {
                self.window_states.insert(new, state);
            }
            (None, false) => {
                self.window_states.entry(new).or_default();
            }
            (None, true) => {}
        }
        if self.focused_window.as_ref() == Some(old) {
            self.focused_window = Some(new);
        }
        self.clamp_view_start();
    }

    /// Swaps `window` toward index 0 (the front / left).
    pub fn move_forward(&mut self, window: &Window) {
        let pos = match self.window_stack.iter().position(|w| w == window) {
            Some(0) | None => return,
            Some(p) => p,
        };
        self.window_stack.swap(pos, pos - 1);
        self.clamp_view_start();
    }

    /// Swaps `window` toward the end (the back / right).
    pub fn move_backward(&mut self, window: &Window) {
        let pos = match self.window_stack.iter().position(|w| w == window) {
            Some(p) if p + 1 < self.window_stack.len() => p,
            _ => return,
        };
        self.window_stack.swap(pos, pos + 1);
        self.clamp_view_start();
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

    /// The panes laid out last, empty until the first `relayout`.
    pub fn visible(&self) -> &[PaneSlot] {
        &self.visible
    }

    /// The window in pane `slot`. A pane slot is `view_start` windows into the stack, so this
    /// reads the stack and not the last layout: a pane can name its window before any frame.
    pub fn slot_window(&self, slot: usize) -> Option<Window> {
        let pos = self.view_start.checked_add(slot)?;
        self.window_stack.get(pos).copied()
    }

    /// The box pane `slot` is drawn in, from the last `relayout`: only a frame asks.
    pub fn slot_box(&self, slot: usize) -> Option<&FlexBox> {
        self.visible.get(slot).map(|slot| &slot.box_)
    }

    /// Lays the panes out for `surface_size` and keeps the answer for the frame and the
    /// events that follow: `slot_box` reads it.
    ///
    /// The view is revealed first: a focused window that fell off it, because the surface
    /// shrank or its pane was moved past the edge, scrolls back onto it before boxes exist.
    /// Should the revealed column still not fit (`MAX_PANES`, near the end of a long stack),
    /// the view moves to the nearest column that does draw focus: the rule this replaced moved
    /// focus into the frame, and this keeps the frame and moves the view.
    pub fn relayout(&mut self, surface_size: VPSize) {
        self.surface_rows = surface_size.row_height.as_u16();
        self.surface_cols = surface_size.col_width.as_u16();
        self.clamp_view_start();
        let focused = self
            .focused_window
            .and_then(|focused| self.window_stack.iter().position(|w| *w == focused));
        if let Some(pos) = focused {
            self.reveal(pos);
        }
        let mut slots = self.layout(surface_size);
        if let Some(pos) = focused
            && !slots
                .iter()
                .any(|slot| Some(slot.window) == self.focused_window)
        {
            let fitting = self
                .columns()
                .into_iter()
                .rev()
                .filter(|(start, _)| *start <= pos)
                .find(|(start, _)| {
                    self.drawn_range(*start)
                        .is_some_and(|(first, end)| first <= pos && pos < end)
                })
                .map(|(start, _)| start);
            if let Some(start) = fitting {
                self.view_start = start;
                slots = self.layout(surface_size);
            }
        }
        self.visible = slots;
    }

    /// The slot of the focused window, when the view shows it.
    pub fn focused_slot(&self) -> Option<usize> {
        let focused = self.focused_window.as_ref()?;
        let pos = self.window_stack.iter().position(|w| w == focused)?;
        let slot = pos.checked_sub(self.view_start)?;
        (slot < self.visible.len()).then_some(slot)
    }

    /// Moves focus one window along the whole stack, on screen or not, and scrolls the view
    /// by as many whole columns as it takes to show where focus landed. A move past either end
    /// of the stack does nothing.
    pub fn focus_along(&mut self, direction: i32) {
        let len = self.window_stack.len();
        if len == 0 {
            return;
        }
        let current = self
            .focused_window
            .and_then(|f| self.window_stack.iter().position(|w| *w == f))
            .unwrap_or(self.view_start);
        let next = match current.checked_add_signed(direction as isize) {
            Some(next) if next < len => next,
            _ => return,
        };
        self.focused_window = Some(self.window_stack[next]);
        self.reveal(next);
    }

    /// Keeps `view_start` a window at the top of a column: the stack's packing changes with
    /// the surface, so a resize can leave it mid-column, and the view is placed by columns.
    fn clamp_view_start(&mut self) {
        self.view_start = match self.window_stack.len() {
            0 => return,
            len => self.view_start.min(len - 1),
        };
        let columns = self.columns();
        let aligned = columns
            .iter()
            .rposition(|(start, _)| *start <= self.view_start)
            .map(|index| columns[index].0)
            .unwrap_or(0);
        self.view_start = aligned;
    }

    /// Scrolls the view so the window at `pos` is on screen, if it is not already.
    ///
    /// A window past the right edge brings its column to the right edge and a window past the
    /// left one puts its column at the left edge: the view slides by whole columns rather
    /// than jumping.
    fn reveal(&mut self, pos: usize) {
        let columns = self.columns();
        let Some(view) = columns
            .iter()
            .position(|(start, _)| *start == self.view_start)
        else {
            return;
        };
        let Some(target) = columns.iter().rposition(|(start, _)| *start <= pos) else {
            return;
        };
        let visible = self.visible_columns();
        if target >= view && target < view + visible {
            return;
        }
        if target > view {
            let first = (target + 1).saturating_sub(visible).max(view);
            let last = columns.len().saturating_sub(visible);
            self.view_start = columns[first.min(last).min(target)].0;
        } else {
            self.view_start = columns[target].0;
        }
    }

    /// How many columns the view shows at once: the surface's column count, minus any column
    /// `MAX_PANES` leaves out of the frame. Counted from `view_start`, exactly as `layout`
    /// counts it, so where `reveal` puts the view is always a column it draws.
    fn visible_columns(&self) -> usize {
        let cols = self.surface_column_count();
        let mut shown = 0;
        let mut panes = 0;
        for (_, count) in self
            .columns()
            .into_iter()
            .filter(|(start, _)| *start >= self.view_start)
            .take(cols)
        {
            if shown > 0 && panes + count > MAX_PANES {
                break;
            }
            panes += count;
            shown += 1;
        }
        shown.max(1)
    }

    /// The stack positions a view at `start` draws, from `start` to one past its last pane,
    /// under the surface's columns and `MAX_PANES`: `None` when it draws nothing.
    fn drawn_range(&self, start: usize) -> Option<(usize, usize)> {
        let cols = self.surface_column_count();
        let mut panes = 0;
        let mut drawn = 0;
        let mut end = start;
        for (column_start, count) in self
            .columns()
            .into_iter()
            .filter(|(column_start, _)| *column_start >= start)
        {
            if drawn == cols || (drawn > 0 && panes + count > MAX_PANES) {
                break;
            }
            panes += count;
            drawn += 1;
            end = column_start + count;
        }
        (drawn > 0).then_some((start, end))
    }

    /// The columns the surface is wide enough for: its width over `TARGET_PANE_WIDTH`,
    /// rounded to the nearest.
    fn surface_column_count(&self) -> usize {
        const TARGET_PANE_WIDTH: u16 = 100;
        ((u32::from(self.surface_cols) + u32::from(TARGET_PANE_WIDTH) / 2)
            / u32::from(TARGET_PANE_WIDTH))
        .max(1) as usize
    }

    /// Every column of the whole stack, as its first window and how many panes it holds.
    fn columns(&self) -> Vec<(usize, usize)> {
        let mut columns = Vec::new();
        let mut start = 0;
        while start < self.window_stack.len() {
            // A column always advances: a zero-length one would loop here forever.
            let count = self.column_at(start, self.surface_rows).len().max(1);
            columns.push((start, count));
            start += count;
        }
        columns
    }

    /// The rows `window` asks for on a surface of `surface_rows`.
    fn requested_rows(&self, window: &Window, surface_rows: u16) -> u16 {
        let pane_size = self
            .window_states
            .get(window)
            .map(|s| s.pane_size)
            .unwrap_or_default();
        ((surface_rows as f32 * pane_size.height_factor()) as u16).max(1)
    }

    /// The panes of the column that starts at `start`, with the rows each one gets: stacked
    /// top to bottom by their `PaneSize` until the next one would leave a sliver of less than
    /// a quarter, the last one absorbing the leftover.
    ///
    /// A column always holds at least one pane, even on a surface of no rows yet (before the
    /// first frame): `columns` walks the stack by column length, so an empty one would stall.
    fn column_at(&self, start: usize, surface_rows: u16) -> Vec<(Window, u16)> {
        let mut panes: Vec<(Window, u16)> = Vec::new();
        let mut used_rows: u16 = 0;
        for window in self.window_stack.get(start..).unwrap_or_default() {
            let requested_rows = self.requested_rows(window, surface_rows);
            let remaining_rows = surface_rows.saturating_sub(used_rows);
            if !panes.is_empty()
                && (remaining_rows == 0
                    || (requested_rows > remaining_rows
                        && remaining_rows * 100 / surface_rows.max(1) < 25))
            {
                break;
            }
            let actual_rows = requested_rows.min(remaining_rows);
            panes.push((*window, actual_rows));
            used_rows += actual_rows;
        }
        if let Some(last) = panes.last_mut() {
            last.1 = last
                .1
                .saturating_add(surface_rows.saturating_sub(used_rows));
        }
        panes
    }

    /// Lays the visible panes out into columns and rows, from `view_start` on.
    ///
    /// Column count is `surface_size.col_width / TARGET_PANE_WIDTH` rounded to the nearest
    /// whole number, so the target is the ideal pane width, not a minimum. The width is then
    /// split evenly between the columns.
    /// Within each column, panes are stacked top-to-bottom using their requested
    /// `PaneSize`. The last pane in each column is stretched to fill any remaining
    /// vertical space in that column.
    fn layout(&self, surface_size: VPSize) -> Vec<PaneSlot> {
        const TARGET_PANE_WIDTH: u16 = 100;

        let surface_cols = surface_size.col_width.as_u16();
        let surface_rows = surface_size.row_height.as_u16();
        if surface_rows == 0 || surface_cols == 0 {
            return Vec::new();
        }

        let cols = ((surface_cols + TARGET_PANE_WIDTH / 2) / TARGET_PANE_WIDTH).max(1) as usize;
        let base_col_width = surface_cols / cols as u16;
        let remainder = surface_cols % cols as u16;
        // The first `remainder` columns are one cell wider.
        let col_origin = |col: usize| {
            let col = col as u16;
            col * base_col_width + col.min(remainder)
        };

        let mut slots: Vec<PaneSlot> = Vec::with_capacity(MAX_PANES);
        let mut start = self.view_start.min(self.window_stack.len());
        let mut col: usize = 0;
        while start < self.window_stack.len() && col < cols {
            let column = self.column_at(start, surface_rows);
            debug_assert!(!column.is_empty(), "a column always holds a pane");
            // A column is rendered whole or not at all: a column cut at `MAX_PANES` would leave
            // its bottom panes on screen with nothing drawn in their box, and a window Tab
            // reached there could never be shown.
            if !slots.is_empty() && slots.len() + column.len() > MAX_PANES {
                break;
            }
            let mut used_rows: u16 = 0;
            for (window, rows) in &column {
                let origin = vp_col(col_origin(col)) + vp_row(used_rows);
                let size = vp_width(col_origin(col + 1) - col_origin(col)) + vp_height(*rows);
                slots.push(PaneSlot {
                    slot: slots.len(),
                    window: *window,
                    box_: FlexBox {
                        style_adjusted_bounds: VPBoundingBox::new(origin, size),
                        ..FlexBox::default()
                    },
                });
                used_rows += rows;
            }
            start += column.len();
            col += 1;
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

    /// Number of columns of `window` at its last paint.
    pub fn window_page_width(&self, window: &Window) -> usize {
        self.window_states
            .get(window)
            .map(|s| s.page_width)
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

    /// Sets the number of columns of `window` at its last paint.
    pub fn set_window_page_width(&mut self, window: &Window, page_width: usize) {
        self.window_states.entry(*window).or_default().page_width = page_width;
    }

    /// Sets the total content height for `window`.
    pub fn set_window_scroll_max(&mut self, window: &Window, scroll_max: usize) {
        self.window_states.entry(*window).or_default().scroll_max = scroll_max;
    }

    /// Puts the view at the column that starts at `start`, as a run of Tabs would. The
    /// surface must be known, as it is after a first layout.
    #[cfg(test)]
    pub fn set_view_start(&mut self, start: usize) {
        self.restore_view_start(start);
    }

    /// Sets the scroll offset, total content height and rendered page size of `window`
    /// at once: what the last paint showed, for the scrollbar and the paging keys.
    #[cfg(test)]
    pub fn set_window_view(
        &mut self,
        window: &Window,
        scroll: usize,
        scroll_max: usize,
        page_size: usize,
    ) {
        let view = self.window_states.entry(*window).or_default();
        view.scroll = scroll;
        view.scroll_max = scroll_max;
        view.page_size = page_size;
    }

    /// Scrolls `window` for a key or wheel event and answers whether the event was one:
    /// Up/Down and Shift+Up/Down by a row, PageUp/PageDown by a page, Home/End and Ctrl+A/Ctrl+E to the
    /// ends, the wheel by `WHEEL_STEP`.
    pub fn scroll_for_input(&mut self, window: &Window, input_event: &InputEvent) -> bool {
        let current = self.window_scroll(window);
        let page = self.window_page_size(window).max(1);
        let target = match input_event {
            InputEvent::Keyboard(KeyPress::Plain { key }) => match key {
                Key::SpecialKey(SpecialKey::Up) => current.saturating_sub(1),
                Key::SpecialKey(SpecialKey::Down) => current.saturating_add(1),
                Key::SpecialKey(SpecialKey::PageUp) => current.saturating_sub(page),
                Key::SpecialKey(SpecialKey::PageDown) => current.saturating_add(page),
                Key::SpecialKey(SpecialKey::Home) => 0,
                Key::SpecialKey(SpecialKey::End) => usize::MAX,
                _ => return false,
            },
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(SpecialKey::Up),
                mask:
                    ModifierKeysMask {
                        shift_key_state: KeyState::Pressed,
                        ctrl_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => current.saturating_sub(1),
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(SpecialKey::Down),
                mask:
                    ModifierKeysMask {
                        shift_key_state: KeyState::Pressed,
                        ctrl_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => current.saturating_add(1),
            InputEvent::Keyboard(KeyPress::WithModifiers { key, mask })
                if *mask == ModifierKeysMask::new().with_ctrl() =>
            {
                match key {
                    Key::Character('a') => 0,
                    Key::Character('e') => usize::MAX,
                    _ => return false,
                }
            }
            InputEvent::Mouse(mouse) => match mouse.kind {
                MouseInputKind::ScrollUp => current.saturating_sub(WHEEL_STEP),
                MouseInputKind::ScrollDown => current.saturating_add(WHEEL_STEP),
                _ => return false,
            },
            _ => return false,
        };
        self.set_window_scroll(window, target);
        self.clamp_scroll(window);
        true
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

    /// Whether `window`'s scroll offset sits where `clamp_scroll` settles it.
    pub fn window_at_scroll_max(&self, window: &Window) -> bool {
        self.window_states
            .get(window)
            .is_some_and(|s| s.scroll >= s.scroll_max.saturating_sub(s.page_size))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manager(stack: &[Window]) -> PaneManager {
        let mut manager = PaneManager::new();
        manager.window_stack = stack.to_vec();
        for window in stack {
            manager.window_states.entry(*window).or_default();
        }
        if let Some(first) = manager.window_stack.first() {
            manager.focused_window = Some(*first);
        }
        manager
    }

    fn size(cols: u16, rows: u16) -> VPSize {
        vp_width(cols) + vp_height(rows)
    }

    #[test]
    fn scroll_for_input_moves_by_row_page_and_end_and_clamps() {
        use crate::tui::test_support::key;
        let window = Window::GitLog(1);
        let mut manager = make_manager(&[window]);
        manager.set_window_page_size(&window, 10);
        manager.set_window_scroll_max(&window, 100);
        let wheel = |kind| {
            InputEvent::Mouse(MouseInput {
                pos: VPPos::default(),
                kind,
                maybe_modifier_keys: None,
            })
        };
        assert!(manager.scroll_for_input(&window, &key(Key::SpecialKey(SpecialKey::Down))));
        assert_eq!(manager.window_scroll(&window), 1);
        assert!(manager.scroll_for_input(&window, &key(Key::SpecialKey(SpecialKey::PageDown))));
        assert_eq!(manager.window_scroll(&window), 11);
        assert!(manager.scroll_for_input(&window, &wheel(MouseInputKind::ScrollUp)));
        assert_eq!(manager.window_scroll(&window), 11 - WHEEL_STEP);
        assert!(manager.scroll_for_input(&window, &key(Key::SpecialKey(SpecialKey::End))));
        assert_eq!(manager.window_scroll(&window), 90);
        assert!(manager.scroll_for_input(&window, &key(Key::SpecialKey(SpecialKey::Down))));
        assert_eq!(manager.window_scroll(&window), 90);
        let ctrl_a = InputEvent::Keyboard(KeyPress::WithModifiers {
            key: Key::Character('a'),
            mask: ModifierKeysMask::new().with_ctrl(),
        });
        assert!(manager.scroll_for_input(&window, &ctrl_a));
        assert_eq!(manager.window_scroll(&window), 0);
        assert!(!manager.scroll_for_input(&window, &key(Key::Character('x'))));
        let shift = |key| {
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(key),
                mask: ModifierKeysMask::new().with_shift(),
            })
        };
        assert!(manager.scroll_for_input(&window, &shift(SpecialKey::Down)));
        assert_eq!(manager.window_scroll(&window), 1);
        assert!(manager.scroll_for_input(&window, &shift(SpecialKey::Up)));
        assert!(manager.scroll_for_input(&window, &shift(SpecialKey::Up)));
        assert_eq!(manager.window_scroll(&window), 0);
        assert!(!manager.scroll_for_input(&window, &shift(SpecialKey::Left)));
        let combo = |mask| {
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(SpecialKey::Down),
                mask,
            })
        };
        assert!(!manager.scroll_for_input(
            &window,
            &combo(ModifierKeysMask::new().with_shift().with_ctrl())
        ));
        assert!(!manager.scroll_for_input(
            &window,
            &combo(ModifierKeysMask::new().with_shift().with_alt())
        ));
        assert!(!manager.scroll_for_input(&window, &combo(ModifierKeysMask::new().with_alt())));
        assert_eq!(manager.window_scroll(&window), 0);
    }

    #[test]
    fn single_full_pane_uses_full_height() {
        let manager = make_manager(&[Window::ConversationTree]);
        let slots = manager.layout(size(80, 24));
        assert_eq!(slots.len(), 1);
        assert_eq!(
            slots[0]
                .box_
                .style_adjusted_bounds
                .bounds_size
                .row_height
                .as_u16(),
            24
        );
    }

    #[test]
    fn two_columns_for_wide_surface() {
        let manager = make_manager(&[Window::ConversationTree, Window::GitLog(1)]);
        let slots = manager.layout(size(240, 24));
        assert_eq!(slots.len(), 2);
        assert_eq!(
            slots[0]
                .box_
                .style_adjusted_bounds
                .origin_pos
                .col_index
                .as_u16(),
            0
        );
        assert_eq!(
            slots[1]
                .box_
                .style_adjusted_bounds
                .origin_pos
                .col_index
                .as_u16(),
            120
        );
    }

    #[test]
    fn half_panes_stack_in_one_column() {
        let mut manager = make_manager(&[Window::ConversationTree, Window::GitLog(1)]);
        for state in manager.window_states.values_mut() {
            state.pane_size = PaneSize::Half;
        }
        let slots = manager.layout(size(80, 24));
        assert_eq!(slots.len(), 2);
        assert_eq!(
            slots[0]
                .box_
                .style_adjusted_bounds
                .bounds_size
                .row_height
                .as_u16(),
            12
        );
        assert_eq!(
            slots[1]
                .box_
                .style_adjusted_bounds
                .bounds_size
                .row_height
                .as_u16(),
            12
        );
    }

    #[test]
    fn last_full_pane_shrinks_to_fill_remaining_space() {
        let mut manager = make_manager(&[
            Window::ConversationTree,
            Window::GitLog(1),
            Window::Terminal(0),
        ]);
        manager
            .window_states
            .get_mut(&Window::ConversationTree)
            .unwrap()
            .pane_size = PaneSize::Full;
        manager
            .window_states
            .get_mut(&Window::GitLog(1))
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
            slots[0]
                .box_
                .style_adjusted_bounds
                .bounds_size
                .row_height
                .as_u16(),
            24
        );
        // Second column top: half height.
        assert_eq!(
            slots[1]
                .box_
                .style_adjusted_bounds
                .bounds_size
                .row_height
                .as_u16(),
            12
        );
        // Second column bottom: shrunk to remaining 12 rows.
        assert_eq!(
            slots[2]
                .box_
                .style_adjusted_bounds
                .bounds_size
                .row_height
                .as_u16(),
            12
        );
        assert_eq!(
            slots[2]
                .box_
                .style_adjusted_bounds
                .origin_pos
                .row_index
                .as_u16(),
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
            assert_eq!(
                slot.box_
                    .style_adjusted_bounds
                    .bounds_size
                    .row_height
                    .as_u16(),
                6
            );
            assert_eq!(
                slot.box_
                    .style_adjusted_bounds
                    .origin_pos
                    .col_index
                    .as_u16(),
                0
            );
        }
    }

    #[test]
    fn quarter_and_half_leftover_absorbed_into_last_pane() {
        let mut manager = make_manager(&[Window::ConversationTree, Window::GitLog(1)]);
        manager
            .window_states
            .get_mut(&Window::ConversationTree)
            .unwrap()
            .pane_size = PaneSize::Quarter;
        manager
            .window_states
            .get_mut(&Window::GitLog(1))
            .unwrap()
            .pane_size = PaneSize::Half;
        let slots = manager.layout(size(80, 24));
        assert_eq!(slots.len(), 2);
        assert_eq!(
            slots[0]
                .box_
                .style_adjusted_bounds
                .bounds_size
                .row_height
                .as_u16(),
            6
        );
        // 25% leftover (6 rows) absorbed into the last pane.
        assert_eq!(
            slots[1]
                .box_
                .style_adjusted_bounds
                .bounds_size
                .row_height
                .as_u16(),
            18
        );
    }

    #[test]
    fn meaningful_leftover_stays_in_same_column() {
        let mut manager = make_manager(&[
            Window::ConversationTree,
            Window::GitLog(1),
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
            assert_eq!(
                slot.box_
                    .style_adjusted_bounds
                    .bounds_size
                    .row_height
                    .as_u16(),
                3
            );
            assert_eq!(
                slot.box_
                    .style_adjusted_bounds
                    .origin_pos
                    .col_index
                    .as_u16(),
                0
            );
        }
    }

    #[test]
    fn leftover_always_absorbed_into_last_pane() {
        let mut manager = make_manager(&[
            Window::ConversationTree,
            Window::GitLog(1),
            Window::Terminal(0),
        ]);
        manager
            .window_states
            .get_mut(&Window::ConversationTree)
            .unwrap()
            .pane_size = PaneSize::Quarter;
        manager
            .window_states
            .get_mut(&Window::GitLog(1))
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
            slots[0]
                .box_
                .style_adjusted_bounds
                .bounds_size
                .row_height
                .as_u16(),
            11
        );
        assert_eq!(
            slots[1]
                .box_
                .style_adjusted_bounds
                .bounds_size
                .row_height
                .as_u16(),
            11
        );
        assert_eq!(
            slots[2]
                .box_
                .style_adjusted_bounds
                .bounds_size
                .row_height
                .as_u16(),
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

    /// Swapping two panes re-packs the columns around them, which can take the window the
    /// view starts at off a column start: the view is snapped back onto the one it falls in.
    #[test]
    fn moving_a_pane_keeps_the_view_on_a_column_start() {
        let mut manager = make_manager(&[
            Window::Terminal(0),
            Window::Terminal(1),
            Window::Terminal(2),
            Window::Terminal(3),
        ]);
        for (id, pane_size) in [
            PaneSize::Full,
            PaneSize::Full,
            PaneSize::Full,
            PaneSize::Half,
        ]
        .into_iter()
        .enumerate()
        {
            manager
                .window_states
                .get_mut(&Window::Terminal(id))
                .unwrap()
                .pane_size = pane_size;
        }
        // Five rows, one full pane a column: the half pane asks for two rows and starts a
        // fourth column with two rows left over.
        manager.focused_window = Some(Window::Terminal(3));
        manager.relayout(size(200, 5));
        assert_eq!(manager.columns(), [(0, 1), (1, 1), (2, 1), (3, 1)]);

        // The half pane moves left of the last full one; the two now share a column, so
        // there is no column left at 3 and a view starting there no longer starts one.
        manager.set_view_start(3);
        manager.move_forward(&Window::Terminal(3));

        let columns = manager.columns();
        assert_eq!(
            columns,
            [(0, 1), (1, 1), (2, 2)],
            "the last two share a column"
        );
        assert!(
            columns
                .iter()
                .any(|(start, _)| *start == manager.view_start()),
            "the view starts a column: {} among {columns:?}",
            manager.view_start()
        );
    }

    #[test]
    fn move_forward_and_backward() {
        let mut manager = make_manager(&[
            Window::ConversationTree,
            Window::GitLog(1),
            Window::Terminal(0),
        ]);
        manager.move_backward(&Window::ConversationTree);
        assert_eq!(
            manager.window_stack,
            vec![
                Window::GitLog(1),
                Window::ConversationTree,
                Window::Terminal(0)
            ]
        );
        manager.move_forward(&Window::ConversationTree);
        assert_eq!(
            manager.window_stack,
            vec![
                Window::ConversationTree,
                Window::GitLog(1),
                Window::Terminal(0)
            ]
        );
    }

    fn set_all_pane_sizes(manager: &mut PaneManager, pane_size: PaneSize) {
        for state in manager.window_states.values_mut() {
            state.pane_size = pane_size;
        }
    }

    /// Tab stops at the end of the stack instead of wrapping: a move past either end does
    /// nothing, so the user can feel where the stack ends.
    #[test]
    fn focus_along_stops_at_the_end_of_the_stack() {
        let mut manager = make_manager(&[
            Window::ConversationTree,
            Window::GitLog(1),
            Window::Terminal(0),
        ]);
        set_all_pane_sizes(&mut manager, PaneSize::Quarter);
        manager.relayout(size(80, 24));

        manager.focused_window = Some(Window::Terminal(0));
        manager.focus_along(1);
        assert_eq!(
            manager.focused_window,
            Some(Window::Terminal(0)),
            "the last window has nowhere to go forward"
        );

        manager.focused_window = Some(Window::ConversationTree);
        manager.focus_along(-1);
        assert_eq!(
            manager.focused_window,
            Some(Window::ConversationTree),
            "the first window has nowhere to go back"
        );
        assert_eq!(manager.view_start(), 0, "and the view did not move");
    }

    #[test]
    fn focus_along_on_an_empty_stack_is_a_noop() {
        let mut manager = PaneManager::new();
        manager.focus_along(1);
        assert!(manager.focused_window.is_none());
    }

    #[test]
    fn focus_along_starts_from_the_view_when_nothing_is_focused() {
        let mut manager = make_manager(&[
            Window::ConversationTree,
            Window::GitLog(1),
            Window::Terminal(0),
        ]);
        manager.focused_window = None;
        manager.focus_along(1);
        assert_eq!(manager.focused_window, Some(Window::GitLog(1)));
    }

    /// Four full-height windows on a 200-column surface: two columns in view, one window each,
    /// so Tab walks column by column.
    fn four_columns() -> PaneManager {
        let mut manager = make_manager(&[
            Window::Terminal(0),
            Window::Terminal(1),
            Window::Terminal(2),
            Window::Terminal(3),
        ]);
        set_all_pane_sizes(&mut manager, PaneSize::Full);
        manager.focused_window = Some(Window::Terminal(0));
        manager.relayout(size(200, 24));
        assert_eq!(manager.visible().len(), 2, "the view shows two columns");
        manager
    }

    fn focused_and_visible(manager: &PaneManager) -> (Option<Window>, Vec<Window>) {
        (
            manager.focused_window,
            manager.visible().iter().map(|s| s.window).collect(),
        )
    }

    /// One Tab, then the layout the app runs for the event: what the next frame shows.
    fn tab(manager: &mut PaneManager, direction: i32) {
        manager.focus_along(direction);
        manager.relayout(size(200, 24));
    }

    #[test]
    fn tab_to_a_window_right_of_the_view_scrolls_one_column() {
        let mut manager = four_columns();
        tab(&mut manager, 1);
        assert_eq!(
            focused_and_visible(&manager),
            (
                Some(Window::Terminal(1)),
                vec![Window::Terminal(0), Window::Terminal(1)]
            )
        );

        tab(&mut manager, 1);
        assert_eq!(
            focused_and_visible(&manager),
            (
                Some(Window::Terminal(2)),
                vec![Window::Terminal(1), Window::Terminal(2)]
            ),
            "the second column became the first, the third appeared with the focus"
        );
    }

    #[test]
    fn shift_tab_to_a_window_left_of_the_view_scrolls_one_column() {
        let mut manager = four_columns();
        manager.focused_window = Some(Window::Terminal(3));
        manager.set_view_start(2);
        manager.relayout(size(200, 24));
        assert_eq!(
            focused_and_visible(&manager),
            (
                Some(Window::Terminal(3)),
                vec![Window::Terminal(2), Window::Terminal(3)]
            )
        );

        tab(&mut manager, -1);
        assert_eq!(
            focused_and_visible(&manager),
            (
                Some(Window::Terminal(2)),
                vec![Window::Terminal(2), Window::Terminal(3)]
            )
        );

        tab(&mut manager, -1);
        assert_eq!(
            focused_and_visible(&manager),
            (
                Some(Window::Terminal(1)),
                vec![Window::Terminal(1), Window::Terminal(2)]
            ),
            "the column left of the view became its first"
        );
    }

    #[test]
    fn tab_at_the_last_window_does_nothing() {
        let mut manager = four_columns();
        manager.focused_window = Some(Window::Terminal(3));
        manager.set_view_start(2);
        manager.relayout(size(200, 24));

        tab(&mut manager, 1);

        assert_eq!(
            focused_and_visible(&manager),
            (
                Some(Window::Terminal(3)),
                vec![Window::Terminal(2), Window::Terminal(3)]
            ),
            "the focus and the view stay where they were"
        );
    }

    #[test]
    fn shift_tab_at_the_first_window_does_nothing() {
        let mut manager = four_columns();

        tab(&mut manager, -1);

        assert_eq!(
            focused_and_visible(&manager),
            (
                Some(Window::Terminal(0)),
                vec![Window::Terminal(0), Window::Terminal(1)]
            ),
            "the focus and the view stay where they were"
        );
    }

    /// Tab over windows that share a column moves within it first, then a whole column
    /// scrolls once focus passes the right edge.
    #[test]
    fn tab_within_a_column_does_not_scroll() {
        let mut manager = make_manager(&[
            Window::Terminal(0),
            Window::Terminal(1),
            Window::Terminal(2),
            Window::Terminal(3),
            Window::Terminal(4),
            Window::Terminal(5),
        ]);
        set_all_pane_sizes(&mut manager, PaneSize::Half);
        manager.focused_window = Some(Window::Terminal(0));
        manager.relayout(size(200, 24));
        assert_eq!(
            manager.visible().len(),
            4,
            "three columns of two, the first two in view"
        );

        for (step, expected, view) in [(1, 1, 0), (2, 2, 0), (3, 3, 0), (4, 4, 2)] {
            manager.focus_along(1);
            assert_eq!(
                manager.focused_window,
                Some(Window::Terminal(expected)),
                "step {step}"
            );
            assert_eq!(manager.view_start(), view, "after step {step}");
            manager.relayout(size(200, 24));
        }
    }

    #[test]
    fn tab_scrolls_to_a_last_column_of_one_window() {
        let mut manager = make_manager(&[
            Window::Terminal(0),
            Window::Terminal(1),
            Window::Terminal(2),
            Window::Terminal(3),
            Window::Terminal(4),
            Window::GitLog(1),
        ]);
        set_all_pane_sizes(&mut manager, PaneSize::Full);
        manager.focused_window = Some(Window::Terminal(0));
        manager.relayout(size(200, 24));
        for _ in 0..5 {
            manager.focus_along(1);
            manager.relayout(size(200, 24));
        }
        assert_eq!(
            focused_and_visible(&manager),
            (
                Some(Window::GitLog(1)),
                vec![Window::Terminal(4), Window::GitLog(1)]
            )
        );
        assert_eq!(manager.view_start(), 4);
    }

    /// Tab reaches every window of the stack from the first to the last, the one it lands on
    /// is always on screen, and the last Tab has nowhere left to go.
    #[test]
    fn tab_reaches_every_window_of_the_stack() {
        let mut manager = PaneManager::new();
        for i in 0..14 {
            manager.push_window(Window::Terminal(i));
            manager
                .window_states
                .get_mut(&Window::Terminal(i))
                .unwrap()
                .pane_size = match i % 3 {
                0 => PaneSize::Half,
                1 => PaneSize::Quarter,
                _ => PaneSize::Third,
            };
        }
        let surface = size(200, 24);
        manager.focused_window = manager.window_stack.first().copied();
        manager.relayout(surface);
        let mut seen = vec![manager.focused_window.unwrap()];
        for _ in 0..13 {
            manager.focus_along(1);
            manager.relayout(surface);
            seen.push(manager.focused_window.unwrap());
            assert!(
                manager
                    .visible()
                    .iter()
                    .any(|s| Some(s.window) == manager.focused_window),
                "the focused window is on screen after Tab: {:?}",
                manager.focused_window
            );
        }

        assert_eq!(
            seen.last().copied(),
            manager.window_stack.last().copied(),
            "thirteen Tabs reached the last window"
        );
        let mut unique = seen.clone();
        unique.sort_by_key(|w| match w {
            Window::Terminal(id) => *id,
            _ => 0,
        });
        unique.dedup();
        assert_eq!(unique.len(), 14, "Tab visited every window");

        manager.focus_along(1);
        assert_eq!(
            manager.focused_window,
            manager.window_stack.last().copied(),
            "and the last Tab does nothing"
        );
    }

    #[test]
    fn resize_focused_grows_and_shrinks() {
        let mut manager = make_manager(&[Window::ConversationTree]);
        manager.focused_window = Some(Window::ConversationTree);
        manager
            .window_states
            .get_mut(&Window::ConversationTree)
            .unwrap()
            .pane_size = PaneSize::Half;
        manager.resize_focused(ResizeDelta::Shrink);
        manager.resize_focused(ResizeDelta::Shrink);
        assert_eq!(
            manager
                .window_states
                .get(&Window::ConversationTree)
                .unwrap()
                .pane_size,
            PaneSize::Quarter
        );
        manager.resize_focused(ResizeDelta::Grow);
        manager.resize_focused(ResizeDelta::Grow);
        assert_eq!(
            manager
                .window_states
                .get(&Window::ConversationTree)
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

    /// The view holds still when a window is pushed: the window comes to `view_start`, the
    /// leftmost visible pane. With nothing scrolled that is the front of the stack.
    #[test]
    fn push_window_brings_the_window_to_the_front_of_the_view() {
        let mut manager = make_manager(&[
            Window::Terminal(0),
            Window::Terminal(1),
            Window::Terminal(2),
            Window::Terminal(3),
        ]);
        manager.set_view_start(2);

        manager.push_window(Window::Terminal(3));

        assert_eq!(
            manager.window_stack,
            vec![
                Window::Terminal(0),
                Window::Terminal(1),
                Window::Terminal(3),
                Window::Terminal(2)
            ],
            "the picked window takes the place of the leftmost visible pane"
        );
        assert_eq!(manager.view_start(), 2, "the view did not scroll");
        assert_eq!(manager.slot_window(0), Some(Window::Terminal(3)));
    }

    #[test]
    fn push_window_opens_a_new_window_at_the_front_of_the_view() {
        let mut manager = make_manager(&[
            Window::Terminal(0),
            Window::Terminal(1),
            Window::Terminal(2),
        ]);
        manager.set_view_start(1);

        manager.push_window(Window::Terminal(9));

        assert_eq!(
            manager.window_stack,
            vec![
                Window::Terminal(0),
                Window::Terminal(9),
                Window::Terminal(1),
                Window::Terminal(2)
            ]
        );
        assert_eq!(manager.view_start(), 1);
    }

    /// A window scrolled off to the left is out of the view, not out of the stack: picking it
    /// brings it over the hidden ones, which keep their order behind it.
    #[test]
    fn push_window_keeps_the_windows_hidden_left_of_the_view() {
        let mut manager = make_manager(&[
            Window::Terminal(0),
            Window::Terminal(1),
            Window::Terminal(2),
            Window::Terminal(3),
        ]);
        manager.set_view_start(2);

        manager.push_window(Window::Terminal(0));

        assert_eq!(
            manager.window_stack,
            vec![
                Window::Terminal(1),
                Window::Terminal(0),
                Window::Terminal(2),
                Window::Terminal(3)
            ]
        );
        assert_eq!(
            manager.view_start(),
            1,
            "the view still shows the leftmost pane it did"
        );
        assert_eq!(
            manager.slot_window(0),
            Some(Window::Terminal(0)),
            "and the picked window is its leftmost pane"
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
        assert_eq!(
            manager.focused_window,
            Some(Window::Terminal(2)),
            "focus takes the closed window's place, not the front of the stack"
        );
        assert!(!manager.window_states.contains_key(&Window::Terminal(1)));
    }

    /// With the view scrolled, the front of the stack is off screen: focus must fall on a
    /// pane the user can see.
    #[test]
    fn closing_the_last_window_refocuses_the_one_before_it() {
        let mut manager = make_manager(&[Window::Terminal(0), Window::Terminal(1)]);
        set_all_pane_sizes(&mut manager, PaneSize::Quarter);
        manager.focused_window = Some(Window::Terminal(1));
        manager.set_view_start(1);
        manager.relayout(size(80, 24));

        manager.remove_window(&Window::Terminal(1));

        assert_eq!(manager.focused_window, Some(Window::Terminal(0)));
        assert_eq!(
            manager.view_start(),
            0,
            "the view cannot stay past the stack"
        );
    }

    #[test]
    fn closing_a_window_before_the_view_keeps_it_on_the_same_window() {
        let mut manager = make_manager(&[
            Window::Terminal(0),
            Window::Terminal(1),
            Window::Terminal(2),
        ]);
        set_all_pane_sizes(&mut manager, PaneSize::Full);
        manager.focused_window = Some(Window::Terminal(2));
        manager.set_view_start(2);
        manager.relayout(size(80, 24));

        manager.remove_window(&Window::Terminal(0));

        assert_eq!(manager.view_start(), 1);
        assert_eq!(manager.visible()[0].window, Window::Terminal(2));
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
        use crate::loader::FileKey;
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
        let manager = make_manager(&[Window::ConversationTree, Window::GitLog(1)]);
        let slots = manager.layout(size(0, 0));
        assert!(slots.is_empty());
    }

    #[test]
    fn layout_narrow_surface_uses_one_column() {
        let mut manager = make_manager(&[Window::ConversationTree, Window::GitLog(1)]);
        set_all_pane_sizes(&mut manager, PaneSize::Half);
        let slots = manager.layout(size(80, 24));
        assert_eq!(slots.len(), 2);
        for slot in &slots {
            assert_eq!(
                slot.box_
                    .style_adjusted_bounds
                    .origin_pos
                    .col_index
                    .as_u16(),
                0
            );
        }
    }

    #[test]
    fn layout_column_count_rounds_to_the_nearest_target_width() {
        let columns_at = |width: u16| {
            let mut manager = make_manager(&[
                Window::Terminal(0),
                Window::Terminal(1),
                Window::Terminal(2),
            ]);
            set_all_pane_sizes(&mut manager, PaneSize::Full);
            let slots = manager.layout(size(width, 24));
            let mut origins: Vec<u16> = slots
                .iter()
                .map(|s| s.box_.style_adjusted_bounds.origin_pos.col_index.as_u16())
                .collect();
            origins.sort_unstable();
            origins.dedup();
            let widths: Vec<u16> = slots
                .iter()
                .map(|s| s.box_.style_adjusted_bounds.bounds_size.col_width.as_u16())
                .collect();
            (origins.len(), widths)
        };
        assert_eq!(columns_at(149).0, 1);
        assert_eq!(columns_at(150).0, 2);
        assert_eq!(columns_at(193).0, 2);
        assert_eq!(columns_at(249).0, 2);
        assert!(columns_at(193).1.iter().all(|w| *w == 96 || *w == 97));
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

    /// A terminal closing gives its preview back in the place it replaced, not at the front of
    /// the view: `push_window` would move it over the hidden windows instead.
    #[test]
    fn replacing_a_window_puts_it_back_in_its_place() {
        let preview = Window::FilePreview(crate::loader::FileKey(1));
        let mut manager = make_manager(&[
            Window::Terminal(0),
            Window::Terminal(1),
            preview,
            Window::Terminal(2),
        ]);
        manager.set_view_start(1);

        manager.replace_window(&Window::Terminal(1), preview);

        assert_eq!(
            manager.window_stack,
            vec![Window::Terminal(0), preview, Window::Terminal(2)],
            "the other terminal was the same window and is gone, the rest keep their order"
        );
        assert_eq!(manager.view_start(), 1);
    }

    /// Every pane draws the window and the box of its own slot, so a slot that is not its
    /// index in the view would draw one window into another's box. Holds for every way
    /// `layout` can wrap, truncate or stretch panes, at every position of the view.
    #[test]
    fn a_slot_is_its_index_in_the_view() {
        let mixes = [
            vec![PaneSize::Half; 16],
            vec![PaneSize::Quarter; 16],
            vec![PaneSize::Full; 16],
            vec![
                PaneSize::Half,
                PaneSize::Half,
                PaneSize::Half,
                PaneSize::Half,
                PaneSize::Quarter,
                PaneSize::Quarter,
                PaneSize::Full,
                PaneSize::Half,
                PaneSize::Quarter,
                PaneSize::Quarter,
                PaneSize::Quarter,
                PaneSize::Quarter,
                PaneSize::Third,
                PaneSize::Third,
                PaneSize::Third,
                PaneSize::Third,
            ],
            vec![
                PaneSize::Full,
                PaneSize::Half,
                PaneSize::Full,
                PaneSize::Half,
                PaneSize::Full,
                PaneSize::Half,
                PaneSize::Full,
                PaneSize::Half,
                PaneSize::Full,
                PaneSize::Half,
                PaneSize::Full,
                PaneSize::Half,
                PaneSize::Full,
                PaneSize::Half,
                PaneSize::Full,
                PaneSize::Half,
            ],
        ];
        for count in 1..=crate::tui::pane_manager::MAX_PANES {
            for sizes in &mixes {
                for rows in 5u16..90 {
                    for cols in [80u16, 240, 400, 600] {
                        let mut manager = PaneManager::new();
                        for i in 0..count {
                            manager.push_window(Window::GitLog(15 - i));
                        }
                        for i in 0..count {
                            let window = manager.window_stack[i];
                            manager.window_states.get_mut(&window).unwrap().pane_size = sizes[i];
                        }
                        let size = VPSize {
                            col_width: vp_width(cols),
                            row_height: vp_height(rows),
                        };
                        for view in [0, count / 2, count - 1] {
                            manager.set_view_start(view);
                            manager.relayout(size);
                            let view = manager.view_start();
                            assert!(view < count, "{cols}x{rows}, {count}");
                            assert!(!manager.visible().is_empty(), "{cols}x{rows}, {count}");
                            for (idx, slot) in manager.visible().iter().enumerate() {
                                assert_eq!(slot.slot, idx, "{cols}x{rows}, {count} panes");
                                assert_eq!(manager.slot_window(idx), Some(slot.window));
                                assert_eq!(manager.window_stack[view + idx], slot.window);
                            }
                        }
                    }
                }
            }
        }
    }

    /// The visible panes fill the surface at every position of the view: the last pane of each
    /// column reaches the bottom row, so no window is left with a box a column short.
    #[test]
    fn every_visible_column_reaches_the_bottom_of_the_surface() {
        let mixes = [
            vec![PaneSize::Half; 16],
            vec![PaneSize::Quarter; 16],
            vec![PaneSize::Third; 16],
            vec![
                PaneSize::Half,
                PaneSize::Quarter,
                PaneSize::Full,
                PaneSize::Third,
                PaneSize::Half,
                PaneSize::Quarter,
                PaneSize::Third,
                PaneSize::Full,
                PaneSize::Half,
                PaneSize::Quarter,
                PaneSize::Third,
                PaneSize::Full,
                PaneSize::Half,
                PaneSize::Quarter,
                PaneSize::Third,
                PaneSize::Full,
            ],
        ];
        for count in 1..=crate::tui::pane_manager::MAX_PANES {
            for sizes in &mixes {
                for rows in 5u16..90 {
                    for cols in [80u16, 170, 250, 400] {
                        let mut manager = PaneManager::new();
                        for i in 0..count {
                            manager.push_window(Window::GitLog(15 - i));
                        }
                        for i in 0..count {
                            let window = manager.window_stack[i];
                            manager.window_states.get_mut(&window).unwrap().pane_size = sizes[i];
                        }
                        let size = VPSize {
                            col_width: vp_width(cols),
                            row_height: vp_height(rows),
                        };
                        for view in [0, count / 2, count - 1] {
                            manager.set_view_start(view);
                            manager.relayout(size);
                            let view = manager.view_start();
                            // A column is a run of slots starting in the same cell column.
                            let mut bottoms: Vec<(u16, u16)> = Vec::new();
                            for slot in manager.visible() {
                                let bounds = slot.box_.style_adjusted_bounds;
                                let col = bounds.origin_pos.col_index.as_u16();
                                let bottom = bounds.origin_pos.row_index.as_u16()
                                    + bounds.bounds_size.row_height.as_u16();
                                match bottoms.last_mut() {
                                    Some((c, b)) if *c == col => *b = bottom,
                                    _ => bottoms.push((col, bottom)),
                                }
                            }
                            for (col, bottom) in bottoms {
                                assert_eq!(
                                    bottom, rows,
                                    "{cols}x{rows}, {count} panes, view {view}, column {col}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// A column too tall for `MAX_PANES` is not rendered at all rather than cut: a cut column
    /// would leave panes on screen with nothing drawn in their boxes.
    #[test]
    fn a_column_is_rendered_whole_or_not_at_all() {
        let mut manager = PaneManager::new();
        for i in 0..18 {
            manager.push_window(Window::Terminal(i));
            manager
                .window_states
                .get_mut(&Window::Terminal(i))
                .unwrap()
                .pane_size = PaneSize::Third;
        }
        // Six columns of three thirds: five fit in `MAX_PANES`, the sixth does not.
        let slots = manager.layout(size(600, 24));
        assert_eq!(slots.len(), MAX_PANES - 1);
        let mut columns: Vec<u16> = slots
            .iter()
            .map(|s| s.box_.style_adjusted_bounds.origin_pos.col_index.as_u16())
            .collect();
        columns.dedup();
        assert_eq!(columns.len(), 5, "five whole columns of three");
        assert!(
            manager.window_stack.len() > slots.len(),
            "the rest of the stack stays open behind the view"
        );
    }

    /// `MAX_PANES` can leave the column focus is in out of a frame near the end of a long
    /// stack: the view then moves to the nearest column that does show it, rather than focus
    /// leaving the frame as it did before the view could scroll.
    #[test]
    fn focus_stays_on_screen_when_max_panes_hides_its_column() {
        let stack: Vec<Window> = (0..30).map(Window::Terminal).collect();
        let mut manager = make_manager(&stack);
        set_all_pane_sizes(&mut manager, PaneSize::Quarter);
        manager.focused_window = Some(Window::Terminal(25));
        manager.set_view_start(26);

        manager.relayout(size(100, 40));

        let shown: Vec<Window> = manager.visible().iter().map(|s| s.window).collect();
        assert!(
            shown.contains(&Window::Terminal(25)),
            "the focused window is on screen: {shown:?}"
        );
        assert_eq!(
            manager.view_start(),
            24,
            "the nearest column the frame can draw that holds it"
        );
    }
}
