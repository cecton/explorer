/// How a fuzzy query reads its candidates. `Substring` wants every word held by the text, in
/// any order; `Paths` gives a bonus to a segment boundary.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Plain,
    Paths,
    Substring,
}

/// A parsed query with the matcher and the haystack it reuses, so every picker runs one
/// fuzzy loop: parse once, then rank or match each candidate without a fresh
/// `nucleo::Matcher` (about 1 µs, and its 135 KB scoring matrix) or a haystack per candidate.
pub(crate) struct Query {
    kind: Kind,
    pattern: nucleo::pattern::Pattern,
    matcher: nucleo::Matcher,
    chars: Vec<char>,
    /// A substring atom misses a match that ends a non-ASCII haystack.
    pad: bool,
    /// The text the pattern was parsed from, so a frame that did not change it does not
    /// parse it again.
    text: String,
}

impl Query {
    pub(crate) fn new(kind: Kind) -> Self {
        Self {
            kind,
            pattern: nucleo::pattern::Pattern::parse(
                "",
                nucleo::pattern::CaseMatching::Smart,
                nucleo::pattern::Normalization::Smart,
            ),
            matcher: nucleo::Matcher::new(match kind {
                Kind::Paths => nucleo::Config::DEFAULT.match_paths(),
                Kind::Plain | Kind::Substring => nucleo::Config::DEFAULT,
            }),
            chars: Vec::new(),
            pad: kind == Kind::Substring,
            text: String::new(),
        }
    }

    /// Parses `text`, unless it is the query already held.
    pub(crate) fn set(&mut self, text: &str) {
        if self.text == text {
            return;
        }
        self.text.clear();
        self.text.push_str(text);
        match self.kind {
            Kind::Plain | Kind::Paths => self.pattern.reparse(
                &self.text,
                nucleo::pattern::CaseMatching::Smart,
                nucleo::pattern::Normalization::Smart,
            ),
            Kind::Substring => {
                self.pattern = nucleo::pattern::Pattern::new(
                    &self.text,
                    nucleo::pattern::CaseMatching::Smart,
                    nucleo::pattern::Normalization::Smart,
                    nucleo::pattern::AtomKind::Substring,
                );
            }
        }
    }

    /// The query holds no word: it matches everything, and the caller keeps its own order.
    pub(crate) fn is_empty(&self) -> bool {
        self.pattern.atoms.is_empty()
    }

    /// The score of `text`, `None` when it does not match it.
    pub(crate) fn rank(&mut self, text: &str) -> Option<u32> {
        let Self {
            pattern,
            matcher,
            chars,
            pad,
            ..
        } = self;
        pattern.score(haystack(text, chars, *pad), matcher)
    }

    /// The score of `text` and every char index of it that matched, sorted: char indices,
    /// not bytes and not graphemes, so a highlight indexes the chars it paints.
    pub(crate) fn match_indices(&mut self, text: &str, indices: &mut Vec<u32>) -> Option<u32> {
        indices.clear();
        let Self {
            pattern,
            matcher,
            chars,
            pad,
            ..
        } = self;
        let score = pattern.indices(haystack(text, chars, *pad), matcher, indices)?;
        indices.sort_unstable();
        indices.dedup();
        Some(score)
    }

    /// The score of `text` and every place every word of the query occurs in it, sorted, off
    /// one haystack: an atom alone reports only its best occurrence.
    pub(crate) fn score_and_occurrences(
        &mut self,
        text: &str,
        indices: &mut Vec<u32>,
    ) -> Option<u32> {
        indices.clear();
        let Self {
            pattern,
            matcher,
            chars,
            pad,
            ..
        } = self;
        let haystack = haystack(text, chars, *pad);
        let score = pattern.score(haystack, matcher)?;
        let mut found = Vec::new();
        for atom in &pattern.atoms {
            let mut from = 0;
            while from < haystack.len() {
                found.clear();
                if atom
                    .indices(haystack.slice(from..), matcher, &mut found)
                    .is_none()
                {
                    break;
                }
                let Some(last) = found.iter().max().copied() else {
                    break;
                };
                indices.extend(found.iter().map(|index| index + from as u32));
                from += last as usize + 1;
            }
        }
        indices.sort_unstable();
        indices.dedup();
        Some(score)
    }
}

