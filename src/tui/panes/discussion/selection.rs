#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

pub(crate) enum DragKind {
    /// A plain drag: a selection, copied when the button is released.
    Select,
    /// A Shift or Ctrl drag: adds to or removes from the highlights `node` had when the
    /// drag began.
    Highlight {
        modifier: crate::tui::selection::DragModifier,
        node: crate::conversation::NodeId,
        snapshot: Vec<(usize, usize)>,
    },
}

/// Source offset of the cell at `col` of a row. A cell the source has no place for
/// (marker, padding, border) or a column past the text stands for the nearest cell
/// before it, or else after it.
pub(crate) fn offset_at(
    spans: &[crate::tui::widgets::markdown::Span],
    col: usize,
) -> Option<usize> {
    use unicode_segmentation::UnicodeSegmentation;
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
            x += crate::tui::widgets::text_layout::display_width(grapheme);
        }
    }
    found
}

/// The text and source offset under `row` and `col` of the document. A row without
/// text (blank line, rule) stands for the end of the text above it, or else the start
/// of the text below.
pub(crate) fn locate(
    doc: &crate::tui::panes::discussion::document::Doc,
    row: usize,
    col: usize,
) -> Option<(crate::tui::selection::DocPart, usize)> {
    let last = doc.len().checked_sub(1)?;
    let (row, col) = if row > last {
        (last, usize::MAX)
    } else {
        (row, col)
    };
    let on = |line: &crate::tui::panes::discussion::document::DocLine, col: usize| {
        Some((line.part?, offset_at(&line.spans, col)?))
    };
    on(doc.line(row)?, col)
        .or_else(|| doc.text_before(row).and_then(|line| on(line, usize::MAX)))
        .or_else(|| doc.text_after(row).and_then(|line| on(line, 0)))
}

