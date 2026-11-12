#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::borrow::Cow;
pub(crate) const SCROLL_STEP: usize = 3;
/// The texts a window shows, in the order it shows them.
const SECTIONS: usize = 4;

pub(crate) struct DocLine {
    /// Text the line shows a piece of; `None` for the blank line between two of them.
    pub(crate) part: Option<crate::tui::pane_manager::DocPart>,
    /// Each span's `src` is a byte offset in the text of `part`.
    pub(crate) spans: Vec<crate::tui::widgets::markdown::Span>,
    /// Set on code block lines, painted instead of the surrounding background.
    pub(crate) bg: Option<RgbValue>,
}

/// The document of one window: the wrapped lines of each text, in the order the window
/// shows them. Each text is laid out on its own, so a running turn re-wraps what it
/// changes and leaves the rest of the document alone.
#[derive(Default)]
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
/// texts takes the document as it is.
#[derive(Default)]
pub(crate) struct DocCache {
    window_id: usize,
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
    rebuilds: usize,
    pub(crate) doc: Doc,
}

impl DocCache {
    /// Whether the document was laid out from the texts and view settings `discussion`
    /// has now.
    pub(crate) fn shows(
        &self,
        window_id: usize,
        discussion: &crate::tui::state::DiscussionState,
    ) -> bool {
        self.window_id == window_id
            && self.content_revision == discussion.content_revision
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

/// Wraps `text` into the lines of `part`, whose source offsets start at `base`.
fn push_wrapped(
    lines: &mut Vec<DocLine>,
    text: &str,
    base: usize,
    width: usize,
    part: crate::tui::pane_manager::DocPart,
    fg: RgbValue,
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

pub(crate) fn doc_lines<'a>(
    component: &'a mut crate::tui::panes::discussion::DiscussionComponent,
    discussion: &crate::tui::state::DiscussionState,
    window_id: usize,
    work_log_shown: bool,
    theme: &crate::tui::theme::HelixTheme,
    width: usize,
) -> &'a Doc {
    use crate::tui::pane_manager::DocPart;
    let mut cache = component.cache.take().unwrap_or_default();
    let text_fg = theme
        .ui_fg("ui.text")
        .unwrap_or(RgbValue::from_u8(170, 170, 200));
    let dim_fg = theme.color_for_scope("comment").unwrap_or(text_fg);
    let shown = discussion.node.is_some();
    let width_changed = cache.width != width;
    // A text replaced as a whole may have the length of what it replaced: only the
    // revision tells them apart, and every section is laid out again for it. A new theme
    // is the same kind of change, since every span carries the color it was given.
    let replaced = cache.content_revision != discussion.content_revision
        || cache.window_id != window_id
        || cache.theme != theme.name();

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

    cache.window_id = window_id;
    cache.width = width;
    if cache.theme != theme.name() {
        cache.theme.clear();
        cache.theme.push_str(theme.name());
    }
    cache.content_revision = discussion.content_revision;
    cache.doc.refresh();
    component.cache = Some(cache);
    &component.cache.as_ref().unwrap().doc
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

    fn component() -> crate::tui::panes::discussion::DiscussionComponent {
        crate::tui::panes::discussion::DiscussionComponent::new(
            crate::tui::pane_component::pane_id(0),
        )
    }

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
    fn layout(
        component: &mut crate::tui::panes::discussion::DiscussionComponent,
        discussion: &DiscussionState,
        width: usize,
    ) -> (usize, usize) {
        let theme = crate::tui::theme::HelixTheme::default();
        let rows = doc_lines(component, discussion, 0, true, &theme, width).len();
        let laid_out = component.cache.as_ref().unwrap().rebuilds;
        (rows, laid_out)
    }

    #[test]
    fn a_frame_with_the_same_texts_lays_nothing_out_again() {
        let mut component = component();
        let discussion = discussion("do it", "done");
        layout(&mut component, &discussion, 40);
        let (_, laid_out) = layout(&mut component, &discussion, 40);
        assert_eq!(laid_out, 4, "one per text, on the first frame only");
    }

    #[test]
    fn each_text_is_laid_out_again_only_when_it_changes() {
        let mut component = component();
        let mut discussion = discussion("do it", "");
        discussion.work_log = vec!["one".to_string()];
        layout(&mut component, &discussion, 40);
        assert_eq!(component.cache.as_ref().unwrap().rebuilds, 4);

        // A work-log entry appends its lines to the ones already laid out.
        discussion.work_log.push("two".to_string());
        layout(&mut component, &discussion, 40);
        assert_eq!(
            component.cache.as_ref().unwrap().rebuilds,
            4,
            "the work log alone"
        );

        discussion.reply.push_str("the answer\n\nin two paragraphs");
        layout(&mut component, &discussion, 40);
        assert_eq!(
            component.cache.as_ref().unwrap().rebuilds,
            5,
            "the reply alone"
        );

        layout(&mut component, &discussion, 20);
        assert_eq!(
            component.cache.as_ref().unwrap().rebuilds,
            9,
            "a new width lays every text out again"
        );
    }

    #[test]
    fn a_text_replaced_with_one_of_the_same_length_is_laid_out_again() {
        let mut component = component();
        let mut discussion = discussion("do it", "first");
        layout(&mut component, &discussion, 40);

        discussion.show_content("do it".to_string(), "other".to_string(), Vec::new());
        layout(&mut component, &discussion, 40);
        assert_eq!(component.cache.as_ref().unwrap().rebuilds, 8);
        use crate::tui::pane_manager::DocPart;
        let doc = &component.cache.as_ref().unwrap().doc;
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
        let mut component = component();
        let mut discussion = discussion("do it", "");
        discussion.work_log = vec!["one".to_string(), "two".to_string()];
        let (_, _) = layout(&mut component, &discussion, 40);
        use crate::tui::pane_manager::DocPart;
        let doc = &component.cache.as_ref().unwrap().doc;
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
            mocha.ui_fg("ui.text"),
            light.ui_fg("ui.text"),
            "the two themes paint body text differently"
        );
        let colors = |component: &crate::tui::panes::discussion::DiscussionComponent| {
            let doc = &component.cache.as_ref().unwrap().doc;
            (0..doc.len())
                .filter_map(|row| doc.line(row))
                .flat_map(|line| line.spans.iter().map(|span| span.fg))
                .collect::<Vec<_>>()
        };

        let mut component = component();
        let discussion = discussion("do it", "**done**");
        doc_lines(&mut component, &discussion, 0, true, &mocha, 40);
        let before = colors(&component);

        doc_lines(&mut component, &discussion, 0, true, &light, 40);
        assert_eq!(component.cache.as_ref().unwrap().rebuilds, 8);
        assert_ne!(
            before,
            colors(&component),
            "a new theme repaints the document it was laid out from"
        );

        // And the same theme again is not a change.
        doc_lines(&mut component, &discussion, 0, true, &light, 40);
        assert_eq!(component.cache.as_ref().unwrap().rebuilds, 8);
    }

    #[test]
    fn a_row_past_the_end_of_a_document_has_no_line() {
        let mut component = component();
        let discussion = discussion("do it", "");
        let (rows, _) = layout(&mut component, &discussion, 40);
        let doc = &component.cache.as_ref().unwrap().doc;
        assert!(doc.line(rows).is_none());
        assert!(doc.line(0).is_some());
    }
}
