#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) type PickerResultMsg = (u64, Vec<crate::tui::pane_manager::Window>);

pub struct WindowPickerComponent {
    id: FlexBoxId,
    picker: crate::tui::widgets::fuzzy_picker::FuzzyPicker,
    generation: Arc<AtomicU64>,
    results_tx: tokio::sync::mpsc::Sender<PickerResultMsg>,
}

impl WindowPickerComponent {
    pub(crate) fn new(
        id: FlexBoxId,
        results_tx: tokio::sync::mpsc::Sender<PickerResultMsg>,
        generation: Arc<AtomicU64>,
    ) -> Self {
        use crate::tui::widgets::fuzzy::Kind;
        use crate::tui::widgets::fuzzy_picker::FuzzyPicker;
        Self {
            id,
            picker: FuzzyPicker::new(Kind::Paths),
            generation,
            results_tx,
        }
    }

    fn on_query_changed(
        &self,
        state: &crate::tui::state::AppState,
        main_tx: crate::signal::MainTx,
    ) {
        Self::spawn_match(
            state,
            Arc::clone(&self.generation),
            self.results_tx.clone(),
            main_tx,
        );
    }

    pub(crate) fn spawn_match(
        state: &crate::tui::state::AppState,
        generation: Arc<AtomicU64>,
        results_tx: tokio::sync::mpsc::Sender<PickerResultMsg>,
        main_tx: crate::signal::MainTx,
    ) {
        use crate::signal::AppSignal;
        let query = state.window_picker.query.text().to_string();
        let files = Arc::clone(&state.files);
        let root = state.root.clone();
        let window_stack = state.pane_manager.window_stack.clone();
        let current_generation = generation.fetch_add(1, Ordering::Relaxed) + 1;
        let gen_counter = Arc::clone(&generation);
        tokio::task::spawn_blocking(move || {
            let snapshot = files.load_full();
            let results = run_file_name_match(&query, &snapshot, &root, &window_stack);
            if gen_counter.load(Ordering::Relaxed) == current_generation {
                let _ = results_tx.try_send((current_generation, results));
                crate::signal::send(&main_tx, AppSignal::Redraw);
            }
        });
    }

    pub(crate) fn open_windows_results(
        files: &[Arc<crate::loader::LoadedFile>],
        window_stack: &[crate::tui::pane_manager::Window],
    ) -> Vec<crate::tui::pane_manager::Window> {
        use crate::tui::pane_manager::Window;
        let mut seen: HashSet<Window> = HashSet::new();
        let mut results = Vec::new();
        for window in window_stack {
            let listed = match window {
                Window::FilePreview(key) => !files[key.0].removed.load(Ordering::Relaxed),
                Window::Terminal(_)
                | Window::GitLog(_)
                | Window::CommitDiff(_)
                | Window::Report(_)
                | Window::ConversationTree
                | Window::Discussion(_) => true,
                Window::WindowPicker | Window::ThemePicker | Window::ModelPicker => false,
            };
            if listed && seen.insert(*window) {
                results.push(*window);
            }
        }
        results
    }
}

impl crate::tui::widgets::title_row::TitleRow for WindowPickerComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        state: &mut crate::tui::state::AppState,
    ) -> usize {
        crate::tui::widgets::title_row::render_input_title(
            ops,
            pane_box,
            &mut state.window_picker.query,
            "",
            focused,
            &state.theme,
            crate::tui::widgets::title_row::half_height(pane_box),
        )
    }
}

/// Foreground colors of the rows that are not files, by kind of window.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RowAccents {
    terminal: RgbValue,
    agent: RgbValue,
    git: RgbValue,
}

/// Accent colors for terminal, agent and git rows in the picker.
///
/// Pulls vivid, theme-defined syntax colors (preferring `function`) via
/// [`crate::tui::theme::HelixTheme::color_for_scope`], which walks each scope's fallback chain and
/// always resolves to a real color. Keeps the candidates that differ from
/// `ui.text` and from each other, so these rows are never identical to file rows
/// or to another kind; the default blue, green and mauve stand in for what the
/// theme lacks. `constant` is intentionally excluded — it maps to the same color
/// the picker uses for fuzzy-match highlights.
fn row_accents(theme: &crate::tui::theme::HelixTheme) -> RowAccents {
    let text = theme.ui_fg("ui.text");
    let defaults = [
        RgbValue::from_u8(137, 180, 250),
        RgbValue::from_u8(166, 227, 161),
        RgbValue::from_u8(203, 166, 247),
    ];
    let mut distinct: Vec<RgbValue> = Vec::with_capacity(3);
    for color in ["function", "string", "keyword", "type"]
        .into_iter()
        .filter_map(|scope| theme.color_for_scope(scope))
        .chain(defaults)
    {
        if Some(color) != text && !distinct.contains(&color) {
            distinct.push(color);
        }
    }
    let pick = |index: usize| distinct.get(index).copied().unwrap_or(defaults[index]);
    RowAccents {
        terminal: pick(0),
        agent: pick(1),
        git: pick(2),
    }
}

