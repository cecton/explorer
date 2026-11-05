use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(crate) fn display_width(text: &str) -> usize {
    if text.is_ascii() {
        return text.len();
    }
    text.graphemes(true).map(UnicodeWidthStr::width).sum()
}

/// Cuts `text` to at most `width` display columns, on a grapheme boundary.
pub(crate) fn truncate(text: &str, width: usize) -> String {
    if text.is_ascii() {
        let mut end = width.min(text.len());
        // CRLF is one grapheme cluster, two columns: never cut between the two bytes.
        if end > 0 && text.as_bytes()[end - 1] == b'\r' && text.as_bytes().get(end) == Some(&b'\n')
        {
            end -= 1;
        }
        return text[..end].to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let w = display_width(grapheme);
        if used + w > width {
            break;
        }
        used += w;
        out.push_str(grapheme);
    }
    out
}

/// Word-wraps `text` to `width` display columns. Explicit newlines are kept, and a word
/// longer than a whole line is split on grapheme boundaries. Each line comes with the
/// byte offset in `text` where it starts; it is a slice of `text` from there.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<(usize, String)> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut base = 0;
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut start = base;
        let mut used = 0;
        for (index, word) in paragraph.split_word_bound_indices() {
            let offset = base + index;
            let word_width = display_width(word);
            if used + word_width <= width {
                if line.is_empty() {
                    start = offset;
                }
                line.push_str(word);
                used += word_width;
                continue;
            }
            if word.trim().is_empty() {
                lines.push((start, std::mem::take(&mut line)));
                start = offset + word.len();
                used = 0;
                continue;
            }
            if used > 0 {
                push_trimmed(&mut lines, start, std::mem::take(&mut line));
                used = 0;
            }
            for (at, grapheme) in word.grapheme_indices(true) {
                let w = display_width(grapheme);
                if used + w > width && used > 0 {
                    lines.push((start, std::mem::take(&mut line)));
                    used = 0;
                }
                if line.is_empty() {
                    start = offset + at;
                }
                line.push_str(grapheme);
                used += w;
            }
        }
        push_trimmed(&mut lines, start, line);
        base += paragraph.len() + 1;
    }
    lines
}

fn push_trimmed(lines: &mut Vec<(usize, String)>, start: usize, mut line: String) {
    let trimmed = line.trim_end().len();
    line.truncate(trimmed);
    lines.push((start, line));
}

#[cfg(test)]
mod tests {
    use super::*;
    use r3bl_tui::GCStringOwned;

    fn gc(text: &str) -> usize {
        GCStringOwned::from(text).display_width().as_usize()
    }

    fn texts(text: &str, width: usize) -> Vec<String> {
        wrap(text, width)
            .into_iter()
            .map(|(_, line)| line)
            .collect()
    }

    #[test]
    fn display_width_matches_gc_string() {
        let samples = [
            "",
            "hello",
            "héllo wörld",
            "日本語のテキスト",
            "👨‍👩‍👧‍👦 family",
            "e\u{0301}cole",
            "🇧🇪🇫🇷",
            "a\tb",
            "\u{200b}zero width space",
            "❤️ heart",
            "ﾊﾝｶｸ halfwidth",
            "ÀÉÎÕÜ",
            "\u{1b}[31mred",
            "trailing\n",
            " combining \u{0301}",
            "\u{00a0}nbsp",
        ];
        for sample in samples {
            assert_eq!(display_width(sample), gc(sample), "{sample:?}");
            for grapheme in sample.graphemes(true) {
                assert_eq!(display_width(grapheme), gc(grapheme), "{grapheme:?}");
            }
        }
    }

    #[test]
    fn display_width_counts_ascii_controls_as_one_column() {
        for sample in ["\t", "\u{0}", "\u{7f}", "a\tb", "\u{1b}[0m"] {
            assert_eq!(display_width(sample), gc(sample), "{sample:?}");
        }
    }

    #[test]
    fn wrap_breaks_on_word_boundaries() {
        assert_eq!(texts("the quick brown fox", 10), ["the quick", "brown fox"]);
    }

    #[test]
    fn wrap_keeps_explicit_newlines_and_blank_lines() {
        assert_eq!(texts("one\n\ntwo", 10), ["one", "", "two"]);
    }

    #[test]
    fn wrap_splits_words_longer_than_a_line() {
        assert_eq!(texts("abcdefgh ij", 3), ["abc", "def", "gh", "ij"]);
    }

    #[test]
    fn wrap_never_exceeds_the_width() {
        for line in texts("a mix of short and extraordinarily long words, wrapped", 7) {
            assert!(display_width(&line) <= 7, "{line:?}");
        }
    }

    #[test]
    fn wrap_lines_are_slices_of_the_text_at_their_offset() {
        let text = "  the quick  brown\n\nextraordinarily héllo wörld, wrapped   ";
        for width in 1..=20 {
            for (start, line) in wrap(text, width) {
                assert_eq!(&text[start..start + line.len()], line, "width {width}");
            }
        }
    }

    #[test]
    fn truncate_cuts_on_grapheme_boundaries() {
        assert_eq!(truncate("héllo wörld", 4), "héll");
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abcdef", 3), "abc");
        assert_eq!(truncate("\u{1b}[31m", 2), "\u{1b}[");
        assert_eq!(truncate("日本語", 4), "日本");
        assert_eq!(truncate("日本語", 0), "");
        assert_eq!(truncate("\r\n", 1), "");
        assert_eq!(truncate("a\r\nb", 2), "a");
    }
}
