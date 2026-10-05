use crate::conversation::{Node, NodeId, NodeStatus, NodeType};
use crate::tui::text_layout::wrap;
use crate::tui::*;
use tokio::sync::mpsc;

const SCROLL_STEP: usize = 3;

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
}

impl DiscussionComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self {
            id,
            window_id: 0,
            cache: None,
        }
    }

    pub fn set_window_id(&mut self, window_id: usize) {
        self.window_id = window_id;
    }

    /// Focuses the window already showing `node`, or opens a new one and loads the
    /// node's prompt and reply from disk.
    pub(crate) fn open(
        state: &mut AppState,
        main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
        node: NodeId,
    ) {
        let existing = state
            .discussions
            .iter()
            .find(|(_, discussion)| discussion.node == node)
            .map(|(id, _)| *id);
        let id = existing.unwrap_or_else(|| {
            let id = state.next_discussion_id;
            state.next_discussion_id += 1;
            state.discussions.insert(id, DiscussionState::new(node));
            Self::load_content(state, main_tx, id);
            id
        });
        let window = Window::Discussion(id);
        let old = state.pane_manager.focused_window;
        state.pane_manager.push_window(window);
        state.pane_manager.focused_window = Some(window);
        crate::tui::app::notify_terminal_focus_change(state, old, Some(window));
        state.mark_session_dirty();
    }

    /// Reads the prompt and reply of window `id`'s node off the UI thread.
    pub(crate) fn load_content(
        state: &AppState,
        main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
        id: usize,
    ) {
        let Some(discussion) = state.discussions.get(&id) else {
            return;
        };
        let node = discussion.node.clone();
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

    fn doc_lines(&mut self, discussion: &DiscussionState, width: usize) -> &[DocLine] {
        let fresh = self.cache.as_ref().is_some_and(|cache| {
            cache.window_id == self.window_id
                && cache.width == width
                && cache.prompt_len == discussion.prompt.len()
                && cache.reply_len == discussion.reply.len()
        });
        if !fresh {
            let mut lines = Vec::new();
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
        let title = state
            .discussions
            .get(&window_id)
            .and_then(|discussion| {
                let tree = state.conversations.read();
                tree.node(&discussion.node)
                    .map(|node| node.display_title().to_string())
            })
            .unwrap_or_else(|| "Discussion".to_string());
        render_pane_title(ops, pane_box, &title, false, theme, focused);
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

impl Component<AppState, AppSignal> for DiscussionComponent {
    fn reset(&mut self) {
        self.cache = None;
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
        let state = &mut global_data.state;
        let window = Window::Discussion(self.window_id);
        let scroll = state.pane_manager.window_scroll(&window);
        let page = state.pane_manager.window_page_size(&window).max(1);
        let target = match &input_event {
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
            _ => return Ok(EventPropagation::Propagate),
        };
        state.pane_manager.set_window_scroll(&window, target);
        state.pane_manager.clamp_scroll(&window);
        Ok(EventPropagation::ConsumedRender)
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        current_box: FlexBox,
        _surface_bounds: SurfaceBounds,
        _has_focus: &mut HasFocus,
    ) -> CommonResult {
        throws!({
            let origin = current_box.style_adjusted_origin_pos;
            let bounds = current_box.style_adjusted_bounds_size;
            let total_rows = bounds.row_height.as_usize();
            let pane_width = bounds.col_width.as_usize();
            let window = Window::Discussion(self.window_id);
            let state = &mut global_data.state;
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

            let (status, failed) = match &discussion.error {
                Some(error) => (format!("error: {error}"), true),
                None if !discussion.loaded => ("Loading…".to_string(), false),
                None => state
                    .conversations
                    .read()
                    .node(&discussion.node)
                    .map(status_text)
                    .unwrap_or_default(),
            };

            let doc_rows = total_rows.saturating_sub(1);
            let lines = self.doc_lines(discussion, pane_width);
            let line_count = lines.len();
            let scroll = state
                .pane_manager
                .window_scroll(&window)
                .min(line_count.saturating_sub(doc_rows));

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
            if total_rows > 0 {
                let fg = if failed { error_fg } else { status_fg };
                let text = crate::tui::text_layout::truncate(&status, pane_width);
                paint_row(&mut render_ops, doc_rows, &text, fg, pane_bg);
            }

            state.pane_manager.set_window_page_size(&window, doc_rows);
            state
                .pane_manager
                .set_window_scroll_max(&window, line_count);
            state.pane_manager.set_window_scroll(&window, scroll);
            global_data.pipeline.push(ZOrder::Normal, render_ops);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::ConversationTree;
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
}
