use crate::tui::*;

pub(crate) trait TitleRow {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        theme: &HelixTheme,
        state: &AppState,
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

pub(crate) fn title_bar_colors(focused: bool, theme: &HelixTheme) -> (RgbValue, RgbValue) {
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

pub(crate) fn render_pane_title(
    mut render_ops: &mut RenderOpIRVec,
    pane_box: &FlexBox,
    title: &str,
    is_deleted: bool,
    theme: &HelixTheme,
    focused: bool,
) {
    let origin = pane_box.style_adjusted_origin_pos;
    let width = pane_box.style_adjusted_bounds_size.col_width.as_usize();

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
    let display = if padded.len() > width {
        let truncated = &padded[..width.saturating_sub(1)];
        format!("{truncated}…")
    } else {
        padded
    };

    render_ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
    render_ops += RenderOpCommon::ResetColor;
    render_ops += RenderOpCommon::SetBgColor(color_bg);
    render_ops += RenderOpIR::PaintTextWithAttributes(SPACER_GLYPH.repeat(width).into(), None);
    render_ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
    render_ops += RenderOpIR::PaintTextWithAttributes(
        display.into(),
        Some(if focused {
            new_style!(bold color_fg: {color_fg} color_bg: {color_bg})
        } else {
            new_style!(color_fg: {color_fg} color_bg: {color_bg})
        }),
    );
}
