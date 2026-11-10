#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::borrow::Cow;
pub(crate) const SCROLL_STEP: usize = 3;

pub(crate) struct DocLine {
    /// Text the line shows a piece of; `None` for the blank line between two of them.
    pub(crate) part: Option<crate::tui::pane_manager::DocPart>,
    /// Each span's `src` is a byte offset in the text of `part`.
    pub(crate) spans: Vec<crate::tui::widgets::markdown::Span>,
    /// Set on code block lines, painted instead of the surrounding background.
    pub(crate) bg: Option<RgbValue>,
}

/// Wrapped prompt, thinking, work log and reply of one window, reused while the texts and view hold.
pub(crate) struct DocCache {
    window_id: usize,
    width: usize,
    prompt_len: usize,
    reply_len: usize,
    work_log_len: usize,
    log_start: usize,
    thinking_len: usize,
    show_work_log: Option<bool>,
    work_log_shown: bool,
    pub(crate) lines: Vec<DocLine>,
}

impl DocCache {
    /// Whether the lines were laid out from the texts and view settings `discussion` has now.
    pub(crate) fn shows(
        &self,
        window_id: usize,
        discussion: &crate::tui::state::DiscussionState,
    ) -> bool {
        self.window_id == window_id
            && self.prompt_len == discussion.prompt.len()
            && self.reply_len == discussion.reply.len()
            && self.work_log_len == discussion.work_log.len()
            && self.log_start == discussion.log_start
            && self.show_work_log == discussion.show_work_log
            && self.thinking_len == discussion.thinking.len()
    }
}

/// The text of `part`, as the offsets of a [`DocLine`] index it. The work log entries
/// are joined by a newline.
pub(crate) fn part_text(
    discussion: &crate::tui::state::DiscussionState,
    part: crate::tui::pane_manager::DocPart,
) -> Cow<'_, str> {
    use crate::tui::pane_manager::DocPart;
    match part {
        DocPart::Prompt => Cow::Borrowed(&discussion.prompt),
        DocPart::WorkLog => Cow::Owned(discussion.shown_work_log().join("\n")),
        DocPart::Thinking => Cow::Borrowed(&discussion.thinking),
        DocPart::Reply => Cow::Borrowed(&discussion.reply),
    }
}

/// Points the view of window `id` at a document that starts now: at its top, with no
/// height of its own yet, so the render reads a running turn as one to follow.
pub(crate) fn start_view(state: &mut crate::tui::state::AppState, id: usize) {
    use crate::tui::pane_manager::Window;
    let window = Window::Discussion(id);
    state.pane_manager.set_window_scroll(&window, 0);
    state.pane_manager.set_window_scroll_max(&window, 0);
}

pub(crate) fn doc_lines<'a>(
    component: &'a mut crate::tui::panes::discussion::DiscussionComponent,
    discussion: &crate::tui::state::DiscussionState,
    work_log_shown: bool,
    theme: &crate::tui::theme::HelixTheme,
    width: usize,
) -> &'a [DocLine] {
    use crate::tui::pane_manager::DocPart;
    use crate::tui::widgets::markdown::Span;
    let fresh = component.cache.as_ref().is_some_and(|cache| {
        cache.window_id == component.window_id
            && cache.width == width
            && cache.prompt_len == discussion.prompt.len()
            && cache.reply_len == discussion.reply.len()
            && cache.work_log_len == discussion.work_log.len()
            && cache.log_start == discussion.log_start
            && cache.show_work_log == discussion.show_work_log
            && cache.thinking_len == discussion.thinking.len()
            && cache.work_log_shown == work_log_shown
    });
    if !fresh {
        let text_fg = theme
            .ui_fg("ui.text")
            .unwrap_or(RgbValue::from_u8(170, 170, 200));
        let dim_fg = theme.color_for_scope("comment").unwrap_or(text_fg);
        let mut lines = Vec::new();
        if discussion.node.is_some() {
            for (start, text) in crate::tui::widgets::text_layout::wrap(&discussion.prompt, width) {
                lines.push(DocLine {
                    part: Some(DocPart::Prompt),
                    spans: vec![Span {
                        src: Some(start),
                        ..crate::tui::widgets::markdown::Span::plain(text, text_fg)
                    }],
                    bg: None,
                });
            }
            lines.push(DocLine {
                part: None,
                spans: Vec::new(),
                bg: None,
            });
            if !discussion.thinking.is_empty() {
                for (start, text) in
                    crate::tui::widgets::text_layout::wrap(&discussion.thinking, width)
                {
                    lines.push(DocLine {
                        part: Some(DocPart::Thinking),
                        spans: vec![Span {
                            src: Some(start),
                            ..crate::tui::widgets::markdown::Span::plain(text, dim_fg)
                        }],
                        bg: None,
                    });
                }
                lines.push(DocLine {
                    part: None,
                    spans: Vec::new(),
                    bg: None,
                });
            }
            let work_log_to_show = discussion.shown_work_log();
            if work_log_shown && !work_log_to_show.is_empty() {
                let mut base = 0;
                for entry in work_log_to_show {
                    for (start, text) in crate::tui::widgets::text_layout::wrap(entry, width) {
                        lines.push(DocLine {
                            part: Some(DocPart::WorkLog),
                            spans: vec![Span {
                                src: Some(base + start),
                                ..crate::tui::widgets::markdown::Span::plain(text, dim_fg)
                            }],
                            bg: None,
                        });
                    }
                    base += entry.len() + 1;
                }
                lines.push(DocLine {
                    part: None,
                    spans: Vec::new(),
                    bg: None,
                });
            }
            for md in crate::tui::widgets::markdown::render(&discussion.reply, theme, width) {
                lines.push(DocLine {
                    part: Some(DocPart::Reply),
                    spans: md.spans,
                    bg: md.bg,
                });
            }
        }
        component.cache = Some(DocCache {
            window_id: component.window_id,
            width,
            prompt_len: discussion.prompt.len(),
            reply_len: discussion.reply.len(),
            work_log_len: discussion.work_log.len(),
            log_start: discussion.log_start,
            show_work_log: discussion.show_work_log,
            thinking_len: discussion.thinking.len(),
            work_log_shown,
            lines,
        });
    }
    component.cache.as_ref().map_or(&[], |cache| &cache.lines)
}
