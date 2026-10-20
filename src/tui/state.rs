use crate::agent::mcp::{McpHost, McpServerDecl};
use crate::agent::turn::{TurnHandle, TurnOutcome};
use crate::agent::usage::Usage;
use crate::agent::{AgentConfig, Effort, ModelSpec};
use crate::conversation::{ConversationTree, NodeId};
use crate::git::{DiffLine, GitRef, LogHistory};
use crate::keymap::Keymap;
use crate::loader::{FileKey, LoadedFile};
use crate::session::TerminalRestoreInfo;
use crate::tui::RgbValue;
use crate::tui::input_line::InputLine;
use crate::tui::pane_manager::{DocPart, PaneManager, TextSelection, Window};
use crate::tui::theme::HelixTheme;
use crate::watcher::BatchedWatchEvent;
use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use parking_lot::{Mutex, RwLock};
use r3bl_tui::core::pty::{ControlledChildTerminationHandle, PtyInputEvent};
use r3bl_tui::{MouseInput, OfsBufVT100, Size};
use std::collections::{HashMap, HashSet};
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
    pub query: InputLine,
    pub results: Vec<(T, Vec<u32>)>,
    pub selected: Option<T>,
}

// Manual `Default` avoids the `T: Default` bound the derive would add; the
// picker's fields (`InputLine`, `Vec`, `Option`) are all `Default` for any `T`.
impl<T> Default for FuzzyPickerState<T> {
    fn default() -> Self {
        Self {
            query: InputLine::new(),
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

/// What a git log window shows the history of. A commit source points the walk at
/// that commit, so the newest entry is the commit itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogSource {
    #[default]
    Head,
    Branch(String),
    Commit(String),
}

#[derive(Clone, Default)]
pub struct GitLogWindowState {
    pub entries: Option<Arc<LogHistory>>,
    pub error: Option<String>,
    pub selected: usize,
    pub query: InputLine,
    /// Whether the `/` input is open in the title bar.
    pub search_active: bool,
    /// Some = query active: (entry index, nucleo match positions), relevance order.
    pub filtered: Option<Vec<(usize, Vec<u32>)>>,
    /// Where the history walk starts.
    pub source: LogSource,
    pub branch_picking: bool,
    pub branch_picker: FuzzyPickerState<usize>,
}

/// View state of the Conversation Tree window.
#[derive(Clone, Default)]
pub struct TreeViewState {
    pub selected: Option<RowTarget>,
    /// Roots and forks whose children are shown; everything else is collapsed.
    pub expanded: HashSet<NodeId>,
    /// Nodes whose sub-agent runs are listed under them. Never saved: they are hidden
    /// at startup.
    pub shown_agents: HashSet<NodeId>,
    /// Node a first `Ctrl+D` marked for deletion; a second one on it deletes.
    pub armed_delete: Option<NodeId>,
    /// False until the startup load of the tree has finished.
    pub loaded: bool,
    /// Last failure to report in the status bar, cleared by the next key.
    pub notice: Option<String>,
    /// The `/` filter: while it holds a word, the window lists the matching nodes flat.
    pub query: InputLine,
    /// Whether the `/` input is open in the title bar.
    pub search_active: bool,
    /// Row selected when `/` was opened, given back when the filter matches nothing.
    pub before_search: Option<RowTarget>,
    /// The `:` command line, while it is open.
    pub command: Option<InputLine>,
    /// MCP servers added from the command line whose start is still to be reported.
    pub announced_starts: HashSet<String>,
}

impl TreeViewState {
    pub fn filtering(&self) -> bool {
        !self.query.text().trim().is_empty()
    }
}

/// State of one Discussion window.
#[derive(Clone, Default)]
pub struct DiscussionState {
    /// Node shown; `None` for a new conversation whose first prompt is not sent yet.
    pub node: Option<NodeId>,
    /// Sub-agent run of `node` shown instead of the node itself, read-only.
    pub agent: Option<u32>,
    pub prompt: String,
    pub reply: String,
    /// Work log of the node's turn: narration and tool calls, never part of the reply.
    pub work_log: Vec<String>,
    /// Work log entries of the current round, synced from the running turn.
    pub visible_work_log: Vec<String>,
    /// Thinking of the running turn, streamed dimmed; empty once the turn is over.
    pub thinking: String,
    /// Whether the work log is shown, once toggled with Ctrl+O; `None` shows it only
    /// while the node has no finished reply.
    pub show_work_log: Option<bool>,
    /// False until the node's prompt and reply have been read from disk.
    pub loaded: bool,
    /// The next prompt, as typed so far.
    pub draft: InputLine,
    /// Model the next prompt is sent to.
    pub model: Option<ModelSpec>,
    /// Why the last send was refused; cleared by the next edit.
    pub notice: Option<String>,
}

/// A prompt waiting for the running turn to end. Its text is not copied: it stays the
/// draft of `window`, editable until the turn starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingTurn {
    pub window: usize,
    /// Node the window showed when the prompt was queued.
    pub parent: Option<NodeId>,
}

/// What a Conversation Tree row points at: a node, a sub-agent run of a node, or a
/// queued prompt whose node does not exist yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowTarget {
    Node(NodeId),
    /// Sub-agent run `index` of `node`.
    Agent {
        node: NodeId,
        index: u32,
    },
    /// A prompt waiting in the queue, by the window that holds it. `position` is its
    /// place in the queue, shown in the gutter.
    Queued {
        window: usize,
        position: usize,
    },
}

