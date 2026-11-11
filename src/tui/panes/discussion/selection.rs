#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

pub(crate) enum DragKind {
    /// A plain drag: a selection, copied when the button is released.
    Select,
    /// A Shift or Ctrl drag: adds to or removes from the highlights `node` had when the
    /// drag began.
    Highlight {
        modifier: crate::tui::panes::preview::DragModifier,
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
) -> Option<(crate::tui::pane_manager::DocPart, usize)> {
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

/// A run of cells of one span that are all marked (selected or highlighted) or all not.
pub(crate) struct Piece<'a> {
    pub(crate) text: &'a str,
    pub(crate) span: &'a crate::tui::widgets::markdown::Span,
    pub(crate) marked: bool,
}

/// Cuts the spans of a row where `marked` source ranges begin and end. Cells the source
/// has no place for are marked when the text on both sides of them is.
pub(crate) fn row_pieces<'a>(
    spans: &'a [crate::tui::widgets::markdown::Span],
    marked: &[(usize, usize)],
) -> Vec<Piece<'a>> {
    use crate::tui::widgets::markdown::Span;
    let mut runs: Vec<(&str, &Span, Option<bool>)> = Vec::with_capacity(spans.len());
    for span in spans {
        use unicode_segmentation::UnicodeSegmentation;
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

/// Whether the selection in progress lies in the work log of window `window_id`.
fn holds_work_log_selection(state: &crate::tui::state::AppState, window_id: usize) -> bool {
    use crate::tui::pane_manager::DocPart;
    use crate::tui::pane_manager::SelPoint;
    use crate::tui::pane_manager::TextSelection;
    use crate::tui::pane_manager::Window;
    matches!(
        &state.text_selection,
        Some(TextSelection {
            window: Window::Discussion(id),
            start: SelPoint::Discussion {
                part: DocPart::WorkLog,
                ..
            },
            ..
        }) if *id == window_id
    )
}

impl crate::tui::panes::discussion::DiscussionComponent {
    /// Drops the drag and the selection a work log held, when the window shows other
    /// entries: the offsets no longer name the text the user saw.
    pub(crate) fn clear_work_log_selection(
        &mut self,
        state: &mut crate::tui::state::AppState,
        window_id: usize,
    ) {
        use crate::tui::pane_manager::DocPart;
        if self
            .drag
            .as_ref()
            .is_some_and(|drag| drag.window_id == window_id && drag.part == DocPart::WorkLog)
        {
            self.cancel_drag(state);
        }
        if holds_work_log_selection(state, window_id) {
            state.text_selection = None;
        }
    }

    /// Starts a drag on the word under the mouse: a selection, or with Shift or Ctrl a
    /// change to the highlights of the node shown.
    pub(crate) fn begin_drag(
        &mut self,
        state: &mut crate::tui::state::AppState,
        mouse: &MouseInput,
    ) {
        use crate::tui::pane_manager::DocPart;
        use crate::tui::pane_manager::SelPoint;
        use crate::tui::pane_manager::TextSelection;
        use crate::tui::pane_manager::Window;
        let id = self.window_id;
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
        // A sub-agent run has no highlights: a modified drag selects too.
        let highlighted = discussion
            .node
            .as_ref()
            .filter(|_| discussion.agent.is_none());
        let kind = match (
            crate::tui::pane_component::drag_modifier_from_mouse(mouse),
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
                    active: true,
                });
                DragKind::Select
            }
        };
        self.drag = Some(crate::tui::panes::discussion::Drag {
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
    pub(crate) fn update_drag(&mut self, state: &mut crate::tui::state::AppState, pos: VPPos) {
        use crate::tui::pane_manager::SelPoint;
        use crate::tui::panes::preview::DragModifier;
        let Some(drag) = &self.drag else {
            return;
        };
        let Some(discussion) = state.discussions.get(&drag.window_id) else {
            return;
        };
        let text = crate::tui::panes::discussion::document::part_text(discussion, drag.part);
        let (lo, hi) = match self.locate(state, discussion, drag.window_id, pos) {
            Some((part, cursor)) if part == drag.part => {
                if cursor >= text.len() || !text.is_char_boundary(cursor) {
                    return;
                }
                crate::util::text::drag_range(&text, drag.anchor, drag.word, cursor)
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
    pub(crate) fn end_drag(&mut self, state: &mut crate::tui::state::AppState) {
        use crate::tui::pane_manager::SelPoint;
        use crate::tui::pane_manager::TextSelection;
        if let Some(crate::tui::panes::discussion::Drag {
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
            let text = crate::tui::panes::discussion::document::part_text(discussion, *part);
            if let Some(selected) = text.get(*lo..*hi).filter(|selected| !selected.is_empty()) {
                let mut clipboard = r3bl_tui::SystemClipboard;
                let _ = clipboard.try_to_put_content_into_clipboard(selected.to_string());
            }
        }
        self.cancel_drag(state);
    }
}
