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
    use crate::tui::modal::Modal;
    crate::tui::app::sync_has_focus(&global_data.state, has_focus);

    let mut best_generation = 0u64;
    let mut best_results = None;
    while let Ok((arrived_generation, results)) = app.picker_results_rx.try_recv() {
        if arrived_generation > best_generation {
            best_generation = arrived_generation;
            best_results = Some(results);
        }
    }
    // A match started before the picker closed can land after it: it belongs to a picker
    // that is no longer in front, and the list it would overwrite is not on screen.
    if let Some(results) = best_results
        && matches!(global_data.state.modal, Some(Modal::WindowPicker))
    {
        global_data.state.window_picker.results = results;
    }

    crate::tui::panes::terminal::spawn::drain_exits(
        &mut app.terminal_event_rx,
        &mut global_data.state,
    );

    throws!({
        let window_size = global_data.window_size;
        let surface_size = crate::tui::app::surface_size(window_size);
        // One layout for the frame: every pane below reads its window and its box from it.
        global_data.state.pane_manager.relayout(surface_size);
        let visible = global_data.state.pane_manager.visible();

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

        render_panes(global_data, component_registry_map, has_focus, surface_size)?;

        // Take the pipeline (components have already pushed into it) and add
        // background fill and status bar on top.
        let mut pipeline = std::mem::take(&mut global_data.pipeline);

        // Fill entire surface area with pane background (covers padding
        // between panes, which the FlexBox layout system does not fill).
        let bg = global_data.state.theme.ui.background;
        let bg_style = global_data.state.theme.ui.text.on(bg);
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

        // A modal is painted last, at a layer of its own: every pane and the status bar have
        // already painted the cells it stands on, and its box covers them.
        if let Some(modal_ops) = crate::tui::modal::render(
            &mut global_data.state,
            &mut app.modal_painters,
            surface_size,
        ) {
            pipeline.push(ZOrder::High, modal_ops);
        }

        global_data.pipeline = pipeline;
    });
}

/// Paints every visible pane in the box `relayout` gave it, the one focus-follows-mouse and
/// the mouse hit tests read; each pane pushes its ops into `global_data.pipeline`.
fn render_panes(
    global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
    component_registry_map: &mut ComponentRegistryMap<
        crate::tui::state::AppState,
        crate::signal::AppSignal,
    >,
    has_focus: &mut HasFocus,
    surface_size: VPSize,
) -> CommonResult<()> {
    let surface_bounds = VPBoundingBox::new(vp_col(0) + vp_row(0), surface_size);
    let boxes: Vec<(usize, FlexBox)> = global_data
        .state
        .pane_manager
        .visible()
        .iter()
        .map(|slot| (slot.slot, slot.box_))
        .collect();
    for (slot, box_) in boxes {
        let pane_id = crate::tui::pane_component::pane_id(slot);
        if let Some(component) =
            ComponentRegistry::try_to_get_component_by_id(component_registry_map, pane_id)
        {
            component.render(global_data, box_, surface_bounds, has_focus)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::pane_manager::Window;

    /// On 300 columns, three columns of panes are 100 wide: each paints where `relayout`
    /// put it, not in the percentage boxes r3bl truncated to 0, 99 and 198.
    #[test]
    fn panes_paint_in_the_boxes_relayout_computed() {
        use crate::loader::FileKey;
        use crate::tui::state::AppState;
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<_> = ["x", "y", "z"]
            .iter()
            .map(|name| {
                let path = dir.path().join(format!("{name}.txt"));
                std::fs::write(&path, format!("{name}\n")).unwrap();
                crate::loader::LoadedFile::load(path).unwrap()
            })
            .collect();
        let mut state =
            AppState::for_test_with_files(files, camino::Utf8PathBuf::from("/tmp/nowhere"));
        for key in 0..3 {
            state
                .pane_manager
                .push_window(Window::FilePreview(FileKey(key)));
        }
        let size = vp_width(300) + vp_height(24);
        state.pane_manager.relayout(size);
        // The newest window opens at the left: read the names off the layout.
        let names: Vec<char> = state
            .pane_manager
            .visible()
            .iter()
            .map(|slot| match slot.window {
                Window::FilePreview(FileKey(key)) => ['x', 'y', 'z'][key],
                other => panic!("unexpected window {other:?}"),
            })
            .collect();
        let origins: Vec<u16> = state
            .pane_manager
            .visible()
            .iter()
            .map(|slot| {
                slot.box_
                    .style_adjusted_bounds
                    .origin_pos
                    .col_index
                    .as_u16()
            })
            .collect();
        assert_eq!(origins, [0, 100, 200], "the layout under test");

        let mut global_data = crate::tui::test_support::global_data(state);
        let mut registry = ComponentRegistryMap::default();
        for slot in 0..3 {
            let id = crate::tui::pane_component::pane_id(slot);
            ComponentRegistry::put(
                &mut registry,
                id,
                crate::tui::pane_component::PaneComponent::new_boxed(slot, id),
            );
        }
        render_panes(
            &mut global_data,
            &mut registry,
            &mut HasFocus::default(),
            size,
        )
        .unwrap();

        let mut ofs_buf = OfsBuf::new(Flat2DArray::new_empty(size, PixelChar::Spacer));
        global_data.pipeline.compose_render_ops_into_ofs_buf(
            size,
            &mut ofs_buf,
            &mut std::default::Default::default(),
        );
        // The text of the first line starts 3 cells past the box, on the row under the title.
        let row = ofs_buf.get_row(vp_row(1)).expect("a painted row");
        for (origin, name) in origins.iter().zip(names) {
            let cell = row[*origin as usize + 3];
            assert!(
                matches!(cell, PixelChar::PlainText { display_char, .. } if display_char == name),
                "pane {name} starts at column {origin}: {cell:?}"
            );
        }
    }
}
