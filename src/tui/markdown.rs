use crate::tui::text_layout::{display_width, truncate};
use crate::tui::{HelixTheme, RgbValue};
use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use unicode_segmentation::UnicodeSegmentation;

/// One styled run of text on a rendered line.
#[derive(Clone)]
pub struct Span {
    pub text: String,
    pub fg: RgbValue,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
}

impl Span {
    pub(crate) fn plain(text: impl Into<String>, fg: RgbValue) -> Self {
        Self {
            text: text.into(),
            fg,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
        }
    }

    fn of(text: impl Into<String>, style: InlineStyle) -> Self {
        Self {
            text: text.into(),
            fg: style.fg,
            bold: style.bold,
            italic: style.italic,
            underline: style.underline,
            strikethrough: style.strikethrough,
        }
    }

    fn same_style(&self, style: InlineStyle) -> bool {
        self.fg == style.fg
            && self.bold == style.bold
            && self.italic == style.italic
            && self.underline == style.underline
            && self.strikethrough == style.strikethrough
    }
}

/// One rendered row. `bg` set marks a code block line, painted with that background
/// instead of the pane background.
pub struct MdLine {
    pub spans: Vec<Span>,
    pub bg: Option<RgbValue>,
}

impl MdLine {
    fn is_blank(&self) -> bool {
        self.spans.is_empty() && self.bg.is_none()
    }
}

#[derive(Clone, Copy)]
struct InlineStyle {
    fg: RgbValue,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
}

impl InlineStyle {
    fn of(span: &Span) -> Self {
        Self {
            fg: span.fg,
            bold: span.bold,
            italic: span.italic,
            underline: span.underline,
            strikethrough: span.strikethrough,
        }
    }
}

fn push_run(line: &mut Vec<Span>, text: &str, style: InlineStyle) {
    match line.last_mut() {
        Some(last) if last.same_style(style) => last.text.push_str(text),
        _ => line.push(Span::of(text, style)),
    }
}

struct ItemCtx {
    marker: Vec<Span>,
    marker_width: usize,
    flushed: bool,
}

/// Longest word a table column refuses to split when columns shrink to fit.
const MAX_WORD_FLOOR: usize = 12;

struct TableCtx {
    alignments: Vec<Alignment>,
    rows: Vec<Vec<Vec<Span>>>,
    row: Vec<Vec<Span>>,
}

struct Renderer<'a> {
    theme: &'a HelixTheme,
    width: usize,
    text_fg: RgbValue,
    plain: InlineStyle,
    style: InlineStyle,
    stack: Vec<InlineStyle>,
    lines: Vec<MdLine>,
    content: Vec<Span>,
    quote_depth: usize,
    quote_fg: RgbValue,
    items: Vec<ItemCtx>,
    lists: Vec<Option<u64>>,
    code_bg: Option<RgbValue>,
    table: Option<TableCtx>,
}

