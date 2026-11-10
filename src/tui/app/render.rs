#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

pub(super) fn render(
    app: &mut crate::tui::app::AppMain,
    global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
    component_registry_map: &mut ComponentRegistryMap<
        crate::tui::state::AppState,
        crate::signal::AppSignal,
    >,
    has_focus: &mut HasFocus,
) -> CommonResult {
    use crate::tui::panes_renderer::PanesRenderer;
    crate::tui::app::sync_has_focus(&global_data.state, has_focus);

    let mut best_generation = 0u64;
    let mut best_results = None;
    while let Ok((arrived_generation, results)) = app.picker_results_rx.try_recv() {
        if arrived_generation > best_generation {
            best_generation = arrived_generation;
            best_results = Some(results);
        }
    }
    if let Some(results) = best_results {
        global_data.state.window_picker.results = results;
    }

    crate::tui::app::poll_terminal_output(app, &mut global_data.state);

    throws!({
        let window_size = global_data.window_size;
        let surface_size = crate::tui::app::surface_size(window_size);
        global_data.state.last_surface_size = surface_size;
        let visible = global_data.state.pane_manager.layout(surface_size);

        // Sync focused window with actual visible windows: if the currently focused
        // window is not visible, focus the frontmost visible one.
        let focused = global_data.state.pane_manager.focused_window;
        let focused_is_visible = focused
            .as_ref()
            .map(|f| visible.iter().any(|s| &s.window == f))
            .unwrap_or(false);
        if !global_data.state.mouse_drag_active
            && !focused_is_visible
            && let Some(first) = visible.first()
        {
            let old = global_data.state.pane_manager.focused_window;
            global_data.state.pane_manager.focused_window = Some(first.window);
            crate::tui::focus::refocus(&mut global_data.state, old);
        }
        crate::tui::app::sync_has_focus(&global_data.state, has_focus);

        // Build surface and render components. They push ops into global_data.pipeline.
        {
            let mut it = surface!(stylesheet: create_stylesheet(&global_data.state.theme)?);
            it.surface_start(SurfaceProps {
                pos: vp_col(0) + vp_row(0),
                size: surface_size,
            })?;

            PanesRenderer { visible: &visible }.render_in_surface(
                &mut it,
                global_data,
                component_registry_map,
                has_focus,
            )?;

            it.surface_end()?;
        }

        // Take the pipeline (components have already pushed into it) and add
        // background fill and status bar on top.
        let mut pipeline = std::mem::take(&mut global_data.pipeline);

        // Fill entire surface area with pane background (covers padding
        // between panes, which the FlexBox layout system does not fill).
        let bg_rgb = global_data
            .state
            .theme
            .ui_bg("ui.background")
            .unwrap_or(crate::tui::widgets::title_row::DEFAULT_BG);
        let bg = TuiColor::from(bg_rgb);
        let bg_style = new_style!(color_bg: {bg});
        let mut bg_ops = RenderOpIRVec::new();
        let surface_rows = surface_size.row_height.as_usize();
        let surface_col_count = window_size.col_width.as_usize();
        for row_idx in 0..surface_rows {
            let abs_row: u16 = row_idx as u16;
            bg_ops += RenderOpCommon::MoveCursorPositionAbs(vp_col(0) + vp_row(abs_row));
            bg_ops += RenderOpCommon::ApplyColors(Some(bg_style));
            bg_ops += RenderOpIR::PaintTextWithAttributes(
                " ".repeat(surface_col_count).as_str().into(),
                Some(bg_style),
            );
        }
        let mut fill_pipeline = render_pipeline!();
        fill_pipeline.push(ZOrder::Normal, bg_ops);
        fill_pipeline.join_into(pipeline);
        pipeline = fill_pipeline;

        if global_data.state.command_line.is_some() {
            crate::tui::command_line::render_command_line(
                &mut pipeline,
                window_size,
                &mut global_data.state,
            );
        } else {
            crate::tui::app::status_bar::render_status_bar(
                &mut pipeline,
                window_size,
                &global_data.state,
                &global_data.state.theme,
            );
        }

        global_data.pipeline = pipeline;
    });
}

fn create_stylesheet(theme: &crate::tui::theme::HelixTheme) -> CommonResult<TuiStylesheet> {
    let bg = theme
        .ui_bg("ui.background")
        .unwrap_or(crate::tui::widgets::title_row::DEFAULT_BG);
    throws_with_return!({
        let mut styles = tui_stylesheet! {
            new_style!(
                id: {crate::tui::pane_component::container_id()}
                color_bg: {TuiColor::from(bg)}
            )
        };
        for col_idx in 0..crate::tui::pane_manager::MAX_PANES {
            let id = FlexBoxId::new(100 + col_idx as u8);
            styles.add_style(new_style!(
                id: {id}
                padding: {0}
                color_bg: {TuiColor::from(bg)}
            ))?;
        }
        for slot in 0..crate::tui::pane_manager::MAX_PANES {
            let id = crate::tui::pane_component::pane_id(slot);
            styles.add_style(new_style!(
                id: {id}
                padding: {0}
                color_bg: {TuiColor::from(bg)}
            ))?;
        }
        styles
    })
}
