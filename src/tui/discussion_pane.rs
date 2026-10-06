use crate::agent::commands::{self, Expansion};
use crate::agent::turn::{self, TurnRequest};
use crate::agent::{Mode, ModelSpec};
use crate::conversation::{ConversationTree, Metadata, Node, NodeId, NodeStatus, NodeType};
use crate::loader::word_bounds;
use crate::tui::markdown;
use crate::tui::pane_component::drag_modifier_from_mouse;
use crate::tui::text_layout::{display_width, truncate, wrap};
use crate::tui::*;
use std::borrow::Cow;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use unicode_segmentation::UnicodeSegmentation;

const SCROLL_STEP: usize = 3;

type MainTx = mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>;

struct DocLine {
    /// Text the line shows a piece of; `None` for the blank line between two of them.
    part: Option<DocPart>,
    /// Each span's `src` is a byte offset in the text of `part`.
    spans: Vec<markdown::Span>,
    /// Set on code block lines, painted instead of the surrounding background.
    bg: Option<RgbValue>,
}

/// Wrapped prompt, work log and reply of one window, reused until the text or the
/// width changes.
struct DocCache {
    window_id: usize,
    width: usize,
    prompt_len: usize,
    reply_len: usize,
    work_log_len: usize,
    work_log_shown: bool,
    lines: Vec<DocLine>,
}

impl DocCache {
    /// Whether the lines were laid out from the texts `discussion` holds now.
    fn shows(&self, window_id: usize, discussion: &DiscussionState) -> bool {
        self.window_id == window_id
            && self.prompt_len == discussion.prompt.len()
            && self.reply_len == discussion.reply.len()
            && self.work_log_len == discussion.work_log.len()
    }
}

enum DragKind {
    /// A plain drag: a selection, copied when the button is released.
    Select,
    /// A Shift or Ctrl drag: adds to or removes from the highlights `node` had when the
    /// drag began.
    Highlight {
        modifier: DragModifier,
        node: NodeId,
        snapshot: Vec<(usize, usize)>,
    },
}

/// A mouse drag over the document, from the word under the first click.
struct Drag {
    window_id: usize,
    kind: DragKind,
    part: DocPart,
    /// Byte offset under the click, and the word around it.
    anchor: usize,
    word: (usize, usize),
}

pub struct DiscussionComponent {
    id: FlexBoxId,
    window_id: usize,
    cache: Option<DocCache>,
    input: InputLine,
    /// Window the input cursor belongs to; another window's draft resets it.
    input_window: Option<usize>,
    /// Screen row and column of the document area at last render, and its height, used
    /// for mouse event handling.
    doc_origin: (usize, usize),
    doc_rows: usize,
    doc_width: usize,
    drag: Option<Drag>,
}

/// What a turn is asked to do.
pub(crate) enum TurnKind {
    /// A prompt sent in a mode.
    Prompt(Mode),
    /// The built-in `/compact`: summarize the branch so far.
    Compact { instructions: String },
}

/// A turn to start: a new node under `parent`, or a new root.
pub(crate) struct Launch {
    pub parent: Option<NodeId>,
    /// Text stored as the node's `prompt.md`.
    pub prompt: String,
    pub kind: TurnKind,
    /// Custom slash command the prompt was expanded from.
    pub command: Option<String>,
    pub spec: ModelSpec,
    /// An incomplete child of `parent` to run again in place, instead of a new node.
    pub rerun: Option<NodeId>,
}

impl Launch {
    /// The turn of an existing node, to run again as it was sent: same prompt, mode and
    /// command. `prompt` is the node's `prompt.md`.
    fn replay(
        tree: &ConversationTree,
        node: &NodeId,
        prompt: String,
        spec: ModelSpec,
    ) -> Option<Self> {
        let source = tree.node(node)?;
        let kind = match source.meta.node_type {
            Some(NodeType::Compact) => match commands::expand(&prompt, &[]) {
                Expansion::Compact { instructions } => TurnKind::Compact { instructions },
                Expansion::Prompt { .. } => TurnKind::Compact {
                    instructions: String::new(),
                },
            },
            _ => TurnKind::Prompt(node_mode(source)),
        };
        Some(Self {
            parent: source.parent.clone(),
            prompt,
            kind,
            command: source.meta.command.clone(),
            spec,
            rerun: None,
        })
    }
}

/// Checks the guards, creates the node on disk and starts its turn. On refusal nothing
/// is created and the reason is returned.
pub(crate) fn start_turn(
    state: &mut AppState,
    main_tx: MainTx,
    launch: Launch,
) -> Result<NodeId, String> {
    let Launch {
        parent,
        prompt,
        kind,
        command,
        spec,
        rerun,
    } = launch;
    // A compact turn changes nothing, so it runs under the shared lock like feedback.
    let (mode, node_type, compact) = match kind {
        TurnKind::Prompt(Mode::Feedback) => (Mode::Feedback, NodeType::Feedback, None),
        TurnKind::Prompt(Mode::Commit) => (Mode::Commit, NodeType::Commit, None),
        TurnKind::Compact { instructions } => {
            (Mode::Feedback, NodeType::Compact, Some(instructions))
        }
    };
    let endpoint_kind = state
        .agent
        .endpoint(&spec.endpoint)
        .map(|endpoint| endpoint.kind.label());
    let (Some(endpoint_kind), Some(backend)) = (endpoint_kind, state.agent.backend(&spec)) else {
        return Err(format!(
            "endpoint '{}' is not configured or has no token",
            spec.endpoint
        ));
    };
    let parent_status = parent
        .as_ref()
        .and_then(|parent| state.conversations.read().node(parent)?.meta.status);
    match parent_status {
        Some(NodeStatus::Done) | None => {}
        Some(NodeStatus::Running) => return Err("this node is still running".to_string()),
        Some(_) => return Err("this node is not complete; continue from its parent".to_string()),
    }
    let running: Vec<Mode> = state
        .running_turns
        .values()
        .map(|handle| handle.mode)
        .collect();
    if let Some(reason) = lock_refusal(&running) {
        return Err(reason.to_string());
    }
    if crate::git::status_short(&state.root).is_ok_and(|status| !status.trim().is_empty()) {
        return Err(
            "the working tree has uncommitted changes; commit or stash them first".to_string(),
        );
    }

    let head = state.repo.as_ref().and_then(crate::git::head_id);
    let now = now_secs();
    let meta = Metadata {
        node_type: Some(node_type),
        endpoint: Some(spec.endpoint.clone()),
        kind: Some(endpoint_kind.to_string()),
        model: Some(spec.model.clone()),
        effort: Some(spec.effort.as_str().to_string()),
        status: Some(NodeStatus::Running),
        head_before: head.clone(),
        command,
        ..Default::default()
    };
    let (node, lineage, context) = {
        let mut tree = state.conversations.write();
        let node = match rerun {
            Some(node) => {
                tree.restart_node(&node, |previous| {
                    *previous = Metadata {
                        created: previous.created.take(),
                        favorite: previous.favorite,
                        ..meta
                    };
                })
                .map_err(|e| format!("could not restart the node: {e}"))?;
                state.discussion_highlights.remove(&node);
                node
            }
            None => tree
                .create_node(parent.as_ref(), now, meta, &prompt)
                .map_err(|e| format!("could not create the node: {e}"))?,
        };
        let context = tree
            .turn_context(&node)
            .ok_or("the new node has no directory")?;
        (node.clone(), tree.lineage(&node), context)
    };
    let session = lineage.first().unwrap_or(&node).as_str().to_string();
    state.tree_view.expanded.extend(lineage);
    state.tree_view.selected = Some(node.clone());

    let summary_backend = state
        .agent
        .summary_model
        .as_ref()
        .and_then(|summary| state.agent.backend(summary));
    let request = TurnRequest {
        mode,
        node: node.clone(),
        node_dir: context.node_dir.clone(),
        ancestor_dirs: context.ancestor_dirs,
        parent_head: context.parent_head,
        head,
        root: state.root.clone(),
        repo: state.repo.clone(),
        prompt: prompt.clone(),
        date: utc_date(now),
        backend,
        summary_backend,
        session,
        parent_session: context.parent_session,
        compact,
        compacted_parent: context.compacted_parent,
        tree: Arc::clone(&state.conversations),
    };
    let handle = turn::spawn(request, main_tx.clone());
    state.running_turns.insert(node.clone(), handle);
    state.mark_session_dirty();
    Ok(node)
}

