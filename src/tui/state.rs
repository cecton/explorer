use crate::loader::{FileKey, LoadedFile};
use crate::tui::theme::HelixTheme;
use crate::watcher::BatchedWatchEvent;
use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use r3bl_tui::{EditorBuffer, FlexBoxId, HasEditorBuffers};
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static FILES_VERSION: AtomicU64 = AtomicU64::new(0);

/// A pane that can appear in the window stack.
///
/// Each variant is unique: there is at most one `FileNamePicker` and at most one
/// `FilePreview` per `FileKey` in the stack at any time.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Window {
    FilePreview(FileKey),
    FileNamePicker,
}

/// Scroll and page-size state for a single window pane.
#[derive(Clone, Debug, Default)]
pub struct WindowState {
    pub scroll: usize,
    pub page_size: usize,
}

#[derive(Clone)]
pub struct State {
    pub files: Arc<ArcSwap<Vec<LoadedFile>>>,
    /// Incremented whenever file contents change so PartialEq detects mutations.
    pub files_version: u64,
    pub root: Utf8PathBuf,
    /// Stack of open windows, most-recently-opened first (index 0 = leftmost pane).
    pub window_stack: Vec<Window>,
    /// The window that currently receives keyboard input.
    pub focused_window: Option<Window>,
    /// Per-window scroll and page-size state.
    pub window_states: HashMap<Window, WindowState>,
    pub file_name_picker_open: bool,
    /// Each entry: (FileKey into files vec, sorted+deduped matched char positions).
    pub file_name_picker_results: Vec<(FileKey, Vec<u32>)>,
    pub file_name_picker_selected: Option<FileKey>,
    pub editor_buffers: HashMap<FlexBoxId, EditorBuffer>,
    pub theme: HelixTheme,
}

impl HasEditorBuffers for State {
    fn get_mut_editor_buffer(&mut self, id: FlexBoxId) -> Option<&mut EditorBuffer> {
        self.editor_buffers.get_mut(&id)
    }

    fn insert_editor_buffer(&mut self, id: FlexBoxId, buffer: EditorBuffer) {
        self.editor_buffers.insert(id, buffer);
    }

    fn contains_editor_buffer(&self, id: FlexBoxId) -> bool {
        self.editor_buffers.contains_key(&id)
    }
}

impl State {
    pub fn bump_files_version(&mut self) {
        self.files_version = FILES_VERSION.fetch_add(1, Ordering::Relaxed) + 1;
    }

    /// Moves `window` to the front of the stack (index 0). If it is not present, inserts it.
    pub fn push_window(&mut self, window: Window) {
        if let Some(pos) = self.window_stack.iter().position(|w| w == &window) {
            self.window_stack.remove(pos);
        }
        self.window_stack.insert(0, window);
    }

    /// Removes `window` from the stack entirely.
    pub fn remove_window(&mut self, window: &Window) {
        self.window_stack.retain(|w| w != window);
        self.window_states.remove(window);
        if self.focused_window.as_ref() == Some(window) {
            self.focused_window = self.window_stack.first().cloned();
        }
    }

    /// Moves `window` to the back of the stack (last position).
    pub fn send_to_back(&mut self, window: &Window) {
        if let Some(pos) = self.window_stack.iter().position(|w| w == window) {
            let w = self.window_stack.remove(pos);
            self.window_stack.push(w);
        }
        if self.focused_window.as_ref() == Some(window) {
            self.focused_window = self.window_stack.first().cloned();
        }
    }

    /// Returns the windows that fit in `surface_cols`, along with their assigned column
    /// widths. Each pane requires at least `MIN_PANE_WIDTH` columns; extra space is
    /// distributed equally among all visible panes.
    pub fn visible_windows(&self, surface_cols: u16) -> Vec<(Window, u16)> {
        const MIN_PANE_WIDTH: u16 = 100;
        if surface_cols < MIN_PANE_WIDTH {
            return vec![];
        }
        let count = (surface_cols / MIN_PANE_WIDTH) as usize;
        let n = self.window_stack.len().min(count) as u16;
        if n == 0 {
            return vec![];
        }
        let base_width = surface_cols / n;
        let remainder = surface_cols % n;
        self.window_stack
            .iter()
            .take(n as usize)
            .enumerate()
            .map(|(i, w)| {
                let width = if (i as u16) < remainder {
                    base_width + 1
                } else {
                    base_width
                };
                (w.clone(), width)
            })
            .collect()
    }

    pub fn window_scroll(&self, window: &Window) -> usize {
        self.window_states
            .get(window)
            .map(|s| s.scroll)
            .unwrap_or(0)
    }

    pub fn window_page_size(&self, window: &Window) -> usize {
        self.window_states
            .get(window)
            .map(|s| s.page_size)
            .unwrap_or(0)
    }

    pub fn set_window_scroll(&mut self, window: &Window, scroll: usize) {
        self.window_states.entry(window.clone()).or_default().scroll = scroll;
    }

    pub fn set_window_page_size(&mut self, window: &Window, page_size: usize) {
        self.window_states
            .entry(window.clone())
            .or_default()
            .page_size = page_size;
    }
}

impl State {
    pub fn new(files: Arc<ArcSwap<Vec<LoadedFile>>>, root: Utf8PathBuf, theme: HelixTheme) -> Self {
        let snapshot = files.load();
        let file_name_picker_results = (0..snapshot.len()).map(|i| (FileKey(i), vec![])).collect();
        let mut state = Self {
            files,
            files_version: 0,
            root,
            window_stack: vec![Window::FileNamePicker],
            focused_window: Some(Window::FileNamePicker),
            window_states: HashMap::new(),
            file_name_picker_open: true,
            file_name_picker_results,
            file_name_picker_selected: None,
            editor_buffers: HashMap::new(),
            theme,
        };
        state
            .window_states
            .insert(Window::FileNamePicker, WindowState::default());
        state
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            files: Arc::new(ArcSwap::from_pointee(Vec::new())),
            files_version: 0,
            root: Utf8PathBuf::new(),
            window_stack: Vec::new(),
            focused_window: None,
            window_states: HashMap::new(),
            file_name_picker_open: false,
            file_name_picker_results: Vec::new(),
            file_name_picker_selected: None,
            editor_buffers: HashMap::new(),
            theme: HelixTheme::default(),
        }
    }
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.files, &other.files)
            && self.files_version == other.files_version
            && self.window_stack == other.window_stack
            && self.focused_window == other.focused_window
            && self.file_name_picker_open == other.file_name_picker_open
            && self.file_name_picker_selected == other.file_name_picker_selected
            && self.file_name_picker_results.len() == other.file_name_picker_results.len()
    }
}

impl Eq for State {}

impl Debug for State {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let count = self.files.load().len();
        write!(
            f,
            "State {{ files: {}, stack: {:?}, focused: {:?} }}",
            count, self.window_stack, self.focused_window
        )
    }
}

impl Display for State {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "State[files={}]", self.files.load().len())
    }
}

#[derive(Default, Clone, Debug)]
#[non_exhaustive]
pub enum AppSignal {
    OpenFileNamePicker,
    CloseFileNamePicker,
    FileNamePickerQueryChanged,
    FileNamePickerSelectNext,
    FileNamePickerSelectPrev,
    FileNamePickerConfirm,
    ScrollPreviewDown(usize),
    ScrollPreviewUp(usize),
    SendFocusedWindowToBack,
    FocusNextPane,
    FocusPrevPane,
    FilesChanged(Arc<BatchedWatchEvent>),
    #[default]
    Noop,
}
