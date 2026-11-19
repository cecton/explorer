#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

#[derive(Clone, Debug)]
pub struct FuzzyPickerState<T> {
    pub query: crate::tui::widgets::input_line::InputLine,
    /// The candidates the query kept, best match first. Only the keys: the chars a row
    /// highlights are matched for the rows a frame paints, not for all of them.
    pub results: Vec<T>,
    pub selected: Option<T>,
    /// The first row the list paints, kept across frames so a selection can move past the
    /// bottom of the list without the list jumping back.
    pub scroll_offset: usize,
}

// Manual `Default` avoids the `T: Default` bound the derive would add; the
// picker's fields (`InputLine`, `Vec`, `Option`) are all `Default` for any `T`.
impl<T> Default for FuzzyPickerState<T> {
    fn default() -> Self {
        use crate::tui::widgets::input_line::InputLine;
        Self {
            query: InputLine::new(),
            results: Vec::new(),
            selected: None,
            scroll_offset: 0,
        }
    }
}

impl<T: Clone + PartialEq> FuzzyPickerState<T> {
    pub fn reset(&mut self) {
        self.results.clear();
        self.selected = None;
        self.query.clear();
        self.scroll_offset = 0;
    }

    /// The index of the selected row among `results`: `0` when nothing is selected, or when
    /// the selection is no longer one of them.
    pub fn selected_index(&self) -> usize {
        let Some(key) = &self.selected else {
            return 0;
        };
        self.results
            .iter()
            .position(|result| result == key)
            .unwrap_or(0)
    }

    /// The first row a list of `rows` rows shows so that row `selected` is inside it, and
    /// answers it. The offset only moves as far as the selection needs.
    pub fn scroll_to_selection(&mut self, selected: usize, rows: usize) -> usize {
        if rows > 0 && selected < self.scroll_offset {
            self.scroll_offset = selected;
        } else if rows > 0 && !self.results.is_empty() && selected >= self.scroll_offset + rows {
            self.scroll_offset = selected + 1 - rows;
        }
        self.scroll_offset
    }

    /// Moves the selection by an arrow, a page or a wheel notch, and answers whether the
    /// event was one of them. `page_size` is the rows the list shows at once.
    pub fn navigate(&mut self, input_event: &InputEvent, page_size: usize) -> bool {
        let count = self.results.len();
        if count == 0 {
            return false;
        }
        let current = self.selected_index();
        let new = match input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Down),
            })
            | InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::ScrollDown,
                ..
            }) => (current + 1).min(count - 1),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Up),
            })
            | InputEvent::Mouse(MouseInput {
                kind: MouseInputKind::ScrollUp,
                ..
            }) => current.saturating_sub(1),
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::PageDown),
            }) => {
                let page = page_size.saturating_sub(1).max(1);
                (current + page).min(count - 1)
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::PageUp),
            }) => {
                let page = page_size.saturating_sub(1).max(1);
                current.saturating_sub(page)
            }
            _ => return false,
        };
        self.selected = Some(self.results[new].clone());
        true
    }
}

/// The colors a picker's rows are painted with: the surface behind them, the text of an
/// unselected row, the chars the query matched, and the bar and text of the selected row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PickerColors {
    pub surface: crate::tui::theme::Color,
    pub text: crate::tui::theme::Color,
    pub matched: crate::tui::theme::Color,
    pub selected: crate::tui::theme::Color,
    pub selected_text: crate::tui::theme::Color,
}

impl PickerColors {
    /// A picker painted inside a pane: the pane's own background and the theme's selection.
    pub fn in_pane(theme: &crate::tui::theme::HelixTheme) -> Self {
        Self {
            surface: theme.ui.background,
            text: theme.ui.text,
            matched: theme.ui.matched,
            selected: theme.ui.selection,
            selected_text: theme.ui.text,
        }
    }

    /// A picker painted as a modal over the panes: the overlay surface and its own bar.
    pub fn in_overlay(theme: &crate::tui::theme::HelixTheme) -> Self {
        Self {
            surface: theme.ui.overlay,
            text: theme.ui.overlay_fg,
            matched: theme.ui.matched,
            selected: theme.ui.overlay_selected,
            selected_text: theme.ui.overlay_selected_fg,
        }
    }
}

