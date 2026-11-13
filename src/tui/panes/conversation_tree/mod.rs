#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::collections::HashSet;
use std::time::Instant;

pub(crate) mod filter;
pub(crate) mod input;
pub(crate) mod layout;
pub(crate) mod render;

#[cfg(test)]
pub(crate) mod test_support;

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
    shown_selection: Option<crate::tui::state::RowTarget>,
    /// Time and row of the last click, to recognize a double click.
    last_click: Option<(Instant, usize)>,
    /// The filter's last answer, so a frame that changes neither the query nor the tree
    /// does not run it again.
    filter_cache: filter::FilterCache,
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
            filter_cache: filter::FilterCache::default(),
        }
    }

    pub(crate) fn title(state: &crate::tui::state::AppState) -> String {
        // The tree is where a conversation starts, so it is where the repository says
        // whether one can: its title carries that instead of a status-bar segment.
        let name = if state.trusted {
            Self::TITLE.to_string()
        } else {
            format!("{} (untrusted)", Self::TITLE)
        };
        if state.tree_view.filtering() {
            format!("{name} — {}", state.tree_view.query.text().trim())
        } else {
            name
        }
    }

    /// The rows the window shows, and what matched in them while a filter holds a word.
    pub(crate) fn rows(
        &mut self,
        state: &crate::tui::state::AppState,
    ) -> (Vec<layout::TreeRow>, std::sync::Arc<[filter::FilterMatch]>) {
        let tree = state.conversations.read();
        layout::rows(state, &tree, &mut self.filter_cache)
    }

    /// Asks the summary model again for an untitled node the cursor just landed on.
    fn retitle_selected(
        state: &mut crate::tui::state::AppState,
        main_tx: crate::signal::MainTx,
        id: crate::conversation::NodeId,
    ) {
        use crate::signal::AppSignal;
        let Some(backend) = state
            .agent
            .summary_model
            .as_ref()
            .and_then(|summary| state.agent.backend(summary))
        else {
            return;
        };
        let (node_dir, session, meta) = {
            let tree = state.conversations.read();
            let Some(node) = tree.node(&id) else { return };
            let Some(node_dir) = tree.node_dir(&id) else {
                return;
            };
            let has_reply = node.title.is_none()
                && crate::conversation::files::has_content(
                    &crate::conversation::files::read_reply(&node_dir),
                );
            if !needs_retitle(&id, node, has_reply, &state.tree_view.retitling) {
                return;
            }
            let session = tree
                .lineage(&id)
                .first()
                .unwrap_or(&id)
                .as_str()
                .to_string();
            (node_dir, session, node.meta.clone())
        };
        state.tree_view.retitling.insert(id.clone());
        let root = state.root.clone();
        tokio::spawn(async move {
            crate::agent::turn::title::retitle(
                &id,
                &node_dir,
                &session,
                &root,
                meta.head_before,
                meta.head_after,
                &backend,
                &main_tx,
            )
            .await;
            crate::signal::send(&main_tx, AppSignal::NodeRetitleEnded { node: id });
        });
    }
}

impl crate::tui::widgets::title_row::TitleRow for ConversationTreeComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        state: &mut crate::tui::state::AppState,
    ) -> usize {
        if !state.tree_view.search_active {
            let title = Self::title(state);
            crate::tui::widgets::title_row::render_pane_title(
                ops,
                pane_box,
                &title,
                false,
                &state.theme,
                focused,
            );
            return 1;
        }
        crate::tui::widgets::title_row::render_input_title(
            ops,
            pane_box,
            &mut state.tree_view.query,
            "/",
            focused,
            &state.theme,
            1,
        )
    }
}

/// Whether an untitled finished node has something to describe and no retitle is running.
fn needs_retitle(
    id: &crate::conversation::NodeId,
    node: &crate::conversation::Node,
    has_reply: bool,
    retitling: &HashSet<crate::conversation::NodeId>,
) -> bool {
    use crate::conversation::NodeStatus;
    node.title.is_none()
        && node.meta.status != Some(NodeStatus::Running)
        && (node.meta.status == Some(NodeStatus::Done)
            || has_reply
            || !node.meta.commits.is_empty())
        && !retitling.contains(id)
}

