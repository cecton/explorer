use crate::agent::commands::{self, Expansion};
use crate::agent::turn::{self, TurnRequest};
use crate::agent::{Mode, ModelSpec};
use crate::conversation::{ConversationTree, Metadata, Node, NodeId, NodeStatus, NodeType};
use crate::tui::text_layout::{display_width, truncate, wrap};
use crate::tui::*;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

const SCROLL_STEP: usize = 3;

type MainTx = mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DocKind {
    Prompt,
    Gap,
    Reply,
}

struct DocLine {
    kind: DocKind,
    text: String,
}

/// Wrapped prompt and reply of one window, reused until the text or the width changes.
struct DocCache {
    window_id: usize,
    width: usize,
    prompt_len: usize,
    reply_len: usize,
    lines: Vec<DocLine>,
}

pub struct DiscussionComponent {
    id: FlexBoxId,
    window_id: usize,
    cache: Option<DocCache>,
    input: InputLine,
    /// Window the input cursor belongs to; another window's draft resets it.
    input_window: Option<usize>,
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
    if let Some(reason) = lock_refusal(&running, mode) {
        return Err(reason.to_string());
    }
    if mode == Mode::Commit && !crate::git::status_short(&state.root)?.trim().is_empty() {
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
    let is_rerun = rerun.is_some();
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
        parent_session: context.parent_session,
        compact,
        compacted_parent: context.compacted_parent,
    };
    let handle = turn::spawn(request, main_tx.clone());
    state.running_turns.insert(node.clone(), handle);
    if !is_rerun {
        turn::spawn_title_and_summary(
            node.clone(),
            context.node_dir,
            prompt,
            summary_backend,
            main_tx,
        );
    }
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

/// Why a prompt in `mode` cannot start while turns in the `running` modes are in
/// progress, under the repository's read-write lock: a commit turn excludes every other
/// turn, feedback turns only exclude commit turns.
fn lock_refusal(running: &[Mode], mode: Mode) -> Option<&'static str> {
    if running.contains(&Mode::Commit) {
        return Some("a commit turn is running; wait for it to finish");
    }
    (mode == Mode::Commit && !running.is_empty())
        .then_some("other turns are running; a commit turn must run alone")
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
        }
    }

    pub fn set_window_id(&mut self, window_id: usize) {
        self.window_id = window_id;
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
            send_signal!(
                main_tx,
                TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::NodeContentLoaded {
                    window_id: id,
                    node,
                    prompt,
                    reply,
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
        discussion.draft.clear();
        discussion.loaded = true;
        discussion.notice = None;
        discussion.follow = true;
        state
            .pane_manager
            .set_window_scroll(&Window::Discussion(id), 0);
    }

    fn doc_lines(&mut self, discussion: &DiscussionState, width: usize) -> &[DocLine] {
        let fresh = self.cache.as_ref().is_some_and(|cache| {
            cache.window_id == self.window_id
                && cache.width == width
                && cache.prompt_len == discussion.prompt.len()
                && cache.reply_len == discussion.reply.len()
        });
        if !fresh {
            let mut lines = Vec::new();
            if discussion.node.is_some() {
                for text in wrap(&discussion.prompt, width) {
                    lines.push(DocLine {
                        kind: DocKind::Prompt,
                        text,
                    });
                }
                lines.push(DocLine {
                    kind: DocKind::Gap,
                    text: String::new(),
                });
                for text in wrap(&discussion.reply, width) {
                    lines.push(DocLine {
                        kind: DocKind::Reply,
                        text,
                    });
                }
            }
            self.cache = Some(DocCache {
                window_id: self.window_id,
                width,
                prompt_len: discussion.prompt.len(),
                reply_len: discussion.reply.len(),
                lines,
            });
        }
        self.cache.as_ref().map_or(&[], |cache| &cache.lines)
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
        let discussion = state.discussions.get(&window_id);
        let title = discussion
            .and_then(|discussion| discussion.node.as_ref())
            .and_then(|node| {
                let tree = state.conversations.read();
                tree.node(node).map(|node| node.display_title().to_string())
            })
            .unwrap_or_else(|| "New conversation".to_string());
        let model = discussion
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
    if let Some(tokens) = node.meta.tokens_in {
        push(&format!("{tokens} tokens in"));
    }
    (text, failed)
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
    state
        .conversations
        .read()
        .node(node)
        .map(status_text)
        .unwrap_or_default()
}

impl Component<AppState, AppSignal> for DiscussionComponent {
    fn reset(&mut self) {
        self.cache = None;
        self.input_window = None;
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
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::PageUp),
            }) => scroll.saturating_sub(page),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::PageDown),
            }) => scroll + page,
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
                if live.reply.len() != discussion.reply.len() {
                    discussion.reply.clone_from(&live.reply);
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

            let lines = self.doc_lines(discussion, pane_width);
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
                             text: &str,
                             fg: RgbValue,
                             bg: RgbValue| {
                let style =
                    new_style!(color_fg: {TuiColor::from(fg)} color_bg: {TuiColor::from(bg)});
                *ops += RenderOpCommon::MoveCursorPositionRelTo(
                    origin,
                    col(0) + row(row_offset as u16),
                );
                *ops += RenderOpCommon::ApplyColors(Some(style));
                *ops +=
                    RenderOpIR::PaintTextWithAttributes(" ".repeat(pane_width).into(), Some(style));
                *ops += RenderOpCommon::MoveCursorPositionRelTo(
                    origin,
                    col(0) + row(row_offset as u16),
                );
                *ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
            };
            for row_offset in 0..doc_rows {
                match lines.get(scroll + row_offset) {
                    Some(line) if line.kind == DocKind::Prompt => {
                        paint_row(&mut render_ops, row_offset, &line.text, text_fg, prompt_bg);
                    }
                    Some(line) => {
                        paint_row(&mut render_ops, row_offset, &line.text, text_fg, pane_bg);
                    }
                    None => paint_row(&mut render_ops, row_offset, "", text_fg, pane_bg),
                }
            }
            if status_rows > 0 {
                let fg = if alert { error_fg } else { status_fg };
                let text = truncate(&status, pane_width);
                paint_row(&mut render_ops, doc_rows, &text, fg, pane_bg);
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
    fn lock_lets_feedback_turns_share_and_commit_turns_run_alone() {
        use Mode::{Commit, Feedback};
        assert!(lock_refusal(&[], Feedback).is_none());
        assert!(lock_refusal(&[], Commit).is_none());
        assert!(lock_refusal(&[Feedback, Feedback], Feedback).is_none());
        assert!(lock_refusal(&[Feedback], Commit).is_some());
        assert!(lock_refusal(&[Commit], Feedback).is_some());
        assert!(lock_refusal(&[Commit], Commit).is_some());
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
