use crate::agent::turn::spinner_frame;
use crate::conversation::{ConversationTree, Node, NodeId, NodeStatus, NodeType};
use crate::tui::state::{PendingTurn, RowTarget};
use crate::tui::text_layout::{display_width, truncate, wrap};
use crate::tui::*;
use nucleo::pattern::{Atom, AtomKind, CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};
use std::cmp::Ordering;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

const GUTTER_GAP: &str = "   ";
const SCROLL_STEP: usize = 3;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fold {
    /// Neither a fork nor the head of a thread with replies to hide: nothing to fold.
    Leaf,
    Collapsed,
    Expanded,
}

/// Whether a row starts a branch: a child of a fork, or the first row below a root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Branch {
    /// A root, or the continuation of a thread.
    None,
    Start,
    LastStart,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TreeRow {
    pub target: RowTarget,
    /// One entry per level: whether the fork above still has branches below this row.
    pub rails: Vec<bool>,
    pub fold: Fold,
    pub branch: Branch,
}

impl TreeRow {
    fn node(id: &NodeId, rails: Vec<bool>, fold: Fold, branch: Branch) -> Self {
        Self {
            target: RowTarget::Node(id.clone()),
            rails,
            fold,
            branch,
        }
    }
}

/// Flattens the tree into display rows: roots oldest first, like the nodes of a
/// conversation. A thread, the straight run of single replies below a root or a
/// branch, folds into its head; the fork ending it stays visible. A root's descendants
/// sit one level below it; after that, indentation grows only below a fork.
pub(crate) fn layout_rows(tree: &ConversationTree, expanded: &HashSet<NodeId>) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    let mut rails = Vec::new();
    for root in tree.roots() {
        push_thread(tree, expanded, root, Branch::None, &mut rails, &mut rows);
    }
    rows
}

/// The straight run below `head`: the single replies that fold into it, then the fork
/// ending the run, if any. Both are empty when `head` is itself a fork.
fn thread_below<'a>(
    tree: &'a ConversationTree,
    head: &NodeId,
) -> (Vec<&'a NodeId>, Option<&'a NodeId>) {
    let mut hidden = Vec::new();
    let mut current = tree.node(head);
    while let Some([only]) = current.map(|node| node.children.as_slice()) {
        let reply = tree.node(only);
        if reply.is_some_and(|reply| reply.children.len() > 1) {
            return (hidden, Some(only));
        }
        hidden.push(only);
        current = reply;
    }
    (hidden, None)
}

/// Last of the replies folded into the thread head `head`: where `Right` lands.
pub(crate) fn thread_end(tree: &ConversationTree, head: &NodeId) -> Option<NodeId> {
    thread_below(tree, head).0.last().map(|id| (*id).clone())
}

/// The nodes on the way to `node`, itself included, that can hold a fold: forks and
/// thread heads.
pub(crate) fn folds_on_the_way(tree: &ConversationTree, node: &NodeId) -> Vec<NodeId> {
    let is_fork = |id: &NodeId| tree.node(id).is_some_and(|node| node.children.len() > 1);
    tree.lineage(node)
        .into_iter()
        .filter(|id| {
            let parent = tree.node(id).and_then(|node| node.parent.as_ref());
            is_fork(id) || parent.is_none_or(is_fork)
        })
        .collect()
}

fn fold_of(foldable: bool, expanded: bool) -> Fold {
    match (foldable, expanded) {
        (false, _) => Fold::Leaf,
        (true, true) => Fold::Expanded,
        (true, false) => Fold::Collapsed,
    }
}

fn push_thread(
    tree: &ConversationTree,
    expanded: &HashSet<NodeId>,
    head: &NodeId,
    branch: Branch,
    rails: &mut Vec<bool>,
    rows: &mut Vec<TreeRow>,
) {
    let Some(node) = tree.node(head) else {
        return;
    };
    let (hidden, fork) = thread_below(tree, head);
    let is_fork = node.children.len() > 1;
    let fold = fold_of(is_fork || !hidden.is_empty(), expanded.contains(head));
    rows.push(TreeRow::node(head, rails.clone(), fold, branch));
    if is_fork {
        if fold == Fold::Expanded {
            push_branches(tree, expanded, node, rails, rows);
        }
        return;
    }
    let below_root = node.parent.is_none() && !node.children.is_empty();
    let mut branch = Branch::None;
    if below_root {
        rails.push(false);
        branch = Branch::LastStart;
    }
    if fold == Fold::Expanded {
        for id in hidden {
            rows.push(TreeRow::node(id, rails.clone(), Fold::Leaf, branch));
            branch = Branch::None;
        }
    }
    if let Some(fork) = fork
        && let Some(fork_node) = tree.node(fork)
    {
        let fold = fold_of(true, expanded.contains(fork));
        rows.push(TreeRow::node(fork, rails.clone(), fold, branch));
        if fold == Fold::Expanded {
            push_branches(tree, expanded, fork_node, rails, rows);
        }
    }
    if below_root {
        rails.pop();
    }
}

fn push_branches(
    tree: &ConversationTree,
    expanded: &HashSet<NodeId>,
    fork: &Node,
    rails: &mut Vec<bool>,
    rows: &mut Vec<TreeRow>,
) {
    let last = fork.children.len().saturating_sub(1);
    for (i, child) in fork.children.iter().enumerate() {
        let branch = if i == last {
            Branch::LastStart
        } else {
            Branch::Start
        };
        rails.push(i != last);
        push_thread(tree, expanded, child, branch, rails, rows);
        rails.pop();
    }
}