impl Component<crate::tui::state::AppState, crate::signal::AppSignal>
    for ConversationTreeComponent
{
    fn reset(&mut self) {}

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        input_event: InputEvent,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        use crate::tui::state::RowTarget;
        let before = global_data.state.tree_view.selected.clone();
        let main_tx = global_data.main_thread_channel_sender.clone();
        let propagation = self.handle_input(&mut global_data.state, main_tx, input_event);
        let state = &mut global_data.state;
        if state.tree_view.selected != before
            && let Some(RowTarget::Node(id)) = state.tree_view.selected.clone()
        {
            let main_tx = global_data.main_thread_channel_sender.clone();
            Self::retitle_selected(state, main_tx, id);
        }
        Ok(propagation)
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        current_box: FlexBox,
        _surface_bounds: VPBoundingBox,
        _has_focus: &mut HasFocus,
    ) -> CommonResult {
        self.render_pane(global_data, current_box)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_says_the_repository_is_untrusted() {
        use crate::tui::keymap::Keymap;
        use crate::tui::state::AppState;
        use crate::tui::theme::HelixTheme;
        let mut state = AppState::new(
            std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(Vec::new())),
            camino::Utf8PathBuf::from("/tmp"),
            HelixTheme::default(),
            Keymap::default(),
        );
        assert_eq!(
            ConversationTreeComponent::title(&state),
            "Conversations (untrusted)"
        );
        state.trusted = true;
        assert_eq!(ConversationTreeComponent::title(&state), "Conversations");
        state.tree_view.query.set_text("queue");
        state.tree_view.search_active = true;
        assert_eq!(
            ConversationTreeComponent::title(&state),
            "Conversations — queue"
        );
    }

    #[test]
    fn only_finished_untitled_nodes_with_something_to_describe_are_retitled() {
        use crate::conversation::NodeId;
        use crate::conversation::NodeStatus;
        use crate::tui::panes::conversation_tree::test_support::tree_with;
        let (_tmp, tree) = tree_with(&["a1"]);
        let id = NodeId::new("a1");
        let mut node = tree.node(&id).unwrap().clone();
        let none = HashSet::new();
        node.meta.status = Some(NodeStatus::Failed);
        assert!(!needs_retitle(&id, &node, false, &none));
        assert!(needs_retitle(&id, &node, true, &none));
        node.meta.commits = vec!["abc".to_string()];
        assert!(needs_retitle(&id, &node, false, &none));
        node.meta.commits.clear();
        node.meta.status = Some(NodeStatus::Done);
        assert!(needs_retitle(&id, &node, false, &none));
        assert!(!needs_retitle(
            &id,
            &node,
            false,
            &HashSet::from([id.clone()])
        ));
        node.meta.status = Some(NodeStatus::Running);
        assert!(!needs_retitle(&id, &node, true, &none));
        node.meta.status = Some(NodeStatus::Done);
        node.title = Some("Titled".to_string());
        assert!(!needs_retitle(&id, &node, true, &none));
    }

    #[test]
    fn thread_end_is_the_last_reply_before_a_leaf_or_a_fork() {
        use crate::conversation::NodeId;
        use crate::tui::panes::conversation_tree::test_support::tree_with;
        let (_tmp, tree) = tree_with(&["a1/a2/a3", "b1/b2/b3/c4", "b1/b2/b3/d4", "e1"]);
        let end = |head: &str| tree.thread_end(&NodeId::new(head));
        assert_eq!(end("a1"), Some(NodeId::new("a3")));
        assert_eq!(end("b1"), Some(NodeId::new("b2")));
        assert_eq!(end("b3"), None);
        assert_eq!(end("e1"), None);
    }

    #[test]
    fn folds_on_the_way_are_the_forks_and_thread_heads() {
        use crate::conversation::NodeId;
        use crate::tui::panes::conversation_tree::test_support::tree_with;
        let (_tmp, tree) = tree_with(&["a1/a2/a3/b4/b5", "a1/a2/a3/c4"]);
        let folds = tree.folds_on_the_way(&NodeId::new("b5"));
        let names: Vec<&str> = folds.iter().map(NodeId::as_str).collect();
        assert_eq!(names, ["a1", "a3", "b4"]);
    }
}
