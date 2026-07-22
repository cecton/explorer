use crate::loader::{FileKey, LoadedFile};
use crate::session::TerminalRestoreInfo;
use crate::tui::file_name_picker::FileNamePickerComponent;
use crate::tui::pane_manager::{PaneManager, TextSelection, Window};
use crate::tui::theme::HelixTheme;
use crate::watcher::BatchedWatchEvent;
use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use r3bl_tui::core::pty::{ControlledChildTerminationHandle, PtyInputEvent};
use r3bl_tui::{OfsBufVT100, Size};
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::mpsc;

static FILES_VERSION: AtomicU64 = AtomicU64::new(0);

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
    pub ofs_buf: OfsBufVT100,
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
    /// How many lines back from the bottom of the terminal the viewport is scrolled.
    /// 0 means showing the current buffer (bottom); >0 shows scrollback history.
    pub scroll_offset: usize,
    /// Working directory of the terminal process.
    pub cwd: Utf8PathBuf,
    /// Command used to start the terminal process, if any.
    pub command: Option<String>,
}

impl Debug for TerminalPane {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalPane")
            .field("ofs_buf", &self.ofs_buf)
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
            .field("scroll_offset", &self.scroll_offset)
            .field("cwd", &self.cwd)
            .field("command", &self.command)
            .finish()
    }
}

#[derive(Clone, Default)]
pub struct AppState {
    pub files: Arc<ArcSwap<Vec<Arc<LoadedFile>>>>,
    pub files_version: u64,
    pub root: Utf8PathBuf,
    /// Pane stack, sizes, layout, and focus state.
    pub pane_manager: PaneManager,
    /// Last known surface size used for on-demand layout recomputation.
    pub last_surface_size: Size,
    /// Per-file highlight ranges (1-indexed, inclusive).
    pub highlight_ranges: HashMap<FileKey, Vec<(usize, usize)>>,
    pub leader_active: bool,
    pub command_mode_active: bool,
    pub file_name_picker: FuzzyPickerState<FileKey>,
    pub theme_picker: FuzzyPickerState<String>,
    pub theme: HelixTheme,
    pub saved_theme: HelixTheme,
    /// Terminal panes keyed by their unique ID.
    pub terminal_panes: HashMap<usize, Arc<Mutex<TerminalPane>>>,
    /// Next available terminal pane ID.
    pub next_terminal_id: usize,
    /// Terminal windows restored from the session that still need a PTY spawned.
    pub pending_terminals: HashMap<usize, TerminalRestoreInfo>,
    /// Maps editor terminal IDs back to the FileKey they should restore on exit.
    pub terminal_to_preview: HashMap<usize, FileKey>,
    pub mouse_drag_active: bool,
    pub terminal_grabbed: bool,
    pub text_selection: Option<TextSelection>,
    pub session_dirty_at: Option<Instant>,
}

impl AppState {
    pub fn bump_files_version(&mut self) {
        self.files_version = FILES_VERSION.fetch_add(1, Ordering::Relaxed) + 1;
    }

    pub fn mark_session_dirty(&mut self) {
        self.session_dirty_at = Some(Instant::now());
    }

    pub fn recompute_file_name_picker_results(&mut self) {
        self.file_name_picker.results = FileNamePickerComponent::compute_results(self);
    }
}

impl AppState {
    pub fn new(
        files: Arc<ArcSwap<Vec<Arc<LoadedFile>>>>,
        root: Utf8PathBuf,
        theme: HelixTheme,
    ) -> Self {
        let saved_theme = theme.clone();
        let mut pane_manager = PaneManager::new();
        pane_manager.push_window(Window::FileNamePicker);
        pane_manager.focused_window = Some(Window::FileNamePicker);

        let mut state = Self {
            files,
            files_version: 0,
            root,
            pane_manager,
            last_surface_size: Size::default(),
            highlight_ranges: HashMap::new(),
            leader_active: false,
            command_mode_active: false,
            file_name_picker: FuzzyPickerState::default(),
            theme_picker: FuzzyPickerState::default(),
            theme,
            saved_theme,
            terminal_panes: HashMap::new(),
            next_terminal_id: 0,
            pending_terminals: HashMap::new(),
            terminal_to_preview: HashMap::new(),
            mouse_drag_active: false,
            terminal_grabbed: false,
            text_selection: None,
            session_dirty_at: None,
        };
        state.recompute_file_name_picker_results();

        let all_themes: Vec<(String, Vec<u32>)> = HelixTheme::theme_names()
            .map(|n| (n.to_string(), Vec::new()))
            .collect();
        state.theme_picker.selected = all_themes
            .iter()
            .position(|(n, _)| n == state.theme.name())
            .and_then(|i| all_themes.get(i).map(|(n, _)| n.clone()));
        state.theme_picker.results = all_themes;

        state
    }
}

impl Debug for AppState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let count = self.files.load().len();
        write!(
            f,
            "AppState {{ files: {}, stack: {:?}, focused: {:?} }}",
            count, self.pane_manager.window_stack, self.pane_manager.focused_window
        )
    }
}

impl Display for AppState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "AppState[files={}]", self.files.load().len())
    }
}

#[derive(Default, Clone, Debug)]
#[non_exhaustive]
pub enum AppSignal {
    FilesChanged(Arc<BatchedWatchEvent>),
    /// Open a terminal pane. `cmd = None` means an interactive shell; `cmd = Some(s)` runs
    /// `/bin/sh -c s`. `cwd` is the working directory for the child process.
    OpenTerminal {
        cmd: Option<String>,
        cwd: Utf8PathBuf,
    },
    /// Open an embedded editor terminal that replaces the FilePreview in-place.
    OpenEditor {
        cmd: String,
        cwd: Utf8PathBuf,
        file_key: FileKey,
    },
    #[default]
    Noop,
}
