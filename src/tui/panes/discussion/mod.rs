#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

pub(crate) mod document;
pub(crate) mod launch;
pub(crate) mod queue;
pub(crate) mod selection;
pub(crate) mod status;

/// A mouse drag over the document, from the word under the first click.
struct Drag {
    window_id: usize,
    kind: crate::tui::panes::discussion::selection::DragKind,
    part: crate::tui::pane_manager::DocPart,
    /// Byte offset under the click, and the word around it.
    anchor: usize,
    word: (usize, usize),
}

pub struct DiscussionComponent {
    id: FlexBoxId,
    window_id: usize,
    cache: Option<crate::tui::panes::discussion::document::DocCache>,
    /// Screen row and column of the document area at last render, and its height, used
    /// for mouse event handling.
    doc_origin: (usize, usize),
    doc_rows: usize,
    doc_width: usize,
    status_rows: usize,
    drag: Option<Drag>,
}

impl DiscussionComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self {
            id,
            window_id: 0,
            cache: None,
            doc_origin: (0, 0),
            doc_rows: 0,
            doc_width: 0,
            status_rows: 0,
            drag: None,
        }
    }

    pub fn set_window_id(&mut self, window_id: usize) {
        self.window_id = window_id;
    }

    /// Drops the drag in progress, if any, and its selection.
    pub(crate) fn cancel_drag(&mut self, state: &mut crate::tui::state::AppState) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        use crate::tui::panes::discussion::selection::DragKind;
        if matches!(drag.kind, DragKind::Select) {
            state.text_selection = None;
        }
        state.mouse_drag_active = false;
    }

    /// The rows the pane's scrollbar spans: the document and the status line, not the input.
    pub fn scrollbar_rows(&self) -> usize {
        self.doc_rows + self.status_rows
    }
    fn in_doc(&self, pos: VPPos) -> bool {
        let (origin_row, origin_col) = self.doc_origin;
        let row = pos.row_index.as_usize();
        let col = pos.col_index.as_usize();
        (origin_row..origin_row + self.doc_rows).contains(&row)
            && (origin_col..origin_col + self.doc_width).contains(&col)
    }
    /// The text and source offset under a screen position, clamped to the document area.
    fn locate(
        &self,
        state: &crate::tui::state::AppState,
        discussion: &crate::tui::state::DiscussionState,
        window_id: usize,
        pos: VPPos,
    ) -> Option<(crate::tui::pane_manager::DocPart, usize)> {
        use crate::tui::pane_manager::Window;
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
        crate::tui::panes::discussion::selection::locate(&cache.lines, scroll + row, col)
    }

    pub(crate) fn show(state: &mut crate::tui::state::AppState, id: usize) {
        use crate::tui::pane_manager::Window;
        crate::tui::focus::focus_window(state, Window::Discussion(id));
        state.mark_session_dirty();
    }

    /// Runs the prompt of `node` again as a new sibling, with the model `spec`, and
    /// opens it. A refusal is reported in the tree.
    pub(crate) fn duplicate(
        state: &mut crate::tui::state::AppState,
        main_tx: crate::signal::MainTx,
        node: &crate::conversation::NodeId,
        spec: crate::model_spec::ModelSpec,
    ) {
        let launch = {
            let tree = state.conversations.read();
            let dir = tree.node_dir(node);
            let prompt = dir
                .as_ref()
                .map(|dir| crate::conversation::read_prompt_and_reply(dir).0)
                .unwrap_or_default();
            let input = dir.and_then(|dir| crate::conversation::read_typed_input(&dir));
            crate::tui::panes::discussion::launch::Launch::replay(&tree, node, prompt, spec)
                .map(|launch| crate::tui::panes::discussion::launch::Launch { input, ..launch })
        };
        let Some(launch) = launch else {
            return;
        };
        match crate::tui::panes::discussion::launch::start_turn(state, main_tx.clone(), launch) {
            Ok(sibling) => Self::open(state, main_tx, sibling, None),
            Err(refusal) => state.tree_view.notice = Some(refusal.into_reason()),
        }
    }

    /// Opens a window on the parent of `node` with the input prefilled with the node's
    /// prompt as it was typed, so that an edited version can be sent as a new sibling.
    pub(crate) fn open_edit(
        state: &mut crate::tui::state::AppState,
        main_tx: crate::signal::MainTx,
        node: &crate::conversation::NodeId,
    ) {
        use crate::tui::state::DiscussionState;
        use crate::tui::widgets::input_line::InputLine;
        let (parent, draft, model) = {
            let tree = state.conversations.read();
            let Some(source) = tree.node(node) else {
                return;
            };
            let draft = tree
                .node_dir(node)
                .map(|dir| crate::conversation::read_input(&dir))
                .unwrap_or_default();
            (
                source.parent.clone(),
                draft,
                crate::tui::panes::discussion::launch::node_model(source),
            )
        };
        let id = state.next_discussion_id;
        state.next_discussion_id += 1;
        state.discussions.insert(
            id,
            DiscussionState {
                loaded: parent.is_none(),
                node: parent,
                draft: InputLine::with_text(draft),
                model,
                ..Default::default()
            },
        );
        Self::load_content(state, main_tx, id);
        Self::show(state, id);
    }

    /// Focuses the window already showing `node` (or sub-agent run `index` of it), or
    /// opens a new one and loads what it shows from disk. The window starts with the
    /// model that ran it and an empty draft: opening never runs anything.
    pub(crate) fn open(
        state: &mut crate::tui::state::AppState,
        main_tx: crate::signal::MainTx,
        node: crate::conversation::NodeId,
        agent: Option<u32>,
    ) {
        use crate::tui::state::DiscussionState;
        let existing = state
            .discussions
            .iter()
            .find(|(_, discussion)| {
                discussion.node.as_ref() == Some(&node) && discussion.agent == agent
            })
            .map(|(id, _)| *id);
        let id = existing.unwrap_or_else(|| {
            let id = state.next_discussion_id;
            state.next_discussion_id += 1;
            let model = {
                let tree = state.conversations.read();
                match agent {
                    Some(index) => tree
                        .agent(&node, index)
                        .and_then(|agent| agent.meta.model_spec()),
                    None => tree
                        .node(&node)
                        .and_then(crate::tui::panes::discussion::launch::node_model),
                }
            };
            state.discussions.insert(
                id,
                DiscussionState {
                    node: Some(node),
                    agent,
                    model,
                    ..Default::default()
                },
            );
            Self::load_content(state, main_tx, id);
            id
        });
        Self::show(state, id);
    }

    /// Reads again the windows showing sub-agent runs of `node`, or only run `index`:
    /// while a run is in progress they follow its live output, which is gone once it
    /// ends.
    pub(crate) fn reload_agent_windows(
        state: &crate::tui::state::AppState,
        main_tx: crate::signal::MainTx,
        node: &crate::conversation::NodeId,
        index: Option<u32>,
    ) {
        let shown: Vec<usize> = state
            .discussions
            .iter()
            .filter(|(_, discussion)| {
                discussion.node.as_ref() == Some(node)
                    && discussion.agent.is_some()
                    && index.is_none_or(|index| discussion.agent == Some(index))
            })
            .map(|(id, _)| *id)
            .collect();
        for id in shown {
            Self::load_content(state, main_tx.clone(), id);
        }
    }

    /// Opens a window for a new conversation, offering the model last picked.
    pub(crate) fn open_new_root(state: &mut crate::tui::state::AppState) {
        use crate::tui::state::DiscussionState;
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

    /// Reads the prompt and reply of window `id`'s node or sub-agent run off the UI
    /// thread.
    pub(crate) fn load_content(
        state: &crate::tui::state::AppState,
        main_tx: crate::signal::MainTx,
        id: usize,
    ) {
        use crate::signal::AppSignal;
        let Some(discussion) = state.discussions.get(&id) else {
            return;
        };
        let Some(node) = discussion.node.clone() else {
            return;
        };
        let dir = {
            let tree = state.conversations.read();
            match discussion.agent {
                Some(index) => tree.agent_dir(&node, index),
                None => tree.node_dir(&node),
            }
        };
        let Some(dir) = dir else {
            return;
        };
        tokio::task::spawn_blocking(move || {
            let (prompt, reply) = crate::conversation::read_prompt_and_reply(&dir);
            let work_log = crate::conversation::read_work_log(&dir);
            crate::signal::send(
                &main_tx,
                AppSignal::NodeContentLoaded {
                    window_id: id,
                    node,
                    prompt,
                    reply,
                    work_log,
                },
            );
        });
    }

    /// Sends the draft of window `id`. A turn running on the node it shows takes it
    /// directly: it joins the loop at its next round and the draft is cleared. A prompt
    /// for any other turn is queued, and stays in the input until its turn starts. A
    /// refused send keeps the draft and explains itself in the window's notice.
    fn send(state: &mut crate::tui::state::AppState, main_tx: crate::signal::MainTx, id: usize) {
        use crate::tui::panes::discussion::launch::Refusal;
        use crate::tui::panes::pickers::model_picker::ModelPickerComponent;
        use crate::tui::state::ModelPickerPurpose;
        use crate::tui::state::PendingTurn;
        if Self::inject(state, id) {
            return;
        }
        let has_model = {
            let Some(discussion) = state.discussions.get(&id) else {
                return;
            };
            if discussion.draft.text().trim().is_empty() {
                return;
            }
            discussion.model.is_some()
        };
        // A queue held back goes first, so that this prompt cannot overtake it.
        Self::release_queue(state, main_tx.clone());
        if state.queued_turns.iter().any(|entry| entry.window == id) {
            return;
        }
        if !has_model {
            ModelPickerComponent::open(state, main_tx, ModelPickerPurpose::Window(id));
            return;
        }
        match crate::tui::panes::discussion::launch::launch_window(state, main_tx, id) {
            None | Some(Ok(())) => {}
            Some(Err(Refusal::Busy(_))) => {
                let Some(discussion) = state.discussions.get_mut(&id) else {
                    return;
                };
                discussion.notice = None;
                let parent = discussion.node.clone();
                state.queued_turns.push(PendingTurn { window: id, parent });
            }
            Some(Err(refusal)) => {
                if let Some(discussion) = state.discussions.get_mut(&id) {
                    discussion.notice = Some(refusal.into_reason());
                }
            }
        }
    }

    /// Hands the draft of window `id` to the turn running on the node it shows, when
    /// there is one: it joins the loop at its next round and the draft is cleared.
    /// Returns whether it did.
    fn inject(state: &mut crate::tui::state::AppState, id: usize) -> bool {
        let Some(discussion) = state.discussions.get_mut(&id) else {
            return false;
        };
        let typed = discussion.draft.text().trim().to_string();
        if typed.is_empty() {
            return false;
        }
        let running = discussion
            .node
            .clone()
            .and_then(|node| state.running_turns.get(&node));
        let Some(handle) = running else {
            return false;
        };
        {
            let mut live = handle.live.lock();
            if live.closed {
                return false;
            }
            live.injected.push_back(typed);
        }
        discussion.draft.clear();
        discussion.notice = None;
        state.mark_session_dirty();
        true
    }

    /// Makes window `id` show `node`, the node its draft was just sent as.
    fn apply_sent(
        state: &mut crate::tui::state::AppState,
        id: usize,
        node: crate::conversation::NodeId,
        prompt: String,
    ) {
        let Some(discussion) = state.discussions.get_mut(&id) else {
            return;
        };
        discussion.node = Some(node);
        discussion.prompt = prompt;
        discussion.reply.clear();
        discussion.work_log.clear();
        discussion.log_start = 0;
        discussion.thinking.clear();
        discussion.show_work_log = None;
        discussion.draft.clear();
        discussion.loaded = true;
        discussion.notice = None;
        crate::tui::panes::discussion::document::start_view(state, id);
    }

    pub(crate) fn title(state: &crate::tui::state::AppState, window_id: usize) -> String {
        let tree = state.conversations.read();
        state
            .discussions
            .get(&window_id)
            .and_then(|discussion| {
                let node = discussion.node.as_ref()?;
                match discussion.agent {
                    Some(index) => tree.agent(node, index).map(|agent| agent.title()),
                    None => tree.node(node).map(|node| {
                        let marker = if node.meta.favorite { "* " } else { "" };
                        format!("{marker}{}", node.display_title())
                    }),
                }
            })
            .unwrap_or_else(|| "New conversation".to_string())
    }

    pub fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        state: &mut crate::tui::state::AppState,
        window_id: usize,
    ) -> usize {
        let theme = &state.theme;
        let title = Self::title(state, window_id);
        let model = state
            .discussions
            .get(&window_id)
            .and_then(|discussion| discussion.model.as_ref())
            .map_or_else(
                || {
                    format!(
                        "no model ({})",
                        crate::tui::panes::discussion::status::model_picker_chord(&state.keymap)
                    )
                },
                |spec| spec.short_label(),
            );

        let pane_width = pane_box
            .style_adjusted_bounds
            .bounds_size
            .col_width
            .as_usize();
        let label = format!(" {model} ");
        let label_width = crate::tui::widgets::text_layout::display_width(&label);
        let title_box = FlexBox {
            style_adjusted_bounds: VPBoundingBox::new(
                pane_box.style_adjusted_bounds.origin_pos,
                vp_width(pane_width.saturating_sub(label_width) as u16)
                    + pane_box.style_adjusted_bounds.bounds_size.row_height,
            ),
            ..*pane_box
        };
        crate::tui::widgets::title_row::render_pane_title(ops, pane_box, "", false, theme, focused);
        crate::tui::widgets::title_row::render_pane_title(
            ops, &title_box, &title, false, theme, focused,
        );
        if label_width < pane_width {
            let (bg, fg) = crate::tui::widgets::title_row::title_bar_colors(focused, theme);
            let style = new_style!(color_fg: {TuiColor::from(fg)} color_bg: {TuiColor::from(bg)});
            *ops += RenderOpCommon::MoveCursorPositionRelTo(
                pane_box.style_adjusted_bounds.origin_pos,
                vp_col((pane_width - label_width) as u16) + vp_row(0),
            );
            *ops += RenderOpCommon::ApplyColors(Some(style));
            *ops += RenderOpIR::PaintTextWithAttributes(label.into(), Some(style));
        }
        1
    }
}

