/// Focuses `window`: brings it to the front, tells a terminal it lost or gained focus,
/// and applies the grab rule.
pub(crate) fn focus_window(
    state: &mut crate::tui::state::AppState,
    window: crate::tui::pane_manager::Window,
) {
    let old = state.pane_manager.focused_window;
    state.pane_manager.push_window(window);
    state.pane_manager.focused_window = Some(window);
    notify_terminal_focus_change(state, old, Some(window));
    sync_terminal_grabbed(state);
}

/// The focus half of a stack change the caller already made (`cycle_focus`,
/// `send_to_back`, closing a window): `old` is where focus was before it.
pub(crate) fn refocus(
    state: &mut crate::tui::state::AppState,
    old: Option<crate::tui::pane_manager::Window>,
) {
    notify_terminal_focus_change(state, old, state.pane_manager.focused_window);
    sync_terminal_grabbed(state);
}

/// Closes `window` for good: kills a terminal's process, restores the preview it
/// replaced, drops the window's state, and refocuses.
pub(crate) fn close_window(
    state: &mut crate::tui::state::AppState,
    window: crate::tui::pane_manager::Window,
) {
    use crate::tui::pane_manager::Window;
    let old = state.pane_manager.focused_window;
    match window {
        // `close_terminal` is the whole teardown, refocusing included.
        Window::Terminal(id) => {
            close_terminal(state, id);
            state.mark_session_dirty();
            return;
        }
        Window::GitLog(id) => {
            state.git_logs.remove(&id);
        }
        Window::CommitDiff(id) => {
            state.commit_diffs.remove(&id);
        }
        Window::Discussion(id) => {
            crate::tui::panes::discussion_pane::drop_queue_entry(state, id);
            state.discussions.remove(&id);
        }
        Window::FilePreview(key) => {
            state.preview_inputs.remove(&key);
        }
        Window::Report(id) => {
            state.reports.remove(&id);
        }
        Window::ConversationTree
        | Window::WindowPicker
        | Window::ThemePicker
        | Window::ModelPicker => {}
    }
    state.pane_manager.remove_window(&window);
    refocus(state, old);
    state.mark_session_dirty();
}

/// Closes terminal `id`, whether its process ended on its own or the window is being
/// closed: the one path every terminal teardown takes.
pub(crate) fn close_terminal(state: &mut crate::tui::state::AppState, id: usize) {
    use crate::tui::pane_manager::Window;
    let window = Window::Terminal(id);
    let old = state.pane_manager.focused_window;
    match state.terminal_to_preview.remove(&id) {
        Some(file_key) => state
            .pane_manager
            .replace_window(&window, Window::FilePreview(file_key)),
        None => state.pane_manager.remove_window(&window),
    }
    // Before the pane is dropped: the escape goes out through its PTY.
    notify_terminal_focus_change(state, old, state.pane_manager.focused_window);
    if let Some(pane) = state.terminal_panes.remove(&id) {
        let mut p = pane.lock();
        if let Some(mut killer) = p.child_killer.take() {
            let _ = killer.kill();
        }
    }
    sync_terminal_grabbed(state);
}

/// A terminal is grabbed unless it is scrolled back into its history; any other window
/// ungrabs it.
pub(crate) fn sync_terminal_grabbed(state: &mut crate::tui::state::AppState) {
    use crate::tui::pane_manager::Window;
    state.terminal_grabbed = match state.pane_manager.focused_window {
        Some(Window::Terminal(id)) => state
            .with_terminal(id, |pane| pane.scroll_offset == 0)
            .unwrap_or(false),
        _ => false,
    };
}

