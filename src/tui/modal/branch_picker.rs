#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

/// A row of the branch picker: HEAD, or the ref at this index of `AppState::git_refs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchRow {
    Head,
    Ref(usize),
}

/// Opens the branch picker over git log window `window_id`, loading the refs when missing.
pub fn open(
    state: &mut crate::tui::state::AppState,
    main_tx: crate::signal::MainTx,
    window_id: usize,
) {
    use crate::signal::AppSignal;
    use crate::tui::modal::Modal;
    state.branch_picker.reset();
    refresh(state);
    if state.git_refs.is_none() {
        match state.repo.clone() {
            Some(repo) => {
                tokio::task::spawn_blocking(move || {
                    let result = crate::git::load_refs(&repo).map(std::sync::Arc::new);
                    crate::signal::send(&main_tx, AppSignal::GitRefsLoaded(result));
                });
            }
            None => crate::signal::send(
                &main_tx,
                AppSignal::GitRefsLoaded(Err("open repository: not a git repository".to_string())),
            ),
        }
    }
    Modal::open(state, Modal::BranchPicker(window_id));
}

/// Matches the query again against the refs there are now, and selects the best match.
pub fn refresh(state: &mut crate::tui::state::AppState) {
    let refs = state.git_refs.as_deref().map_or(&[][..], Vec::as_slice);
    let results = run_ref_match(state.branch_picker.query.text(), refs);
    state.branch_picker.selected = results.first().copied();
    state.branch_picker.results = results;
    state.branch_picker.scroll_offset = 0;
}

/// Hands `input_event` to the branch picker, the modal in front.
pub fn handle(
    state: &mut crate::tui::state::AppState,
    input_event: &InputEvent,
    window_id: usize,
    main_tx: crate::signal::MainTx,
) {
    use crate::tui::modal::Modal;
    match input_event {
        InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(SpecialKey::Esc),
        }) => {
            Modal::close(state);
            return;
        }
        InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(SpecialKey::Enter),
        }) => {
            let Some(source) = picked_source(state) else {
                return;
            };
            Modal::close(state);
            show(state, main_tx, window_id, source);
            return;
        }
        _ => {}
    }

    if state.branch_picker.query.handle_key(input_event) {
        refresh(state);
        return;
    }
    if !crate::tui::modal::list::wheel_outside(state, input_event) {
        let page = crate::tui::modal::list::page_size(state);
        state.branch_picker.navigate(input_event, page);
    }
}

/// The history the selected row stands for; `None` while the refs it names are not loaded.
fn picked_source(state: &crate::tui::state::AppState) -> Option<crate::tui::state::LogSource> {
    use crate::tui::state::LogSource;
    let row = state
        .branch_picker
        .selected
        .or_else(|| state.branch_picker.results.first().copied())?;
    match row {
        BranchRow::Head => Some(LogSource::Head),
        BranchRow::Ref(index) => {
            let git_ref = state.git_refs.as_ref()?.get(index)?;
            Some(LogSource::Branch(git_ref.name.clone()))
        }
    }
}

/// Points log window `window_id` at `source`, forgetting its filter, and loads the history.
fn show(
    state: &mut crate::tui::state::AppState,
    main_tx: crate::signal::MainTx,
    window_id: usize,
    source: crate::tui::state::LogSource,
) {
    use crate::tui::panes::git_log_pane::GitLogPaneComponent;
    let Some(win) = state.git_logs.get_mut(&window_id) else {
        return;
    };
    win.source = source.clone();
    win.query.clear();
    win.filtered = None;
    win.selected = 0;
    GitLogPaneComponent::reload_log(state.repo.clone(), main_tx, window_id, &source);
}

/// The rows whose name holds the query, best first; an empty query lists all, HEAD first.
pub fn run_ref_match(text: &str, refs: &[crate::git::GitRef]) -> Vec<BranchRow> {
    use crate::util::fuzzy::Kind;
    let named = std::iter::once((BranchRow::Head, "HEAD")).chain(
        refs.iter()
            .enumerate()
            .map(|(index, git_ref)| (BranchRow::Ref(index), git_ref.name.as_str())),
    );
    crate::util::fuzzy::ranked(Kind::Plain, text, named).unwrap_or_else(|| {
        std::iter::once(BranchRow::Head)
            .chain((0..refs.len()).map(BranchRow::Ref))
            .collect()
    })
}

