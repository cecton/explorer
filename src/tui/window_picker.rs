use crate::loader::{FileKey, LoadedFile};
use crate::tui::*;
use camino::Utf8PathBuf;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;

pub(crate) type PickerResultMsg = (u64, Vec<(Window, Vec<u32>)>);

pub struct WindowPickerComponent {
    id: FlexBoxId,
    picker: FuzzyPicker,
    input_line: InputLine,
    generation: Arc<AtomicU64>,
    results_tx: mpsc::Sender<PickerResultMsg>,
}

impl WindowPickerComponent {
    pub(crate) fn new(
        id: FlexBoxId,
        results_tx: mpsc::Sender<PickerResultMsg>,
        generation: Arc<AtomicU64>,
    ) -> Self {
        Self {
            id,
            picker: FuzzyPicker::new(),
            input_line: InputLine::new(),
            generation,
            results_tx,
        }
    }

    fn on_query_changed(
        &self,
        state: &AppState,
        main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
    ) {
        Self::spawn_match(
            state,
            Arc::clone(&self.generation),
            self.results_tx.clone(),
            main_tx,
        );
    }

    pub(crate) fn spawn_match(
        state: &AppState,
        generation: Arc<AtomicU64>,
        results_tx: mpsc::Sender<PickerResultMsg>,
        main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
    ) {
        let query = state.window_picker.query.clone();
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
                send_signal!(
                    main_tx,
                    TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::Noop)
                );
            }
        });
    }

    pub(crate) fn open_windows_results(
        files: &[Arc<LoadedFile>],
        window_stack: &[Window],
    ) -> Vec<(Window, Vec<u32>)> {
        let mut seen: HashSet<Window> = HashSet::new();
        let mut results = Vec::new();
        for window in window_stack {
            let listed = match window {
                Window::FilePreview(key) => !files[key.0].removed.load(Ordering::Relaxed),
                Window::Terminal(_)
                | Window::GitLog(_)
                | Window::CommitDiff(_)
                | Window::ConversationTree
                | Window::Discussion(_) => true,
                Window::WindowPicker | Window::ThemePicker | Window::ModelPicker => false,
            };
            if listed && seen.insert(*window) {
                results.push((*window, vec![]));
            }
        }
        results
    }
}

impl TitleRow for WindowPickerComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        theme: &HelixTheme,
        state: &AppState,
    ) -> usize {
        let origin = pane_box.style_adjusted_origin_pos;
        let width = pane_box.style_adjusted_bounds_size.col_width.as_u16();
        let query = &state.window_picker.query;
        let max_rows = (pane_box.style_adjusted_bounds_size.row_height.as_usize() / 2).max(1);
        let height = InputLine::height(query, None, "", width, max_rows);

        let (bg_rgb, fg_rgb) = title_bar_colors(focused, theme);
        let color_bg = TuiColor::from(bg_rgb);
        let color_fg = TuiColor::from(fg_rgb);
        let bg_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});

        for row_offset in 0..height {
            *ops +=
                RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(row_offset as u16));
            *ops += RenderOpCommon::SetBgColor(color_bg);
            *ops += RenderOpIR::PaintTextWithAttributes(
                " ".repeat(width as usize).as_str().into(),
                Some(bg_style),
            );
        }
        self.input_line.render(
            ops,
            query,
            None,
            "",
            origin,
            width,
            height,
            focused,
            (color_bg, color_fg),
        );

        height
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
        let windows: Vec<Window> = results.into_iter().map(|(w, _)| w).collect();
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
/// [`HelixTheme::color_for_scope`], which walks each scope's fallback chain and
/// always resolves to a real color. Keeps the candidates that differ from
/// `ui.text` and from each other, so these rows are never identical to file rows
/// or to another kind; the default blue, green and mauve stand in for what the
/// theme lacks. `constant` is intentionally excluded — it maps to the same color
/// the picker uses for fuzzy-match highlights.
fn row_accents(theme: &HelixTheme) -> RowAccents {
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
    query: &str,
    files: &[Arc<LoadedFile>],
    root: &Utf8PathBuf,
    window_stack: &[Window],
) -> Vec<(Window, Vec<u32>)> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);

    if pattern.atoms.is_empty() {
        return WindowPickerComponent::open_windows_results(files, window_stack);
    }

    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let mut buf = Vec::new();
    let mut scored: Vec<(FileKey, u32, Vec<u32>)> = files
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.removed.load(Ordering::Relaxed))
        .filter_map(|(i, file)| {
            let rel = file.path.strip_prefix(root).unwrap_or(&file.path);
            let haystack = Utf32Str::new(rel.as_str(), &mut buf);
            let mut indices = Vec::new();
            pattern
                .indices(haystack, &mut matcher, &mut indices)
                .map(|score| {
                    indices.sort_unstable();
                    indices.dedup();
                    (FileKey(i), score, indices)
                })
        })
        .collect();
    scored.sort_by_key(|&(_, score, _)| std::cmp::Reverse(score));
    scored
        .into_iter()
        .map(|(key, _, idx)| (Window::FilePreview(key), idx))
        .collect()
}

