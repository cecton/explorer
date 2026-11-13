#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

pub(super) fn handle(
    app: &mut crate::tui::app::AppMain,
    input_event: InputEvent,
    global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
    component_registry_map: &mut ComponentRegistryMap<
        crate::tui::state::AppState,
        crate::signal::AppSignal,
    >,
    has_focus: &mut HasFocus,
) -> CommonResult<EventPropagation> {
    use crate::tui::command_line::CommandLine;
    use crate::tui::keymap::Action;
    use crate::tui::pane_manager::Window;
    use crate::tui::panes::git_log_pane::GitLogPaneComponent;
    use crate::tui::panes::pickers::model_picker::ModelPickerComponent;
    use crate::tui::state::GitLogWindowState;
    use crate::tui::state::LogSource;
    use crate::tui::state::ModelPickerPurpose;
    use crate::tui::state::RowTarget;
    use crate::tui::theme::HelixTheme;
    crate::tui::app::sync_has_focus(&global_data.state, has_focus);
    let surface_size = crate::tui::app::surface_size(global_data.window_size);
    global_data.state.pane_manager.relayout(surface_size);

    if crate::tui::after_agents::cancel_on_input(&mut global_data.state, &input_event) {
        return Ok(EventPropagation::ConsumedRender);
    }

    if global_data.state.command_line.is_some()
        && CommandLine::handle_event(&input_event, global_data)
    {
        return Ok(EventPropagation::ConsumedRender);
    }

    // The single KeyPress driving all top-level (leader + global) shortcuts, resolved
    // against the configurable keymap below.
    let keyboard = match input_event {
        InputEvent::Keyboard(kp) => Some(kp),
        _ => None,
    };

    // Leader key activation.
    if let Some(kp) = keyboard
        && kp == global_data.state.keymap.leader_key
        && !global_data.state.mouse_drag_active
        && !global_data.state.leader_active
    {
        global_data.state.leader_active = true;
        global_data.state.terminal_grabbed = false;
        return Ok(EventPropagation::ConsumedRender);
    }

    // Leader key dispatch.
    if global_data.state.leader_active && !global_data.state.mouse_drag_active {
        global_data.state.leader_active = false;
        match keyboard.and_then(|kp| global_data.state.keymap.leader_action(&kp)) {
            Some(Action::GrabTerminal) => {
                // Pressing the leader key again grabs the focused terminal. Unlike the
                // other arms this deliberately does not return: the event still falls
                // through to the focused component (matching the original behavior).
                if matches!(
                    global_data.state.pane_manager.focused_window,
                    Some(Window::Terminal(_))
                ) {
                    global_data.state.terminal_grabbed = true;
                }
            }
            Some(Action::OpenWindowPicker) => {
                let state = &mut global_data.state;
                crate::tui::focus::focus_window(state, Window::WindowPicker);
                state.window_picker.selected = None;
                state.window_picker.query.clear();
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::OpenTerminal) => {
                return crate::tui::panes::terminal::spawn::open(
                    global_data,
                    crate::tui::panes::terminal::spawn::Request {
                        cmd: None,
                        cwd: None,
                        pane_size: None,
                        id: None,
                        replace_window: None,
                        event_tx: &app.terminal_event_tx,
                    },
                );
            }
            Some(Action::OpenCommandLine) => {
                let state = &mut global_data.state;
                state.command_line = Some(CommandLine::new(state.pane_manager.focused_window));
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::OpenThemePicker) => {
                let state = &mut global_data.state;
                if !state
                    .pane_manager
                    .window_stack
                    .contains(&Window::ThemePicker)
                {
                    state.saved_theme = state.theme.clone();
                }
                let all_themes: Vec<String> =
                    HelixTheme::theme_names().map(str::to_string).collect();
                crate::tui::focus::focus_window(state, Window::ThemePicker);
                state.theme_picker.selected = all_themes
                    .iter()
                    .find(|name| name.as_str() == state.theme.name())
                    .cloned();
                state.theme_picker.results = all_themes;
                state.theme_picker.query.clear();
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::OpenGitLog) => {
                let state = &mut global_data.state;
                let id = state.next_id();
                state.git_logs.insert(id, GitLogWindowState::default());
                crate::tui::focus::focus_window(state, Window::GitLog(id));
                let main_tx = global_data.main_thread_channel_sender.clone();
                GitLogPaneComponent::reload_log(state.repo.clone(), main_tx, id, &LogSource::Head);
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::OpenConversations) => {
                let state = &mut global_data.state;
                crate::tui::focus::focus_window(state, Window::ConversationTree);
                state.mark_session_dirty();
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::ToggleFavorite) => {
                let state = &mut global_data.state;
                let node = match state.pane_manager.focused_window {
                    Some(Window::Discussion(id)) => state
                        .discussions
                        .get(&id)
                        .filter(|discussion| discussion.agent.is_none())
                        .and_then(|discussion| discussion.node.clone()),
                    Some(Window::ConversationTree) => state
                        .tree_view
                        .selected
                        .as_ref()
                        .and_then(RowTarget::node)
                        .cloned(),
                    _ => None,
                };
                if let Some(node) = node
                    && let Err(e) = state.conversations.write().toggle_favorite(&node)
                {
                    tracing::warn!("failed to save favorite: {e}");
                    let notice = Some(format!("favorite not saved: {e}"));
                    match state.pane_manager.focused_window {
                        Some(Window::Discussion(id)) => {
                            if let Some(discussion) = state.discussions.get_mut(&id) {
                                discussion.notice = notice;
                            }
                        }
                        _ => state.tree_view.notice = notice,
                    }
                }
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::OpenModelPicker) => {
                if let Some(Window::Discussion(id)) = global_data.state.pane_manager.focused_window
                    && global_data
                        .state
                        .discussions
                        .get(&id)
                        .is_some_and(|discussion| discussion.agent.is_none())
                {
                    let main_tx = global_data.main_thread_channel_sender.clone();
                    let purpose = ModelPickerPurpose::Window(id);
                    ModelPickerComponent::open(&mut global_data.state, main_tx, purpose);
                }
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(action) => {
                if let Some(propagation) = crate::tui::app::input::apply_action(action, global_data)
                {
                    return Ok(propagation);
                }
            }
            _ => {}
        }
    }

    // Global shortcuts: Tab/BackTab cycles focus; Ctrl+Up/Down resize; Ctrl+Left/Right reorder.
    // Skipped for Terminal panes so Tab reaches the PTY (e.g. shell completion).
    // When a terminal is ungrabbed Tab cycles focus.
    let shortcuts_reach_here = (!matches!(
        global_data.state.pane_manager.focused_window,
        Some(Window::Terminal(_))
    ) || !global_data.state.terminal_grabbed)
        && !global_data.state.mouse_drag_active;
    if shortcuts_reach_here
        && let Some(action) = keyboard.and_then(|kp| global_data.state.keymap.global_action(&kp))
        && let Some(propagation) = crate::tui::app::input::apply_action(action, global_data)
    {
        return Ok(propagation);
    }

    // Focus follows mouse: hovering over a pane makes it the focused window.
    if let InputEvent::Mouse(MouseInput {
        kind: MouseInputKind::MouseMove,
        maybe_modifier_keys: None,
        pos,
    }) = &input_event
        && !global_data.state.mouse_drag_active
    {
        let layout = global_data.state.pane_manager.visible();
        for slot in layout {
            let ox = slot.box_.style_adjusted_bounds.origin_pos.col_index;
            let oy = slot.box_.style_adjusted_bounds.origin_pos.row_index;
            let w = slot.box_.style_adjusted_bounds.bounds_size.col_width;
            let h = slot.box_.style_adjusted_bounds.bounds_size.row_height;
            if pos.col_index >= ox
                && pos.col_index < ox + w
                && pos.row_index >= oy
                && pos.row_index < oy + h
            {
                let window = slot.window;
                if global_data.state.pane_manager.focused_window != Some(window) {
                    let old = global_data.state.pane_manager.focused_window;
                    global_data.state.pane_manager.focused_window = Some(window);
                    crate::tui::focus::focus_pointer(&mut global_data.state, old);
                    return Ok(EventPropagation::ConsumedRender);
                }
                break;
            }
        }
    }

    ComponentRegistry::route_event_to_focused_component(
        global_data,
        input_event,
        component_registry_map,
        has_focus,
    )
}

/// Runs the shortcuts that mean the same thing in both keymap contexts, so the leader and
/// global tables share one body. Returns the propagation when it acted, `None` when the
/// action is one only the context that resolved it may handle.
fn apply_action(
    action: crate::tui::keymap::Action,
    global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
) -> Option<EventPropagation> {
    use crate::tui::keymap::Action;
    use crate::tui::pane_manager::ResizeDelta;
    let state = &mut global_data.state;
    match action {
        Action::FocusNext | Action::FocusPrev => {
            let old = state.pane_manager.focused_window;
            let direction = if action == Action::FocusNext { 1 } else { -1 };
            state.pane_manager.cycle_focus(direction);
            crate::tui::focus::refocus(state, old);
        }
        Action::ResizeGrow | Action::ResizeShrink => {
            let delta = if action == Action::ResizeGrow {
                ResizeDelta::Grow
            } else {
                ResizeDelta::Shrink
            };
            state.pane_manager.resize_focused(delta);
            state.mark_session_dirty();
        }
        Action::MovePaneForward | Action::MovePaneBackward => {
            let Some(focused) = state.pane_manager.focused_window else {
                return Some(EventPropagation::ConsumedRender);
            };
            if action == Action::MovePaneForward {
                state.pane_manager.move_forward(&focused);
            } else {
                state.pane_manager.move_backward(&focused);
            }
            state.mark_session_dirty();
        }
        Action::ClosePane => {
            let Some(old) = state.pane_manager.focused_window else {
                return Some(EventPropagation::ConsumedRender);
            };
            crate::tui::focus::close_window(state, old);
        }
        Action::SendToBack => {
            let Some(focused) = state.pane_manager.focused_window else {
                return Some(EventPropagation::ConsumedRender);
            };
            state.pane_manager.send_to_back(&focused);
            crate::tui::focus::refocus(state, Some(focused));
            state.mark_session_dirty();
        }
        // Only the leader table binds this, so the global path can never reach it.
        Action::Quit => return Some(EventPropagation::ExitMainEventLoop),
        Action::DismissLeader => {}
        // The rest needs the context that resolved it: opening a window, or grabbing.
        Action::GrabTerminal
        | Action::OpenWindowPicker
        | Action::OpenTerminal
        | Action::OpenCommandLine
        | Action::OpenThemePicker
        | Action::OpenGitLog
        | Action::OpenConversations
        | Action::ToggleFavorite
        | Action::OpenModelPicker => return None,
    }
    Some(EventPropagation::ConsumedRender)
}
