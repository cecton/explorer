#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

pub struct CommitDiffPaneComponent {
    id: FlexBoxId,
}

impl CommitDiffPaneComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self { id }
    }

    fn active_commit(&self, state: &crate::tui::state::AppState) -> Option<[u8; 20]> {
        use crate::tui::pane_manager::Window;
        let slot = crate::tui::pane_component::pane_slot(self.id)?;
        match state.pane_manager.slot_window(slot)? {
            Window::CommitDiff(id) => Some(id),
            _ => None,
        }
    }

    pub(crate) fn title(state: &crate::tui::state::AppState, id: [u8; 20]) -> String {
        let short = crate::git::short_id(&id);
        match state.commit_diffs.get(&id) {
            Some(diff) if !diff.summary.is_empty() => format!("commit {short} {}", diff.summary),
            _ => format!("commit {short}"),
        }
    }

    fn kind_colors(theme: &crate::tui::theme::HelixTheme) -> DiffColors {
        DiffColors {
            file: theme.color_for_scope("function"),
            hunk: theme.color_for_scope("diff.delta"),
            add: theme.color_for_scope("diff.plus"),
            del: theme.color_for_scope("diff.minus"),
            context: theme.ui.text,
        }
    }
}

/// The colors of the diff row kinds, resolved once per frame.
struct DiffColors {
    file: crate::tui::theme::Color,
    hunk: crate::tui::theme::Color,
    add: crate::tui::theme::Color,
    del: crate::tui::theme::Color,
    context: crate::tui::theme::Color,
}

impl DiffColors {
    fn of(&self, kind: crate::git::DiffKind) -> crate::tui::theme::Color {
        use crate::git::DiffKind;
        match kind {
            DiffKind::FileHeader | DiffKind::File => self.file,
            DiffKind::Hunk => self.hunk,
            DiffKind::Add => self.add,
            DiffKind::Del => self.del,
            DiffKind::Context => self.context,
        }
    }
}

/// Keeps a loaded diff only while a window still shows its commit.
pub(crate) fn diff_loaded(
    state: &mut crate::tui::state::AppState,
    id: [u8; 20],
    result: &Result<(String, std::sync::Arc<Vec<crate::git::DiffLine>>), String>,
) {
    use crate::tui::pane_manager::Window;
    let still_open = state
        .pane_manager
        .window_stack
        .iter()
        .any(|w| matches!(w, Window::CommitDiff(other) if *other == id));
    if !still_open {
        return;
    }
    let entry = state.commit_diffs.entry(id).or_default();
    match result {
        Ok((summary, lines)) => {
            entry.summary.clone_from(summary);
            entry.lines = Some(std::sync::Arc::clone(lines));
            entry.error = None;
        }
        Err(e) => entry.error = Some(e.clone()),
    }
}

impl CommitDiffPaneComponent {
    pub(crate) fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        state: &mut crate::tui::state::AppState,
    ) -> usize {
        let theme = &state.theme;
        let Some(id) = self.active_commit(state) else {
            return 0;
        };
        crate::tui::widgets::title_row::render_pane_title(
            ops,
            pane_box,
            &Self::title(state, id),
            false,
            theme,
            focused,
        );
        1
    }
}

impl Component<crate::tui::state::AppState, crate::signal::AppSignal> for CommitDiffPaneComponent {
    fn reset(&mut self) {}

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        input_event: InputEvent,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        use crate::tui::pane_manager::Window;
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
                crate::tui::focus::close_window(state, window);
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

