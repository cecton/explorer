use std::collections::HashSet;

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
    pub target: crate::tui::state::RowTarget,
    /// One entry per level: whether the fork above still has branches below this row.
    pub rails: Vec<bool>,
    pub fold: Fold,
    pub branch: Branch,
}

impl TreeRow {
    fn node(
        id: &crate::conversation::NodeId,
        rails: Vec<bool>,
        fold: Fold,
        branch: Branch,
    ) -> Self {
        use crate::tui::state::RowTarget;
        Self {
            target: RowTarget::Node(id.clone()),
            rails,
            fold,
            branch,
        }
    }
}

/// The rows the window shows, and what matched in them while the filter holds a word:
/// the tree with a provisional row per queued prompt and per sub-agent run, or the
/// matches of the filter, flat.
pub(crate) fn rows(
    state: &crate::tui::state::AppState,
    tree: &crate::conversation::ConversationTree,
) -> (
    Vec<TreeRow>,
    Vec<crate::tui::panes::conversation_tree::filter::FilterMatch>,
) {
    if !state.tree_view.filtering() {
        let rows = with_agents(
            with_queued(
                layout_rows(tree, &state.tree_view.expanded),
                &state.queued_turns,
            ),
            tree,
            &state.tree_view.shown_agents,
        );
        return (rows, Vec::new());
    }
    let matches = crate::tui::panes::conversation_tree::filter::filter_nodes(
        tree,
        state.tree_view.query.text(),
    );
    let rows = flat_rows(&matches);
    (rows, matches)
}

/// The row `selected` sits on. Without one, the newest root of the tree, or the
/// best match of a filter.
pub(crate) fn selected_index(
    rows: &[TreeRow],
    selected: Option<&crate::tui::state::RowTarget>,
    filtering: bool,
) -> usize {
    selected
        .and_then(|target| rows.iter().position(|row| &row.target == target))
        .or_else(|| {
            rows.iter()
                .rposition(|row| row.rails.is_empty())
                .filter(|_| !filtering)
        })
        .unwrap_or(0)
}

/// The summary shown under a node's title, when it says something else.
pub(crate) fn shown_summary(node: &crate::conversation::Node) -> Option<&str> {
    node.summary
        .as_deref()
        .filter(|summary| *summary != node.display_title())
}

/// Flattens the tree into display rows: roots oldest first, like the nodes of a
/// conversation. A thread, the straight run of single replies below a root or a
/// branch, folds into its head; the fork ending it stays visible. A root's descendants
/// sit one level below it; after that, indentation grows only below a fork.
pub(crate) fn layout_rows(
    tree: &crate::conversation::ConversationTree,
    expanded: &HashSet<crate::conversation::NodeId>,
) -> Vec<TreeRow> {
    let mut rows = Vec::new();
    let mut rails = Vec::new();
    for root in tree.roots() {
        push_thread(tree, expanded, root, Branch::None, &mut rails, &mut rows);
    }
    rows
}

fn fold_of(foldable: bool, expanded: bool) -> Fold {
    match (foldable, expanded) {
        (false, _) => Fold::Leaf,
        (true, true) => Fold::Expanded,
        (true, false) => Fold::Collapsed,
    }
}

