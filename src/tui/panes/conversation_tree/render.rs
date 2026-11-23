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

fn state_marker(meta: &crate::conversation::metadata::Metadata, spinner_tick: u8) -> Option<char> {
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

/// The colors a frame of the tree is painted with.
struct TreeColors {
    pane_bg: crate::tui::theme::Color,
    selected_bg: crate::tui::theme::Color,
    text: crate::tui::theme::Color,
    dim: crate::tui::theme::Color,
    gutter: crate::tui::theme::Color,
    selected_gutter: crate::tui::theme::Color,
    commit: crate::tui::theme::Color,
    hash: crate::tui::theme::Color,
    running: crate::tui::theme::Color,
    error: crate::tui::theme::Color,
    /// Favorite nodes and the matched text of a filter.
    matched: crate::tui::theme::Color,
}

impl TreeColors {
    fn of(theme: &crate::tui::theme::HelixTheme) -> Self {
        Self {
            pane_bg: theme.ui.background,
            selected_bg: theme.ui.selection,
            text: theme.ui.text,
            dim: theme.ui.dim,
            gutter: theme.ui.line_number,
            selected_gutter: theme.ui.line_number_selected,
            commit: theme.color_for_scope("diff.plus"),
            hash: theme.color_for_scope("constant"),
            running: theme.color_for_scope("diff.delta"),
            error: theme.ui.error,
            matched: theme.ui.matched,
        }
    }
}

fn paint_at(
    ops: &mut RenderOpIRVec,
    origin: VPPos,
    row_offset: usize,
    column: usize,
    text: &str,
    style: TuiStyle,
) {
    *ops += RenderOpCommon::MoveCursorPositionRelTo(
        origin,
        vp_col(column as u16) + vp_row(row_offset as u16),
    );
    *ops += RenderOpCommon::ApplyColors(Some(style));
    *ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
}

/// A blank pane with `message` on its first row.
fn paint_message(
    ops: &mut RenderOpIRVec,
    origin: VPPos,
    size: (usize, usize),
    message: &str,
    style: TuiStyle,
) {
    let (pane_width, total_rows) = size;
    for row_offset in 0..total_rows {
        paint_at(ops, origin, row_offset, 0, &" ".repeat(pane_width), style);
    }
    paint_at(ops, origin, 0, 0, message, style);
}

/// The lines of every row, and the span of lines the selected row takes.
fn build_lines(
    state: &crate::tui::state::AppState,
    tree: &crate::conversation::ConversationTree,
    rows: &[crate::tui::panes::conversation_tree::layout::TreeRow],
    selected: usize,
    gutter_width: usize,
    pane_width: usize,
) -> (Vec<Line>, (usize, usize)) {
    use crate::tui::panes::conversation_tree::layout;
    let mut lines: Vec<Line> = Vec::with_capacity(rows.len() + 8);
    let mut selected_span = (0, 0);
    for (row_idx, row) in rows.iter().enumerate() {
        let Some(id) = row.target.node() else {
            let prefix_width =
                gutter_width + crate::tui::widgets::text_layout::display_width(&row_prefix(row));
            let width = pane_width.saturating_sub(prefix_width).max(1);
            let text = queued_text(state, row);
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
        let prefix_width =
            gutter_width + crate::tui::widgets::text_layout::display_width(&row_prefix(row));
        let label_width = commit_label(node).map_or(0, |label| {
            crate::tui::widgets::text_layout::display_width(&label) + 1
        });
        let text_width = pane_width.saturating_sub(prefix_width + label_width).max(1);
        if row_idx != selected {
            lines.push(Line {
                row: row_idx,
                kind: LineKind::Head,
                offset: 0,
                text: crate::tui::widgets::text_layout::truncate(node.display_title(), text_width),
            });
            continue;
        }
        let start = lines.len();
        for (i, (offset, text)) in
            crate::tui::widgets::text_layout::wrap(node.display_title(), text_width)
                .into_iter()
                .enumerate()
        {
            let kind = if i == 0 {
                LineKind::Head
            } else {
                LineKind::TitleRest
            };
            lines.push(Line {
                row: row_idx,
                kind,
                offset,
                text: text.to_string(),
            });
        }
        if let Some(summary) = layout::shown_summary(node) {
            for (offset, text) in crate::tui::widgets::text_layout::wrap(summary, text_width) {
                lines.push(Line {
                    row: row_idx,
                    kind: LineKind::Summary,
                    offset,
                    text: text.to_string(),
                });
            }
        }
        selected_span = (start, lines.len());
    }
    (lines, selected_span)
}

/// What painting the lines of one frame reads.
struct LinePainter<'a> {
    tree: &'a crate::conversation::ConversationTree,
    rows: &'a [crate::tui::panes::conversation_tree::layout::TreeRow],
    matches: &'a [crate::tui::panes::conversation_tree::filter::FilterMatch],
    colors: &'a TreeColors,
    armed_delete: Option<&'a crate::conversation::NodeId>,
    spinner_tick: u8,
    origin: VPPos,
    pane_width: usize,
    gutter_width: usize,
    selected: usize,
}

impl LinePainter<'_> {
    /// Returns the commit label's click zone as `(column, width)`, if the line shows one.
    fn paint_line(
        &self,
        ops: &mut RenderOpIRVec,
        row_offset: usize,
        line: &Line,
        bg: crate::tui::theme::Color,
    ) -> Option<(usize, usize)> {
        use crate::conversation::NodeStatus;
        use crate::conversation::NodeType;
        use crate::tui::panes::conversation_tree::layout;
        use crate::tui::state::RowTarget;
        use crate::tui::theme::Color;
        let colors = self.colors;
        let origin = self.origin;
        let gutter_width = self.gutter_width;
        let row_info = &self.rows[line.row];
        let gutter = if line.row == self.selected {
            colors.selected_gutter
        } else {
            colors.gutter
        };
        let paint_plain = |ops: &mut RenderOpIRVec, column: usize, text: &str, fg: Color| {
            paint_at(ops, origin, row_offset, column, text, fg.on(bg));
        };
        let (rail_prefix, fold_text) = row_prefix_parts(row_info);
        let fold_column =
            gutter_width + crate::tui::widgets::text_layout::display_width(&rail_prefix);
        let text_column = fold_column + crate::tui::widgets::text_layout::display_width(fold_text);

        // A queued prompt is not a node: nothing to arm, fold or label.
        let Some(node) = row_info.target.node().and_then(|id| self.tree.node(id)) else {
            let (glyph, glyph_fg) = match &row_info.target {
                RowTarget::Queued { position, .. } => (Some(queue_glyph(*position)), colors.dim),
                RowTarget::Node(_) => return None,
            };
            paint_plain(ops, gutter_width, &rail_prefix, gutter);
            paint_plain(ops, fold_column, fold_text, gutter);
            if let Some(glyph) = glyph {
                paint_plain(ops, gutter_width - 2, &glyph.to_string(), glyph_fg);
            }
            paint_plain(ops, text_column, &line.text, colors.dim);
            return None;
        };

        let armed = self.armed_delete == row_info.target.node();
        let favorite = node.meta.favorite;
        let type_fg = match (node.meta.status, node.meta.node_type) {
            _ if armed => colors.error,
            (Some(NodeStatus::Failed), _) => colors.error,
            (Some(NodeStatus::Running), _) => colors.running,
            _ if favorite => colors.matched,
            (_, Some(NodeType::Commit)) => colors.commit,
            (_, Some(NodeType::Compact)) => colors.dim,
            _ => colors.text,
        };
        let styled = |fg: Color| {
            if favorite { fg.on_bold(bg) } else { fg.on(bg) }
        };
        let paint = |ops: &mut RenderOpIRVec, column: usize, text: &str, fg: Color| {
            paint_at(ops, origin, row_offset, column, text, styled(fg));
        };

        // While filtering, the rows are the matches, in the same order.
        let matched: &[(usize, usize)] = match (self.matches.get(line.row), line.kind) {
            (Some(found), LineKind::Summary) => &found.summary,
            (Some(found), _) => &found.title,
            (None, _) => &[],
        };
        let paint_text = |ops: &mut RenderOpIRVec, fg: Color| {
            let mut column = text_column;
            for (text, is_match) in segments(&line.text, line.offset, matched) {
                let mut style = styled(if is_match { colors.matched } else { fg });
                if is_match {
                    style.attribs.underline = Some(tui_style_attrib::Underline);
                }
                paint_at(ops, origin, row_offset, column, text, style);
                column += crate::tui::widgets::text_layout::display_width(text);
            }
        };

        match line.kind {
            LineKind::Head => {
                paint_plain(ops, 0, &node.created_display, gutter);
                paint_plain(ops, gutter_width, &rail_prefix, gutter);
                if row_info.fold == layout::Fold::Leaf {
                    paint_plain(ops, fold_column, fold_text, gutter);
                } else {
                    paint(ops, fold_column, fold_text, type_fg);
                }
                if let Some(marker) = state_marker(&node.meta, self.spinner_tick) {
                    paint(ops, gutter_width - 2, &marker.to_string(), type_fg);
                }
                paint_text(ops, type_fg);
                let label = commit_label(node)?;
                let width = crate::tui::widgets::text_layout::display_width(&label);
                let column = self.pane_width.saturating_sub(width);
                paint(ops, column, &label, colors.hash);
                Some((column, width))
            }
            LineKind::TitleRest | LineKind::Summary => {
                let hangs = self
                    .rows
                    .get(line.row + 1)
                    .is_some_and(|next| next.rails.len() > row_info.rails.len());
                let continuation = row_continuation_prefix(row_info, hangs);
                paint_plain(ops, gutter_width, &continuation, colors.selected_gutter);
                let fg = match line.kind {
                    LineKind::Summary if armed => colors.error,
                    LineKind::Summary => colors.dim,
                    _ => type_fg,
                };
                paint_text(ops, fg);
                None
            }
        }
    }
}

