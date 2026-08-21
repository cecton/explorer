use crate::keymap::Keymap;
use crate::loader::{FileKey, LoadedFile};
use crate::session::TerminalRestoreInfo;
use crate::tui::RgbValue;
use crate::tui::file_name_picker::FileNamePickerComponent;
use crate::tui::pane_manager::{PaneManager, TextSelection, Window};
use crate::tui::theme::HelixTheme;
use crate::watcher::BatchedWatchEvent;
use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use parking_lot::Mutex;
use r3bl_tui::core::pty::{ControlledChildTerminationHandle, PtyInputEvent};
use r3bl_tui::{OfsBufVT100, Size};
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

static FILES_VERSION: AtomicU64 = AtomicU64::new(0);

/// A gap longer than this between input events (or a loss of terminal focus) is treated
/// as inactivity and excluded from the "active" time counter.
const IDLE_THRESHOLD: Duration = Duration::from_secs(60);

/// Tracks time spent in the app, split into "active" (excluding inactivity) and "total"
/// (wall-clock) for both the current session and, cumulatively, the whole project.
///
/// Accrual is a monotonic "heartbeat with timeout": every settle point adds the elapsed
/// wall time to `active_accum` only while the terminal is focused and the last real input
/// was within [`IDLE_THRESHOLD`]. The 1s `AppSignal::Noop` tick calls [`Self::tick`], so
/// the counters advance live without a dedicated timer.
#[derive(Clone, Debug)]
pub struct SessionTime {
    /// When this session (app launch) began.
    session_start: Instant,
    /// Timestamp of the last real input event.
    last_activity: Instant,
    /// Last point `active_accum` was settled up to.
    last_tick: Instant,
    /// Active time accumulated so far this session.
    active_accum: Duration,
    /// Whether the terminal window currently has focus.
    focused: bool,
    /// Active time carried over from previous sessions (loaded from the session file).
    prior_active: Duration,
    /// Total time carried over from previous sessions (loaded from the session file).
    prior_total: Duration,
}

impl Default for SessionTime {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            session_start: now,
            last_activity: now,
            last_tick: now,
            active_accum: Duration::ZERO,
            focused: true,
            prior_active: Duration::ZERO,
            prior_total: Duration::ZERO,
        }
    }
}

impl SessionTime {
    /// Fold the wall time since the last settle into `active_accum`, but only while the
    /// user is focused and was recently active. Idle stretches (and unfocused time) are
    /// silently dropped, so active time never counts inactivity — and never decreases.
    fn settle(&mut self, now: Instant) {
        let delta = now.saturating_duration_since(self.last_tick);
        if self.focused && now.saturating_duration_since(self.last_activity) <= IDLE_THRESHOLD {
            self.active_accum += delta;
        }
        self.last_tick = now;
    }

    /// Record a real input event (keyboard, mouse, resize, paste).
    pub fn on_input(&mut self) {
        let now = Instant::now();
        self.settle(now);
        self.last_activity = now;
    }

    /// Record a terminal focus change. Losing focus immediately stops active accrual;
    /// regaining it restarts the idle clock (the away period is never counted).
    pub fn on_focus(&mut self, gained: bool) {
        let now = Instant::now();
        self.settle(now);
        self.focused = gained;
        if gained {
            self.last_activity = now;
        }
    }

    /// Advance the counters (called from the 1s tick) without registering activity.
    pub fn tick(&mut self) {
        self.settle(Instant::now());
    }

    /// Treat a terminal-output render frame as recent activity, so streaming output
    /// keeps the active counter alive even without keyboard input. Same effect as
    /// [`Self::on_input`]; gated by the `render-counts-as-active` config at the call site.
    pub fn note_render(&mut self) {
        let now = Instant::now();
        self.settle(now);
        self.last_activity = now;
    }

    /// Seed the cumulative project totals restored from the session file.
    pub fn set_prior(&mut self, active: Duration, total: Duration) {
        self.prior_active = active;
        self.prior_total = total;
    }

    /// Active time this session (excludes inactivity).
    pub fn session_active(&self) -> Duration {
        self.active_accum
    }

    /// Wall-clock time this session (includes inactivity).
    pub fn session_total(&self) -> Duration {
        self.session_start.elapsed()
    }

    /// Cumulative active time across all sessions for this project.
    pub fn project_active(&self) -> Duration {
        self.prior_active + self.active_accum
    }

