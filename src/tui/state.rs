#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::collections::{HashMap, HashSet};
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

static FILES_VERSION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct FuzzyPickerState<T> {
    pub query: crate::tui::widgets::input_line::InputLine,
    pub results: Vec<(T, Vec<u32>)>,
    pub selected: Option<T>,
}

// Manual `Default` avoids the `T: Default` bound the derive would add; the
// picker's fields (`InputLine`, `Vec`, `Option`) are all `Default` for any `T`.
impl<T> Default for FuzzyPickerState<T> {
    fn default() -> Self {
        use crate::tui::widgets::input_line::InputLine;
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
    pub entries: Option<Arc<crate::git::LogHistory>>,
    pub error: Option<String>,
    pub selected: usize,
    pub query: crate::tui::widgets::input_line::InputLine,
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
    pub expanded: HashSet<crate::conversation::NodeId>,
    /// Nodes whose sub-agent runs are listed under them. Never saved: they are hidden
    /// at startup.
    pub shown_agents: HashSet<crate::conversation::NodeId>,
    /// Node a first `d` marked for deletion; a second one on it deletes.
    pub armed_delete: Option<crate::conversation::NodeId>,
    /// False until the startup load of the tree has finished.
    pub loaded: bool,
    /// Last failure to report in the status bar, cleared by the next key.
    pub notice: Option<String>,
    /// The `/` filter: while it holds a word, the window lists the matching nodes flat.
    pub query: crate::tui::widgets::input_line::InputLine,
    /// Whether the `/` input is open in the title bar.
    pub search_active: bool,
    /// Row selected when `/` was opened, given back when the filter matches nothing.
    pub before_search: Option<RowTarget>,
    /// MCP servers added from the command line whose start is still to be reported.
    pub announced_starts: HashSet<String>,
    /// Nodes whose title and summary are being asked again after the cursor landed on them.
    pub retitling: HashSet<crate::conversation::NodeId>,
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
    pub node: Option<crate::conversation::NodeId>,
    /// Sub-agent run of `node` shown instead of the node itself, read-only.
    pub agent: Option<u32>,
    pub prompt: String,
    pub reply: String,
    /// Work log of the node's turn: narration and tool calls, never part of the reply.
    pub work_log: Vec<String>,
    /// First work log entry of the current round; 0 again once the next render sees no running turn.
    pub log_start: usize,
    /// Thinking of the running turn, streamed dimmed; empty once the turn is over.
    pub thinking: String,
    /// Revision of the running turn's live texts the fields above were copied from.
    pub live_revision: u64,
    /// Bumped whenever the texts are replaced as a whole (content read from disk, a
    /// prompt sent, a turn finished), which no length tells apart.
    pub content_revision: u64,
    /// Whether the work log is shown, once toggled with Ctrl+O; `None` shows it only
    /// while the node has no finished reply.
    pub show_work_log: Option<bool>,
    /// False until the node's prompt and reply have been read from disk.
    pub loaded: bool,
    /// The next prompt, as typed so far.
    pub draft: crate::tui::widgets::input_line::InputLine,
    /// Model the next prompt is sent to.
    pub model: Option<crate::model_spec::ModelSpec>,
    /// Why the last send was refused; cleared by the next edit.
    pub notice: Option<String>,
}

/// What a report window shows: a title and the markdown under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportState {
    pub title: String,
    pub markdown: String,
}

/// A prompt waiting for the running turn to end. Its text is not copied: it stays the
/// draft of `window`, editable until the turn starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingTurn {
    pub window: usize,
    /// Node the window showed when the prompt was queued.
    pub parent: Option<crate::conversation::NodeId>,
}