fn push_thread(
    tree: &crate::conversation::ConversationTree,
    expanded: &HashSet<crate::conversation::NodeId>,
    head: &crate::conversation::NodeId,
    branch: Branch,
    rails: &mut Vec<bool>,
    rows: &mut Vec<TreeRow>,
) {
    let Some(node) = tree.node(head) else {
        return;
    };
    let (hidden, fork) = tree.thread_below(head);
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
    tree: &crate::conversation::ConversationTree,
    expanded: &HashSet<crate::conversation::NodeId>,
    fork: &crate::conversation::Node,
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

/// The rows of a filtered window: its matches, flat.
fn flat_rows(
    matches: &[crate::tui::panes::conversation_tree::filter::FilterMatch],
) -> Vec<TreeRow> {
    matches
        .iter()
        .map(|found| TreeRow::node(&found.node, Vec::new(), Fold::Leaf, Branch::None))
        .collect()
}

/// The tree with one provisional row per queued prompt, after the row of the node it
/// will follow. An entry whose anchor row is not shown (folded away, or a root prompt
/// whose window shows no node yet) is left out: the queue is not part of the tree, so
/// it cannot invent a row for a node that is not on screen.
fn with_queued(rows: Vec<TreeRow>, queue: &[crate::tui::state::PendingTurn]) -> Vec<TreeRow> {
    use crate::tui::state::RowTarget;
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

/// The rows with one row per sub-agent run of the turn of each node in `shown`, right
/// after the node's row and one level below it.
fn with_agents(
    rows: Vec<TreeRow>,
    tree: &crate::conversation::ConversationTree,
    shown: &HashSet<crate::conversation::NodeId>,
) -> Vec<TreeRow> {
    use crate::tui::state::RowTarget;
    if shown.is_empty() {
        return rows;
    }
    let mut lines = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        lines.push(row.clone());
        let Some(id) = row.target.node().filter(|id| shown.contains(*id)) else {
            continue;
        };
        let Some(node) = tree.node(id) else {
            continue;
        };
        // Rows hanging from the node below its runs keep the arm going past them.
        let hangs = rows
            .get(index + 1)
            .is_some_and(|next| next.rails.len() > row.rails.len());
        let last = node.agents.len().saturating_sub(1);
        for (position, agent) in node.agents.iter().enumerate() {
            let continues = position != last || hangs;
            let mut rails = row.rails.clone();
            rails.push(continues);
            lines.push(TreeRow {
                target: RowTarget::Agent {
                    node: id.clone(),
                    index: agent.index,
                },
                rails,
                fold: Fold::Leaf,
                branch: if continues {
                    Branch::Start
                } else {
                    Branch::LastStart
                },
            });
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::panes::conversation_tree::test_support::described_tree;
    use crate::tui::panes::conversation_tree::test_support::ids;
    use crate::tui::panes::conversation_tree::test_support::tree_with;

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
        use crate::conversation::NodeId;
        use crate::tui::state::RowTarget;
        let (_tmp, tree) = tree_with(&["a1/a2", "b1/b2"]);
        let rows = layout_rows(&tree, &ids(&["a1", "b1"]));
        assert_eq!(selected_index(&rows, None, false), 2);
        let known = RowTarget::Node(NodeId::new("a2"));
        assert_eq!(selected_index(&rows, Some(&known), false), 1);
    }

    #[test]
    fn a_filter_selects_its_best_match_when_nothing_is_selected() {
        let (_tmp, tree) = described_tree(&[("a1", "Wrap", ""), ("b1", "Wrap", "")]);
        let matches = crate::tui::panes::conversation_tree::filter::filter_nodes(&tree, "wrap");
        let rows = flat_rows(&matches);
        assert_eq!(selected_index(&rows, None, true), 0);
        assert_eq!(
            shape(&rows),
            [("b1", vec![], Fold::Leaf), ("a1", vec![], Fold::Leaf)]
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
            shape(&rows),
            [
                ("a1", vec![], Fold::Expanded),
                ("a2", vec![true], Fold::Expanded),
                ("a3", vec![true], Fold::Leaf),
                ("a4", vec![true], Fold::Leaf),
                ("b2", vec![false], Fold::Leaf),
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
    fn a_queued_prompt_gets_a_row_below_the_node_it_will_follow() {
        use crate::conversation::NodeId;
        use crate::tui::state::PendingTurn;
        use crate::tui::state::RowTarget;
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
        use crate::conversation::NodeId;
        use crate::tui::state::PendingTurn;
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
    fn sub_agent_runs_hang_from_their_node_only_when_shown() {
        use crate::conversation::NodeId;
        use crate::tui::state::RowTarget;
        let (_tmp, tree) = tree_with(&["a1/a2", "a1/agents/1", "a1/agents/2", "b1/agents/1"]);
        let agent = |index| RowTarget::Agent {
            node: NodeId::new("a1"),
            index,
        };
        let layout = |expanded: &[&str], shown: &[&str]| {
            let rows = with_agents(layout_rows(&tree, &ids(expanded)), &tree, &ids(shown));
            rows.into_iter()
                .map(|row| (row.target, row.rails, row.branch))
                .collect::<Vec<_>>()
        };
        let node = |name| RowTarget::Node(NodeId::new(name));

        assert_eq!(
            layout(&["a1"], &[]),
            [
                (node("a1"), vec![], Branch::None),
                (node("a2"), vec![false], Branch::LastStart),
                (node("b1"), vec![], Branch::None),
            ]
        );
        // The thread below the root keeps the arm going past the runs.
        assert_eq!(
            layout(&["a1"], &["a1"]),
            [
                (node("a1"), vec![], Branch::None),
                (agent(1), vec![true], Branch::Start),
                (agent(2), vec![true], Branch::Start),
                (node("a2"), vec![false], Branch::LastStart),
                (node("b1"), vec![], Branch::None),
            ]
        );
        assert_eq!(
            layout(&[], &["a1"]),
            [
                (node("a1"), vec![], Branch::None),
                (agent(1), vec![true], Branch::Start),
                (agent(2), vec![false], Branch::LastStart),
                (node("b1"), vec![], Branch::None),
            ]
        );
    }
}