fn run_file_name_match(
    text: &str,
    files: &[Arc<crate::loader::LoadedFile>],
    root: &camino::Utf8PathBuf,
    window_stack: &[crate::tui::pane_manager::Window],
) -> Vec<crate::tui::pane_manager::Window> {
    use crate::loader::FileKey;
    use crate::tui::pane_manager::Window;
    use crate::tui::widgets::fuzzy::{Kind, Query};
    let mut query = Query::new(Kind::Paths);
    query.set(text);

    if query.is_empty() {
        return WindowPickerComponent::open_windows_results(files, window_stack);
    }

    let mut scored: Vec<(FileKey, u32)> = files
        .iter()
        .enumerate()
        .filter(|(_, file)| !file.removed.load(Ordering::Relaxed))
        .filter_map(|(i, file)| {
            let rel = file.path.strip_prefix(root).unwrap_or(&file.path);
            query.rank(rel.as_str()).map(|score| (FileKey(i), score))
        })
        .collect();
    scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
    scored
        .into_iter()
        .map(|(key, _)| Window::FilePreview(key))
        .collect()
}

impl Component<crate::tui::state::AppState, crate::signal::AppSignal> for WindowPickerComponent {
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
        match &input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }) => {
                let state = &mut global_data.state;
                crate::tui::focus::close_window(state, Window::WindowPicker);
                state.window_picker.reset();
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Enter),
            }) => {
                let state = &mut global_data.state;
                if state.window_picker.results.is_empty() {
                    return Ok(EventPropagation::ConsumedRender);
                }
                let selected = state.window_picker.resolve_selected_index();
                if let Some(&window) = state.window_picker.results.get(selected) {
                    if let Window::FilePreview(_) = window
                        && !state.pane_manager.window_states.contains_key(&window)
                    {
                        state.pane_manager.set_window_scroll(&window, 0);
                    }
                    if let Window::FilePreview(key) = window {
                        crate::tui::focus::focus_window(state, window);
                        crate::lsp::send_file_request(key.0);
                    } else if !matches!(
                        window,
                        Window::WindowPicker | Window::ThemePicker | Window::ModelPicker
                    ) {
                        crate::tui::focus::focus_window(state, window);
                    }
                }
                crate::tui::focus::close_window(state, Window::WindowPicker);
                state.window_picker.reset();
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('x'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        shift_key_state: KeyState::NotPressed,
                        alt_key_state: KeyState::NotPressed,
                    },
            }) => {
                let state = &mut global_data.state;
                let index = state.window_picker.resolve_selected_index();
                let Some(&window) = state.window_picker.results.get(index) else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                if !state.pane_manager.window_stack.contains(&window) {
                    return Ok(EventPropagation::ConsumedRender);
                }
                crate::tui::focus::close_window(state, window);
                if state.window_picker.query.text().is_empty() {
                    state.window_picker.results.remove(index);
                    let results = &state.window_picker.results;
                    state.window_picker.selected = results.get(index).or(results.last()).copied();
                }
                return Ok(EventPropagation::ConsumedRender);
            }
            _ => {}
        }

        if global_data
            .state
            .window_picker
            .query
            .handle_key(&input_event)
        {
            if !global_data.state.window_picker.query.text().is_empty() {
                let main_tx = global_data.main_thread_channel_sender.clone();
                self.on_query_changed(&global_data.state, main_tx);
            }
            return Ok(EventPropagation::ConsumedRender);
        }

        let page_size = global_data
            .state
            .pane_manager
            .window_page_size(&Window::WindowPicker);
        if let Some(result) = self.picker.handle_navigation(
            &input_event,
            page_size,
            &mut global_data.state.window_picker,
        ) {
            return Ok(result);
        }

        Ok(EventPropagation::Propagate)
    }

    fn render(
        &mut self,
        global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
        current_box: FlexBox,
        _surface_bounds: VPBoundingBox,
        _has_focus: &mut HasFocus,
    ) -> CommonResult {
        use crate::tui::pane_manager::Window;
        use crate::tui::panes::commit_diff_pane::CommitDiffPaneComponent;
        use crate::tui::panes::conversation_tree::ConversationTreeComponent;
        use crate::tui::panes::discussion::DiscussionComponent;
        use crate::tui::panes::git_log_pane::GitLogPaneComponent;
        use crate::tui::panes::report_pane::ReportComponent;
        throws!({
            let origin = current_box.style_adjusted_bounds.origin_pos;
            let bounds = current_box.style_adjusted_bounds.bounds_size;
            let total_rows = bounds.row_height.as_usize();
            let pane_width = bounds.col_width.as_usize();

            if total_rows == 0 {
                return Ok(());
            }

            // Windows open and close while the picker stays up; an empty query lists them.
            let state = &mut global_data.state;
            if state.window_picker.query.text().is_empty() {
                let snapshot = state.files.load();
                state.window_picker.results =
                    Self::open_windows_results(&snapshot, &state.pane_manager.window_stack);
            }

            let accents = row_accents(&global_data.state.theme);
            let result_ops = self.picker.render_results(
                &global_data.state,
                &global_data.state.window_picker,
                origin,
                total_rows,
                pane_width,
                |window, state| match window {
                    Window::FilePreview(key) => {
                        let snapshot = state.files.load_full();
                        let file = &snapshot[key.0];
                        let rel = file.path.strip_prefix(&state.root).unwrap_or(&file.path);
                        (rel.to_string(), None)
                    }
                    Window::Terminal(id) => {
                        let title = state
                            .with_terminal(*id, |p| p.title.clone())
                            .flatten()
                            .unwrap_or_else(|| format!("Terminal {id}"));
                        (title, Some(accents.terminal))
                    }
                    Window::GitLog(id) => {
                        let title = state
                            .git_logs
                            .get(id)
                            .map(GitLogPaneComponent::title)
                            .unwrap_or_default();
                        (title, Some(accents.git))
                    }
                    Window::CommitDiff(id) => (
                        CommitDiffPaneComponent::title(state, *id),
                        Some(accents.git),
                    ),
                    Window::ConversationTree => {
                        (ConversationTreeComponent::title(state), Some(accents.agent))
                    }
                    Window::Discussion(id) => {
                        (DiscussionComponent::title(state, *id), Some(accents.agent))
                    }
                    Window::Report(id) => (ReportComponent::title(state, *id), None),
                    Window::WindowPicker | Window::ThemePicker | Window::ModelPicker => {
                        (String::new(), None)
                    }
                },
            );
            let result_count = global_data.state.window_picker.results.len();

            self.picker.publish_geometry(
                &mut global_data.state.pane_manager,
                &Window::WindowPicker,
                result_count,
                total_rows,
            );

            global_data.pipeline.push(ZOrder::Normal, result_ops);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::LoadedFile;

    fn live_file(path: &str) -> Arc<LoadedFile> {
        let f = LoadedFile::stub(path.into());
        f.removed.store(false, Ordering::Relaxed);
        f
    }

    #[test]
    fn open_list_includes_every_window_but_pickers_in_stack_order() {
        use crate::loader::FileKey;
        use crate::tui::pane_manager::Window;
        // index 0: live file, index 1: removed file.
        let removed = LoadedFile::stub("gone.rs".into()); // stub() marks removed = true
        let files = vec![live_file("src/main.rs"), removed];

        // Stack front-to-back, with a duplicate terminal and the pickers.
        let stack = vec![
            Window::Discussion(2),
            Window::FilePreview(FileKey(0)),
            Window::Terminal(3),
            Window::WindowPicker,
            Window::ConversationTree,
            Window::ThemePicker,
            Window::GitLog(1),
            Window::ModelPicker,
            Window::CommitDiff([7; 20]),
            Window::FilePreview(FileKey(1)), // removed -> excluded
            Window::Terminal(3),             // duplicate -> deduped
        ];

        let results = WindowPickerComponent::open_windows_results(&files, &stack);
        let windows: Vec<Window> = results;
        assert_eq!(
            windows,
            vec![
                Window::Discussion(2),
                Window::FilePreview(FileKey(0)),
                Window::Terminal(3),
                Window::ConversationTree,
                Window::GitLog(1),
                Window::CommitDiff([7; 20]),
            ],
        );
    }

    #[test]
    fn row_accents_are_distinct_and_theme_sourced() {
        use crate::tui::theme::HelixTheme;
        let theme = HelixTheme::default();
        let accents = row_accents(&theme);
        // Never identical to the file-row (ui.text) color, nor to each other.
        for accent in [accents.terminal, accents.agent, accents.git] {
            assert_ne!(Some(accent), theme.ui_fg("ui.text"));
        }
        assert_ne!(accents.terminal, accents.agent);
        assert_ne!(accents.terminal, accents.git);
        assert_ne!(accents.agent, accents.git);
        // On the default theme terminals keep the `function` scope color.
        assert_eq!(Some(accents.terminal), theme.color_for_scope("function"));
    }
}
