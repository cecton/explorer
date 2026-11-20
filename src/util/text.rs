/// Returns the byte range `(start, end)` of the "word" under `cursor_byte`.
///
/// The definition of "word" depends on what the cursor is sitting on:
///
/// 1. **Whitespace** — the entire contiguous run of whitespace characters
///    (spaces, tabs, newlines, etc.).
/// 2. **URL** — if the non-whitespace span parses as a `url::Url`, the full
///    URL is returned. URL boundaries are whitespace and the characters
///    `"`, `)`, `]`, `}`.
/// 3. **Alphanumeric word** — otherwise, a run of `[A-Za-z0-9_]` characters.
///
/// # Panics
///
/// Panics in debug builds if `cursor_byte` is not a valid byte index into
/// `content` (i.e. `cursor_byte > content.len()`). Callers are
/// expected to keep the cursor aligned to character boundaries.
///
/// # Edge cases
///
/// * Empty content → `(0, 0)`.
/// * Cursor on punctuation that is not part of a URL → returns the single
///   character at that position.
pub fn word_bounds(content: &str, cursor_byte: usize) -> (usize, usize) {
    if content.is_empty() {
        return (0, 0);
    }
    debug_assert!(
        cursor_byte <= content.len(),
        "cursor_byte {cursor_byte} out of bounds for content of length {}",
        content.len()
    );
    let cursor_byte = cursor_byte.min(content.len() - 1);
    let c = content[cursor_byte..].chars().next().unwrap_or('\0');

    macro_rules! scan_backward {
        ($i:ident, $char:ident => $take_while:expr) => {{
            content[..(cursor_byte + c.len_utf8())]
                .char_indices()
                .rev()
                .take_while(|&($i, $char)| $take_while)
                .last()
                .map(|(i, _)| i)
                .unwrap_or(cursor_byte)
        }};
    }

    macro_rules! scan_forward {
        ($i:ident, $char:ident => $take_while:expr) => {{
            content[cursor_byte..]
                .char_indices()
                .take_while(|&($i, $char)| $take_while)
                .last()
                .map(|(i, c)| cursor_byte + i + c.len_utf8())
                .unwrap_or(cursor_byte + c.len_utf8())
        }};
    }

    if c.is_whitespace() {
        let start = scan_backward!(_i, c => c.is_whitespace());
        let end = scan_forward!(_i, c => c.is_whitespace());
        (start, end)
    } else {
        let is_url_boundary = |c: char| c.is_whitespace() || matches!(c, '"' | ')' | ']' | '}');
        let end = scan_forward!(_i, c => !is_url_boundary(c));
        let run_start = scan_backward!(_i, c => !is_url_boundary(c));
        let url = url_start(content, run_start, cursor_byte, end).map(|start| (start, end));
        if let Some((start, end)) = url {
            (start, end)
        } else {
            let is_word = |c: char| c.is_alphanumeric() || c == '_';
            let start = scan_backward!(_i, c => is_word(c));
            let end = scan_forward!(_i, c => is_word(c));
            (start, end)
        }
    }
}

/// Earliest URL start in `run_start..=cursor` ending at `end`: one parse per scheme class and `:`.
fn url_start(content: &str, run_start: usize, cursor: usize, end: usize) -> Option<usize> {
    let run = &content[run_start..end];
    let last_authority = run.rfind("://")? + run_start;
    let is_scheme_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.');
    let mut scheme_from = 0;
    for (colon, _) in run.match_indices(':') {
        let scheme = &run[scheme_from..colon];
        let scheme_at = run_start + scheme_from;
        scheme_from = colon + 1;
        let begin = scheme
            .char_indices()
            .rev()
            .take_while(|&(_, c)| is_scheme_char(c))
            .last()
            .map_or(scheme.len(), |(i, _)| i);
        // `Url::parse` trims the C0 controls in front of its input.
        let lead = scheme[..begin]
            .char_indices()
            .rev()
            .take_while(|&(_, c)| c <= ' ')
            .last()
            .map_or(begin, |(i, _)| i);
        let mut tried = [false; 3];
        for (offset, _) in scheme[begin..]
            .char_indices()
            .filter(|(_, c)| c.is_ascii_alphabetic())
        {
            let start = scheme_at + begin + offset;
            let from = if offset == 0 { scheme_at + lead } else { start };
            if from > cursor || from > last_authority {
                return None;
            }
            let name = &scheme[begin + offset..];
            let class = if name.eq_ignore_ascii_case("file") {
                0
            } else if ["http", "https", "ws", "wss", "ftp"]
                .iter()
                .any(|special| name.eq_ignore_ascii_case(special))
            {
                1
            } else {
                2
            };
            if !std::mem::replace(&mut tried[class], true)
                && url::Url::parse(&content[from..end]).is_ok()
            {
                return Some(from);
            }
        }
    }
    None
}

/// Range covered by a drag from `word`, clicked at `anchor`, to the word under
/// `cursor`: whole words at both ends, whatever the rows they are on.
pub(crate) fn drag_range(
    text: &str,
    anchor: usize,
    word: (usize, usize),
    cursor: usize,
) -> (usize, usize) {
    let (start, end) = word_bounds(text, cursor);
    if cursor >= anchor {
        (word.0, end.max(word.1))
    } else {
        (start.min(word.0), word.1)
    }
}