impl RowTarget {
    /// The node this row shows, when it has one.
    pub fn node(&self) -> Option<&NodeId> {
        match self {
            Self::Node(id) => Some(id),
            Self::Agent { .. } | Self::Queued { .. } => None,
        }
    }
}

/// Highlighted byte ranges (start inclusive, end exclusive) in the prompt and the reply
/// of one node.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NodeHighlights {
    #[serde(default)]
    pub prompt: Vec<(usize, usize)>,
    #[serde(default)]
    pub reply: Vec<(usize, usize)>,
}

impl NodeHighlights {
    pub fn part(&self, part: DocPart) -> &[(usize, usize)] {
        match part {
            DocPart::Prompt => &self.prompt,
            DocPart::Reply => &self.reply,
            DocPart::WorkLog | DocPart::Thinking => &[],
        }
    }

    /// The ranges of `part`; the work log has none, it is not kept on screen.
    pub fn part_mut(&mut self, part: DocPart) -> Option<&mut Vec<(usize, usize)>> {
        match part {
            DocPart::Prompt => Some(&mut self.prompt),
            DocPart::Reply => Some(&mut self.reply),
            DocPart::WorkLog | DocPart::Thinking => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.prompt.is_empty() && self.reply.is_empty()
    }
}

/// What a pick in the Model Picker applies to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelPickerPurpose {
    /// The next prompt of a Discussion window.
    Window(usize),
    /// A new sibling of this node, running the same prompt with the picked model.
    Duplicate(NodeId),
}

/// Models offered by the Model Picker.
#[derive(Clone, Default)]
pub struct ModelCatalog {
    /// Result of the last `/models` fetch of each endpoint, by endpoint name.
    pub lists: HashMap<String, Result<Vec<String>, String>>,
    /// Every `endpoint × model × effort` combination, in display order.
    pub specs: Vec<ModelSpec>,
}

impl ModelCatalog {
    pub fn set_list(
        &mut self,
        agent: &AgentConfig,
        endpoint: &str,
        result: Result<Vec<String>, String>,
    ) {
        self.lists.insert(endpoint.to_string(), result);
        self.specs.clear();
        for endpoint in &agent.endpoints {
            let Some(Ok(models)) = self.lists.get(&endpoint.name) else {
                continue;
            };
            for model in models {
                for effort in Effort::ALL {
                    self.specs.push(ModelSpec {
                        endpoint: endpoint.name.clone(),
                        model: model.clone(),
                        effort,
                    });
                }
            }
        }
    }

