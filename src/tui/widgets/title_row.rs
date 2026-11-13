#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

pub(crate) trait TitleRow {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        state: &mut crate::tui::state::AppState,
    ) -> usize;
}

pub(crate) const DEFAULT_BG: RgbValue = RgbValue {
    red: 15,
    green: 15,
    blue: 25,
};
pub(crate) const DEFAULT_SELECTION_BG: RgbValue = RgbValue {
    red: 50,
    green: 50,
    blue: 90,
};
pub(crate) const DEFAULT_STATUSLINE_BG: RgbValue = RgbValue {
    red: 30,
    green: 30,
    blue: 50,
};
pub(crate) const DEFAULT_STATUSLINE_FG: RgbValue = RgbValue {
    red: 180,
    green: 180,
    blue: 220,
};
pub(crate) const DEFAULT_TITLE_FG: RgbValue = RgbValue {
    red: 220,
    green: 220,
    blue: 255,
};
pub(crate) const DEFAULT_ERROR_FG: RgbValue = RgbValue {
    red: 220,
    green: 80,
    blue: 80,
};
pub(crate) const DEFAULT_WARNING_FG: RgbValue = RgbValue {
    red: 220,
    green: 170,
    blue: 80,
};

pub(crate) fn title_bar_colors(
    focused: bool,
    theme: &crate::tui::theme::HelixTheme,
) -> (RgbValue, RgbValue) {
    if focused {
        (
            theme.ui_bg("ui.selection").unwrap_or(DEFAULT_SELECTION_BG),
            theme.ui_fg("ui.text").unwrap_or(DEFAULT_TITLE_FG),
        )
    } else {
        (
            theme
                .ui_bg("ui.statusline")
                .unwrap_or(DEFAULT_STATUSLINE_BG),
            theme
                .ui_fg("ui.statusline")
                .unwrap_or(DEFAULT_STATUSLINE_FG),
        )
    }
}

/// Background of a mouse selection in a title-bar input: a focused title bar already
/// wears `ui.selection`, so the selection takes the other title bar's background.
pub(crate) fn title_selection_bg(focused: bool, theme: &crate::tui::theme::HelixTheme) -> RgbValue {
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

/// Paints `input` on the title colors in at most `max_rows` rows; answers the rows taken.
pub(crate) fn render_input_title(
    ops: &mut RenderOpIRVec,
    pane_box: &FlexBox,
    input: &mut crate::tui::widgets::input_line::InputLine,
    prompt: &str,
    focused: bool,
    theme: &crate::tui::theme::HelixTheme,
    max_rows: usize,
) -> usize {
    let origin = pane_box.style_adjusted_bounds.origin_pos;
    let width = pane_box
        .style_adjusted_bounds
        .bounds_size
        .col_width
        .as_u16();
    let height = input.height(None, prompt, width, max_rows);
    let (bg_rgb, fg_rgb) = title_bar_colors(focused, theme);
    let color_bg = TuiColor::from(bg_rgb);
    let color_fg = TuiColor::from(fg_rgb);
    let color_selection = TuiColor::from(title_selection_bg(focused, theme));
    let bg_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});
    for row_offset in 0..height {
        *ops +=
            RenderOpCommon::MoveCursorPositionRelTo(origin, vp_col(0) + vp_row(row_offset as u16));
        *ops += RenderOpCommon::SetBgColor(color_bg);
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

    let (bg_active_rgb, fg_active_rgb) = title_bar_colors(true, theme);
    let (bg_inactive_rgb, fg_inactive_rgb) = title_bar_colors(false, theme);
    let fg_deleted_rgb = theme.ui_fg("error").unwrap_or(DEFAULT_ERROR_FG);

    let color_bg = if focused {
        TuiColor::from(bg_active_rgb)
    } else {
        TuiColor::from(bg_inactive_rgb)
    };
    let color_fg = if is_deleted {
        TuiColor::from(fg_deleted_rgb)
    } else if focused {
        TuiColor::from(fg_active_rgb)
    } else {
        TuiColor::from(fg_inactive_rgb)
    };

    let padded = format!(" {title} ");
    let display = truncate_title(&padded, width);

    render_ops += RenderOpCommon::MoveCursorPositionRelTo(origin, vp_col(0) + vp_row(0));
    render_ops += RenderOpCommon::ResetColor;
    render_ops += RenderOpCommon::SetBgColor(color_bg);
    render_ops += RenderOpIR::PaintTextWithAttributes(SPACER_GLYPH.repeat(width).into(), None);
    render_ops += RenderOpCommon::MoveCursorPositionRelTo(origin, vp_col(0) + vp_row(0));
    render_ops += RenderOpIR::PaintTextWithAttributes(
        display.into(),
        Some(if focused {
            new_style!(bold color_fg: {color_fg} color_bg: {color_bg})
        } else {
            new_style!(color_fg: {color_fg} color_bg: {color_bg})
        }),
    );
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