pub(crate) fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Splits a command file into its metadata block and what follows it. The block opens
/// with `---` on the first line; without a closing `---` the file has no metadata.
pub(crate) fn split_metadata(text: &str) -> (&str, &str) {
    let mut offset = 0;
    let mut block_start = None;
    for line in text.split_inclusive('\n') {
        let is_rule = line.trim_end() == "---";
        match block_start {
            None if is_rule => block_start = Some(line.len()),
            None => break,
            Some(start) if is_rule => return (&text[start..offset], &text[offset + line.len()..]),
            Some(_) => {}
        }
        offset += line.len();
    }
    ("", text)
}

/// The non-empty `key: value` pairs of a metadata block, key and value trimmed.
pub(crate) fn key_values(metadata: &str) -> impl Iterator<Item = (&str, &str)> {
    metadata.lines().filter_map(|line| {
        let (key, value) = line.split_once(':')?;
        let value = value.trim();
        (!value.is_empty()).then(|| (key.trim(), value))
    })
}

/// Keeps the head and the tail of an output that is too long.
pub(crate) fn truncate_middle(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let half = max_bytes / 2;
    let mut head_end = half;
    while !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len() - half;
    while !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!(
        "{}\n[... {} bytes omitted ...]\n{}",
        &text[..head_end],
        tail_start - head_end,
        &text[tail_start..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(content: &str, cursor: usize) -> &str {
        let (start, end) = word_bounds(content, cursor);
        &content[start..end]
    }

    #[test]
    fn word_bounds_whitespace() {
        assert_eq!(of("foo    bar", 3), "    ");
        assert_eq!(of("foo  \n  bar", 3), "  \n  ");
        assert_eq!(of("foo\t\tbar", 3), "\t\t");
    }

    #[test]
    fn word_bounds_url() {
        assert_eq!(of("url=file://url?foo#bar baz", 6), "file://url?foo#bar");
        assert_eq!(
            of("format!(\"file://root\").parse().unwrap()", 15),
            "file://root"
        );
        assert_eq!(
            of("see also https://example.com/path?q=1", 10),
            "https://example.com/path?q=1"
        );
        assert_eq!(of("[https://a.b]", 5), "https://a.b");
        assert_eq!(of("{https://a.b}", 5), "https://a.b");
    }

    #[test]
    fn word_bounds_alphanumeric() {
        assert_eq!(of("fn foo_bar() {}", 6), "foo_bar");
        assert_eq!(of("123abc", 3), "123abc");
        assert_eq!(of("use serde_json::Value;", 12), "serde_json");
    }

    #[test]
    fn word_bounds_multibyte() {
        assert_eq!(of("héllo", 3), "héllo");
    }

    #[test]
    fn word_bounds_long_run_without_whitespace_is_fast() {
        let run = "a".repeat(200_000);
        let started = std::time::Instant::now();
        assert_eq!(of(&run, 100_000).len(), run.len());
        let refused = "x://[".repeat(40_000);
        assert_eq!(of(&refused, 100_000), "x");
        let urls = "http://a/".repeat(20_000);
        assert_eq!(of(&urls, 100_000).len(), urls.len());
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn word_bounds_url_after_junk_scheme_chars() {
        assert_eq!(of("(1http://a.b/c x", 8), "http://a.b/c");
        assert_eq!(of("x=\"ssh+git://h/p\" y", 10), "ssh+git://h/p");
    }

    #[test]
    fn word_bounds_url_starting_with_control_chars() {
        // `Url::parse` trims the C0 controls in front of a URL, so the URL starts at them.
        assert_eq!(of("x=\u{1}http://a.b/c y", 3), "\u{1}http://a.b/c");
        assert_eq!(of("\u{1}http://a.b/c", 0), "\u{1}http://a.b/c");
    }

    #[test]
    fn word_bounds_url_whose_earlier_scheme_letters_do_not_parse() {
        // `xhttp` is non-special, so its backslash host fails; `http` parses as special.
        assert_eq!(of("xhttp://a\\b", 8), "http://a\\b");
    }

    #[test]
    fn word_bounds_empty() {
        assert_eq!(word_bounds("", 0), (0, 0));
    }

    #[test]
    fn word_bounds_trailing_punctuation() {
        assert_eq!(of("foo.bar", 3), ".");
    }

    #[test]
    fn single_line_folds_the_whitespace_runs() {
        assert_eq!(single_line(" a \n\t b  c "), "a b c");
        assert_eq!(single_line("   "), "");
    }

    #[test]
    fn key_values_skips_blank_and_valueless_lines() {
        let pairs: Vec<(&str, &str)> = key_values("a: 1\nno value\nb:  \nc: two words\n").collect();
        assert_eq!(pairs, vec![("a", "1"), ("c", "two words")]);
    }

    #[test]
    fn truncate_middle_keeps_head_and_tail() {
        let text = "a".repeat(50) + &"b".repeat(50);
        let cut = truncate_middle(&text, 20);
        assert!(cut.starts_with("aaaaaaaaaa\n[... 80 bytes omitted ...]\n"));
        assert!(cut.ends_with("bbbbbbbbbb"));
    }
}
