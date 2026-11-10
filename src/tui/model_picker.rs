#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

pub struct ModelPickerComponent {
    id: FlexBoxId,
    picker: crate::tui::fuzzy_picker::FuzzyPicker,
}

impl ModelPickerComponent {
    pub fn new(id: FlexBoxId) -> Self {
        use crate::tui::fuzzy_picker::FuzzyPicker;
        Self {
            id,
            picker: FuzzyPicker::new(),
        }
    }

    /// Opens the picker for `purpose`, preselecting the model in use, and retries the
    /// model lists that failed to load.
    pub(crate) fn open(
        state: &mut crate::tui::state::AppState,
        main_tx: crate::signal::MainTx,
        purpose: crate::tui::state::ModelPickerPurpose,
    ) {
        use crate::tui::pane_manager::Window;
        use crate::tui::state::ModelPickerPurpose;
        // The model of a window whose turn is running is settled for the whole turn:
        // the turn already runs on it, and the node records the model it ran on. Once
        // the turn is over the reasoning on disk resumes on whatever is picked here.
        if let ModelPickerPurpose::Window(id) = purpose
            && crate::tui::discussion_pane::window_is_running(state, id)
        {
            if let Some(discussion) = state.discussions.get_mut(&id) {
                discussion.notice =
                    Some("this turn is running: its model is fixed until it ends".to_string());
            }
            return;
        }
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
        crate::tui::focus::focus_window(state, Window::ModelPicker);
    }

    /// Closes the picker and gives the focus back to the window it was opened from.
    fn close(
        state: &mut crate::tui::state::AppState,
    ) -> Option<crate::tui::state::ModelPickerPurpose> {
        use crate::tui::pane_manager::Window;
        use crate::tui::state::ModelPickerPurpose;
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
        crate::tui::focus::refocus(state, Some(Window::ModelPicker));
        purpose
    }
}

/// Gets the model list of `endpoint` in the background: fetched from an
/// OpenAI-compatible server, taken from the configuration for the `claude` CLI.
pub(crate) fn fetch_models(endpoint: &crate::agent::Endpoint, main_tx: crate::signal::MainTx) {
    use crate::agent::EndpointKind;
    use crate::signal::AppSignal;
    let name = endpoint.name.clone();
    let kind = endpoint.kind.clone();
    tokio::spawn(async move {
        let result = match kind {
            EndpointKind::OpenAi { url, token }
            | EndpointKind::OpenCodeGo { url, token }
            | EndpointKind::KimiCode { url, token }
            | EndpointKind::OpenRouter { url, token } => {
                crate::agent::models::fetch(&url, &token).await
            }
            EndpointKind::Cline { url, token } => {
                crate::agent::models::fetch_cline(&url, &token).await
            }
            EndpointKind::ClaudeCode { models } => Ok(models),
        };
        crate::signal::send(
            &main_tx,
            AppSignal::ModelsFetched {
                endpoint: name,
                result,
            },
        );
    });
}

/// Fuzzy-matches `query` against every model spec; an empty query lists them all.
pub(crate) fn run_model_match(
    query: &str,
    state: &crate::tui::state::AppState,
) -> Vec<(usize, Vec<u32>)> {
    let pattern = nucleo::pattern::Pattern::parse(
        query,
        nucleo::pattern::CaseMatching::Smart,
        nucleo::pattern::Normalization::Smart,
    );
    let specs = &state.models.specs;
    if pattern.atoms.is_empty() {
        return (0..specs.len()).map(|idx| (idx, Vec::new())).collect();
    }
    let mut matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(usize, u32, Vec<u32>)> = specs
        .iter()
        .enumerate()
        .filter_map(|(idx, spec)| {
            let text = spec.to_string();
            let haystack = nucleo::Utf32Str::new(&text, &mut buf);
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

impl crate::tui::title_row::TitleRow for ModelPickerComponent {
    fn render_title_row(
        &self,
        ops: &mut RenderOpIRVec,
        pane_box: &FlexBox,
        focused: bool,
        state: &mut crate::tui::state::AppState,
    ) -> usize {
        let origin = pane_box.style_adjusted_bounds.origin_pos;
        let width = pane_box
            .style_adjusted_bounds
            .bounds_size
            .col_width
            .as_u16();
        let prompt = "Model: ";
        let max_rows = (pane_box
            .style_adjusted_bounds
            .bounds_size
            .row_height
            .as_usize()
            / 2)
        .max(1);
        let height = state
            .model_picker
            .query
            .height(None, prompt, width, max_rows);

        let (bg_rgb, fg_rgb) = crate::tui::title_row::title_bar_colors(focused, &state.theme);
        let color_bg = TuiColor::from(bg_rgb);
        let color_fg = TuiColor::from(fg_rgb);
        let color_selection = TuiColor::from(crate::tui::title_row::title_selection_bg(
            focused,
            &state.theme,
        ));
        let bg_style = new_style!(color_fg: {color_fg} color_bg: {color_bg});
        let query = &mut state.model_picker.query;

        for row_offset in 0..height {
            *ops += RenderOpCommon::MoveCursorPositionRelTo(
                origin,
                vp_col(0) + vp_row(row_offset as u16),
            );
            *ops += RenderOpCommon::SetBgColor(color_bg);
            *ops += RenderOpIR::PaintTextWithAttributes(
                " ".repeat(width as usize).as_str().into(),
                Some(bg_style),
            );
        }
        query.render(
            ops,
            None,
            prompt,
            origin,
            width,
            height,
            focused,
            (color_bg, color_fg, color_selection),
        );
        height
    }
}

impl Component<crate::tui::state::AppState, crate::signal::AppSignal> for ModelPickerComponent {
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
        use crate::tui::discussion_pane::DiscussionComponent;
        use crate::tui::pane_manager::Window;
        use crate::tui::state::ModelPickerPurpose;
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

        if state.model_picker.query.handle_key(&input_event) {
            let query = state.model_picker.query.text().to_string();
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