/// The haystack a matcher reads, and the indices it reports back. `Utf32Str::new` is not
/// used: it indexes by grapheme, and as bytes for any text whose graphemes all start with an
/// ASCII char, so its positions drift from the chars a row paints. An ASCII text is read as
/// bytes, where a byte index is a char index, and any other as its chars. A padded query
/// reads chars too: nucleo scores the same text differently as bytes and as chars, and the
/// conversation tree's filter has always ranked a padded char haystack.
fn haystack<'a>(text: &'a str, chars: &'a mut Vec<char>, pad: bool) -> nucleo::Utf32Str<'a> {
    if !pad && text.is_ascii() {
        return nucleo::Utf32Str::Ascii(text.as_bytes());
    }
    chars.clear();
    chars.extend(text.chars());
    if pad {
        chars.push('\n');
    }
    nucleo::Utf32Str::Unicode(chars)
}

/// Paints `text` with one op per run of one style, the styles `style_at` gives per char
/// index, instead of one op and one string per char.
pub(crate) fn paint_runs(
    ops: &mut r3bl_tui::RenderOpIRVec,
    text: &str,
    style_at: impl Fn(usize) -> r3bl_tui::TuiStyle,
) {
    let mut start = 0;
    let mut current: Option<r3bl_tui::TuiStyle> = None;
    for (index, (byte, _)) in text.char_indices().enumerate() {
        let style = style_at(index);
        if current != Some(style) {
            if let Some(done) = current {
                *ops += r3bl_tui::RenderOpIR::PaintTextWithAttributes(
                    text[start..byte].into(),
                    Some(done),
                );
            }
            current = Some(style);
            start = byte;
        }
    }
    if let Some(done) = current {
        *ops += r3bl_tui::RenderOpIR::PaintTextWithAttributes(text[start..].into(), Some(done));
    }
}