/// The model a node was run with, when its metadata names one completely.
pub(crate) fn node_model(node: &Node) -> Option<ModelSpec> {
    ModelSpec::from_parts(
        node.meta.endpoint.as_deref()?,
        node.meta.model.as_deref()?,
        node.meta.effort.as_deref()?,
    )
}

/// The mode a node was sent in; a compact node counts as feedback.
fn node_mode(node: &Node) -> Mode {
    match node.meta.node_type {
        Some(NodeType::Commit) => Mode::Commit,
        _ => Mode::Feedback,
    }
}

fn input_label(mode: Mode) -> String {
    format!("{}> ", mode.label())
}

/// Share of the theme's warning color in the background of highlighted text.
const MARKER_STRENGTH: f32 = 0.4;

/// Background of highlighted text: the theme's warning color laid over the pane
/// background, since `ui.selection` barely differs from it in many themes.
fn marker_bg(theme: &HelixTheme, pane_bg: RgbValue) -> RgbValue {
    let Some(accent) = theme.color_for_scope("warning") else {
        return theme.ui_bg("ui.selection").unwrap_or(pane_bg);
    };
    let mix = |bg: u8, accent: u8| {
        (f32::from(bg) + (f32::from(accent) - f32::from(bg)) * MARKER_STRENGTH).round() as u8
    };
    RgbValue::from_u8(
        mix(pane_bg.red, accent.red),
        mix(pane_bg.green, accent.green),
        mix(pane_bg.blue, accent.blue),
    )
}

/// Paint style for one markdown span: its fg over `bg`, with the span's attributes.
fn span_style(span: &markdown::Span, bg: RgbValue) -> TuiStyle {
    let mut style = new_style!(
        color_fg: {TuiColor::from(span.fg)} color_bg: {TuiColor::from(bg)}
    );
    use r3bl_tui::tui_style_attrib;
    if span.bold {
        style.attribs.bold = Some(tui_style_attrib::Bold);
    }
    if span.italic {
        style.attribs.italic = Some(tui_style_attrib::Italic);
    }
    if span.underline {
        style.attribs.underline = Some(tui_style_attrib::Underline);
    }
    if span.strikethrough {
        style.attribs.strikethrough = Some(tui_style_attrib::Strikethrough);
    }
    style
}

/// Why a prompt in `mode` cannot start while `running` turns are in progress: turns
/// run one at a time, since a feedback turn experiments freely in the working tree
/// and a commit turn must have it clean and to itself.
fn lock_refusal(running: &[Mode]) -> Option<&'static str> {
    (!running.is_empty()).then_some("another turn is running; wait for it to finish")
}

/// The text of `part`, as the offsets of a [`DocLine`] index it. The work log entries
/// are joined by a newline.
fn part_text(discussion: &DiscussionState, part: DocPart) -> Cow<'_, str> {
    match part {
        DocPart::Prompt => Cow::Borrowed(&discussion.prompt),
        DocPart::WorkLog => Cow::Owned(discussion.work_log.join("\n")),
        DocPart::Reply => Cow::Borrowed(&discussion.reply),
    }
}

/// Source offset of the cell at `col` of a row. A cell the source has no place for
/// (marker, padding, border) or a column past the text stands for the nearest cell
/// before it, or else after it.
fn offset_at(spans: &[markdown::Span], col: usize) -> Option<usize> {
    let mut found = None;
    let mut x = 0;
    for span in spans {
        for (at, grapheme) in span.text.grapheme_indices(true) {
            if let Some(src) = span.src {
                if x > col {
                    return found.or(Some(src + at));
                }
                found = Some(src + at);
            }
            x += display_width(grapheme);
        }
    }
    found
}

/// The text and source offset under `row` and `col` of the document. A row without
/// text (blank line, rule) stands for the end of the text above it, or else the start
/// of the text below.
fn locate(lines: &[DocLine], row: usize, col: usize) -> Option<(DocPart, usize)> {
    let last = lines.len().checked_sub(1)?;
    let (row, col) = if row > last {
        (last, usize::MAX)
    } else {
        (row, col)
    };
    let on = |line: &DocLine, col: usize| Some((line.part?, offset_at(&line.spans, col)?));
    on(&lines[row], col)
        .or_else(|| {
            lines[..row]
                .iter()
                .rev()
                .find_map(|line| on(line, usize::MAX))
        })
        .or_else(|| lines[row + 1..].iter().find_map(|line| on(line, 0)))
}

/// Range covered by a drag from `word`, clicked at `anchor`, to the word under
/// `cursor`: whole words at both ends, whatever the rows they are on.
fn drag_range(text: &str, anchor: usize, word: (usize, usize), cursor: usize) -> (usize, usize) {
    let (start, end) = word_bounds(text, cursor);
    if cursor >= anchor {
        (word.0, end.max(word.1))
    } else {
        (start.min(word.0), word.1)
    }
}

/// `ranges` with `lo..hi` added, overlapping and touching ranges merged.
fn add_range(ranges: &[(usize, usize)], lo: usize, hi: usize) -> Vec<(usize, usize)> {
    let mut all = ranges.to_vec();
    all.push((lo, hi));
    all.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(all.len());
    for (start, end) in all {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// `ranges` with `lo..hi` removed, splitting the ranges it cuts through.
fn remove_range(ranges: &[(usize, usize)], lo: usize, hi: usize) -> Vec<(usize, usize)> {
    let mut kept = Vec::with_capacity(ranges.len() + 1);
    for &(start, end) in ranges {
        if start < lo.min(end) {
            kept.push((start, lo.min(end)));
        }
        if hi.max(start) < end {
            kept.push((hi.max(start), end));
        }
    }
    kept
}

/// A run of cells of one span that are all marked (selected or highlighted) or all not.
struct Piece<'a> {
    text: &'a str,
    span: &'a markdown::Span,
    marked: bool,
}

/// Cuts the spans of a row where `marked` source ranges begin and end. Cells the source
/// has no place for are marked when the text on both sides of them is.
fn row_pieces<'a>(spans: &'a [markdown::Span], marked: &[(usize, usize)]) -> Vec<Piece<'a>> {
    let mut runs: Vec<(&str, &markdown::Span, Option<bool>)> = Vec::with_capacity(spans.len());
    for span in spans {
        let Some(src) = span.src.filter(|_| !marked.is_empty()) else {
            runs.push((&span.text, span, span.src.map(|_| false)));
            continue;
        };
        let mut start = 0;
        let mut state = None;
        for (at, _) in span.text.grapheme_indices(true) {
            let hit = marked
                .iter()
                .any(|&(lo, hi)| (lo..hi).contains(&(src + at)));
            if state.is_some_and(|state| state != hit) {
                runs.push((&span.text[start..at], span, state));
                start = at;
            }
            state = Some(hit);
        }
        if start < span.text.len() {
            runs.push((&span.text[start..], span, state));
        }
    }
    let states: Vec<Option<bool>> = runs.iter().map(|run| run.2).collect();
    runs.into_iter()
        .enumerate()
        .map(|(i, (text, span, state))| Piece {
            text,
            span,
            marked: state.unwrap_or_else(|| {
                let before = states[..i].iter().rev().find_map(|state| *state);
                let after = states[i + 1..].iter().find_map(|state| *state);
                before == Some(true) && after == Some(true)
            }),
        })
        .collect()
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