/// Byte ranges of a text, start inclusive and end exclusive, in order.
type Ranges = Vec<(usize, usize)>;

/// A node the filter kept, with what matched in its title and in its summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FilterMatch {
    pub node: NodeId,
    pub title: Ranges,
    pub summary: Ranges,
}

/// The summary shown under a node's title, when it says something else.
fn shown_summary(node: &Node) -> Option<&str> {
    node.summary
        .as_deref()
        .filter(|summary| *summary != node.display_title())
}

/// The nodes holding every word of `query` in their title or their summary, in any
/// order: best match first, then newest first.
pub(crate) fn filter_nodes(tree: &ConversationTree, query: &str) -> Vec<FilterMatch> {
    let pattern = Pattern::new(
        query,
        CaseMatching::Smart,
        Normalization::Smart,
        AtomKind::Substring,
    );
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut chars = Vec::new();
    let mut scored: Vec<(u32, FilterMatch)> = tree
        .nodes()
        .filter_map(|(id, node)| {
            let title = node.display_title();
            let text = match shown_summary(node) {
                Some(summary) => format!("{title}\n{summary}"),
                None => title.to_string(),
            };
            // `Utf32Str::new` indexes by byte or by grapheme depending on the text.
            chars.clear();
            chars.extend(text.chars());
            // Nucleo misses a substring that ends a non-ASCII haystack.
            chars.push('\n');
            let score = pattern.score(Utf32Str::Unicode(&chars), &mut matcher)?;
            let mut indices = Vec::new();
            for atom in &pattern.atoms {
                push_occurrences(atom, &chars, &mut matcher, &mut indices);
            }
            indices.sort_unstable();
            indices.dedup();
            let (title, summary) = matched_ranges(&text, title.len(), &indices);
            Some((
                score,
                FilterMatch {
                    node: id.clone(),
                    title,
                    summary,
                },
            ))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.node.cmp(&a.1.node)));
    scored.into_iter().map(|(_, found)| found).collect()
}

/// Every place `atom` matches in `chars`, as char indices: an atom alone reports only
/// the first one.
fn push_occurrences(atom: &Atom, chars: &[char], matcher: &mut Matcher, indices: &mut Vec<u32>) {
    let mut from = 0;
    let mut found = Vec::new();
    while from < chars.len() {
        found.clear();
        let rest = Utf32Str::Unicode(&chars[from..]);
        if atom.indices(rest, matcher, &mut found).is_none() {
            break;
        }
        let Some(last) = found.iter().max().copied() else {
            break;
        };
        indices.extend(found.iter().map(|index| index + from as u32));
        from += last as usize + 1;
    }
}

/// Turns sorted char indices of `text`, a title of `title_len` bytes then a newline and
/// a summary, into byte ranges of the title and of the summary.
fn matched_ranges(text: &str, title_len: usize, indices: &[u32]) -> (Ranges, Ranges) {
    let mut title = Ranges::new();
    let mut summary = Ranges::new();
    let mut wanted = indices.iter().copied().peekable();
    for (index, (at, ch)) in text.char_indices().enumerate() {
        if wanted.peek() != Some(&(index as u32)) {
            continue;
        }
        wanted.next();
        let (ranges, start) = match at.cmp(&title_len) {
            Ordering::Less => (&mut title, at),
            Ordering::Equal => continue,
            Ordering::Greater => (&mut summary, at - title_len - 1),
        };
        let end = start + ch.len_utf8();
        match ranges.last_mut() {
            Some(last) if last.1 == start => last.1 = end,
            _ => ranges.push((start, end)),
        }
    }
    (title, summary)
}

/// Cuts `text`, which starts at byte `offset` of its source, where it enters and leaves
/// the matched `ranges` of that source. Each piece says whether it is matched.
fn segments<'a>(text: &'a str, offset: usize, ranges: &[(usize, usize)]) -> Vec<(&'a str, bool)> {
    let mut pieces = Vec::new();
    let mut at = 0;
    for &(start, end) in ranges {
        let start = start.saturating_sub(offset);
        let end = end.saturating_sub(offset).min(text.len());
        if start >= end {
            continue;
        }
        if start > at {
            pieces.push((&text[at..start], false));
        }
        pieces.push((&text[start..end], true));
        at = end;
    }
    if at < text.len() {
        pieces.push((&text[at..], false));
    }
    pieces
}

/// The rows of a filtered window: its matches, flat.
fn flat_rows(matches: &[FilterMatch]) -> Vec<TreeRow> {
    matches
        .iter()
        .map(|found| TreeRow::node(&found.node, Vec::new(), Fold::Leaf, Branch::None))
        .collect()
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
    /// Byte offset of `text` in the title or the summary it is cut from.
    offset: usize,
    text: String,
}

fn state_marker(node: &Node, running_for: Option<Duration>) -> Option<char> {
    match (node.meta.status, node.meta.node_type) {
        (Some(NodeStatus::Running), _) => Some(spinner_frame(running_for.unwrap_or_default())),
        (Some(NodeStatus::Failed), _) => Some('✗'),
        (Some(NodeStatus::Cancelled), _) => Some('■'),
        _ if node.meta.stash.is_some() => Some('±'),
        (_, Some(NodeType::Compact)) => Some('≡'),
        _ => None,
    }
}

