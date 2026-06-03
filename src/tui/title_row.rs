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

pub(crate) fn title_bar_colors(focused: bool, theme: &HelixTheme) -> ([u8; 3], [u8; 3]) {
    if focused {
        (
            theme.ui_bg("ui.selection").unwrap_or([50, 50, 90]),
            theme.ui_fg("ui.text").unwrap_or([220, 220, 255]),
        )
    } else {
        (
            theme.ui_bg("ui.statusline").unwrap_or([30, 30, 50]),
            theme.ui_fg("ui.statusline").unwrap_or([180, 180, 220]),
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

    let (bg_active_rgb, fg_active_rgb) = (
        theme.ui_bg("ui.selection").unwrap_or([50, 50, 90]),
        theme.ui_fg("ui.text").unwrap_or([220, 220, 255]),
    );
    let bg_inactive_rgb = theme.ui_bg("ui.statusline").unwrap_or([30, 30, 50]);
    let fg_inactive_rgb = theme.ui_fg("ui.statusline").unwrap_or([180, 180, 220]);
    let fg_deleted_rgb = theme.ui_fg("error").unwrap_or([220, 80, 80]);

    let color_bg_active = tui_color!(bg_active_rgb[0], bg_active_rgb[1], bg_active_rgb[2]);
    let color_fg_active = tui_color!(fg_active_rgb[0], fg_active_rgb[1], fg_active_rgb[2]);
    let color_bg_inactive = tui_color!(bg_inactive_rgb[0], bg_inactive_rgb[1], bg_inactive_rgb[2]);
    let color_fg_inactive = tui_color!(fg_inactive_rgb[0], fg_inactive_rgb[1], fg_inactive_rgb[2]);
    let color_fg_deleted = tui_color!(fg_deleted_rgb[0], fg_deleted_rgb[1], fg_deleted_rgb[2]);

    let color_bg = if focused {
        color_bg_active
    } else {
        color_bg_inactive
    };
    let color_fg = if is_deleted {
        color_fg_deleted
    } else if focused {
        color_fg_active
    } else {
        color_fg_inactive
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
