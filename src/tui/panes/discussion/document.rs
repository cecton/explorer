use std::borrow::Cow;

/// The texts a window shows, in the order it shows them.
const SECTIONS: usize = 4;

#[derive(Clone)]
pub(crate) struct DocLine {
    /// Text the line shows a piece of; `None` for the blank line between two of them.
    pub(crate) part: Option<crate::tui::selection::DocPart>,
    /// Each span's `src` is a byte offset in the text of `part`.
    pub(crate) spans: Vec<crate::tui::widgets::markdown::Span>,
    /// Set on code block lines, painted instead of the surrounding background.
    pub(crate) bg: Option<crate::tui::theme::Color>,
}

/// The document of one window: the wrapped lines of each text, in the order the window
/// shows them. Each text is laid out on its own, so a running turn re-wraps what it
/// changes and leaves the rest of the document alone.
#[derive(Clone, Default)]
pub(crate) struct Doc {
    prompt: Vec<DocLine>,
    thinking: Vec<DocLine>,
    work_log: Vec<DocLine>,
    reply: Vec<DocLine>,
    /// Row the lines of each section start at.
    starts: [usize; SECTIONS],
    rows: usize,
}

impl Doc {
    pub(crate) fn len(&self) -> usize {
        self.rows
    }

    /// The line at `row` of the document.
    pub(crate) fn line(&self, row: usize) -> Option<&DocLine> {
        // Sections are cumulative, so the one holding `row` is the last that starts at
        // or before it; an empty section holds nothing and the search goes on.
        self.starts
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, start)| **start <= row)
            .find_map(|(index, start)| self.section(index).get(row - start))
    }

    /// The last line before `row` that shows text.
    pub(crate) fn text_before(&self, row: usize) -> Option<&DocLine> {
        (0..row.min(self.rows))
            .rev()
            .find_map(|row| self.line(row).filter(|line| !line.spans.is_empty()))
    }

    /// The first line after `row` that shows text.
    pub(crate) fn text_after(&self, row: usize) -> Option<&DocLine> {
        (row + 1..self.rows).find_map(|row| self.line(row).filter(|line| !line.spans.is_empty()))
    }

    fn section(&self, index: usize) -> &[DocLine] {
        match index {
            0 => &self.prompt,
            1 => &self.thinking,
            2 => &self.work_log,
            _ => &self.reply,
        }
    }

    fn refresh(&mut self) {
        let lens = [
            self.prompt.len(),
            self.thinking.len(),
            self.work_log.len(),
            self.reply.len(),
        ];
        let mut at = 0;
        for (index, len) in lens.into_iter().enumerate() {
            self.starts[index] = at;
            at += len;
        }
        self.rows = at;
    }
}

/// The lines a window was laid out from, so that a frame at the same width with the same
/// texts takes the document as it is. It lives in the window's `DiscussionState`, so a
/// window that changes pane slot keeps its document.
#[derive(Clone, Default)]
pub(crate) struct DocCache {
    width: usize,
    /// Name of the theme the lines were laid out with: every span carries the color it
    /// was rendered with, so a new theme is a new document.
    theme: String,
    content_revision: u64,
    node_shown: bool,
    prompt_len: usize,
    thinking_len: usize,
    reply_len: usize,
    work_log_len: usize,
    log_start: usize,
    show_work_log: Option<bool>,
    work_log_shown: bool,
    /// Work-log entries laid out, and the source offset the next one starts at.
    work_log_entries: usize,
    work_log_base: usize,
    /// How many texts were laid out again, for the tests that pin the caching.
    #[cfg(test)]
    pub(crate) rebuilds: usize,
    pub(crate) doc: Doc,
}

impl DocCache {
    /// Whether the document was laid out from the texts and view settings `discussion`
    /// has now.
    pub(crate) fn shows(&self, discussion: &crate::tui::state::DiscussionState) -> bool {
        self.content_revision == discussion.content_revision
            && self.node_shown == discussion.node.is_some()
            && self.prompt_len == discussion.prompt.len()
            && self.reply_len == discussion.reply.len()
            && self.thinking_len == discussion.thinking.len()
            && self.work_log_len == discussion.work_log.len()
            && self.log_start == discussion.log_start
            && self.show_work_log == discussion.show_work_log
    }
}

/// The text of `part`, as the offsets of a [`DocLine`] index it. The work log entries
/// are joined by a newline.
pub(crate) fn part_text(
    discussion: &crate::tui::state::DiscussionState,
    part: crate::tui::selection::DocPart,
) -> Cow<'_, str> {
    use crate::tui::selection::DocPart;
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

/// Wraps `text` into the lines of `part`, whose source offsets start at `base`.
fn push_wrapped(
    lines: &mut Vec<DocLine>,
    text: &str,
    base: usize,
    width: usize,
    part: crate::tui::selection::DocPart,
    fg: crate::tui::theme::Color,
) {
    use crate::tui::widgets::markdown::Span;
    for (start, text) in crate::tui::widgets::text_layout::wrap(text, width) {
        lines.push(DocLine {
            part: Some(part),
            spans: vec![Span {
                src: Some(base + start),
                ..Span::plain(text, fg)
            }],
            bg: None,
        });
    }
}