fn utc_date(secs: i64) -> String {
    let (year, month, day) = crate::git::civil_from_days(secs.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

impl DiscussionComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self {
            id,
            window_id: 0,
            cache: None,
            input: InputLine::new(),
            input_window: None,
            doc_origin: (0, 0),
            doc_rows: 0,
            doc_width: 0,
            drag: None,
        }
    }

    pub fn set_window_id(&mut self, window_id: usize) {
        self.window_id = window_id;
    }

    fn in_doc(&self, pos: Pos) -> bool {
        let (origin_row, origin_col) = self.doc_origin;
        let row = pos.row_index.as_usize();
        let col = pos.col_index.as_usize();
        (origin_row..origin_row + self.doc_rows).contains(&row)
            && (origin_col..origin_col + self.doc_width).contains(&col)
    }

    /// The text and source offset under a screen position, clamped to the document area.
    fn locate(
        &self,
        state: &AppState,
        discussion: &DiscussionState,
        window_id: usize,
        pos: Pos,
    ) -> Option<(DocPart, usize)> {
        let cache = self
            .cache
            .as_ref()
            .filter(|cache| cache.shows(window_id, discussion))?;
        let (origin_row, origin_col) = self.doc_origin;
        let row = pos
            .row_index
            .as_usize()
            .saturating_sub(origin_row)
            .min(self.doc_rows.saturating_sub(1));
        let col = pos.col_index.as_usize().saturating_sub(origin_col);
        let scroll = state
            .pane_manager
            .window_scroll(&Window::Discussion(window_id));
        locate(&cache.lines, scroll + row, col)
    }

    /// Starts a drag on the word under the mouse: a selection, or with Shift or Ctrl a
    /// change to the highlights of the node shown.
    fn begin_drag(&mut self, state: &mut AppState, mouse: &MouseInput) {
        let id = self.window_id;
        let Some(discussion) = state.discussions.get(&id) else {
            return;
        };
        let Some((part, anchor)) = self.locate(state, discussion, id, mouse.pos) else {
            return;
        };
        let text = part_text(discussion, part);
        if anchor >= text.len() || !text.is_char_boundary(anchor) {
            return;
        }
        let word = word_bounds(&text, anchor);
        let kind = match (drag_modifier_from_mouse(mouse), &discussion.node) {
            (Some(modifier), Some(node)) => {
                let refusal = if part == DocPart::WorkLog {
                    Some("the work log cannot be highlighted")
                } else if state.running_turns.contains_key(node) {
                    Some("highlights are available once the turn is done")
                } else {
                    None
                };
                if let Some(reason) = refusal {
                    if let Some(discussion) = state.discussions.get_mut(&id) {
                        discussion.notice = Some(reason.to_string());
                    }
                    return;
                }
                DragKind::Highlight {
                    modifier,
                    node: node.clone(),
                    snapshot: state
                        .discussion_highlights
                        .get(node)
                        .map(|highlights| highlights.part(part).to_vec())
                        .unwrap_or_default(),
                }
            }
            _ => {
                let point = |offset| SelPoint::Discussion { part, offset };
                state.text_selection = Some(TextSelection {
                    window: Window::Discussion(id),
                    start: point(word.0),
                    end: point(word.0),
                    click_anchor: Some(point(anchor)),
                    click_word: Some((point(word.0), point(word.1))),
                    active: true,
                });
                DragKind::Select
            }
        };
        self.drag = Some(Drag {
            window_id: id,
            kind,
            part,
            anchor,
            word,
        });
        state.mouse_drag_active = true;
    }

    /// Extends the drag to the word under the mouse. A drag never leaves the text it
    /// began in: past it, it reaches that text's start or end.
    fn update_drag(&mut self, state: &mut AppState, pos: Pos) {
        let Some(drag) = &self.drag else {
            return;
        };
        let Some(discussion) = state.discussions.get(&drag.window_id) else {
            return;
        };
        let text = part_text(discussion, drag.part);
        let (lo, hi) = match self.locate(state, discussion, drag.window_id, pos) {
            Some((part, cursor)) if part == drag.part => {
                if cursor >= text.len() || !text.is_char_boundary(cursor) {
                    return;
                }
                drag_range(&text, drag.anchor, drag.word, cursor)
            }
            Some((part, _)) if part < drag.part => (0, drag.word.1),
            Some(_) => (drag.word.0, text.len()),
            None => return,
        };
        match &drag.kind {
            DragKind::Select => {
                if let Some(selection) = &mut state.text_selection {
                    let part = drag.part;
                    selection.start = SelPoint::Discussion { part, offset: lo };
                    selection.end = SelPoint::Discussion { part, offset: hi };
                }
            }
            DragKind::Highlight {
                modifier,
                node,
                snapshot,
            } => {
                let ranges = match modifier {
                    DragModifier::Shift => add_range(snapshot, lo, hi),
                    DragModifier::Ctrl => remove_range(snapshot, lo, hi),
                };
                let highlights = state.discussion_highlights.entry(node.clone()).or_default();
                let changed = highlights
                    .part_mut(drag.part)
                    .filter(|current| **current != ranges)
                    .map(|current| *current = ranges)
                    .is_some();
                if highlights.is_empty() {
                    state.discussion_highlights.remove(node);
                }
                if changed {
                    state.mark_session_dirty();
                }
            }
        }
    }

    /// Ends the drag; a selection is copied to the clipboard as its source text.
    fn end_drag(&mut self, state: &mut AppState) {
        if let Some(Drag {
            window_id,
            kind: DragKind::Select,
            ..
        }) = &self.drag
            && let Some(TextSelection {
                start: SelPoint::Discussion { part, offset: lo },
                end: SelPoint::Discussion { offset: hi, .. },
                ..
            }) = &state.text_selection
            && let Some(discussion) = state.discussions.get(window_id)
        {
            let text = part_text(discussion, *part);
            if let Some(selected) = text.get(*lo..*hi).filter(|selected| !selected.is_empty()) {
                let mut clipboard = r3bl_tui::SystemClipboard;
                let _ = clipboard.try_to_put_content_into_clipboard(selected.to_string());
            }
        }
        self.cancel_drag(state);
    }

    /// Drops the drag in progress, if any, and its selection.
    pub(crate) fn cancel_drag(&mut self, state: &mut AppState) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        if matches!(drag.kind, DragKind::Select) {
            state.text_selection = None;
        }
        state.mouse_drag_active = false;
    }

    fn show(state: &mut AppState, id: usize) {
        let window = Window::Discussion(id);
        let old = state.pane_manager.focused_window;
        state.pane_manager.push_window(window);
        state.pane_manager.focused_window = Some(window);
        crate::tui::app::notify_terminal_focus_change(state, old, Some(window));
        state.mark_session_dirty();
    }

    /// Opens `node` like [`Self::open`], then runs its turn again in place when it is
    /// incomplete: failed, cancelled or interrupted. It is run as it was sent, with the
    /// model it was sent to. A refusal is shown in the window, over the old reply.
    pub(crate) fn open_and_recover(state: &mut AppState, main_tx: MainTx, node: NodeId) {
        Self::open(state, main_tx.clone(), node.clone());
        let launch = {
            let tree = state.conversations.read();
            let Some(shown) = tree.node(&node) else {
                return;
            };
            let incomplete = matches!(
                shown.meta.status,
                Some(NodeStatus::Failed | NodeStatus::Cancelled)
            );
            if !incomplete || !shown.children.is_empty() {
                return;
            }
            let prompt = tree
                .node_dir(&node)
                .map(|dir| crate::conversation::read_prompt_and_reply(&dir).0)
                .unwrap_or_default();
            node_model(shown)
                .and_then(|spec| Launch::replay(&tree, &node, prompt, spec))
                .map(|launch| Launch {
                    rerun: Some(node.clone()),
                    ..launch
                })
                .ok_or("this node does not record the model it was sent to")
        };
        let started = launch
            .map_err(str::to_string)
            .and_then(|launch| start_turn(state, main_tx, launch));
        for discussion in state.discussions.values_mut() {
            if discussion.node.as_ref() != Some(&node) {
                continue;
            }
            match &started {
                Ok(_) => {
                    discussion.reply.clear();
                    discussion.work_log.clear();
                    discussion.notice = None;
                    discussion.follow = true;
                }
                Err(reason) => discussion.notice = Some(reason.clone()),
            }
        }
    }

    /// Runs the prompt of `node` again as a new sibling, with the model `spec`, and
    /// opens it. A refusal is reported in the tree.
    pub(crate) fn duplicate(state: &mut AppState, main_tx: MainTx, node: &NodeId, spec: ModelSpec) {
        let launch = {
            let tree = state.conversations.read();
            let prompt = tree
                .node_dir(node)
                .map(|dir| crate::conversation::read_prompt_and_reply(&dir).0)
                .unwrap_or_default();
            Launch::replay(&tree, node, prompt, spec)
        };
        let Some(launch) = launch else {
            return;
        };
        match start_turn(state, main_tx.clone(), launch) {
            Ok(sibling) => Self::open(state, main_tx, sibling),
            Err(reason) => state.tree_view.notice = Some(reason),
        }
    }

    /// Opens a window on the parent of `node` with the input prefilled with the node's
    /// prompt, so that an edited version can be sent as a new sibling.
    pub(crate) fn open_edit(state: &mut AppState, main_tx: MainTx, node: &NodeId) {
        let (parent, draft, model, mode) = {
            let tree = state.conversations.read();
            let Some(source) = tree.node(node) else {
                return;
            };
            let draft = tree
                .node_dir(node)
                .map(|dir| crate::conversation::read_prompt_and_reply(&dir).0)
                .unwrap_or_default();
            (
                source.parent.clone(),
                draft,
                node_model(source),
                node_mode(source),
            )
        };
        let id = state.next_discussion_id;
        state.next_discussion_id += 1;
        state.discussions.insert(
            id,
            DiscussionState {
                loaded: parent.is_none(),
                node: parent,
                draft,
                model,
                mode,
                ..Default::default()
            },
        );
        Self::load_content(state, main_tx, id);
        Self::show(state, id);
    }

    /// Focuses the window already showing `node`, or opens a new one and loads the
    /// node's prompt and reply from disk. The window starts with the node's model.
    pub(crate) fn open(state: &mut AppState, main_tx: MainTx, node: NodeId) {
        let existing = state
            .discussions
            .iter()
            .find(|(_, discussion)| discussion.node.as_ref() == Some(&node))
            .map(|(id, _)| *id);
        let id = existing.unwrap_or_else(|| {
            let id = state.next_discussion_id;
            state.next_discussion_id += 1;
            let (model, mode) = {
                let tree = state.conversations.read();
                let shown = tree.node(&node);
                (
                    shown.and_then(node_model),
                    shown.map(node_mode).unwrap_or_default(),
                )
            };
            state.discussions.insert(
                id,
                DiscussionState {
                    node: Some(node),
                    model,
                    mode,
                    ..Default::default()
                },
            );
            Self::load_content(state, main_tx, id);
            id
        });
        Self::show(state, id);
    }

    /// Opens a window for a new conversation, offering the model last picked.
    pub(crate) fn open_new_root(state: &mut AppState) {
        let id = state.next_discussion_id;
        state.next_discussion_id += 1;
        state.discussions.insert(
            id,
            DiscussionState {
                loaded: true,
                model: state.last_picked_model.clone(),
                ..Default::default()
            },
        );
        Self::show(state, id);
    }

    /// Reads the prompt and reply of window `id`'s node off the UI thread.
    pub(crate) fn load_content(state: &AppState, main_tx: MainTx, id: usize) {
        let Some(node) = state.discussions.get(&id).and_then(|d| d.node.clone()) else {
            return;
        };
        let Some(dir) = state.conversations.read().node_dir(&node) else {
            return;
        };
        tokio::task::spawn_blocking(move || {
            let (prompt, reply) = crate::conversation::read_prompt_and_reply(&dir);
            let work_log = crate::conversation::read_work_log(&dir);
            send_signal!(
                main_tx,
                TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::NodeContentLoaded {
                    window_id: id,
                    node,
                    prompt,
                    reply,
                    work_log,
                })
            );
        });
    }

    /// Sends the draft of window `id` as a new node below the node it shows. A refused
    /// send keeps the draft and explains itself in the window's notice.
    fn send(state: &mut AppState, main_tx: MainTx, id: usize) {
        let Some(discussion) = state.discussions.get(&id) else {
            return;
        };
        if discussion.draft.trim().is_empty() {
            return;
        }
        let Some(spec) = discussion.model.clone() else {
            ModelPickerComponent::open(state, main_tx, ModelPickerPurpose::Window(id));
            return;
        };
        let typed = discussion.draft.clone();
        let (prompt, kind, command) = match commands::expand(&typed, &state.agent.commands) {
            Expansion::Compact { instructions } => {
                (typed, TurnKind::Compact { instructions }, None)
            }
            Expansion::Prompt { text, command } => {
                (text, TurnKind::Prompt(discussion.mode), command)
            }
        };
        let launch = Launch {
            parent: discussion.node.clone(),
            prompt: prompt.clone(),
            kind,
            command,
            spec,
            rerun: None,
        };
        let node = match start_turn(state, main_tx, launch) {
            Ok(node) => node,
            Err(reason) => {
                if let Some(discussion) = state.discussions.get_mut(&id) {
                    discussion.notice = Some(reason);
                }
                return;
            }
        };
        let Some(discussion) = state.discussions.get_mut(&id) else {
            return;
        };
        discussion.node = Some(node);
        discussion.prompt = prompt;
        discussion.reply.clear();
        discussion.work_log.clear();
        discussion.show_work_log = None;
        discussion.draft.clear();
        discussion.loaded = true;
        discussion.notice = None;
        discussion.follow = true;
        state
            .pane_manager
            .set_window_scroll(&Window::Discussion(id), 0);
    }

    fn doc_lines(
        &mut self,
        discussion: &DiscussionState,
        work_log_shown: bool,
        theme: &HelixTheme,
        width: usize,
    ) -> &[DocLine] {
        let fresh = self.cache.as_ref().is_some_and(|cache| {
            cache.window_id == self.window_id
                && cache.width == width
                && cache.prompt_len == discussion.prompt.len()
                && cache.reply_len == discussion.reply.len()
                && cache.work_log_len == discussion.work_log.len()
                && cache.work_log_shown == work_log_shown
        });
        if !fresh {
            let text_fg = theme
                .ui_fg("ui.text")
                .unwrap_or(RgbValue::from_u8(170, 170, 200));
            let dim_fg = theme.color_for_scope("comment").unwrap_or(text_fg);
            let mut lines = Vec::new();
            if discussion.node.is_some() {
                for (start, text) in wrap(&discussion.prompt, width) {
                    lines.push(DocLine {
                        part: Some(DocPart::Prompt),
                        spans: vec![markdown::Span {
                            src: Some(start),
                            ..markdown::Span::plain(text, text_fg)
                        }],
                        bg: None,
                    });
                }
                lines.push(DocLine {
                    part: None,
                    spans: Vec::new(),
                    bg: None,
                });
                if work_log_shown && !discussion.work_log.is_empty() {
                    let mut base = 0;
                    for entry in &discussion.work_log {
                        for (start, text) in wrap(entry, width) {
                            lines.push(DocLine {
                                part: Some(DocPart::WorkLog),
                                spans: vec![markdown::Span {
                                    src: Some(base + start),
                                    ..markdown::Span::plain(text, dim_fg)
                                }],
                                bg: None,
                            });
                        }
                        base += entry.len() + 1;
                    }
                    lines.push(DocLine {
                        part: None,
                        spans: Vec::new(),
                        bg: None,
                    });
                }
                for md in markdown::render(&discussion.reply, theme, width) {
                    lines.push(DocLine {
                        part: Some(DocPart::Reply),
                        spans: md.spans,
                        bg: md.bg,
                    });
                }
            }
            self.cache = Some(DocCache {
                window_id: self.window_id,
                width,
                prompt_len: discussion.prompt.len(),
                reply_len: discussion.reply.len(),
                work_log_len: discussion.work_log.len(),
                work_log_shown,
                lines,
            });
        }
        self.cache.as_ref().map_or(&[], |cache| &cache.lines)
    }

    pub(crate) fn title(state: &AppState, window_id: usize) -> String {
        state
            .discussions
            .get(&window_id)
            .and_then(|discussion| discussion.node.as_ref())
            .and_then(|node| {
                let tree = state.conversations.read();
                tree.node(node).map(|node| node.display_title().to_string())
            })
            .unwrap_or_else(|| "New conversation".to_string())
    }

    pub fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        theme: &HelixTheme,
        state: &AppState,
        window_id: usize,
    ) -> usize {
        let title = Self::title(state, window_id);
        let model = state
            .discussions
            .get(&window_id)
            .and_then(|discussion| discussion.model.as_ref())
            .map_or_else(|| "no model (Ctrl+P)".to_string(), ToString::to_string);

        let pane_width = pane_box.style_adjusted_bounds_size.col_width.as_usize();
        let label = format!(" {model} ");
        let label_width = display_width(&label);
        let title_box = FlexBox {
            style_adjusted_bounds_size: width(pane_width.saturating_sub(label_width) as u16)
                + pane_box.style_adjusted_bounds_size.row_height,
            ..*pane_box
        };
        render_pane_title(ops, pane_box, "", false, theme, focused);
        render_pane_title(ops, &title_box, &title, false, theme, focused);
        if label_width < pane_width {
            let (bg, fg) = title_bar_colors(focused, theme);
            let style = new_style!(color_fg: {TuiColor::from(fg)} color_bg: {TuiColor::from(bg)});
            *ops += RenderOpCommon::MoveCursorPositionRelTo(
                pane_box.style_adjusted_origin_pos,
                col((pane_width - label_width) as u16) + row(0),
            );
            *ops += RenderOpCommon::ApplyColors(Some(style));
            *ops += RenderOpIR::PaintTextWithAttributes(label.into(), Some(style));
        }
        1
    }
}

