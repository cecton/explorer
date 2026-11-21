#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;
use std::collections::{HashMap, HashSet};
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

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
    /// Some = query active: the entries the query kept, best match first.
    pub filtered: Option<Vec<usize>>,
    /// Where the history walk starts.
    pub source: LogSource,
    pub branch_picking: bool,
    pub branch_picker: crate::tui::widgets::fuzzy_picker::FuzzyPickerState<usize>,
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
    /// Revision of the running turn's live texts the fields above were copied from; `None`
    /// when they have to be taken again, as after the texts were replaced from disk.
    pub live_revision: Option<u64>,
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
    /// The document laid out from the texts above, kept with the window rather than the
    /// pane slot, so a window moved to another slot is not laid out again.
    pub(crate) doc_cache: crate::tui::panes::discussion::document::DocCache,
}

/// What a report window shows: a title and the markdown under it.
#[derive(Clone)]
pub struct ReportState {
    pub title: String,
    pub markdown: String,
    /// The markdown last laid out, kept with the window rather than the pane slot, so a
    /// window moved to another slot is not laid out again.
    pub(crate) cache: Option<crate::tui::panes::report_pane::ReportCache>,
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
        &self.work_log[self.shown_log_start()..]
    }

    /// The first work log entry on screen: all of them when `Ctrl+O` shows the dropped
    /// rounds, else those of the rounds kept.
    pub fn shown_log_start(&self) -> usize {
        if self.show_work_log == Some(true) {
            0
        } else {
            self.log_start.min(self.work_log.len())
        }
    }

    /// Takes what the running turn has produced since the last frame. The texts only
    /// grow over a turn and its work-log entries are never rewritten, so the copy
    /// appends what is new instead of taking the whole texts again; a text that shrank
    /// or was rewritten is taken whole, which the layout cache has to know.
    pub fn mirror(&mut self, live: &crate::agent::turn::live::LiveTurn) {
        if self.live_revision == Some(live.revision()) {
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
        self.live_revision = Some(live.revision());
    }

    /// Replaces what the window shows with the texts of a node or sub-agent run read
    /// from disk. A running turn's texts are taken again at the next frame, since the
    /// disk copy is not the live one: the reply may be up to a second old, the thinking
    /// was never written, and the work log has none of the rounds the turn dropped.
    pub fn show_content(&mut self, prompt: String, reply: String, work_log: Vec<String>) {
        self.prompt = prompt;
        self.reply = reply;
        self.work_log = work_log;
        self.thinking.clear();
        self.log_start = 0;
        self.live_revision = None;
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
        text.push_str(&live[text.len()..]);
        false
    } else {
        text.clear();
        text.push_str(live);
        true
    }
}

impl NodeHighlights {
    pub fn part(&self, part: crate::tui::selection::DocPart) -> &[(usize, usize)] {
        use crate::tui::selection::DocPart;
        match part {
            DocPart::Prompt => &self.prompt,
            DocPart::Reply => &self.reply,
            DocPart::WorkLog | DocPart::Thinking => &[],
        }
    }

