#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

pub(crate) mod list;
pub(crate) mod model_picker;
pub(crate) mod theme_picker;
pub(crate) mod window_picker;

/// The modal painted over every pane, which takes every event while it is open.
///
/// A modal is not a window: it has no pane slot, no entry in the session and no place in the
/// focus cycle. It paints at [`ZOrder::High`], above the frame the panes and the status bar
/// already painted, and while it is open no pane sees an event at all.
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)] // each variant names the picker it shows
pub enum Modal {
    /// The fuzzy picker of open windows and, once a query is typed, of files by name.
    WindowPicker,
    /// The theme picker, previewing each theme as the selection moves.
    ThemePicker,
    /// The model picker, choosing for what [`crate::tui::state::ModelPickerPurpose`] names.
    ModelPicker(crate::tui::state::ModelPickerPurpose),
}

impl Modal {
    /// What the modal's frame calls itself.
    pub fn title(&self) -> &'static str {
        match self {
            Modal::WindowPicker => "Windows",
            Modal::ThemePicker => "Themes",
            Modal::ModelPicker(_) => "Model",
        }
    }

    /// The keys the modal takes, for the status bar. A modal takes every key there is, so
    /// this is the whole of what the bar says while one is open.
    pub fn hint(&self) -> &'static str {
        match self {
            Modal::WindowPicker => "Esc:Close  Enter:Open  Ctrl+X:Close window",
            Modal::ThemePicker => "Esc:Cancel  Enter:Save",
            Modal::ModelPicker(_) => "Esc:Cancel  Enter:Choose",
        }
    }

    /// Opens `modal`, remembering where the keyboard was so it can go back there.
    ///
    /// The panes stop taking the keyboard the moment a modal opens: a grabbed terminal is
    /// released, a selection in a pane is dropped and a leader chord in progress is
    /// abandoned, so nothing behind the modal keeps acting as if it were in front.
    pub fn open(state: &mut crate::tui::state::AppState, modal: Modal) {
        if modal != Modal::ThemePicker {
            Self::revert_previewed_theme(state);
        }
        if state.modal.is_none() {
            state.modal_origin = state.pane_manager.focused_window;
        }
        state.modal = Some(modal);
        state.terminal_grabbed = false;
        state.cancel_input_drag();
        state.mouse_drag_active = false;
        state.leader_active = false;
    }

    /// Puts the confirmed theme back when the theme picker is left without Enter, so a
    /// previewed theme is never kept (nor saved to the session) by accident.
    fn revert_previewed_theme(state: &mut crate::tui::state::AppState) {
        if state.modal == Some(Modal::ThemePicker) {
            state.theme = state.saved_theme.clone();
        }
    }

    /// Closes the modal and gives the keyboard back to the window it opened over, if that
    /// window is still in the stack. A terminal gets it back ungrabbed, as after `Tab`.
    pub fn close(state: &mut crate::tui::state::AppState) {
        Self::revert_previewed_theme(state);
        state.modal = None;
        state.modal_list = None;
        state.window_picker.reset();
        state.theme_picker.reset();
        state.model_picker.reset();
        let Some(origin) = state.modal_origin.take() else {
            return;
        };
        if !state.pane_manager.window_stack.contains(&origin) {
            return;
        }
        let old = state.pane_manager.focused_window;
        state.pane_manager.focused_window = Some(origin);
        crate::tui::focus::refocus(state, old);
    }
}

/// Hands `input_event` to the open modal, and answers whether it did something with it.
///
/// `AppMain` sends every event here while a modal is up, so the panes never see one.
pub fn dispatch(
    state: &mut crate::tui::state::AppState,
    input_event: InputEvent,
    window_picker_senders: crate::tui::modal::window_picker::Senders<'_>,
    main_tx: crate::signal::MainTx,
) {
    use crate::tui::modal::model_picker;
    use crate::tui::modal::theme_picker;
    use crate::tui::modal::window_picker;
    match state.modal.clone() {
        Some(Modal::WindowPicker) => {
            window_picker::handle(state, &input_event, window_picker_senders, main_tx)
        }
        Some(Modal::ThemePicker) => theme_picker::handle(state, &input_event),
        Some(Modal::ModelPicker(purpose)) => {
            model_picker::handle(state, &input_event, purpose, main_tx)
        }
        None => {}
    }
}