/// One-line description of a node's outcome, and whether it reports a failure.
fn status_text(node: &Node) -> (String, bool) {
    let kind = match node.meta.node_type {
        Some(NodeType::Feedback) => "feedback",
        Some(NodeType::Commit) => "commit",
        Some(NodeType::Compact) => "compact",
        None => "",
    };
    let (mut text, failed) = match node.meta.status {
        Some(NodeStatus::Failed) => {
            let error = node.meta.error.as_deref().unwrap_or("unknown error");
            (format!("failed: {error}"), true)
        }
        Some(NodeStatus::Cancelled) => ("cancelled".to_string(), true),
        Some(NodeStatus::Running) => ("running".to_string(), false),
        Some(NodeStatus::Done) => ("done".to_string(), false),
        None => (String::new(), false),
    };
    let mut push = |part: &str| {
        if !part.is_empty() {
            if !text.is_empty() {
                text.push_str(" · ");
            }
            text.push_str(part);
        }
    };
    push(kind);
    let hashes: Vec<String> = node
        .meta
        .commits
        .iter()
        .map(|hash| hash.chars().take(7).collect())
        .collect();
    push(&hashes.join(" "));
    if node.meta.stash.is_some() {
        push("uncommitted changes stashed");
    }
    if node.meta.scratch_reset {
        push("scratch discarded");
    }
    if let Some(tokens) = node.meta.tokens_in {
        push(&format!("{tokens} tokens in"));
    }
    (text, failed)
}

