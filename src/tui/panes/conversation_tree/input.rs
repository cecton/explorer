#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::time::{Duration, Instant};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// The rows as the event found them, with the row the selection sits on and how many rows
/// a page covers.
#[derive(Clone, Copy)]
struct Shown<'a> {
    rows: &'a [crate::tui::panes::conversation_tree::layout::TreeRow],
    index: usize,
    page: usize,
}

impl crate::tui::panes::conversation_tree::ConversationTreeComponent {
    /// Routes one event to navigation, the open filter, the tree's keys or a click.
    pub(crate) fn handle_input(
        &mut self,
        state: &mut crate::tui::state::AppState,
        main_tx: crate::signal::MainTx,
        input_event: InputEvent,
    ) -> EventPropagation {
        use crate::tui::pane_manager::Window;
        use crate::tui::panes::conversation_tree::layout::selected_index;
        let (rows, _) = self.rows(state);
        let index = selected_index(
            &rows,
            state.tree_view.selected.as_ref(),
            state.tree_view.filtering(),
        );
        let page = state
            .pane_manager
            .window_page_size(&Window::ConversationTree)
            .saturating_sub(1)
            .max(1);
        let shown = Shown {
            rows: &rows,
            index,
            page,
        };

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

        if Self::navigate(state, shown, &input_event) {
            return EventPropagation::ConsumedRender;
        }
        if state.tree_view.search_active && matches!(input_event, InputEvent::Keyboard(_)) {
            self.handle_search_key(state, &input_event, &rows, index);
            return EventPropagation::ConsumedRender;
        }

        match &input_event {
            InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::MouseDown(Button::Left),
                pos,
                ..
            }) => self.handle_click(state, main_tx, *pos, &rows),
            input_event => self.handle_key(state, main_tx, armed, shown, input_event),
        }
    }

    /// Arrows and pages move the selection, in the tree or its open filter; returns whether it did.
    fn navigate(
        state: &mut crate::tui::state::AppState,
        shown: Shown<'_>,
        input_event: &InputEvent,
    ) -> bool {
        let Shown { rows, index, page } = shown;
        let InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(key),
        }) = input_event
        else {
            return false;
        };
        let delta = match key {
            SpecialKey::Up => -1,
            SpecialKey::Down => 1,
            SpecialKey::PageUp => -(page as isize),
            SpecialKey::PageDown => page as isize,
            _ => return false,
        };
        Self::move_selection(state, rows, index, delta);
        true
    }

    /// The open filter's keys: Enter validates, Esc clears, every other key edits the query.
    fn handle_search_key(
        &mut self,
        state: &mut crate::tui::state::AppState,
        input_event: &InputEvent,
        rows: &[crate::tui::panes::conversation_tree::layout::TreeRow],
        index: usize,
    ) {
        match input_event {
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
                state.tree_view.query.handle_key(input_event);
                if state.tree_view.query.text() != before {
                    state.tree_view.selected = if state.tree_view.filtering() {
                        self.rows(state).0.first().map(|row| row.target.clone())
                    } else {
                        state.tree_view.before_search.clone()
                    };
                    self.shown_selection = None;
                    state.mark_session_dirty();
                }
            }
        }
    }

    fn handle_key(
        &mut self,
        state: &mut crate::tui::state::AppState,
        main_tx: crate::signal::MainTx,
        armed: Option<crate::conversation::NodeId>,
        shown: Shown<'_>,
        input_event: &InputEvent,
    ) -> EventPropagation {
        use crate::tui::modal::model_picker;
        use crate::tui::pane_manager::Window;
        use crate::tui::panes::conversation_tree::layout::Fold;
        use crate::tui::panes::discussion::DiscussionComponent;
        use crate::tui::state::ModelPickerPurpose;
        use crate::tui::state::RowTarget;
        let Shown { rows, index, .. } = shown;
        let window = Window::ConversationTree;
        match input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('/'),
            }) => {
                state.tree_view.search_active = true;
                state.tree_view.before_search = rows.get(index).map(|row| row.target.clone());
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Home),
            }) => Self::select(state, rows, 0),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::End),
            }) => Self::select(state, rows, rows.len().saturating_sub(1)),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Right),
            }) => {
                let Some(row) = rows.get(index) else {
                    return EventPropagation::ConsumedRender;
                };
                let Some(id) = row.target.node().cloned() else {
                    return EventPropagation::ConsumedRender;
                };
                if row.fold == Fold::Collapsed {
                    state.tree_view.expanded.insert(id.clone());
                    state.mark_session_dirty();
                }
                let end = match row.fold {
                    Fold::Leaf => None,
                    _ => state.conversations.read().thread_end(&id),
                };
                if let Some(end) = end {
                    state.tree_view.selected = Some(RowTarget::Node(end));
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Left),
            }) => {
                let Some(row) = rows.get(index) else {
                    return EventPropagation::ConsumedRender;
                };
                if let RowTarget::Agent { node, .. } = &row.target {
                    state.tree_view.selected = Some(RowTarget::Node(node.clone()));
                    return EventPropagation::ConsumedRender;
                }
                let Some(id) = row.target.node().cloned() else {
                    return EventPropagation::ConsumedRender;
                };
                if row.fold == Fold::Expanded {
                    state.tree_view.expanded.remove(&id);
                    state.mark_session_dirty();
                } else {
                    let ancestor = Self::foldable_ancestor(rows, &state.conversations.read(), &id);
                    if let Some(ancestor) = ancestor {
                        Self::select(state, rows, ancestor);
                    }
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }) => match rows.get(index).map(|row| &row.target) {
                Some(RowTarget::Queued { window, .. }) => {
                    crate::tui::panes::discussion::queue::take_back_window(state, main_tx, *window);
                }
                Some(RowTarget::Node(id)) => {
                    crate::tui::panes::discussion::queue::take_back(state, main_tx, id);
                }
                Some(RowTarget::Agent { .. }) | None => {}
            },
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Backspace),
            }) => {
                state.pane_manager.send_to_back(&window);
                state.mark_session_dirty();
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Enter),
            }) => {
                if let Some(row) = rows.get(index) {
                    Self::open_row(state, main_tx, &row.target);
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('s'),
            }) => {
                let node = match rows.get(index).map(|row| &row.target) {
                    Some(RowTarget::Node(id)) => Some(id.clone()),
                    Some(RowTarget::Agent { node, .. }) => Some(node.clone()),
                    Some(RowTarget::Queued { .. }) | None => None,
                };
                if let Some(node) = node
                    && state
                        .conversations
                        .read()
                        .node(&node)
                        .is_some_and(|shown| !shown.agents.is_empty())
                {
                    let view = &mut state.tree_view;
                    if view.shown_agents.remove(&node) {
                        if matches!(view.selected, Some(RowTarget::Agent { .. })) {
                            view.selected = Some(RowTarget::Node(node));
                        }
                    } else {
                        view.shown_agents.insert(node);
                    }
                }
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('n'),
            }) => DiscussionComponent::open_new_root(state),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('m'),
            }) => {
                if let Some(id) = rows.get(index).and_then(|row| row.target.node()) {
                    let purpose = ModelPickerPurpose::Duplicate(id.clone());
                    model_picker::open(state, main_tx, purpose);
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
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('d'),
            }) => {
                let Some(row) = rows.get(index) else {
                    return EventPropagation::ConsumedRender;
                };
                let Some(id) = row.target.node().cloned() else {
                    return EventPropagation::ConsumedRender;
                };
                if armed.as_ref() == Some(&id) {
                    self.delete_selected(state, rows, index);
                } else {
                    state.tree_view.selected = Some(RowTarget::Node(id.clone()));
                    state.tree_view.armed_delete = Some(id);
                }
            }
            InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::ScrollUp,
                ..
            }) => {
                let scroll = state.pane_manager.window_scroll(&window);
                state.pane_manager.set_window_scroll(
                    &window,
                    scroll.saturating_sub(crate::tui::pane_manager::WHEEL_STEP),
                );
            }
            InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::ScrollDown,
                ..
            }) => {
                let scroll = state.pane_manager.window_scroll(&window);
                state
                    .pane_manager
                    .set_window_scroll(&window, scroll + crate::tui::pane_manager::WHEEL_STEP);
                state.pane_manager.clamp_scroll(&window);
            }
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::SpecialKey(key @ (SpecialKey::Up | SpecialKey::Down)),
                mask:
                    ModifierKeysMask {
                        shift_key_state: KeyState::Pressed,
                        ctrl_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => {
                let scroll = state.pane_manager.window_scroll(&window);
                let target = match key {
                    SpecialKey::Up => scroll.saturating_sub(1),
                    _ => scroll + 1,
                };
                state.pane_manager.set_window_scroll(&window, target);
                state.pane_manager.clamp_scroll(&window);
            }
            _ => return EventPropagation::Propagate,
        }
        EventPropagation::ConsumedRender
    }

    /// A click selects the row under it, and a second one opens what Enter would. A
    /// click on a node's commit label opens that commit instead.
    fn handle_click(
        &mut self,
        state: &mut crate::tui::state::AppState,
        main_tx: crate::signal::MainTx,
        pos: VPPos,
        rows: &[crate::tui::panes::conversation_tree::layout::TreeRow],
    ) -> EventPropagation {
        use crate::tui::panes::git_log_pane::GitLogPaneComponent;
        let line = pos
            .row_index
            .as_usize()
            .checked_sub(self.content_origin_row);
        let column = pos
            .col_index
            .as_usize()
            .saturating_sub(self.content_origin_col);
        let Some(row) = line.and_then(|line| self.line_rows.get(line)).copied() else {
            return EventPropagation::ConsumedRender;
        };
        let on_label = line
            .and_then(|line| self.label_zones.get(line).copied().flatten())
            .is_some_and(|(start, width)| (start..start + width).contains(&column));
        let Some(target) = rows.get(row) else {
            return EventPropagation::ConsumedRender;
        };
        Self::select(state, rows, row);
        if on_label && let Some(id) = target.target.node() {
            self.last_click = None;
            let hash = state
                .conversations
                .read()
                .node(id)
                .and_then(|node| node.meta.commits.last().cloned());
            if let Some(hash) = hash {
                GitLogPaneComponent::open_log_at_commit(state, main_tx, hash);
            }
            return EventPropagation::ConsumedRender;
        }
        let now = Instant::now();
        let double = self
            .last_click
            .is_some_and(|(at, last)| last == row && now.duration_since(at) < DOUBLE_CLICK);
        self.last_click = (!double).then_some((now, row));
        if double {
            Self::open_row(state, main_tx, &target.target);
        }
        EventPropagation::ConsumedRender
    }

    /// Enter and a double click: show a queued window, or open a node or sub-agent run.
    fn open_row(
        state: &mut crate::tui::state::AppState,
        main_tx: crate::signal::MainTx,
        target: &crate::tui::state::RowTarget,
    ) {
        use crate::tui::panes::discussion::DiscussionComponent;
        use crate::tui::state::RowTarget;
        match target {
            RowTarget::Queued { window, .. } => DiscussionComponent::show(state, *window),
            RowTarget::Node(id) => DiscussionComponent::open(state, main_tx, id.clone(), None),
            RowTarget::Agent { node, index } => {
                DiscussionComponent::open(state, main_tx, node.clone(), Some(*index));
            }
        }
    }

    /// Selects `target` for the unfiltered tree, opening the folds that hide its node.
    fn reveal(state: &mut crate::tui::state::AppState, target: crate::tui::state::RowTarget) {
        if let Some(id) = target.node() {
            let folds = state.conversations.read().folds_on_the_way(id);
            state
                .tree_view
                .expanded
                .extend(folds.into_iter().filter(|fold| fold != id));
        }
        state.tree_view.selected = Some(target);
    }

    fn select(
        state: &mut crate::tui::state::AppState,
        rows: &[crate::tui::panes::conversation_tree::layout::TreeRow],
        index: usize,
    ) {
        if let Some(row) = rows.get(index.min(rows.len().saturating_sub(1))) {
            state.tree_view.selected = Some(row.target.clone());
        }
    }

    /// Moves the selection `delta` rows from `index`, stopping at either end of the tree.
    fn move_selection(
        state: &mut crate::tui::state::AppState,
        rows: &[crate::tui::panes::conversation_tree::layout::TreeRow],
        index: usize,
        delta: isize,
    ) {
        let index = if delta < 0 {
            index.saturating_sub(delta.unsigned_abs())
        } else {
            index.saturating_add(delta as usize)
        };
        Self::select(state, rows, index);
    }

    /// Nearest ancestor that can be folded: the fork or root a branch hangs from.
    fn foldable_ancestor(
        rows: &[crate::tui::panes::conversation_tree::layout::TreeRow],
        tree: &crate::conversation::ConversationTree,
        id: &crate::conversation::NodeId,
    ) -> Option<usize> {
        use crate::tui::panes::conversation_tree::layout::Fold;
        let mut current = tree.node(id)?.parent.clone();
        while let Some(ancestor) = current {
            let found = rows
                .iter()
                .position(|row| row.target.node() == Some(&ancestor) && row.fold != Fold::Leaf);
            if let Some(index) = found {
                return Some(index);
            }
            current = tree.node(&ancestor)?.parent.clone();
        }
        None
    }

    fn delete_selected(
        &mut self,
        state: &mut crate::tui::state::AppState,
        rows: &[crate::tui::panes::conversation_tree::layout::TreeRow],
        index: usize,
    ) {
        use crate::tui::pane_manager::Window;
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
            state.tree_view.shown_agents.remove(id);
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
            crate::tui::focus::close_window(state, Window::Discussion(id));
        }
        let rows = self.rows(state).0;
        state.tree_view.selected = None;
        Self::select(state, &rows, index);
        state.mark_session_dirty();
    }

    fn open_commits(
        state: &mut crate::tui::state::AppState,
        main_tx: crate::signal::MainTx,
        id: &crate::conversation::NodeId,
    ) {
        use crate::tui::panes::git_log_pane::GitLogPaneComponent;
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
}
