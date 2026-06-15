use super::app::Id;
use crate::tui::*;

pub(super) struct PanesRenderer<'a> {
    pub(super) visible: &'a [PaneSlot],
}

impl SurfaceRender<AppState, AppSignal> for PanesRenderer<'_> {
    fn render_in_surface(
        &mut self,
        surface: &mut Surface,
        global_data: &mut GlobalData<AppState, AppSignal>,
        component_registry_map: &mut ComponentRegistryMap<AppState, AppSignal>,
        has_focus: &mut HasFocus,
    ) -> CommonResult<()> {
        throws!({
            const COLUMN_ID_BASE: u8 = 100;
            let container_id = FlexBoxId::from(Id::Container);
            box_start!(
                in: surface,
                id: container_id,
                dir: LayoutDirection::Horizontal,
                requested_size_percent: req_size_pc!(width: 100, height: 100),
                styles: [container_id],
            );

            let window_size = global_data.window_size;
            let surface_rows = window_size.row_height.as_u16().saturating_sub(1);
            let mut current_col_origin: Option<u16> = None;
            let mut col_idx = 0usize;
            let mut column_id;

            for slot in self.visible {
                let slot_origin_col = slot.box_.style_adjusted_origin_pos.col_index.as_u16();
                let slot_width = slot.box_.style_adjusted_bounds_size.col_width.as_u16();
                let slot_height = slot.box_.style_adjusted_bounds_size.row_height.as_u16();

                if current_col_origin != Some(slot_origin_col) {
                    if current_col_origin.is_some() {
                        box_end!(in: surface);
                    }
                    current_col_origin = Some(slot_origin_col);
                    column_id = FlexBoxId::new(COLUMN_ID_BASE + col_idx as u8);
                    col_idx += 1;
                    let width_pc =
                        (slot_width as i32 * 100).div_euclid(window_size.col_width.as_u16() as i32);
                    box_start!(
                        in: surface,
                        id: column_id,
                        dir: LayoutDirection::Vertical,
                        requested_size_percent: req_size_pc!(width: {width_pc}, height: 100),
                        styles: [column_id],
                    );
                }

                let pane_id = FlexBoxId::from(Id::pane(slot.slot));
                let height_pc = ((slot_height as i32 * 100) + surface_rows as i32 - 1)
                    .div_euclid(surface_rows as i32)
                    .max(1);
                box_start!(
                    in: surface,
                    id: pane_id,
                    dir: LayoutDirection::Vertical,
                    requested_size_percent: req_size_pc!(width: 100, height: {height_pc}),
                    styles: [pane_id],
                );
                render_component_in_current_box!(
                    in: surface,
                    component_id: pane_id,
                    from: component_registry_map,
                    global_data: global_data,
                    has_focus: has_focus
                );
                box_end!(in: surface);
            }

            if current_col_origin.is_some() {
                box_end!(in: surface);
            }
            box_end!(in: surface);
        });
    }
}