fn blank_line() -> DocLine {
    DocLine {
        part: None,
        spans: Vec::new(),
        bg: None,
    }
}

fn ends_with_blank(lines: &[DocLine]) -> bool {
    lines.last().is_some_and(|line| line.part.is_none())
}

/// The document of `discussion` at `width`, laid out again only where its texts, the width
/// or the theme changed since the last frame.
pub(crate) fn doc_lines<'a>(
    discussion: &'a mut crate::tui::state::DiscussionState,
    work_log_shown: bool,
    theme: &crate::tui::theme::HelixTheme,
    width: usize,
) -> &'a Doc {
    // Taken out for the layout, which reads the texts of the same state.
    let mut cache = std::mem::take(&mut discussion.doc_cache);
    lay_out(&mut cache, discussion, work_log_shown, theme, width);
    discussion.doc_cache = cache;
    &discussion.doc_cache.doc
}

fn lay_out(
    cache: &mut DocCache,
    discussion: &crate::tui::state::DiscussionState,
    work_log_shown: bool,
    theme: &crate::tui::theme::HelixTheme,
    width: usize,
) {
    use crate::tui::selection::DocPart;
    let text_fg = theme.ui.text;
    let dim_fg = theme.ui.dim;
    let shown = discussion.node.is_some();
    let width_changed = cache.width != width;
    // A text replaced as a whole may have the length of what it replaced: only the
    // revision tells them apart, and every section is laid out again for it. A new theme
    // is the same kind of change, since every span carries the color it was given.
    let replaced =
        cache.content_revision != discussion.content_revision || cache.theme != theme.name();

    if width_changed
        || replaced
        || cache.node_shown != shown
        || cache.prompt_len != discussion.prompt.len()
    {
        cache.doc.prompt.clear();
        if shown {
            push_wrapped(
                &mut cache.doc.prompt,
                &discussion.prompt,
                0,
                width,
                DocPart::Prompt,
                text_fg,
            );
            cache.doc.prompt.push(blank_line());
        }
        cache.node_shown = shown;
        cache.prompt_len = discussion.prompt.len();
        #[cfg(test)]
        {
            cache.rebuilds += 1;
        }
    }

    if width_changed || replaced || cache.thinking_len != discussion.thinking.len() {
        cache.doc.thinking.clear();
        if shown && !discussion.thinking.is_empty() {
            push_wrapped(
                &mut cache.doc.thinking,
                &discussion.thinking,
                0,
                width,
                DocPart::Thinking,
                dim_fg,
            );
            cache.doc.thinking.push(blank_line());
        }
        cache.thinking_len = discussion.thinking.len();
        #[cfg(test)]
        {
            cache.rebuilds += 1;
        }
    }

    if width_changed
        || replaced
        || cache.log_start != discussion.log_start
        || cache.show_work_log != discussion.show_work_log
        || cache.work_log_shown != work_log_shown
        || cache.work_log_entries > discussion.shown_work_log().len()
    {
        cache.doc.work_log.clear();
        cache.work_log_entries = 0;
        cache.work_log_base = 0;
        cache.log_start = discussion.log_start;
        cache.show_work_log = discussion.show_work_log;
        cache.work_log_shown = work_log_shown;
        #[cfg(test)]
        {
            cache.rebuilds += 1;
        }
    }
    let entries = discussion.shown_work_log();
    if work_log_shown && !entries.is_empty() {
        // The entries are append-only, so only those not laid out yet are wrapped.
        if ends_with_blank(&cache.doc.work_log) {
            cache.doc.work_log.pop();
        }
        for entry in &entries[cache.work_log_entries..] {
            push_wrapped(
                &mut cache.doc.work_log,
                entry,
                cache.work_log_base,
                width,
                DocPart::WorkLog,
                dim_fg,
            );
            cache.work_log_base += entry.len() + 1;
        }
        cache.work_log_entries = entries.len();
        cache.doc.work_log.push(blank_line());
    }
    cache.work_log_len = discussion.work_log.len();

    if width_changed || replaced || cache.reply_len != discussion.reply.len() {
        cache.doc.reply = crate::tui::widgets::markdown::render(&discussion.reply, theme, width)
            .into_iter()
            .map(|line| DocLine {
                part: Some(DocPart::Reply),
                spans: line.spans,
                bg: line.bg,
            })
            .collect();
        cache.reply_len = discussion.reply.len();
        #[cfg(test)]
        {
            cache.rebuilds += 1;
        }
    }

    cache.width = width;
    if cache.theme != theme.name() {
        cache.theme.clear();
        cache.theme.push_str(theme.name());
    }
    cache.content_revision = discussion.content_revision;
    cache.doc.refresh();
}