pub(crate) fn notify_terminal_focus_change(
    state: &crate::tui::state::AppState,
    old: Option<crate::tui::pane_manager::Window>,
    new: Option<crate::tui::pane_manager::Window>,
) {
    use crate::tui::pane_manager::Window;
    if old == new {
        return;
    }
    for (window, bytes) in [(old, b"\x1b[O" as &[u8]), (new, b"\x1b[I")] {
        let Some(Window::Terminal(id)) = window else {
            continue;
        };
        let Some(pane) = state.terminal_panes.get(&id) else {
            continue;
        };
        let p = pane.lock();
        if p.ofs_buf.get_terminal_mode().focus_events {
            let _ = p
                .pty_input_tx
                .try_send(r3bl_tui::PtyInputEvent::Write(bytes.to_vec()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::FileKey;
    use crate::tui::keymap::Keymap;
    use crate::tui::pane_manager::Window;
    use crate::tui::state::AppState;
    use crate::tui::state::PreviewInput;
    use crate::tui::state::SearchDirection;
    use crate::tui::state::TerminalPane;
    use crate::tui::theme::HelixTheme;
    use arc_swap::ArcSwap;
    use camino::Utf8PathBuf;
    use r3bl_tui::OfsBufVT100;
    use r3bl_tui::VPSize;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn empty_state() -> AppState {
        AppState::new(
            Arc::new(ArcSwap::from_pointee(Vec::new())),
            Utf8PathBuf::from("/tmp/nowhere"),
            HelixTheme::default(),
            Keymap::default(),
        )
    }

    /// Nothing here needs a PTY: the pane is the state `close_terminal` works on.
    fn dummy_terminal_pane(scroll_offset: usize) -> TerminalPane {
        let (pty_input_tx, _) = mpsc::channel(1);
        TerminalPane {
            ofs_buf: OfsBufVT100::new_empty(VPSize::default()),
            title: None,
            pty_input_tx: Arc::new(pty_input_tx),
            child_killer: None,
            last_size: VPSize::default(),
            is_command_pane: false,
            exited: false,
            exit_code: None,
            exit_signal: None,
            scroll_offset,
            cwd: Utf8PathBuf::from("/tmp/nowhere"),
            command: None,
            flush_pending: Default::default(),
        }
    }

    fn insert_terminal(state: &mut AppState, id: usize, scroll_offset: usize) {
        state.terminal_panes.insert(
            id,
            Arc::new(parking_lot::Mutex::new(dummy_terminal_pane(scroll_offset))),
        );
    }

    fn with_terminal(state: &mut AppState, id: usize, scroll_offset: usize) {
        insert_terminal(state, id, scroll_offset);
        focus_window(state, Window::Terminal(id));
    }

    /// What `open_terminal` does for `e`: the terminal takes the preview's place in the
    /// stack, and the preview waits in `terminal_to_preview`.
    fn open_editor_terminal(state: &mut AppState, id: usize, key: FileKey) {
        insert_terminal(state, id, 0);
        state.terminal_to_preview.insert(id, key);
        state
            .pane_manager
            .replace_window(&Window::FilePreview(key), Window::Terminal(id));
        state.pane_manager.focused_window = Some(Window::Terminal(id));
        refocus(state, Some(Window::FilePreview(key)));
    }

    #[test]
    fn closing_a_preview_drops_its_state_and_refocuses_the_next_window() {
        let mut state = empty_state();
        let key = FileKey(7);
        focus_window(&mut state, Window::ConversationTree);
        focus_window(&mut state, Window::FilePreview(key));
        state.preview_inputs.insert(
            key,
            PreviewInput::Search(Default::default(), SearchDirection::Forward),
        );

        close_window(&mut state, Window::FilePreview(key));

        assert!(state.preview_inputs.is_empty());
        assert!(
            !state
                .pane_manager
                .window_stack
                .contains(&Window::FilePreview(key))
        );
        assert_eq!(
            state.pane_manager.focused_window,
            Some(Window::ConversationTree)
        );
        assert!(!state.terminal_grabbed);
        assert!(state.session_dirty);
    }

    #[test]
    fn a_terminal_is_grabbed_unless_it_is_scrolled_back() {
        let mut state = empty_state();
        with_terminal(&mut state, 0, 0);
        assert!(
            state.terminal_grabbed,
            "a terminal at its bottom is grabbed"
        );

        focus_window(&mut state, Window::ConversationTree);
        assert!(!state.terminal_grabbed, "another window ungrabs it");

        with_terminal(&mut state, 1, 3);
        assert!(
            !state.terminal_grabbed,
            "a terminal scrolled into its history is not grabbed"
        );
        with_terminal(&mut state, 1, 0);
        assert!(
            state.terminal_grabbed,
            "scrolling back to the bottom grabs it"
        );
    }

    #[test]
    fn closing_an_editor_terminal_restores_the_preview_it_replaced() {
        let mut state = empty_state();
        let key = FileKey(3);
        state.pane_manager.push_window(Window::FilePreview(key));
        open_editor_terminal(&mut state, 0, key);
        assert_eq!(state.pane_manager.focused_window, Some(Window::Terminal(0)));

        close_terminal(&mut state, 0);

        assert!(state.terminal_panes.is_empty());
        assert!(state.terminal_to_preview.is_empty());
        assert_eq!(
            state.pane_manager.window_stack.first(),
            Some(&Window::FilePreview(key)),
            "the preview comes back in place of the terminal"
        );
        assert!(
            !state
                .pane_manager
                .window_stack
                .contains(&Window::Terminal(0))
        );
        assert_eq!(
            state.pane_manager.focused_window,
            Some(Window::FilePreview(key))
        );
        assert!(!state.terminal_grabbed);
    }

    #[test]
    fn closing_a_terminal_without_a_preview_removes_its_window() {
        let mut state = empty_state();
        focus_window(&mut state, Window::ConversationTree);
        with_terminal(&mut state, 0, 0);

        close_terminal(&mut state, 0);

        assert!(state.terminal_panes.is_empty());
        assert!(
            !state
                .pane_manager
                .window_stack
                .contains(&Window::Terminal(0))
        );
        assert_eq!(
            state.pane_manager.focused_window,
            Some(Window::ConversationTree)
        );
        assert!(!state.terminal_grabbed);
    }

    /// The B6 fix through the path `<leader>x` takes: a terminal closed as a window gives
    /// back the preview it was opened from.
    #[test]
    fn close_window_of_an_editor_terminal_restores_its_preview() {
        let mut state = empty_state();
        let key = FileKey(5);
        state.pane_manager.push_window(Window::FilePreview(key));
        open_editor_terminal(&mut state, 2, key);

        close_window(&mut state, Window::Terminal(2));

        assert_eq!(
            state.pane_manager.window_stack.first(),
            Some(&Window::FilePreview(key))
        );
        assert_eq!(
            state.pane_manager.focused_window,
            Some(Window::FilePreview(key))
        );
        assert!(state.terminal_panes.is_empty());
        assert!(state.terminal_to_preview.is_empty());
        assert!(state.session_dirty);
    }
}
