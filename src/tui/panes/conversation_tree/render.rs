#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
const GUTTER_GAP: &str = "   ";

/// One column, nine digits.
fn queue_glyph(position: usize) -> char {
    if position < 10 {
        (b'0' + position as u8) as char
    } else {
        '…'
    }
}

/// What a provisional row shows: the first line of the prompt its window holds.
fn queued_text(
    state: &crate::tui::state::AppState,
    row: &crate::tui::panes::conversation_tree::layout::TreeRow,
) -> String {
    use crate::tui::state::RowTarget;
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

fn state_marker(meta: &crate::conversation::Metadata, spinner_tick: u8) -> Option<char> {
    use crate::conversation::NodeStatus;
    use crate::conversation::NodeType;
    match (meta.status, meta.node_type) {
        (Some(NodeStatus::Running), _) => Some(crate::agent::turn::spinner_char(spinner_tick)),
        (Some(NodeStatus::Failed), _) => Some('✗'),
        (_, Some(NodeType::Compact)) => Some('≡'),
        _ => None,
    }
}

/// The tree prefix of a row's head line, split where the color changes: the rails
/// and arm (gutter color) and the fold column (`+`/`-` in the row's type color,
/// but a leaf's `─` stays gutter color as part of the arm).
fn row_prefix_parts(
    row: &crate::tui::panes::conversation_tree::layout::TreeRow,
) -> (String, &'static str) {
    use crate::tui::panes::conversation_tree::layout::Branch;
    use crate::tui::panes::conversation_tree::layout::Fold;
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

fn row_prefix(row: &crate::tui::panes::conversation_tree::layout::TreeRow) -> String {
    let (mut prefix, fold) = row_prefix_parts(row);
    prefix.push_str(fold);
    prefix
}

/// The prefix of the wrapped title and summary lines under a selected row's head:
/// rails only, plus a `│` in the fold column when rows hang from it (`hangs`: the next
/// row is one level deeper), so the drop connects to them.
fn row_continuation_prefix(
    row: &crate::tui::panes::conversation_tree::layout::TreeRow,
    hangs: bool,
) -> String {
    let mut prefix = String::new();
    for rail in &row.rails {
        prefix.push_str(if *rail { "│ " } else { "  " });
    }
    prefix.push_str(if hangs { "│ " } else { "  " });
    prefix
}

/// `hash +N` of a node's commits, tip first: the walk a click on it opens. The
/// metadata stores them oldest first.
fn commit_label(node: &crate::conversation::Node) -> Option<String> {
    let tip = node.meta.commits.last()?;
    let short: String = tip.chars().take(7).collect();
    Some(match node.meta.commits.len() {
        1 => short,
        n => format!("{short} +{}", n - 1),
    })
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

impl crate::tui::panes::conversation_tree::ConversationTreeComponent {
    pub(crate) fn render_pane(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        current_box: FlexBox,
    ) -> CommonResult {
        use crate::conversation::NodeStatus;
        use crate::conversation::NodeType;
        use crate::tui::pane_manager::Window;
        use crate::tui::panes::conversation_tree::layout;
        use crate::tui::state::RowTarget;
        throws!({
            let origin = current_box.style_adjusted_bounds.origin_pos;
            let bounds = current_box.style_adjusted_bounds.bounds_size;
            let total_rows = bounds.row_height.as_usize();
            let pane_width = bounds.col_width.as_usize();
            self.content_origin_row = origin.row_index.as_usize();
            self.content_origin_col = origin.col_index.as_usize();
            self.line_rows.clear();
            self.label_zones.clear();

            let window = Window::ConversationTree;
            let state = &mut global_data.state;
            let was_at_scroll_max = state.pane_manager.window_at_scroll_max(&window);
            state.pane_manager.set_window_page_size(&window, total_rows);

            let tree_handle = std::sync::Arc::clone(&state.conversations);
            let tree = tree_handle.read();
            let filtering = state.tree_view.filtering();
            let (rows, matches) = layout::rows(state, &tree);
            let selected =
                layout::selected_index(&rows, state.tree_view.selected.as_ref(), filtering);
            let selected_target = rows.get(selected).map(|row| row.target.clone());
            state.tree_view.selected = selected_target.clone();

            let theme = &state.theme;
            let pane_bg = theme
                .ui_bg("ui.background")
                .unwrap_or(crate::tui::widgets::title_row::DEFAULT_BG);
            let selected_bg = theme
                .ui_bg("ui.selection")
                .unwrap_or(crate::tui::widgets::title_row::DEFAULT_SELECTION_BG);
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
            let error_fg = theme
                .ui_fg("error")
                .unwrap_or(crate::tui::widgets::title_row::DEFAULT_ERROR_FG);
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
                        vp_col(0) + vp_row(row_offset as u16),
                    );
                    render_ops += RenderOpCommon::ApplyColors(Some(bg_style));
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        " ".repeat(pane_width).into(),
                        Some(bg_style),
                    );
                }
                render_ops +=
                    RenderOpCommon::MoveCursorPositionRelTo(origin, vp_col(0) + vp_row(0));
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
                .map(|node| crate::tui::widgets::text_layout::display_width(&node.created_display))
                .max()
                .unwrap_or(0)
                + GUTTER_GAP.len();

            let mut lines: Vec<Line> = Vec::with_capacity(rows.len() + 8);
            let mut selected_span = (0, 0);
            for (row_idx, row) in rows.iter().enumerate() {
                let Some(id) = row.target.node() else {
                    let prefix_width = gutter_width
                        + crate::tui::widgets::text_layout::display_width(&row_prefix(row));
                    let width = pane_width.saturating_sub(prefix_width).max(1);
                    let text = match &row.target {
                        RowTarget::Agent { node, index } => tree
                            .agent(node, *index)
                            .map(|agent| agent.title())
                            .unwrap_or_default(),
                        RowTarget::Node(_) | RowTarget::Queued { .. } => queued_text(state, row),
                    };
                    let start = lines.len();
                    lines.push(Line {
                        row: row_idx,
                        kind: LineKind::Head,
                        offset: 0,
                        text: crate::tui::widgets::text_layout::truncate(&text, width),
                    });
                    if row_idx == selected {
                        selected_span = (start, lines.len());
                    }
                    continue;
                };
                let Some(node) = tree.node(id) else {
                    continue;
                };
                let prefix_width = gutter_width
                    + crate::tui::widgets::text_layout::display_width(&row_prefix(row));
                let label_width = commit_label(node).map_or(0, |label| {
                    crate::tui::widgets::text_layout::display_width(&label) + 1
                });
                let text_width = pane_width.saturating_sub(prefix_width + label_width).max(1);
                if row_idx != selected {
                    lines.push(Line {
                        row: row_idx,
                        kind: LineKind::Head,
                        offset: 0,
                        text: crate::tui::widgets::text_layout::truncate(
                            node.display_title(),
                            text_width,
                        ),
                    });
                    continue;
                }
                let start = lines.len();
                for (i, (offset, text)) in
                    crate::tui::widgets::text_layout::wrap(node.display_title(), text_width)
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
                if let Some(summary) = layout::shown_summary(node) {
                    for (offset, text) in
                        crate::tui::widgets::text_layout::wrap(summary, text_width)
                    {
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
            } else if was_at_scroll_max && total_rows > 0 {
                scroll = lines.len().saturating_sub(total_rows);
                state.pane_manager.set_window_scroll(&window, scroll);
            }

            for row_offset in 0..total_rows {
                let line = lines.get(scroll + row_offset);
                let is_selected = line.is_some_and(|line| line.row == selected);
                let bg = TuiColor::from(if is_selected { selected_bg } else { pane_bg });
                let bg_style = new_style!(color_bg: {bg});
                render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                    origin,
                    vp_col(0) + vp_row(row_offset as u16),
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
                            vp_col(column as u16) + vp_row(row_offset as u16),
                        );
                        *ops += RenderOpCommon::ApplyColors(Some(style));
                        *ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
                    };
                let (rail_prefix, fold_text) = row_prefix_parts(row_info);
                let fold_column =
                    gutter_width + crate::tui::widgets::text_layout::display_width(&rail_prefix);
                let text_column =
                    fold_column + crate::tui::widgets::text_layout::display_width(fold_text);

                // A queued prompt or a sub-agent run is not a node: nothing to arm, fold
                // or label.
                let Some(node) = row_info.target.node().and_then(|id| tree.node(id)) else {
                    let (glyph, glyph_fg) = match &row_info.target {
                        RowTarget::Queued { position, .. } => {
                            (Some(queue_glyph(*position)), dim_fg)
                        }
                        RowTarget::Agent { node, index } => {
                            let meta = tree.agent(node, *index).map(|agent| &agent.meta);
                            let fg = match meta.and_then(|meta| meta.status) {
                                Some(NodeStatus::Failed) => error_fg,
                                _ => running_fg,
                            };
                            (
                                meta.and_then(|meta| state_marker(meta, state.spinner_tick)),
                                fg,
                            )
                        }
                        RowTarget::Node(_) => continue,
                    };
                    paint_plain(&mut render_ops, gutter_width, &rail_prefix, gutter);
                    paint_plain(&mut render_ops, fold_column, fold_text, gutter);
                    if let Some(glyph) = glyph {
                        paint_plain(
                            &mut render_ops,
                            gutter_width - 2,
                            &glyph.to_string(),
                            glyph_fg,
                        );
                    }
                    paint_plain(&mut render_ops, text_column, &line.text, dim_fg);
                    continue;
                };
                let node_id = row_info.target.node().expect("checked above");

                let armed = state.tree_view.armed_delete.as_ref() == Some(node_id);
                let favorite = node.meta.favorite;
                let type_fg = match (node.meta.status, node.meta.node_type) {
                    _ if armed => error_fg,
                    (Some(NodeStatus::Failed), _) => error_fg,
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
                        vp_col(column as u16) + vp_row(row_offset as u16),
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
                            vp_col(column as u16) + vp_row(row_offset as u16),
                        );
                        *ops += RenderOpCommon::ApplyColors(Some(style));
                        *ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
                        column += crate::tui::widgets::text_layout::display_width(text);
                    }
                };

                let marker = state_marker(&node.meta, state.spinner_tick);
                match line.kind {
                    LineKind::Head => {
                        paint_plain(&mut render_ops, 0, &node.created_display, gutter);
                        paint_plain(&mut render_ops, gutter_width, &rail_prefix, gutter);
                        if row_info.fold == layout::Fold::Leaf {
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
                            let column = pane_width.saturating_sub(
                                crate::tui::widgets::text_layout::display_width(&label),
                            );
                            paint(&mut render_ops, column, &label, hash_fg);
                            let slot = self.label_zones.last_mut().expect("pushed above");
                            *slot = Some((
                                column,
                                crate::tui::widgets::text_layout::display_width(&label),
                            ));
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
    use crate::tui::panes::conversation_tree::layout::layout_rows;
    use crate::tui::panes::conversation_tree::test_support::ids;
    use crate::tui::panes::conversation_tree::test_support::tree_with;

    fn prefixes(tree: &crate::conversation::ConversationTree, expanded: &[&str]) -> Vec<String> {
        layout_rows(tree, &ids(expanded))
            .iter()
            .map(row_prefix)
            .collect()
    }

    #[test]
    fn head_prefix_draws_rails_arms_and_fold_column() {
        let (_tmp, tree) = tree_with(&["a1/a2", "a1/b2", "c1/c2/d3", "c1/c2/e3"]);
        assert_eq!(
            prefixes(&tree, &["a1", "c2"]),
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
        let layout_rows = layout_rows(&tree, &ids(&["a1"]));
        assert_eq!(
            layout_rows.iter().map(row_prefix).collect::<Vec<_>>(),
            [
                "- ",   // a1: expanded root
                "├─+ ", // a2: collapsed fork starting a branch
                "└── ", // b2: last branch, leaf
            ]
        );
        assert_eq!(row_continuation_prefix(&layout_rows[1], false), "│   ");
    }

    #[test]
    fn continuation_prefix_hangs_under_an_expanded_fork() {
        let (_tmp, tree) = tree_with(&["a1/a2/b3", "a1/a2/c3"]);
        let rows = layout_rows(&tree, &ids(&["a2"]));
        assert_eq!(row_continuation_prefix(&rows[1], true), "  │ ");
    }

    #[test]
    fn an_expanded_thread_draws_its_replies_under_the_arm() {
        let (_tmp, tree) = tree_with(&["a1/a2/a3/a4", "a1/b2"]);
        assert_eq!(
            prefixes(&tree, &["a1", "a2"]),
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
    fn the_first_row_below_a_root_gets_the_arm() {
        let (_tmp, tree) = tree_with(&["a1/a2/a3/b4", "a1/a2/a3/c4"]);
        assert_eq!(prefixes(&tree, &[]), ["+ ", "└─+ "]);
        assert_eq!(prefixes(&tree, &["a1"]), ["- ", "└── ", "  + "]);
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
    fn a_position_past_nine_renders_an_ellipsis() {
        assert_eq!(queue_glyph(1), '1');
        assert_eq!(queue_glyph(9), '9');
        assert_eq!(queue_glyph(10), '…');
    }

    #[test]
    fn label_zone_hit_test_is_pane_relative() {
        let pane_width = 120;
        let label = "abc1234 +2";
        let width = crate::tui::widgets::text_layout::display_width(label);
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
}
