use crate::agent::CLAUDE_CODE_KIND;
use crate::conversation::{ConversationTree, Node, NodeId, NodeStatus, NodeType};
use crate::tui::text_layout::{display_width, truncate, wrap};
use crate::tui::*;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const GUTTER_GAP: &str = "  ";
const SCROLL_STEP: usize = 3;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fold {
    /// Neither a root with children nor a fork: nothing to fold.
    Leaf,
    Collapsed,
    Expanded,
}

/// Where a row sits among the children of an expanded fork.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Branch {
    /// Not a direct child of a fork: a root, or the continuation of a straight line.
    None,
    Start,
    LastStart,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TreeRow {
    pub id: NodeId,
    /// One entry per level: whether the fork above still has branches below this row.
    pub rails: Vec<bool>,
    pub fold: Fold,
    pub branch: Branch,
}

/// Flattens the tree into display rows: roots newest first, nodes of a conversation
/// oldest first. A root's descendants sit one level below it; after that, indentation
/// grows only below a fork, so a straight line of prompts stays a flat list.
pub(crate) fn layout_rows(tree: &ConversationTree, expanded: &HashSet<NodeId>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    let mut rails = Vec::new();
    for root in tree.roots().iter().rev() {
        push_rows(tree, expanded, root, Branch::None, &mut rails, &mut rows);
    }
    rows
}