impl crate::tui::panes::conversation_tree::ConversationTreeComponent {
    pub(crate) fn render_pane(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        current_box: FlexBox,
    ) -> CommonResult {
        use crate::tui::pane_manager::Window;
        use crate::tui::panes::conversation_tree::layout;
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
            let (rows, matches) = layout::rows(state, &tree, &mut self.filter_cache);
            let selected =
                layout::selected_index(&rows, state.tree_view.selected.as_ref(), filtering);
            let selected_target = rows.get(selected).map(|row| row.target.clone());
            state.tree_view.selected = selected_target.clone();

            let colors = TreeColors::of(&state.theme);
            let mut render_ops = RenderOpIRVec::new();

            if rows.is_empty() {
                let message = if !state.tree_view.loaded {
                    "Loading conversations…"
                } else if filtering {
                    "No matches"
                } else {
                    "(no conversations)"
                };
                let style = colors.text.on(colors.pane_bg);
                paint_message(
                    &mut render_ops,
                    origin,
                    (pane_width, total_rows),
                    message,
                    style,
                );
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

            let (lines, selected_span) =
                build_lines(state, &tree, &rows, selected, gutter_width, pane_width);

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

            let painter = LinePainter {
                tree: &tree,
                rows: &rows,
                matches: &matches,
                colors: &colors,
                armed_delete: state.tree_view.armed_delete.as_ref(),
                spinner_tick: state.spinner_tick,
                origin,
                pane_width,
                gutter_width,
                selected,
            };
            for row_offset in 0..total_rows {
                let line = lines.get(scroll + row_offset);
                let is_selected = line.is_some_and(|line| line.row == selected);
                let bg = if is_selected {
                    colors.selected_bg
                } else {
                    colors.pane_bg
                };
                let blank = " ".repeat(pane_width);
                paint_at(
                    &mut render_ops,
                    origin,
                    row_offset,
                    0,
                    &blank,
                    colors.text.on(bg),
                );
                let Some(line) = line else {
                    continue;
                };
                self.line_rows.push(line.row);
                self.label_zones
                    .push(painter.paint_line(&mut render_ops, row_offset, line, bg));
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

    use crate::tui::pane_manager::Window;
    use crate::tui::panes::conversation_tree::ConversationTreeComponent;
    use crate::tui::state::AppState;
    use crate::tui::state::RowTarget;

    type Data = GlobalData<AppState, crate::signal::AppSignal>;

    struct Screen(Vec<Vec<PixelChar>>);

    impl Screen {
        fn text(&self, row: usize) -> String {
            let line: String = self.0[row]
                .iter()
                .map(|pixel| match pixel {
                    PixelChar::PlainText { display_char, .. } => *display_char,
                    _ => ' ',
                })
                .collect();
            line.trim_end().to_string()
        }

        fn texts(&self) -> Vec<String> {
            (0..self.0.len()).map(|row| self.text(row)).collect()
        }

        fn style(&self, row: usize, col: usize) -> TuiStyle {
            match self.0[row][col] {
                PixelChar::PlainText { style, .. } => style,
                _ => TuiStyle::default(),
            }
        }
    }

    fn data_over(tree: crate::conversation::ConversationTree, rows: usize) -> Data {
        let mut state = AppState::for_test(camino::Utf8PathBuf::from("/tmp"));
        *state.conversations.write() = tree;
        state.tree_view.loaded = true;
        state
            .pane_manager
            .set_window_view(&Window::ConversationTree, 0, 100, rows);
        crate::tui::test_support::global_data(state)
    }

    fn component() -> ConversationTreeComponent {
        ConversationTreeComponent::new(crate::tui::pane_component::pane_id(0))
    }

    /// Renders one frame of the pane at `cols` x `rows` and reads the screen back.
    fn frame(
        component: &mut ConversationTreeComponent,
        global_data: &mut Data,
        cols: u16,
        rows: u16,
    ) -> Screen {
        let size = vp_width(cols) + vp_height(rows);
        component
            .render_pane(global_data, crate::tui::test_support::frame_box(cols, rows))
            .unwrap();
        let pipeline = std::mem::take(&mut global_data.pipeline);
        let mut ofs_buf = OfsBuf::new(Flat2DArray::new_empty(size, PixelChar::Spacer));
        pipeline.compose_render_ops_into_ofs_buf(
            size,
            &mut ofs_buf,
            &mut std::default::Default::default(),
        );
        Screen(
            (0..rows)
                .map(|row| ofs_buf.get_row(vp_row(row)).unwrap().to_vec())
                .collect(),
        )
    }

    fn tree_of(
        nodes: &[(&str, &[(&str, &str)])],
    ) -> (tempfile::TempDir, crate::conversation::ConversationTree) {
        use crate::conversation::ConversationTree;
        use crate::conversation::test_support::write_node;
        use crate::util::test_support::temp_root;
        let (tmp, root) = temp_root();
        for (path, files) in nodes {
            write_node(&root.join(path), files);
        }
        let tree = ConversationTree::load(&root);
        (tmp, tree)
    }

    /// `count` root nodes named `n0`, `n1`, ..., with a title each.
    fn numbered_tree(count: usize) -> (tempfile::TempDir, crate::conversation::ConversationTree) {
        let names: Vec<String> = (0..count).map(|i| format!("n{i}")).collect();
        let nodes: Vec<(&str, &[(&str, &str)])> = names
            .iter()
            .map(|name| {
                (
                    name.as_str(),
                    &[(crate::conversation::files::TITLE, "x")][..],
                )
            })
            .collect();
        tree_of(&nodes)
    }

    fn node_id(name: &str) -> crate::conversation::NodeId {
        use crate::conversation::NodeId;
        NodeId::new(name)
    }

    fn select(data: &mut Data, name: &str) {
        data.state.tree_view.selected = Some(RowTarget::Node(node_id(name)));
    }

    #[test]
    fn an_empty_tree_says_why_over_a_blank_pane() {
        let (_tmp, tree) = tree_of(&[]);
        let mut data = data_over(tree, 4);
        let mut component = component();
        data.state.tree_view.loaded = false;
        let screen = frame(&mut component, &mut data, 30, 4);
        assert_eq!(screen.texts(), ["Loading conversations…", "", "", ""]);
        data.state.tree_view.loaded = true;
        let screen = frame(&mut component, &mut data, 30, 4);
        assert_eq!(screen.texts(), ["(no conversations)", "", "", ""]);
        let pane_bg = data.state.theme.ui.background.tui();
        assert_eq!(screen.style(3, 29).color_bg, pane_bg);
        assert_eq!(
            data.state
                .pane_manager
                .window_scroll(&Window::ConversationTree),
            0
        );
    }

    #[test]
    fn a_filter_without_a_match_says_so() {
        let (_tmp, tree) = tree_of(&[("a1", &[(crate::conversation::files::TITLE, "First")])]);
        let mut data = data_over(tree, 3);
        data.state.tree_view.query.set_text("zzzz");
        let screen = frame(&mut component(), &mut data, 30, 3);
        assert_eq!(screen.texts(), ["No matches", "", ""]);
    }

    #[test]
    fn rows_draw_rails_markers_and_the_commit_label() {
        let (_tmp, tree) = tree_of(&[
            (
                "a1",
                &[
                    (crate::conversation::files::TITLE, "First thing"),
                    (
                        crate::conversation::files::METADATA,
                        "commits: abc1234def abc9999aaa\n",
                    ),
                ],
            ),
            (
                "a1/a2",
                &[
                    (crate::conversation::files::TITLE, "Reply"),
                    (crate::conversation::files::METADATA, "status: failed\n"),
                ],
            ),
            (
                "a1/b2",
                &[
                    (crate::conversation::files::TITLE, "Compacted"),
                    (crate::conversation::files::METADATA, "type: compact\n"),
                ],
            ),
            ("b1", &[(crate::conversation::files::TITLE, "Second")]),
        ]);
        let mut data = data_over(tree, 6);
        data.state.tree_view.expanded.insert(node_id("a1"));
        select(&mut data, "b1");
        let screen = frame(&mut component(), &mut data, 30, 6);
        assert_eq!(
            screen.texts(),
            [
                "   - First thing    abc9999 +1",
                " ✗ ├── Reply",
                " ≡ └── Compacted",
                "     Second",
                "",
                "",
            ]
        );
    }

    #[test]
    fn the_selected_row_wraps_its_title_and_shows_its_summary_on_the_selection_color() {
        let (_tmp, tree) = tree_of(&[
            ("a1", &[(crate::conversation::files::TITLE, "First thing")]),
            (
                "b1",
                &[
                    (
                        crate::conversation::files::TITLE,
                        "Second thing that has a quite long title to wrap",
                    ),
                    (crate::conversation::files::SUMMARY, "sum"),
                ],
            ),
        ]);
        let mut data = data_over(tree, 6);
        let theme = data.state.theme.clone();
        let screen = frame(&mut component(), &mut data, 30, 6);
        assert_eq!(
            screen.texts(),
            [
                "     First thing",
                "     Second thing that has a",
                "     quite long title to wrap",
                "     sum",
                "",
                "",
            ]
        );
        assert_eq!(screen.style(0, 29).color_bg, theme.ui.background.tui());
        for row in 1..4 {
            assert_eq!(screen.style(row, 29).color_bg, theme.ui.selection.tui());
        }
        assert_eq!(screen.style(4, 0).color_bg, theme.ui.background.tui());
        assert_eq!(screen.style(3, 5).color_fg, theme.ui.dim.tui());
        assert_eq!(screen.style(1, 5).color_fg, theme.ui.text.tui());
    }

    #[test]
    fn a_narrow_pane_cuts_a_row_short_and_leaves_room_for_the_label() {
        let (_tmp, tree) = tree_of(&[
            (
                "a1",
                &[
                    (
                        crate::conversation::files::TITLE,
                        "A title far too long for the pane",
                    ),
                    (
                        crate::conversation::files::METADATA,
                        "commits: abc1234def\n",
                    ),
                ],
            ),
            ("b1", &[(crate::conversation::files::TITLE, "Second")]),
        ]);
        let mut data = data_over(tree, 3);
        let mut component = component();
        let screen = frame(&mut component, &mut data, 20, 3);
        assert_eq!(screen.texts(), ["     A title abc1234", "     Second", ""]);
        assert_eq!(component.label_zones, [Some((13, 7)), None]);
    }

    #[test]
    fn a_queued_prompt_is_listed_under_its_parent_in_dim_text() {
        use crate::tui::state::PendingTurn;
        let (_tmp, tree) = tree_of(&[("a1", &[(crate::conversation::files::TITLE, "First")])]);
        let mut data = data_over(tree, 3);
        data.state.queued_turns.push(PendingTurn {
            window: 0,
            parent: Some(node_id("a1")),
        });
        let screen = frame(&mut component(), &mut data, 30, 3);
        assert_eq!(screen.texts(), ["     First", " 1 └── (empty prompt)", ""]);
        assert_eq!(screen.style(1, 8).color_fg, data.state.theme.ui.dim.tui());
    }

    #[test]
    fn a_filter_lists_the_matches_flat_with_the_matched_text_underlined() {
        let (_tmp, tree) = tree_of(&[
            ("a1", &[(crate::conversation::files::TITLE, "First thing")]),
            ("b1", &[(crate::conversation::files::TITLE, "Second item")]),
        ]);
        let mut data = data_over(tree, 3);
        data.state.tree_view.query.set_text("item");
        let screen = frame(&mut component(), &mut data, 30, 3);
        assert_eq!(screen.texts()[0], "     Second item");
        for col in 12..16 {
            assert!(screen.style(0, col).attribs.underline.is_some());
        }
        assert!(screen.style(0, 5).attribs.underline.is_none());
        assert_eq!(
            screen.style(0, 12).color_fg,
            data.state.theme.ui.matched.tui()
        );
    }

    #[test]
    fn an_armed_row_turns_the_error_color() {
        let (_tmp, tree) = tree_of(&[("a1", &[(crate::conversation::files::TITLE, "First")])]);
        let mut data = data_over(tree, 2);
        select(&mut data, "a1");
        data.state.tree_view.armed_delete = Some(node_id("a1"));
        let screen = frame(&mut component(), &mut data, 30, 2);
        assert_eq!(screen.style(0, 5).color_fg, data.state.theme.ui.error.tui());
    }

    #[test]
    fn a_new_selection_scrolls_into_view_and_a_growing_tree_follows_the_bottom() {
        let (_tmp, tree) = numbered_tree(10);
        let mut data = data_over(tree, 4);
        let window = Window::ConversationTree;
        let mut component = component();
        select(&mut data, "n9");
        frame(&mut component, &mut data, 30, 4);
        assert_eq!(data.state.pane_manager.window_scroll(&window), 6);
        select(&mut data, "n0");
        frame(&mut component, &mut data, 30, 4);
        assert_eq!(data.state.pane_manager.window_scroll(&window), 0);
        select(&mut data, "n5");
        frame(&mut component, &mut data, 30, 4);
        assert_eq!(data.state.pane_manager.window_scroll(&window), 2);
        assert_eq!(component.line_rows, [2, 3, 4, 5]);
        select(&mut data, "n9");
        frame(&mut component, &mut data, 30, 4);
        assert_eq!(data.state.pane_manager.window_scroll(&window), 6);
        let (_grown_tmp, grown) = numbered_tree(11);
        *data.state.conversations.write() = grown;
        frame(&mut component, &mut data, 30, 4);
        assert_eq!(data.state.pane_manager.window_scroll(&window), 7);
        assert_eq!(component.line_rows, [7, 8, 9, 10]);
    }

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