/// Whether `indices`, sorted, hold `index`: the style a highlight gives a char.
pub(crate) fn matched(indices: &[u32], index: usize) -> bool {
    indices.binary_search(&(index as u32)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_count_chars_not_graphemes_nor_bytes() {
        // `Utf32Str::new` reads this text as bytes and reported chars 7..11, which spell
        // "atte": a combining mark makes its byte positions drift from the painted chars.
        let text = "cafe\u{301} latte";
        let mut query = Query::new(Kind::Plain);
        query.set("latte");
        let mut indices = Vec::new();
        query.match_indices(text, &mut indices).unwrap();
        assert_eq!(indices, [6, 7, 8, 9, 10]);
        assert_eq!(painted(text, &indices), "latte");
    }

    #[test]
    fn a_zwj_emoji_costs_its_own_chars() {
        let text = "👨‍👩‍👧‍👦 family time";
        let mut query = Query::new(Kind::Plain);
        query.set("time");
        let mut indices = Vec::new();
        query.match_indices(text, &mut indices).unwrap();
        assert_eq!(painted(text, &indices), "time");
    }

    #[test]
    fn an_empty_query_matches_everything_with_no_indices() {
        let mut query = Query::new(Kind::Plain);
        query.set("");
        assert!(query.is_empty());
        assert_eq!(query.rank("anything"), Some(0));
        let mut indices = Vec::new();
        assert_eq!(query.match_indices("anything", &mut indices), Some(0));
        assert!(indices.is_empty());
    }

    #[test]
    fn a_query_ranks_and_matches_with_the_same_score() {
        let mut query = Query::new(Kind::Plain);
        query.set("readme");
        let mut indices = Vec::new();
        let ranked = query.rank("docs: update readme").unwrap();
        let scored = query
            .match_indices("docs: update readme", &mut indices)
            .unwrap();
        assert_eq!(ranked, scored);
        assert_eq!(painted("docs: update readme", &indices), "readme");
    }

    #[test]
    fn a_path_query_scores_a_segment_boundary_as_the_start() {
        let mut paths = Query::new(Kind::Paths);
        paths.set("srcpane");
        let mut plain = Query::new(Kind::Plain);
        plain.set("srcpane");
        let text = "src/tui/panes/preview.rs";
        assert_ne!(paths.rank(text).unwrap(), plain.rank(text).unwrap());
    }

    #[test]
    fn a_substring_query_finds_a_word_ending_a_non_ascii_text() {
        let mut query = Query::new(Kind::Substring);
        query.set("café");
        assert!(
            query.rank("le café").is_some(),
            "an atom alone misses a match that ends the haystack"
        );
    }

    #[test]
    fn a_plain_query_reads_an_ascii_text_as_bytes_and_a_substring_one_as_padded_chars() {
        // nucleo scores and picks differently over the same text as bytes and as chars, so
        // which representation a kind reads is a ranking decision, not a detail.
        let mut chars = Vec::new();
        assert!(matches!(
            haystack("docs: readme", &mut chars, false),
            nucleo::Utf32Str::Ascii(_)
        ));
        assert!(matches!(
            haystack("café", &mut chars, false),
            nucleo::Utf32Str::Unicode(_)
        ));
        let mut chars = Vec::new();
        assert!(matches!(
            haystack("docs", &mut chars, true),
            nucleo::Utf32Str::Unicode(_)
        ));
        assert_eq!(chars, ['d', 'o', 'c', 's', '\n']);
    }

    #[test]
    fn a_substring_query_keeps_the_ranking_it_has_always_had() {
        // The tree's filter always scored a padded char haystack, and nucleo scores the same
        // text differently as bytes: this pins the representation, not only the score.
        let text = "Café queue\nthe café and the queue";
        let mut query = Query::new(Kind::Substring);
        query.set("café queue");
        let mut indices = Vec::new();
        let scored = query.score_and_occurrences(text, &mut indices).unwrap();

        let mut chars: Vec<char> = text.chars().collect();
        chars.push('\n');
        let pattern = nucleo::pattern::Pattern::new(
            "café queue",
            nucleo::pattern::CaseMatching::Smart,
            nucleo::pattern::Normalization::Smart,
            nucleo::pattern::AtomKind::Substring,
        );
        let mut matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        assert_eq!(
            Some(scored),
            pattern.score(nucleo::Utf32Str::Unicode(&chars), &mut matcher)
        );
    }

    #[test]
    fn occurrences_report_every_place_not_just_the_best() {
        let mut query = Query::new(Kind::Substring);
        query.set("queue");
        let mut indices = Vec::new();
        query
            .score_and_occurrences("queue the queue", &mut indices)
            .unwrap();
        assert_eq!(indices, [0, 1, 2, 3, 4, 10, 11, 12, 13, 14]);
    }

    #[test]
    fn setting_the_same_text_again_keeps_the_pattern() {
        let mut query = Query::new(Kind::Plain);
        query.set("pane");
        let first = query.rank("src/tui/panes/preview.rs");
        query.set("pane");
        assert_eq!(query.rank("src/tui/panes/preview.rs"), first);
        query.set("prev");
        assert_eq!(query.rank("src/tui/panes/preview.rs"), first);
    }

    #[test]
    fn runs_of_one_style_are_painted_whole() {
        #[allow(clippy::wildcard_imports)] // the style macros: see AGENTS.md
        use r3bl_tui::*;
        let mut ops = RenderOpIRVec::new();
        let bold = new_style!(bold);
        let plain = new_style!();
        paint_runs(&mut ops, "hello world", |index| {
            if index < 5 { bold } else { plain }
        });
        let painted: Vec<String> = ops
            .iter()
            .filter_map(|op| match op {
                RenderOpIR::PaintTextWithAttributes(text, _) => Some(text.as_str().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(painted, ["hello", " world"]);
    }

    #[test]
    fn matched_finds_a_char_the_indices_hold() {
        assert!(matched(&[0, 1, 5], 5));
        assert!(!matched(&[0, 1, 5], 2));
        assert!(!matched(&[], 0));
    }

    fn painted(text: &str, indices: &[u32]) -> String {
        indices
            .iter()
            .filter_map(|index| text.chars().nth(*index as usize))
            .collect()
    }
}
