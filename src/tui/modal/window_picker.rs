#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A batch of window picker results: the generation it answers, and the windows.
pub(crate) type PickerResultMsg = (u64, Vec<crate::tui::pane_manager::Window>);

/// What matching a query off the main thread needs: the counter that drops a stale answer,
/// and the channel a fresh one is sent on.
pub(crate) struct Senders<'a> {
    pub generation: &'a Arc<AtomicU64>,
    pub results_tx: &'a tokio::sync::mpsc::Sender<PickerResultMsg>,
}

/// Opens the window picker over the panes: the open windows while the query is empty, and the
/// files of the repository once one is typed.
pub fn open(state: &mut crate::tui::state::AppState) {
    use crate::tui::modal::Modal;
    state.window_picker.query.clear();
    state.window_picker.selected = None;
    state.window_picker.scroll_offset = 0;
    let snapshot = state.files.load();
    state.window_picker.results = open_windows_results(&snapshot, &state.pane_manager.window_stack);
    Modal::open(state, Modal::WindowPicker);
}

/// Hands `input_event` to the window picker while it is the modal in front.
pub fn handle(
    state: &mut crate::tui::state::AppState,
    input_event: &InputEvent,
    senders: Senders<'_>,
    main_tx: crate::signal::MainTx,
) {
    use crate::tui::modal::Modal;
    if !matches!(state.modal, Some(Modal::WindowPicker)) {
        return;
    }
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
            open_selected(state);
            return;
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
            close_selected(state);
            return;
        }
        _ => {}
    }

    if state.window_picker.query.handle_key(input_event) {
        state.window_picker.scroll_offset = 0;
        if state.window_picker.query.text().is_empty() {
            let snapshot = state.files.load();
            state.window_picker.results =
                open_windows_results(&snapshot, &state.pane_manager.window_stack);
            state.window_picker.selected = None;
            return;
        }
        spawn_match(
            state,
            Arc::clone(senders.generation),
            senders.results_tx.clone(),
            main_tx,
        );
        return;
    }
    let page_size = state.modal_page_size;
    crate::tui::modal::list::navigate(state, input_event, page_size);
}

/// Opens the window the picker highlights, then closes the picker.
fn open_selected(state: &mut crate::tui::state::AppState) {
    use crate::tui::modal::Modal;
    use crate::tui::pane_manager::Window;
    let index = state.window_picker.selected_index();
    let Some(window) = state.window_picker.results.get(index).copied() else {
        return;
    };
    Modal::close(state);
    crate::tui::focus::focus_window(state, window);
    if let Window::FilePreview(key) = window {
        crate::lsp::send_file_request(key.0);
    }
}

/// Closes the window the picker highlights, and stays open: the picker's own list loses the
/// row, so a run of windows closes without reopening it.
fn close_selected(state: &mut crate::tui::state::AppState) {
    let index = state.window_picker.selected_index();
    let Some(window) = state.window_picker.results.get(index).copied() else {
        return;
    };
    if !state.pane_manager.window_stack.contains(&window) {
        return;
    }
    crate::tui::focus::close_window(state, window);
    if state.window_picker.query.text().is_empty() {
        state.window_picker.results.remove(index);
        let results = &state.window_picker.results;
        state.window_picker.selected = results.get(index).or(results.last()).copied();
    }
}

/// Starts matching the query against the files of the repository, off the main thread. The
/// answer comes back on `results_tx` with its generation, so a match that arrives after a
/// newer one is dropped by the frame that reads the channel.
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

/// The windows open in the stack, in its order, each listed once.
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
        };
        if listed && seen.insert(*window) {
            results.push(*window);
        }
    }
    results
}

/// The foreground colors of the rows that are not files, by kind of window.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RowAccents {
    terminal: crate::tui::theme::Color,
    agent: crate::tui::theme::Color,
    git: crate::tui::theme::Color,
}

