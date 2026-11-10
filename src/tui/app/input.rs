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
    use crate::tui::pane_manager::ResizeDelta;
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
    global_data.state.last_surface_size = surface_size;

    if crate::tui::after_agents::cancel_on_input(&mut global_data.state, &input_event) {
        return Ok(EventPropagation::ConsumedRender);
    }

    // The command line takes every key while it is open, ahead of the leader key.
    // `Tab`/`Shift+Tab` walk the palette and `Enter` takes what it highlights; a second
    // `Enter` runs the completed line. The arrows stay with the input, as everywhere.
    if global_data.state.command_line.is_some() {
        if !matches!(input_event, InputEvent::Mouse(_))
            && global_data
                .state
                .command_line
                .as_ref()
                .is_some_and(|command_line| command_line.input.selecting())
        {
            if let Some(command_line) = global_data.state.command_line.as_mut() {
                command_line.input.cancel_select();
            }
            global_data.state.mouse_drag_active = false;
        }
        let entries = global_data
            .state
            .command_line
            .as_ref()
            .map(|command_line| {
                crate::tui::command_line::palette_entries(
                    command_line.input.text(),
                    &global_data.state,
                )
            })
            .unwrap_or_default();
        match &input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Tab),
            }) if !entries.is_empty() => {
                if let Some(command_line) = global_data.state.command_line.as_mut() {
                    command_line.move_selection(true, entries.len());
                    command_line.error = None;
                }
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::BackTab),
            }) if !entries.is_empty() => {
                if let Some(command_line) = global_data.state.command_line.as_mut() {
                    command_line.move_selection(false, entries.len());
                    command_line.error = None;
                }
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Enter),
            }) if !entries.is_empty() => {
                if let Some(command_line) = global_data.state.command_line.as_mut() {
                    command_line.accept(&entries);
                }
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Enter),
            }) => {
                crate::tui::command_line::run_command_line(global_data);
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }) => {
                global_data.state.command_line = None;
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
            }) => {
                if let Some(command_line) = global_data.state.command_line.as_mut() {
                    command_line.input.clear();
                    command_line.error = None;
                    command_line.selected = 0;
                }
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('d'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) if global_data
                .state
                .command_line
                .as_ref()
                .is_some_and(|command_line| command_line.input.text().is_empty()) =>
            {
                global_data.state.command_line = None;
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(_) | InputEvent::BracketedPaste(_) => {
                if let Some(command_line) = global_data.state.command_line.as_mut() {
                    let before = command_line.input.text().to_string();
                    command_line.input.handle_key(&input_event);
                    command_line.error = None;
                    if command_line.input.text() != before {
                        command_line.selected = 0;
                    }
                }
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Mouse(mouse) => {
                let Some(command_line) = global_data.state.command_line.as_mut() else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                if command_line.input.handle_mouse(mouse) {
                    global_data.state.mouse_drag_active = command_line.input.selecting();
                    return Ok(EventPropagation::ConsumedRender);
                }
                // A mouse event the input does not take means its release was lost.
                if command_line.input.selecting() {
                    command_line.input.cancel_select();
                    global_data.state.mouse_drag_active = false;
                }
            }
            _ => {}
        }
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
                return app.open_terminal(global_data, None, None, None, None, None);
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
                let all_themes: Vec<(String, Vec<u32>)> = HelixTheme::theme_names()
                    .map(|n| (n.to_string(), Vec::new()))
                    .collect();
                crate::tui::focus::focus_window(state, Window::ThemePicker);
                state.theme_picker.selected = all_themes
                    .iter()
                    .position(|(n, _)| n == state.theme.name())
                    .and_then(|i| all_themes.get(i).map(|(n, _)| n.clone()));
                state.theme_picker.results = all_themes;
                state.theme_picker.query.clear();
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::OpenGitLog) => {
                let state = &mut global_data.state;
                let id = state.next_git_log_id;
                state.next_git_log_id += 1;
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
            Some(Action::Quit) => {
                return Ok(EventPropagation::ExitMainEventLoop);
            }
            Some(Action::FocusNext) => {
                let visible = global_data.state.pane_manager.layout(surface_size);
                let old = global_data.state.pane_manager.focused_window;
                global_data.state.pane_manager.cycle_focus(&visible, 1);
                crate::tui::focus::refocus(&mut global_data.state, old);
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::FocusPrev) => {
                let visible = global_data.state.pane_manager.layout(surface_size);
                let old = global_data.state.pane_manager.focused_window;
                global_data.state.pane_manager.cycle_focus(&visible, -1);
                crate::tui::focus::refocus(&mut global_data.state, old);
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::ClosePane) => {
                let state = &mut global_data.state;
                if let Some(old) = state.pane_manager.focused_window {
                    crate::tui::focus::close_window(state, old);
                }
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::SendToBack) => {
                let state = &mut global_data.state;
                if let Some(focused) = state.pane_manager.focused_window {
                    state.pane_manager.send_to_back(&focused);
                    crate::tui::focus::refocus(state, Some(focused));
                    state.mark_session_dirty();
                }
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::DismissLeader) => {
                return Ok(EventPropagation::ConsumedRender);
            }
            _ => {}
        }
    }

    // Global shortcuts: Tab/BackTab cycles focus; Ctrl+Up/Down resize; Ctrl+Left/Right reorder.
    // Skipped for Terminal panes so Tab reaches the PTY (e.g. shell completion).
    // When a terminal is ungrabbed Tab cycles focus.
    if (!matches!(
        global_data.state.pane_manager.focused_window,
        Some(Window::Terminal(_))
    ) || !global_data.state.terminal_grabbed)
        && !global_data.state.mouse_drag_active
    {
        match keyboard.and_then(|kp| global_data.state.keymap.global_action(&kp)) {
            Some(Action::FocusNext) => {
                let visible = global_data.state.pane_manager.layout(surface_size);
                let old = global_data.state.pane_manager.focused_window;
                global_data.state.pane_manager.cycle_focus(&visible, 1);
                crate::tui::focus::refocus(&mut global_data.state, old);
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::FocusPrev) => {
                let visible = global_data.state.pane_manager.layout(surface_size);
                let old = global_data.state.pane_manager.focused_window;
                global_data.state.pane_manager.cycle_focus(&visible, -1);
                crate::tui::focus::refocus(&mut global_data.state, old);
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::ResizeGrow) => {
                global_data
                    .state
                    .pane_manager
                    .resize_focused(ResizeDelta::Grow);
                global_data.state.mark_session_dirty();
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::ResizeShrink) => {
                global_data
                    .state
                    .pane_manager
                    .resize_focused(ResizeDelta::Shrink);
                global_data.state.mark_session_dirty();
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::MovePaneForward) => {
                let Some(focused) = global_data.state.pane_manager.focused_window else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                global_data.state.pane_manager.move_forward(&focused);
                global_data.state.mark_session_dirty();
                return Ok(EventPropagation::ConsumedRender);
            }
            Some(Action::MovePaneBackward) => {
                let Some(focused) = global_data.state.pane_manager.focused_window else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                global_data.state.pane_manager.move_backward(&focused);
                global_data.state.mark_session_dirty();
                return Ok(EventPropagation::ConsumedRender);
            }
            _ => {}
        }
    }

    // Focus follows mouse: hovering over a pane makes it the focused window.
    if let InputEvent::Mouse(MouseInput {
        kind: MouseInputKind::MouseMove,
        maybe_modifier_keys: None,
        pos,
    }) = &input_event
        && !global_data.state.mouse_drag_active
    {
        let layout = global_data.state.pane_manager.layout(surface_size);
        for slot in &layout {
            let ox = slot.box_.style_adjusted_bounds.origin_pos.col_index;
            let oy = slot.box_.style_adjusted_bounds.origin_pos.row_index;
            let w = slot.box_.style_adjusted_bounds.bounds_size.col_width;
            let h = slot.box_.style_adjusted_bounds.bounds_size.row_height;
            if pos.col_index >= ox
                && pos.col_index < ox + w
                && pos.row_index >= oy
                && pos.row_index < oy + h
            {
                if global_data.state.pane_manager.focused_window.as_ref() != Some(&slot.window) {
                    let old = global_data.state.pane_manager.focused_window;
                    global_data.state.pane_manager.focused_window = Some(slot.window);
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
