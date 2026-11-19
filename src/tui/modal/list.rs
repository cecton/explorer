#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

/// Paints a picker's list in `list`, after moving the list so the selection is inside it.
pub(crate) fn paint_rows(
    theme: &crate::tui::theme::HelixTheme,
    kind: crate::tui::widgets::fuzzy::Kind,
    rows: &[crate::tui::widgets::fuzzy_picker::PickerRow],
    picker_state: &mut crate::tui::widgets::fuzzy_picker::FuzzyPickerState<impl Clone + PartialEq>,
    list: &crate::tui::modal::ModalBox,
) -> RenderOpIRVec {
    use crate::tui::widgets::fuzzy_picker::FuzzyPicker;
    use crate::tui::widgets::fuzzy_picker::PickerColors;
    let selected = picker_state.selected_index();
    let scroll = picker_state.scroll_to_selection(selected, list.rows);
    let query = picker_state.query.text().to_string();
    let mut painter = FuzzyPicker::new(kind);
    painter.render_results(
        PickerColors::in_overlay(theme),
        &query,
        rows,
        selected,
        scroll,
        list.origin,
        list.rows,
        list.width,
    )
}

/// Moves the open modal's selection and answers whether the event moved it. The wheel only
/// counts over the modal's list: the panes behind it are out of reach, so a scroll that lands
/// on one of them moves nothing.
pub(crate) fn navigate(
    state: &mut crate::tui::state::AppState,
    input_event: &InputEvent,
    page_size: usize,
) -> bool {
    use crate::tui::modal::Modal;
    if let InputEvent::Mouse(mouse) = input_event
        && !state.modal_box.is_some_and(|list| list.contains(mouse.pos))
    {
        return false;
    }
    match state.modal {
        Some(Modal::WindowPicker) => state.window_picker.navigate(input_event, page_size),
        Some(Modal::ThemePicker) => state.theme_picker.navigate(input_event, page_size),
        Some(Modal::ModelPicker(_)) => state.model_picker.navigate(input_event, page_size),
        None => false,
    }
}
