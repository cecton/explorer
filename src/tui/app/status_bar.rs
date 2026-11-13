#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::time::Instant;
pub(super) fn render_status_bar(
    pipeline: &mut RenderPipeline,
    size: VPSize,
    state: &crate::tui::state::AppState,
    theme: &crate::tui::theme::HelixTheme,
) {
    use crate::tui::keymap::Action;
    use crate::tui::pane_manager::Window;
    let bg_rgb = theme
        .ui_bg("ui.statusline")
        .unwrap_or(crate::tui::widgets::title_row::DEFAULT_STATUSLINE_BG);
    let fg_rgb = theme
        .ui_fg("ui.statusline")
        .unwrap_or(crate::tui::widgets::title_row::DEFAULT_STATUSLINE_FG);
    let color_bg = TuiColor::from(bg_rgb);
    let color_fg = TuiColor::from(fg_rgb);

    let leader_style = new_style!(bold color_fg: {color_fg} color_bg: {color_bg});
    let normal_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});

    // Status-bar shortcut hints are derived from the live keymap so they reflect any
    // rebindings from config.kdl, rather than hardcoded key letters.
    let leader_lbl = |a: Action| {
        state
            .keymap
            .leader_key_for(a)
            .map(|k| crate::tui::keymap::hint(&k))
            .unwrap_or_default()
    };
    // Two global keys sharing a modifier read as one chord: `Ctrl+↑/↓`, not `Ctrl+↑/Ctrl+↓`.
    let global_pair = |a: Action, b: Action| state.keymap.global_pair(a, b);

    let (leader_text, rest_text) = if state.leader_active {
        (
            " Leader ".to_string(),
            format!(
                "{}:Picker  {}:Term  {}:Cmd  {}:Theme  {}:Git  {}:Chat  {}:Favorite  {}:Model  {}:To back  {}:Close  {}:Quit  {}:Next/Prev  {}:Cancel",
                leader_lbl(Action::OpenWindowPicker),
                leader_lbl(Action::OpenTerminal),
                leader_lbl(Action::OpenCommandLine),
                leader_lbl(Action::OpenThemePicker),
                leader_lbl(Action::OpenGitLog),
                leader_lbl(Action::OpenConversations),
                leader_lbl(Action::ToggleFavorite),
                leader_lbl(Action::OpenModelPicker),
                leader_lbl(Action::SendToBack),
                leader_lbl(Action::ClosePane),
                leader_lbl(Action::Quit),
                state
                    .keymap
                    .leader_pair(Action::FocusNext, Action::FocusPrev),
                leader_lbl(Action::DismissLeader),
            ),
        )
    } else {
        let pane = {
            let rest = match state.pane_manager.focused_window.as_ref() {
                Some(Window::WindowPicker) => {
                    "Esc:Close picker  Enter:Open  Ctrl+X:Close window".to_string()
                }
                Some(Window::ThemePicker) => "Esc:Cancel  Enter:Save".to_string(),
                Some(Window::FilePreview(_)) => {
                    "e:Edit  Bksp:Send to back  ::Command".to_string()
                }
                Some(Window::GitLog(_)) => {
                    "Bksp:Send to back  b:Branch  ↑↓:Select  /:Search  Enter:Diff  Esc:Clear search".to_string()
                }
                Some(Window::CommitDiff(_)) => {
                    "Bksp:Send to back  Esc:Close  ↑↓PgUp/PgDn:scroll".to_string()
                }
                Some(Window::ConversationTree) => {
                    "Bksp:Send to back  /:Search  n:New  m:Other model  e:Edit  c:Commits  f:Favorite  s:Sub-agents  dd:Delete  Esc:Clear search"
                        .to_string()
                }
                Some(Window::Discussion(_)) => {
                    "Enter:Send  Shift+Enter:New line  PgUp/PgDn:scroll"
                        .to_string()
                }
                Some(Window::Report(_)) => {
                    "Bksp:Send to back  Esc:Close  PgUp/PgDn:scroll".to_string()
                }
                Some(Window::ModelPicker) => model_picker_hint(state),
                Some(Window::Terminal(_)) if !state.terminal_grabbed => {
                    "Bksp:Send to back  Enter:grab  ↑↓PgUp/PgDn:scroll".to_string()
                }
                Some(Window::Terminal(_)) | None => String::new(),
            };
            let alert = match state.pane_manager.focused_window {
                Some(Window::ConversationTree) => conversation_tree_alert(state),
                _ => None,
            };
            // Global shortcuts.
            if let Some(alert) = alert {
                alert
            } else if (!matches!(state.pane_manager.focused_window, Some(Window::Terminal(_)))
                || !state.terminal_grabbed)
                && !state.mouse_drag_active
            {
                format!(
                    "{}:Next/Prev  {}:Resize  {}:Move  {rest}",
                    global_pair(Action::FocusNext, Action::FocusPrev),
                    global_pair(Action::ResizeShrink, Action::ResizeGrow),
                    global_pair(Action::MovePaneForward, Action::MovePaneBackward),
                )
            } else {
                rest
            }
        };
        (
            format!(
                " {}: Leader ",
                crate::tui::keymap::hint(&state.keymap.leader_key)
            ),
            pane,
        )
    };

    let styled_texts = tui_styled_texts! {
        tui_styled_text! {
            @style: leader_style,
            @text: leader_text
        },
        tui_styled_text! {
            @style: normal_style,
            @text: rest_text
        },
    };

    let row_idx = size.row_height.convert_to_index();
    let mut render_ops = RenderOpIRVec::new();
    render_ops += RenderOpCommon::MoveCursorPositionAbs(vp_col(0) + row_idx);
    render_ops += RenderOpCommon::ResetColor;
    render_ops += RenderOpCommon::SetBgColor(color_bg);
    render_ops += RenderOpIR::PaintTextWithAttributes(
        SPACER_GLYPH.repeat(size.col_width.as_usize()).into(),
        None,
    );
    render_ops += RenderOpCommon::MoveCursorPositionAbs(vp_col(0) + row_idx);
    render_tui_styled_texts_into(&styled_texts, &mut render_ops);

    // Right-aligned "dirty" flag (uncommitted staged/unstaged changes), shown in a
    // bright error-like theme color, followed by the usage of the spending behind the
    // focused conversation's endpoint. The flag reflects `state.is_dirty` unconditionally.
    let error_fg = TuiColor::from(
        theme
            .ui_fg("error")
            .unwrap_or(crate::tui::widgets::title_row::DEFAULT_ERROR_FG),
    );
    let warning_fg = TuiColor::from(
        theme
            .ui_fg("warning")
            .unwrap_or(crate::tui::widgets::title_row::DEFAULT_WARNING_FG),
    );
    let dirty_style = new_style!(bold color_fg: {error_fg} color_bg: {color_bg});
    let warning_style = new_style!(color_fg: {warning_fg} color_bg: {color_bg});
    let error_style = new_style!(color_fg: {error_fg} color_bg: {color_bg});
    let dirty = state.is_dirty.then_some(("│ ", "dirty "));
    let usage = focused_usage(state);
    let after_agents = state
        .after_agents
        .as_ref()
        .map(|after| crate::tui::after_agents::status_text(after, Instant::now()));
    let clock = {
        let now = crate::util::time::now_secs();
        clock_text(now, crate::util::time::local_offset_secs(now))
    };
    let width_of = crate::tui::widgets::text_layout::display_width;
    let dirty_width = dirty.map_or(0, |(bar, label)| width_of(bar) + width_of(label));
    let usage_width: usize = usage.iter().map(|(text, _)| width_of(text)).sum();
    let after_width = after_agents.as_ref().map_or(0, |(text, _)| width_of(text));
    let clock_width = width_of(&clock);
    let total_width = size.col_width.as_usize();
    // Usage goes first, then after-agents: a narrow terminal keeps the flag and the clock.
    let base_width = dirty_width + clock_width;
    let usage_fits = total_width > base_width + after_width + usage_width + 2;
    let after_fits = total_width > base_width + after_width + 2;
    let combined_width = base_width
        + if after_fits { after_width } else { 0 }
        + if usage_fits { usage_width } else { 0 };
    // Only draw when it fits alongside the left hints (leave a small gap), so narrow
    // terminals keep the shortcut hints rather than being overwritten.
    if combined_width > 0 && total_width > combined_width + 2 {
        let start_col = total_width - combined_width - 1;
        render_ops += RenderOpCommon::MoveCursorPositionAbs(vp_col(start_col as u16) + row_idx);
        render_ops += RenderOpIR::PaintTextWithAttributes(" ".into(), Some(normal_style));
        if let Some((bar, label)) = dirty {
            render_ops += RenderOpIR::PaintTextWithAttributes(bar.into(), Some(normal_style));
            render_ops += RenderOpIR::PaintTextWithAttributes(label.into(), Some(dirty_style));
        }
        if after_fits && let Some((text, counting_down)) = after_agents {
            let style = if counting_down {
                warning_style
            } else {
                normal_style
            };
            render_ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
        }
        if usage_fits {
            for (text, percent) in usage {
                let style = match percent {
                    Some(USAGE_ERROR_PERCENT..) => error_style,
                    Some(USAGE_WARNING_PERCENT..) => warning_style,
                    _ => normal_style,
                };
                render_ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
            }
        }
        render_ops += RenderOpIR::PaintTextWithAttributes(clock.into(), Some(normal_style));
    }

    pipeline.push(ZOrder::Normal, render_ops);
}

