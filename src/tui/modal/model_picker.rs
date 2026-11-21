#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

/// Opens the model picker for `purpose`, preselecting the model in use, and retries the model
/// lists that failed to load.
pub fn open(
    state: &mut crate::tui::state::AppState,
    main_tx: crate::signal::MainTx,
    purpose: crate::tui::state::ModelPickerPurpose,
) {
    use crate::tui::modal::Modal;
    use crate::tui::state::ModelPickerPurpose;
    // The model of a window whose turn is running is settled for the whole turn: the turn
    // runs on it already, and its node records it. Once the turn is over, the reasoning on
    // disk resumes on whatever is picked here.
    if let ModelPickerPurpose::Window(id) = &purpose
        && crate::tui::panes::discussion::launch::window_is_running(state, *id)
    {
        if let Some(discussion) = state.discussions.get_mut(id) {
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
    state.model_picker.scroll_offset = 0;
    let current = match &purpose {
        ModelPickerPurpose::Window(id) => state
            .discussions
            .get(id)
            .and_then(|discussion| discussion.model.clone()),
        ModelPickerPurpose::Duplicate(node) => state
            .conversations
            .read()
            .node(node)
            .and_then(|node| node.meta.model_spec()),
    };
    state.model_picker.selected =
        current.and_then(|model| state.models.specs.iter().position(|spec| *spec == model));
    Modal::open(state, Modal::ModelPicker(purpose));
}

/// Hands `input_event` to the model picker, the modal in front.
pub fn handle(
    state: &mut crate::tui::state::AppState,
    input_event: &InputEvent,
    purpose: crate::tui::state::ModelPickerPurpose,
    main_tx: crate::signal::MainTx,
) {
    use crate::tui::modal::Modal;
    use crate::tui::panes::discussion::DiscussionComponent;
    use crate::tui::state::ModelPickerPurpose;
    match input_event {
        InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(SpecialKey::Esc),
        }) => {
            Modal::close(state);
            return;
        }
        InputEvent::Keyboard(KeyPress::Plain {
            key: Key::SpecialKey(SpecialKey::Enter),
        }) => {
            let picked = state
                .model_picker
                .selected
                .or_else(|| state.model_picker.results.first().copied())
                .and_then(|index| state.models.specs.get(index).cloned());
            let Some(spec) = picked else {
                return;
            };
            Modal::close(state);
            match purpose {
                ModelPickerPurpose::Window(id) => {
                    if let Some(discussion) = state.discussions.get_mut(&id) {
                        discussion.model = Some(spec.clone());
                        discussion.notice = None;
                    }
                    state.last_picked_model = Some(spec);
                }
                ModelPickerPurpose::Duplicate(node) => {
                    DiscussionComponent::duplicate(state, main_tx, &node, spec);
                }
            }
            return;
        }
        _ => {}
    }

    if state.model_picker.query.handle_key(input_event) {
        let query = state.model_picker.query.text().to_string();
        state.model_picker.results = run_model_match(&query, state);
        state.model_picker.selected = state.model_picker.results.first().copied();
        state.model_picker.scroll_offset = 0;
        return;
    }
    if !crate::tui::modal::list::wheel_outside(state, input_event) {
        let page = crate::tui::modal::list::page_size(state);
        state.model_picker.navigate(input_event, page);
    }
}

/// Gets the model list of `endpoint` in the background: fetched from an OpenAI-compatible
/// server, taken from the configuration for the `claude` CLI.
pub fn fetch_models(endpoint: &crate::endpoint::Endpoint, main_tx: crate::signal::MainTx) {
    use crate::endpoint::EndpointKind;
    use crate::signal::AppSignal;
    let name = endpoint.name.clone();
    let kind = endpoint.kind.clone();
    tokio::spawn(async move {
        let result = match kind {
            EndpointKind::Http {
                url,
                token,
                provider,
            } => crate::agent::models::fetch(&url, &token, provider).await,
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

/// The specs matching `text`, best match first; an empty query lists them all.
pub fn run_model_match(text: &str, state: &crate::tui::state::AppState) -> Vec<usize> {
    use crate::tui::widgets::fuzzy::Kind;
    let specs = &state.models.specs;
    crate::tui::widgets::fuzzy::ranked(
        Kind::Plain,
        text,
        specs
            .iter()
            .enumerate()
            .map(|(index, spec)| (index, spec.to_string())),
    )
    .unwrap_or_else(|| (0..specs.len()).collect())
}

/// Paints the model picker's list.
pub fn render(
    state: &mut crate::tui::state::AppState,
    painter: &mut crate::tui::widgets::fuzzy_picker::FuzzyPicker,
    list: &crate::tui::modal::ModalBox,
) -> RenderOpIRVec {
    use crate::tui::widgets::fuzzy_picker::{PickerColors, PickerRow};
    let view = crate::tui::modal::list::visible(&mut state.model_picker, list.rows);
    let mut rows: Vec<PickerRow> = state.model_picker.results[view.range]
        .iter()
        .map(|index| {
            PickerRow::plain(
                state
                    .models
                    .specs
                    .get(*index)
                    .map(ToString::to_string)
                    .unwrap_or_default(),
            )
        })
        .collect();
    // A picker with nothing to offer says why: an endpoint whose list failed, or none at
    // all, is the reason there is no model to choose.
    if state.model_picker.results.is_empty() {
        let errors = state.models.errors();
        let text = if !errors.is_empty() {
            errors.join("  ")
        } else if state.agent.endpoints.is_empty() {
            "No endpoint: declare one in config.kdl and its token in auth.kdl".to_string()
        } else {
            "No model: none of the endpoints reported one".to_string()
        };
        rows.push(PickerRow::accented(text, state.theme.ui.error));
    }
    // The explanation is no result, so it never wears the selection bar.
    let selected = if state.model_picker.results.is_empty() {
        None
    } else {
        Some(view.selected)
    };
    crate::tui::modal::list::paint_rows(
        PickerColors::in_overlay(&state.theme),
        painter,
        state.model_picker.query.text(),
        &rows,
        selected,
        list,
    )
}