            let consumed = state.pane_manager.scroll_for_input(&window, &input_event);
            if consumed {
                EventPropagation::ConsumedRender
            } else {
                EventPropagation::Propagate
            }
        });
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        current_box: FlexBox,
        _surface_bounds: VPBoundingBox,
        _has_focus: &mut HasFocus,
    ) -> CommonResult {
        use crate::tui::pane_manager::Window;
        throws!({
            let origin = current_box.style_adjusted_bounds.origin_pos;
            let bounds = current_box.style_adjusted_bounds.bounds_size;
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

            let pane_bg = state.theme.ui.background;
            let bg_style = state.theme.ui.text.on(pane_bg);

            let mut render_ops = RenderOpIRVec::new();
            match (&lines, &error) {
                (_, Some(err)) => {
                    let fg = state.theme.ui.error;
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
                    let fg = state.theme.ui.text;
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
                    let fg = state.theme.ui.text;
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
                    let colors = Self::kind_colors(&state.theme);
                    for row_offset in 0..total_rows {
                        render_ops += RenderOpCommon::MoveCursorPositionRelTo(
                            origin,
                            vp_col(0) + vp_row(row_offset as u16),
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
                            vp_col(0) + vp_row(row_offset as u16),
                        );
                        let fg = colors.of(line.kind);
                        let style = fg.on(pane_bg);
                        let text = diff_row_text(&line.text, pane_width);
                        render_ops += RenderOpCommon::ApplyColors(Some(style));
                        render_ops += RenderOpIR::PaintTextWithAttributes(text.into(), Some(style));
                    }
                }
            }

            global_data.pipeline.push(ZOrder::Normal, render_ops);
        });
    }
}

/// The part of a diff line that fits a pane `pane_width` columns wide, one column short of
/// its edge.
fn diff_row_text(text: &str, pane_width: usize) -> String {
    crate::tui::widgets::text_layout::truncate(text, pane_width.saturating_sub(1))
}

fn paint_centered(
    mut render_ops: &mut RenderOpIRVec,
    origin: &VPPos,
    pane_width: usize,
    total_rows: usize,
    text: &str,
    fg: crate::tui::theme::Color,
    pane_bg: crate::tui::theme::Color,
) {
    let text_width = crate::tui::widgets::text_layout::display_width(text);
    let pad = pane_width.saturating_sub(text_width) / 2;
    let padded = format!("{:pad$}{}", "", text, pad = pad);
    let style = fg.on(pane_bg);
    for row_offset in 0..total_rows {
        render_ops +=
            RenderOpCommon::MoveCursorPositionRelTo(*origin, vp_col(0) + vp_row(row_offset as u16));
        render_ops += RenderOpCommon::ApplyColors(Some(style));
        render_ops +=
            RenderOpIR::PaintTextWithAttributes(" ".repeat(pane_width).into(), Some(style));
    }
    render_ops += RenderOpCommon::MoveCursorPositionRelTo(*origin, vp_col(0) + vp_row(0));
    render_ops += RenderOpCommon::ApplyColors(Some(style));
    render_ops += RenderOpIR::PaintTextWithAttributes(padded.into(), Some(style));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CJK diff line is cut by columns, a column short of the pane's edge.
    #[test]
    fn a_wide_diff_line_never_reaches_the_edge_of_the_pane() {
        for pane_width in 1..20 {
            let text = diff_row_text("+抵抵抵抵抵抵抵抵抵抵", pane_width);
            assert!(
                crate::tui::widgets::text_layout::display_width(&text)
                    <= pane_width.saturating_sub(1),
                "{pane_width}: {text:?}"
            );
        }
    }

    #[test]
    fn a_diff_title_shows_its_summary_without_a_log_window() {
        use crate::tui::state::AppState;
        use crate::tui::state::CommitDiffState;
        let mut state = AppState::default();
        let id = [7u8; 20];
        let short = crate::git::short_id(&id);
        assert_eq!(
            CommitDiffPaneComponent::title(&state, id),
            format!("commit {short}")
        );
        state.commit_diffs.insert(
            id,
            CommitDiffState {
                summary: "fix: a bug".to_string(),
                ..Default::default()
            },
        );
        assert_eq!(
            CommitDiffPaneComponent::title(&state, id),
            format!("commit {short} fix: a bug")
        );
    }
}
