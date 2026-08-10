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

pub struct FileNamePickerComponent {
    id: FlexBoxId,
    picker: FuzzyPicker,
    input_line: InputLine,
    generation: Arc<AtomicU64>,
    results_tx: mpsc::Sender<PickerResultMsg>,
}

impl FileNamePickerComponent {
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
        let query = state.file_name_picker.query.clone();
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

    pub(crate) fn open_previews_results(
        files: &[Arc<LoadedFile>],
        window_stack: &[Window],
    ) -> Vec<(Window, Vec<u32>)> {
        let mut seen: HashSet<Window> = HashSet::new();
        let mut results = Vec::new();
        for window in window_stack {
            match window {
                Window::FilePreview(key)
                    if !files[key.0].removed.load(Ordering::Relaxed) && seen.insert(*window) =>
                {
                    results.push((*window, vec![]));
                }
                Window::Terminal(_) if seen.insert(*window) => {
                    results.push((*window, vec![]));
                }
                _ => {}
            }
        }
        results
    }

    pub(crate) fn compute_results(state: &AppState) -> Vec<(Window, Vec<u32>)> {
        let snapshot = state.files.load();
        if state.file_name_picker.query.is_empty() {
            Self::open_previews_results(&snapshot, &state.pane_manager.window_stack)
        } else {
            run_file_name_match(
                &state.file_name_picker.query,
                &snapshot,
                &state.root,
                &state.pane_manager.window_stack,
            )
        }
    }
}

impl TitleRow for FileNamePickerComponent {
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
        let query = &state.file_name_picker.query;
        let height = InputLine::line_count(query);

        let (bg_rgb, fg_rgb) = title_bar_colors(focused, theme);
        let color_bg = tui_color!(bg_rgb[0], bg_rgb[1], bg_rgb[2]);
        let color_fg = tui_color!(fg_rgb[0], fg_rgb[1], fg_rgb[2]);
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
        self.input_line
            .render(ops, query, "", origin, width, focused, (color_bg, color_fg));

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
    fn open_list_includes_terminals_and_files_in_stack_order() {
        // index 0: live file, index 1: removed file.
        let removed = LoadedFile::stub("gone.rs".into()); // stub() marks removed = true
        let files = vec![live_file("src/main.rs"), removed];

        // Stack front-to-back, with a duplicate terminal and the picker itself.
        let stack = vec![
            Window::FilePreview(FileKey(0)),
            Window::Terminal(3),
            Window::FileNamePicker,
            Window::FilePreview(FileKey(1)), // removed -> excluded
            Window::Terminal(3),             // duplicate -> deduped
        ];

        let results = FileNamePickerComponent::open_previews_results(&files, &stack);
        let windows: Vec<Window> = results.into_iter().map(|(w, _)| w).collect();
        assert_eq!(
            windows,
            vec![Window::FilePreview(FileKey(0)), Window::Terminal(3)],
        );
    }

    #[test]
    fn terminal_accent_is_distinct_and_theme_sourced() {
        let theme = HelixTheme::default();
        let accent = terminal_accent(&theme);
        // Never identical to the file-row (ui.text) color.
        assert_ne!(Some(accent), theme.ui_fg("ui.text"));
        // On the default theme it resolves to the `function` scope color.
        assert_eq!(Some(accent), theme.color_for_scope("function"));
    }
}

/// Foreground color for terminal rows in the picker.
///
/// Pulls a vivid, theme-defined syntax color (preferring `function`) via
/// [`HelixTheme::color_for_scope`], which walks each scope's fallback chain and
/// always resolves to a real color. Picks the first candidate whose color
/// differs from `ui.text` so terminal rows are never identical to file rows;
/// falls back to the default `function` blue if the theme somehow lacks all of
/// them. `constant` is intentionally excluded — it maps to the same color the
/// picker uses for fuzzy-match highlights.
fn terminal_accent(theme: &HelixTheme) -> [u8; 3] {
    let text = theme.ui_fg("ui.text");
    ["function", "string", "keyword", "type"]
        .into_iter()
        .filter_map(|scope| theme.color_for_scope(scope))
        .find(|color| Some(*color) != text)
        .unwrap_or([137, 180, 250])
}

fn run_file_name_match(
    query: &str,
    files: &[Arc<LoadedFile>],
    root: &Utf8PathBuf,
    window_stack: &[Window],
) -> Vec<(Window, Vec<u32>)> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);

    if pattern.atoms.is_empty() {
        return FileNamePickerComponent::open_previews_results(files, window_stack);
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

impl Component<AppState, AppSignal> for FileNamePickerComponent {
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
                state.pane_manager.remove_window(&Window::FileNamePicker);
                state.file_name_picker.reset();
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Enter),
            }) => {
                let state = &mut global_data.state;
                if state.file_name_picker.results.is_empty() {
                    return Ok(EventPropagation::ConsumedRender);
                }
                let selected = state.file_name_picker.resolve_selected_index();
                if let Some(&(window, _)) = state.file_name_picker.results.get(selected) {
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
                                .terminal_panes
                                .get(&id)
                                .and_then(|p| p.lock().ok())
                                .is_some_and(|p| p.scroll_offset > 0);
                            state.terminal_grabbed = !scrolled;
                            crate::tui::app::notify_terminal_focus_change(state, old, Some(window));
                        }
                        _ => {}
                    }
                }
                state.pane_manager.remove_window(&Window::FileNamePicker);
                state.file_name_picker.reset();
                state.mark_session_dirty();
                return Ok(EventPropagation::ConsumedRender);
            }
            _ => {}
        }

        if self
            .input_line
            .handle_key(&input_event, &mut global_data.state.file_name_picker.query)
        {
            let state = &mut global_data.state;
            if state.file_name_picker.query.is_empty() {
                let snapshot = state.files.load();
                state.file_name_picker.results = FileNamePickerComponent::open_previews_results(
                    &snapshot,
                    &state.pane_manager.window_stack,
                );
            } else {
                let main_tx = global_data.main_thread_channel_sender.clone();
                self.on_query_changed(&*state, main_tx);
            }
            return Ok(EventPropagation::ConsumedRender);
        }

        let page_size = global_data
            .state
            .pane_manager
            .window_page_size(&Window::FileNamePicker);
        if let Some(result) = self.picker.handle_navigation(
            &input_event,
            page_size,
            &mut global_data.state.file_name_picker,
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

            let result_ops = self.picker.render_results(
                &global_data.state,
                &global_data.state.file_name_picker,
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
                            .terminal_panes
                            .get(id)
                            .and_then(|p| p.lock().ok())
                            .and_then(|p| p.title.clone())
                            .unwrap_or_else(|| format!("Terminal {id}"));
                        // Render terminals in a distinct theme color (+ bold) so
                        // they stand out from file paths.
                        (title, Some(terminal_accent(&state.theme)))
                    }
                    _ => (String::new(), None),
                },
            );
            let result_count = global_data.state.file_name_picker.results.len();

            global_data
                .state
                .pane_manager
                .set_window_scroll(&Window::FileNamePicker, self.picker.scroll_offset);
            global_data
                .state
                .pane_manager
                .set_window_scroll_max(&Window::FileNamePicker, result_count);
            global_data
                .state
                .pane_manager
                .set_window_page_size(&Window::FileNamePicker, total_rows);

            global_data.pipeline.push(ZOrder::Normal, result_ops);
        });
    }
}