fn push_rows(
    tree: &ConversationTree,
    expanded: &HashSet<NodeId>,
    id: &NodeId,
    branch: Branch,
    rails: &mut Vec<bool>,
    rows: &mut Vec<TreeRow>,
) {
    let Some(node) = tree.node(id) else {
        return;
    };
    let is_root = node.parent.is_none();
    let foldable = node.children.len() > 1 || (is_root && !node.children.is_empty());
    let fold = match (foldable, expanded.contains(id)) {
        (false, _) => Fold::Leaf,
        (true, true) => Fold::Expanded,
        (true, false) => Fold::Collapsed,
    };
    rows.push(TreeRow {
        id: id.clone(),
        rails: rails.clone(),
        fold,
        branch,
    });
    if fold == Fold::Collapsed {
        return;
    }
    if fold == Fold::Expanded {
        rails.push(true);
    }
    let last = node.children.len().saturating_sub(1);
    for (i, child) in node.children.iter().enumerate() {
        let child_branch = match node.children.len() {
            0 | 1 => Branch::None,
            _ if i == last => Branch::LastStart,
            _ => Branch::Start,
        };
        if fold == Fold::Expanded {
            *rails.last_mut().unwrap() = i != last;
        }
        push_rows(tree, expanded, child, child_branch, rails, rows);
    }
    if fold == Fold::Expanded {
        rails.pop();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Head,
    TitleRest,
    Summary,
}

struct Line {
    row: usize,
    kind: LineKind,
    text: String,
}

/// Whether `node` runs on the other side of the Claude Code boundary than its parent,
/// which made it start from the ancestors' text only.
fn is_handover(tree: &ConversationTree, node: &Node) -> bool {
    let is_cli = |kind: &Option<String>| kind.as_deref() == Some(CLAUDE_CODE_KIND);
    node.parent
        .as_ref()
        .and_then(|parent| tree.node(parent))
        .is_some_and(|parent| {
            parent.meta.kind.is_some()
                && node.meta.kind.is_some()
                && is_cli(&parent.meta.kind) != is_cli(&node.meta.kind)
        })
}

fn state_marker(tree: &ConversationTree, node: &Node) -> char {
    match (node.meta.status, node.meta.node_type) {
        (Some(NodeStatus::Running), _) => '~',
        (Some(NodeStatus::Failed), _) => '!',
        (Some(NodeStatus::Cancelled), _) => 'x',
        _ if node.meta.stash.is_some() => '?',
        (_, Some(NodeType::Compact)) => '=',
        _ if is_handover(tree, node) => '>',
        _ => ' ',
    }
}

/// The tree prefix of a row's head line, split where the color changes: the rails
/// and arm (gutter color) and the fold column (`+`/`-` in the row's type color,
/// but a leaf's `─` stays gutter color as part of the arm).
fn row_prefix_parts(row: &TreeRow) -> (String, &'static str) {
    let mut prefix = String::new();
    for rail in &row.rails {
        prefix.push_str(if *rail { "│ " } else { "  " });
    }
    match row.branch {
        Branch::None => prefix.push_str("  "),
        Branch::Start => prefix.push_str("├─"),
        Branch::LastStart => prefix.push_str("└─"),
    }
    let fold = match (row.branch, row.fold) {
        (_, Fold::Collapsed) => "+ ",
        (_, Fold::Expanded) => "- ",
        (Branch::None, Fold::Leaf) => "  ",
        (_, Fold::Leaf) => "─ ",
    };
    (prefix, fold)
}

fn row_prefix(row: &TreeRow) -> String {
    let (mut prefix, fold) = row_prefix_parts(row);
    prefix.push_str(fold);
    prefix
}

/// The prefix of the wrapped title and summary lines under a selected row's head:
/// rails only, plus a `│` under an expanded fold so the drop connects to its children.
fn row_continuation_prefix(row: &TreeRow) -> String {
    let mut prefix = String::new();
    for rail in &row.rails {
        prefix.push_str(if *rail { "│ " } else { "  " });
    }
    if row.fold == Fold::Expanded {
        prefix.push_str("│ ");
    } else {
        prefix.push_str("  ");
    }
    prefix
}

/// `hash +N` of a node's commits, tip first: the walk a click on it opens. The
/// metadata stores them oldest first.
fn commit_label(node: &Node) -> Option<String> {
    let tip = node.meta.commits.last()?;
    let short: String = tip.chars().take(7).collect();
    Some(match node.meta.commits.len() {
        1 => short,
        n => format!("{short} +{}", n - 1),
    })
}

pub struct ConversationTreeComponent {
    id: FlexBoxId,
    content_origin_row: usize,
    /// Row index shown on each visible content line, for mouse hit-testing.
    line_rows: Vec<usize>,
    /// Clickable commit label of each visible line as `(start column, width)`:
    /// `Some` only on a head line whose node has commits.
    label_zones: Vec<Option<(usize, usize)>>,
    /// Selection as of the last render; a change re-anchors the scroll onto it.
    shown_selection: Option<NodeId>,
    /// Time and row of the last click, to recognize a double click.
    last_click: Option<(Instant, usize)>,
}

impl ConversationTreeComponent {
    pub(crate) const TITLE: &str = "Conversations";

    pub fn new(id: FlexBoxId) -> Self {
        Self {
            id,
            content_origin_row: 0,
            line_rows: Vec::new(),
            label_zones: Vec::new(),
            shown_selection: None,
            last_click: None,
        }
    }

    fn selected_index(rows: &[TreeRow], selected: Option<&NodeId>) -> usize {
        selected
            .and_then(|id| rows.iter().position(|row| &row.id == id))
            .unwrap_or(0)
    }

    fn select(state: &mut AppState, rows: &[TreeRow], index: usize) {
        if let Some(row) = rows.get(index.min(rows.len().saturating_sub(1))) {
            state.tree_view.selected = Some(row.id.clone());
        }
    }

    /// Nearest ancestor that can be folded: the fork or root a branch hangs from.
    fn foldable_ancestor(rows: &[TreeRow], tree: &ConversationTree, id: &NodeId) -> Option<usize> {
        let mut current = tree.node(id)?.parent.clone();
        while let Some(ancestor) = current {
            if let Some(index) = rows
                .iter()
                .position(|row| row.id == ancestor && row.fold != Fold::Leaf)
            {
                return Some(index);
            }
            current = tree.node(&ancestor)?.parent.clone();
        }
        None
    }

    fn open_commits(
        state: &mut AppState,
        main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
        id: &NodeId,
    ) {
        let commits: Vec<[u8; 20]> = state
            .conversations
            .read()
            .node(id)
            .map(|node| {
                node.meta
                    .commits
                    .iter()
                    .filter_map(|hash| hex::decode(hash).ok())
                    .filter_map(|bytes| <[u8; 20]>::try_from(bytes.as_slice()).ok())
                    .collect()
            })
            .unwrap_or_default();
        for commit in commits.into_iter().rev() {
            GitLogPaneComponent::open_commit_diff(state, main_tx.clone(), commit);
        }
    }

    fn delete_selected(state: &mut AppState, rows: &[TreeRow], index: usize) {
        let Some(row) = rows.get(index) else {
            return;
        };
        let running_below = {
            let tree = state.conversations.read();
            state
                .running_turns
                .keys()
                .any(|running| tree.lineage(running).contains(&row.id))
        };
        if running_below {
            state.tree_view.notice = Some("a turn is running in this branch".to_string());
            return;
        }
        let removed = match state.conversations.write().delete(&row.id) {
            Ok(removed) => removed,
            Err(e) => {
                tracing::warn!("failed to delete conversation node: {e}");
                state.tree_view.notice = Some(format!("delete failed: {e}"));
                return;
            }
        };
        for id in &removed {
            state.tree_view.expanded.remove(id);
            state.discussion_highlights.remove(id);
        }
        let closed: Vec<usize> = state
            .discussions
            .iter()
            .filter(|(_, discussion)| {
                discussion
                    .node
                    .as_ref()
                    .is_some_and(|node| removed.contains(node))
            })
            .map(|(id, _)| *id)
            .collect();
        for id in closed {
            state.discussions.remove(&id);
            state.pane_manager.remove_window(&Window::Discussion(id));
        }
        let rows = layout_rows(&state.conversations.read(), &state.tree_view.expanded);
        state.tree_view.selected = None;
        Self::select(state, &rows, index);
        state.mark_session_dirty();
    }
}

impl TitleRow for ConversationTreeComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        theme: &HelixTheme,
        _state: &AppState,
    ) -> usize {
        render_pane_title(ops, pane_box, Self::TITLE, false, theme, focused);
        1
    }
}