impl Component<AppState, AppSignal> for WindowPickerComponent {
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
        match &input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }) => {
                let state = &mut global_data.state;
                state.pane_manager.remove_window(&Window::WindowPicker);
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
                if let Some(&(window, _)) = state.window_picker.results.get(selected) {
                    let old = state.pane_manager.focused_window;
                    match window {
                        Window::FilePreview(key) => {
                            if !state.pane_manager.window_states.contains_key(&window) {
                                state.pane_manager.set_window_scroll(&window, 0);
                            }
                            state.pane_manager.push_window(window);
                            state.pane_manager.focused_window = Some(window);
                            crate::lsp::send_file_request(key.0);
                        }
                        Window::Terminal(id) => {
                            state.pane_manager.push_window(window);
                            state.pane_manager.focused_window = Some(window);
                            // Mirror click-to-focus: grab the keyboard unless the
                            // terminal is scrolled back into its history.
                            let scrolled = state
                                .with_terminal(id, |p| p.scroll_offset > 0)
                                .unwrap_or(false);
                            state.terminal_grabbed = !scrolled;
                            crate::tui::app::notify_terminal_focus_change(state, old, Some(window));
                        }
                        Window::GitLog(_)
                        | Window::CommitDiff(_)
                        | Window::ConversationTree
                        | Window::Discussion(_) => {
                            state.pane_manager.push_window(window);
                            state.pane_manager.focused_window = Some(window);
                        }
                        Window::WindowPicker | Window::ThemePicker | Window::ModelPicker => {}
                    }
                }
                state.pane_manager.remove_window(&Window::WindowPicker);
                state.window_picker.reset();
                state.mark_session_dirty();
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
                let Some(&(window, _)) = state.window_picker.results.get(index) else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                if !state.pane_manager.window_stack.contains(&window) {
                    return Ok(EventPropagation::ConsumedRender);
                }
                crate::tui::app::close_window(state, window);
                if state.window_picker.query.is_empty() {
                    state.window_picker.results.remove(index);
                    let results = &state.window_picker.results;
                    state.window_picker.selected = results
                        .get(index)
                        .or(results.last())
                        .map(|(window, _)| *window);
                }
                return Ok(EventPropagation::ConsumedRender);
            }
            _ => {}
        }

        if self
            .input_line
            .handle_key(&input_event, &mut global_data.state.window_picker.query)
        {
            if !global_data.state.window_picker.query.is_empty() {
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

            if total_rows == 0 {
                return Ok(());
            }

            // Windows open and close while the picker stays up; an empty query lists them.
            let state = &mut global_data.state;
            if state.window_picker.query.is_empty() {
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
                    Window::WindowPicker | Window::ThemePicker | Window::ModelPicker => {
                        (String::new(), None)
                    }
                },
            );
            let result_count = global_data.state.window_picker.results.len();

            global_data
                .state
                .pane_manager
                .set_window_scroll(&Window::WindowPicker, self.picker.scroll_offset);
            global_data
                .state
                .pane_manager
                .set_window_scroll_max(&Window::WindowPicker, result_count);
            global_data
                .state
                .pane_manager
                .set_window_page_size(&Window::WindowPicker, total_rows);

            global_data.pipeline.push(ZOrder::Normal, result_ops);
        });
    }
}