/// The tree with one provisional row per queued prompt, after the row of the node it
/// will follow. An entry whose anchor row is not shown (folded away, or a root prompt
/// whose window shows no node yet) is left out: the queue is not part of the tree, so
/// it cannot invent a row for a node that is not on screen.
pub(crate) fn with_queued(rows: Vec<TreeRow>, queue: &[PendingTurn]) -> Vec<TreeRow> {
    let mut extra: Vec<Vec<TreeRow>> = vec![Vec::new(); rows.len()];
    for (position, entry) in queue.iter().enumerate() {
        let Some(parent) = &entry.parent else {
            continue;
        };
        let Some(anchor) = rows
            .iter()
            .position(|row| row.target.node() == Some(parent))
        else {
            continue;
        };
        extra[anchor].push(TreeRow {
            target: RowTarget::Queued {
                window: entry.window,
                position: position + 1,
            },
            rails: rows[anchor].rails.clone(),
            fold: Fold::Leaf,
            branch: Branch::LastStart,
        });
    }
    let mut lines = Vec::with_capacity(rows.len());
    for (index, row) in rows.into_iter().enumerate() {
        lines.push(row);
        for queued in extra[index].drain(..) {
            lines.push(queued);
        }
    }
    lines
}

/// One column, nine digits.
fn queue_glyph(position: usize) -> char {
    if position < 10 {
        (b'0' + position as u8) as char
    } else {
        '…'
    }
}

/// What a provisional row shows: the first line of the prompt its window holds.
fn queued_text(state: &AppState, row: &TreeRow) -> String {
    let RowTarget::Queued { window, .. } = &row.target else {
        return String::new();
    };
    let draft = state
        .discussions
        .get(window)
        .map(|discussion| discussion.draft.text())
        .unwrap_or_default();
    draft
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("(empty prompt)")
        .to_string()
}