/// The box a modal paints in: centred on the frame the panes painted, as wide as a pane is
/// worth reading and as tall as its rows need, but never more than half a screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModalBox {
    pub origin: VPPos,
    pub width: usize,
    pub rows: usize,
}

/// Horizontal margin of the box on each side, so the panes behind it stay visible.
const MARGIN: u16 = 4;
/// The widest a modal grows: about one pane's worth of readable lines.
const MAX_WIDTH: u16 = 80;
/// The frame's top row, the query line, and the frame's bottom row.
const CHROME_ROWS: usize = 3;

impl ModalBox {
    /// The box of a modal listing `results` rows over a surface of `surface`.
    pub fn for_modal(surface: VPSize, results: usize) -> Self {
        let cols = surface.col_width.as_u16().max(1);
        let surface_rows = surface.row_height.as_u16().max(1);
        let width = cols
            .saturating_sub(2 * MARGIN)
            .clamp(1, MAX_WIDTH)
            .max(3)
            .min(cols);
        // No `clamp`: its bounds cross on a terminal shorter than the frame, and it panics.
        // A one-row screen gets its one row, with no frame above or below.
        let max_rows = (surface_rows / 2).max(CHROME_ROWS as u16).min(surface_rows) as usize;
        let rows = (results + CHROME_ROWS).min(max_rows).max(1);
        Self {
            origin: vp_col((cols - width) / 2) + vp_row((surface_rows - rows as u16) / 2),
            width: width as usize,
            rows,
        }
    }

    /// The box the list paints in: under the query line, inside the frame on both sides and
    /// its bottom, so nothing is painted over the border.
    pub fn list_box(&self) -> ModalBox {
        ModalBox {
            origin: self.origin + vp_col(1) + vp_row(2),
            width: self.width.saturating_sub(2),
            rows: self.rows.saturating_sub(CHROME_ROWS),
        }
    }

    /// The box the query line paints in.
    pub fn query_box(&self) -> FlexBox {
        FlexBox {
            style_adjusted_bounds: VPBoundingBox::new(
                self.origin + vp_col(1) + vp_row(1),
                vp_width(self.width.saturating_sub(2) as u16) + vp_height(1),
            ),
            ..FlexBox::default()
        }
    }

    /// Whether `pos` is inside the box: a modal takes the wheel only over itself.
    pub fn contains(&self, pos: VPPos) -> bool {
        let col = pos.col_index.as_usize();
        let row = pos.row_index.as_usize();
        let left = self.origin.col_index.as_usize();
        let top = self.origin.row_index.as_usize();
        col >= left && col < left + self.width && row >= top && row < top + self.rows
    }

    /// The frame of the box: every row filled with `surface`, the outline drawn in
    /// `border`, and `title` written on the top edge.
    pub fn render_frame(
        &self,
        surface: crate::tui::theme::Color,
        border: crate::tui::theme::Color,
        title: &str,
    ) -> RenderOpIRVec {
        let mut ops = RenderOpIRVec::new();
        let fill = surface.on(surface);
        let line = border.on(surface);
        for offset in 0..self.rows {
            ops += RenderOpCommon::MoveCursorPositionAbs(self.origin + vp_row(offset as u16));
            ops += RenderOpCommon::ApplyColors(Some(fill));
            ops += RenderOpIR::PaintTextWithAttributes(" ".repeat(self.width).into(), Some(fill));
        }

        // A box too narrow for corners and an edge between them keeps its fill alone: a
        // frame drawn around it would paint over the cells beside it.
        if self.width < 3 || self.rows < 2 {
            return ops;
        }
        let width = self.width;
        let inner = width - 2;
        let last_row = self.rows.saturating_sub(1) as u16;
        for (offset, text) in [
            (0u16, format!("╭{}╮", "─".repeat(inner))),
            (last_row, format!("╰{}╯", "─".repeat(inner))),
        ] {
            ops += RenderOpCommon::MoveCursorPositionAbs(self.origin + vp_row(offset));
            ops += RenderOpCommon::ApplyColors(Some(line));
            ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(line));
        }
        for offset in 1..last_row {
            for col in [0u16, (width - 1) as u16] {
                ops += RenderOpCommon::MoveCursorPositionAbs(
                    self.origin + vp_col(col) + vp_row(offset),
                );
                ops += RenderOpCommon::ApplyColors(Some(line));
                ops += RenderOpIR::PaintTextWithAttributes("│".into(), Some(line));
            }
        }

