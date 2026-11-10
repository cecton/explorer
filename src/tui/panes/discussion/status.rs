#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::time::Instant;
/// Share of the theme's warning color in the background of highlighted text.
const MARKER_STRENGTH: f32 = 0.4;

/// Background of highlighted text: the theme's warning color laid over the pane
/// background, since `ui.selection` barely differs from it in many themes.
pub(crate) fn marker_bg(theme: &crate::tui::theme::HelixTheme, pane_bg: RgbValue) -> RgbValue {
    let Some(accent) = theme.color_for_scope("warning") else {
        return theme.ui_bg("ui.selection").unwrap_or(pane_bg);
    };
    let mix = |bg: u8, accent: u8| {
        (f32::from(bg) + (f32::from(accent) - f32::from(bg)) * MARKER_STRENGTH).round() as u8
    };
    RgbValue::from_u8(
        mix(pane_bg.red, accent.red),
        mix(pane_bg.green, accent.green),
        mix(pane_bg.blue, accent.blue),
    )
}

/// One-line description of the outcome of a node or a sub-agent run, and whether it
/// reports a failure.
pub(crate) fn status_text(meta: &crate::conversation::metadata::Metadata) -> (String, bool) {
    use crate::conversation::NodeStatus;
    use crate::conversation::NodeType;
    let kind = match meta.node_type {
        Some(NodeType::Compact) => "compact",
        _ => "",
    };
    let (mut text, failed) = match meta.status {
        Some(NodeStatus::Failed) => {
            let error = meta.error.as_deref().unwrap_or("unknown error");
            (format!("failed: {error}"), true)
        }
        Some(NodeStatus::Cancelled) => ("cancelled".to_string(), false),
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
    let hashes: Vec<String> = meta
        .commits
        .iter()
        .map(|hash| hash.chars().take(7).collect())
        .collect();
    push(&hashes.join(" "));
    if let Some(model) = meta.model_label() {
        push(&model);
    }
    if let Some(tokens) = meta.tokens_in {
        push(&fmt_tokens(tokens));
    }
    if let Some(cached) = meta.cached_in {
        push(&format!("{} cached", fmt_tokens(cached)));
    }
    if let Some(secs) = meta.secs {
        push(&fmt_elapsed(secs));
    }
    (text, failed)
}

/// Size of the context of a turn: `962`, `96k`, `1.2M`.
pub(crate) fn fmt_tokens(tokens: u64) -> String {
    match tokens {
        0..1000 => tokens.to_string(),
        1000..1_000_000 => format!("{}k", tokens / 1000),
        _ => format!("{}.{}M", tokens / 1_000_000, tokens % 1_000_000 / 100_000),
    }
}

/// How long a turn ran or has been running: `5s`, `2m 5s`, `1h 0m 5s`.
pub(crate) fn fmt_elapsed(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    match (h, m) {
        (0, 0) => format!("{s}s"),
        (0, _) => format!("{m}m {s}s"),
        _ => format!("{h}h {m}m {s}s"),
    }
}

/// Whether the work log of the node shown is displayed: as toggled, otherwise only
/// until the node is done and has a reply.
pub(crate) fn work_log_shown(
    tree: &crate::conversation::ConversationTree,
    discussion: &crate::tui::state::DiscussionState,
) -> bool {
    use crate::conversation::NodeStatus;
    discussion.show_work_log.unwrap_or_else(|| {
        let done = crate::tui::panes::discussion::launch::shown_meta(tree, discussion)
            .is_some_and(|meta| meta.status == Some(NodeStatus::Done));
        !done || discussion.reply.trim().is_empty()
    })
}

/// Hint of the input of a window whose node's turn stopped: sending a message there
/// resumes the turn, the way `/continue` does.
pub(crate) fn resume_hint(
    state: &crate::tui::state::AppState,
    discussion: &crate::tui::state::DiscussionState,
) -> Option<String> {
    if discussion.agent.is_some() || !discussion.draft.text().trim().is_empty() {
        return None;
    }
    crate::tui::panes::discussion::launch::resumes_on_message(state, discussion)
        .then(|| "sending a message resumes the interrupted turn".to_string())
}

/// Status line of window `id`: a refusal, the progress of a running turn, the place of
/// its prompt in the queue, or the outcome of the node shown. The flag asks for the
/// error color.
pub(crate) fn status_line(
    state: &crate::tui::state::AppState,
    id: usize,
    discussion: &crate::tui::state::DiscussionState,
) -> (String, bool) {
    if let Some(notice) = &discussion.notice {
        return (notice.clone(), true);
    }
    if !discussion.loaded {
        return ("Loading…".to_string(), false);
    }
    if discussion.agent.is_some()
        && let Some(running) =
            crate::tui::panes::discussion::launch::running_agent(state, discussion)
    {
        return (
            progress_text(&running.live.lock(), running.started, state.spinner_tick),
            false,
        );
    }
    let queued = crate::tui::panes::discussion::queue::queue_hint(state, id);
    let Some(node) = &discussion.node else {
        let help = "Enter:Send  Shift+Enter:New line";
        return (queued.unwrap_or_else(|| help.to_string()), false);
    };
    if discussion.agent.is_none()
        && let Some(handle) = state.running_turns.get(node)
    {
        let mut text = progress_text(&handle.live.lock(), handle.started, state.spinner_tick);
        text.push_str(" · ");
        text.push_str(queued.as_deref().unwrap_or("Esc:Cancel"));
        let hiding_dropped = discussion.shown_work_log().len() < discussion.work_log.len();
        if hiding_dropped {
            text.push_str(" · Ctrl+O:Full log");
        }
        return (text, false);
    }
    if let Some(queued) = queued {
        return (queued, false);
    }
    let (mut text, failed) =
        crate::tui::panes::discussion::launch::shown_meta(&state.conversations.read(), discussion)
            .map(status_text)
            .unwrap_or_default();
    if !discussion.work_log.is_empty() {
        if !text.is_empty() {
            text.push_str(" · ");
        }
        text.push_str("Ctrl+O:Work log");
    }
    (text, failed)
}

/// The progress of a turn or a sub-agent run started at `started`: spinner, time,
/// activity and context size.
fn progress_text(
    live: &crate::agent::turn::LiveTurn,
    started: Instant,
    spinner_tick: u8,
) -> String {
    let elapsed = started.elapsed();
    let spinner = crate::agent::turn::spinner_char(spinner_tick);
    let time = fmt_elapsed(elapsed.as_secs());
    let mut text = match (&live.activity, live.reply.is_empty()) {
        (Some(activity), _) => format!("{spinner} running {time} · {activity}"),
        (None, true) => format!("{spinner} thinking {time}"),
        (None, false) => format!("{spinner} running {time}"),
    };
    if let Some(tokens) = live.tokens_in {
        text.push_str(&format!(" · {}", fmt_tokens(tokens)));
    }
    if let Some(cached) = live.cached_in {
        text.push_str(&format!(" · {} cached", fmt_tokens(cached)));
    }
    text
}

pub(crate) fn model_picker_chord(keymap: &crate::tui::keymap::Keymap) -> String {
    use crate::tui::keymap::Action;
    let key = keymap
        .leader_key_for(Action::OpenModelPicker)
        .map(|key| crate::tui::keymap::hint(&key))
        .unwrap_or_default();
    format!("{} {key}", crate::tui::keymap::hint(&keymap.leader_key))
}
