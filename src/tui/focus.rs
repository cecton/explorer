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
/// `send_to_back`, closing a window, a window opening over a terminal): `old` is where
/// focus was before it. The keyboard is only ever *given back* here, never taken, and it
/// is given back whenever focus moved at all, so Tab can land on a terminal and Tab again
/// leave it, and one terminal cannot inherit another's grab. Only a terminal the user
/// picked explicitly takes the keyboard (see `focus_pointer`, `Enter`/`q` in a pane, and
/// a freshly spawned terminal).
pub(crate) fn refocus(
    state: &mut crate::tui::state::AppState,
    old: Option<crate::tui::pane_manager::Window>,
) {
    notify_terminal_focus_change(state, old, state.pane_manager.focused_window);
    if state.pane_manager.focused_window != old {
        state.terminal_grabbed = false;
    }
}

/// Focus followed the pointer onto the pane now at `focused_window`: a terminal the user
/// pointed at takes the keyboard, unless it is scrolled back into its history.
pub(crate) fn focus_pointer(
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
            crate::tui::panes::discussion::queue::drop_queue_entry(state, id);
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
    // Closing a terminal is not the user picking a new one: the terminal focus falls onto
    // is left ungrabbed, and a background close leaves the focused one as it was.
    if state.pane_manager.focused_window != old {
        state.terminal_grabbed = false;
    }
}

/// A terminal is grabbed unless it is scrolled back into its history; any other window
/// ungrabs it.
fn sync_terminal_grabbed(state: &mut crate::tui::state::AppState) {
    use crate::tui::pane_manager::Window;
    state.terminal_grabbed = match state.pane_manager.focused_window {
        Some(Window::Terminal(id)) => state
            .with_terminal(id, |pane| pane.scroll_offset == 0)
            .unwrap_or(false),
        _ => false,
    };
}

fn notify_terminal_focus_change(
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
    use crate::tui::pane_manager::Window;
    use crate::tui::state::AppState;
    use crate::tui::state::PreviewInput;
    use crate::tui::state::SearchDirection;
    use crate::tui::state::TerminalPane;
    use camino::Utf8PathBuf;
    use r3bl_tui::OfsBufVT100;
    use r3bl_tui::VPSize;
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn empty_state() -> AppState {
        AppState::for_test(Utf8PathBuf::from("/tmp/nowhere"))
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
    /// stack, the preview waits in `terminal_to_preview`, and the terminal takes the
    /// keyboard as a freshly spawned one does.
    fn open_editor_terminal(state: &mut AppState, id: usize, key: FileKey) {
        insert_terminal(state, id, 0);
        state.terminal_to_preview.insert(id, key);
        state
            .pane_manager
            .replace_window(&Window::FilePreview(key), Window::Terminal(id));
        state.pane_manager.focused_window = Some(Window::Terminal(id));
        refocus(state, Some(Window::FilePreview(key)));
        state.terminal_grabbed = true;
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

    /// Tab owned by the PTY is a Tab the user cannot use to leave the pane: focus that
    /// moved onto a terminal without the user pointing at it must leave the keyboard free.
    #[test]
    fn tabbing_onto_a_terminal_does_not_grab_the_keyboard() {
        let mut state = empty_state();
        focus_window(&mut state, Window::ConversationTree);
        insert_terminal(&mut state, 0, 0);
        state.pane_manager.push_window(Window::Terminal(0));

        let old = state.pane_manager.focused_window;
        state.pane_manager.focused_window = Some(Window::Terminal(0));
        refocus(&mut state, old);

        assert!(
            !state.terminal_grabbed,
            "Tab onto a terminal leaves the keyboard free, so Tab can leave it again"
        );
    }

    /// The pointer is how the user says "this one": a terminal it lands on takes the
    /// keyboard, unless it is scrolled back into its history.
    #[test]
    fn pointing_at_a_terminal_grabs_the_keyboard() {
        let mut state = empty_state();
        focus_window(&mut state, Window::ConversationTree);
        insert_terminal(&mut state, 0, 0);
        state.pane_manager.push_window(Window::Terminal(0));

        let old = state.pane_manager.focused_window;
        state.pane_manager.focused_window = Some(Window::Terminal(0));
        focus_pointer(&mut state, old);
        assert!(
            state.terminal_grabbed,
            "pointing at a live terminal grabs it"
        );

        let mut scrolled = empty_state();
        focus_window(&mut scrolled, Window::ConversationTree);
        insert_terminal(&mut scrolled, 1, 4);
        scrolled.pane_manager.push_window(Window::Terminal(1));
        let old = scrolled.pane_manager.focused_window;
        scrolled.pane_manager.focused_window = Some(Window::Terminal(1));
        focus_pointer(&mut scrolled, old);
        assert!(
            !scrolled.terminal_grabbed,
            "a terminal scrolled into its history is not grabbed by the pointer"
        );
    }

    /// A terminal ending in the background is not the user picking the one in front: the
    /// focused terminal keeps the keyboard it had, grabbed or not.
    #[test]
    fn closing_a_background_terminal_leaves_the_focused_one_as_it_was() {
        let mut state = empty_state();
        insert_terminal(&mut state, 0, 0);
        insert_terminal(&mut state, 1, 0);
        state.pane_manager.push_window(Window::Terminal(1));
        state.pane_manager.push_window(Window::Terminal(0));
        // Focus arrived at terminal 1 the way Tab leaves it: focused, not grabbed.
        state.pane_manager.focused_window = Some(Window::Terminal(1));
        refocus(&mut state, Some(Window::Terminal(0)));
        assert!(!state.terminal_grabbed);

        close_terminal(&mut state, 0);

        assert_eq!(state.pane_manager.focused_window, Some(Window::Terminal(1)));
        assert!(
            !state.terminal_grabbed,
            "a background terminal ending does not grab the focused one"
        );
    }

    /// The render fallback moves focus when the focused window stops being visible: the
    /// terminal it lands on is a terminal focus arrives at, so it does not inherit a grab.
    #[test]
    fn focus_moving_between_terminals_releases_the_keyboard() {
        let mut state = empty_state();
        insert_terminal(&mut state, 0, 0);
        insert_terminal(&mut state, 1, 0);
        state.pane_manager.push_window(Window::Terminal(1));
        state.pane_manager.push_window(Window::Terminal(0));
        state.pane_manager.focused_window = Some(Window::Terminal(0));
        state.terminal_grabbed = true;

        state.pane_manager.focused_window = Some(Window::Terminal(1));
        refocus(&mut state, Some(Window::Terminal(0)));

        assert!(
            !state.terminal_grabbed,
            "a terminal focus moved to does not keep the previous one's grab"
        );
    }

    /// Focus falling from a grabbed terminal onto another is the same arrival as Tab: the
    /// second one does not inherit the first one's grab.
    #[test]
    fn a_closed_grabbed_terminal_does_not_hand_its_grab_to_the_next() {
        let mut state = empty_state();
        insert_terminal(&mut state, 0, 0);
        insert_terminal(&mut state, 1, 0);
        state.pane_manager.push_window(Window::Terminal(1));
        state.pane_manager.push_window(Window::Terminal(0));
        state.pane_manager.focused_window = Some(Window::Terminal(0));
        state.terminal_grabbed = true;

        close_terminal(&mut state, 0);

        assert_eq!(state.pane_manager.focused_window, Some(Window::Terminal(1)));
        assert!(
            !state.terminal_grabbed,
            "the terminal focus fell onto is not grabbed"
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
