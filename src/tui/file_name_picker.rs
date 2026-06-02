use super::fuzzy_picker::FuzzyPicker;
use super::input_line::InputLine;
use super::state::{AppSignal, State, Window};
use super::theme::HelixTheme;
use crate::loader::{FileKey, LoadedFile};
use camino::Utf8PathBuf;
use nucleo::Matcher;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Utf32Str};
use r3bl_tui::{
    CommonResult, Component, EventPropagation, FlexBox, FlexBoxId, GlobalData, HasFocus,
    InputEvent, Key, KeyPress, KeyState, ModifierKeysMask, Pos, RenderOpCommon, RenderOpIR,
    RenderOpIRVec, RenderPipeline, SpecialKey, SurfaceBounds, TerminalWindowMainThreadSignal,
    ZOrder, col, new_style, render_pipeline, row, send_signal, throws_with_return, tui_color,
};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;

pub(crate) type PickerResultMsg = (u64, Vec<(FileKey, Vec<u32>)>);

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

    pub(crate) fn render_title_row(
        &self,
        mut ops: &mut RenderOpIRVec,
        origin: Pos,
        width: u16,
        focused: bool,
        theme: &HelixTheme,
        query: &str,
    ) {
        let (bg_rgb, fg_rgb) = title_bar_colors(focused, theme);
        let color_bg = tui_color!(bg_rgb[0], bg_rgb[1], bg_rgb[2]);
        let color_fg = tui_color!(fg_rgb[0], fg_rgb[1], fg_rgb[2]);
        let bg_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});

        ops += RenderOpCommon::MoveCursorPositionRelTo(origin, col(0) + row(0));
        ops += RenderOpCommon::SetBgColor(color_bg);
        ops += RenderOpIR::PaintTextWithAttributes(
            " ".repeat(width as usize).as_str().into(),
            Some(bg_style),
        );
        self.input_line
            .render(ops, query, origin, width, focused, bg_rgb, fg_rgb);
    }

    fn on_query_changed(
        &self,
        state: &State,
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
        state: &State,
        generation: Arc<AtomicU64>,
        results_tx: mpsc::Sender<PickerResultMsg>,
        main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
    ) {
        let query = state.file_name_picker.query.clone();
        let files = Arc::clone(&state.files);
        let root = state.root.clone();
        let window_stack = state.window_stack.clone();
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

    pub(crate) fn all_files_results(
        files: &[LoadedFile],
        window_stack: &[Window],
    ) -> Vec<(FileKey, Vec<u32>)> {
        let mut seen: HashSet<FileKey> = HashSet::new();
        let mut results = Vec::new();
        for window in window_stack {
            if let Window::FilePreview(key) = window
                && !files[key.0].removed.load(Ordering::Relaxed)
                && seen.insert(*key)
            {
                results.push((*key, vec![]));
            }
        }
        results
    }
}

fn title_bar_colors(focused: bool, theme: &HelixTheme) -> ([u8; 3], [u8; 3]) {
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

fn run_file_name_match(
    query: &str,
    files: &[LoadedFile],
    root: &Utf8PathBuf,
    window_stack: &[Window],
) -> Vec<(FileKey, Vec<u32>)> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);

    if pattern.atoms.is_empty() {
        return FileNamePickerComponent::all_files_results(files, window_stack);
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
    scored.into_iter().map(|(key, _, idx)| (key, idx)).collect()
}

impl Component<State, AppSignal> for FileNamePickerComponent {
    fn reset(&mut self) {}

    fn get_id(&self) -> FlexBoxId {
        self.id
    }

    fn handle_event(
        &mut self,
        global_data: &mut GlobalData<State, AppSignal>,
        input_event: InputEvent,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<EventPropagation> {
        match &input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }) => {
                let state = &mut global_data.state;
                state.remove_window(&Window::FileNamePicker);
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
                if let Some(&(key, _)) = state.file_name_picker.results.get(selected) {
                    if !state.window_states.contains_key(&Window::FilePreview(key)) {
                        state.set_window_scroll(&Window::FilePreview(key), 0);
                    }
                    state.push_window(Window::FilePreview(key));
                    state.focused_window = Some(Window::FilePreview(key));
                    crate::lsp::send_file_request(key.0);
                }
                state.remove_window(&Window::FileNamePicker);
                state.file_name_picker.reset();
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::WithModifiers {
                key: Key::Character('c'),
                mask:
                    ModifierKeysMask {
                        ctrl_key_state: KeyState::Pressed,
                        ..
                    },
            }) => {
                let state = &mut global_data.state;
                state.remove_window(&Window::FileNamePicker);
                state.file_name_picker.reset();
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
                state.file_name_picker.results =
                    FileNamePickerComponent::all_files_results(&snapshot, &state.window_stack);
            } else {
                let main_tx = global_data.main_thread_channel_sender.clone();
                self.on_query_changed(&*state, main_tx);
            }
            return Ok(EventPropagation::ConsumedRender);
        }

        let page_size = global_data.state.window_page_size(&Window::FileNamePicker);
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
        global_data: &mut GlobalData<State, AppSignal>,
        current_box: FlexBox,
        _surface_bounds: SurfaceBounds,
        _has_focus: &mut HasFocus,
    ) -> CommonResult<RenderPipeline> {
        throws_with_return!({
            let origin = current_box.style_adjusted_origin_pos;
            let bounds = current_box.style_adjusted_bounds_size;
            let total_rows = bounds.row_height.as_usize();
            let pane_width = bounds.col_width.as_usize();

            let mut pipeline = render_pipeline!();

            if total_rows == 0 {
                return Ok(pipeline);
            }

            let result_ops = self.picker.render_results(
                &global_data.state,
                origin,
                total_rows,
                pane_width,
                &global_data.state.file_name_picker,
                |key, state| {
                    let snapshot = state.files.load_full();
                    let file = &snapshot[key.0];
                    let rel = file.path.strip_prefix(&state.root).unwrap_or(&file.path);
                    rel.to_string()
                },
            );
            let result_count = global_data.state.file_name_picker.results.len();

            global_data
                .state
                .set_window_scroll(&Window::FileNamePicker, self.picker.scroll_offset);
            global_data
                .state
                .set_window_scroll_max(&Window::FileNamePicker, result_count);
            global_data
                .state
                .set_window_page_size(&Window::FileNamePicker, total_rows);

            pipeline.push(ZOrder::Normal, result_ops);
            pipeline
        });
    }
}