/// What a Conversation Tree row points at: a node, a sub-agent run of a node, or a
/// queued prompt whose node does not exist yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowTarget {
    Node(crate::conversation::NodeId),
    /// Sub-agent run `index` of `node`.
    Agent {
        node: crate::conversation::NodeId,
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
    pub fn node(&self) -> Option<&crate::conversation::NodeId> {
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

impl DiscussionState {
    /// The work log entries on screen; layout, selection and status line all read them.
    pub fn shown_work_log(&self) -> &[String] {
        if self.show_work_log == Some(true) {
            &self.work_log
        } else {
            &self.work_log[self.log_start.min(self.work_log.len())..]
        }
    }

    /// Takes what the running turn has produced since the last frame. The texts only
    /// grow over a turn and its work-log entries are never rewritten, so the copy
    /// appends what is new instead of taking the whole texts again; a text that shrank
    /// or was rewritten is taken whole, which the layout cache has to know.
    pub fn mirror(&mut self, live: &crate::agent::turn::live::LiveTurn) {
        if live.revision() == self.live_revision {
            return;
        }
        let reply_replaced = append_str(&mut self.reply, &live.reply);
        let thinking_replaced = append_str(&mut self.thinking, &live.thinking);
        let work_log_replaced = if live.work_log.len() >= self.work_log.len()
            && live.work_log.starts_with(&self.work_log)
        {
            self.work_log
                .extend_from_slice(&live.work_log[self.work_log.len()..]);
            false
        } else {
            self.work_log.clone_from(&live.work_log);
            true
        };
        if reply_replaced || thinking_replaced || work_log_replaced {
            self.content_revision += 1;
        }
        if self.prompt != live.prompt {
            self.prompt.clone_from(&live.prompt);
        }
        self.log_start = live.log_start();
        self.live_revision = live.revision();
    }

    /// Replaces what the window shows with the texts of a node or sub-agent run read
    /// from disk.
    pub fn show_content(&mut self, prompt: String, reply: String, work_log: Vec<String>) {
        self.prompt = prompt;
        self.reply = reply;
        self.work_log = work_log;
        self.thinking.clear();
        self.log_start = 0;
        self.content_revision += 1;
    }

    /// The texts a finished turn left, which the window still showed live.
    pub fn show_finished_turn(&mut self, reply: &str, work_log: &[String]) {
        self.reply.clear();
        self.reply.push_str(reply);
        self.work_log.clear();
        self.work_log.extend_from_slice(work_log);
        self.content_revision += 1;
    }
}

/// `text` takes `live` as a whole: what is new is appended, and a text that is not an
/// extension of it — a reply rewritten, a node swapped under the window — is copied and
/// reports `true`, since only a revision tells the layout cache such a change apart.
fn append_str(text: &mut String, live: &str) -> bool {
    if live.len() >= text.len() && live.starts_with(text.as_str()) {
        let added = live[text.len()..].to_string();
        text.push_str(&added);
        false
    } else {
        text.clear();
        text.push_str(live);
        true
    }
}

impl NodeHighlights {
    pub fn part(&self, part: crate::tui::pane_manager::DocPart) -> &[(usize, usize)] {
        use crate::tui::pane_manager::DocPart;
        match part {
            DocPart::Prompt => &self.prompt,
            DocPart::Reply => &self.reply,
            DocPart::WorkLog | DocPart::Thinking => &[],
        }
    }

    /// The ranges of `part`; the work log and thinking have none, highlights cover the prompt and reply.
    pub fn part_mut(
        &mut self,
        part: crate::tui::pane_manager::DocPart,
    ) -> Option<&mut Vec<(usize, usize)>> {
        use crate::tui::pane_manager::DocPart;
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
    Duplicate(crate::conversation::NodeId),
}

/// Models offered by the Model Picker.
#[derive(Clone, Default)]
pub struct ModelCatalog {
    /// Result of the last `/models` fetch of each endpoint, by endpoint name.
    pub lists: HashMap<String, Result<Vec<String>, String>>,
    /// Every `endpoint × model × effort` combination, in display order.
    pub specs: Vec<crate::model_spec::ModelSpec>,
}

impl ModelCatalog {
    pub fn set_list(
        &mut self,
        agent: &crate::agent::AgentConfig,
        endpoint: &str,
        result: Result<Vec<String>, String>,
    ) {
        use crate::model_spec::Effort;
        use crate::model_spec::ModelSpec;
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
    pub lines: Option<Arc<Vec<crate::git::DiffLine>>>,
    pub error: Option<String>,
}

pub struct TerminalPane {
    pub ofs_buf: OfsBufVT100,
    pub title: Option<String>,
    pub pty_input_tx: Arc<tokio::sync::mpsc::Sender<PtyInputEvent>>,
    pub child_killer: Option<ControlledChildTerminationHandle>,
    pub last_size: VPSize,
    /// True when this pane was opened via `!<cmd>` rather than as an interactive shell.
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
    pub cwd: camino::Utf8PathBuf,
    /// Command used to start the terminal process, if any.
    pub command: Option<String>,
    /// Set by the reader task when output reached the buffer without a render signal
    /// (the flood throttle held it back); cleared by the 1s `Tick`, which then repaints
    /// so a flooding pane still shows its progress and its final state.
    pub flush_pending: Arc<AtomicBool>,
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
    pub files: Arc<arc_swap::ArcSwap<Vec<Arc<crate::loader::LoadedFile>>>>,
    pub files_version: u64,
    pub root: camino::Utf8PathBuf,
    /// Shared git repository handle, opened once at startup and cheaply cloned
    /// for each background git operation instead of re-opening `root` every
    /// time. `None` only if opening fails (e.g. permissions) — git.rs call
    /// sites already handle this gracefully via `Result<_, String>`.
    pub repo: Option<gix::ThreadSafeRepository>,
    /// Pane stack, sizes, layout, and focus state.
    pub pane_manager: crate::tui::pane_manager::PaneManager,
    /// Last known surface size used for on-demand layout recomputation.
    pub last_surface_size: VPSize,
    /// Per-file highlight ranges (1-indexed, inclusive).
    pub highlight_ranges: HashMap<crate::loader::FileKey, Vec<(usize, usize)>>,
    pub leader_active: bool,
    pub window_picker: FuzzyPickerState<crate::tui::pane_manager::Window>,
    pub theme_picker: FuzzyPickerState<String>,
    pub theme: crate::tui::theme::HelixTheme,
    pub saved_theme: crate::tui::theme::HelixTheme,
    /// Terminal panes keyed by their unique ID.
    pub terminal_panes: HashMap<usize, Arc<parking_lot::Mutex<TerminalPane>>>,
    /// Next available terminal pane ID.
    pub next_terminal_id: usize,
    /// Terminal windows restored from the session that still need a PTY spawned.
    pub pending_terminals: HashMap<usize, crate::tui::session::TerminalRestoreInfo>,
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
    pub terminal_to_preview: HashMap<usize, crate::loader::FileKey>,
    /// Agent conversation tree, shared with the background task that loads it.
    pub conversations: Arc<parking_lot::RwLock<crate::conversation::ConversationTree>>,
    pub tree_view: TreeViewState,
    /// Discussion windows keyed by their unique ID.
    pub discussions: HashMap<usize, DiscussionState>,
    /// Next available discussion window ID.
    pub next_discussion_id: usize,
    /// Highlights of the nodes shown in Discussion windows, by node.
    pub discussion_highlights: HashMap<crate::conversation::NodeId, NodeHighlights>,
    /// Endpoints, tokens and agent settings from `config.kdl` and `auth.kdl`.
    pub agent: Arc<crate::agent::AgentConfig>,
    pub models: ModelCatalog,
    /// Last known usage of the spending behind each endpoint, by endpoint name; kept
    /// in `<data_dir>/usage.json`.
    pub usage: HashMap<String, crate::usage::Usage>,
    /// MCP servers of the repository, as saved in its session.
    pub mcp_servers: Vec<crate::agent::mcp::McpServerDecl>,
    /// The running MCP servers, shared with the turns.
    pub mcp: Arc<crate::agent::mcp::McpHost>,
    /// Markdown shown in the report windows, by window id. In memory only: a report is not
    /// saved, and leaves nothing behind when its window closes.
    pub reports: HashMap<usize, ReportState>,
    /// Next available report window ID.
    pub next_report_id: usize,
    /// Model last picked during this run, offered to new conversations.
    pub last_picked_model: Option<crate::model_spec::ModelSpec>,
    /// Model Picker entries are indexes into `models.specs`.
    pub model_picker: FuzzyPickerState<usize>,
    /// What the open Model Picker is choosing a model for.
    pub model_picker_purpose: Option<ModelPickerPurpose>,
    /// Turns in progress, by node.
    pub running_turns: HashMap<crate::conversation::NodeId, crate::agent::turn::TurnHandle>,
    /// Animation step shared by every spinner on screen; advanced by `TurnTick`.
    pub spinner_tick: u8,
    /// Prompts waiting for their turn, next to start first. Never saved with the session.
    pub queued_turns: Vec<PendingTurn>,
    /// The `after-agents` command, in memory only; replacing or clearing it kills a running one.
    pub after_agents: Option<crate::tui::after_agents::AfterAgents>,
    pub after_agents_grace: Duration,
    /// Aliases from `config.kdl`.
    pub config_aliases: Vec<crate::config::Alias>,
    /// Aliases of this repository, saved in its session; they win over the config's.
    pub project_aliases: Vec<crate::config::Alias>,
    /// The user allowed agent turns in this repository, saved in its session. Until it is
    /// set, every turn is refused.
    pub trusted: bool,
    pub next_after_agents_run: u64,
    pub mouse_drag_active: bool,
    /// Window whose input holds the mouse selection in progress.
    pub input_drag: Option<crate::tui::pane_manager::Window>,
    pub terminal_grabbed: bool,
    pub text_selection: Option<crate::tui::pane_manager::TextSelection>,
    pub session_dirty: bool,
    /// Whether the repository has uncommitted changes (staged or unstaged),
    /// refreshed periodically by a background `git::is_dirty` check.
    pub is_dirty: bool,
    pub symbol_highlights: Vec<SymbolHighlightGroup>,
    pub next_palette_index: usize,
    /// Per-file regex search (vim-style `/` `?` `n` `N`). Transient — never persisted to the
    /// session file, mirroring `text_selection`.
    pub search: HashMap<crate::loader::FileKey, PreviewSearch>,
    /// The input open in a file preview's title bar, by file. Transient like `search`.
    pub preview_inputs: HashMap<crate::loader::FileKey, PreviewInput>,
    /// The `<leader>:` command line, open over the status bar. Transient.
    pub command_line: Option<crate::tui::command_line::CommandLine>,
    /// Configurable top-level (leader + global) key bindings.
    pub keymap: crate::tui::keymap::Keymap,
    /// Which Cargo features rust-analyzer analyzes; set via `features`/`all-features`/
    /// `default-features` on the global command line, persisted per-project in the
    /// session file.
    pub cargo_features: crate::config::CargoFeatureSelection,
}

impl AppState {
    pub fn bump_files_version(&mut self) {
        self.files_version = FILES_VERSION.fetch_add(1, Ordering::Relaxed) + 1;
    }

    pub fn mark_session_dirty(&mut self) {
        self.session_dirty = true;
    }

    /// The input `window` shows right now, if any.
    pub fn open_input(
        &mut self,
        window: &crate::tui::pane_manager::Window,
    ) -> Option<&mut crate::tui::widgets::input_line::InputLine> {
        use crate::tui::pane_manager::Window;
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
            Window::FilePreview(key) => {
                self.preview_inputs.get_mut(key).map(PreviewInput::line_mut)
            }
            Window::WindowPicker => Some(&mut self.window_picker.query),
            Window::ThemePicker => Some(&mut self.theme_picker.query),
            Window::ModelPicker => Some(&mut self.model_picker.query),
            Window::ConversationTree
            | Window::Terminal(_)
            | Window::CommitDiff(_)
            | Window::Report(_) => None,
        }
    }

    /// Hands a mouse event to the input of `window`: true when the input took it. A
    /// selection it starts holds the mouse until the button is released.
    pub fn input_mouse(
        &mut self,
        window: crate::tui::pane_manager::Window,
        mouse: &MouseInput,
    ) -> bool {
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
}

impl AppState {
    pub fn new(
        files: Arc<arc_swap::ArcSwap<Vec<Arc<crate::loader::LoadedFile>>>>,
        root: camino::Utf8PathBuf,
        theme: crate::tui::theme::HelixTheme,
        keymap: crate::tui::keymap::Keymap,
    ) -> Self {
        use crate::agent::mcp::McpHost;
        use crate::config::CargoFeatureSelection;
        use crate::tui::pane_manager::PaneManager;
        use crate::tui::pane_manager::Window;
        use crate::tui::theme::HelixTheme;
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
            last_surface_size: VPSize::default(),
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
            reports: HashMap::new(),
            next_report_id: 0,
            mcp,
            last_picked_model: None,
            model_picker: FuzzyPickerState::default(),
            model_picker_purpose: None,
            running_turns: HashMap::new(),
            spinner_tick: 0,
            queued_turns: Vec::new(),
            after_agents: None,
            config_aliases: Vec::new(),
            project_aliases: Vec::new(),
            trusted: false,
            after_agents_grace: Duration::from_secs(crate::config::DEFAULT_GRACE_SECONDS),
            next_after_agents_run: 0,
            mouse_drag_active: false,
            input_drag: None,
            terminal_grabbed: false,
            text_selection: None,
            session_dirty: false,
            is_dirty: false,
            symbol_highlights: Vec::new(),
            next_palette_index: 0,
            search: HashMap::new(),
            preview_inputs: HashMap::new(),
            command_line: None,
            keymap,
            cargo_features: CargoFeatureSelection::default(),
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

#[derive(Clone, Debug)]
pub struct SymbolHighlightGroup {
    pub qualified_name: String,
    pub origin_file: crate::loader::FileKey,
    pub origin_line: u32,
    pub origin_char: u32,
    /// Word originally clicked, re-sent on rebuild so the PlainText-hover
    /// fallback synthesizes the same `parent::word` qualified name.
    pub origin_word: Option<String>,
    pub origin_byte_start: Option<usize>,
    pub origin_byte_end: Option<usize>,
    pub color: RgbValue,
    pub locations: Vec<crate::signal::SymbolRefLocation>,
    pub needs_rebuild: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchDirection {
    Forward,
    Backward,
}

/// The input open in a file preview's title bar: the `/` `?` search.
#[derive(Clone)]
pub enum PreviewInput {
    /// Prefilled with the file's active pattern; the direction is the key that opened it.
    Search(crate::tui::widgets::input_line::InputLine, SearchDirection),
}

impl PreviewInput {
    pub fn line(&self) -> &crate::tui::widgets::input_line::InputLine {
        match self {
            Self::Search(line, _) => line,
        }
    }

    pub fn line_mut(&mut self) -> &mut crate::tui::widgets::input_line::InputLine {
        match self {
            Self::Search(line, _) => line,
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
