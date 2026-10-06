use crate::tui::*;
use unicode_segmentation::UnicodeSegmentation;

pub(crate) fn display_width(text: &str) -> usize {
    GCStringOwned::from(text).display_width().as_usize()
}

/// Cuts `text` to at most `width` display columns, on a grapheme boundary.
pub(crate) fn truncate(text: &str, width: usize) -> String {
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
                lines.push((start, std::mem::take(&mut line).trim_end().to_string()));
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
        lines.push((start, line.trim_end().to_string()));
        base += paragraph.len() + 1;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(text: &str, width: usize) -> Vec<String> {
        wrap(text, width)
            .into_iter()
            .map(|(_, line)| line)
            .collect()
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
    }
}
