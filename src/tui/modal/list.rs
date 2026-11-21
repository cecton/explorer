#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

/// The painters of the modal lists, kept across frames: a `FuzzyPicker` owns a matcher
/// (a zeroed allocation of about 135 KB) and the parsed query, so a frame must not make one.
pub struct ModalPainters {
    paths: crate::tui::widgets::fuzzy_picker::FuzzyPicker,
    plain: crate::tui::widgets::fuzzy_picker::FuzzyPicker,
}

impl ModalPainters {
    pub fn new() -> Self {
        use crate::tui::widgets::fuzzy::Kind;
        use crate::tui::widgets::fuzzy_picker::FuzzyPicker;
        Self {
            paths: FuzzyPicker::new(Kind::Paths),
            plain: FuzzyPicker::new(Kind::Plain),
        }
    }

    /// The painter that reads candidates the way `kind` says.
    pub fn of(
        &mut self,
        kind: crate::tui::widgets::fuzzy::Kind,
    ) -> &mut crate::tui::widgets::fuzzy_picker::FuzzyPicker {
        use crate::tui::widgets::fuzzy::Kind;
        match kind {
            Kind::Paths => &mut self.paths,
            Kind::Plain | Kind::Substring => &mut self.plain,
        }
    }
}

/// The part of a picker's results one frame paints.
pub(crate) struct Visible {
    /// The indices into `results` of the rows inside the list.
    pub range: std::ops::Range<usize>,
    /// The selection among those rows.
    pub selected: usize,
}

/// Moves the list so the selection is inside it, and answers which results it shows: a frame
/// builds the rows of these, never of all the results.
pub(crate) fn visible(
    picker_state: &mut crate::tui::widgets::fuzzy_picker::FuzzyPickerState<impl Clone + PartialEq>,
    rows: usize,
) -> Visible {
    let selected = picker_state.selected_index();
    let scroll = picker_state.scroll_to_selection(selected, rows);
    let count = picker_state.results.len();
    let start = scroll.min(count);
    let end = start.saturating_add(rows).min(count);
    Visible {
        range: start..end,
        selected: selected.saturating_sub(start),
    }
}

/// Paints the rows `visible` chose, in `list`.
pub(crate) fn paint_rows(
    colors: crate::tui::widgets::fuzzy_picker::PickerColors,
    painter: &mut crate::tui::widgets::fuzzy_picker::FuzzyPicker,
    query: &str,
    rows: &[crate::tui::widgets::fuzzy_picker::PickerRow],
    selected: Option<usize>,
    list: &crate::tui::modal::ModalBox,
) -> RenderOpIRVec {
    painter.render_results(
        colors,
        query,
        rows,
        selected,
        0,
        list.origin,
        list.rows,
        list.width,
    )
}

/// A wheel outside the modal's list moves nothing: the panes behind it are out of reach.
pub(crate) fn wheel_outside(state: &crate::tui::state::AppState, input_event: &InputEvent) -> bool {
    matches!(input_event, InputEvent::Mouse(mouse)
        if !state.modal_list.is_some_and(|list| list.contains(mouse.pos)))
}

/// The rows the open modal's list shows at once: the page a PageUp/PageDown moves by.
pub(crate) fn page_size(state: &crate::tui::state::AppState) -> usize {
    state.modal_list.map_or(0, |list| list.rows)
}
