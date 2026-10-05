use crate::git::{DiffKind, short_id};
use crate::tui::pane_component::pane_slot;
use crate::tui::*;

pub struct CommitDiffPaneComponent {
    id: FlexBoxId,
}

impl CommitDiffPaneComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self { id }
    }

    fn active_commit(&self, state: &AppState) -> Option<[u8; 20]> {
        let slot = pane_slot(self.id)?;
        match state.pane_manager.window_stack.get(slot)? {
            Window::CommitDiff(id) => Some(*id),
            _ => None,
        }
    }

    fn commit_summary(state: &AppState, id: [u8; 20]) -> Option<String> {
        state
            .git_logs
            .values()
            .filter_map(|win| win.entries.as_ref())
            .flat_map(|history| history.entries.iter())
            .find(|entry| entry.id == id)
            .map(|entry| entry.summary.clone())
    }

    fn kind_color(theme: &HelixTheme, kind: DiffKind) -> RgbValue {
        let fallback = |r: u8, g: u8, b: u8| RgbValue::from_u8(r, g, b);
        match kind {
            DiffKind::FileHeader => theme
                .color_for_scope("function")
                .unwrap_or(fallback(137, 180, 250)),
            DiffKind::File => theme
                .color_for_scope("function")
                .unwrap_or(fallback(110, 130, 170)),
            DiffKind::Hunk => theme
                .color_for_scope("diff.delta")
                .unwrap_or(fallback(250, 200, 90)),
            DiffKind::Add => theme
                .color_for_scope("diff.plus")
                .unwrap_or(fallback(150, 220, 120)),
            DiffKind::Del => theme
                .color_for_scope("diff.minus")
                .unwrap_or(fallback(230, 120, 120)),
            DiffKind::Context => theme.ui_fg("ui.text").unwrap_or(fallback(170, 170, 200)),
        }
    }
}

impl TitleRow for CommitDiffPaneComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        theme: &HelixTheme,
        state: &AppState,
    ) -> usize {
        let Some(id) = self.active_commit(state) else {
            return 0;
        };
        let short = short_id(&id);
        let title = match Self::commit_summary(state, id) {
            Some(summary) => format!("commit {short} {summary}"),
            None => format!("commit {short}"),
        };
        render_pane_title(ops, pane_box, &title, false, theme, focused);
        1
    }
}

impl Component<AppState, AppSignal> for CommitDiffPaneComponent {
    fn reset(&mut self) {}

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        input_event: InputEvent,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        throws_with_return!({
            let Some(id) = self.active_commit(&global_data.state) else {
                return Ok(EventPropagation::Propagate);
            };
            let window = Window::CommitDiff(id);
            let state = &mut global_data.state;

            if let InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }) = input_event
            {
                state.pane_manager.remove_window(&window);
                state.commit_diffs.remove(&id);
                state.mark_session_dirty();
                return Ok(EventPropagation::ConsumedRender);
            }