/// Renders markdown to styled lines fitted to `width`, colored from the theme's
/// `markup.*` scopes. Attributes (bold, italic, …) come from the markdown element;
/// colors come from the theme.
pub(crate) fn render(source: &str, theme: &HelixTheme, width: usize) -> Vec<MdLine> {
    let mut r = Renderer::new(theme, width);
    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_TABLES;
    for event in Parser::new_ext(source, options) {
        match event {
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                r.flush_content();
                r.push_blank();
            }
            Event::Start(Tag::Heading { level, .. }) => r.start_heading(level),
            Event::End(TagEnd::Heading(_)) => {
                r.end_style();
                r.flush_content();
                r.push_blank();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                r.start_style(InlineStyle {
                    fg: r.fg("markup.quote"),
                    ..r.plain
                });
                r.quote_fg = r.fg("markup.quote");
                r.quote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                r.quote_depth -= 1;
                r.end_style();
                r.push_blank();
            }
            Event::Start(Tag::CodeBlock(_)) => {
                r.flush_content();
                r.code_bg = r.theme.ui_bg("ui.virtual.ruler");
            }
            Event::End(TagEnd::CodeBlock) => {
                r.code_bg = None;
                r.push_blank();
            }
            Event::Start(Tag::List(start)) => {
                r.flush_content();
                r.lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                r.lists.pop();
                if r.lists.is_empty() {
                    r.push_blank();
                }
            }
            Event::Start(Tag::Item) => r.start_item(),
            Event::End(TagEnd::Item) => {
                r.flush_content();
                r.items.pop();
            }
            Event::TaskListMarker(checked) => r.task_marker(checked),
            Event::Start(Tag::Table(alignments)) => {
                r.flush_content();
                r.table = Some(TableCtx {
                    alignments,
                    rows: Vec::new(),
                    row: Vec::new(),
                });
            }
            Event::Start(Tag::TableHead) => r.start_style(InlineStyle {
                bold: true,
                ..r.style
            }),
            Event::End(TagEnd::TableHead) => {
                r.end_style();
                r.end_table_row();
            }
            Event::End(TagEnd::TableRow) => r.end_table_row(),
            Event::End(TagEnd::TableCell) => r.end_table_cell(),
            Event::End(TagEnd::Table) => {
                r.flush_table();
                r.push_blank();
            }
            Event::Start(Tag::Emphasis) => {
                let mut style = r.style;
                if let Some(fg) = r.exact_fg("markup.italic") {
                    style.fg = fg;
                }
                style.italic = true;
                r.start_style(style);
            }
            Event::End(TagEnd::Emphasis) => r.end_style(),
            Event::Start(Tag::Strong) => {
                let mut style = r.style;
                if let Some(fg) = r.exact_fg("markup.bold") {
                    style.fg = fg;
                }
                style.bold = true;
                r.start_style(style);
            }
            Event::End(TagEnd::Strong) => r.end_style(),
            Event::Start(Tag::Strikethrough) => {
                let mut style = r.style;
                if let Some(fg) = r.exact_fg("markup.strikethrough") {
                    style.fg = fg;
                }
                style.strikethrough = true;
                r.start_style(style);
            }
            Event::End(TagEnd::Strikethrough) => r.end_style(),
            Event::Start(Tag::Link { .. }) => {
                let mut style = r.style;
                if let Some(fg) = r.exact_fg("markup.link.text") {
                    style.fg = fg;
                }
                style.underline = true;
                r.start_style(style);
            }
            Event::End(TagEnd::Link) => r.end_style(),
            // The alt text of an image renders inline as plain text.
            Event::Start(Tag::Image { .. }) => r.start_style(r.style),
            Event::End(TagEnd::Image) => r.end_style(),
            Event::Text(text) if r.code_bg.is_some() => r.push_code_lines(&text),
            Event::Text(text) => r.push_content(&text, r.style),
            Event::Code(text) => r.push_content(
                &text,
                InlineStyle {
                    fg: r.fg("markup.raw"),
                    ..r.style
                },
            ),
            Event::SoftBreak => r.push_content(" ", r.style),
            Event::HardBreak => r.flush_content(),
            Event::Rule => {
                let style = InlineStyle {
                    fg: r.fg("comment"),
                    ..r.plain
                };
                r.lines.push(MdLine {
                    spans: vec![Span::of("─".repeat(r.width), style)],
                    bg: None,
                });
                r.push_blank();
            }
            Event::InlineHtml(text) if r.table.is_some() => {
                if is_line_break_tag(&text) {
                    r.push_content(" ", r.style);
                } else {
                    r.push_content(&text, r.style);
                }
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                let style = InlineStyle {
                    fg: r.fg("comment"),
                    ..r.plain
                };
                for line in text.lines() {
                    r.lines.push(MdLine {
                        spans: vec![Span::of(line, style)],
                        bg: None,
                    });
                }
            }
            Event::FootnoteReference(name) => {
                let style = InlineStyle {
                    fg: r.fg("comment"),
                    ..r.plain
                };
                r.push_content(&format!("[{name}]"), style);
            }
            _ => {}
        }
    }
    while r.lines.last().is_some_and(MdLine::is_blank) {
        r.lines.pop();
    }
    r.lines
}

