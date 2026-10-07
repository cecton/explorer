use crate::agent::EndpointKind;
use crate::tui::*;
use nucleo::Matcher;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Utf32Str};
use tokio::sync::mpsc;

pub struct ModelPickerComponent {
    id: FlexBoxId,
    picker: FuzzyPicker,
    input_line: InputLine,
}

impl ModelPickerComponent {
    pub fn new(id: FlexBoxId) -> Self {
        Self {
            id,
            picker: FuzzyPicker::new(),
            input_line: InputLine::new(),
        }
    }

    /// Opens the picker for `purpose`, preselecting the model in use, and retries the
    /// model lists that failed to load.
    pub(crate) fn open(
        state: &mut AppState,
        main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
        purpose: ModelPickerPurpose,
    ) {
        for endpoint in &state.agent.endpoints {
            if matches!(state.models.lists.get(&endpoint.name), Some(Err(_))) {
                fetch_models(endpoint, main_tx.clone());
            }
        }
        state.model_picker.query.clear();
        state.model_picker.results = run_model_match("", state);
        let current = match &purpose {
            ModelPickerPurpose::Window(id) => state
                .discussions
                .get(id)
                .and_then(|discussion| discussion.model.clone()),
            ModelPickerPurpose::Duplicate(node) => state
                .conversations
                .read()
                .node(node)
                .and_then(crate::tui::discussion_pane::node_model),
        };
        state.model_picker.selected =
            current.and_then(|model| state.models.specs.iter().position(|spec| *spec == model));
        state.model_picker_purpose = Some(purpose);
        let old = state.pane_manager.focused_window;
        state.pane_manager.push_window(Window::ModelPicker);
        state.pane_manager.focused_window = Some(Window::ModelPicker);
        crate::tui::app::notify_terminal_focus_change(state, old, Some(Window::ModelPicker));
    }

    /// Closes the picker and gives the focus back to the window it was opened from.
    fn close(state: &mut AppState) -> Option<ModelPickerPurpose> {
        state.pane_manager.remove_window(&Window::ModelPicker);
        state.model_picker.reset();
        let purpose = state.model_picker_purpose.take();
        let origin = match &purpose {
            Some(ModelPickerPurpose::Window(id)) => Some(Window::Discussion(*id)),
            Some(ModelPickerPurpose::Duplicate(_)) => Some(Window::ConversationTree),
            None => None,
        };
        if let Some(window) = origin
            && state.pane_manager.window_stack.contains(&window)
        {
            state.pane_manager.focused_window = Some(window);
        }
        purpose
    }
}

/// Gets the model list of `endpoint` in the background: fetched from an
/// OpenAI-compatible server, taken from the configuration for the `claude` CLI.
pub(crate) fn fetch_models(
    endpoint: &crate::agent::Endpoint,
    main_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
) {
    let name = endpoint.name.clone();
    let kind = endpoint.kind.clone();
    tokio::spawn(async move {
        let result = match kind {
            EndpointKind::OpenAi { url, token } => crate::agent::models::fetch(&url, &token).await,
            EndpointKind::ClaudeCode { models } => Ok(models),
        };
        let _ = main_tx
            .send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                AppSignal::ModelsFetched {
                    endpoint: name,
                    result,
                },
            ))
            .await;
    });
}

/// Fuzzy-matches `query` against every model spec; an empty query lists them all.
pub(crate) fn run_model_match(query: &str, state: &AppState) -> Vec<(usize, Vec<u32>)> {
    let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
    let specs = &state.models.specs;
    if pattern.atoms.is_empty() {
        return (0..specs.len()).map(|idx| (idx, Vec::new())).collect();
    }
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(usize, u32, Vec<u32>)> = specs
        .iter()
        .enumerate()
        .filter_map(|(idx, spec)| {
            let text = spec.to_string();
            let haystack = Utf32Str::new(&text, &mut buf);
            let mut indices = Vec::new();
            pattern
                .indices(haystack, &mut matcher, &mut indices)
                .map(|score| {
                    indices.sort_unstable();
                    indices.dedup();
                    (idx, score, indices)
                })
        })
        .collect();
    scored.sort_by_key(|&(_, score, _)| std::cmp::Reverse(score));
    scored
        .into_iter()
        .map(|(idx, _, indices)| (idx, indices))
        .collect()
}

impl TitleRow for ModelPickerComponent {
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
        let query = &state.model_picker.query;
        let prompt = "Model: ";
        let max_rows = (pane_box.style_adjusted_bounds_size.row_height.as_usize() / 2).max(1);
        let height = InputLine::height(query, None, prompt, width, max_rows);

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
            prompt,
            origin,
            width,
            height,
            focused,
            (color_bg, color_fg),
        );
        height
    }
}

impl Component<AppState, AppSignal> for ModelPickerComponent {
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
        let main_tx = global_data.main_thread_channel_sender.clone();
        let state = &mut global_data.state;
        match &input_event {
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Esc),
            }) => {
                Self::close(state);
                return Ok(EventPropagation::ConsumedRender);
            }
            InputEvent::Keyboard(KeyPress::Plain {
                key: Key::SpecialKey(SpecialKey::Enter),
            }) => {
                let picked = state
                    .model_picker
                    .selected
                    .or_else(|| state.model_picker.results.first().map(|(idx, _)| *idx))
                    .and_then(|idx| state.models.specs.get(idx).cloned());
                let Some(spec) = picked else {
                    return Ok(EventPropagation::ConsumedRender);
                };
                match Self::close(state) {
                    Some(ModelPickerPurpose::Window(id)) => {
                        if let Some(discussion) = state.discussions.get_mut(&id) {
                            discussion.model = Some(spec.clone());
                            discussion.notice = None;
                        }
                        state.last_picked_model = Some(spec);
                    }
                    Some(ModelPickerPurpose::Duplicate(node)) => {
                        DiscussionComponent::duplicate(state, main_tx, &node, spec);
                    }
                    None => {}
                }
                return Ok(EventPropagation::ConsumedRender);
            }
            _ => {}
        }

        if self
            .input_line
            .handle_key(&input_event, &mut state.model_picker.query)
        {
            let query = state.model_picker.query.clone();
            state.model_picker.results = run_model_match(&query, state);
            state.model_picker.selected = state.model_picker.results.first().map(|(idx, _)| *idx);
            return Ok(EventPropagation::ConsumedRender);
        }

        let page_size = state.pane_manager.window_page_size(&Window::ModelPicker);
        if let Some(result) =
            self.picker
                .handle_navigation(&input_event, page_size, &mut state.model_picker)
        {
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
                &global_data.state.model_picker,
                origin,
                total_rows,
                pane_width,
                |idx, state| {
                    let text = state
                        .models
                        .specs
                        .get(*idx)
                        .map(ToString::to_string)
                        .unwrap_or_default();
                    (text, None)
                },
            );
            let result_count = global_data.state.model_picker.results.len();
            let pane_manager = &mut global_data.state.pane_manager;
            pane_manager.set_window_scroll(&Window::ModelPicker, self.picker.scroll_offset);
            pane_manager.set_window_scroll_max(&Window::ModelPicker, result_count);
            pane_manager.set_window_page_size(&Window::ModelPicker, total_rows);
            global_data.pipeline.push(ZOrder::Normal, result_ops);
        });
    }
}