/// The status-bar clock: local time of day, 24h, as a `│ HH:MM ` segment.
fn clock_text(now: i64, offset_secs: i32) -> String {
    format!("│ {} ", crate::util::time::local_hhmm(now, offset_secs))
}

/// Percent of a subscription window from which the status bar warns about it.
const USAGE_WARNING_PERCENT: u32 = 75;
const USAGE_ERROR_PERCENT: u32 = 90;

/// The endpoint the focused window spends: the model of a Discussion window, the
/// endpoint of the Conversation Tree's selected node or sub-agent run (for a queued
/// row, the model of the window holding the prompt), the selected row of the Model
/// Picker.
fn focused_endpoint(state: &crate::tui::state::AppState) -> Option<String> {
    use crate::tui::pane_manager::Window;
    use crate::tui::state::RowTarget;
    let window_endpoint =
        |id: &usize| Some(state.discussions.get(id)?.model.as_ref()?.endpoint.clone());
    match state.pane_manager.focused_window.as_ref()? {
        Window::Discussion(id) => window_endpoint(id),
        Window::ConversationTree => match state.tree_view.selected.as_ref()? {
            RowTarget::Node(node) => state.conversations.read().node(node)?.meta.endpoint.clone(),
            RowTarget::Agent { node, index } => state
                .conversations
                .read()
                .agent(node, *index)?
                .meta
                .endpoint
                .clone(),
            RowTarget::Queued { window, .. } => window_endpoint(window),
        },
        Window::ModelPicker => {
            let picker = &state.model_picker;
            let selected = picker
                .selected
                .or_else(|| picker.results.first().copied())?;
            Some(state.models.specs.get(selected)?.endpoint.clone())
        }
        _ => None,
    }
}