/// The tree prefix of a row's head line, split where the color changes: the rails
/// and arm (gutter color) and the fold column (`+`/`-` in the row's type color,
/// but a leaf's `─` stays gutter color as part of the arm).
fn row_prefix_parts(row: &TreeRow) -> (String, &'static str) {
    let mut prefix = String::new();
    let (own, above) = match row.rails.split_last() {
        Some((own, above)) => (Some(*own), above),
        None => (None, &row.rails[..]),
    };
    for rail in above {
        prefix.push_str(if *rail { "│ " } else { "  " });
    }
    match (row.branch, own) {
        (Branch::Start, _) => prefix.push_str("├─"),
        (Branch::LastStart, _) => prefix.push_str("└─"),
        (Branch::None, Some(true)) => prefix.push_str("│ "),
        (Branch::None, Some(false)) => prefix.push_str("  "),
        (Branch::None, None) => {}
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
/// rails only, plus a `│` in the fold column when rows hang from it (`hangs`: the next
/// row is one level deeper), so the drop connects to them.
fn row_continuation_prefix(row: &TreeRow, hangs: bool) -> String {
    let mut prefix = String::new();
    for rail in &row.rails {
        prefix.push_str(if *rail { "│ " } else { "  " });
    }
    prefix.push_str(if hangs { "│ " } else { "  " });
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
    content_origin_col: usize,
    /// Row index shown on each visible content line, for mouse hit-testing.
    line_rows: Vec<usize>,
    /// Clickable commit label of each visible line as `(start column, width)`:
    /// `Some` only on a head line whose node has commits.
    label_zones: Vec<Option<(usize, usize)>>,
    /// Selection as of the last render; a change re-anchors the scroll onto it.
    shown_selection: Option<RowTarget>,
    /// Time and row of the last click, to recognize a double click.
    last_click: Option<(Instant, usize)>,
}

impl ConversationTreeComponent {
    const TITLE: &str = "Conversations";

    pub fn new(id: FlexBoxId) -> Self {
        Self {
            id,
            content_origin_row: 0,
            content_origin_col: 0,
            line_rows: Vec::new(),
            label_zones: Vec::new(),
            shown_selection: None,
            last_click: None,
        }
    }

    pub(crate) fn title(state: &AppState) -> String {
        if state.tree_view.filtering() {
            format!("{} — {}", Self::TITLE, state.tree_view.query.text().trim())
        } else {
            Self::TITLE.to_string()
        }
    }

    /// The row `selected` sits on. Without one, the newest root of the tree, or the
    /// best match of a filter.
    fn selected_index(rows: &[TreeRow], selected: Option<&RowTarget>, filtering: bool) -> usize {
        selected
            .and_then(|target| rows.iter().position(|row| &row.target == target))
            .or_else(|| {
                rows.iter()
                    .rposition(|row| row.rails.is_empty())
                    .filter(|_| !filtering)
            })
            .unwrap_or(0)
    }

    /// The rows the pane shows: the tree, plus a provisional row per queued prompt; or
    /// the matches of the filter, flat.
    fn rows(state: &AppState) -> Vec<TreeRow> {
        let tree = state.conversations.read();
        if state.tree_view.filtering() {
            return flat_rows(&filter_nodes(&tree, state.tree_view.query.text()));
        }
        with_queued(
            layout_rows(&tree, &state.tree_view.expanded),
            &state.queued_turns,
        )
    }

    /// Selects `target` for the unfiltered tree, opening the folds that hide its node.
    fn reveal(state: &mut AppState, target: RowTarget) {
        if let Some(id) = target.node() {
            let folds = folds_on_the_way(&state.conversations.read(), id);
            state
                .tree_view
                .expanded
                .extend(folds.into_iter().filter(|fold| fold != id));
        }
        state.tree_view.selected = Some(target);
    }

    fn select(state: &mut AppState, rows: &[TreeRow], index: usize) {
        if let Some(row) = rows.get(index.min(rows.len().saturating_sub(1))) {
            state.tree_view.selected = Some(row.target.clone());
        }
    }

    /// Nearest ancestor that can be folded: the fork or root a branch hangs from.
    fn foldable_ancestor(rows: &[TreeRow], tree: &ConversationTree, id: &NodeId) -> Option<usize> {
        let mut current = tree.node(id)?.parent.clone();
        while let Some(ancestor) = current {
            if let Some(index) = rows
                .iter()
                .position(|row| row.target.node() == Some(&ancestor) && row.fold != Fold::Leaf)
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
        let Some(id) = rows.get(index).and_then(|row| row.target.node().cloned()) else {
            return;
        };
        let running_below = {
            let tree = state.conversations.read();
            state
                .running_turns
                .keys()
                .any(|running| tree.lineage(running).contains(&id))
        };
        if running_below {
            state.tree_view.notice = Some("a turn is running in this branch".to_string());
            return;
        }
        let removed = match state.conversations.write().delete(&id) {
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
            crate::tui::discussion_pane::drop_queue_entry(state, id);
            state.discussions.remove(&id);
            state.pane_manager.remove_window(&Window::Discussion(id));
        }
        let rows = Self::rows(state);
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
        state: &AppState,
    ) -> usize {
        if !state.tree_view.search_active {
            render_pane_title(ops, pane_box, &Self::title(state), false, theme, focused);
            return 1;
        }
        let origin = pane_box.style_adjusted_origin_pos;
        let width = pane_box.style_adjusted_bounds_size.col_width.as_u16();
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
        state.tree_view.query.render(
            ops,
            None,
            "/",
            origin,
            width,
            1,
            focused,
            (color_bg, color_fg),
        );
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
        let rows = Self::rows(state);
        let index = Self::selected_index(
            &rows,
            state.tree_view.selected.as_ref(),
            state.tree_view.filtering(),
        );
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

        if state.tree_view.search_active && matches!(input_event, InputEvent::Keyboard(_)) {
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
                    key: Key::SpecialKey(SpecialKey::Enter),
                }) => {
                    state.tree_view.search_active = false;
                    state.tree_view.before_search = None;
                }
                InputEvent::Keyboard(KeyPress::Plain {
                    key: Key::SpecialKey(SpecialKey::Esc),
                }) => {
                    if state.tree_view.filtering() {
                        let kept = rows
                            .get(index)
                            .map(|row| row.target.clone())
                            .or_else(|| state.tree_view.before_search.take());
                        if let Some(target) = kept {
                            Self::reveal(state, target);
                        }
                    }
                    state.tree_view.query.clear();
                    state.tree_view.search_active = false;
                    state.tree_view.before_search = None;
                    self.shown_selection = None;
                    state.mark_session_dirty();
                }
                _ => {
                    let before = state.tree_view.query.text().to_string();
                    state.tree_view.query.handle_key(&input_event);
                    if state.tree_view.query.text() != before {
                        state.tree_view.selected = if state.tree_view.filtering() {
                            Self::rows(state).first().map(|row| row.target.clone())
                        } else {
                            state.tree_view.before_search.clone()
                        };
                        self.shown_selection = None;
                        state.mark_session_dirty();
                    }
                }
            }
            return Ok(EventPropagation::ConsumedRender);
        }

        match &input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('/'),
            }) => {
                state.tree_view.search_active = true;
                state.tree_view.before_search = rows.get(index).map(|row| row.target.clone());
            }
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
                let Some(row) = rows.get(index) else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                let Some(id) = row.target.node().cloned() else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                if row.fold == Fold::Collapsed {
                    state.tree_view.expanded.insert(id.clone());
                    state.mark_session_dirty();
                }
                let end = match row.fold {
                    Fold::Leaf => None,
                    _ => thread_end(&state.conversations.read(), &id),
                };
                if let Some(end) = end {
                    state.tree_view.selected = Some(RowTarget::Node(end));
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Left),
            }) => {
                let Some(row) = rows.get(index) else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                let Some(id) = row.target.node().cloned() else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                if row.fold == Fold::Expanded {
                    state.tree_view.expanded.remove(&id);
                    state.mark_session_dirty();
                } else {
                    let ancestor = Self::foldable_ancestor(&rows, &state.conversations.read(), &id);
                    if let Some(ancestor) = ancestor {
                        Self::select(state, &rows, ancestor);
                    }
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }) => match rows.get(index).map(|row| &row.target) {
                Some(RowTarget::Queued { window, .. }) => {
                    crate::tui::discussion_pane::take_back_window(state, main_tx, *window);
                }
                Some(RowTarget::Node(id)) => {
                    crate::tui::discussion_pane::take_back(state, main_tx, id);
                }
                None => {}
            },
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Backspace),
            }) => {
                state.pane_manager.send_to_back(&window);
                state.mark_session_dirty();
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Enter),
            }) => match rows.get(index).map(|row| &row.target) {
                Some(RowTarget::Queued { window, .. }) => {
                    DiscussionComponent::show(state, *window);
                }
                Some(RowTarget::Node(id)) => {
                    DiscussionComponent::open_and_recover(state, main_tx, id.clone());
                }
                None => {}
            },
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('n'),
            }) => DiscussionComponent::open_new_root(state),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('m'),
            }) => {
                if let Some(id) = rows.get(index).and_then(|row| row.target.node()) {
                    let purpose = ModelPickerPurpose::Duplicate(id.clone());
                    ModelPickerComponent::open(state, main_tx, purpose);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('e'),
            }) => {
                if let Some(id) = rows.get(index).and_then(|row| row.target.node()) {
                    DiscussionComponent::open_edit(state, main_tx, id);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('c'),
            }) => {
                if let Some(id) = rows.get(index).and_then(|row| row.target.node()) {
                    Self::open_commits(state, main_tx, id);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('f'),
            }) => {
                if let Some(id) = rows.get(index).and_then(|row| row.target.node())
                    && let Err(e) = state.conversations.write().toggle_favorite(id)
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
                let Some(id) = row.target.node().cloned() else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                if armed.as_ref() == Some(&id) {
                    Self::delete_selected(state, &rows, index);
                } else {
                    state.tree_view.selected = Some(RowTarget::Node(id.clone()));
                    state.tree_view.armed_delete = Some(id);
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
                let column = pos
                    .col_index
                    .as_usize()
                    .saturating_sub(self.content_origin_col);
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
                let Some(id) = target.target.node().cloned() else {
                    let now = Instant::now();
                    let double = self.last_click.is_some_and(|(at, last)| {
                        last == row && now.duration_since(at) < DOUBLE_CLICK
                    });
                    self.last_click = (!double).then_some((now, row));
                    if double && let RowTarget::Queued { window, .. } = &target.target {
                        DiscussionComponent::show(state, *window);
                    }
                    return Ok(EventPropagation::ConsumedRender);
                };
                if on_label {
                    self.last_click = None;
                    let hash = state
                        .conversations
                        .read()
                        .node(&id)
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
                    DiscussionComponent::open_and_recover(state, main_tx, id);
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
            self.content_origin_col = origin.col_index.as_usize();
            self.line_rows.clear();
            self.label_zones.clear();

            let window = Window::ConversationTree;
            let state = &mut global_data.state;
            state.pane_manager.set_window_page_size(&window, total_rows);

            let tree_handle = state.conversations.clone();
            let tree = tree_handle.read();
            let filtering = state.tree_view.filtering();
            let matches = if filtering {
                filter_nodes(&tree, state.tree_view.query.text())
            } else {
                Vec::new()
            };
            let rows = if filtering {
                flat_rows(&matches)
            } else {
                with_queued(
                    layout_rows(&tree, &state.tree_view.expanded),
                    &state.queued_turns,
                )
            };
            let selected =
                Self::selected_index(&rows, state.tree_view.selected.as_ref(), filtering);
            let selected_target = rows.get(selected).map(|row| row.target.clone());
            state.tree_view.selected = selected_target.clone();

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
            let match_fg = favorite_fg;

            let mut render_ops = RenderOpIRVec::new();

            if rows.is_empty() {
                let message = if !state.tree_view.loaded {
                    "Loading conversations…"
                } else if filtering {
                    "No matches"
                } else {
                    "(no conversations)"
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
                .filter_map(|row| row.target.node())
                .filter_map(|id| tree.node(id))
                .map(|node| display_width(&node.created_display))
                .max()
                .unwrap_or(0)
                + GUTTER_GAP.len();

            let mut lines: Vec<Line> = Vec::with_capacity(rows.len() + 8);
            let mut selected_span = (0, 0);
            for (row_idx, row) in rows.iter().enumerate() {
                let Some(id) = row.target.node() else {
                    let prefix_width = gutter_width + display_width(&row_prefix(row));
                    let width = pane_width.saturating_sub(prefix_width).max(1);
                    let start = lines.len();
                    lines.push(Line {
                        row: row_idx,
                        kind: LineKind::Head,
                        offset: 0,
                        text: truncate(&queued_text(state, row), width),
                    });
                    if row_idx == selected {
                        selected_span = (start, lines.len());
                    }
                    continue;
                };
                let Some(node) = tree.node(id) else {
                    continue;
                };
                let prefix_width = gutter_width + display_width(&row_prefix(row));
                let label_width = commit_label(node).map_or(0, |label| display_width(&label) + 1);
                let text_width = pane_width.saturating_sub(prefix_width + label_width).max(1);
                if row_idx != selected {
                    lines.push(Line {
                        row: row_idx,
                        kind: LineKind::Head,
                        offset: 0,
                        text: truncate(node.display_title(), text_width),
                    });
                    continue;
                }
                let start = lines.len();
                for (i, (offset, text)) in wrap(node.display_title(), text_width)
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
                        offset,
                        text,
                    });
                }
                if let Some(summary) = shown_summary(node) {
                    for (offset, text) in wrap(summary, text_width) {
                        lines.push(Line {
                            row: row_idx,
                            kind: LineKind::Summary,
                            offset,
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
            if self.shown_selection != selected_target && total_rows > 0 {
                let (start, end) = selected_span;
                if end > scroll + total_rows {
                    scroll = end - total_rows;
                }
                if start < scroll {
                    scroll = start;
                }
                state.pane_manager.set_window_scroll(&window, scroll);
                self.shown_selection = selected_target.clone();
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
                let gutter = if is_selected {
                    selected_gutter_fg
                } else {
                    gutter_fg
                };
                let plain =
                    |fg: RgbValue| new_style!(color_fg: {TuiColor::from(fg)} color_bg: {bg});
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

                // A queued prompt has no node: nothing to arm, fold or label.
                let Some(node) = row_info.target.node().and_then(|id| tree.node(id)) else {
                    let RowTarget::Queued { position, .. } = row_info.target else {
                        continue;
                    };
                    paint_plain(&mut render_ops, gutter_width, &rail_prefix, gutter);
                    paint_plain(&mut render_ops, fold_column, fold_text, gutter);
                    paint_plain(
                        &mut render_ops,
                        gutter_width - 2,
                        &queue_glyph(position).to_string(),
                        dim_fg,
                    );
                    paint_plain(&mut render_ops, text_column, &line.text, dim_fg);
                    continue;
                };
                let node_id = row_info.target.node().expect("checked above");

                let armed = state.tree_view.armed_delete.as_ref() == Some(node_id);
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
                let paint = |ops: &mut RenderOpIRVec, column: usize, text: &str, fg: RgbValue| {
                    let style = styled(fg);
                    *ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        col(column as u16) + row(row_offset as u16),
                    );
                    *ops += RenderOpCommon::ApplyColors(Some(style));
                    *ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
                };

                // While filtering, the rows are the matches, in the same order.
                let matched: &[(usize, usize)] = match (matches.get(line.row), line.kind) {
                    (Some(found), LineKind::Summary) => &found.summary,
                    (Some(found), _) => &found.title,
                    (None, _) => &[],
                };
                let paint_text = |ops: &mut RenderOpIRVec, fg: RgbValue| {
                    let mut column = text_column;
                    for (text, is_match) in segments(&line.text, line.offset, matched) {
                        let mut style = styled(if is_match { match_fg } else { fg });
                        if is_match {
                            style.attribs.underline = Some(tui_style_attrib::Underline);
                        }
                        *ops += RenderOpCommon::MoveCursorPositionRelTo(
                            origin,
                            col(column as u16) + row(row_offset as u16),
                        );
                        *ops += RenderOpCommon::ApplyColors(Some(style));
                        *ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
                        column += display_width(text);
                    }
                };

                let running_for = state
                    .running_turns
                    .get(node_id)
                    .map(|handle| handle.started.elapsed());
                let marker = state_marker(node, running_for);
                match line.kind {
                    LineKind::Head => {
                        paint_plain(&mut render_ops, 0, &node.created_display, gutter);
                        paint_plain(&mut render_ops, gutter_width, &rail_prefix, gutter);
                        if row_info.fold == Fold::Leaf {
                            paint_plain(&mut render_ops, fold_column, fold_text, gutter);
                        } else {
                            paint(&mut render_ops, fold_column, fold_text, type_fg);
                        }
                        if let Some(marker) = marker {
                            paint(
                                &mut render_ops,
                                gutter_width - 2,
                                &marker.to_string(),
                                type_fg,
                            );
                        }
                        paint_text(&mut render_ops, type_fg);
                        if let Some(label) = commit_label(node) {
                            let column = pane_width.saturating_sub(display_width(&label));
                            paint(&mut render_ops, column, &label, hash_fg);
                            let slot = self.label_zones.last_mut().expect("pushed above");
                            *slot = Some((column, display_width(&label)));
                        }
                    }
                    LineKind::TitleRest | LineKind::Summary => {
                        let hangs = rows
                            .get(line.row + 1)
                            .is_some_and(|next| next.rails.len() > row_info.rails.len());
                        let continuation = row_continuation_prefix(row_info, hangs);
                        paint_plain(
                            &mut render_ops,
                            gutter_width,
                            &continuation,
                            selected_gutter_fg,
                        );
                        let fg = match line.kind {
                            LineKind::Summary if armed => error_fg,
                            LineKind::Summary => dim_fg,
                            _ => type_fg,
                        };
                        paint_text(&mut render_ops, fg);
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
            .map(|row| {
                (
                    row.target.node().map(|id| id.as_str()).unwrap_or("?"),
                    row.rails.clone(),
                    row.fold,
                )
            })
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
    fn roots_are_oldest_first_and_collapsed_by_default() {
        let (_tmp, tree) = tree_with(&["a1/a2", "b1"]);
        let rows = layout_rows(&tree, &HashSet::new());
        assert_eq!(
            shape(&rows),
            [("a1", vec![], Fold::Collapsed), ("b1", vec![], Fold::Leaf)]
        );
    }

    #[test]
    fn the_newest_root_is_selected_when_nothing_is() {
        let (_tmp, tree) = tree_with(&["a1/a2", "b1/b2"]);
        let rows = layout_rows(&tree, &ids(&["a1", "b1"]));
        assert_eq!(
            ConversationTreeComponent::selected_index(&rows, None, false),
            2
        );
        let known = RowTarget::Node(NodeId::new("a2"));
        assert_eq!(
            ConversationTreeComponent::selected_index(&rows, Some(&known), false),
            1
        );
    }

    fn described_tree(nodes: &[(&str, &str, &str)]) -> (tempfile::TempDir, ConversationTree) {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        for (path, title, summary) in nodes {
            let dir = root.join(path);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("title.md"), title).unwrap();
            if !summary.is_empty() {
                std::fs::write(dir.join("summary.md"), summary).unwrap();
            }
        }
        let tree = ConversationTree::load(&root);
        (tmp, tree)
    }

    fn matched_ids(matches: &[FilterMatch]) -> Vec<&str> {
        matches.iter().map(|found| found.node.as_str()).collect()
    }

    #[test]
    fn the_filter_wants_every_word_in_any_order_across_title_and_summary() {
        let (_tmp, tree) = described_tree(&[
            ("a1", "Queue rows", "Esc takes the prompt back"),
            ("a1/a2", "Queue badge", "Shown in the gutter"),
            ("b1", "Spinner frames", "No queued prompt, no escape"),
        ]);
        let sorted_ids = |query: &str| {
            let mut ids: Vec<String> = filter_nodes(&tree, query)
                .into_iter()
                .map(|found| found.node.as_str().to_string())
                .collect();
            ids.sort();
            ids
        };
        assert_eq!(sorted_ids("esc queue"), ["a1", "b1"]);
        assert_eq!(sorted_ids("queue esc"), ["a1", "b1"]);
        assert_eq!(sorted_ids("queue"), ["a1", "a2", "b1"]);
        // An uppercase letter makes the word case-sensitive.
        assert_eq!(sorted_ids("Queue"), ["a1", "a2"]);
        assert!(sorted_ids("qeue").is_empty());
    }

    #[test]
    fn a_word_ending_the_text_is_found() {
        let (_tmp, tree) = described_tree(&[
            ("a1", "Fix — the queue", ""),
            ("b1", "Fix the queue", ""),
            ("c1", "Queue", "Le café"),
        ]);
        let matches = filter_nodes(&tree, "queue");
        assert_eq!(matched_ids(&matches), ["c1", "b1", "a1"]);
        assert_eq!(matches[2].title, [(12, 17)]);
        assert_eq!(filter_nodes(&tree, "café")[0].summary, [(3, 8)]);
    }

    #[test]
    fn equal_matches_come_newest_first() {
        let (_tmp, tree) = described_tree(&[
            ("a1", "Wrap the title", ""),
            ("a1/a2", "Wrap the title", ""),
            ("b1", "Wrap the title", ""),
        ]);
        for _ in 0..5 {
            assert_eq!(
                matched_ids(&filter_nodes(&tree, "wrap")),
                ["b1", "a2", "a1"]
            );
        }
    }

    #[test]
    fn matches_are_byte_ranges_of_the_title_and_of_the_summary() {
        let (_tmp, tree) =
            described_tree(&[("a1", "Café queue", "Le café: the queue, then the café")]);
        let matches = filter_nodes(&tree, "café queue");
        assert_eq!(
            matches,
            [FilterMatch {
                node: NodeId::new("a1"),
                title: vec![(0, 5), (6, 11)],
                summary: vec![(3, 8), (14, 19), (30, 35)],
            }]
        );
        let title = tree.node(&NodeId::new("a1")).unwrap().display_title();
        assert_eq!(&title[0..5], "Café");
        assert_eq!(&title[6..11], "queue");
    }

    #[test]
    fn a_filter_selects_its_best_match_when_nothing_is_selected() {
        let (_tmp, tree) = described_tree(&[("a1", "Wrap", ""), ("b1", "Wrap", "")]);
        let rows = flat_rows(&filter_nodes(&tree, "wrap"));
        assert_eq!(
            ConversationTreeComponent::selected_index(&rows, None, true),
            0
        );
        assert_eq!(
            shape(&rows),
            [("b1", vec![], Fold::Leaf), ("a1", vec![], Fold::Leaf)]
        );
    }

    #[test]
    fn segments_follow_a_wrapped_line_through_its_offset() {
        let ranges = [(2, 5), (8, 12)];
        assert_eq!(
            segments("abcdefgh", 0, &ranges),
            [("ab", false), ("cde", true), ("fgh", false)]
        );
        assert_eq!(
            segments("efghijkl", 4, &ranges),
            [("e", true), ("fgh", false), ("ijkl", true)]
        );
        assert_eq!(segments("mnop", 12, &ranges), [("mnop", false)]);
        assert_eq!(segments("abc", 0, &[]), [("abc", false)]);
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
        let collapsed_fork = layout_rows(&tree, &HashSet::new());
        assert_eq!(
            shape(&collapsed_fork),
            [
                ("a1", vec![], Fold::Leaf),
                ("a2", vec![false], Fold::Collapsed)
            ]
        );

        let collapsed_branch = layout_rows(&tree, &ids(&["a2"]));
        assert_eq!(
            shape(&collapsed_branch),
            [
                ("a1", vec![], Fold::Leaf),
                ("a2", vec![false], Fold::Expanded),
                ("a3", vec![false, true], Fold::Collapsed),
                ("b3", vec![false, false], Fold::Leaf),
            ]
        );

        let rows = layout_rows(&tree, &ids(&["a2", "a3"]));
        assert_eq!(
            shape(&rows),
            [
                ("a1", vec![], Fold::Leaf),
                ("a2", vec![false], Fold::Expanded),
                ("a3", vec![false, true], Fold::Expanded),
                ("a4", vec![false, true], Fold::Leaf),
                ("b3", vec![false, false], Fold::Leaf),
            ]
        );
        assert_eq!(
            branches(&rows),
            [
                Branch::None,
                Branch::LastStart,
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
        let rows = layout_rows(&tree, &ids(&["a1", "c2"]));
        assert_eq!(
            prefixes(&rows),
            [
                "- ",     // a1: expanded root
                "├── ",   // a2: first branch of a1
                "└── ",   // b2: last branch of a1
                "  ",     // c1: root with nothing to fold but its fork
                "└─- ",   // c2: expanded fork, only child of c1
                "  ├── ", // d3: first branch of c2, arm under c2's fold column
                "  └── ", // e3: last branch of c2
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
                "- ",   // a1: expanded root
                "├─+ ", // a2: collapsed fork starting a branch
                "└── ", // b2: last branch, leaf
            ]
        );
        let continuation = row_continuation_prefix(&rows[1], false);
        assert_eq!(continuation, "│   ");
    }

    #[test]
    fn label_zone_hit_test_is_pane_relative() {
        let pane_width = 120;
        let label = "abc1234 +2";
        let width = display_width(label);
        let column = pane_width - width;
        let zone = (column, width);
        let inside = |col: usize| (zone.0..zone.0 + zone.1).contains(&col);
        assert!(inside(column));
        assert!(inside(pane_width - 1));
        assert!(!inside(column.saturating_sub(1)));
        // A click at the same absolute column in pane 2 (origin 100) hits zone
        // `column - 100`, not `column`.
        let origin_col = 100;
        let abs = column + origin_col;
        assert!(!(zone.0..zone.0 + zone.1).contains(&abs));
        assert!(inside(abs.saturating_sub(origin_col)));
    }

    #[test]
    fn continuation_prefix_hangs_under_an_expanded_fork() {
        let (_tmp, tree) = tree_with(&["a1/a2/b3", "a1/a2/c3"]);
        let rows = layout_rows(&tree, &ids(&["a2"]));
        let continuation = row_continuation_prefix(&rows[1], true);
        assert_eq!(continuation, "  │ ");
    }

    #[test]
    fn a_thread_hides_its_run_until_its_head_is_expanded() {
        let (_tmp, tree) = tree_with(&["a1/a2/a3/a4", "a1/b2"]);
        let collapsed = layout_rows(&tree, &ids(&["a1"]));
        assert_eq!(
            shape(&collapsed),
            [
                ("a1", vec![], Fold::Expanded),
                ("a2", vec![true], Fold::Collapsed),
                ("b2", vec![false], Fold::Leaf),
            ]
        );

        let rows = layout_rows(&tree, &ids(&["a1", "a2"]));
        assert_eq!(
            prefixes(&rows),
            [
                "- ",   // a1: expanded fork
                "├─- ", // a2: expanded thread head
                "│   ", // a3: reply in the thread
                "│   ", // a4
                "└── ", // b2
            ]
        );
    }

    #[test]
    fn the_fork_ending_a_collapsed_thread_stays_visible() {
        let (_tmp, tree) = tree_with(&["a1/a2/a3/a4/b5", "a1/a2/a3/a4/c5", "a1/z2"]);
        let rows = layout_rows(&tree, &ids(&["a1", "a4"]));
        assert_eq!(
            shape(&rows),
            [
                ("a1", vec![], Fold::Expanded),
                ("a2", vec![true], Fold::Collapsed),
                ("a4", vec![true], Fold::Expanded),
                ("b5", vec![true, true], Fold::Leaf),
                ("c5", vec![true, false], Fold::Leaf),
                ("z2", vec![false], Fold::Leaf),
            ]
        );
    }

    #[test]
    fn the_first_row_below_a_root_gets_the_arm() {
        let (_tmp, tree) = tree_with(&["a1/a2/a3/b4", "a1/a2/a3/c4"]);
        let collapsed = layout_rows(&tree, &HashSet::new());
        assert_eq!(prefixes(&collapsed), ["+ ", "└─+ "]);
        let rows = layout_rows(&tree, &ids(&["a1"]));
        assert_eq!(prefixes(&rows), ["- ", "└── ", "  + "]);
    }

    #[test]
    fn a_queued_prompt_gets_a_row_below_the_node_it_will_follow() {
        let (_tmp, tree) = tree_with(&["a1/a2", "b1"]);
        let rows = layout_rows(&tree, &ids(&["a1"]));
        let entry = |window: usize, parent: &str| PendingTurn {
            window,
            parent: Some(NodeId::new(parent)),
        };
        let rows = with_queued(
            rows,
            &[
                entry(0, "a1"),
                entry(1, "a2"),
                entry(2, "a1"),
                PendingTurn {
                    window: 3,
                    parent: None,
                },
            ],
        );

        // Each entry sits after its anchor, in queue order; the root prompt (no parent)
        // has no anchor row, so it is left out.
        assert_eq!(
            rows.iter().map(|row| &row.target).collect::<Vec<_>>(),
            [
                &RowTarget::Node(NodeId::new("a1")),
                &RowTarget::Queued {
                    window: 0,
                    position: 1
                },
                &RowTarget::Queued {
                    window: 2,
                    position: 3
                },
                &RowTarget::Node(NodeId::new("a2")),
                &RowTarget::Queued {
                    window: 1,
                    position: 2
                },
                &RowTarget::Node(NodeId::new("b1")),
            ]
        );
    }

    #[test]
    fn a_queued_prompt_whose_anchor_is_folded_away_has_no_row() {
        let (_tmp, tree) = tree_with(&["a1/a2"]);
        let rows = layout_rows(&tree, &HashSet::new());
        assert_eq!(rows.len(), 1, "a2 is folded into a1");
        let rows = with_queued(
            rows,
            &[PendingTurn {
                window: 0,
                parent: Some(NodeId::new("a2")),
            }],
        );
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn a_position_past_nine_renders_an_ellipsis() {
        assert_eq!(queue_glyph(1), '1');
        assert_eq!(queue_glyph(9), '9');
        assert_eq!(queue_glyph(10), '…');
    }

    #[test]
    fn thread_end_is_the_last_reply_before_a_leaf_or_a_fork() {
        let (_tmp, tree) = tree_with(&["a1/a2/a3", "b1/b2/b3/c4", "b1/b2/b3/d4", "e1"]);
        let end = |head: &str| thread_end(&tree, &NodeId::new(head));
        assert_eq!(end("a1"), Some(NodeId::new("a3")));
        assert_eq!(end("b1"), Some(NodeId::new("b2")));
        assert_eq!(end("b3"), None);
        assert_eq!(end("e1"), None);
    }

    #[test]
    fn folds_on_the_way_are_the_forks_and_thread_heads() {
        let (_tmp, tree) = tree_with(&["a1/a2/a3/b4/b5", "a1/a2/a3/c4"]);
        let folds = folds_on_the_way(&tree, &NodeId::new("b5"));
        let names: Vec<&str> = folds.iter().map(NodeId::as_str).collect();
        assert_eq!(names, ["a1", "a3", "b4"]);
    }
}