/// Whether the work log of the node shown is displayed: as toggled, otherwise only
/// until the node is done and has a reply.
fn work_log_shown(tree: &ConversationTree, discussion: &DiscussionState) -> bool {
    discussion.show_work_log.unwrap_or_else(|| {
        let done = discussion
            .node
            .as_ref()
            .and_then(|node| tree.node(node))
            .is_some_and(|node| node.meta.status == Some(NodeStatus::Done));
        !done || discussion.reply.trim().is_empty()
    })
}

/// Status line of a window: a refusal, the progress of a running turn, or the outcome
/// of the node shown. The flag asks for the error color.
fn status_line(state: &AppState, discussion: &DiscussionState) -> (String, bool) {
    if let Some(notice) = &discussion.notice {
        return (notice.clone(), true);
    }
    if !discussion.loaded {
        return ("Loading…".to_string(), false);
    }
    let Some(node) = &discussion.node else {
        return (
            "Enter:Send  Shift+Enter:New line  Shift+Tab:Mode".to_string(),
            false,
        );
    };
    if let Some(handle) = state.running_turns.get(node) {
        let live = handle.live.lock();
        let mut text = format!("running {}s", handle.started.elapsed().as_secs());
        if let Some(activity) = &live.activity {
            text.push_str(" · ");
            text.push_str(activity);
        }
        if let Some(tokens) = live.tokens_in {
            text.push_str(&format!(" · {tokens} tokens in"));
        }
        text.push_str(" · Ctrl+C:Cancel");
        return (text, false);
    }
    let (mut text, failed) = state
        .conversations
        .read()
        .node(node)
        .map(status_text)
        .unwrap_or_default();
    if !discussion.work_log.is_empty() {
        if !text.is_empty() {
            text.push_str(" · ");
        }
        text.push_str("Ctrl+O:Work log");
    }
    (text, failed)
}