/// The status-bar entry for the spending behind the focused window's endpoint;
/// empty when that endpoint reports no usage or its usage is not known yet.
fn focused_usage(state: &crate::tui::state::AppState) -> Vec<(String, Option<u32>)> {
    let entry = || {
        let endpoint = focused_endpoint(state)?;
        let kind = &state.agent.endpoint(&endpoint)?.kind;
        let label = kind.usage_label()?;
        let usage = state.usage.get(&endpoint)?;
        Some(usage.entry(
            kind.provider(),
            label,
            crate::util::time::now_secs(),
            crate::util::time::local_offset_secs,
        ))
    };
    entry().unwrap_or_default()
}

/// Asks what each OpenRouter key of the configuration has spent. Its counter settles
/// seconds after the requests that spent it, so the display is polled at startup and
/// between turns, not only when a turn ends.
pub(super) fn poll_openrouter_usage(
    state: &crate::tui::state::AppState,
    main_tx: crate::signal::MainTx,
) {
    use crate::endpoint::EndpointKind;
    for endpoint in &state.agent.endpoints {
        let EndpointKind::Http { provider, .. } = &endpoint.kind else {
            continue;
        };
        if crate::usage::polls_the_key(*provider) {
            crate::agent::turn::report_usage(endpoint.clone(), None, main_tx.clone());
        }
    }
}

fn model_picker_hint(state: &crate::tui::state::AppState) -> String {
    let errors = state.models.errors();
    if !errors.is_empty() {
        return errors.join("  ");
    }
    if state.agent.endpoints.is_empty() {
        return "No endpoint: declare one in config.kdl and its token in auth.kdl".to_string();
    }
    "Esc:Cancel  Enter:Select".to_string()
}

/// A message that must not be cut off by the regular hints: a failure, or a pending
/// delete confirmation.
fn conversation_tree_alert(state: &crate::tui::state::AppState) -> Option<String> {
    if let Some(notice) = &state.tree_view.notice {
        return Some(notice.clone());
    }
    let id = state.tree_view.armed_delete.as_ref()?;
    let count = state.conversations.read().subtree_len(id);
    Some(format!(
        "d again: delete {count} node(s)  any other key: Cancel"
    ))
}

#[cfg(test)]
mod tests {
    use super::clock_text;

    #[test]
    fn clock_text_is_local_24h() {
        assert_eq!(clock_text(0, 0), "│ 00:00 ");
        assert_eq!(clock_text(86_399, 0), "│ 23:59 ");
        assert_eq!(clock_text(0, 7_200), "│ 02:00 ");
        assert_eq!(clock_text(0, -3_600), "│ 23:00 ");
    }
}