impl Renderer<'_> {
    fn new(theme: &HelixTheme, width: usize) -> Renderer<'_> {
        let text_fg = theme
            .ui_fg("ui.text")
            .unwrap_or(RgbValue::from_u8(170, 170, 200));
        let plain = InlineStyle {
            fg: text_fg,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
        };
        Renderer {
            theme,
            width,
            text_fg,
            plain,
            style: plain,
            stack: Vec::new(),
            lines: Vec::new(),
            content: Vec::new(),
            quote_depth: 0,
            quote_fg: text_fg,
            items: Vec::new(),
            lists: Vec::new(),
            code_bg: None,
            table: None,
        }
    }

    fn fg(&self, scope: &str) -> RgbValue {
        self.theme.color_for_scope(scope).unwrap_or(self.text_fg)
    }

    fn exact_fg(&self, scope: &str) -> Option<RgbValue> {
        self.theme.ui_fg(scope)
    }

    fn start_style(&mut self, style: InlineStyle) {
        self.stack.push(self.style);
        self.style = style;
    }

    fn end_style(&mut self) {
        self.style = self.stack.pop().unwrap_or(self.plain);
    }

    fn start_heading(&mut self, level: HeadingLevel) {
        let scope = match level {
            HeadingLevel::H1 => "markup.heading.1",
            HeadingLevel::H2 => "markup.heading.2",
            HeadingLevel::H3 => "markup.heading.3",
            HeadingLevel::H4 => "markup.heading.4",
            HeadingLevel::H5 => "markup.heading.5",
            HeadingLevel::H6 => "markup.heading.6",
        };
        self.start_style(InlineStyle {
            fg: self.fg(scope),
            bold: true,
            ..self.plain
        });
    }

    fn start_item(&mut self) {
        let marker_text = match self.lists.last_mut() {
            Some(Some(next)) => {
                let marker = format!("{next}. ");
                *next += 1;
                marker
            }
            _ => "• ".to_string(),
        };
        let style = InlineStyle {
            fg: self.fg("markup.list"),
            ..self.plain
        };
        let marker_width = display_width(&marker_text);
        self.items.push(ItemCtx {
            marker: vec![Span::of(marker_text, style)],
            marker_width,
            flushed: false,
        });
    }

    fn task_marker(&mut self, checked: bool) {
        let (glyph, scope) = if checked {
            ("☑ ", "markup.list.checked")
        } else {
            ("☐ ", "markup.list.unchecked")
        };
        let style = InlineStyle {
            fg: self
                .exact_fg(scope)
                .unwrap_or_else(|| self.fg("markup.list")),
            ..self.plain
        };
        let Some(item) = self.items.last_mut() else {
            return;
        };
        item.marker[0] = Span::of(glyph, style);
        item.marker_width = display_width(glyph);
    }

    fn push_content(&mut self, text: &str, style: InlineStyle) {
        push_run(&mut self.content, text, style);
    }

    fn push_code_lines(&mut self, text: &str) {
        let Some(bg) = self.code_bg else {
            return;
        };
        let style = InlineStyle {
            fg: self.fg("markup.raw"),
            ..self.plain
        };
        for line in text.lines() {
            self.lines.push(MdLine {
                spans: vec![Span::of(truncate(line, self.width), style)],
                bg: Some(bg),
            });
        }
    }

    fn prefixes(&self) -> (Vec<Span>, Vec<Span>) {
        let quote_style = InlineStyle {
            fg: self.quote_fg,
            ..self.plain
        };
        let mut first = Vec::new();
        let mut cont = Vec::new();
        for _ in 0..self.quote_depth {
            first.push(Span::of("> ", quote_style));
            cont.push(Span::of("> ", quote_style));
        }
        for item in &self.items {
            if item.flushed {
                first.push(Span::of(" ".repeat(item.marker_width), self.plain));
            } else {
                first.extend(item.marker.iter().cloned());
            }
            cont.push(Span::of(" ".repeat(item.marker_width), self.plain));
        }
        (first, cont)
    }

    fn flush_content(&mut self) {
        let item_pending = self.items.last().is_some_and(|item| !item.flushed);
        if self.content.is_empty() && !item_pending {
            return;
        }
        let (first, cont) = self.prefixes();
        for spans in wrap_spans(&self.content, self.width, first, cont) {
            self.lines.push(MdLine { spans, bg: None });
        }
        if let Some(item) = self.items.last_mut() {
            item.flushed = true;
        }
        self.content.clear();
    }

    fn end_table_cell(&mut self) {
        let cell = std::mem::take(&mut self.content);
        if let Some(table) = self.table.as_mut() {
            table.row.push(cell);
        }
    }

    fn end_table_row(&mut self) {
        if let Some(table) = self.table.as_mut() {
            let row = std::mem::take(&mut table.row);
            table.rows.push(row);
        }
    }

    fn flush_table(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        let (first, cont) = self.prefixes();
        let width = self.width.saturating_sub(spans_width(&cont)).max(1);
        let lines = self
            .table_grid(&table, width)
            .unwrap_or_else(|| self.table_stacked(&table, width));
        for (i, spans) in lines.into_iter().enumerate() {
            let mut line = if i == 0 { first.clone() } else { cont.clone() };
            line.extend(spans);
            self.lines.push(MdLine {
                spans: line,
                bg: None,
            });
        }
        if let Some(item) = self.items.last_mut() {
            item.flushed = true;
        }
    }

    /// Lays the table out as aligned columns, or `None` when they cannot fit `width`.
    fn table_grid(&self, table: &TableCtx, width: usize) -> Option<Vec<Vec<Span>>> {
        let columns = table.alignments.len();
        let avail = width.checked_sub(3 * columns.saturating_sub(1))?;
        let mut widths = vec![1usize; columns];
        let mut floors = vec![1usize; columns];
        for row in &table.rows {
            for ((cell, width), floor) in row.iter().zip(&mut widths).zip(&mut floors) {
                let word_widths: Vec<usize> = split_words(cell)
                    .iter()
                    .map(|word| spans_width(word))
                    .collect();
                let natural =
                    word_widths.iter().sum::<usize>() + word_widths.len().saturating_sub(1);
                let longest = word_widths.iter().copied().max().unwrap_or(0);
                *width = (*width).max(natural);
                *floor = (*floor).max(longest.min(MAX_WORD_FLOOR));
            }
        }
        if floors.iter().sum::<usize>() > avail {
            return None;
        }
        while widths.iter().sum::<usize>() > avail {
            let widest = widths
                .iter_mut()
                .zip(&floors)
                .filter(|(width, floor)| **width > **floor)
                .map(|(width, _)| width)
                .max_by_key(|width| **width)?;
            *widest -= 1;
        }

        let rule_style = InlineStyle {
            fg: self.fg("comment"),
            ..self.plain
        };
        let rule = widths
            .iter()
            .map(|width| "─".repeat(*width))
            .collect::<Vec<_>>()
            .join("─┼─");
        let rows: Vec<Vec<_>> = table
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .zip(&widths)
                    .map(|(cell, width)| wrap_spans(cell, *width, Vec::new(), Vec::new()))
                    .collect()
            })
            .collect();
        let body_wraps = rows.iter().skip(1).flatten().any(|cell| cell.len() > 1);
        let mut out = Vec::new();
        for (r, row) in rows.iter().enumerate() {
            if r > 1 && body_wraps {
                out.push(vec![Span::of(rule.clone(), rule_style)]);
            }
            let height = row.iter().map(Vec::len).max().unwrap_or(1);
            for k in 0..height {
                let mut line = Vec::new();
                let cells = row.iter().zip(&widths).zip(&table.alignments);
                for (c, ((cell, width), alignment)) in cells.enumerate() {
                    if c > 0 {
                        line.push(Span::of(" │ ", rule_style));
                    }
                    let spans = cell.get(k).map_or(&[][..], Vec::as_slice);
                    let pad = width.saturating_sub(spans_width(spans));
                    let (left, right) = match alignment {
                        Alignment::Right => (pad, 0),
                        Alignment::Center => (pad / 2, pad - pad / 2),
                        Alignment::Left | Alignment::None => (0, pad),
                    };
                    if left > 0 {
                        line.push(Span::of(" ".repeat(left), self.plain));
                    }
                    line.extend(spans.iter().cloned());
                    if right > 0 && c + 1 < columns {
                        line.push(Span::of(" ".repeat(right), self.plain));
                    }
                }
                out.push(line);
            }
            if r == 0 {
                out.push(vec![Span::of(rule.clone(), rule_style)]);
            }
        }
        Some(out)
    }

    /// Lays the table out as one `Header: value` record per body row.
    fn table_stacked(&self, table: &TableCtx, width: usize) -> Vec<Vec<Span>> {
        let rule_style = InlineStyle {
            fg: self.fg("comment"),
            ..self.plain
        };
        let label_style = InlineStyle {
            bold: true,
            ..self.style
        };
        let Some((head, body)) = table.rows.split_first() else {
            return Vec::new();
        };
        if body.is_empty() {
            let mut content = Vec::new();
            for (c, cell) in head.iter().enumerate() {
                if c > 0 {
                    content.push(Span::of(" │ ", rule_style));
                }
                content.extend(cell.iter().cloned());
            }
            return wrap_spans(&content, width, Vec::new(), Vec::new());
        }
        let mut out = Vec::new();
        for (r, row) in body.iter().enumerate() {
            if r > 0 {
                out.push(vec![Span::of("─".repeat(width), rule_style)]);
            }
            for (label, cell) in head.iter().zip(row) {
                let mut content = label.clone();
                if !label.is_empty() {
                    push_run(&mut content, ": ", label_style);
                }
                content.extend(cell.iter().cloned());
                if content.is_empty() {
                    continue;
                }
                out.extend(wrap_spans(&content, width, Vec::new(), Vec::new()));
            }
        }
        out
    }

    fn push_blank(&mut self) {
        if self.lines.is_empty() || self.lines.last().is_some_and(MdLine::is_blank) {
            return;
        }
        self.lines.push(MdLine {
            spans: Vec::new(),
            bg: None,
        });
    }
}

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| display_width(&s.text)).sum()
}