impl Component<AppState, AppSignal> for ConversationTreeComponent {
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
        let main_tx = global_data.main_thread_channel_sender.clone();
        let state = &mut global_data.state;
        let window = Window::ConversationTree;
        let rows = layout_rows(&state.conversations.read(), &state.tree_view.expanded);
        let index = Self::selected_index(&rows, state.tree_view.selected.as_ref());
        let page = state
            .pane_manager
            .window_page_size(&window)
            .saturating_sub(1)
            .max(1);

        let armed = match &input_event {
            InputEvent::Keyboard(_)
            | InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::MouseDown(_),
                ..
            }) => {
                state.tree_view.notice = None;
                state.tree_view.armed_delete.take()
            }
            _ => None,
        };

        match &input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Up),
            }) => Self::select(state, &rows, index.saturating_sub(1)),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Down),
            }) => Self::select(state, &rows, index + 1),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::PageUp),
            }) => Self::select(state, &rows, index.saturating_sub(page)),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::PageDown),
            }) => Self::select(state, &rows, index + page),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Home),
            }) => Self::select(state, &rows, 0),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::End),
            }) => Self::select(state, &rows, rows.len().saturating_sub(1)),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Right),
            }) => {
                if let Some(row) = rows.get(index)
                    && row.fold == Fold::Collapsed
                {
                    state.tree_view.expanded.insert(row.id.clone());
                    state.mark_session_dirty();
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Left),
            }) => {
                let Some(row) = rows.get(index) else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                if row.fold == Fold::Expanded {
                    state.tree_view.expanded.remove(&row.id);
                    state.mark_session_dirty();
                } else {
                    let ancestor =
                        Self::foldable_ancestor(&rows, &state.conversations.read(), &row.id);
                    if let Some(ancestor) = ancestor {
                        Self::select(state, &rows, ancestor);
                    }
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Enter),
            }) => {
                if let Some(row) = rows.get(index) {
                    DiscussionComponent::open_and_recover(state, main_tx, row.id.clone());
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('n'),
            }) => DiscussionComponent::open_new_root(state),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('m'),
            }) => {
                if let Some(row) = rows.get(index) {
                    let purpose = ModelPickerPurpose::Duplicate(row.id.clone());
                    ModelPickerComponent::open(state, main_tx, purpose);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('e'),
            }) => {
                if let Some(row) = rows.get(index) {
                    DiscussionComponent::open_edit(state, main_tx, &row.id);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('c'),
            }) => {
                if let Some(row) = rows.get(index) {
                    Self::open_commits(state, main_tx, &row.id);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('f'),
            }) => {
                if let Some(row) = rows.get(index)
                    && let Err(e) = state.conversations.write().toggle_favorite(&row.id)
                {
                    tracing::warn!("failed to save favorite: {e}");
                    state.tree_view.notice = Some(format!("favorite not saved: {e}"));
                }
            }
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('d'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => {
                let Some(row) = rows.get(index) else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                if armed.as_ref() == Some(&row.id) {
                    Self::delete_selected(state, &rows, index);
                } else {
                    state.tree_view.selected = Some(row.id.clone());
                    state.tree_view.armed_delete = Some(row.id.clone());
                }
            }
            InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::MouseDown(Button::Left),
                pos,
                ..
            }) => {
                let line = pos
                    .row_index
                    .as_usize()
                    .checked_sub(self.content_origin_row);
                let column = pos.col_index.as_usize();
                let Some(row) = line.and_then(|line| self.line_rows.get(line)).copied() else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                let on_label = line
                    .and_then(|line| self.label_zones.get(line).copied().flatten())
                    .is_some_and(|(start, width)| (start..start + width).contains(&column));
                let Some(target) = rows.get(row) else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                Self::select(state, &rows, row);
                if on_label {
                    self.last_click = None;
                    let hash = state
                        .conversations
                        .read()
                        .node(&target.id)
                        .and_then(|node| node.meta.commits.last().cloned());
                    if let Some(hash) = hash {
                        GitLogPaneComponent::open_log_at_commit(state, main_tx, hash);
                    }
                    return Ok(EventPropagation::ConsumedRender);
                }
                let now = Instant::now();
                let double = self
                    .last_click
                    .is_some_and(|(at, last)| last == row && now.duration_since(at) < DOUBLE_CLICK);
                self.last_click = (!double).then_some((now, row));
                if double {
                    DiscussionComponent::open_and_recover(state, main_tx, target.id.clone());
                }
            }
            InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::ScrollUp,
                ..
            }) => {
                let scroll = state.pane_manager.window_scroll(&window);
                state
                    .pane_manager
                    .set_window_scroll(&window, scroll.saturating_sub(SCROLL_STEP));
            }
            InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::ScrollDown,
                ..
            }) => {
                let scroll = state.pane_manager.window_scroll(&window);
                state
                    .pane_manager
                    .set_window_scroll(&window, scroll + SCROLL_STEP);
                state.pane_manager.clamp_scroll(&window);
            }
            _ => return Ok(EventPropagation::Propagate),
        }
        Ok(EventPropagation::ConsumedRender)
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
            self.line_rows.clear();
            self.label_zones.clear();

            let window = Window::ConversationTree;
            let state = &mut global_data.state;
            state.pane_manager.set_window_page_size(&window, total_rows);

            let tree_handle = state.conversations.clone();
            let tree = tree_handle.read();
            let rows = layout_rows(&tree, &state.tree_view.expanded);
            let selected = Self::selected_index(&rows, state.tree_view.selected.as_ref());
            let selected_id = rows.get(selected).map(|row| row.id.clone());
            state.tree_view.selected = selected_id.clone();

            let theme = &state.theme;
            let pane_bg = theme.ui_bg("ui.background").unwrap_or(DEFAULT_BG);
            let selected_bg = theme.ui_bg("ui.selection").unwrap_or(DEFAULT_SELECTION_BG);
            let text_fg = theme
                .ui_fg("ui.text")
                .unwrap_or(RgbValue::from_u8(170, 170, 200));
            let dim_fg = theme.color_for_scope("comment").unwrap_or(text_fg);
            let gutter_fg = theme.ui_fg("ui.linenr").unwrap_or(dim_fg);
            let selected_gutter_fg = theme
                .ui_fg("ui.linenr.selected")
                .or_else(|| theme.ui_fg("ui.text"))
                .unwrap_or(dim_fg);
            let commit_fg = theme
                .color_for_scope("diff.plus")
                .unwrap_or(RgbValue::from_u8(150, 220, 120));
            let hash_fg = theme
                .color_for_scope("constant")
                .unwrap_or(RgbValue::from_u8(250, 200, 90));
            let running_fg = theme
                .color_for_scope("diff.delta")
                .unwrap_or(RgbValue::from_u8(250, 200, 90));
            let warning_fg = theme
                .ui_fg("warning")
                .unwrap_or(RgbValue::from_u8(250, 170, 80));
            let error_fg = theme.ui_fg("error").unwrap_or(DEFAULT_ERROR_FG);
            let favorite_fg = theme
                .ui_fg("ui.cursor.match")
                .unwrap_or(RgbValue::from_u8(255, 200, 60));

            let mut render_ops = RenderOpIRVec::new();

            if rows.is_empty() {
                let message = if state.tree_view.loaded {
                    "(no conversations)"
                } else {
                    "Loading conversations…"
                };
                let bg_style = new_style!(color_bg: {TuiColor::from(pane_bg)});
                let text_style = new_style!(color_fg: {TuiColor::from(text_fg)} color_bg: {TuiColor::from(pane_bg)});
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
                }
                render_ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
                render_ops += RenderOpCommon::ApplyColors(Some(text_style));
                render_ops += RenderOpIR::PaintTextWithAttributes(message.into(), Some(text_style));
                state.pane_manager.set_window_scroll_max(&window, 0);
                global_data.pipeline.push(ZOrder::Normal, render_ops);
                return Ok(());
            }

            let gutter_width = rows
                .iter()
                .filter_map(|row| tree.node(&row.id))
                .map(|node| display_width(&node.created_display))
                .max()
                .unwrap_or(0)
                + GUTTER_GAP.len();

            let mut lines: Vec<Line> = Vec::with_capacity(rows.len() + 8);
            let mut selected_span = (0, 0);
            for (row_idx, row) in rows.iter().enumerate() {
                let Some(node) = tree.node(&row.id) else {
                    continue;
                };
                let prefix_width = gutter_width + display_width(&row_prefix(row));
                let label_width = commit_label(node).map_or(0, |label| display_width(&label) + 1);
                let text_width = pane_width.saturating_sub(prefix_width + label_width).max(1);
                if row_idx != selected {
                    lines.push(Line {
                        row: row_idx,
                        kind: LineKind::Head,
                        text: truncate(node.display_title(), text_width),
                    });
                    continue;
                }
                let start = lines.len();
                for (i, (_, text)) in wrap(node.display_title(), text_width)
                    .into_iter()
                    .enumerate()
                {
                    lines.push(Line {
                        row: row_idx,
                        kind: if i == 0 {
                            LineKind::Head
                        } else {
                            LineKind::TitleRest
                        },
                        text,
                    });
                }
                if let Some(summary) = node.summary.as_deref()
                    && summary != node.display_title()
                {
                    for (_, text) in wrap(summary, text_width) {
                        lines.push(Line {
                            row: row_idx,
                            kind: LineKind::Summary,
                            text,
                        });
                    }
                }
                selected_span = (start, lines.len());
            }

            state
                .pane_manager
                .set_window_scroll_max(&window, lines.len());
            state.pane_manager.clamp_scroll(&window);
            let mut scroll = state.pane_manager.window_scroll(&window);
            if self.shown_selection != selected_id && total_rows > 0 {
                let (start, end) = selected_span;
                if end > scroll + total_rows {
                    scroll = end - total_rows;
                }
                if start < scroll {
                    scroll = start;
                }
                state.pane_manager.set_window_scroll(&window, scroll);
                self.shown_selection = selected_id;
            }

            for row_offset in 0..total_rows {
                let line = lines.get(scroll + row_offset);
                let is_selected = line.is_some_and(|line| line.row == selected);
                let bg = TuiColor::from(if is_selected { selected_bg } else { pane_bg });
                let bg_style = new_style!(color_bg: {bg});
                render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                    origin,
                    col(0) + row(row_offset as u16),
                );
                render_ops += RenderOpCommon::ApplyColors(Some(bg_style));
                render_ops += RenderOpIR::PaintTextWithAttributes(
                    " ".repeat(pane_width).into(),
                    Some(bg_style),
                );
                let Some(line) = line else {
                    continue;
                };
                self.line_rows.push(line.row);
                self.label_zones.push(None);
                let row_info = &rows[line.row];
                let Some(node) = tree.node(&row_info.id) else {
                    continue;
                };

                let armed = state.tree_view.armed_delete.as_ref() == Some(&row_info.id);
                let favorite = node.meta.favorite;
                let type_fg = match (node.meta.status, node.meta.node_type) {
                    _ if armed => error_fg,
                    (Some(NodeStatus::Failed), _) => error_fg,
                    (Some(NodeStatus::Cancelled), _) => warning_fg,
                    (Some(NodeStatus::Running), _) => running_fg,
                    _ if favorite => favorite_fg,
                    (_, Some(NodeType::Commit)) => commit_fg,
                    (_, Some(NodeType::Compact)) => dim_fg,
                    _ => text_fg,
                };
                let styled = |fg: RgbValue| {
                    let fg = TuiColor::from(fg);
                    if favorite {
                        new_style!(bold color_fg: {fg} color_bg: {bg})
                    } else {
                        new_style!(color_fg: {fg} color_bg: {bg})
                    }
                };
                let plain =
                    |fg: RgbValue| new_style!(color_fg: {TuiColor::from(fg)} color_bg: {bg});
                let paint = |ops: &mut RenderOpIRVec, column: usize, text: &str, fg: RgbValue| {
                    let style = styled(fg);
                    *ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        col(column as u16) + row(row_offset as u16),
                    );
                    *ops += RenderOpCommon::ApplyColors(Some(style));
                    *ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
                };
                let paint_plain =
                    |ops: &mut RenderOpIRVec, column: usize, text: &str, fg: RgbValue| {
                        let style = plain(fg);
                        *ops += RenderOpCommon::MoveCursorPositionRelTo(
                            origin,
                            col(column as u16) + row(row_offset as u16),
                        );
                        *ops += RenderOpCommon::ApplyColors(Some(style));
                        *ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
                    };

                let (rail_prefix, fold_text) = row_prefix_parts(row_info);
                let fold_column = gutter_width + display_width(&rail_prefix);
                let text_column = fold_column + display_width(fold_text);
                let marker = state_marker(&tree, node);
                match line.kind {
                    LineKind::Head => {
                        let gutter = if is_selected {
                            selected_gutter_fg
                        } else {
                            gutter_fg
                        };
                        paint_plain(&mut render_ops, 0, &node.created_display, gutter);
                        paint_plain(&mut render_ops, gutter_width, &rail_prefix, gutter);
                        let fold_fg = if row_info.fold == Fold::Leaf {
                            gutter
                        } else {
                            type_fg
                        };
                        paint(&mut render_ops, fold_column, fold_text, fold_fg);
                        if marker != ' ' {
                            paint(&mut render_ops, gutter_width, &marker.to_string(), type_fg);
                        }
                        paint(&mut render_ops, text_column, &line.text, type_fg);
                        if let Some(label) = commit_label(node) {
                            let column = pane_width.saturating_sub(display_width(&label));
                            paint(&mut render_ops, column, &label, hash_fg);
                            let slot = self.label_zones.last_mut().expect("pushed above");
                            *slot = Some((column, display_width(&label)));
                        }
                    }
                    LineKind::TitleRest | LineKind::Summary => {
                        let continuation = row_continuation_prefix(row_info);
                        paint_plain(&mut render_ops, gutter_width, &continuation, gutter_fg);
                        let fg = match line.kind {
                            LineKind::Summary if armed => error_fg,
                            LineKind::Summary => dim_fg,
                            _ => type_fg,
                        };
                        paint(&mut render_ops, text_column, &line.text, fg);
                    }
                }
            }

            global_data.pipeline.push(ZOrder::Normal, render_ops);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    fn tree_with(paths: &[&str]) -> (tempfile::TempDir, ConversationTree) {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        for path in paths {
            std::fs::create_dir_all(root.join(path)).unwrap();
        }
        let tree = ConversationTree::load(&root);
        (tmp, tree)
    }

    fn shape(rows: &[TreeRow]) -> Vec<(&str, Vec<bool>, Fold)> {
        rows.iter()
            .map(|row| (row.id.as_str(), row.rails.clone(), row.fold))
            .collect()
    }

    fn branches(rows: &[TreeRow]) -> Vec<Branch> {
        rows.iter().map(|row| row.branch).collect()
    }

    fn prefixes(rows: &[TreeRow]) -> Vec<String> {
        rows.iter().map(row_prefix).collect()
    }

    fn ids(names: &[&str]) -> HashSet<NodeId> {
        names.iter().map(|name| NodeId::new(name)).collect()
    }

    #[test]
    fn roots_are_newest_first_and_collapsed_by_default() {
        let (_tmp, tree) = tree_with(&["a1/a2", "b1"]);
        let rows = layout_rows(&tree, &HashSet::new());
        assert_eq!(
            shape(&rows),
            [("b1", vec![], Fold::Leaf), ("a1", vec![], Fold::Collapsed)]
        );
    }

    #[test]
    fn straight_line_stays_at_one_level_below_its_root() {
        let (_tmp, tree) = tree_with(&["a1/a2/a3/a4"]);
        let rows = layout_rows(&tree, &ids(&["a1"]));
        assert_eq!(
            shape(&rows),
            [
                ("a1", vec![], Fold::Expanded),
                ("a2", vec![false], Fold::Leaf),
                ("a3", vec![false], Fold::Leaf),
                ("a4", vec![false], Fold::Leaf),
            ]
        );
    }

    #[test]
    fn indentation_grows_only_below_an_expanded_fork() {
        let (_tmp, tree) = tree_with(&["a1/a2/a3/a4", "a1/a2/b3"]);
        let collapsed_fork = layout_rows(&tree, &ids(&["a1"]));
        assert_eq!(
            shape(&collapsed_fork),
            [
                ("a1", vec![], Fold::Expanded),
                ("a2", vec![false], Fold::Collapsed)
            ]
        );

        let rows = layout_rows(&tree, &ids(&["a1", "a2"]));
        assert_eq!(
            shape(&rows),
            [
                ("a1", vec![], Fold::Expanded),
                ("a2", vec![false], Fold::Expanded),
                ("a3", vec![false, true], Fold::Leaf),
                ("a4", vec![false, true], Fold::Leaf),
                ("b3", vec![false, false], Fold::Leaf),
            ]
        );
        assert_eq!(
            branches(&rows),
            [
                Branch::None,
                Branch::None,
                Branch::Start,
                Branch::None,
                Branch::LastStart,
            ]
        );
    }

    #[test]
    fn rails_run_only_while_a_fork_has_branches_below() {
        let (_tmp, tree) = tree_with(&["a1/a2", "a1/b2", "a1/c2"]);
        let rows = layout_rows(&tree, &ids(&["a1"]));
        assert_eq!(
            shape(&rows),
            [
                ("a1", vec![], Fold::Expanded),
                ("a2", vec![true], Fold::Leaf),
                ("b2", vec![true], Fold::Leaf),
                ("c2", vec![false], Fold::Leaf),
            ]
        );
    }

    #[test]
    fn head_prefix_draws_rails_arms_and_fold_column() {
        let (_tmp, tree) = tree_with(&["a1/a2", "a1/b2", "c1/c2/d3", "c1/c2/e3"]);
        let rows = layout_rows(&tree, &ids(&["a1", "c1", "c2"]));
        assert_eq!(
            prefixes(&rows),
            [
                "  - ",     // c1: expanded root
                "    - ",   // c2: expanded fork, only child of c1 → no rail
                "  │ ├── ", // d3: first branch of c2, rail hangs from c2
                "    └── ", // e3: last branch of c2
                "  - ",     // a1: expanded root
                "│ ├── ",   // a2: first branch of a1
                "  └── ",   // b2: last branch of a1
            ]
        );
    }

    #[test]
    fn collapsed_fork_leaf_and_last_branch_leaf_markers() {
        let (_tmp, tree) = tree_with(&["a1/a2/a3", "a1/a2/b3", "a1/b2"]);
        let rows = layout_rows(&tree, &ids(&["a1"]));
        assert_eq!(
            prefixes(&rows),
            [
                "  - ",   // a1: expanded root
                "│ ├─+ ", // a2: collapsed fork starting a branch
                "  └── ", // b2: last branch, leaf
            ]
        );
        let continuation = row_continuation_prefix(&rows[1]);
        assert_eq!(continuation, "│   ");
    }

    #[test]
    fn continuation_prefix_hangs_under_an_expanded_fork() {
        let (_tmp, tree) = tree_with(&["a1/a2/b3", "a1/a2/c3"]);
        let rows = layout_rows(&tree, &ids(&["a1", "a2"]));
        let continuation = row_continuation_prefix(&rows[1]);
        assert_eq!(continuation, "  │ ");
    }
}
