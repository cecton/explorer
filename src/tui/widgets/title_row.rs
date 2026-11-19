#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

/// The colors of an input painted in a bar: the bar itself, its text, and the background of
/// a mouse selection. A bar already wearing the selection color takes the other title bar's
/// background for the selection, so the two are never the same cell color.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BarColors {
    pub bar: crate::tui::theme::Color,
    pub text: crate::tui::theme::Color,
    pub selection: crate::tui::theme::Color,
}

impl BarColors {
    /// A pane's title bar: focused it wears `ui.selection`, else the status line's colors.
    pub(crate) fn in_pane(focused: bool, theme: &crate::tui::theme::HelixTheme) -> Self {
        let (bar, text) = title_bar_colors(focused, theme);
        Self {
            bar,
            text,
            selection: title_selection_bg(focused, theme),
        }
    }

    /// A modal's query line: the overlay surface, its text and its selected row's bar.
    pub(crate) fn in_overlay(theme: &crate::tui::theme::HelixTheme) -> Self {
        Self {
            bar: theme.ui.overlay,
            text: theme.ui.overlay_fg,
            selection: theme.ui.overlay_selected,
        }
    }
}

/// The background and the text of a title bar: a focused one wears the selection.
pub(crate) fn title_bar_colors(
    focused: bool,
    theme: &crate::tui::theme::HelixTheme,
) -> (crate::tui::theme::Color, crate::tui::theme::Color) {
    if focused {
        (theme.ui.selection, theme.ui.text)
    } else {
        (theme.ui.statusline_bg, theme.ui.statusline_fg)
    }
}

/// Background of a mouse selection in a title-bar input: a focused title bar already
/// wears `ui.selection`, so the selection takes the other title bar's background.
pub(crate) fn title_selection_bg(
    focused: bool,
    theme: &crate::tui::theme::HelixTheme,
) -> crate::tui::theme::Color {
    title_bar_colors(!focused, theme).0
}

/// Half the rows of `pane_box`, at least one: the most a title-bar input may grow to.
pub(crate) fn half_height(pane_box: &FlexBox) -> usize {
    (pane_box
        .style_adjusted_bounds
        .bounds_size
        .row_height
        .as_usize()
        / 2)
    .max(1)
}

/// Paints `input` on a bar's colors in at most `max_rows` rows; answers the rows taken.
pub(crate) fn render_input_title(
    ops: &mut RenderOpIRVec,
    pane_box: &FlexBox,
    input: &mut crate::tui::widgets::input_line::InputLine,
    prompt: &str,
    focused: bool,
    colors: BarColors,
    max_rows: usize,
) -> usize {
    let origin = pane_box.style_adjusted_bounds.origin_pos;
    let width = pane_box
        .style_adjusted_bounds
        .bounds_size
        .col_width
        .as_u16();
    let height = input.height(None, prompt, width, max_rows);
    let BarColors {
        bar: color_bg,
        text: color_fg,
        selection: color_selection,
    } = colors;
    let bg_style = color_fg.on(color_bg);
    for row_offset in 0..height {
        *ops +=
            RenderOpCommon::MoveCursorPositionRelTo(origin, vp_col(0) + vp_row(row_offset as u16));
        *ops += RenderOpCommon::ApplyColors(Some(bg_style));
        *ops += RenderOpIR::PaintTextWithAttributes(
            " ".repeat(width as usize).as_str().into(),
            Some(bg_style),
        );
    }
    input.render(
        ops,
        None,
        prompt,
        origin,
        width,
        height,
        focused,
        (color_bg, color_fg, color_selection),
    );
    height
}

pub(crate) fn render_pane_title(
    mut render_ops: &mut RenderOpIRVec,
    pane_box: &FlexBox,
    title: &str,
    is_deleted: bool,
    theme: &crate::tui::theme::HelixTheme,
    focused: bool,
) {
    let origin = pane_box.style_adjusted_bounds.origin_pos;
    let width = pane_box
        .style_adjusted_bounds
        .bounds_size
        .col_width
        .as_usize();

    let (color_bg, title_fg) = title_bar_colors(focused, theme);
    let color_fg = if is_deleted { theme.ui.error } else { title_fg };
    let mut style = color_fg.on(color_bg);
    if focused {
        style.attribs.bold = Some(tui_style_attrib::Bold);
    }

    let padded = format!(" {title} ");
    let display = truncate_title(&padded, width);

    render_ops += RenderOpCommon::MoveCursorPositionRelTo(origin, vp_col(0) + vp_row(0));
    render_ops += RenderOpCommon::ApplyColors(Some(style));
    render_ops += RenderOpIR::PaintTextWithAttributes(" ".repeat(width).into(), Some(style));
    render_ops += RenderOpCommon::MoveCursorPositionRelTo(origin, vp_col(0) + vp_row(0));
    render_ops += RenderOpIR::PaintTextWithAttributes(display.into(), Some(style));
}

fn truncate_title(padded: &str, width: usize) -> String {
    if padded.chars().count() <= width {
        return padded.to_string();
    }
    let max_chars = width.saturating_sub(1);
    let truncated: String = padded.chars().take(max_chars).collect();
    format!("{truncated}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_title_never_splits_multibyte_chars() {
        let title = "commit abcdef0 né抵 — summary text that is long";
        let out = truncate_title(title, 21);
        assert!(out.chars().count() <= 21);
        assert!(out.ends_with('…'));
        assert_eq!(truncate_title("short", 21), "short");
    }
}