/// Accent colors for terminal, agent and git rows in the picker.
///
/// Pulls vivid, theme-defined syntax colors (preferring `function`) via
/// [`crate::tui::theme::HelixTheme::color_for_scope`], which walks each scope's fallback chain
/// and always resolves to a real color. Keeps the candidates that differ from `ui.text` and
/// from each other, so these rows are never identical to file rows or to another kind; the
/// default blue, green and mauve stand in for what the theme lacks. `constant` is
/// intentionally excluded — it maps to the same color the picker uses for fuzzy matches.
fn row_accents(theme: &crate::tui::theme::HelixTheme) -> RowAccents {
    use crate::tui::theme::Color;
    let text = theme.ui.text;
    let defaults = [
        Color::from_u8(137, 180, 250),
        Color::from_u8(166, 227, 161),
        Color::from_u8(203, 166, 247),
    ];
    let mut distinct: Vec<Color> = Vec::with_capacity(3);
    for color in ["function", "string", "keyword", "type"]
        .into_iter()
        .map(|scope| theme.color_for_scope(scope))
        .chain(defaults)
    {
        if color != text && !distinct.contains(&color) {
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
    use crate::tui::widgets::fuzzy::Kind;
    use crate::tui::widgets::fuzzy::Query;
    let mut query = Query::new(Kind::Paths);
    query.set(text);

    if query.is_empty() {
        return open_windows_results(files, window_stack);
    }

    let mut scored: Vec<(FileKey, u32)> = files
        .iter()
        .enumerate()
        .filter(|(_, file)| !file.removed.load(Ordering::Relaxed))
        .filter_map(|(index, file)| {
            let rel = file.path.strip_prefix(root).unwrap_or(&file.path);
            query
                .rank(rel.as_str())
                .map(|score| (FileKey(index), score))
        })
        .collect();
    scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
    scored
        .into_iter()
        .map(|(key, _)| Window::FilePreview(key))
        .collect()
}

/// Paints the window picker's list.
pub fn render(
    state: &mut crate::tui::state::AppState,
    painter: &mut crate::tui::widgets::fuzzy_picker::FuzzyPicker,
    list: &crate::tui::modal::ModalBox,
) -> RenderOpIRVec {
    use crate::tui::widgets::fuzzy_picker::{PickerColors, PickerRow};
    let accents = row_accents(&state.theme);
    let view = crate::tui::modal::list::visible(&mut state.window_picker, list.rows);
    let rows: Vec<PickerRow> = state.window_picker.results[view.range]
        .iter()
        .map(|window| row_text(state, window, &accents))
        .collect();
    crate::tui::modal::list::paint_rows(
        PickerColors::in_overlay(&state.theme),
        painter,
        state.window_picker.query.text(),
        &rows,
        Some(view.selected),
        list,
    )
}

/// The text of one row and the color its kind wears.
fn row_text(
    state: &crate::tui::state::AppState,
    window: &crate::tui::pane_manager::Window,
    accents: &RowAccents,
) -> crate::tui::widgets::fuzzy_picker::PickerRow {
    use crate::tui::pane_manager::Window;
    use crate::tui::panes::commit_diff_pane::CommitDiffPaneComponent;
    use crate::tui::panes::conversation_tree::ConversationTreeComponent;
    use crate::tui::panes::discussion::DiscussionComponent;
    use crate::tui::panes::git_log_pane::GitLogPaneComponent;
    use crate::tui::panes::report_pane::ReportComponent;
    use crate::tui::widgets::fuzzy_picker::PickerRow;
    match window {
        Window::FilePreview(key) => {
            let snapshot = state.files.load_full();
            let file = &snapshot[key.0];
            let rel = file.path.strip_prefix(&state.root).unwrap_or(&file.path);
            PickerRow::plain(rel.to_string())
        }
        Window::Terminal(id) => {
            let title = state
                .with_terminal(*id, |pane| pane.title.clone())
                .flatten()
                .unwrap_or_else(|| format!("Terminal {id}"));
            PickerRow::accented(title, accents.terminal)
        }
        Window::GitLog(id) => PickerRow::accented(
            state
                .git_logs
                .get(id)
                .map(GitLogPaneComponent::title)
                .unwrap_or_default(),
            accents.git,
        ),
        Window::CommitDiff(id) => {
            PickerRow::accented(CommitDiffPaneComponent::title(state, *id), accents.git)
        }
        Window::ConversationTree => {
            PickerRow::accented(ConversationTreeComponent::title(state), accents.agent)
        }
        Window::Discussion(id) => {
            PickerRow::accented(DiscussionComponent::title(state, *id), accents.agent)
        }
        Window::Report(id) => PickerRow::plain(ReportComponent::title(state, *id)),
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
    fn open_list_lists_every_window_in_stack_order_once() {
        use crate::loader::FileKey;
        use crate::tui::pane_manager::Window;
        // index 0: live file, index 1: removed file.
        let removed = LoadedFile::stub("gone.rs".into()); // stub() marks removed = true
        let files = vec![live_file("src/main.rs"), removed];
        let stack = vec![
            Window::Discussion(2),
            Window::FilePreview(FileKey(0)),
            Window::Terminal(3),
            Window::ConversationTree,
            Window::GitLog(1),
            Window::CommitDiff([7; 20]),
            Window::FilePreview(FileKey(1)), // removed -> excluded
            Window::Terminal(3),             // duplicate -> deduped
        ];

        let windows = open_windows_results(&files, &stack);
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
        let theme = HelixTheme::bundled_default();
        let accents = row_accents(&theme);
        // Never identical to the file-row (ui.text) color, nor to each other.
        for accent in [accents.terminal, accents.agent, accents.git] {
            assert_ne!(accent, theme.ui.text);
        }
        assert_ne!(accents.terminal, accents.agent);
        assert_ne!(accents.terminal, accents.git);
        assert_ne!(accents.agent, accents.git);
        // On the default theme terminals keep the `function` scope color.
        assert_eq!(accents.terminal, theme.color_for_scope("function"));
    }

    /// An empty query lists the open windows: matching files is what a typed query does.
    #[test]
    fn an_empty_query_lists_the_open_windows() {
        use crate::tui::pane_manager::Window;
        let files = vec![live_file("src/main.rs")];
        let stack = vec![Window::ConversationTree];
        let root = camino::Utf8PathBuf::from("/repo");
        assert_eq!(
            run_file_name_match("", &files, &root, &stack),
            vec![Window::ConversationTree]
        );
    }
}