#[cfg(test)]
impl Doc {
    /// A document of a prompt and a reply, for the selection tests.
    pub(crate) fn of(prompt: Vec<DocLine>, reply: Vec<DocLine>) -> Self {
        let mut doc = Self {
            prompt,
            reply,
            ..Self::default()
        };
        doc.refresh();
        doc
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::DiscussionState;

    fn discussion(prompt: &str, reply: &str) -> DiscussionState {
        DiscussionState {
            node: Some(crate::conversation::NodeId::new("2026-11-11T10-00-00Z")),
            prompt: prompt.to_string(),
            reply: reply.to_string(),
            show_work_log: Some(true),
            ..Default::default()
        }
    }

    /// The document of `discussion` at `width`, and how many texts this laid out again.
    fn layout(discussion: &mut DiscussionState, width: usize) -> (usize, usize) {
        let theme = crate::tui::theme::HelixTheme::bundled_default();
        let rows = doc_lines(discussion, true, &theme, width).len();
        (rows, discussion.doc_cache.rebuilds)
    }

    #[test]
    fn a_frame_with_the_same_texts_lays_nothing_out_again() {
        let mut discussion = discussion("do it", "done");
        layout(&mut discussion, 40);
        let (_, laid_out) = layout(&mut discussion, 40);
        assert_eq!(laid_out, 4, "one per text, on the first frame only");
    }

    #[test]
    fn each_text_is_laid_out_again_only_when_it_changes() {
        let mut discussion = discussion("do it", "");
        discussion.work_log = vec!["one".to_string()];
        layout(&mut discussion, 40);
        assert_eq!(discussion.doc_cache.rebuilds, 4);

        // A work-log entry appends its lines to the ones already laid out.
        discussion.work_log.push("two".to_string());
        layout(&mut discussion, 40);
        assert_eq!(discussion.doc_cache.rebuilds, 4, "the work log alone");

        discussion.reply.push_str("the answer\n\nin two paragraphs");
        layout(&mut discussion, 40);
        assert_eq!(discussion.doc_cache.rebuilds, 5, "the reply alone");

        layout(&mut discussion, 20);
        assert_eq!(
            discussion.doc_cache.rebuilds, 9,
            "a new width lays every text out again"
        );
    }

    #[test]
    fn a_text_replaced_with_one_of_the_same_length_is_laid_out_again() {
        let mut discussion = discussion("do it", "first");
        layout(&mut discussion, 40);

        discussion.show_content("do it".to_string(), "other".to_string(), Vec::new());
        layout(&mut discussion, 40);
        assert_eq!(discussion.doc_cache.rebuilds, 8);
        use crate::tui::selection::DocPart;
        let doc = &discussion.doc_cache.doc;
        let reply = (0..doc.len())
            .filter_map(|row| doc.line(row))
            .filter(|line| line.part == Some(DocPart::Reply))
            .flat_map(|line| line.spans.iter())
            .map(|span| span.text.as_str())
            .collect::<String>();
        assert_eq!(reply, "other");
    }

    #[test]
    fn the_work_log_lines_carry_the_offsets_of_the_joined_entries() {
        let mut discussion = discussion("do it", "");
        discussion.work_log = vec!["one".to_string(), "two".to_string()];
        let (_, _) = layout(&mut discussion, 40);
        use crate::tui::selection::DocPart;
        let doc = &discussion.doc_cache.doc;
        let offsets: Vec<usize> = (0..doc.len())
            .filter_map(|row| doc.line(row))
            .filter(|line| line.part == Some(DocPart::WorkLog))
            .filter_map(|line| line.spans.first())
            .filter_map(|span| span.src)
            .collect();
        // `part_text` joins the entries with a newline: "one\ntwo".
        assert_eq!(offsets, [0, 4]);
    }

    #[test]
    fn a_new_theme_lays_the_document_out_again() {
        use crate::tui::theme::HelixTheme;
        let mocha = HelixTheme::from_name("catppuccin_mocha").expect("bundled theme");
        let light = HelixTheme::from_name("adwaita-light").expect("bundled theme");
        assert_ne!(
            mocha.ui.text, light.ui.text,
            "the two themes paint body text differently"
        );
        let colors = |discussion: &DiscussionState| {
            let doc = &discussion.doc_cache.doc;
            (0..doc.len())
                .filter_map(|row| doc.line(row))
                .flat_map(|line| line.spans.iter().map(|span| span.style.fg))
                .collect::<Vec<_>>()
        };

        let mut discussion = discussion("do it", "**done**");
        doc_lines(&mut discussion, true, &mocha, 40);
        let before = colors(&discussion);

        doc_lines(&mut discussion, true, &light, 40);
        assert_eq!(discussion.doc_cache.rebuilds, 8);
        assert_ne!(
            before,
            colors(&discussion),
            "a new theme repaints the document it was laid out from"
        );

        // And the same theme again is not a change.
        doc_lines(&mut discussion, true, &light, 40);
        assert_eq!(discussion.doc_cache.rebuilds, 8);
    }

    #[test]
    fn a_row_past_the_end_of_a_document_has_no_line() {
        let mut discussion = discussion("do it", "");
        let (rows, _) = layout(&mut discussion, 40);
        let doc = &discussion.doc_cache.doc;
        assert!(doc.line(rows).is_none());
        assert!(doc.line(0).is_some());
    }
}
