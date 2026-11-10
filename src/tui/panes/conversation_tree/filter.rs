/// Byte ranges of a text, start inclusive and end exclusive, in order.
pub(crate) type Ranges = Vec<(usize, usize)>;

/// A node the filter kept, with what matched in its title and in its summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FilterMatch {
    pub node: crate::conversation::NodeId,
    pub title: Ranges,
    pub summary: Ranges,
}

/// The nodes holding every word of `query` in their title or their summary, in any
/// order: best match first, then newest first.
pub(crate) fn filter_nodes(
    tree: &crate::conversation::ConversationTree,
    query: &str,
) -> Vec<FilterMatch> {
    let pattern = nucleo::pattern::Pattern::new(
        query,
        nucleo::pattern::CaseMatching::Smart,
        nucleo::pattern::Normalization::Smart,
        nucleo::pattern::AtomKind::Substring,
    );
    let mut matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
    let mut chars = Vec::new();
    let mut scored: Vec<(u32, FilterMatch)> = tree
        .nodes()
        .filter_map(|(id, node)| {
            let title = node.display_title();
            let text = match crate::tui::panes::conversation_tree::layout::shown_summary(node) {
                Some(summary) => format!("{title}\n{summary}"),
                None => title.to_string(),
            };
            // `Utf32Str::new` indexes by byte or by grapheme depending on the text.
            chars.clear();
            chars.extend(text.chars());
            // Nucleo misses a substring that ends a non-ASCII haystack.
            chars.push('\n');
            let score = pattern.score(nucleo::Utf32Str::Unicode(&chars), &mut matcher)?;
            let mut indices = Vec::new();
            for atom in &pattern.atoms {
                push_occurrences(atom, &chars, &mut matcher, &mut indices);
            }
            indices.sort_unstable();
            indices.dedup();
            let (title, summary) = matched_ranges(&text, title.len(), &indices);
            Some((
                score,
                FilterMatch {
                    node: id.clone(),
                    title,
                    summary,
                },
            ))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.node.cmp(&a.1.node)));
    scored.into_iter().map(|(_, found)| found).collect()
}

/// Every place `atom` matches in `chars`, as char indices: an atom alone reports only
/// the first one.
fn push_occurrences(
    atom: &nucleo::pattern::Atom,
    chars: &[char],
    matcher: &mut nucleo::Matcher,
    indices: &mut Vec<u32>,
) {
    let mut from = 0;
    let mut found = Vec::new();
    while from < chars.len() {
        found.clear();
        let rest = nucleo::Utf32Str::Unicode(&chars[from..]);
        if atom.indices(rest, matcher, &mut found).is_none() {
            break;
        }
        let Some(last) = found.iter().max().copied() else {
            break;
        };
        indices.extend(found.iter().map(|index| index + from as u32));
        from += last as usize + 1;
    }
}

/// Turns sorted char indices of `text`, a title of `title_len` bytes then a newline and
/// a summary, into byte ranges of the title and of the summary.
fn matched_ranges(text: &str, title_len: usize, indices: &[u32]) -> (Ranges, Ranges) {
    let mut title = Ranges::new();
    let mut summary = Ranges::new();
    let mut wanted = indices.iter().copied().peekable();
    for (index, (at, ch)) in text.char_indices().enumerate() {
        if wanted.peek() != Some(&(index as u32)) {
            continue;
        }
        wanted.next();
        let (ranges, start) = match at.cmp(&title_len) {
            std::cmp::Ordering::Less => (&mut title, at),
            std::cmp::Ordering::Equal => continue,
            std::cmp::Ordering::Greater => (&mut summary, at - title_len - 1),
        };
        let end = start + ch.len_utf8();
        match ranges.last_mut() {
            Some(last) if last.1 == start => last.1 = end,
            _ => ranges.push((start, end)),
        }
    }
    (title, summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::NodeId;
    use crate::tui::panes::conversation_tree::test_support::described_tree;

    fn matched_ids(matches: &[FilterMatch]) -> Vec<&str> {
        matches.iter().map(|found| found.node.as_str()).collect()
    }

    #[test]
    fn the_filter_wants_every_word_in_any_order_across_title_and_summary() {
        let (_tmp, tree) = described_tree(&[
            ("a1", "Queue rows", "Esc takes the prompt back"),
            ("a1/a2", "Queue badge", "Shown in the gutter"),
            ("b1", "Spinner frames", "No queued prompt, no escape"),
        ]);
        let sorted_ids = |query: &str| {
            let mut ids: Vec<String> = filter_nodes(&tree, query)
                .into_iter()
                .map(|found| found.node.as_str().to_string())
                .collect();
            ids.sort();
            ids
        };
        assert_eq!(sorted_ids("esc queue"), ["a1", "b1"]);
        assert_eq!(sorted_ids("queue esc"), ["a1", "b1"]);
        assert_eq!(sorted_ids("queue"), ["a1", "a2", "b1"]);
        // An uppercase letter makes the word case-sensitive.
        assert_eq!(sorted_ids("Queue"), ["a1", "a2"]);
        assert!(sorted_ids("qeue").is_empty());
    }

    #[test]
    fn a_word_ending_the_text_is_found() {
        let (_tmp, tree) = described_tree(&[
            ("a1", "Fix — the queue", ""),
            ("b1", "Fix the queue", ""),
            ("c1", "Queue", "Le café"),
        ]);
        let matches = filter_nodes(&tree, "queue");
        assert_eq!(matched_ids(&matches), ["c1", "b1", "a1"]);
        assert_eq!(matches[2].title, [(12, 17)]);
        assert_eq!(filter_nodes(&tree, "café")[0].summary, [(3, 8)]);
    }

    #[test]
    fn equal_matches_come_newest_first() {
        let (_tmp, tree) = described_tree(&[
            ("a1", "Wrap the title", ""),
            ("a1/a2", "Wrap the title", ""),
            ("b1", "Wrap the title", ""),
        ]);
        for _ in 0..5 {
            assert_eq!(
                matched_ids(&filter_nodes(&tree, "wrap")),
                ["b1", "a2", "a1"]
            );
        }
    }

    #[test]
    fn matches_are_byte_ranges_of_the_title_and_of_the_summary() {
        let (_tmp, tree) =
            described_tree(&[("a1", "Café queue", "Le café: the queue, then the café")]);
        let matches = filter_nodes(&tree, "café queue");
        assert_eq!(
            matches,
            [FilterMatch {
                node: NodeId::new("a1"),
                title: vec![(0, 5), (6, 11)],
                summary: vec![(3, 8), (14, 19), (30, 35)],
            }]
        );
        let title = tree.node(&NodeId::new("a1")).unwrap().display_title();
        assert_eq!(&title[0..5], "Café");
        assert_eq!(&title[6..11], "queue");
    }
}