impl Component<AppState, AppSignal> for DiscussionComponent {
    fn reset(&mut self) {
        self.cache = None;
        self.input_window = None;
        self.drag = None;
    }

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
        let id = self.window_id;
        let window = Window::Discussion(id);
        if self.drag.as_ref().is_some_and(|drag| drag.window_id != id) {
            self.cancel_drag(state);
        }
        let Some(discussion) = state.discussions.get_mut(&id) else {
            return Ok(EventPropagation::Propagate);
        };
        if self.input_window != Some(id) {
            self.input.set_cursor_end(&discussion.draft);
            self.input_window = Some(id);
        }
        let running = discussion
            .node
            .as_ref()
            .and_then(|node| state.running_turns.get(node));
        let scroll = state.pane_manager.window_scroll(&window);
        let page = state.pane_manager.window_page_size(&window).max(1);

        let target = match &input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Enter),
            }) => {
                Self::send(state, main_tx, id);
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::BackTab),
            }) => {
                discussion.mode = discussion.mode.toggled();
                discussion.notice = None;
                state.mark_session_dirty();
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('p'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => {
                ModelPickerComponent::open(state, main_tx, ModelPickerPurpose::Window(id));
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('c'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) if running.is_some() => {
                if let Some(handle) = running {
                    handle.cancel();
                }
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('o'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => {
                let shown = work_log_shown(&state.conversations.read(), discussion);
                discussion.show_work_log = Some(!shown);
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::PageUp),
            }) => scroll.saturating_sub(page),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::PageDown),
            }) => scroll + page,
            InputEvent::Mouse(
                mouse @ MouseInput {
                    kind: MouseInputKind::MouseDown(Button::Left),
                    ..
                },
            ) if self.in_doc(mouse.pos) => {
                self.begin_drag(state, mouse);
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::MouseDrag(Button::Left),
                pos,
                ..
            }) if self.drag.is_some() => {
                self.update_drag(state, *pos);
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::MouseUp(Button::Left),
                ..
            }) if self.drag.is_some() => {
                self.end_drag(state);
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::ScrollUp,
                ..
            }) => scroll.saturating_sub(SCROLL_STEP),
            InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::ScrollDown,
                ..
            }) => scroll + SCROLL_STEP,
            _ => {
                if self.input.handle_key(&input_event, &mut discussion.draft) {
                    discussion.notice = None;
                    state.mark_session_dirty();
                    return Ok(EventPropagation::ConsumedRender);
                }
                return Ok(EventPropagation::Propagate);
            }
        };
        discussion.follow = false;
        state.pane_manager.set_window_scroll(&window, target);
        state.pane_manager.clamp_scroll(&window);
        Ok(EventPropagation::ConsumedRender)
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        current_box: FlexBox,
        _surface_bounds: SurfaceBounds,
        has_focus: &mut HasFocus,
    ) -> CommonResult {
        throws!({
            let origin = current_box.style_adjusted_origin_pos;
            let bounds = current_box.style_adjusted_bounds_size;
            let total_rows = bounds.row_height.as_usize();
            let pane_width = bounds.col_width.as_usize();
            let window = Window::Discussion(self.window_id);
            let focused = has_focus.get_id() == Some(self.id);
            let state = &mut global_data.state;

            // A running turn's reply lives in its handle; mirror it while it grows.
            if let Some(discussion) = state.discussions.get_mut(&self.window_id)
                && let Some(handle) = discussion
                    .node
                    .as_ref()
                    .and_then(|node| state.running_turns.get(node))
            {
                let live = handle.live.lock();
                if live.reply.len() != discussion.reply.len()
                    || live.work_log.len() != discussion.work_log.len()
                {
                    discussion.reply.clone_from(&live.reply);
                    discussion.work_log.clone_from(&live.work_log);
                }
            }
            let Some(discussion) = state.discussions.get(&self.window_id) else {
                return Ok(());
            };

            let theme = &state.theme;
            let pane_bg = theme.ui_bg("ui.background").unwrap_or(DEFAULT_BG);
            let prompt_bg = theme
                .ui_bg("ui.cursorline")
                .or_else(|| theme.ui_bg("ui.statusline"))
                .unwrap_or(DEFAULT_STATUSLINE_BG);
            let text_fg = theme
                .ui_fg("ui.text")
                .unwrap_or(RgbValue::from_u8(170, 170, 200));
            let status_fg = theme.color_for_scope("comment").unwrap_or(text_fg);
            let error_fg = theme.ui_fg("error").unwrap_or(DEFAULT_ERROR_FG);
            let (input_bg, input_fg) = title_bar_colors(focused, theme);
            // `ui.selection`, the regular color of the file preview.
            let selection_bg = theme
                .ui_bg("ui.selection")
                .unwrap_or(RgbValue::from_u8(50, 50, 90));
            let marker = marker_bg(theme, pane_bg);

            let (status, alert) = status_line(state, discussion);
            let mode = discussion.mode;
            let label = input_label(mode);
            let input_rows = InputLine::height(
                &discussion.draft,
                &label,
                pane_width as u16,
                (total_rows / 2).max(1),
            )
            .min(total_rows);
            let status_rows = usize::from(total_rows > input_rows);
            let doc_rows = total_rows - input_rows - status_rows;
            let follow = discussion.follow;

            if self.input_window != Some(self.window_id) {
                self.input.set_cursor_end(&discussion.draft);
                self.input_window = Some(self.window_id);
            }
            let mut input_ops = RenderOpIRVec::new();
            let input_origin = origin + height((doc_rows + status_rows) as u16);
            let input_style = new_style!(
                color_fg: {TuiColor::from(input_fg)} color_bg: {TuiColor::from(input_bg)}
            );
            for row_offset in 0..input_rows {
                input_ops += RenderOpCommon::MoveCursorPositionRelTo(
                    input_origin,
                    col(0) + row(row_offset as u16),
                );
                input_ops += RenderOpCommon::ApplyColors(Some(input_style));
                input_ops += RenderOpIR::PaintTextWithAttributes(
                    " ".repeat(pane_width).into(),
                    Some(input_style),
                );
            }
            self.input.render(
                &mut input_ops,
                &discussion.draft,
                &label,
                input_origin,
                pane_width as u16,
                input_rows,
                focused,
                (TuiColor::from(input_bg), TuiColor::from(input_fg)),
            );

            // The mode label is repainted in its own color over the input's prompt.
            let mode_fg = match mode {
                Mode::Feedback => theme.color_for_scope("function").unwrap_or(input_fg),
                Mode::Commit => theme
                    .color_for_scope("diff.plus")
                    .unwrap_or(RgbValue::from_u8(150, 220, 120)),
            };
            let label_style = new_style!(
                bold color_fg: {TuiColor::from(mode_fg)} color_bg: {TuiColor::from(input_bg)}
            );
            input_ops += RenderOpCommon::MoveCursorPositionRelTo(input_origin, col(0) + row(0));
            input_ops += RenderOpCommon::ApplyColors(Some(label_style));
            input_ops +=
                RenderOpIR::PaintTextWithAttributes(label.as_str().into(), Some(label_style));

            self.doc_origin = (origin.row_index.as_usize(), origin.col_index.as_usize());
            self.doc_rows = doc_rows;
            self.doc_width = pane_width;

            // As in the file preview, a selection in progress hides the highlights.
            let selection = match &state.text_selection {
                Some(TextSelection {
                    window: selected,
                    start:
                        SelPoint::Discussion {
                            part,
                            offset: start,
                        },
                    end: SelPoint::Discussion { offset: end, .. },
                    active: true,
                    ..
                }) if *selected == window => Some((*part, (*start.min(end), *start.max(end)))),
                _ => None,
            };
            let highlights = discussion
                .node
                .as_ref()
                .and_then(|node| state.discussion_highlights.get(node));
            // Each part carries either the selection in progress or the node's highlights,
            // never both: a selection hides the highlights, as in the file preview. A
            // selection is painted on the regular selection color with its foreground
            // auto-contrasted, a highlight on the marker color with the markdown colors.
            let marked = |part: DocPart| -> (Vec<(usize, usize)>, RgbValue, bool) {
                match (selection, highlights) {
                    (Some((selected, range)), _) if selected == part => {
                        (vec![range], selection_bg, false)
                    }
                    (None, Some(highlights)) => (highlights.part(part).to_vec(), marker, true),
                    _ => (Vec::new(), pane_bg, true),
                }
            };
            let marks = [
                marked(DocPart::Prompt),
                marked(DocPart::WorkLog),
                marked(DocPart::Reply),
            ];
            let marks_of = |part: Option<DocPart>| -> (&[(usize, usize)], RgbValue, bool) {
                match part {
                    Some(DocPart::Prompt) => (&marks[0].0, marks[0].1, marks[0].2),
                    Some(DocPart::WorkLog) => (&marks[1].0, marks[1].1, marks[1].2),
                    Some(DocPart::Reply) => (&marks[2].0, marks[2].1, marks[2].2),
                    None => (&[], pane_bg, true),
                }
            };

            let shown = work_log_shown(&state.conversations.read(), discussion);
            let lines = self.doc_lines(discussion, shown, theme, pane_width);
            let line_count = lines.len();
            let max_scroll = line_count.saturating_sub(doc_rows);
            let scroll = if follow {
                max_scroll
            } else {
                state.pane_manager.window_scroll(&window).min(max_scroll)
            };

            let mut render_ops = RenderOpIRVec::new();
            let paint_row = |ops: &mut RenderOpIRVec,
                             row_offset: usize,
                             spans: &[markdown::Span],
                             fg: RgbValue,
                             bg: RgbValue,
                             marked: &[(usize, usize)],
                             marked_bg: RgbValue,
                             keep_fg: bool| {
                let strip_style =
                    new_style!(color_fg: {TuiColor::from(fg)} color_bg: {TuiColor::from(bg)});
                *ops += RenderOpCommon::MoveCursorPositionRelTo(
                    origin,
                    col(0) + row(row_offset as u16),
                );
                *ops += RenderOpCommon::ApplyColors(Some(strip_style));
                *ops += RenderOpIR::PaintTextWithAttributes(
                    " ".repeat(pane_width).into(),
                    Some(strip_style),
                );
                let mut x = 0usize;
                for piece in row_pieces(spans, marked) {
                    let truncated;
                    let text: &str = if x + display_width(piece.text) > pane_width {
                        truncated = truncate(piece.text, pane_width.saturating_sub(x));
                        truncated.as_str()
                    } else {
                        piece.text
                    };
                    let mut style = span_style(piece.span, bg);
                    if piece.marked {
                        style.color_bg = Some(TuiColor::from(marked_bg));
                        if !keep_fg {
                            style.color_fg =
                                Some(TuiColor::from(ensure_readable_fg(piece.span.fg, marked_bg)));
                        }
                    }
                    *ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        col(x as u16) + row(row_offset as u16),
                    );
                    *ops += RenderOpCommon::ApplyColors(Some(style));
                    *ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
                    x += display_width(text);
                }
            };
            for row_offset in 0..doc_rows {
                match lines.get(scroll + row_offset) {
                    Some(line) => {
                        let bg = match line.part {
                            Some(DocPart::Prompt) => prompt_bg,
                            _ => line.bg.unwrap_or(pane_bg),
                        };
                        let (marked, marked_bg, keep_fg) = marks_of(line.part);
                        paint_row(
                            &mut render_ops,
                            row_offset,
                            &line.spans,
                            text_fg,
                            bg,
                            marked,
                            marked_bg,
                            keep_fg,
                        );
                    }
                    None => paint_row(
                        &mut render_ops,
                        row_offset,
                        &[],
                        text_fg,
                        pane_bg,
                        &[],
                        pane_bg,
                        false,
                    ),
                }
            }
            if status_rows > 0 {
                let fg = if alert { error_fg } else { status_fg };
                let text = truncate(&status, pane_width);
                let span = [markdown::Span::plain(text, fg)];
                paint_row(
                    &mut render_ops,
                    doc_rows,
                    &span,
                    fg,
                    pane_bg,
                    &[],
                    pane_bg,
                    false,
                );
            }

            state.pane_manager.set_window_page_size(&window, doc_rows);
            state
                .pane_manager
                .set_window_scroll_max(&window, line_count);
            state.pane_manager.set_window_scroll(&window, scroll);
            global_data.pipeline.push(ZOrder::Normal, render_ops);
            global_data.pipeline.push(ZOrder::Normal, input_ops);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8PathBuf;

    fn node_with_metadata(metadata: &str) -> (tempfile::TempDir, ConversationTree, NodeId) {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let id = NodeId::new("2026-10-05T14-32-07Z");
        let dir = root.join(id.as_str());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("metadata.md"), metadata).unwrap();
        (tmp, ConversationTree::load(&root), id)
    }

    #[test]
    fn marker_bg_lays_the_warning_color_over_the_pane_background() {
        let theme = HelixTheme::default();
        let pane_bg = theme.ui_bg("ui.background").unwrap();
        let marker = marker_bg(&theme, pane_bg);
        assert_ne!(marker, pane_bg);
        assert_ne!(Some(marker), theme.ui_bg("ui.selection"));
        assert_eq!(marker, RgbValue::from_u8(0x76, 0x6c, 0x62));
    }

    fn span(text: &str, src: Option<usize>) -> markdown::Span {
        markdown::Span {
            src,
            ..markdown::Span::plain(text, RgbValue::from_u8(0, 0, 0))
        }
    }

    fn doc_line(part: Option<DocPart>, spans: Vec<markdown::Span>) -> DocLine {
        DocLine {
            part,
            spans,
            bg: None,
        }
    }

    #[test]
    fn offset_at_maps_a_column_to_the_nearest_source_cell() {
        let spans = [
            span("• ", None),
            span("héllo", Some(2)),
            span(" ", None),
            span("wörld", Some(10)),
        ];
        assert_eq!(offset_at(&spans, 0), Some(2));
        assert_eq!(offset_at(&spans, 2), Some(2));
        assert_eq!(offset_at(&spans, 4), Some(5));
        assert_eq!(offset_at(&spans, 7), Some(7));
        assert_eq!(offset_at(&spans, 8), Some(10));
        assert_eq!(offset_at(&spans, 100), Some(15));
        assert_eq!(offset_at(&[span("───", None)], 1), None);
    }

    #[test]
    fn locate_sends_a_row_without_text_to_the_text_around_it() {
        let lines = [
            doc_line(Some(DocPart::Prompt), vec![span("hi", Some(0))]),
            doc_line(None, Vec::new()),
            doc_line(Some(DocPart::Reply), vec![span("one", Some(0))]),
            doc_line(Some(DocPart::Reply), Vec::new()),
            doc_line(Some(DocPart::Reply), vec![span("two", Some(5))]),
        ];
        assert_eq!(locate(&lines, 0, 1), Some((DocPart::Prompt, 1)));
        assert_eq!(locate(&lines, 1, 0), Some((DocPart::Prompt, 1)));
        assert_eq!(locate(&lines, 3, 0), Some((DocPart::Reply, 2)));
        assert_eq!(locate(&lines, 4, 1), Some((DocPart::Reply, 6)));
        assert_eq!(locate(&lines, 99, 0), Some((DocPart::Reply, 7)));
        assert_eq!(locate(&lines[1..2], 0, 0), None);
        assert_eq!(locate(&[], 0, 0), None);
    }

    #[test]
    fn drag_range_takes_whole_words_across_lines_in_both_directions() {
        let text = "one two\nthree four";
        let word = word_bounds(text, 5);
        assert_eq!(word, (4, 7));
        assert_eq!(drag_range(text, 5, word, 5), (4, 7));
        assert_eq!(drag_range(text, 5, word, 15), (4, 18));
        assert_eq!(&text[4..18], "two\nthree four");
        assert_eq!(drag_range(text, 5, word, 1), (0, 7));
    }

    #[test]
    fn add_range_merges_overlapping_and_touching_ranges() {
        assert_eq!(add_range(&[], 3, 6), [(3, 6)]);
        assert_eq!(add_range(&[(0, 3), (10, 12)], 3, 6), [(0, 6), (10, 12)]);
        assert_eq!(add_range(&[(0, 3), (10, 12)], 2, 11), [(0, 12)]);
        assert_eq!(add_range(&[(8, 9)], 0, 2), [(0, 2), (8, 9)]);
    }

    #[test]
    fn remove_range_trims_splits_and_drops_ranges() {
        assert_eq!(remove_range(&[(0, 10)], 3, 6), [(0, 3), (6, 10)]);
        assert_eq!(remove_range(&[(0, 5), (8, 12)], 4, 9), [(0, 4), (9, 12)]);
        assert_eq!(remove_range(&[(2, 4), (6, 8)], 0, 5), [(6, 8)]);
        assert_eq!(remove_range(&[(2, 4)], 4, 9), [(2, 4)]);
        assert_eq!(remove_range(&[(2, 4)], 0, 2), [(2, 4)]);
    }

    #[test]
    fn row_pieces_cut_spans_at_the_marks_and_fill_the_gaps_between_them() {
        let spans = [span("one", Some(0)), span(" ", None), span("two", Some(8))];
        let pieces = |marked: &[(usize, usize)]| -> Vec<(String, bool)> {
            row_pieces(&spans, marked)
                .iter()
                .map(|piece| (piece.text.to_string(), piece.marked))
                .collect()
        };
        let piece = |text: &str, marked: bool| (text.to_string(), marked);
        assert_eq!(
            pieces(&[]),
            [piece("one", false), piece(" ", false), piece("two", false)]
        );
        assert_eq!(
            pieces(&[(1, 11)]),
            [
                piece("o", false),
                piece("ne", true),
                piece(" ", true),
                piece("two", true)
            ]
        );
        assert_eq!(
            pieces(&[(0, 3)]),
            [piece("one", true), piece(" ", false), piece("two", false)]
        );
    }

    #[test]
    fn status_text_lists_outcome_type_and_short_hashes() {
        let (_tmp, tree, id) = node_with_metadata(
            "type: commit\nstatus: done\ncommits: 0123456789abcdef 89abcdef01234567\ntokens-in: 1200\n",
        );
        let (text, failed) = status_text(tree.node(&id).unwrap());
        assert_eq!(text, "done · commit · 0123456 89abcde · 1200 tokens in");
        assert!(!failed);
    }

    #[test]
    fn status_text_reports_the_error_of_a_failed_node() {
        let (_tmp, tree, id) =
            node_with_metadata("type: feedback\nstatus: failed\nerror: rate limited\n");
        let (text, failed) = status_text(tree.node(&id).unwrap());
        assert_eq!(text, "failed: rate limited · feedback");
        assert!(failed);
    }

    #[test]
    fn node_model_needs_endpoint_model_and_a_known_effort() {
        let (_tmp, tree, id) =
            node_with_metadata("endpoint: router\nmodel: org/model-x\neffort: high\n");
        let spec = node_model(tree.node(&id).unwrap()).unwrap();
        assert_eq!(spec.to_string(), "router/org/model-x/high");

        let (_tmp, tree, id) = node_with_metadata("endpoint: router\nmodel: m\n");
        assert!(node_model(tree.node(&id).unwrap()).is_none());
    }

    #[test]
    fn utc_date_formats_the_day_of_a_unix_time() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(86_400 * 365 + 3600), "1971-01-01");
    }

    #[test]
    fn node_mode_follows_the_node_type() {
        let (_tmp, tree, id) = node_with_metadata("type: commit\n");
        assert_eq!(node_mode(tree.node(&id).unwrap()), Mode::Commit);
        let (_tmp, tree, id) = node_with_metadata("type: compact\n");
        assert_eq!(node_mode(tree.node(&id).unwrap()), Mode::Feedback);
        assert_eq!(input_label(Mode::Commit), "commit> ");
    }

    #[test]
    fn lock_excludes_every_turn_while_one_is_running() {
        use Mode::{Commit, Feedback};
        assert!(lock_refusal(&[]).is_none());
        assert!(lock_refusal(&[Feedback]).is_some());
        assert!(lock_refusal(&[Commit]).is_some());
        assert!(lock_refusal(&[Feedback, Commit]).is_some());
    }

    #[test]
    fn replay_keeps_parent_mode_and_command_of_the_node() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let parent = root.join("a1");
        let commit = parent.join("a2");
        let compact = parent.join("a3");
        for (dir, metadata) in [
            (&parent, "type: feedback\n"),
            (&commit, "type: commit\ncommand: review\n"),
            (&compact, "type: compact\n"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("metadata.md"), metadata).unwrap();
        }
        let tree = ConversationTree::load(&root);
        let spec = ModelSpec::parse("mock/m/low").unwrap();

        let launch =
            Launch::replay(&tree, &NodeId::new("a2"), "do it".to_string(), spec.clone()).unwrap();
        assert_eq!(launch.parent, Some(NodeId::new("a1")));
        assert_eq!(launch.prompt, "do it");
        assert!(matches!(launch.kind, TurnKind::Prompt(Mode::Commit)));
        assert_eq!(launch.command.as_deref(), Some("review"));
        assert!(launch.rerun.is_none());

        let typed = "/compact keep names".to_string();
        let launch = Launch::replay(&tree, &NodeId::new("a3"), typed, spec.clone()).unwrap();
        assert!(
            matches!(launch.kind, TurnKind::Compact { ref instructions } if instructions == "keep names")
        );

        let root_launch =
            Launch::replay(&tree, &NodeId::new("a1"), "hi".to_string(), spec).unwrap();
        assert_eq!(root_launch.parent, None);
        assert!(matches!(root_launch.kind, TurnKind::Prompt(Mode::Feedback)));
    }
}