    /// The fetch failures, as `endpoint: reason`.
    pub fn errors(&self) -> Vec<String> {
        let mut errors: Vec<String> = self
            .lists
            .iter()
            .filter_map(|(name, result)| result.as_ref().err().map(|e| format!("{name}: {e}")))
            .collect();
        errors.sort();
        errors
    }
}

#[derive(Clone, Default)]
pub struct CommitDiffState {
    pub lines: Option<Arc<Vec<DiffLine>>>,
    pub error: Option<String>,
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
    /// Last time this pane's PTY produced output, updated by the feed task while it
    /// already holds the pane lock. Lets the time counters treat streaming output as
    /// activity without depending on the render-signal backoff path (which goes silent
    /// while the main-thread channel is saturated).
    pub last_output: Instant,
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
    /// Shared git repository handle, opened once at startup and cheaply cloned
    /// for each background git operation instead of re-opening `root` every
    /// time. `None` only if opening fails (e.g. permissions) — git.rs call
    /// sites already handle this gracefully via `Result<_, String>`.
    pub repo: Option<gix::ThreadSafeRepository>,
    /// Pane stack, sizes, layout, and focus state.
    pub pane_manager: PaneManager,
    /// Last known surface size used for on-demand layout recomputation.
    pub last_surface_size: Size,
    /// Per-file highlight ranges (1-indexed, inclusive).
    pub highlight_ranges: HashMap<FileKey, Vec<(usize, usize)>>,
    pub leader_active: bool,
    pub window_picker: FuzzyPickerState<Window>,
    pub theme_picker: FuzzyPickerState<String>,
    pub theme: HelixTheme,
    pub saved_theme: HelixTheme,
    /// Terminal panes keyed by their unique ID.
    pub terminal_panes: HashMap<usize, Arc<Mutex<TerminalPane>>>,
    /// Next available terminal pane ID.
    pub next_terminal_id: usize,
    /// Terminal windows restored from the session that still need a PTY spawned.
    pub pending_terminals: HashMap<usize, TerminalRestoreInfo>,
    /// Git log windows keyed by their unique ID.
    pub git_logs: HashMap<usize, GitLogWindowState>,
    /// Next available git log window ID.
    pub next_git_log_id: usize,
    /// Cached flat list of all branches and tags for the branch picker.
    pub git_refs: Option<Arc<Vec<crate::git::GitRef>>>,
    /// Git log windows restored from the session that still need a log loaded:
    /// (window id, branch ref, search query).
    pub pending_git_logs: Vec<usize>,
    /// Loaded commit diffs keyed by commit object id.
    pub commit_diffs: HashMap<[u8; 20], CommitDiffState>,
    /// Commit-diff windows restored from the session that still need a diff loaded.
    pub pending_diffs: Vec<[u8; 20]>,
    /// Maps editor terminal IDs back to the FileKey they should restore on exit.
    pub terminal_to_preview: HashMap<usize, FileKey>,
    /// Agent conversation tree, shared with the background task that loads it.
    pub conversations: Arc<RwLock<ConversationTree>>,
    pub tree_view: TreeViewState,
    /// Discussion windows keyed by their unique ID.
    pub discussions: HashMap<usize, DiscussionState>,
    /// Next available discussion window ID.
    pub next_discussion_id: usize,
    /// Highlights of the nodes shown in Discussion windows, by node.
    pub discussion_highlights: HashMap<NodeId, NodeHighlights>,
    /// Endpoints, tokens and agent settings from `config.kdl` and `auth.kdl`.
    pub agent: Arc<AgentConfig>,
    pub models: ModelCatalog,
    /// Last known usage of the subscription behind each endpoint, by endpoint name;
    /// kept in `<data_dir>/usage.json`.
    pub usage: HashMap<String, Usage>,
    /// MCP servers of the repository, as saved in its session.
    pub mcp_servers: Vec<McpServerDecl>,
    /// The running MCP servers, shared with the turns.
    pub mcp: Arc<McpHost>,
    /// Model last picked during this run, offered to new conversations.
    pub last_picked_model: Option<ModelSpec>,
    /// Model Picker entries are indexes into `models.specs`.
    pub model_picker: FuzzyPickerState<usize>,
    /// What the open Model Picker is choosing a model for.
    pub model_picker_purpose: Option<ModelPickerPurpose>,
    /// Turns in progress, by node.
    pub running_turns: HashMap<NodeId, TurnHandle>,
    /// Prompts waiting for their turn, next to start first. Never saved with the session.
    pub queued_turns: Vec<PendingTurn>,
    pub mouse_drag_active: bool,
    /// Window whose input holds the mouse selection in progress.
    pub input_drag: Option<Window>,
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
    /// Whether the repository has uncommitted changes (staged or unstaged),
    /// refreshed periodically by a background `git::is_dirty` check.
    pub is_dirty: bool,
    pub symbol_highlights: Vec<SymbolHighlightGroup>,
    pub next_palette_index: usize,
    /// Per-file regex search (vim-style `/` `?` `n` `N`). Transient — never persisted to the
    /// session file, mirroring `text_selection`.
    pub search: HashMap<FileKey, PreviewSearch>,
    /// The input open in a file preview's title bar, by file. Transient like `search`.
    pub preview_inputs: HashMap<FileKey, PreviewInput>,
    /// Configurable top-level (leader + global) key bindings.
    pub keymap: Keymap,
    /// Which Cargo features rust-analyzer analyzes; set via `:features`/`:all-features`/
    /// `:default-features` in the preview command line, persisted per-project in the
    /// session file.
    pub cargo_features: crate::lsp::CargoFeatureSelection,
}