    /// The ranges of `part`; the work log and thinking have none, highlights cover the prompt and reply.
    pub fn part_mut(
        &mut self,
        part: crate::tui::selection::DocPart,
    ) -> Option<&mut Vec<(usize, usize)>> {
        use crate::tui::selection::DocPart;
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
    /// The repository's files, in the order the walk found them and append-only, so a
    /// `FileKey` stays valid for the life of the state. Contended by every reader that
    /// shows a file and the watcher thread, which is the only one that writes it.
    pub files: Arc<arc_swap::ArcSwap<Vec<Arc<crate::loader::LoadedFile>>>>,
    pub root: camino::Utf8PathBuf,
    /// Shared git repository handle, opened once at startup and cheaply cloned
    /// for each background git operation instead of re-opening `root` every
    /// time. `None` only if opening fails (e.g. permissions) — git.rs call
    /// sites already handle this gracefully via `Result<_, String>`.
    pub repo: Option<gix::ThreadSafeRepository>,
    /// Pane stack, sizes, layout, and focus state.
    pub pane_manager: crate::tui::pane_manager::PaneManager,
    /// Each previewed file's search, title bar input and highlights.
    pub previews: HashMap<crate::loader::FileKey, PreviewState>,
    pub leader_active: bool,
    /// The modal painted over every pane, if one is open: while it is, no pane takes input.
    pub modal: Option<crate::tui::modal::Modal>,
    /// The window that had the keyboard before the modal opened, and gets it back when the
    /// modal closes.
    pub modal_origin: Option<crate::tui::pane_manager::Window>,
    /// Rows the open modal's list shows at once, as the last frame painted it: the page a
    /// PageUp/PageDown moves by.
    pub modal_page_size: usize,
    /// The box the open modal's list was painted in, as the last frame put it: a wheel over
    /// it moves the list, a wheel anywhere else moves nothing.
    pub modal_box: Option<crate::tui::modal::ModalBox>,
    pub window_picker:
        crate::tui::widgets::fuzzy_picker::FuzzyPickerState<crate::tui::pane_manager::Window>,
    pub theme_picker: crate::tui::widgets::fuzzy_picker::FuzzyPickerState<String>,
    pub theme: crate::tui::theme::HelixTheme,
    pub saved_theme: crate::tui::theme::HelixTheme,
    /// Terminal panes keyed by their unique ID.
    pub terminal_panes: HashMap<usize, Arc<parking_lot::Mutex<TerminalPane>>>,
    /// Terminal windows restored from the session that still need a PTY spawned.
    pub pending_terminals: HashMap<usize, crate::tui::session::TerminalRestoreInfo>,
    /// Git log windows keyed by their unique ID.
    pub git_logs: HashMap<usize, GitLogWindowState>,
    /// Cached flat list of all branches and tags for the branch picker.
    pub git_refs: Option<Arc<Vec<crate::git::GitRef>>>,
    /// Loaded commit diffs keyed by commit object id.
    pub commit_diffs: HashMap<[u8; 20], CommitDiffState>,
    /// Maps editor terminal IDs back to the FileKey they should restore on exit.
    pub terminal_to_preview: HashMap<usize, crate::loader::FileKey>,
    /// Agent conversation tree, shared with the background task that loads it.
    pub conversations: Arc<parking_lot::RwLock<crate::conversation::ConversationTree>>,
    pub tree_view: TreeViewState,
    /// Discussion windows keyed by their unique ID.
    pub discussions: HashMap<usize, DiscussionState>,
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
    /// Next id of a terminal, git log, discussion or report window; see `next_id`.
    pub next_window_id: usize,
    /// Model last picked during this run, offered to new conversations.
    pub last_picked_model: Option<crate::model_spec::ModelSpec>,
    /// Model Picker entries are indexes into `models.specs`.
    pub model_picker: crate::tui::widgets::fuzzy_picker::FuzzyPickerState<usize>,
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
    pub text_selection: Option<crate::tui::selection::TextSelection>,
    pub session_dirty: bool,
    /// Whether the repository has uncommitted changes (staged or unstaged),
    /// refreshed periodically by a background `git::is_dirty` check.
    pub is_dirty: bool,
    pub symbol_highlights: Vec<SymbolHighlightGroup>,
    pub next_palette_index: usize,
    /// The `<leader>:` command line, open over the status bar. Transient.
    pub command_line: Option<crate::tui::command_line::CommandLine>,
    /// Configurable top-level (leader + global) key bindings.
    pub keymap: crate::tui::keymap::Keymap,
    /// Which Cargo features rust-analyzer analyzes; set via `features`/`all-features`/
    /// `default-features` on the global command line, persisted per-project in the
    /// session file.
    pub cargo_features: crate::lsp::CargoFeatureSelection,
}

impl AppState {
    /// A fresh id for a terminal, git log, discussion or report window. Windows are
    /// keyed by their `Window` variant, so the kinds share one counter.
    pub fn next_id(&mut self) -> usize {
        let id = self.next_window_id;
        self.next_window_id += 1;
        id
    }

    pub fn mark_session_dirty(&mut self) {
        self.session_dirty = true;
    }

    /// The highlighted line ranges of `key`, empty when it has none.
    pub fn highlights(&self, key: crate::loader::FileKey) -> &[(usize, usize)] {
        self.previews
            .get(&key)
            .map_or(&[], |preview| preview.highlights.as_slice())
    }

    /// The state of `key`'s preview, created empty if it has none yet.
    pub fn preview_mut(&mut self, key: crate::loader::FileKey) -> &mut PreviewState {
        self.previews.entry(key).or_default()
    }

    /// Replaces the highlights of `key`; an empty `ranges` clears them.
    pub fn set_highlights(&mut self, key: crate::loader::FileKey, ranges: Vec<(usize, usize)>) {
        self.preview_mut(key).highlights = ranges;
        self.prune_preview(key);
    }

    /// Replaces the search of `key`; `None` clears it.
    pub fn set_search(&mut self, key: crate::loader::FileKey, search: Option<PreviewSearch>) {
        self.preview_mut(key).search = search;
        self.prune_preview(key);
    }