fn is_line_break_tag(html: &str) -> bool {
    html.trim_start_matches('<')
        .trim_end_matches('>')
        .trim_end_matches('/')
        .trim()
        .eq_ignore_ascii_case("br")
}

/// Word-wraps styled runs to `width` columns, prepending `first_prefix` to the first
/// line and `cont_prefix` (whose width is the hanging indent) to the rest. Words longer
/// than a line are split on grapheme boundaries. Whitespace collapses to single spaces.
fn wrap_spans(
    content: &[Span],
    width: usize,
    first_prefix: Vec<Span>,
    cont_prefix: Vec<Span>,
) -> Vec<Vec<Span>> {
    let cont_width: usize = cont_prefix.iter().map(|s| display_width(&s.text)).sum();
    let eff = width.saturating_sub(cont_width).max(1);
    let words = split_words(content);
    let mut lines: Vec<Vec<Span>> = Vec::new();
    let mut line: Vec<Span> = Vec::new();
    let mut used = 0;
    for word in &words {
        let word_width: usize = word.iter().map(|s| display_width(&s.text)).sum();
        let sep = usize::from(!line.is_empty());
        if used + sep + word_width <= eff {
            if sep == 1 {
                let next_style = InlineStyle::of(&word[0]);
                let merge = line.last().is_some_and(|last| last.same_style(next_style));
                if merge {
                    if let Some(last) = line.last_mut() {
                        last.text.push(' ');
                    }
                } else if let Some(last) = line.last() {
                    let sep_style = InlineStyle::of(last);
                    line.push(Span::of(" ", sep_style));
                }
            }
            for run in word {
                push_run(&mut line, &run.text, InlineStyle::of(run));
            }
            used += sep + word_width;
        } else if word_width > eff {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            for run in word {
                for grapheme in run.text.graphemes(true) {
                    let w = display_width(grapheme);
                    if used + w > eff && used > 0 {
                        lines.push(std::mem::take(&mut line));
                        used = 0;
                    }
                    push_run(&mut line, grapheme, InlineStyle::of(run));
                    used += w;
                }
            }
        } else {
            lines.push(std::mem::take(&mut line));
            for run in word {
                push_run(&mut line, &run.text, InlineStyle::of(run));
            }
            used = word_width;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    let mut out: Vec<Vec<Span>> = Vec::new();
    for (i, spans) in lines.into_iter().enumerate() {
        let mut line = if i == 0 {
            first_prefix.clone()
        } else {
            cont_prefix.clone()
        };
        line.extend(spans);
        out.push(line);
    }
    if out.is_empty() {
        out.push(first_prefix);
    }
    out
}

/// Splits styled runs into words on whitespace, merging runs of the same style.
fn split_words(content: &[Span]) -> Vec<Vec<Span>> {
    let mut words: Vec<Vec<Span>> = Vec::new();
    let mut word: Vec<Span> = Vec::new();
    for span in content {
        for grapheme in span.text.graphemes(true) {
            if grapheme.chars().all(char::is_whitespace) {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            } else {
                push_run(&mut word, grapheme, InlineStyle::of(span));
            }
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> HelixTheme {
        HelixTheme::default()
    }

    fn render_md(src: &str, width: usize) -> Vec<MdLine> {
        render(src, &theme(), width)
    }

    fn text(line: &MdLine) -> String {
        line.spans.iter().map(|s| s.text.as_str()).collect()
    }

    fn texts(src: &str, width: usize) -> Vec<String> {
        render_md(src, width).iter().map(text).collect()
    }

    const TABLE: &str = "\
| Option | Cost | Notes |
|:--|--:|:-:|
| `wrap` cells | 12 | Rows grow **taller**; nothing is lost |
| truncate | 3 | Loses text, stays one row |
";

    #[test]
    fn table_columns_line_up_under_a_ruled_bold_header() {
        let lines = render_md("| a | b |\n|---|---|\n| one | two |\n| three | 4 |", 40);
        let texts: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(
            texts,
            ["a     │ b", "──────┼────", "one   │ two", "three │ 4"]
        );
        assert!(lines[0].spans[0].bold);
        assert!(!lines[2].spans[0].bold);
        assert_eq!(
            lines[1].spans[0].fg,
            theme().color_for_scope("comment").unwrap()
        );
    }

    #[test]
    fn table_cells_follow_the_column_alignment() {
        assert_eq!(
            texts(TABLE, 80),
            [
                "Option     │ Cost │               Notes",
                "───────────┼──────┼──────────────────────────────────",
                "wrap cells │   12 │ Rows grow taller; nothing is lost",
                "truncate   │    3 │     Loses text, stays one row",
            ]
        );
    }

    #[test]
    fn table_cells_wrap_in_their_column_with_rules_between_rows() {
        assert_eq!(
            texts(TABLE, 44),
            [
                "Option     │ Cost │          Notes",
                "───────────┼──────┼─────────────────────────",
                "wrap cells │   12 │    Rows grow taller;",
                "           │      │     nothing is lost",
                "───────────┼──────┼─────────────────────────",
                "truncate   │    3 │  Loses text, stays one",
                "           │      │           row",
            ]
        );
    }

    #[test]
    fn table_too_wide_for_the_pane_stacks_one_record_per_row() {
        assert_eq!(
            texts(TABLE, 24),
            [
                "Option: wrap cells",
                "Cost: 12",
                "Notes: Rows grow taller;",
                "nothing is lost",
                "────────────────────────",
                "Option: truncate",
                "Cost: 3",
                "Notes: Loses text, stays",
                "one row",
            ]
        );
    }

    #[test]
    fn stacked_table_with_only_a_header_shows_its_cells_on_one_line() {
        let src = "| alpha | beta | gamma |\n|---|---|---|";
        assert_eq!(texts(src, 12), ["alpha │ beta", "│ gamma"]);
    }

    #[test]
    fn table_in_a_list_item_is_indented_under_the_marker() {
        let src = "- item:\n\n  | a | b |\n  |---|--:|\n  | 1 | 22 |\n";
        assert_eq!(
            texts(src, 30),
            ["• item:", "", "  a │  b", "  ──┼───", "  1 │ 22"]
        );
    }

    #[test]
    fn line_break_tag_in_a_table_cell_becomes_a_space() {
        let src = "| a | b |\n|---|---|\n| one<br>two | x |";
        assert_eq!(texts(src, 30)[2], "one two │ x");
    }

    #[test]
    fn table_lines_never_exceed_the_width() {
        for width in 1..=100 {
            for line in texts(TABLE, width) {
                assert!(display_width(&line) <= width, "{width}: {line:?}");
            }
        }
    }

    #[test]
    fn nested_list_in_a_tight_item_keeps_one_marker_per_line() {
        let src = "4. Limits.\n   - Today\n   - Summary\n5. One call\n";
        let lines: Vec<String> = render_md(src, 60).iter().map(text).collect();
        assert_eq!(
            lines,
            ["4. Limits.", "   • Today", "   • Summary", "5. One call"]
        );
    }

    #[test]
    fn second_paragraph_of_a_loose_item_is_indented_not_renumbered() {
        let lines: Vec<String> = render_md("1. first\n\n   second\n", 60)
            .iter()
            .map(text)
            .collect();
        assert_eq!(lines, ["1. first", "", "   second"]);
    }

    #[test]
    fn heading_takes_the_theme_color_and_bolds() {
        let lines = render_md("# Title", 40);
        let span = &lines[0].spans[0];
        assert_eq!(span.text, "Title");
        assert!(span.bold);
        assert_eq!(
            span.fg,
            theme().color_for_scope("markup.heading.1").unwrap()
        );
    }

    #[test]
    fn paragraphs_after_a_heading_revert_to_plain_text_color() {
        let lines = render_md("# Title\n\nbody text", 40);
        let plain_fg = theme().ui_fg("ui.text").unwrap();
        let heading = &lines[0].spans[0];
        assert_eq!(
            heading.fg,
            theme().color_for_scope("markup.heading.1").unwrap()
        );
        let body = &lines[2];
        assert_eq!(text(body), "body text");
        for span in &body.spans {
            assert_eq!(
                span.fg, plain_fg,
                "span {:?} kept the heading color",
                span.text
            );
            assert!(!span.bold);
        }
    }

    #[test]
    fn nested_emphasis_composes_attributes() {
        let lines = render_md("**bold _both_** tail", 40);
        let italic = lines[0]
            .spans
            .iter()
            .find(|s| s.italic)
            .expect("italic span");
        assert!(italic.bold);
        assert_eq!(italic.text, "both");
    }

    #[test]
    fn inline_code_takes_the_raw_color() {
        let lines = render_md("run `ls -la` now", 40);
        let code = lines[0]
            .spans
            .iter()
            .find(|s| s.text == "ls -la")
            .expect("code span");
        assert_eq!(code.fg, theme().color_for_scope("markup.raw").unwrap());
    }

    #[test]
    fn code_block_lines_are_truncated_and_colored() {
        let ruler = theme().ui_bg("ui.virtual.ruler");
        let lines = render_md("```\nlet x = 1234567890;\n```", 10);
        assert_eq!(lines.len(), 1);
        assert_eq!(text(&lines[0]), "let x = 12");
        assert_eq!(lines[0].bg, ruler);
        assert!(display_width(&text(&lines[0])) <= 10);
    }

    #[test]
    fn ordered_list_numbers_and_hangs_continuations() {
        let lines = render_md("1. alpha beta", 10);
        assert_eq!(text(&lines[0]), "1. alpha");
        assert_eq!(text(&lines[1]), "   beta");
    }

    #[test]
    fn unordered_list_uses_bullets() {
        let lines = render_md("- one\n- two", 20);
        assert_eq!(text(&lines[0]), "• one");
        assert_eq!(text(&lines[1]), "• two");
    }

    #[test]
    fn task_lists_show_checked_and_unchecked_glyphs() {
        let lines = render_md("- [x] done\n- [ ] todo", 20);
        assert_eq!(text(&lines[0]), "☑ done");
        assert_eq!(text(&lines[1]), "☐ todo");
        assert_eq!(
            lines[0].spans[0].fg,
            theme().color_for_scope("markup.list.checked").unwrap()
        );
        assert_eq!(
            lines[1].spans[0].fg,
            theme().color_for_scope("markup.list.unchecked").unwrap()
        );
    }

    #[test]
    fn block_quotes_get_a_prefix_and_color() {
        let lines = render_md("> quoted", 20);
        assert_eq!(text(&lines[0]), "> quoted");
        assert_eq!(
            lines[0].spans[0].fg,
            theme().color_for_scope("markup.quote").unwrap()
        );
    }

    #[test]
    fn links_underline_the_text_without_the_url() {
        let lines = render_md("[txt](https://example.com)", 40);
        let span = &lines[0].spans[0];
        assert_eq!(span.text, "txt");
        assert!(span.underline);
        assert!(!text(&lines[0]).contains("example.com"));
    }

    #[test]
    fn rule_draws_a_full_width_line() {
        let lines = render_md("---", 12);
        assert_eq!(text(&lines[0]), "─".repeat(12));
    }

    #[test]
    fn paragraphs_are_blank_separated() {
        let lines = render_md("one\n\ntwo", 10);
        let texts: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(texts, ["one", "", "two"]);
    }

    #[test]
    fn wrap_spans_keeps_style_across_a_break() {
        let bold = Span::plain("two", RgbValue::from_u8(255, 0, 0));
        let mut bold_style = bold.clone();
        bold_style.bold = true;
        let plain = Span::plain(" words", RgbValue::from_u8(0, 255, 0));
        let lines = wrap_spans(&[bold_style, plain], 5, Vec::new(), Vec::new());
        assert_eq!(lines.len(), 2);
        assert!(lines[0][0].bold);
        assert!(!lines[1][0].bold);
        assert_eq!(lines[1][0].text, "words");
    }

    #[test]
    fn wrap_spans_splits_words_longer_than_a_line() {
        let span = Span::plain("abcdef", RgbValue::from_u8(0, 0, 0));
        let lines = wrap_spans(&[span], 4, Vec::new(), Vec::new());
        let texts: Vec<String> = lines
            .iter()
            .map(|line| line.iter().map(|s| s.text.as_str()).collect())
            .collect();
        assert_eq!(texts, ["abcd", "ef"]);
    }

    #[test]
    fn wrap_spans_applies_prefixes_as_a_hanging_indent() {
        let marker = Span::plain("• ", RgbValue::from_u8(0, 0, 0));
        let cont = Span::plain("  ", RgbValue::from_u8(0, 0, 0));
        let span = Span::plain("alpha beta", RgbValue::from_u8(0, 0, 0));
        let lines = wrap_spans(&[span], 8, vec![marker], vec![cont]);
        let texts: Vec<String> = lines
            .iter()
            .map(|line| line.iter().map(|s| s.text.as_str()).collect())
            .collect();
        assert_eq!(texts, ["• alpha", "  beta"]);
    }
}
