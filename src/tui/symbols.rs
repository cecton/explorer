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
/// the vanished ones dropped. Works in keys alone: the watcher already turned its paths into
/// them.
pub(crate) fn files_changed(
    state: &mut crate::tui::state::AppState,
    touched: &crate::signal::FilesChanged,
) {
    use crate::loader::FileKey;
    let stale: std::collections::HashSet<FileKey> = touched
        .modified
        .iter()
        .chain(&touched.removed)
        .copied()
        .collect();
    let mut idx = 0;
    while idx < state.symbol_highlights.len() {
        let origin = state.symbol_highlights[idx].origin_file;
        if touched.removed.contains(&origin) {
            state.symbol_highlights.remove(idx);
            state.mark_session_dirty();
            continue;
        }
        if touched.modified.contains(&origin) {
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
            state.symbol_highlights[idx]
                .locations
                .retain(|location| !stale.contains(&location.file_key));
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

    fn state() -> crate::tui::state::AppState {
        use crate::tui::state::AppState;
        AppState::for_test(camino::Utf8PathBuf::from("/tmp/nowhere"))
    }

    fn location(key: usize) -> crate::signal::SymbolRefLocation {
        crate::signal::SymbolRefLocation {
            file_key: crate::loader::FileKey(key),
            start_byte: 0,
            end_byte: 1,
        }
    }

    fn touching(removed: &[usize], modified: &[usize]) -> crate::signal::FilesChanged {
        let keys = |keys: &[usize]| {
            keys.iter()
                .map(|&key| crate::loader::FileKey(key))
                .collect::<Vec<_>>()
        };
        crate::signal::FilesChanged {
            modified: keys(modified),
            created: Vec::new(),
            removed: keys(removed),
        }
    }

    /// The file a group starts in is gone, so the group goes with it.
    #[test]
    fn a_group_whose_origin_was_removed_goes() {
        let mut state = state();
        state.symbol_highlights = vec![group("a"), group("b")];
        state.symbol_highlights[1].origin_file = crate::loader::FileKey(3);

        files_changed(&mut state, &touching(&[0], &[]));

        assert_eq!(names(&state.symbol_highlights), ["b"]);
    }

    /// A file the group starts in changed: its name and location have to be resolved again.
    #[test]
    fn a_group_whose_origin_changed_is_asked_for_again() {
        let mut state = state();
        let mut changed = group("a");
        changed.locations = vec![location(1), location(2)];
        changed.origin_byte_start = Some(4);
        state.symbol_highlights = vec![changed];

        files_changed(&mut state, &touching(&[], &[0]));

        let group = &state.symbol_highlights[0];
        assert!(group.needs_rebuild);
        assert!(group.locations.is_empty());
        assert_eq!(group.origin_byte_start, None);
    }

    /// A file only a group's uses are in: the uses in it vanish, the group stays and keeps
    /// the ones elsewhere, and nothing is re-resolved.
    #[test]
    fn a_group_loses_the_uses_of_a_file_that_changed() {
        let mut state = state();
        let mut group = group("a");
        group.locations = vec![location(1), location(5)];
        state.symbol_highlights = vec![group];

        files_changed(&mut state, &touching(&[], &[5]));

        assert_eq!(state.symbol_highlights[0].locations, [location(1)]);
        assert!(!state.symbol_highlights[0].needs_rebuild);
    }

    /// A batch that touched no file leaves every group as it was.
    #[test]
    fn an_empty_batch_leaves_the_groups_alone() {
        let mut state = state();
        let mut group = group("a");
        group.locations = vec![location(1)];
        state.symbol_highlights = vec![group];

        files_changed(&mut state, &touching(&[], &[]));

        assert_eq!(state.symbol_highlights[0].locations, [location(1)]);
    }
}