impl AppState {
    pub fn bump_files_version(&mut self) {
        self.files_version = FILES_VERSION.fetch_add(1, Ordering::Relaxed) + 1;
    }

    pub fn mark_session_dirty(&mut self) {
        self.session_dirty_at = Some(Instant::now());
    }

    /// The input `window` shows right now, if any.
    pub fn open_input(&mut self, window: &Window) -> Option<&mut InputLine> {
        match window {
            Window::Discussion(id) => self
                .discussions
                .get_mut(id)
                .filter(|shown| shown.agent.is_none())
                .map(|shown| &mut shown.draft),
            Window::GitLog(id) => {
                let win = self.git_logs.get_mut(id)?;
                if win.branch_picking {
                    Some(&mut win.branch_picker.query)
                } else if win.search_active {
                    Some(&mut win.query)
                } else {
                    None
                }
            }
            Window::ConversationTree if self.tree_view.search_active => {
                Some(&mut self.tree_view.query)
            }
            Window::ConversationTree if self.tree_view.command.is_some() => {
                self.tree_view.command.as_mut()
            }
            Window::FilePreview(key) => {
                self.preview_inputs.get_mut(key).map(PreviewInput::line_mut)
            }
            Window::WindowPicker => Some(&mut self.window_picker.query),
            Window::ThemePicker => Some(&mut self.theme_picker.query),
            Window::ModelPicker => Some(&mut self.model_picker.query),
            Window::ConversationTree | Window::Terminal(_) | Window::CommitDiff(_) => None,
        }
    }

    /// Hands a mouse event to the input of `window`: true when the input took it. A
    /// selection it starts holds the mouse until the button is released.
    pub fn input_mouse(&mut self, window: Window, mouse: &MouseInput) -> bool {
        let Some(input) = self.open_input(&window) else {
            return false;
        };
        if !input.handle_mouse(mouse) {
            return false;
        }
        let selecting = input.selecting();
        self.input_drag = selecting.then_some(window);
        self.mouse_drag_active = selecting;
        true
    }

