//! The symbol highlight groups: what a group is, and the two places the shell touches them.
//!
//! A group ties a qualified name to every place it appears, coloured so the origin and its
//! uses can be read at a glance. The bookkeeping lives here so that the file-change handler
//! and the symbol answer handler share one rule for keeping a name unique.

/// Drops every group but the one at `keep` that carries `qualified_name`, so a name has
/// one group. `keep` survives the removals before it: it names that group, not a position.
pub(crate) fn dedup(
    groups: &mut Vec<crate::tui::state::SymbolHighlightGroup>,
    qualified_name: &str,
    keep: Option<usize>,
) {
    let mut keep = keep;
    let mut idx = 0;
    while idx < groups.len() {
        if Some(idx) != keep && groups[idx].qualified_name == qualified_name {
            groups.remove(idx);
            if keep.is_some_and(|kept| idx < kept) {
                keep = keep.map(|kept| kept - 1);
            }
        } else {
            idx += 1;
        }
    }
}

/// Re-locates the group a symbol answer is for, by where it starts rather than by the index
/// the request carried: toggles that land while the query is in flight shift indices.
pub(crate) fn find(
    groups: &[crate::tui::state::SymbolHighlightGroup],
    origin: crate::loader::FileKey,
    line: u32,
    character: u32,
) -> Option<usize> {
    groups.iter().position(|group| {
        group.origin_file == origin && group.origin_line == line && group.origin_char == character
    })
}

/// Invalidates what a changed repository made stale: a group whose origin went away goes
/// with it, one whose origin changed asks for its uses again, and one whose uses moved has
/// the vanished ones dropped.
pub(crate) fn files_changed(
    state: &mut crate::tui::state::AppState,
    batch: &crate::signal::FilesChanged,
) {
    let snapshot = state.files.load_full();
    let stale: Vec<&camino::Utf8Path> = batch
        .modified
        .iter()
        .map(|path| path.as_path())
        .chain(batch.removed.iter().map(|path| path.as_path()))
        .collect();
    let mut idx = 0;
    while idx < state.symbol_highlights.len() {
        let origin = state.symbol_highlights[idx].origin_file;
        let Some(origin_path) = crate::loader::path_for_file_key(&snapshot, origin) else {
            idx += 1;
            continue;
        };
        let origin_path = origin_path.to_path_buf();
        if batch.removed.iter().any(|path| path == &origin_path) {
            state.symbol_highlights.remove(idx);
            state.mark_session_dirty();
            continue;
        }
        if batch.modified.iter().any(|path| path == &origin_path) {
            let group = &mut state.symbol_highlights[idx];
            group.needs_rebuild = true;
            group.origin_byte_start = None;
            group.origin_byte_end = None;
            group.locations.clear();
            crate::lsp::send_symbol_request(
                origin.0,
                group.origin_line,
                group.origin_char,
                group.origin_word.clone(),
                Some(idx),
            );
        } else {
            state.symbol_highlights[idx].locations.retain(|location| {
                !crate::loader::path_for_file_key(&snapshot, location.file_key)
                    .is_some_and(|path| stale.contains(&path))
            });
        }
        idx += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(name: &str) -> crate::tui::state::SymbolHighlightGroup {
        crate::tui::state::SymbolHighlightGroup {
            qualified_name: name.to_string(),
            origin_file: crate::loader::FileKey(0),
            origin_line: 0,
            origin_char: 0,
            origin_word: None,
            origin_byte_start: None,
            origin_byte_end: None,
            color: crate::tui::state::SYMBOL_PALETTE[0],
            locations: Vec::new(),
            needs_rebuild: false,
        }
    }

    fn names(groups: &[crate::tui::state::SymbolHighlightGroup]) -> Vec<&str> {
        groups
            .iter()
            .map(|group| group.qualified_name.as_str())
            .collect()
    }

    #[test]
    fn dedup_keeps_only_the_named_group() {
        let mut groups = vec![group("a"), group("b"), group("a")];
        dedup(&mut groups, "a", Some(2));
        assert_eq!(names(&groups), ["b", "a"]);
    }

    #[test]
    fn dedup_keeps_the_group_it_names_when_an_earlier_duplicate_goes() {
        let mut kept = group("a");
        kept.origin_line = 2;
        let mut trailing = group("a");
        trailing.origin_line = 3;
        let mut groups = vec![group("a"), kept, trailing];
        dedup(&mut groups, "a", Some(1));
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].origin_line, 2);
    }

    #[test]
    fn dedup_without_a_group_to_keep_drops_them_all() {
        let mut groups = vec![group("a"), group("b"), group("a")];
        dedup(&mut groups, "a", None);
        assert_eq!(names(&groups), ["b"]);
    }
}