    /// Replaces the title bar input of `key`; `None` closes it.
    pub fn set_input(&mut self, key: crate::loader::FileKey, input: Option<PreviewInput>) {
        self.preview_mut(key).input = input;
        self.prune_preview(key);
    }

    /// What closing `key`'s window drops: its search, its input and its error. The
    /// highlights stay.
    pub fn close_preview(&mut self, key: crate::loader::FileKey) {
        if let Some(preview) = self.previews.get_mut(&key) {
            preview.search = None;
            preview.input = None;
            preview.error = None;
        }
        self.prune_preview(key);
    }

    fn prune_preview(&mut self, key: crate::loader::FileKey) {
        if self.previews.get(&key).is_some_and(PreviewState::is_empty) {
            self.previews.remove(&key);
        }
    }

    /// Rechecks the dirty status off the main thread; the answer comes back as `DirtyStatusLoaded`.
    pub fn spawn_dirty_check(&self, main_tx: crate::signal::MainTx) {
        use crate::signal::AppSignal;
        let Some(repo) = self.repo.clone() else {
            return;
        };
        tokio::task::spawn_blocking(move || {
            let result = crate::git::is_dirty(&repo);
            crate::signal::send(&main_tx, AppSignal::DirtyStatusLoaded(result));
        });
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
            Window::FilePreview(key) => self
                .previews
                .get_mut(key)
                .and_then(|preview| preview.input.as_mut())
                .map(PreviewInput::line_mut),
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
        use crate::tui::pane_manager::PaneManager;
        let saved_theme = theme.clone();
        let pane_manager = PaneManager::new();
        let repo = gix::ThreadSafeRepository::open(root.as_std_path()).ok();
        let mcp = Arc::new(McpHost::new(root.clone()));

        Self {
            files,
            root,
            repo,
            pane_manager,
            theme,
            saved_theme,
            mcp,
            after_agents_grace: Duration::from_secs(crate::config::DEFAULT_GRACE_SECONDS),
            keymap,
            ..Self::default()
        }
    }
}

#[cfg(test)]
impl TerminalPane {
    /// An empty pane of `size` that no PTY backs, and the receiver of what is typed into it.
    pub(crate) fn for_test(size: VPSize) -> (Self, tokio::sync::mpsc::Receiver<PtyInputEvent>) {
        let (pty_input_tx, rx) = tokio::sync::mpsc::channel(8);
        let pane = Self {
            ofs_buf: OfsBufVT100::new_empty(size),
            title: None,
            pty_input_tx: Arc::new(pty_input_tx),
            child_killer: None,
            last_size: size,
            is_command_pane: false,
            exited: false,
            exit_code: None,
            exit_signal: None,
            scroll_offset: 0,
            cwd: camino::Utf8PathBuf::from("/"),
            command: None,
            flush_pending: Default::default(),
        };
        (pane, rx)
    }
}

#[cfg(test)]
impl AppState {
    /// A state over `root` with no files, the default theme and the default keymap.
    pub(crate) fn for_test(root: camino::Utf8PathBuf) -> Self {
        Self::for_test_with_files(Vec::new(), root)
    }

    /// `for_test` over a root that does not exist.
    pub(crate) fn blank_for_test() -> Self {
        Self::for_test(camino::Utf8PathBuf::from("/tmp/nowhere"))
    }

    /// Like `for_test`, over `files`.
    pub(crate) fn for_test_with_files(
        files: Vec<Arc<crate::loader::LoadedFile>>,
        root: camino::Utf8PathBuf,
    ) -> Self {
        Self::new(
            Arc::new(arc_swap::ArcSwap::from_pointee(files)),
            root,
            crate::tui::theme::HelixTheme::bundled_default(),
            crate::tui::keymap::Keymap::default(),
        )
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

/// The state of one previewed file, kept in `AppState::previews`.
#[derive(Clone, Default)]
pub struct PreviewState {
    /// Regex search (vim-style `/` `?` `n` `N`), never saved; dropped when the window closes.
    pub search: Option<PreviewSearch>,
    /// The input open in the title bar; dropped when the window closes.
    pub input: Option<PreviewInput>,
    /// Highlighted line ranges (1-indexed, inclusive), saved by path; kept on close.
    pub highlights: Vec<(usize, usize)>,
    /// The last search error and when it was set, shown in the title bar for a few
    /// seconds; dropped when the window closes.
    pub error: Option<(String, std::time::Instant)>,
}

impl PreviewState {
    fn is_empty(&self) -> bool {
        self.search.is_none()
            && self.input.is_none()
            && self.highlights.is_empty()
            && self.error.is_none()
    }
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