    /// Drops the input selection in progress, if any.
    pub fn cancel_input_drag(&mut self) {
        let Some(window) = self.input_drag.take() else {
            return;
        };
        if let Some(input) = self.open_input(&window) {
            input.cancel_select();
        }
        self.mouse_drag_active = false;
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

    /// True if any terminal pane produced output within `window`. Deliberately ignores
    /// pane focus (any pane streaming output counts); window focus is still enforced
    /// downstream by [`SessionTime::settle`].
    pub fn any_terminal_output_recently(&self, window: Duration) -> bool {
        self.terminal_panes
            .values()
            .any(|p| p.lock().last_output.elapsed() <= window)
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
        pane_manager.push_window(Window::WindowPicker);
        pane_manager.focused_window = Some(Window::WindowPicker);
        let repo = gix::ThreadSafeRepository::open(root.as_std_path()).ok();
        let mcp = Arc::new(McpHost::new(root.clone()));

        let mut state = Self {
            files,
            files_version: 0,
            root,
            repo,
            pane_manager,
            last_surface_size: Size::default(),
            highlight_ranges: HashMap::new(),
            leader_active: false,
            window_picker: FuzzyPickerState::default(),
            theme_picker: FuzzyPickerState::default(),
            theme,
            saved_theme,
            terminal_panes: HashMap::new(),
            next_terminal_id: 0,
            pending_terminals: HashMap::new(),
            git_logs: HashMap::new(),
            next_git_log_id: 0,
            git_refs: None,
            pending_git_logs: Vec::new(),
            commit_diffs: HashMap::new(),
            pending_diffs: Vec::new(),
            terminal_to_preview: HashMap::new(),
            conversations: Arc::default(),
            tree_view: TreeViewState::default(),
            discussions: HashMap::new(),
            next_discussion_id: 0,
            discussion_highlights: HashMap::new(),
            agent: Arc::default(),
            models: ModelCatalog::default(),
            usage: HashMap::new(),
            mcp_servers: Vec::new(),
            mcp,
            last_picked_model: None,
            model_picker: FuzzyPickerState::default(),
            model_picker_purpose: None,
            running_turns: HashMap::new(),
            queued_turns: Vec::new(),
            mouse_drag_active: false,
            input_drag: None,
            terminal_grabbed: false,
            text_selection: None,
            session_dirty_at: None,
            time: SessionTime::default(),
            show_timers: true,
            count_render_as_active: true,
            is_dirty: false,
            symbol_highlights: Vec::new(),
            next_palette_index: 0,
            search: HashMap::new(),
            preview_inputs: HashMap::new(),
            keymap,
            cargo_features: crate::lsp::CargoFeatureSelection::default(),
        };

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
    /// Change which Cargo features rust-analyzer analyzes. rust-analyzer doesn't
    /// hot-reload `cargo.*` settings, so applying this restarts the LSP worker
    /// (deferred until the old one has actually exited — see `app_handle_signal`).
    SetCargoFeatures(crate::lsp::CargoFeatureSelection),
    /// Result of an async `git::load_log` run for git log window `id`.
    GitLogLoaded {
        id: usize,
        result: Result<Arc<LogHistory>, String>,
    },
    /// Result of an async `git::load_refs` run (branch picker list).
    GitRefsLoaded(Result<Arc<Vec<GitRef>>, String>),
    /// Result of an async `git::load_commit_diff` run for `id`.
    CommitDiffLoaded {
        id: [u8; 20],
        result: Result<Arc<Vec<DiffLine>>, String>,
    },
    /// Result of an async `git::is_dirty` check.
    DirtyStatusLoaded(Result<bool, String>),
    /// The watcher saw `.git/HEAD`, `.git/refs/**` or `.git/packed-refs` change:
    /// the git history or the refs moved since the last knowledge of it, the git
    /// log windows and the branch picker list are stale until reloaded.
    GitHistoryChanged,
    /// The startup load filled `AppState::conversations`.
    ConversationsLoaded,
    /// Result of fetching the model list of `endpoint`.
    ModelsFetched {
        endpoint: String,
        result: Result<Vec<String>, String>,
    },
    /// How much of the subscription behind `endpoint` is used, as known when a turn
    /// that spent some of it ended.
    UsageFetched {
        endpoint: String,
        usage: Usage,
    },
    /// A running turn produced output; nothing to apply, only a render.
    TurnProgress,
    /// The turn of `node` started a sub-agent, already recorded in the tree.
    SubagentStarted {
        node: NodeId,
    },
    /// Sub-agent run `index` of `node` ended; its outcome is in the tree.
    SubagentFinished {
        node: NodeId,
        index: u32,
    },
    /// The MCP server `name` started, failed or is starting again.
    McpStatus {
        name: String,
    },
    /// The turn of `node` ended.
    TurnFinished {
        node: NodeId,
        outcome: TurnOutcome,
    },
    /// `title.md` of `node` was written.
    NodeTitleReady {
        node: NodeId,
        title: String,
    },
    /// `summary.md` of `node` was written.
    NodeSummaryReady {
        node: NodeId,
        summary: String,
    },
    /// Prompt and reply of `node`, read from disk for discussion window `window_id`.
    NodeContentLoaded {
        window_id: usize,
        node: NodeId,
        prompt: String,
        reply: String,
        work_log: Vec<String>,
    },
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

/// The input open in a file preview's title bar: the `:` command line or the `/` `?`
/// search, never both.
#[derive(Clone)]
pub enum PreviewInput {
    Command(InputLine),
    /// Prefilled with the file's active pattern; the direction is the key that opened it.
    Search(InputLine, SearchDirection),
}

impl PreviewInput {
    pub fn line(&self) -> &InputLine {
        match self {
            Self::Command(line) | Self::Search(line, _) => line,
        }
    }

    pub fn line_mut(&mut self) -> &mut InputLine {
        match self {
            Self::Command(line) | Self::Search(line, _) => line,
        }
    }
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

#[cfg(test)]
mod recent_output_tests {
    use super::*;

    fn dummy_pane(last_output: Instant) -> TerminalPane {
        let (pty_input_tx, _) = mpsc::channel(1);
        TerminalPane {
            ofs_buf: OfsBufVT100::new_empty(Size::default()),
            title: None,
            pty_input_tx: Arc::new(pty_input_tx),
            child_killer: None,
            last_size: Size::default(),
            is_command_pane: false,
            exited: false,
            exit_code: None,
            exit_signal: None,
            scroll_offset: 0,
            cwd: Utf8PathBuf::from("/tmp"),
            command: None,
            last_output,
        }
    }

    fn empty_state() -> AppState {
        let files = Arc::new(ArcSwap::from_pointee(Vec::<Arc<LoadedFile>>::new()));
        AppState::new(
            files,
            Utf8PathBuf::from("/tmp"),
            HelixTheme::default(),
            Keymap::default(),
        )
    }

    #[test]
    fn any_terminal_output_recently_ignores_stale_and_missing_panes() {
        let mut state = empty_state();

        // No panes at all -> nothing counts as recent output.
        assert!(!state.any_terminal_output_recently(Duration::from_secs(2)));

        // A pane whose last output is older than the window doesn't count.
        state.terminal_panes.insert(
            0,
            Arc::new(Mutex::new(dummy_pane(
                Instant::now() - Duration::from_secs(10),
            ))),
        );
        assert!(!state.any_terminal_output_recently(Duration::from_secs(2)));

        // Any pane with fresh output counts, regardless of focus.
        state
            .terminal_panes
            .insert(1, Arc::new(Mutex::new(dummy_pane(Instant::now()))));
        assert!(state.any_terminal_output_recently(Duration::from_secs(2)));
    }
}
