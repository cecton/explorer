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

/// Hands `input_event` to the theme picker, the modal in front.
pub fn handle(state: &mut crate::tui::state::AppState, input_event: &InputEvent) {
    use crate::tui::modal::Modal;
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
    if !crate::tui::modal::list::wheel_outside(state, input_event) {
        let page = crate::tui::modal::list::page_size(state);
        if state.theme_picker.navigate(input_event, page) {
            preview_selected_theme(state);
        }
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
    crate::tui::widgets::fuzzy::ranked(
        Kind::Plain,
        text,
        HelixTheme::theme_names().map(|name| (name.to_string(), name)),
    )
    .unwrap_or_else(|| HelixTheme::theme_names().map(str::to_string).collect())
}

/// Paints the theme picker's list.
pub fn render(
    state: &mut crate::tui::state::AppState,
    painter: &mut crate::tui::widgets::fuzzy_picker::FuzzyPicker,
    list: &crate::tui::modal::ModalBox,
) -> RenderOpIRVec {
    use crate::tui::widgets::fuzzy_picker::{PickerColors, PickerRow};
    let view = crate::tui::modal::list::visible(&mut state.theme_picker, list.rows);
    let rows: Vec<PickerRow> = state.theme_picker.results[view.range]
        .iter()
        .map(PickerRow::plain)
        .collect();
    crate::tui::modal::list::paint_rows(
        PickerColors::in_overlay(&state.theme),
        painter,
        state.theme_picker.query.text(),
        &rows,
        Some(view.selected),
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