    /// Cumulative wall-clock time across all sessions for this project.
    pub fn project_total(&self) -> Duration {
        self.prior_total + self.session_total()
    }
}

#[cfg(test)]
mod session_time_tests {
    use super::*;

    fn tracker_at(now: Instant) -> SessionTime {
        SessionTime {
            session_start: now,
            last_activity: now,
            last_tick: now,
            active_accum: Duration::ZERO,
            focused: true,
            prior_active: Duration::ZERO,
            prior_total: Duration::ZERO,
        }
    }

    #[test]
    fn settle_counts_recent_activity_but_drops_idle_gaps() {
        let t0 = Instant::now();
        let mut st = tracker_at(t0);

        // A settle 10s after the last activity is within the threshold -> counted.
        st.settle(t0 + Duration::from_secs(10));
        assert_eq!(st.active_accum, Duration::from_secs(10));

        // New activity at t=10s, then a long silence.
        st.last_activity = t0 + Duration::from_secs(10);
        // Settle at t=100s: the 90s gap since last activity exceeds the threshold, so the
        // idle stretch contributes nothing.
        st.settle(t0 + Duration::from_secs(100));
        assert_eq!(st.active_accum, Duration::from_secs(10));
    }

    #[test]
    fn unfocused_time_is_never_active() {
        let t0 = Instant::now();
        let mut st = tracker_at(t0);
        st.focused = false;
        st.settle(t0 + Duration::from_secs(5));
        assert_eq!(st.active_accum, Duration::ZERO);
    }

    #[test]
    fn note_render_keeps_active_accruing_past_idle() {
        let t0 = Instant::now();
        let mut st = tracker_at(t0);

        // No input for 90s: a plain settle drops the idle stretch (nothing accrues).
        st.settle(t0 + Duration::from_secs(90));
        assert_eq!(st.active_accum, Duration::ZERO);

        // A render frame refreshes last_activity to "now" (its internal Instant::now),
        // so from that point the idle clock restarts.
        st.note_render();
        let base = st.active_accum;
        // Settle 10s past that render: within the idle threshold -> the 10s counts,
        // even though no key was ever pressed.
        st.settle(st.last_tick + Duration::from_secs(10));
        assert_eq!(st.active_accum, base + Duration::from_secs(10));
    }

    #[test]
    fn project_totals_add_prior_sessions() {
        let t0 = Instant::now();
        let mut st = tracker_at(t0);
        st.set_prior(Duration::from_secs(100), Duration::from_secs(200));
        st.settle(t0 + Duration::from_secs(10));
        assert_eq!(st.project_active(), Duration::from_secs(110));
        assert!(st.project_total() >= Duration::from_secs(200));
    }
}

#[derive(Clone, Debug)]
pub struct FuzzyPickerState<T> {
    pub query: String,
    pub results: Vec<(T, Vec<u32>)>,
    pub selected: Option<T>,
}

// Manual `Default` avoids the `T: Default` bound the derive would add; the
// picker's fields (`String`, `Vec`, `Option`) are all `Default` for any `T`.
impl<T> Default for FuzzyPickerState<T> {
    fn default() -> Self {
        Self {
            query: String::new(),
            results: Vec::new(),
            selected: None,
        }
    }
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
    pub file_name_picker: FuzzyPickerState<Window>,
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
    /// Active/total time tracking for the session and project time counters.
    pub time: SessionTime,
    /// Whether the status-bar time counters are rendered (config `counters.show`).
    pub show_timers: bool,
    /// Whether terminal output counts as active time even without keyboard input
    /// (config `counters.render-counts-as-active`).
    pub count_render_as_active: bool,
    pub symbol_highlights: Vec<SymbolHighlightGroup>,
    pub next_palette_index: usize,
    /// Per-file regex search (vim-style `/` `?` `n` `N`). Transient — never persisted to the
    /// session file, mirroring `text_selection`.
    pub search: HashMap<FileKey, PreviewSearch>,
    /// Configurable top-level (leader + global) key bindings.
    pub keymap: Keymap,
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

    pub fn with_terminal<R>(&self, id: usize, f: impl FnOnce(&TerminalPane) -> R) -> Option<R> {
        self.terminal_panes.get(&id).map(|p| f(&p.lock()))
    }

    pub fn with_terminal_mut<R>(
        &self,
        id: usize,
        f: impl FnOnce(&mut TerminalPane) -> R,
    ) -> Option<R> {
        self.terminal_panes.get(&id).map(|p| f(&mut p.lock()))
    }
}

