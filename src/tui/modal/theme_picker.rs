#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

/// Opens the theme picker over the panes, its selection on the theme in force.
pub fn open(state: &mut crate::tui::state::AppState) {
    use crate::tui::modal::Modal;
    use crate::tui::theme::HelixTheme;
    state.saved_theme = state.theme.clone();
    let all_themes: Vec<String> = HelixTheme::theme_names().map(str::to_string).collect();
    state.theme_picker.selected = all_themes
        .iter()
        .find(|name| name.as_str() == state.theme.name())
        .cloned();
    state.theme_picker.results = all_themes;
    state.theme_picker.query.clear();
    state.theme_picker.scroll_offset = 0;
    Modal::open(state, Modal::ThemePicker);
}

/// Hands `input_event` to the theme picker while it is the modal in front.
pub fn handle(state: &mut crate::tui::state::AppState, input_event: &InputEvent) {
    use crate::tui::modal::Modal;
    if !matches!(state.modal, Some(Modal::ThemePicker)) {
        return;
    }
    match input_event {
        InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(SpecialKey::Esc),
        }) => {
            state.theme = state.saved_theme.clone();
            Modal::close(state);
            return;
        }
        InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(SpecialKey::Enter),
        }) => {
            state.saved_theme = state.theme.clone();
            Modal::close(state);
            return;
        }
        _ => {}
    }

    if state.theme_picker.query.handle_key(input_event) {
        let query = state.theme_picker.query.text().to_string();
        state.theme_picker.results = run_theme_name_match(&query);
        state.theme_picker.selected = state.theme_picker.results.first().cloned();
        state.theme_picker.scroll_offset = 0;
        preview_selected_theme(state);
        return;
    }
    let page_size = state.modal_page_size;
    if crate::tui::modal::list::navigate(state, input_event, page_size) {
        preview_selected_theme(state);
    }
}

/// The theme the picker highlights, put in force: the picker previews as it moves.
fn preview_selected_theme(state: &mut crate::tui::state::AppState) {
    use crate::tui::theme::HelixTheme;
    let Some(name) = state.theme_picker.selected.clone() else {
        return;
    };
    if let Some(theme) = HelixTheme::from_name(&name) {
        state.theme = theme;
    }
}

/// The theme names the query keeps, best match first; an empty query lists them all.
pub fn run_theme_name_match(text: &str) -> Vec<String> {
    use crate::tui::theme::HelixTheme;
    use crate::tui::widgets::fuzzy::Kind;
    use crate::tui::widgets::fuzzy::Query;
    let mut query = Query::new(Kind::Plain);
    query.set(text);
    if query.is_empty() {
        return HelixTheme::theme_names().map(str::to_string).collect();
    }
    let mut scored: Vec<(String, u32)> = HelixTheme::theme_names()
        .filter_map(|name| query.rank(name).map(|score| (name.to_string(), score)))
        .collect();
    scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
    scored.into_iter().map(|(name, _)| name).collect()
}

/// Paints the theme picker's list.
pub fn render(
    state: &mut crate::tui::state::AppState,
    list: &crate::tui::modal::ModalBox,
) -> RenderOpIRVec {
    use crate::tui::widgets::fuzzy::Kind;
    use crate::tui::widgets::fuzzy_picker::PickerRow;
    let rows: Vec<PickerRow> = state
        .theme_picker
        .results
        .iter()
        .map(PickerRow::plain)
        .collect();
    crate::tui::modal::list::paint_rows(
        &state.theme.clone(),
        Kind::Plain,
        &rows,
        &mut state.theme_picker,
        list,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_query_lists_every_theme() {
        assert_eq!(
            run_theme_name_match("").len(),
            crate::tui::theme::HelixTheme::theme_names().count()
        );
    }

    /// A query narrows the list and ranks the exact name first.
    #[test]
    fn a_query_keeps_the_names_that_match_best_first() {
        let names = run_theme_name_match("gruvbox");
        assert!(!names.is_empty());
        assert!(names[0].starts_with("gruvbox"), "{:?}", names[0]);
        assert!(names.iter().all(|name| name.contains("gruvbox")));
    }
}
