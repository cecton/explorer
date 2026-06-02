use crate::loader::{FileKey, LoadedFile};
use crate::tui::theme::HelixTheme;
use crate::watcher::BatchedWatchEvent;
use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use r3bl_tui::core::pty::{
    ControlledChildTerminationHandle, CursorKeyMode, MouseTrackingMode, PtyInputEvent,
};
use r3bl_tui::{FlexBox, OffscreenBuffer, Size};
use std::collections::{HashMap, HashSet};
use std::fmt::{Debug, Display, Formatter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub const MAX_PANES: usize = 5;

static FILES_VERSION: AtomicU64 = AtomicU64::new(0);

/// A pane that can appear in the window stack.
///
/// Each variant is unique: there is at most one `FileNamePicker` and at most one
/// `FilePreview` per `FileKey` in the stack at any time.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Window {
    FilePreview(FileKey),
    FileNamePicker,
    ThemePicker,
    Terminal(usize),
}

/// Scroll and page-size state for a single window pane.
#[derive(Clone, Debug, Default)]
pub struct WindowState {
    pub scroll: usize,
    pub page_size: usize,
    pub scroll_max: usize,
}

#[derive(Clone, Debug, Default)]
pub struct FuzzyPickerState<T> {
    pub query: String,
    pub results: Vec<(T, Vec<u32>)>,
    pub selected: Option<T>,
}

impl<T: Clone + PartialEq> FuzzyPickerState<T> {
    pub fn reset(&mut self) {
        self.results.clear();
        self.selected = None;
        self.query.clear();
    }

    pub fn resolve_selected_index(&self) -> usize {
        let key = match &self.selected {
            None => return 0,
            Some(k) => k,
        };
        self.results
            .iter()
            .position(|(result_key, _)| result_key == key)
            .unwrap_or(0)
    }
}

pub struct TerminalPane {
    pub ofs_buf: OffscreenBuffer,
    pub cursor_key_mode: CursorKeyMode,
    pub mouse_tracking_mode: MouseTrackingMode,
    pub title: Option<String>,
    pub pty_input_tx: Arc<mpsc::Sender<PtyInputEvent>>,
    pub child_killer: Option<ControlledChildTerminationHandle>,
    pub last_size: Size,
    /// True when this pane was opened via `:!<cmd>` rather than as an interactive shell.
    /// Command panes are dismissed by Esc or Enter instead of being auto-closed on PTY exit.
    pub is_command_pane: bool,
    /// Set to true when the PTY process has exited; pane stays visible until dismissed.
    pub exited: bool,
    /// Exit code of the child process, set when `exited` becomes true.
    pub exit_code: Option<u32>,
    /// Signal name (e.g. "SIGSEGV") if the process was terminated by a signal.
    pub exit_signal: Option<String>,
}

impl Debug for TerminalPane {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalPane")
            .field("ofs_buf", &self.ofs_buf)
            .field("cursor_key_mode", &self.cursor_key_mode)
            .field("mouse_tracking_mode", &self.mouse_tracking_mode)
            .field("title", &self.title)
            .field("pty_input_tx", &"Sender<..>")
            .field(
                "child_killer",
                &self.child_killer.as_ref().map(|_| "ChildKiller<..>"),
            )
            .field("last_size", &self.last_size)
            .field("is_command_pane", &self.is_command_pane)
            .field("exited", &self.exited)
            .field("exit_code", &self.exit_code)
            .field("exit_signal", &self.exit_signal)
            .finish()
    }
}

#[derive(Clone, Default)]
pub struct State {
    pub files: Arc<ArcSwap<Vec<LoadedFile>>>,
    pub files_version: u64,
    pub root: Utf8PathBuf,
    /// Stack of open windows, most-recently-opened first (index 0 = leftmost pane).
    pub window_stack: Vec<Window>,
    /// The window that currently receives keyboard input.
    pub focused_window: Option<Window>,
    /// Per-window scroll and page-size state.
    pub window_states: HashMap<Window, WindowState>,
    /// Per-file highlight ranges (1-indexed, inclusive).
    pub highlight_ranges: HashMap<FileKey, Vec<(usize, usize)>>,
    pub leader_active: bool,
    pub command_mode_active: bool,
    pub file_name_picker: FuzzyPickerState<FileKey>,
    pub theme_picker: FuzzyPickerState<String>,
    pub theme: HelixTheme,
    pub saved_theme: HelixTheme,
    /// Current `FlexBox` for each pane slot (index 0..MAX_PANES).
    pub pane_boxes: [FlexBox; MAX_PANES],
    /// Terminal panes keyed by their unique ID. Behind `Arc<Mutex<>>` so the
    /// background task can call `apply_ansi_bytes` off the main thread.
    pub terminal_panes: HashMap<usize, Arc<Mutex<TerminalPane>>>,
    /// Next available terminal pane ID.
    pub next_terminal_id: usize,
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
        let count = (surface_cols / MIN_PANE_WIDTH).max(1) as usize;
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

    pub fn window_scroll_max(&self, window: &Window) -> usize {
        self.window_states
            .get(window)
            .map(|s| s.scroll_max)
            .unwrap_or(0)
    }

    pub fn set_window_scroll_max(&mut self, window: &Window, scroll_max: usize) {
        self.window_states
            .entry(window.clone())
            .or_default()
            .scroll_max = scroll_max;
    }

    pub fn clamp_scroll(&mut self, window: &Window) {
        let state = self.window_states.get(window);
        let (scroll, page_size, scroll_max) = match state {
            Some(s) => (s.scroll, s.page_size, s.scroll_max),
            None => return,
        };
        if scroll_max > page_size {
            let clamped = scroll.min(scroll_max - page_size);
            self.window_states.get_mut(window).unwrap().scroll = clamped;
        }
    }
}

impl State {
    pub fn new(files: Arc<ArcSwap<Vec<LoadedFile>>>, root: Utf8PathBuf, theme: HelixTheme) -> Self {
        let snapshot = files.load();
        let saved_theme = theme.clone();
        let mut state = Self {
            files,
            files_version: 0,
            root,
            window_stack: vec![Window::FileNamePicker],
            focused_window: Some(Window::FileNamePicker),
            window_states: HashMap::new(),
            highlight_ranges: HashMap::new(),
            leader_active: false,
            command_mode_active: false,
            file_name_picker: FuzzyPickerState::default(),
            theme_picker: FuzzyPickerState::default(),
            theme,
            saved_theme,
            pane_boxes: [FlexBox::default(); MAX_PANES],
            terminal_panes: HashMap::new(),
            next_terminal_id: 0,
        };
        state.file_name_picker.results = {
            let mut seen: HashSet<usize> = HashSet::new();
            let mut results = Vec::new();
            for window in &state.window_stack {
                if let Window::FilePreview(key) = window
                    && !snapshot[key.0].removed.load(Ordering::Relaxed)
                    && seen.insert(key.0)
                {
                    results.push((*key, vec![]));
                }
            }
            results
        };
        state
            .window_states
            .insert(Window::FileNamePicker, WindowState::default());
        state
    }
}

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
    FileNamePickerQueryChanged,
    FilesChanged(Arc<BatchedWatchEvent>),
    /// Open a terminal pane. `cmd = None` means an interactive shell; `cmd = Some(s)` runs
    /// `/bin/sh -c s`. `cwd` is the working directory for the child process.
    OpenTerminal {
        cmd: Option<String>,
        cwd: Utf8PathBuf,
    },
    #[default]
    Noop,
}