/// Paints the rows of a picker. It keeps nothing but the parsed query, so the rows it paints
/// can be read from the state that owns them.
pub struct FuzzyPicker {
    /// The query the painted rows are highlighted with, kept across frames so its pattern
    /// is parsed only when the draft moves.
    query: crate::tui::widgets::fuzzy::Query,
}

impl FuzzyPicker {
    pub fn new(kind: crate::tui::widgets::fuzzy::Kind) -> Self {
        Self {
            query: crate::tui::widgets::fuzzy::Query::new(kind),
        }
    }

    /// Paints `rows` from `scroll` down, `height` of them, at `origin`, each `width` cells
    /// wide, with row `selected` wearing the selection colors.
    #[allow(clippy::too_many_arguments)]
    pub fn render_results(
        &mut self,
        colors: PickerColors,
        query: &str,
        rows: &[PickerRow],
        selected: usize,
        scroll: usize,
        origin: VPPos,
        height: usize,
        width: usize,
    ) -> RenderOpIRVec {
        let mut render_ops = RenderOpIRVec::new();
        let count = rows.len();
        self.query.set(query);
        let mut indices = Vec::new();

        for offset in 0..height {
            let index = scroll + offset;
            render_ops +=
                RenderOpCommon::MoveCursorPositionRelTo(origin, vp_col(0) + vp_row(offset as u16));

            let is_selected = index < count && index == selected;
            let row_bg = if is_selected {
                colors.selected
            } else {
                colors.surface
            };
            // A selected row wears a bar the theme meant for text on another background, so
            // the theme's own text color for that bar wins there.
            let row_fg = if is_selected {
                colors.selected_text
            } else {
                colors.text
            };
            let row_style = row_fg.on(row_bg);
            render_ops += RenderOpCommon::ApplyColors(Some(row_style));
            render_ops += RenderOpIR::PaintTextWithAttributes(
                " ".repeat(width).as_str().into(),
                Some(row_style),
            );

            if index >= count {
                if count == 0 && offset == 0 {
                    let msg = crate::tui::widgets::text_layout::truncate("No results", width);
                    let pad = width.saturating_sub(msg.len()) / 2;
                    render_ops += RenderOpIR::PaintTextWithAttributes(
                        format!("{:pad$}{msg}", "", pad = pad).into(),
                        Some(colors.text.on(colors.surface)),
                    );
                }
                continue;
            }

            render_ops +=
                RenderOpCommon::MoveCursorPositionRelTo(origin, vp_col(0) + vp_row(offset as u16));
            let PickerRow { text, accent } = &rows[index];
            // A row longer than the list is cut at its edge: a long path would otherwise
            // paint over the frame and past the box.
            let text = &crate::tui::widgets::text_layout::truncate(text, width);
            let base_fg = accent.unwrap_or(row_fg);
            let emphasized = accent.is_some();
            self.query.match_indices(text, &mut indices);
            let style_of = |char_idx: usize| {
                let matched = crate::tui::widgets::fuzzy::matched(&indices, char_idx);
                let fg = if matched { colors.matched } else { base_fg };
                // Bold for fuzzy-match characters and for accented rows; selection is
                // carried by the row's background.
                if matched || emphasized {
                    fg.on_bold(row_bg)
                } else {
                    fg.on(row_bg)
                }
            };
            crate::tui::widgets::fuzzy::paint_runs(&mut render_ops, text, style_of);
        }

        render_ops
    }
}

/// One row of a picker: its text, and the color rows of its kind wear instead of the plain
/// text color.
#[derive(Clone, Debug, PartialEq)]
pub struct PickerRow {
    pub text: String,
    pub accent: Option<crate::tui::theme::Color>,
}

impl PickerRow {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            accent: None,
        }
    }

    pub fn accented(text: impl Into<String>, accent: crate::tui::theme::Color) -> Self {
        Self {
            text: text.into(),
            accent: Some(accent),
        }
    }
}