/// `ranges` with `lo..hi` added, overlapping and touching ranges merged.
pub(crate) fn add_range(ranges: &[(usize, usize)], lo: usize, hi: usize) -> Vec<(usize, usize)> {
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
pub(crate) fn remove_range(ranges: &[(usize, usize)], lo: usize, hi: usize) -> Vec<(usize, usize)> {
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

/// Whether `part` has lines in the document the window shows. The work log leaves it
/// without a byte of its text changing, when `Ctrl+O` hides it or a round of it is
/// dropped, and the offsets of a drag then name cells that are not painted.
pub(crate) fn part_is_on_screen(
    state: &crate::tui::state::AppState,
    discussion: &crate::tui::state::DiscussionState,
    part: crate::tui::selection::DocPart,
) -> bool {
    use crate::tui::selection::DocPart;
    match part {
        DocPart::WorkLog => {
            !discussion.shown_work_log().is_empty()
                && crate::tui::panes::discussion::status::work_log_shown(
                    &state.conversations.read(),
                    discussion,
                )
        }
        DocPart::Thinking => !discussion.thinking.is_empty(),
        DocPart::Prompt | DocPart::Reply => true,
    }
}

impl crate::tui::panes::discussion::DiscussionComponent {
    /// Starts a drag on the word under the mouse: a selection, or with Shift or Ctrl a
    /// change to the highlights of the node shown.
    pub(crate) fn begin_drag(
        &mut self,
        state: &mut crate::tui::state::AppState,
        mouse: &MouseInput,
    ) {
        use crate::tui::pane_manager::Window;
        use crate::tui::selection::DocPart;
        use crate::tui::selection::SelPoint;
        use crate::tui::selection::TextSelection;
        let Some(id) = self.window_id(state) else {
            return;
        };
        let Some(discussion) = state.discussions.get(&id) else {
            return;
        };
        let Some((part, anchor)) = self.locate(state, discussion, id, mouse.pos) else {
            return;
        };
        let text = crate::tui::panes::discussion::document::part_text(discussion, part);
        if anchor >= text.len() || !text.is_char_boundary(anchor) {
            return;
        }
        let word = crate::util::text::word_bounds(&text, anchor);
        let (content_revision, shown_log_start) =
            (discussion.content_revision, discussion.shown_log_start());
        let laid_out = part_is_on_screen(state, discussion, part);
        // A sub-agent run has no highlights: a modified drag selects too.
        let highlighted = discussion
            .node
            .as_ref()
            .filter(|_| discussion.agent.is_none());
        let kind = match (
            crate::tui::selection::drag_modifier_from_mouse(mouse),
            highlighted,
        ) {
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
                });
                DragKind::Select
            }
        };
        self.drag = Some(crate::tui::panes::discussion::Drag {
            window_id: id,
            kind,
            part,
            anchor,
            content_revision,
            shown_log_start,
            laid_out,
        });
        state.mouse_drag_active = true;
    }

    /// Ends the drag when the text it began in is no longer the one under it.
    fn drag_stands(&mut self, state: &mut crate::tui::state::AppState) -> bool {
        let unchanged = self
            .drag
            .as_ref()
            .is_some_and(|drag| drag.still_stands(state));
        if !unchanged {
            self.cancel_drag(state);
        }
        unchanged
    }

    /// Extends the drag to the word under the mouse. A drag never leaves the text it
    /// began in: past it, it reaches that text's start or end.
    pub(crate) fn update_drag(&mut self, state: &mut crate::tui::state::AppState, pos: VPPos) {
        use crate::tui::selection::DragModifier;
        use crate::tui::selection::SelPoint;
        if !self.drag_stands(state) {
            return;
        }
        let Some(drag) = &self.drag else {
            return;
        };
        let (window_id, part, anchor) = (drag.window_id, drag.part, drag.anchor);
        let Some(discussion) = state.discussions.get(&window_id) else {
            return;
        };
        let text = crate::tui::panes::discussion::document::part_text(discussion, part);
        // Found again on each move: the word under the click grows as a turn appends.
        let word = crate::util::text::word_bounds(&text, anchor);
        let (lo, hi) = match self.locate(state, discussion, window_id, pos) {
            Some((cursor_part, cursor)) if cursor_part == part => {
                if cursor >= text.len() || !text.is_char_boundary(cursor) {
                    return;
                }
                crate::util::text::drag_range(&text, anchor, word, cursor)
            }
            Some((cursor_part, _)) if cursor_part < part => (0, word.1),
            Some(_) => (word.0, text.len()),
            None => return,
        };
        let Some(drag) = &self.drag else {
            return;
        };
        match &drag.kind {
            DragKind::Select => {
                if let Some(selection) = &mut state.text_selection {
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
                    .part_mut(part)
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

    /// Ends the drag; the text a selection covers is copied to the clipboard, unless the
    /// text changed under it.
    pub(crate) fn end_drag(&mut self, state: &mut crate::tui::state::AppState) {
        if let Some(selected) = self.selected_text(state) {
            let mut clipboard = r3bl_tui::SystemClipboard;
            let _ = clipboard.try_to_put_content_into_clipboard(selected);
        }
        self.cancel_drag(state);
    }

    /// The text a plain drag covers, when it still stands: the source text between its
    /// offsets, in the text the drag began in.
    pub(crate) fn selected_text(&self, state: &crate::tui::state::AppState) -> Option<String> {
        use crate::tui::selection::SelPoint;
        use crate::tui::selection::TextSelection;
        let drag = self.drag.as_ref()?;
        if !matches!(drag.kind, DragKind::Select) || !drag.still_stands(state) {
            return None;
        }
        let (lo, hi) = match state.text_selection.as_ref()? {
            TextSelection {
                start: SelPoint::Discussion { offset: lo, .. },
                end: SelPoint::Discussion { offset: hi, .. },
                ..
            } => (*lo, *hi),
            _ => return None,
        };
        let text = crate::tui::panes::discussion::document::part_text(
            state.discussions.get(&drag.window_id)?,
            drag.part,
        );
        let selected = text.get(lo..hi).filter(|selected| !selected.is_empty())?;
        Some(selected.to_string())
    }
}