        // The title sits on the top edge with a cell of frame either side, so the corners
        // stay the frame's own. A title too wide for the box is dropped rather than run over
        // the corner.
        let label = format!(" {title} ");
        if crate::tui::widgets::text_layout::display_width(&label) <= inner.saturating_sub(1) {
            let label_style = border.on(surface);
            ops += RenderOpCommon::MoveCursorPositionAbs(self.origin + vp_col(1) + vp_row(0));
            ops += RenderOpCommon::ApplyColors(Some(label_style));
            ops += RenderOpIR::PaintTextWithAttributes(label.into(), Some(label_style));
        }
        ops
    }
}

/// Paints the open modal over the frame: its box, its query line and its list, `None` when
/// no modal is open. This is the one place modals are painted.
pub fn render(
    state: &mut crate::tui::state::AppState,
    painters: &mut crate::tui::modal::list::ModalPainters,
    surface: VPSize,
) -> Option<RenderOpIRVec> {
    use crate::tui::widgets::fuzzy::Kind;
    use crate::tui::widgets::title_row::BarColors;
    let modal = state.modal.clone()?;
    let results = match modal {
        Modal::WindowPicker => state.window_picker.results.len(),
        Modal::ThemePicker => state.theme_picker.results.len(),
        Modal::ModelPicker(_) => state.model_picker.results.len(),
    };
    // An empty list keeps one row, where "No results" or the reason for it is painted.
    let box_ = ModalBox::for_modal(surface, results.max(1));
    let list = box_.list_box();
    state.modal_list = Some(list);
    let mut ops = box_.render_frame(
        state.theme.ui.overlay,
        state.theme.ui.overlay_border,
        modal.title(),
    );

    let (input, prompt) = match &modal {
        Modal::WindowPicker => (&mut state.window_picker.query, ""),
        Modal::ThemePicker => (&mut state.theme_picker.query, ""),
        Modal::ModelPicker(_) => (&mut state.model_picker.query, "Model: "),
    };
    crate::tui::widgets::title_row::render_input_title(
        &mut ops,
        &box_.query_box(),
        input,
        prompt,
        true,
        BarColors::in_overlay(&state.theme),
        1,
    );

    let rows = match &modal {
        Modal::WindowPicker => {
            crate::tui::modal::window_picker::render(state, painters.of(Kind::Paths), &list)
        }
        Modal::ThemePicker => {
            crate::tui::modal::theme_picker::render(state, painters.of(Kind::Plain), &list)
        }
        Modal::ModelPicker(_) => {
            crate::tui::modal::model_picker::render(state, painters.of(Kind::Plain), &list)
        }
    };
    ops.list.extend(rows.list);
    Some(ops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::pane_manager::Window;
    use crate::tui::state::AppState;
    use crate::tui::state::ModelPickerPurpose;

    fn size(cols: u16, rows: u16) -> VPSize {
        vp_width(cols) + vp_height(rows)
    }

    /// Composes `pipeline` at `screen` and reads back one cell, `None` for a cell nothing
    /// painted over the empty buffer.
    fn painted(screen: VPSize, pipeline: RenderPipeline, row: u16, col: u16) -> Option<PixelChar> {
        let mut ofs_buf = OfsBuf::new(Flat2DArray::new_empty(screen, PixelChar::Spacer));
        pipeline.compose_render_ops_into_ofs_buf(
            screen,
            &mut ofs_buf,
            &mut std::default::Default::default(),
        );
        ofs_buf
            .get_row(vp_row(row))
            .map(|line| line[col as usize])
            .filter(|pixel| !matches!(pixel, PixelChar::Spacer))
    }

    fn char_at(screen: VPSize, pipeline: RenderPipeline, row: u16, col: u16) -> Option<char> {
        match painted(screen, pipeline, row, col) {
            Some(PixelChar::PlainText { display_char, .. }) => Some(display_char),
            _ => None,
        }
    }

    fn frame(modal: &ModalBox) -> RenderPipeline {
        let mut pipeline = render_pipeline!();
        pipeline.push(
            ZOrder::High,
            modal.render_frame(
                crate::tui::theme::Color::from_u8(10, 10, 10),
                crate::tui::theme::Color::from_u8(200, 200, 200),
                "Themes",
            ),
        );
        pipeline
    }

    #[test]
    fn a_modal_centres_itself_and_keeps_its_margins() {
        let wide = ModalBox::for_modal(size(200, 50), 4);
        assert_eq!(wide.width, MAX_WIDTH as usize);
        assert_eq!(wide.origin.col_index.as_u16(), (200 - MAX_WIDTH) / 2);
        assert_eq!(wide.rows, 7, "four rows and the three of its frame");
        let narrow = ModalBox::for_modal(size(40, 24), 4);
        assert_eq!(narrow.width, 32);
        assert_eq!(narrow.origin.col_index.as_u16(), 4);
    }

    /// A list longer than half the screen keeps its query line and its frame and gives way
    /// in the middle, so the box never covers the panes it sits over.
    #[test]
    fn a_long_list_stops_at_half_the_screen() {
        let modal = ModalBox::for_modal(size(80, 24), 500);
        assert_eq!(modal.rows, 12);
        assert_eq!(modal.origin.row_index.as_u16(), 6);
        let list = modal.list_box();
        assert_eq!(list.rows, 9);
        assert_eq!(list.width, modal.width - 2);
        assert_eq!(list.origin, modal.origin + vp_col(1) + vp_row(2));
    }

    /// A terminal only a row or two tall still draws its modal: the height gives way, it
    /// never asks for bounds it cannot honour.
    #[test]
    fn a_surface_shorter_than_the_frame_still_has_a_box() {
        for rows in 1..=3u16 {
            let modal = ModalBox::for_modal(size(40, rows), 5);
            assert!(modal.rows <= rows as usize, "{rows} rows: {modal:?}");
            assert!(
                modal.origin.row_index.as_u16() + modal.rows as u16 <= rows,
                "{rows} rows: {modal:?}"
            );
        }
    }

    /// A surface too small for the box still paints inside itself: the height gives way
    /// first, and a width too narrow for a frame keeps the fill alone.
    #[test]
    fn a_tiny_surface_never_paints_outside_itself() {
        let small = ModalBox::for_modal(size(3, 3), 10);
        assert_eq!(small.width, 3);
        assert!(small.rows <= 3);
        assert!(small.origin.row_index.as_u16() + small.rows as u16 <= 3);
        let short = ModalBox::for_modal(size(80, 4), 100);
        assert!(short.rows <= 4);
        assert_eq!(short.origin.row_index.as_u16(), 0);
    }

    #[test]
    fn the_box_knows_what_it_covers() {
        let modal = ModalBox::for_modal(size(100, 40), 5);
        assert!(modal.contains(modal.origin));
        assert!(modal.contains(modal.origin + vp_col(modal.width as u16 - 1)));
        assert!(!modal.contains(modal.origin + vp_col(modal.width as u16)));
        let top = modal.origin.row_index.as_u16();
        assert!(!modal.contains(vp_col(0) + vp_row(top - 1)));
    }

    /// The frame is a frame: corners and sides on the box itself, the surface under every
    /// cell inside it, and nothing painted outside.
    #[test]
    fn the_frame_draws_the_outline_and_fills_the_inside() {
        let modal = ModalBox::for_modal(size(60, 30), 2);
        let (screen, (top, left)) = (
            size(60, 30),
            (
                modal.origin.row_index.as_u16(),
                modal.origin.col_index.as_u16(),
            ),
        );
        let (right, bottom) = (left + modal.width as u16 - 1, top + modal.rows as u16 - 1);
        assert_eq!(char_at(screen, frame(&modal), top, left), Some('╭'));
        assert_eq!(char_at(screen, frame(&modal), top, right), Some('╮'));
        assert_eq!(char_at(screen, frame(&modal), bottom, left), Some('╰'));
        assert_eq!(char_at(screen, frame(&modal), bottom, right), Some('╯'));
        assert_eq!(char_at(screen, frame(&modal), top + 1, left), Some('│'));
        assert_eq!(char_at(screen, frame(&modal), bottom - 1, right), Some('│'));
        assert_eq!(char_at(screen, frame(&modal), top + 1, left + 1), Some(' '));
        assert_eq!(char_at(screen, frame(&modal), top, left + 1), Some(' '));
        assert_eq!(char_at(screen, frame(&modal), 0, 0), None);
    }

    /// The point of painting at `ZOrder::High`: the pane's cells are under the box, and the
    /// cells it covers are the modal's. One column outside, the pane still shows.
    #[test]
    fn the_box_covers_the_pane_it_stands_on() {
        let screen = size(60, 30);
        let pane_style = crate::tui::theme::Color::from_u8(255, 255, 255)
            .on(crate::tui::theme::Color::from_u8(0, 0, 0));
        let mut pane_ops = RenderOpIRVec::new();
        for row in 0..30u16 {
            pane_ops += RenderOpCommon::MoveCursorPositionAbs(vp_col(0) + vp_row(row));
            pane_ops += RenderOpCommon::ApplyColors(Some(pane_style));
            pane_ops +=
                RenderOpIR::PaintTextWithAttributes("x".repeat(60).into(), Some(pane_style));
        }
        let modal = ModalBox::for_modal(screen, 3);
        let mut pipeline = render_pipeline!();
        pipeline.push(ZOrder::Normal, pane_ops);
        pipeline.push(
            ZOrder::High,
            modal.render_frame(
                crate::tui::theme::Color::from_u8(10, 10, 10),
                crate::tui::theme::Color::from_u8(200, 200, 200),
                "Windows",
            ),
        );

        let (top, left) = (
            modal.origin.row_index.as_u16(),
            modal.origin.col_index.as_u16(),
        );
        assert_eq!(char_at(screen, pipeline.clone(), top, left), Some('╭'));
        assert_eq!(
            char_at(screen, pipeline.clone(), top + 1, left + 1),
            Some(' '),
            "inside the box is the modal"
        );
        assert_eq!(
            char_at(screen, pipeline.clone(), top, left - 1),
            Some('x'),
            "beside the box is the pane"
        );
        assert_eq!(
            char_at(screen, pipeline, top - 1, left + 1),
            Some('x'),
            "above the box is the pane"
        );
    }

    /// A row longer than the list keeps its text inside: a long path in a narrow modal
    /// would otherwise paint over the frame and past the box.
    #[test]
    fn a_row_too_wide_for_the_list_stops_at_its_edge() {
        use crate::tui::widgets::fuzzy_picker::PickerColors;
        use crate::tui::widgets::fuzzy_picker::PickerRow;
        let modal = ModalBox::for_modal(size(20, 12), 3);
        let list = modal.list_box();
        let rows = vec![PickerRow::plain("a/very/long/path/that/does/not/fit.rs")];
        use crate::tui::widgets::fuzzy::Kind;
        let mut painter = crate::tui::widgets::fuzzy_picker::FuzzyPicker::new(Kind::Plain);
        let ops = painter.render_results(
            PickerColors::in_overlay(&crate::tui::theme::HelixTheme::bundled_default()),
            "",
            &rows,
            Some(0),
            0,
            list.origin,
            list.rows,
            list.width,
        );
        let mut pipeline = render_pipeline!();
        pipeline.push(
            ZOrder::High,
            modal.render_frame(
                crate::tui::theme::Color::default(),
                crate::tui::theme::Color::from_u8(200, 200, 200),
                "",
            ),
        );
        pipeline.push(ZOrder::High, ops);

        // Every cell of the box's edge column is the frame, and the cell just outside it is
        // untouched: the row's text stopped at the edge.
        let screen = size(20, 12);
        let (top, left) = (
            modal.origin.row_index.as_u16(),
            modal.origin.col_index.as_u16(),
        );
        let right = left + modal.width as u16 - 1;
        for offset in 1..modal.rows as u16 - 1 {
            assert_eq!(
                char_at(screen, pipeline.clone(), top + offset, right),
                Some('│'),
                "row {} of the frame",
                offset
            );
        }
        assert_eq!(
            char_at(screen, pipeline.clone(), top + 2, right + 1),
            None,
            "nothing is painted past the box"
        );
    }

    /// A title too wide for the box is dropped, so it never runs over a corner.
    #[test]
    fn a_title_too_wide_for_the_box_is_not_painted() {
        let narrow = ModalBox::for_modal(size(12, 10), 2);
        let mut pipeline = render_pipeline!();
        pipeline.push(
            ZOrder::High,
            narrow.render_frame(
                crate::tui::theme::Color::from_u8(0, 0, 0),
                crate::tui::theme::Color::from_u8(255, 255, 255),
                "A very long title that cannot fit",
            ),
        );
        let screen = size(12, 10);
        let top = narrow.origin.row_index.as_u16();
        let edge = narrow.origin.col_index.as_u16() + 1;
        assert_eq!(char_at(screen, pipeline, top, edge), Some('─'));
    }

    /// The whole modal, through the frame's own entry point: the theme picker over a state
    /// that has one, its box painted over the pane's cells, its list inside its frame, and
    /// the page size the keys will page by kept for the events that follow.
    #[test]
    fn the_frame_paints_the_open_modal_over_the_panes() {
        use crate::tui::modal::list::ModalPainters;
        use crate::tui::modal::theme_picker;
        let mut state = AppState::blank_for_test();
        crate::tui::focus::focus_window(&mut state, Window::ConversationTree);
        theme_picker::open(&mut state);

        let screen = size(60, 30);
        assert_eq!(state.modal, Some(Modal::ThemePicker));
        let ops = render(&mut state, &mut ModalPainters::new(), screen).expect("the modal paints");
        let modal = ModalBox::for_modal(screen, state.theme_picker.results.len());
        assert_eq!(state.modal_list, Some(modal.list_box()));

        let pane_style = crate::tui::theme::Color::from_u8(255, 255, 255)
            .on(crate::tui::theme::Color::from_u8(0, 0, 0));
        let mut pane_ops = RenderOpIRVec::new();
        for row in 0..30u16 {
            pane_ops += RenderOpCommon::MoveCursorPositionAbs(vp_col(0) + vp_row(row));
            pane_ops += RenderOpCommon::ApplyColors(Some(pane_style));
            pane_ops +=
                RenderOpIR::PaintTextWithAttributes("x".repeat(60).into(), Some(pane_style));
        }
        let mut pipeline = render_pipeline!();
        pipeline.push(ZOrder::Normal, pane_ops);
        pipeline.push(ZOrder::High, ops);

        let (top, left) = (
            modal.origin.row_index.as_u16(),
            modal.origin.col_index.as_u16(),
        );
        assert_eq!(char_at(screen, pipeline.clone(), top, left), Some('╭'));
        assert_eq!(
            char_at(screen, pipeline.clone(), top, left - 1),
            Some('x'),
            "the panes are still there beside the box"
        );
        // The list starts under the query line, at the row its selection scrolled to.
        let scrolled_to = state.theme_picker.results[state.theme_picker.scroll_offset]
            .chars()
            .next();
        assert_eq!(
            char_at(screen, pipeline, top + 2, left + 1),
            scrolled_to,
            "the first row painted is the one the list scrolled to"
        );
    }

    /// The first row of the list, read back as text.
    fn list_row_text(state: &AppState, screen: VPSize, ops: RenderOpIRVec) -> String {
        let list = state.modal_list.expect("the list was painted");
        let mut pipeline = render_pipeline!();
        pipeline.push(ZOrder::High, ops);
        let top = list.origin.row_index.as_u16();
        let left = list.origin.col_index.as_u16();
        (left..left + list.width as u16)
            .map(|col| char_at(screen, pipeline.clone(), top, col).unwrap_or(' '))
            .collect()
    }

    #[test]
    fn a_modal_with_no_result_says_so() {
        use crate::tui::modal::list::ModalPainters;
        use crate::tui::modal::theme_picker;
        let mut state = AppState::blank_for_test();
        theme_picker::open(&mut state);
        state.theme_picker.results.clear();
        state.theme_picker.selected = None;
        let screen = size(60, 30);
        let ops = render(&mut state, &mut ModalPainters::new(), screen).expect("paints");
        assert!(
            list_row_text(&state, screen, ops).contains("No results"),
            "the list keeps one row for the message"
        );
    }

    #[test]
    fn the_model_picker_with_nothing_to_offer_says_why() {
        use crate::tui::modal::list::ModalPainters;
        let mut state = AppState::blank_for_test();
        Modal::open(
            &mut state,
            Modal::ModelPicker(ModelPickerPurpose::Window(1)),
        );
        state.model_picker.results.clear();
        let screen = size(100, 30);
        let ops = render(&mut state, &mut ModalPainters::new(), screen).expect("paints");
        assert!(
            list_row_text(&state, screen, ops).contains("No endpoint"),
            "the explanation is painted, not a list of zero rows"
        );
    }

    #[test]
    fn a_frame_maps_only_the_rows_the_list_shows() {
        use crate::tui::widgets::fuzzy_picker::FuzzyPickerState;
        let mut picker = FuzzyPickerState {
            results: (0..20_000).collect(),
            selected: Some(12_345),
            ..Default::default()
        };
        let list = ModalBox::for_modal(size(80, 24), picker.results.len()).list_box();
        let view = crate::tui::modal::list::visible(&mut picker, list.rows);
        assert_eq!(view.range.len(), list.rows);
        assert!(view.range.contains(&12_345));
        assert_eq!(view.range.start + view.selected, 12_345);
    }

    /// What the gate in `app/input.rs` hands a modal: a key the picker does not know is still
    /// the modal's, so no pane and no leader chord sees it. The theme picker keeps its query
    /// cleared and only Esc closes it.
    #[test]
    fn a_dispatch_keeps_the_event_inside_the_modal() {
        use crate::tui::modal::theme_picker;
        let mut state = AppState::blank_for_test();
        crate::tui::focus::focus_window(&mut state, Window::ConversationTree);
        theme_picker::open(&mut state);
        let (main_tx, _rx) = tokio::sync::mpsc::channel(1);
        let generation = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let (results_tx, _results_rx) = tokio::sync::mpsc::channel(1);
        let send = |state: &mut crate::tui::state::AppState, event| {
            dispatch(
                state,
                event,
                crate::tui::modal::window_picker::Senders {
                    generation: &generation,
                    results_tx: &results_tx,
                },
                main_tx.clone(),
            );
        };

        // A key no picker knows: the event is the modal's alone, and the panes are untouched.
        send(
            &mut state,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::Character('x'),
            }),
        );
        assert_eq!(state.theme_picker.query.text(), "x");
        assert_eq!(state.modal, Some(Modal::ThemePicker));
        assert_eq!(
            state.pane_manager.focused_window,
            Some(Window::ConversationTree),
            "a pane key never reaches the panes"
        );

        send(
            &mut state,
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }),
        );
        assert!(state.modal.is_none(), "Esc is the only way out");
        assert_eq!(
            state.pane_manager.focused_window,
            Some(Window::ConversationTree),
            "the keyboard goes back to the window it came from"
        );
    }

    #[test]
    fn opening_a_modal_releases_the_keyboard_and_a_selection() {
        let mut state = AppState::blank_for_test();
        crate::tui::focus::focus_window(&mut state, Window::ConversationTree);
        state.terminal_grabbed = true;
        state.mouse_drag_active = true;
        state.input_drag = Some(Window::ConversationTree);
        state.leader_active = true;

        Modal::open(&mut state, Modal::ThemePicker);

        assert_eq!(state.modal, Some(Modal::ThemePicker));
        assert!(!state.terminal_grabbed);
        assert!(!state.mouse_drag_active);
        assert!(state.input_drag.is_none());
        assert!(!state.leader_active);
        assert_eq!(state.modal_origin, Some(Window::ConversationTree));
    }

    fn previewing_another_theme(state: &mut AppState) -> String {
        use crate::tui::theme::HelixTheme;
        let confirmed = state.theme.name().to_string();
        crate::tui::modal::theme_picker::open(state);
        let other = HelixTheme::theme_names()
            .find(|name| *name != confirmed)
            .expect("a second bundled theme");
        state.theme = HelixTheme::from_name(other).expect("bundled theme");
        confirmed
    }

    #[test]
    fn opening_another_modal_over_the_theme_picker_reverts_the_preview() {
        let mut state = AppState::blank_for_test();
        let confirmed = previewing_another_theme(&mut state);
        Modal::open(&mut state, Modal::WindowPicker);
        assert_eq!(state.theme.name(), confirmed);
    }

    #[test]
    fn closing_the_theme_picker_any_way_reverts_the_preview() {
        let mut state = AppState::blank_for_test();
        let confirmed = previewing_another_theme(&mut state);
        Modal::close(&mut state);
        assert_eq!(state.theme.name(), confirmed);
    }

    #[test]
    fn closing_a_modal_returns_the_keyboard_to_the_window_it_opened_over() {
        let mut state = AppState::blank_for_test();
        crate::tui::focus::focus_window(&mut state, Window::ConversationTree);
        Modal::open(&mut state, Modal::WindowPicker);
        Modal::close(&mut state);

        assert!(state.modal.is_none());
        assert_eq!(
            state.pane_manager.focused_window,
            Some(Window::ConversationTree)
        );
        assert!(state.modal_origin.is_none());
        assert!(state.modal_list.is_none());
    }

    /// The window the modal opened over can be closed from behind it: the keyboard is not
    /// given to a window that is no longer there.
    #[test]
    fn closing_a_modal_over_a_closed_window_leaves_the_focus_alone() {
        let mut state = AppState::blank_for_test();
        crate::tui::focus::focus_window(&mut state, Window::ConversationTree);
        crate::tui::focus::focus_window(&mut state, Window::Report(0));
        Modal::open(&mut state, Modal::WindowPicker);
        state.pane_manager.remove_window(&Window::Report(0));
        let focused = state.pane_manager.focused_window;
        Modal::close(&mut state);

        assert_eq!(state.pane_manager.focused_window, focused);
    }

    #[test]
    fn a_modal_opened_twice_keeps_the_origin_of_the_first_one() {
        let mut state = AppState::blank_for_test();
        crate::tui::focus::focus_window(&mut state, Window::ConversationTree);
        Modal::open(&mut state, Modal::ThemePicker);
        Modal::open(
            &mut state,
            Modal::ModelPicker(ModelPickerPurpose::Window(1)),
        );
        assert_eq!(
            state.modal,
            Some(Modal::ModelPicker(ModelPickerPurpose::Window(1)))
        );
        assert_eq!(state.modal_origin, Some(Window::ConversationTree));
    }
}