            if let InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Backspace),
            }) = input_event
            {
                state.pane_manager.send_to_back(&window);
                state.mark_session_dirty();
                return Ok(EventPropagation::ConsumedRender);
            }

            let mut consumed = false;
            if let InputEvent::Keyboard(KeyPress::Plain { key: kb_key }) = input_event {
                match kb_key {
                    Key::SpecialKey(SpecialKey::PageUp) => {
                        consumed = true;
                        let page = state.pane_manager.window_page_size(&window);
                        let current = state.pane_manager.window_scroll(&window);
                        state
                            .pane_manager
                            .set_window_scroll(&window, current.saturating_sub(page));
                        state.pane_manager.clamp_scroll(&window);
                    }
                    Key::SpecialKey(SpecialKey::PageDown) => {
                        consumed = true;
                        let page = state.pane_manager.window_page_size(&window);
                        let current = state.pane_manager.window_scroll(&window);
                        state
                            .pane_manager
                            .set_window_scroll(&window, current.saturating_add(page));
                        state.pane_manager.clamp_scroll(&window);
                    }
                    Key::SpecialKey(SpecialKey::Up) => {
                        consumed = true;
                        let current = state.pane_manager.window_scroll(&window);
                        state
                            .pane_manager
                            .set_window_scroll(&window, current.saturating_sub(1));
                        state.pane_manager.clamp_scroll(&window);
                    }
                    Key::SpecialKey(SpecialKey::Down) => {
                        consumed = true;
                        let current = state.pane_manager.window_scroll(&window);
                        state
                            .pane_manager
                            .set_window_scroll(&window, current.saturating_add(1));
                        state.pane_manager.clamp_scroll(&window);
                    }
                    Key::SpecialKey(SpecialKey::Home) => {
                        consumed = true;
                        state.pane_manager.set_window_scroll(&window, 0);
                    }
                    Key::SpecialKey(SpecialKey::End) => {
                        consumed = true;
                        let max = state.pane_manager.window_scroll_max(&window);
                        state.pane_manager.set_window_scroll(&window, max);
                        state.pane_manager.clamp_scroll(&window);
                    }
                    _ => {}
                }
            }
            if consumed {
                EventPropagation::ConsumedRender
            } else {
                EventPropagation::Propagate
            }
        });
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<AppState, AppSignal>,
        current_box: FlexBox,
        _surface_bounds: SurfaceBounds,
        _has_focus: &mut HasFocus,
    ) -> CommonResult {
        throws!({
            let origin = current_box.style_adjusted_origin_pos;
            let bounds = current_box.style_adjusted_bounds_size;
            let total_rows = bounds.row_height.as_usize();
            let pane_width = bounds.col_width.as_usize();

            let Some(id) = self.active_commit(&global_data.state) else {
                return Ok(());
            };
            let window = Window::CommitDiff(id);
            let state = &mut global_data.state;
            state.pane_manager.set_window_page_size(&window, total_rows);

            let diff = state.commit_diffs.get(&id).cloned();
            let lines = diff.as_ref().and_then(|d| d.lines.clone());
            let error = diff.as_ref().and_then(|d| d.error.clone());

            let line_count = lines.as_ref().map_or(0, |l| l.len());
            state
                .pane_manager
                .set_window_scroll_max(&window, line_count);
            state.pane_manager.clamp_scroll(&window);
            let scroll = state.pane_manager.window_scroll(&window);

            let pane_bg = state.theme.ui_bg("ui.background").unwrap_or(DEFAULT_BG);
            let bg = TuiColor::from(pane_bg);
            let bg_style = new_style!(color_bg: {bg});

            let mut render_ops = RenderOpIRVec::new();
            match (&lines, &error) {
                (_, Some(err)) => {
                    let fg = state
                        .theme
                        .ui_fg("error")
                        .unwrap_or(RgbValue::from_u8(220, 80, 80));
                    let text = format!("error: {err}");
                    paint_centered(
                        &mut render_ops,
                        &origin,
                        pane_width,
                        total_rows,
                        &text,
                        fg,
                        pane_bg,
                    );
                }
                (None, _) => {
                    let fg = state
                        .theme
                        .ui_fg("ui.text")
                        .unwrap_or(RgbValue::from_u8(170, 170, 200));
                    paint_centered(
                        &mut render_ops,
                        &origin,
                        pane_width,
                        total_rows,
                        "Loading diff…",
                        fg,
                        pane_bg,
                    );
                }
                (Some(lines), _) if lines.is_empty() => {
                    let fg = state
                        .theme
                        .ui_fg("ui.text")
                        .unwrap_or(RgbValue::from_u8(170, 170, 200));
                    paint_centered(
                        &mut render_ops,
                        &origin,
                        pane_width,
                        total_rows,
                        "(no diff)",
                        fg,
                        pane_bg,
                    );
                }
                (Some(lines), _) => {
                    for row_offset in 0..total_rows {
                        render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                            origin,
                            col(0) + row(row_offset as u16),
                        );
                        render_ops += RenderOpCommon::ApplyColors(Some(bg_style));
                        render_ops += RenderOpIR::PaintTextWithAttributes(
                            " ".repeat(pane_width).into(),
                            Some(bg_style),
                        );
                        let line_idx = scroll + row_offset;
                        let Some(line) = lines.get(line_idx) else {
                            continue;
                        };
                        render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                            origin,
                            col(0) + row(row_offset as u16),
                        );
                        let fg = Self::kind_color(&state.theme, line.kind);
                        let style = new_style!(color_fg: {TuiColor::from(fg)} color_bg: {bg});
                        let mut text = line.text.clone();
                        if text.chars().count() > pane_width.saturating_sub(1) {
                            text = text.chars().take(pane_width.saturating_sub(1)).collect();
                        }
                        render_ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
                    }
                }
            }

            global_data.pipeline.push(ZOrder::Normal, render_ops);
        });
    }
}

fn paint_centered(
    mut render_ops: &mut RenderOpIRVec,
    origin: &Pos,
    pane_width: usize,
    total_rows: usize,
    text: &str,
    fg: RgbValue,
    pane_bg: RgbValue,
) {
    let text_width = GCStringOwned::from(text).display_width().as_usize();
    let pad = pane_width.saturating_sub(text_width) / 2;
    let padded = format!("{:pad$}{}", "", text, pad = pad);
    let style = new_style!(color_fg: {TuiColor::from(fg)} color_bg: {TuiColor::from(pane_bg)});
    let bg_style = new_style!(color_bg: {TuiColor::from(pane_bg)});
    for row_offset in 0..total_rows {
        render_ops +=
            RenderOpCommon::MoveCursorPositionRelTo(*origin, col(0) + row(row_offset as u16));
        render_ops += RenderOpCommon::ApplyColors(Some(bg_style));
        render_ops +=
            RenderOpIR::PaintTextWithAttributes(" ".repeat(pane_width).into(), Some(bg_style));
    }
    render_ops += RenderOpCommon::MoveCursorPositionRelTo(*origin, col(0) + row(0));
    render_ops += RenderOpCommon::ApplyColors(Some(style));
    render_ops += RenderOpIR::PaintTextWithAttributes(padded.into(), Some(style));
}