/// Paints the branch picker's list.
pub fn render(
    state: &mut crate::tui::state::AppState,
    painter: &mut crate::tui::widgets::fuzzy_picker::FuzzyPicker,
    list: &crate::tui::modal::ModalBox,
) -> RenderOpIRVec {
    use crate::git::RefKind;
    use crate::tui::widgets::fuzzy_picker::{PickerColors, PickerRow};
    let view = crate::tui::modal::list::visible(&mut state.branch_picker, list.rows);
    let (branch_color, tag_color) = crate::tui::panes::git_log_pane::ref_colors(&state.theme);
    let rows: Vec<PickerRow> = state.branch_picker.results[view.range]
        .iter()
        .map(|row| match row {
            BranchRow::Head => PickerRow::plain("HEAD"),
            BranchRow::Ref(index) => {
                match state.git_refs.as_ref().and_then(|refs| refs.get(*index)) {
                    Some(git_ref) if git_ref.kind == RefKind::Tag => {
                        PickerRow::accented(git_ref.name.clone(), tag_color)
                    }
                    Some(git_ref) => PickerRow::accented(git_ref.name.clone(), branch_color),
                    None => PickerRow::plain(""),
                }
            }
        })
        .collect();
    let selected = if state.branch_picker.results.is_empty() {
        None
    } else {
        Some(view.selected)
    };
    crate::tui::modal::list::paint_rows(
        PickerColors::in_overlay(&state.theme),
        painter,
        state.branch_picker.query.text(),
        &rows,
        selected,
        list,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{GitRef, RefKind};
    use crate::tui::modal::Modal;
    use crate::tui::state::{AppState, LogSource};
    use crate::tui::test_support::key;

    fn refs() -> std::sync::Arc<Vec<GitRef>> {
        std::sync::Arc::new(vec![
            GitRef {
                kind: RefKind::Branch,
                name: "main".to_string(),
            },
            GitRef {
                kind: RefKind::Branch,
                name: "topic".to_string(),
            },
        ])
    }

    fn opened() -> (AppState, crate::signal::MainTx) {
        let mut state = AppState::blank_for_test();
        state.git_refs = Some(refs());
        state.git_logs.insert(3, Default::default());
        let (main_tx, _rx) = tokio::sync::mpsc::channel(8);
        open(&mut state, main_tx.clone(), 3);
        (state, main_tx)
    }

    fn type_text(state: &mut AppState, main_tx: &crate::signal::MainTx, text: &str) {
        for c in text.chars() {
            handle(state, &key(Key::Character(c)), 3, main_tx.clone());
        }
    }

    #[tokio::test]
    async fn it_lists_head_first_and_then_every_ref() {
        let (state, _) = opened();
        assert_eq!(state.modal, Some(Modal::BranchPicker(3)));
        assert_eq!(
            state.branch_picker.results,
            [BranchRow::Head, BranchRow::Ref(0), BranchRow::Ref(1)]
        );
        assert_eq!(state.branch_picker.selected, Some(BranchRow::Head));
    }

    #[tokio::test]
    async fn a_query_keeps_the_rows_that_match_and_selects_the_best() {
        let (mut state, main_tx) = opened();
        type_text(&mut state, &main_tx, "top");
        assert_eq!(state.branch_picker.results, [BranchRow::Ref(1)]);
        assert_eq!(state.branch_picker.selected, Some(BranchRow::Ref(1)));
        let (mut state, main_tx) = opened();
        type_text(&mut state, &main_tx, "hea");
        assert_eq!(state.branch_picker.results, [BranchRow::Head]);
    }

    #[tokio::test]
    async fn enter_on_a_branch_shows_its_log() {
        let (mut state, main_tx) = opened();
        {
            let win = state.git_logs.get_mut(&3).unwrap();
            win.selected = 5;
            win.query.set_text("old filter");
            win.filtered = Some(vec![1]);
        }
        type_text(&mut state, &main_tx, "topic");
        handle(
            &mut state,
            &key(Key::SpecialKey(SpecialKey::Enter)),
            3,
            main_tx,
        );
        assert!(state.modal.is_none());
        let win = &state.git_logs[&3];
        assert_eq!(win.source, LogSource::Branch("topic".to_string()));
        assert_eq!(win.selected, 0);
        assert!(win.filtered.is_none());
        assert!(win.query.text().is_empty());
        assert!(
            state.branch_picker.results.is_empty(),
            "the picker is reset"
        );
    }

    #[tokio::test]
    async fn enter_on_head_goes_back_to_the_head_log() {
        let (mut state, main_tx) = opened();
        state.git_logs.get_mut(&3).unwrap().source = LogSource::Branch("topic".to_string());
        handle(
            &mut state,
            &key(Key::SpecialKey(SpecialKey::Enter)),
            3,
            main_tx,
        );
        assert!(state.modal.is_none());
        assert_eq!(state.git_logs[&3].source, LogSource::Head);
    }

    #[tokio::test]
    async fn esc_closes_the_picker_and_leaves_the_log_alone() {
        let (mut state, main_tx) = opened();
        state.git_logs.get_mut(&3).unwrap().source = LogSource::Branch("topic".to_string());
        handle(
            &mut state,
            &key(Key::SpecialKey(SpecialKey::Esc)),
            3,
            main_tx,
        );
        assert!(state.modal.is_none());
        assert_eq!(
            state.git_logs[&3].source,
            LogSource::Branch("topic".to_string())
        );
    }

    #[tokio::test]
    async fn refs_loaded_after_opening_fill_the_list() {
        let mut state = AppState::blank_for_test();
        state.git_logs.insert(3, Default::default());
        state.repo = None;
        let (main_tx, _rx) = tokio::sync::mpsc::channel(8);
        open(&mut state, main_tx, 3);
        assert_eq!(state.branch_picker.results, [BranchRow::Head]);
        crate::tui::panes::git_log_pane::refs_loaded(&mut state, &Ok(refs()));
        assert_eq!(state.branch_picker.results.len(), 3);
    }
}