impl Component<crate::tui::state::AppState, crate::signal::AppSignal> for DiscussionComponent {
    fn reset(&mut self) {
        self.cache = None;
        self.drag = None;
    }

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        input_event: InputEvent,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        use crate::tui::pane_manager::Window;
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
        // A sub-agent run is read-only: its window neither sends nor cancels.
        let read_only = discussion.agent.is_some();
        let running = discussion
            .node
            .as_ref()
            .filter(|_| !read_only)
            .and_then(|node| state.running_turns.get(node));
        let queued = state.queued_turns.iter().any(|entry| entry.window == id);
        let scroll = state.pane_manager.window_scroll(&window);
        let page = state.pane_manager.window_page_size(&window).max(1);

        let target = match &input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Enter),
            }) if !read_only => {
                Self::send(state, main_tx, id);
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }) if queued => {
                if let Some(node) = discussion.node.clone() {
                    crate::tui::panes::discussion::queue::take_back(state, main_tx, &node);
                }
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }) if running.is_some() => {
                let Some(node) = discussion.node.clone() else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                let Some(handle) = state.running_turns.get(&node).cloned() else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                // A message the turn has not read yet comes back to the input; only a
                // second Esc cancels the turn itself.
                if handle.live.lock().injected.is_empty() {
                    handle.cancel();
                } else {
                    Self::restore_injected(state, &node, &handle);
                }
                return Ok(EventPropagation::ConsumedRender);
            }
            // Ctrl+Plus arrives as `+` or `=`, shifted or not, depending on the terminal.
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('+' | '='),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        alt_key_state: KeyState::NotPressed,
                        ..
                    },
            }) if queued => {
                crate::tui::panes::discussion::queue::move_queued(state, id, true);
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('-'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        alt_key_state: KeyState::NotPressed,
                        ..
                    },
            }) if queued => {
                crate::tui::panes::discussion::queue::move_queued(state, id, false);
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
                let shown = crate::tui::panes::discussion::status::work_log_shown(
                    &state.conversations.read(),
                    discussion,
                );
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
            }) => scroll.saturating_sub(crate::tui::panes::discussion::document::SCROLL_STEP),
            InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::ScrollDown,
                ..
            }) => scroll + crate::tui::panes::discussion::document::SCROLL_STEP,
            _ => {
                if !read_only && discussion.draft.handle_key(&input_event) {
                    discussion.notice = None;
                    state.mark_session_dirty();
                    return Ok(EventPropagation::ConsumedRender);
                }
                return Ok(EventPropagation::Propagate);
            }
        };
        state.pane_manager.set_window_scroll(&window, target);
        state.pane_manager.clamp_scroll(&window);
        Ok(EventPropagation::ConsumedRender)
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        current_box: FlexBox,
        _surface_bounds: VPBoundingBox,
        has_focus: &mut HasFocus,
    ) -> CommonResult {
        use crate::tui::pane_manager::DocPart;
        use crate::tui::pane_manager::SelPoint;
        use crate::tui::pane_manager::TextSelection;
        use crate::tui::pane_manager::Window;
        use crate::tui::widgets::markdown::Span;
        throws!({
            let origin = current_box.style_adjusted_bounds.origin_pos;
            let bounds = current_box.style_adjusted_bounds.bounds_size;
            let total_rows = bounds.row_height.as_usize();
            let pane_width = bounds.col_width.as_usize();
            // The pane's scrollbar takes the last column, down to the status line.
            let doc_width = pane_width.saturating_sub(1);
            let window = Window::Discussion(self.window_id);
            let focused = has_focus.get_id() == Some(self.id);
            let state = &mut global_data.state;

            // A running turn's reply lives in its handle; mirror it while it grows.
            let output = state
                .discussions
                .get(&self.window_id)
                .and_then(|discussion| {
                    crate::tui::panes::discussion::launch::running_output(state, discussion)
                });
            if let Some(output) = &output
                && let Some(discussion) = state.discussions.get_mut(&self.window_id)
            {
                let live = output.lock();
                if live.reply.len() != discussion.reply.len()
                    || live.work_log.len() != discussion.work_log.len()
                    || live.log_start() != discussion.log_start
                    || live.thinking.len() != discussion.thinking.len()
                    || live.prompt.len() != discussion.prompt.len()
                {
                    discussion.reply.clone_from(&live.reply);
                    discussion.work_log.clone_from(&live.work_log);
                    discussion.log_start = live.log_start();
                    discussion.thinking.clone_from(&live.thinking);
                    discussion.prompt.clone_from(&live.prompt);
                }
            } else if let Some(discussion) = state.discussions.get_mut(&self.window_id)
                && discussion.log_start != 0
            {
                // Once the turn is over, its whole work log is on screen again.
                discussion.log_start = 0;
            }
            let Some(discussion) = state.discussions.get(&self.window_id) else {
                return Ok(());
            };

            let theme = &state.theme;
            let pane_bg = theme
                .ui_bg("ui.background")
                .unwrap_or(crate::tui::widgets::title_row::DEFAULT_BG);
            let prompt_bg = theme
                .ui_bg("ui.cursorline")
                .or_else(|| theme.ui_bg("ui.cursorline.primary"))
                .or_else(|| theme.ui_bg("ui.statusline"))
                .unwrap_or(crate::tui::widgets::title_row::DEFAULT_STATUSLINE_BG);
            let text_fg = theme
                .ui_fg("ui.text")
                .unwrap_or(RgbValue::from_u8(170, 170, 200));
            let status_fg = theme.color_for_scope("comment").unwrap_or(text_fg);
            let error_fg = theme
                .ui_fg("error")
                .unwrap_or(crate::tui::widgets::title_row::DEFAULT_ERROR_FG);
            // A surface of its own, like Helix's help box: the input sits inside the
            // window, where the focused title colors would read as the window's own row.
            let input_bg = theme.ui_bg("ui.help").unwrap_or(pane_bg);
            // `ui.selection`, the regular color of the file preview.
            let selection_bg = theme
                .ui_bg("ui.selection")
                .unwrap_or(RgbValue::from_u8(50, 50, 90));
            let marker = crate::tui::panes::discussion::status::marker_bg(theme, pane_bg);

            let (status, alert) = crate::tui::panes::discussion::status::status_line(
                state,
                self.window_id,
                discussion,
            );
            let read_only = discussion.agent.is_some();
            let hint =
                crate::agent::commands::placeholder(discussion.draft.text(), &state.agent.commands)
                    .or_else(|| {
                        crate::tui::panes::discussion::status::resume_hint(state, discussion)
                    });
            let input_rows = if read_only {
                0
            } else {
                discussion
                    .draft
                    .height(
                        hint.as_deref(),
                        "",
                        pane_width as u16,
                        (total_rows / 2).max(1),
                    )
                    .min(total_rows)
            };
            let status_rows = usize::from(total_rows > input_rows);
            let doc_rows = total_rows - input_rows - status_rows;

            let mut input_ops = RenderOpIRVec::new();
            let input_origin = origin + vp_height((doc_rows + status_rows) as u16);
            let input_style = new_style!(
                color_fg: {TuiColor::from(text_fg)} color_bg: {TuiColor::from(input_bg)}
            );
            for row_offset in 0..input_rows {
                input_ops += RenderOpCommon::MoveCursorPositionRelTo(
                    input_origin,
                    vp_col(0) + vp_row(row_offset as u16),
                );
                input_ops += RenderOpCommon::ApplyColors(Some(input_style));
                input_ops += RenderOpIR::PaintTextWithAttributes(
                    " ".repeat(pane_width).into(),
                    Some(input_style),
                );
            }
            self.doc_origin = (origin.row_index.as_usize(), origin.col_index.as_usize());
            self.doc_rows = doc_rows;
            self.doc_width = doc_width;
            self.status_rows = status_rows;

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
                .filter(|_| !read_only)
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
                marked(DocPart::Thinking),
            ];
            let marks_of = |part: Option<DocPart>| -> (&[(usize, usize)], RgbValue, bool) {
                match part {
                    Some(DocPart::Prompt) => (&marks[0].0, marks[0].1, marks[0].2),
                    Some(DocPart::WorkLog) => (&marks[1].0, marks[1].1, marks[1].2),
                    Some(DocPart::Reply) => (&marks[2].0, marks[2].1, marks[2].2),
                    Some(DocPart::Thinking) => (&marks[3].0, marks[3].1, marks[3].2),
                    None => (&[], pane_bg, true),
                }
            };

            let shown = crate::tui::panes::discussion::status::work_log_shown(
                &state.conversations.read(),
                discussion,
            );
            let running = output.is_some();
            let lines = crate::tui::panes::discussion::document::doc_lines(
                self, discussion, shown, theme, doc_width,
            );
            let line_count = lines.len();
            let max_scroll = line_count.saturating_sub(doc_rows);
            let stored = state.pane_manager.window_scroll(&window);
            let old_max = state
                .pane_manager
                .window_scroll_max(&window)
                .saturating_sub(state.pane_manager.window_page_size(&window));
            // The view rides the bottom of the document across anything that changes how
            // much of it fits, `old_max` being the bottom of the previous frame. A running
            // turn rides it from its first frame, where the document still fits and its
            // two ends are the same row; a finished one only while the user left it there,
            // so a scroll away from the bottom holds.
            let follow = (running || stored > 0) && stored >= old_max;
            let scroll = if follow {
                max_scroll
            } else {
                stored.min(max_scroll)
            };

            let mut render_ops = RenderOpIRVec::new();
            let paint_row = |ops: &mut RenderOpIRVec,
                             row_offset: usize,
                             spans: &[Span],
                             fg: RgbValue,
                             bg: RgbValue,
                             marked: &[(usize, usize)],
                             marked_bg: RgbValue,
                             keep_fg: bool| {
                let strip_style =
                    new_style!(color_fg: {TuiColor::from(fg)} color_bg: {TuiColor::from(bg)});
                *ops += RenderOpCommon::MoveCursorPositionRelTo(
                    origin,
                    vp_col(0) + vp_row(row_offset as u16),
                );
                *ops += RenderOpCommon::ApplyColors(Some(strip_style));
                *ops += RenderOpIR::PaintTextWithAttributes(
                    " ".repeat(doc_width).into(),
                    Some(strip_style),
                );
                let mut x = 0usize;
                for piece in crate::tui::panes::discussion::selection::row_pieces(spans, marked) {
                    let truncated;
                    let text: &str =
                        if x + crate::tui::widgets::text_layout::display_width(piece.text)
                            > doc_width
                        {
                            truncated = crate::tui::widgets::text_layout::truncate(
                                piece.text,
                                doc_width.saturating_sub(x),
                            );
                            truncated.as_str()
                        } else {
                            piece.text
                        };
                    let mut style = crate::tui::widgets::markdown::span_style(piece.span, bg);
                    if piece.marked {
                        style.color_bg = Some(TuiColor::from(marked_bg));
                        if !keep_fg {
                            style.color_fg = Some(TuiColor::from(
                                crate::tui::panes::preview::ensure_readable_fg(
                                    piece.span.fg,
                                    marked_bg,
                                ),
                            ));
                        }
                    }
                    *ops += RenderOpCommon::MoveCursorPositionRelTo(
                        origin,
                        vp_col(x as u16) + vp_row(row_offset as u16),
                    );
                    *ops += RenderOpCommon::ApplyColors(Some(style));
                    *ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
                    x += crate::tui::widgets::text_layout::display_width(text);
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
                let text = crate::tui::widgets::text_layout::truncate(&status, doc_width);
                let span = [crate::tui::widgets::markdown::Span::plain(text, fg)];
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
            if !read_only && let Some(discussion) = state.discussions.get_mut(&self.window_id) {
                discussion.draft.render(
                    &mut input_ops,
                    hint.as_deref()
                        .map(|hint| (hint, TuiColor::from(status_fg))),
                    "",
                    input_origin,
                    pane_width as u16,
                    input_rows,
                    focused,
                    (
                        TuiColor::from(input_bg),
                        TuiColor::from(text_fg),
                        TuiColor::from(selection_bg),
                    ),
                );
            }
            global_data.pipeline.push(ZOrder::Normal, render_ops);
            global_data.pipeline.push(ZOrder::Normal, input_ops);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::panes::discussion::document::DocLine;
    use crate::tui::panes::discussion::document::part_text;
    use crate::tui::panes::discussion::launch::Launch;
    use crate::tui::panes::discussion::launch::Refusal;
    use crate::tui::panes::discussion::launch::TurnKind;
    use crate::tui::panes::discussion::launch::compose_launch;
    use crate::tui::panes::discussion::launch::lock_refusal;
    use crate::tui::panes::discussion::launch::node_mode;
    use crate::tui::panes::discussion::launch::node_model;
    use crate::tui::panes::discussion::launch::start_turn;
    use crate::tui::panes::discussion::queue::drop_queue_entry;
    use crate::tui::panes::discussion::queue::move_queued;
    use crate::tui::panes::discussion::queue::ordinal;
    use crate::tui::panes::discussion::queue::queue_hint;
    use crate::tui::panes::discussion::queue::take_back;
    use crate::tui::panes::discussion::selection::add_range;
    use crate::tui::panes::discussion::selection::locate;
    use crate::tui::panes::discussion::selection::offset_at;
    use crate::tui::panes::discussion::selection::remove_range;
    use crate::tui::panes::discussion::selection::row_pieces;
    use crate::tui::panes::discussion::status::fmt_elapsed;
    use crate::tui::panes::discussion::status::fmt_tokens;
    use crate::tui::panes::discussion::status::marker_bg;
    use crate::tui::panes::discussion::status::resume_hint;
    use crate::tui::panes::discussion::status::status_line;
    use crate::tui::panes::discussion::status::status_text;
    use camino::Utf8PathBuf;
    use std::sync::Arc;

    fn node_with_metadata(
        metadata: &str,
    ) -> (
        tempfile::TempDir,
        crate::conversation::ConversationTree,
        crate::conversation::NodeId,
    ) {
        use crate::conversation::ConversationTree;
        use crate::conversation::NodeId;
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
        use crate::tui::theme::HelixTheme;
        let theme = HelixTheme::default();
        let pane_bg = theme.ui_bg("ui.background").unwrap();
        let marker = marker_bg(&theme, pane_bg);
        assert_ne!(marker, pane_bg);
        assert_ne!(Some(marker), theme.ui_bg("ui.selection"));
        assert_eq!(marker, RgbValue::from_u8(0x76, 0x6c, 0x62));
    }

    fn span(text: &str, src: Option<usize>) -> crate::tui::widgets::markdown::Span {
        use crate::tui::widgets::markdown::Span;
        Span {
            src,
            ..crate::tui::widgets::markdown::Span::plain(text, RgbValue::from_u8(0, 0, 0))
        }
    }

    fn doc_line(
        part: Option<crate::tui::pane_manager::DocPart>,
        spans: Vec<crate::tui::widgets::markdown::Span>,
    ) -> DocLine {
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
        use crate::tui::pane_manager::DocPart;
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
        let word = crate::util::text::word_bounds(text, 5);
        assert_eq!(word, (4, 7));
        assert_eq!(crate::util::text::drag_range(text, 5, word, 5), (4, 7));
        assert_eq!(crate::util::text::drag_range(text, 5, word, 15), (4, 18));
        assert_eq!(&text[4..18], "two\nthree four");
        assert_eq!(crate::util::text::drag_range(text, 5, word, 1), (0, 7));
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
    fn token_counts_are_abbreviated() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1000), "1k");
        assert_eq!(fmt_tokens(1200), "1k");
        assert_eq!(fmt_tokens(28937), "28k");
        assert_eq!(fmt_tokens(1_000_000), "1.0M");
        assert_eq!(fmt_tokens(1_234_567), "1.2M");
    }

    #[test]
    fn status_text_names_the_compact_type() {
        let (_tmp, tree, id) = node_with_metadata("type: compact\nstatus: done\ntokens-in: 1200\n");
        let (text, failed) = status_text(&tree.node(&id).unwrap().meta);
        assert_eq!(text, "done · compact · 1k");
        assert!(!failed);
    }

    #[test]
    fn status_text_does_not_repeat_a_commit_mode() {
        let (_tmp, tree, id) = node_with_metadata(
            "type: commit\nstatus: done\ncommits: 0123456789abcdef 89abcdef01234567\ntokens-in: 1200\ncached-in: 1152\n",
        );
        let (text, failed) = status_text(&tree.node(&id).unwrap().meta);
        assert_eq!(text, "done · 0123456 89abcde · 1k · 1k cached");
        assert!(!failed);
    }

    #[test]
    fn status_text_shows_the_cache_hits_of_the_context() {
        let (_tmp, tree, id) = node_with_metadata(
            "type: feedback\nstatus: done\ntokens-in: 28937\ncached-in: 28416\n",
        );
        let (text, failed) = status_text(&tree.node(&id).unwrap().meta);
        assert_eq!(text, "done · 28k · 28k cached");
        assert!(!failed);
    }

    #[test]
    fn status_text_does_not_repeat_the_feedback_mode() {
        let (_tmp, tree, id) =
            node_with_metadata("type: feedback\nstatus: done\ntokens-in: 28937\n");
        let (text, failed) = status_text(&tree.node(&id).unwrap().meta);
        assert_eq!(text, "done · 28k");
        assert!(!failed);
    }

    #[test]
    fn status_text_reports_the_error_of_a_failed_node() {
        let (_tmp, tree, id) =
            node_with_metadata("type: feedback\nstatus: failed\nerror: rate limited\n");
        let (text, failed) = status_text(&tree.node(&id).unwrap().meta);
        assert_eq!(text, "failed: rate limited");
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
    fn node_mode_follows_the_node_type() {
        use crate::agent::Mode;
        let (_tmp, tree, id) = node_with_metadata("type: commit\n");
        assert_eq!(node_mode(tree.node(&id).unwrap()), Mode::Commit);
        let (_tmp, tree, id) = node_with_metadata("type: compact\n");
        assert_eq!(node_mode(tree.node(&id).unwrap()), Mode::Feedback);
    }

    fn empty_state() -> crate::tui::state::AppState {
        use crate::tui::keymap::Keymap;
        use crate::tui::state::AppState;
        use crate::tui::theme::HelixTheme;
        let files = Arc::new(arc_swap::ArcSwap::from_pointee(Vec::new()));
        AppState::new(
            files,
            Utf8PathBuf::from("/tmp"),
            HelixTheme::default(),
            Keymap::default(),
        )
    }

    /// A state over a done root `a1` and its failed child `a2`, which was sent with the
    /// `review` command; window 0 shows `a1` and window 1 shows `a2`.
    fn state_with_failed_child() -> (tempfile::TempDir, crate::tui::state::AppState) {
        use crate::agent::AgentConfig;
        use crate::agent::commands::Command;
        use crate::conversation::ConversationTree;
        use crate::conversation::NodeId;
        use crate::model_spec::ModelSpec;
        use crate::tui::state::DiscussionState;
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let parent = root.join("a1");
        let child = parent.join("a2");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(parent.join("metadata.md"), "status: done\n").unwrap();
        std::fs::write(child.join("metadata.md"), "status: failed\n").unwrap();
        std::fs::write(child.join("prompt.md"), "Review it.\n\nfoo").unwrap();
        std::fs::write(child.join("input.md"), "/review foo").unwrap();

        let mut state = empty_state();
        *state.conversations.write() = ConversationTree::load(&root);
        state.agent = Arc::new(AgentConfig {
            commands: vec![
                Command::parse("review", "Review it.").unwrap(),
                Command::parse("squash", "---\nsystem-prompt: off\n---\nSquash since:").unwrap(),
            ],
            ..Default::default()
        });
        for (id, node) in [(0, "a1"), (1, "a2")] {
            state.discussions.insert(
                id,
                DiscussionState {
                    node: Some(NodeId::new(node)),
                    model: ModelSpec::parse("mock/m/low"),
                    loaded: true,
                    ..Default::default()
                },
            );
        }
        (tmp, state)
    }

    fn feedback_launch() -> Launch {
        use crate::agent::Mode;
        use crate::model_spec::ModelSpec;
        Launch {
            parent: None,
            prompt: "hello".to_string(),
            input: None,
            kind: TurnKind::Prompt(Mode::Feedback),
            command: None,
            spec: ModelSpec::parse("mock/m/low").unwrap(),
        }
    }

    /// A configured endpoint, so that a refusal is the trust gate and not a missing token.
    fn agent_with_endpoint() -> Arc<crate::agent::AgentConfig> {
        use crate::agent::AgentConfig;
        use crate::agent::Endpoint;
        use crate::agent::EndpointKind;
        Arc::new(AgentConfig {
            endpoints: vec![Endpoint {
                name: "mock".to_string(),
                kind: EndpointKind::OpenAi {
                    url: "http://localhost:1".to_string(),
                    token: "t".to_string(),
                },
            }],
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn no_turn_runs_in_a_repository_the_user_has_not_trusted() {
        let (_tmp, mut state) = state_with_failed_child();
        state.agent = agent_with_endpoint();
        state.trusted = false;
        let (main_tx, _rx) = tokio::sync::mpsc::channel(1);
        let refused = start_turn(&mut state, main_tx.clone(), feedback_launch());
        assert_eq!(refused, Err(Refusal::Untrusted));
        assert!(
            Refusal::Untrusted
                .into_reason()
                .contains("trust on the command line")
        );

        state.trusted = true;
        let started = start_turn(&mut state, main_tx, feedback_launch());
        assert!(started.is_ok(), "{started:?}");
    }

    fn queue(state: &mut crate::tui::state::AppState, windows: &[usize]) {
        use crate::tui::state::PendingTurn;
        state.queued_turns = windows
            .iter()
            .map(|window| PendingTurn {
                window: *window,
                parent: None,
            })
            .collect();
    }

    fn queued_windows(state: &crate::tui::state::AppState) -> Vec<usize> {
        state
            .queued_turns
            .iter()
            .map(|entry| entry.window)
            .collect()
    }

    #[tokio::test]
    async fn a_message_sent_while_the_turn_runs_reaches_the_turn_instead_of_the_queue() {
        let (_tmp, mut state) = state_with_failed_child();
        state.agent = agent_with_endpoint();
        state.trusted = true;
        let (main_tx, _rx) = tokio::sync::mpsc::channel(1);
        let node = start_turn(&mut state, main_tx.clone(), feedback_launch()).unwrap();
        state.discussions.get_mut(&0).unwrap().node = Some(node.clone());
        state
            .discussions
            .get_mut(&0)
            .unwrap()
            .draft
            .set_text("also handle the parser");

        DiscussionComponent::send(&mut state, main_tx, 0);

        let handle = state.running_turns.get(&node).unwrap();
        assert_eq!(
            handle.live.lock().take_injected(),
            ["also handle the parser"]
        );
        assert!(state.discussions[&0].draft.text().is_empty());
        assert!(state.queued_turns.is_empty());
    }

    #[tokio::test]
    async fn a_message_for_a_closed_turn_is_queued_instead() {
        let (_tmp, mut state) = state_with_failed_child();
        state.agent = agent_with_endpoint();
        state.trusted = true;
        let (main_tx, _rx) = tokio::sync::mpsc::channel(1);
        let node = start_turn(&mut state, main_tx.clone(), feedback_launch()).unwrap();
        state.discussions.get_mut(&0).unwrap().node = Some(node.clone());
        state.running_turns.get(&node).unwrap().live.lock().closed = true;
        state
            .discussions
            .get_mut(&0)
            .unwrap()
            .draft
            .set_text("too late");

        assert!(!DiscussionComponent::inject(&mut state, 0));
        let handle = state.running_turns.get(&node).unwrap();
        assert!(handle.live.lock().injected.is_empty());
        assert_eq!(state.discussions[&0].draft.text(), "too late");

        // Sent, it queues on the running turn rather than reaching it.
        DiscussionComponent::send(&mut state, main_tx, 0);
        assert_eq!(queued_windows(&state), [0]);
        assert!(
            state
                .running_turns
                .get(&node)
                .unwrap()
                .live
                .lock()
                .injected
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_message_the_turn_never_read_goes_back_to_the_input() {
        let (_tmp, mut state) = state_with_failed_child();
        state.agent = agent_with_endpoint();
        state.trusted = true;
        let (main_tx, _rx) = tokio::sync::mpsc::channel(1);
        let node = start_turn(&mut state, main_tx.clone(), feedback_launch()).unwrap();
        let handle = state.running_turns.get(&node).unwrap().clone();
        handle
            .live
            .lock()
            .injected
            .push_back("never read".to_string());

        // The window has something newer typed in it; the message lands under it.
        state.discussions.get_mut(&0).unwrap().node = Some(node.clone());
        state
            .discussions
            .get_mut(&0)
            .unwrap()
            .draft
            .set_text("typed later");
        DiscussionComponent::restore_injected(&mut state, &node, &handle);
        assert_eq!(
            state.discussions[&0].draft.text(),
            "typed later\nnever read"
        );

        // An empty input takes the message as is.
        state.discussions.get_mut(&0).unwrap().draft.clear();
        handle.live.lock().injected.push_back("again".to_string());
        DiscussionComponent::restore_injected(&mut state, &node, &handle);
        assert_eq!(state.discussions[&0].draft.text(), "again");
    }

    #[tokio::test]
    async fn a_resume_is_allowed_on_an_interrupted_node_that_already_has_children() {
        use crate::conversation::ConversationTree;
        use crate::conversation::NodeId;
        let (_tmp, mut state) = state_with_failed_child();
        state.agent = agent_with_endpoint();
        state.trusted = true;
        // a2 already has a child, and its own turn failed.
        let root = state
            .conversations
            .read()
            .node_dir(&NodeId::new("a1"))
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let dir = root.join("a1").join("a2");
        std::fs::create_dir_all(dir.join("a3")).unwrap();
        std::fs::write(dir.join("a3").join("metadata.md"), "status: done\n").unwrap();
        *state.conversations.write() = ConversationTree::load(&root);
        assert!(
            !state
                .conversations
                .read()
                .node(&NodeId::new("a2"))
                .unwrap()
                .children
                .is_empty()
        );

        let (main_tx, _rx) = tokio::sync::mpsc::channel(1);
        state
            .discussions
            .get_mut(&1)
            .unwrap()
            .draft
            .set_text("go on");
        let launch = compose_launch(&state, 1).unwrap().unwrap();
        assert!(matches!(launch.kind, TurnKind::Continue(_)));
        assert!(
            start_turn(&mut state, main_tx, launch).is_ok(),
            "a resume of an interrupted node with children must be allowed"
        );
    }

    #[tokio::test]
    async fn the_model_of_a_window_whose_turn_runs_cannot_be_changed() {
        use crate::tui::pane_manager::Window;
        use crate::tui::panes::pickers::model_picker::ModelPickerComponent;
        use crate::tui::state::ModelPickerPurpose;
        let (_tmp, mut state) = state_with_failed_child();
        state.agent = agent_with_endpoint();
        state.trusted = true;
        let (main_tx, _rx) = tokio::sync::mpsc::channel(1);
        let node = start_turn(&mut state, main_tx.clone(), feedback_launch()).unwrap();
        state.discussions.get_mut(&0).unwrap().node = Some(node);

        ModelPickerComponent::open(&mut state, main_tx, ModelPickerPurpose::Window(0));

        assert!(
            !state
                .pane_manager
                .window_stack
                .contains(&Window::ModelPicker)
        );
        assert_eq!(
            state.discussions[&0].notice.as_deref(),
            Some("this turn is running: its model is fixed until it ends")
        );
    }

    #[test]
    fn ordinal_names_a_place_in_the_queue() {
        let places: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 101, 111]
            .into_iter()
            .map(ordinal)
            .collect();
        assert_eq!(
            places,
            [
                "1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "101st",
                "111th"
            ]
        );
    }

    #[test]
    fn a_queued_prompt_moves_one_place_at_a_time_and_stops_at_the_ends() {
        let mut state = empty_state();
        queue(&mut state, &[7, 8, 9]);

        move_queued(&mut state, 9, true);
        assert_eq!(queued_windows(&state), [7, 9, 8]);
        move_queued(&mut state, 9, true);
        move_queued(&mut state, 9, true);
        assert_eq!(queued_windows(&state), [9, 7, 8]);
        move_queued(&mut state, 7, false);
        move_queued(&mut state, 7, false);
        assert_eq!(queued_windows(&state), [9, 8, 7]);
        move_queued(&mut state, 4, true);
        assert_eq!(queued_windows(&state), [9, 8, 7]);

        drop_queue_entry(&mut state, 8);
        assert_eq!(queued_windows(&state), [9, 7]);
        assert!(
            queue_hint(&state, 7)
                .unwrap()
                .starts_with("queued · 2nd · ")
        );
        assert!(queue_hint(&state, 8).is_none());
    }

    #[test]
    fn elapsed_time_drops_the_leading_zero_units_only() {
        assert_eq!(fmt_elapsed(0), "0s");
        assert_eq!(fmt_elapsed(59), "59s");
        assert_eq!(fmt_elapsed(60), "1m 0s");
        assert_eq!(fmt_elapsed(125), "2m 5s");
        assert_eq!(fmt_elapsed(3605), "1h 0m 5s");
        assert_eq!(fmt_elapsed(3725), "1h 2m 5s");
    }

    #[test]
    fn the_work_log_text_follows_the_entries_shown() {
        use crate::tui::pane_manager::DocPart;
        use crate::tui::state::DiscussionState;
        let mut discussion = DiscussionState {
            work_log: vec!["old".into(), "new".into()],
            log_start: 1,
            ..crate::tui::state::DiscussionState::default()
        };
        assert_eq!(part_text(&discussion, DocPart::WorkLog), "new");
        discussion.show_work_log = Some(true);
        assert_eq!(part_text(&discussion, DocPart::WorkLog), "old\nnew");
    }

    #[test]
    fn status_line_shows_the_place_of_a_queued_prompt() {
        let (_tmp, mut state) = state_with_failed_child();
        let (idle, _) = status_line(&state, 0, &state.discussions[&0]);
        assert!(!idle.contains("queued"));

        queue(&mut state, &[1, 0]);
        let (status, alert) = status_line(&state, 0, &state.discussions[&0]);
        assert!(status.starts_with("queued · 2nd · "));
        assert!(status.ends_with("Esc:Take back"));
        assert!(!alert);
    }

    #[test]
    fn a_draft_is_composed_below_a_done_node_and_beside_a_failed_one() {
        use crate::agent::Mode;
        use crate::conversation::NodeId;
        let (_tmp, mut state) = state_with_failed_child();
        assert!(compose_launch(&state, 0).is_none());

        for discussion in state.discussions.values_mut() {
            discussion.draft.set_text("/review bar");
        }
        let below = compose_launch(&state, 0).unwrap().unwrap();
        assert_eq!(below.parent, Some(NodeId::new("a1")));
        assert_eq!(below.prompt, "Review it.\n\nbar");
        assert_eq!(below.input.as_deref(), Some("/review bar"));
        assert_eq!(below.command.as_deref(), Some("review"));
        assert!(matches!(below.kind, TurnKind::Prompt(Mode::Feedback)));

        let beside = compose_launch(&state, 1).unwrap().unwrap();
        assert_eq!(beside.parent, Some(NodeId::new("a1")));

        state
            .discussions
            .get_mut(&0)
            .unwrap()
            .draft
            .set_text("/squash main");
        let squash = compose_launch(&state, 0).unwrap().unwrap();
        assert_eq!(squash.prompt, "Squash since:\n\nmain");
        assert_eq!(squash.input.as_deref(), Some("/squash main"));
        assert_eq!(squash.command.as_deref(), Some("squash"));
        assert!(matches!(squash.kind, TurnKind::NoSystemPrompt));

        state
            .discussions
            .get_mut(&0)
            .unwrap()
            .draft
            .set_text("as typed");
        let typed = compose_launch(&state, 0).unwrap().unwrap();
        assert_eq!(typed.prompt, "as typed");
        assert!(typed.input.is_none());
        assert!(typed.command.is_none());

        state.discussions.get_mut(&0).unwrap().model = None;
        assert!(compose_launch(&state, 0).is_none());
    }

    #[test]
    fn a_whitespace_padded_draft_is_trimmed_and_the_draft_keeps_its_text() {
        let (_tmp, mut state) = state_with_failed_child();
        state
            .discussions
            .get_mut(&0)
            .unwrap()
            .draft
            .set_text("\n  review the parser  \n");
        let launch = compose_launch(&state, 0).unwrap().unwrap();
        assert_eq!(launch.prompt, "review the parser");
        assert!(launch.input.is_none());
        assert_eq!(
            state.discussions[&0].draft.text(),
            "\n  review the parser  \n"
        );
    }

    #[test]
    fn continue_composes_a_child_of_the_node_shown() {
        use crate::agent::Mode;
        use crate::conversation::NodeId;
        use crate::conversation::NodeType;
        let (_tmp, mut state) = state_with_failed_child();
        state
            .discussions
            .get_mut(&1)
            .unwrap()
            .draft
            .set_text("/continue");
        let launch = compose_launch(&state, 1).unwrap().unwrap();
        assert_eq!(launch.parent, Some(NodeId::new("a2")));
        assert!(matches!(launch.kind, TurnKind::Continue(Mode::Feedback)));
        assert!(launch.command.is_none());

        state
            .conversations
            .write()
            .update_meta(&NodeId::new("a2"), |meta| {
                meta.node_type = Some(NodeType::Commit);
            })
            .unwrap();
        let launch = compose_launch(&state, 1).unwrap().unwrap();
        assert!(matches!(launch.kind, TurnKind::Continue(Mode::Commit)));
        // The note carries the recorded error and the prompt as typed.
        assert!(launch.prompt.contains("was interrupted"));
        assert!(launch.prompt.contains("[Note from Explorer:"));
        assert!(launch.input.as_deref() == Some("/continue"));

        // Bare, in a new conversation: nothing to resume.
        state.discussions.get_mut(&0).unwrap().node = None;
        state
            .discussions
            .get_mut(&0)
            .unwrap()
            .draft
            .set_text("/continue");
        assert!(matches!(
            compose_launch(&state, 0),
            Some(Err(Refusal::Rejected(reason))) if reason.contains("nothing to resume")
        ));
    }

    #[test]
    fn continue_note_carries_the_error_and_says_the_work_was_discarded() {
        use crate::conversation::NodeId;
        let (_tmp, mut state) = state_with_failed_child();
        state
            .conversations
            .write()
            .update_meta(&NodeId::new("a2"), |meta| {
                meta.error = Some("rate limited".into());
                meta.scratch_reset = true;
            })
            .unwrap();
        state
            .discussions
            .get_mut(&1)
            .unwrap()
            .draft
            .set_text("/continue finish it");
        let launch = compose_launch(&state, 1).unwrap().unwrap();
        assert!(launch.prompt.contains("was interrupted: rate limited"));
        assert!(launch.prompt.contains("discarded"));
        assert!(!launch.prompt.contains("stash"));
        assert!(
            launch
                .prompt
                .ends_with("Additional instructions from the user: finish it")
        );
    }

    #[test]
    fn continue_note_does_not_claim_a_discard_when_the_turn_left_nothing() {
        let (_tmp, mut state) = state_with_failed_child();
        state
            .discussions
            .get_mut(&1)
            .unwrap()
            .draft
            .set_text("/continue");
        let launch = compose_launch(&state, 1).unwrap().unwrap();
        assert!(!launch.prompt.contains("discarded"));
        assert!(launch.prompt.contains("nothing was lost"));
    }

    #[test]
    fn commit_command_composes_a_go_ahead_in_commit_mode() {
        use crate::agent::Mode;
        use crate::conversation::NodeId;
        let (_tmp, mut state) = state_with_failed_child();
        state
            .discussions
            .get_mut(&0)
            .unwrap()
            .draft
            .set_text("/commit");
        let bare = compose_launch(&state, 0).unwrap().unwrap();
        assert_eq!(bare.parent, Some(NodeId::new("a1")));
        assert_eq!(bare.prompt, "OK, go ahead.");
        assert_eq!(bare.input.as_deref(), Some("/commit"));
        assert!(bare.command.is_none());
        assert!(matches!(bare.kind, TurnKind::Prompt(Mode::Commit)));

        state
            .discussions
            .get_mut(&0)
            .unwrap()
            .draft
            .set_text("/commit keep the name");
        let detailed = compose_launch(&state, 0).unwrap().unwrap();
        assert_eq!(
            detailed.prompt,
            "OK, go ahead.\n\nAdditional instructions:\n\nkeep the name"
        );
        assert_eq!(detailed.input.as_deref(), Some("/commit keep the name"));

        let root = state.discussions.get_mut(&0).unwrap();
        root.node = None;
        assert!(matches!(
            compose_launch(&state, 0),
            Some(Ok(Launch { parent: None, .. }))
        ));
        state
            .discussions
            .get_mut(&0)
            .unwrap()
            .draft
            .set_text("/commit ");
        assert!(matches!(
            compose_launch(&state, 0),
            Some(Err(Refusal::Rejected(_)))
        ));
    }

    #[test]
    fn the_empty_input_of_an_interrupted_node_says_a_message_resumes_it() {
        let (_tmp, state) = state_with_failed_child();
        // Window 1 shows the failed a2; window 0 shows the done a1.
        assert_eq!(
            resume_hint(&state, &state.discussions[&1]).as_deref(),
            Some("sending a message resumes the interrupted turn")
        );
        assert!(resume_hint(&state, &state.discussions[&0]).is_none());
    }

    #[test]
    fn a_plain_message_on_a_failed_node_resumes_it() {
        use crate::agent::Mode;
        use crate::conversation::NodeId;
        let (_tmp, mut state) = state_with_failed_child();
        state
            .discussions
            .get_mut(&1)
            .unwrap()
            .draft
            .set_text("keep going, use the other parser");
        let launch = compose_launch(&state, 1).unwrap().unwrap();
        assert_eq!(launch.parent, Some(NodeId::new("a2")));
        assert!(matches!(launch.kind, TurnKind::Continue(Mode::Feedback)));
        assert!(launch.prompt.contains("was interrupted"));
        assert_eq!(
            launch.input.as_deref(),
            Some("keep going, use the other parser")
        );
        assert!(launch.command.is_none());

        // A slash command keeps its meaning: it runs as a sibling of the failed node.
        state
            .discussions
            .get_mut(&1)
            .unwrap()
            .draft
            .set_text("/review bar");
        let launch = compose_launch(&state, 1).unwrap().unwrap();
        assert_eq!(launch.parent, Some(NodeId::new("a1")));
        assert!(matches!(launch.kind, TurnKind::Prompt(Mode::Feedback)));

        // A done node takes a child as before.
        state
            .discussions
            .get_mut(&0)
            .unwrap()
            .draft
            .set_text("and now this");
        let launch = compose_launch(&state, 0).unwrap().unwrap();
        assert_eq!(launch.parent, Some(NodeId::new("a1")));
        assert!(matches!(launch.kind, TurnKind::Prompt(Mode::Feedback)));
    }

    #[test]
    fn a_message_on_a_cancelled_node_resumes_it() {
        use crate::agent::Mode;
        use crate::conversation::NodeId;
        use crate::conversation::NodeStatus;
        let (_tmp, mut state) = state_with_failed_child();
        state
            .conversations
            .write()
            .update_meta(&NodeId::new("a2"), |meta| {
                meta.status = Some(NodeStatus::Cancelled);
            })
            .unwrap();
        state
            .discussions
            .get_mut(&1)
            .unwrap()
            .draft
            .set_text("go on");
        let launch = compose_launch(&state, 1).unwrap().unwrap();
        assert_eq!(launch.parent, Some(NodeId::new("a2")));
        assert!(matches!(launch.kind, TurnKind::Continue(Mode::Feedback)));
        assert!(launch.prompt.contains(crate::conversation::CANCELLED_ERROR));
    }

    #[test]
    fn a_failed_commit_node_resumes_in_commit_mode() {
        use crate::agent::Mode;
        use crate::conversation::NodeId;
        use crate::conversation::NodeType;
        let (_tmp, mut state) = state_with_failed_child();
        state
            .conversations
            .write()
            .update_meta(&NodeId::new("a2"), |meta| {
                meta.node_type = Some(NodeType::Commit);
            })
            .unwrap();
        state
            .discussions
            .get_mut(&1)
            .unwrap()
            .draft
            .set_text("go on");
        let launch = compose_launch(&state, 1).unwrap().unwrap();
        assert!(matches!(launch.kind, TurnKind::Continue(Mode::Commit)));
    }

    #[test]
    fn a_message_on_a_failed_compact_node_does_not_resume_it() {
        use crate::agent::Mode;
        use crate::conversation::NodeId;
        use crate::conversation::NodeType;
        let (_tmp, mut state) = state_with_failed_child();
        state
            .conversations
            .write()
            .update_meta(&NodeId::new("a2"), |meta| {
                meta.node_type = Some(NodeType::Compact);
            })
            .unwrap();
        state
            .discussions
            .get_mut(&1)
            .unwrap()
            .draft
            .set_text("try again");
        let launch = compose_launch(&state, 1).unwrap().unwrap();
        assert_eq!(launch.parent, Some(NodeId::new("a1")));
        assert!(matches!(launch.kind, TurnKind::Prompt(Mode::Feedback)));

        // And `/continue` there is refused rather than building on nothing.
        state
            .discussions
            .get_mut(&1)
            .unwrap()
            .draft
            .set_text("/continue");
        assert!(matches!(
            compose_launch(&state, 1),
            Some(Err(Refusal::Rejected(reason))) if reason.contains("no work to resume")
        ));
    }

    #[test]
    fn take_back_takes_the_head_most_prompt_below_the_node() {
        use crate::conversation::NodeId;
        use crate::tui::state::DiscussionState;
        use crate::tui::state::PendingTurn;
        let (_tmp, mut state) = state_with_failed_child();
        let (main_tx, _main_rx) = tokio::sync::mpsc::channel(1);
        state.queued_turns = vec![
            PendingTurn {
                window: 1,
                parent: Some(NodeId::new("a1")),
            },
            PendingTurn {
                window: 2,
                parent: Some(NodeId::new("a1")),
            },
        ];
        state.discussions.insert(
            2,
            DiscussionState {
                node: Some(NodeId::new("a1")),
                ..Default::default()
            },
        );
        state
            .discussions
            .get_mut(&1)
            .unwrap()
            .draft
            .set_text("queued text");

        take_back(
            &mut state,
            tokio::sync::mpsc::channel(1).0,
            &NodeId::new("a3"),
        );
        assert_eq!(queued_windows(&state), [1, 2]);

        // The head-most entry leaves the queue and its text stays its window's draft;
        // release then drops the other window's entry, whose draft is empty.
        take_back(&mut state, main_tx, &NodeId::new("a1"));
        assert!(state.queued_turns.is_empty());
        assert_eq!(state.discussions[&1].draft.text(), "queued text");
    }

    #[test]
    fn a_queued_prompt_that_cannot_be_sent_leaves_the_queue() {
        use crate::conversation::NodeId;
        use crate::tui::state::PendingTurn;
        let (_tmp, mut state) = state_with_failed_child();
        let (main_tx, _main_rx) = tokio::sync::mpsc::channel(1);
        state.discussions.get_mut(&0).unwrap().draft.set_text("go");
        state.discussions.get_mut(&1).unwrap().draft.set_text("   ");
        state.queued_turns = vec![
            PendingTurn {
                window: 5,
                parent: None,
            },
            PendingTurn {
                window: 1,
                parent: Some(NodeId::new("a2")),
            },
            PendingTurn {
                window: 0,
                parent: None,
            },
        ];

        DiscussionComponent::release_queue(&mut state, main_tx);

        assert!(state.queued_turns.is_empty());
        assert!(state.running_turns.is_empty());
        assert!(state.discussions[&1].notice.is_none());
        assert_eq!(
            state.discussions[&0].notice.as_deref(),
            Some("this window moved on since the prompt was queued")
        );
        assert_eq!(state.discussions[&0].draft.text(), "go");
    }

    #[test]
    fn lock_excludes_every_turn_while_one_is_running() {
        use crate::agent::Mode;
        use Mode::{Commit, Feedback};
        assert!(lock_refusal(&[]).is_none());
        assert!(lock_refusal(&[Feedback]).is_some());
        assert!(lock_refusal(&[Commit]).is_some());
        assert!(lock_refusal(&[Feedback, Commit]).is_some());
    }

    #[test]
    fn replay_keeps_parent_mode_and_command_of_the_node() {
        use crate::agent::Mode;
        use crate::conversation::ConversationTree;
        use crate::conversation::NodeId;
        use crate::model_spec::ModelSpec;
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let parent = root.join("a1");
        let commit = parent.join("a2");
        let compact = parent.join("a3");
        let squash = parent.join("a4");
        for (dir, metadata) in [
            (&parent, "type: feedback\n"),
            (&commit, "type: commit\ncommand: review\n"),
            (&compact, "type: compact\n"),
            (
                &squash,
                "type: commit\ncommand: squash\nsystem-prompt: off\n",
            ),
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

        let typed = "/compact keep names".to_string();
        let launch = Launch::replay(&tree, &NodeId::new("a3"), typed, spec.clone()).unwrap();
        assert!(
            matches!(launch.kind, TurnKind::Compact { ref instructions } if instructions == "keep names")
        );

        let launch =
            Launch::replay(&tree, &NodeId::new("a4"), "x".to_string(), spec.clone()).unwrap();
        assert!(matches!(launch.kind, TurnKind::NoSystemPrompt));
        assert_eq!(launch.command.as_deref(), Some("squash"));

        let root_launch =
            Launch::replay(&tree, &NodeId::new("a1"), "hi".to_string(), spec).unwrap();
        assert_eq!(root_launch.parent, None);
        assert!(matches!(root_launch.kind, TurnKind::Prompt(Mode::Feedback)));
    }

    #[tokio::test]
    async fn opening_a_node_starts_nothing_and_keeps_what_the_attempt_left() {
        use crate::conversation::ConversationTree;
        use crate::conversation::NodeId;
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let dir = root.join("a1").join("a2");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(root.join("a1").join("metadata.md"), "status: done\n").unwrap();
        // No endpoint, model or effort: opening must not need them.
        let metadata = "status: failed\nerror: boom\n";
        std::fs::write(dir.join("metadata.md"), metadata).unwrap();
        std::fs::write(dir.join("prompt.md"), "do it").unwrap();
        std::fs::write(dir.join("reply.md"), "partial").unwrap();
        std::fs::write(dir.join("activity.md"), "- shell ls\n").unwrap();
        std::fs::write(dir.join("transcript.jsonl"), "{}\n").unwrap();
        std::fs::create_dir_all(dir.join("agents").join("1")).unwrap();
        std::fs::create_dir_all(dir.join("processes")).unwrap();

        let mut state = empty_state();
        *state.conversations.write() = ConversationTree::load(&root);
        let (main_tx, _main_rx) = tokio::sync::mpsc::channel(1);
        DiscussionComponent::open(&mut state, main_tx, NodeId::new("a2"), None);

        assert_eq!(state.discussions.len(), 1);
        let discussion = state.discussions.values().next().unwrap();
        assert_eq!(discussion.node, Some(NodeId::new("a2")));
        assert!(discussion.draft.text().is_empty());
        assert!(discussion.notice.is_none());
        assert!(state.running_turns.is_empty());
        for name in ["reply.md", "activity.md", "transcript.jsonl"] {
            assert!(dir.join(name).exists(), "{name} was deleted");
        }
        for name in ["agents", "processes"] {
            assert!(dir.join(name).exists(), "{name}/ was deleted");
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("metadata.md")).unwrap(),
            metadata
        );
    }

    fn key(key: Key) -> InputEvent {
        InputEvent::Keyboard(KeyPress::Plain { key })
    }

    /// Sends `events` to `window` through `component`, as the pane slot showing it would.
    fn type_into(
        component: &mut DiscussionComponent,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        window: usize,
        events: &[InputEvent],
    ) {
        component.set_window_id(window);
        for event in events {
            component
                .handle_event(global_data, event.clone(), &mut HasFocus::default())
                .unwrap();
        }
    }

    fn global_data_with_drafts(
        drafts: &[&str],
    ) -> GlobalData<crate::tui::state::AppState, crate::signal::AppSignal> {
        use crate::tui::state::DiscussionState;
        use crate::tui::widgets::input_line::InputLine;
        let mut state = empty_state();
        for (id, draft) in drafts.iter().enumerate() {
            state.discussions.insert(
                id,
                DiscussionState {
                    draft: InputLine::with_text(*draft),
                    ..Default::default()
                },
            );
        }
        let (main_tx, _) = tokio::sync::mpsc::channel(8);
        let size = vp_width(80) + vp_height(24);
        GlobalData::try_to_create_instance(
            main_tx,
            state,
            size,
            OutputDevice::new_mock().0,
            OfsBufPool::new(size),
        )
        .unwrap()
    }

    #[test]
    fn the_input_cursor_follows_its_window_to_another_pane_slot() {
        let mut global_data = global_data_with_drafts(&["hello world"]);
        let mut first_slot = DiscussionComponent::new(FlexBoxId::from(1));
        let mut second_slot = DiscussionComponent::new(FlexBoxId::from(2));
        let left = key(Key::SpecialKey(SpecialKey::Left));

        let moves = [left.clone(), left.clone(), left.clone(), left.clone(), left];
        type_into(&mut first_slot, &mut global_data, 0, &moves);
        type_into(
            &mut second_slot,
            &mut global_data,
            0,
            &[key(Key::Character('X'))],
        );
        assert_eq!(
            global_data.state.discussions[&0].draft.text(),
            "hello Xworld"
        );
        type_into(
            &mut first_slot,
            &mut global_data,
            0,
            &[key(Key::Character('Y'))],
        );
        assert_eq!(
            global_data.state.discussions[&0].draft.text(),
            "hello XYworld"
        );
    }

    #[test]
    fn two_windows_of_one_pane_slot_keep_their_own_input_cursors() {
        let mut global_data = global_data_with_drafts(&["hello world", "other"]);
        let mut slot = DiscussionComponent::new(FlexBoxId::from(1));
        let left = key(Key::SpecialKey(SpecialKey::Left));
        let home = key(Key::SpecialKey(SpecialKey::Home));

        let moves = [left.clone(), left.clone(), left.clone(), left.clone(), left];
        type_into(&mut slot, &mut global_data, 0, &moves);
        type_into(
            &mut slot,
            &mut global_data,
            1,
            &[home, key(Key::Character('A'))],
        );
        type_into(&mut slot, &mut global_data, 0, &[key(Key::Character('X'))]);
        type_into(&mut slot, &mut global_data, 1, &[key(Key::Character('B'))]);
        assert_eq!(
            global_data.state.discussions[&0].draft.text(),
            "hello Xworld"
        );
        assert_eq!(global_data.state.discussions[&1].draft.text(), "ABother");
    }

    #[test]
    fn a_window_input_holds_the_mouse_while_it_selects() {
        use crate::tui::pane_manager::Window;
        use crate::tui::state::DiscussionState;
        use crate::tui::widgets::input_line::InputLine;
        let mut state = empty_state();
        state.discussions.insert(
            0,
            DiscussionState {
                draft: InputLine::with_text("one two"),
                ..Default::default()
            },
        );
        let color = TuiColor::from(RgbValue::from_u8(0, 0, 0));
        let mut ops = RenderOpIRVec::new();
        state.discussions.get_mut(&0).unwrap().draft.render(
            &mut ops,
            None,
            "",
            vp_col(0) + vp_row(9),
            20,
            1,
            true,
            (color, color, color),
        );
        let mouse = |kind| MouseInput {
            pos: vp_col(1) + vp_row(9),
            kind,
            maybe_modifier_keys: None,
        };
        let window = Window::Discussion(0);
        let down = mouse(MouseInputKind::MouseDown(Button::Left));
        let up = mouse(MouseInputKind::MouseUp(Button::Left));

        assert!(!state.input_mouse(Window::Discussion(1), &down));
        assert!(!state.input_mouse(Window::ConversationTree, &down));
        assert!(state.input_mouse(window, &down));
        assert_eq!(state.input_drag, Some(window));
        assert!(state.mouse_drag_active);
        assert!(state.input_mouse(window, &up));
        assert_eq!(state.input_drag, None);
        assert!(!state.mouse_drag_active);
        assert!(!state.input_mouse(window, &up));

        assert!(state.input_mouse(window, &down));
        state.cancel_input_drag();
        assert!(!state.mouse_drag_active);
        assert!(!state.discussions[&0].draft.selecting());
    }
}