impl AppState {
    pub fn new(
        files: Arc<ArcSwap<Vec<Arc<LoadedFile>>>>,
        root: Utf8PathBuf,
        theme: HelixTheme,
        keymap: Keymap,
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
            time: SessionTime::default(),
            show_timers: true,
            count_render_as_active: true,
            symbol_highlights: Vec::new(),
            next_palette_index: 0,
            search: HashMap::new(),
            keymap,
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
    SymbolHighlightResult {
        qualified_name: String,
        group_id: Option<usize>,
        origin_file_idx: usize,
        origin_line: u32,
        origin_char: u32,
        origin_word: Option<String>,
        origin_locations: Vec<SymbolRefLocation>,
        reference_locations: Vec<SymbolRefLocation>,
    },
    /// Forward a raw escape sequence (e.g. an OSC 52 clipboard copy emitted by an app
    /// running in a terminal pane) to explorer's own stdout, so the host terminal
    /// handles it. Written to the real terminal on the main thread in `app_handle_signal`.
    ForwardOscToTerminal(Vec<u8>),
    /// A terminal pane produced output and was re-rendered. Handled like [`Self::Noop`]
    /// for tick/flush bookkeeping, but additionally registers activity when
    /// `count_render_as_active` is set, so streaming output keeps the active counter alive.
    TerminalOutput,
    #[default]
    Noop,
}

#[derive(Clone, Debug)]
pub struct SymbolRefLocation {
    pub file_key: FileKey,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Clone, Debug)]
pub struct SymbolHighlightGroup {
    pub qualified_name: String,
    pub origin_file: FileKey,
    pub origin_line: u32,
    pub origin_char: u32,
    /// Word originally clicked, re-sent on rebuild so the PlainText-hover
    /// fallback synthesizes the same `parent::word` qualified name.
    pub origin_word: Option<String>,
    pub origin_byte_start: Option<usize>,
    pub origin_byte_end: Option<usize>,
    pub color: RgbValue,
    pub locations: Vec<SymbolRefLocation>,
    pub needs_rebuild: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchDirection {
    Forward,
    Backward,
}

/// Active regex search for a single preview pane. Built on commit (`Enter`); cleared when the
/// committed pattern is empty. `regex::Regex` is `Clone` (reference-counted internally), so this
/// is fine under `AppState: Clone`.
#[derive(Clone)]
pub struct PreviewSearch {
    pub pattern: String,
    pub regex: regex::Regex,
    pub direction: SearchDirection,
    /// The match `n`/`N` currently sits on: `(line_idx, match_start_byte_within_line)`.
    pub current: Option<(usize, usize)>,
}

/// Search-match highlight background (teal) and the brighter variant for the current `n`/`N` match.
/// Both are distinct from `ui.selection` and every `SYMBOL_PALETTE` color.
pub const SEARCH_MATCH: RgbValue = RgbValue {
    red: 0,
    green: 140,
    blue: 150,
};
pub const SEARCH_CURRENT: RgbValue = RgbValue {
    red: 40,
    green: 205,
    blue: 220,
};
/// Manually-set foreground for search-match text (bypasses `ensure_readable_fg`). Inverted between
/// the two states so the current match is unmistakable: near-white text on the teal matches,
/// near-black text on the brighter-teal current match.
pub const SEARCH_MATCH_FG: RgbValue = RgbValue {
    red: 235,
    green: 235,
    blue: 235,
};
pub const SEARCH_CURRENT_FG: RgbValue = RgbValue {
    red: 20,
    green: 20,
    blue: 20,
};

pub const SYMBOL_PALETTE: [RgbValue; 8] = [
    RgbValue {
        red: 255,
        green: 100,
        blue: 100,
    },
    RgbValue {
        red: 100,
        green: 200,
        blue: 255,
    },
    RgbValue {
        red: 255,
        green: 200,
        blue: 100,
    },
    RgbValue {
        red: 150,
        green: 255,
        blue: 100,
    },
    RgbValue {
        red: 255,
        green: 150,
        blue: 255,
    },
    RgbValue {
        red: 255,
        green: 255,
        blue: 100,
    },
    RgbValue {
        red: 100,
        green: 150,
        blue: 255,
    },
    RgbValue {
        red: 200,
        green: 255,
        blue: 200,
    },
];
