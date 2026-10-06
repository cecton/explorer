use crate::conversation::NodeId;
use crate::loader::{LoadedFile, file_key_for_path, path_for_file_key};
use crate::tui::AppState;
use crate::tui::DiscussionState;
use crate::tui::GitLogWindowState;
use crate::tui::LogSource;
use crate::tui::NodeHighlights;
use crate::tui::pane_manager::{PaneSize, Window, WindowState};
use crate::tui::{RgbValue, SYMBOL_PALETTE, SymbolHighlightGroup};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub const SESSION_VERSION: u32 = 4;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub repo_root: Utf8PathBuf,
    #[serde(default)]
    pub panes: Vec<PaneEntry>,
    #[serde(default)]
    pub highlight_ranges: HashMap<String, Vec<(usize, usize)>>,
    #[serde(default)]
    pub symbol_highlights: Vec<SessionSymbolGroup>,
    /// Cumulative active time (excluding inactivity) spent on this project, in seconds.
    #[serde(default)]
    pub time_active_secs: u64,
    /// Cumulative total wall-clock time spent on this project, in seconds.
    #[serde(default)]
    pub time_total_secs: u64,
    /// Which Cargo features rust-analyzer analyzes for this project.
    #[serde(default)]
    pub cargo_features: crate::lsp::CargoFeatureSelection,
    /// Selected UI theme name for this project.
    #[serde(default)]
    pub theme: Option<String>,
    /// Roots and forks expanded in the Conversation Tree window.
    #[serde(default)]
    pub conversation_expanded: Vec<String>,
    /// Highlights in the prompts and replies of conversation nodes, by node id.
    #[serde(default)]
    pub discussion_highlights: HashMap<String, NodeHighlights>,
}

/// Persisted symbol-highlight identity. Byte-offset locations are never saved —
/// they go stale the moment a file changes outside the app — so restore re-queries
/// rust-analyzer via `origin_path`/`origin_line`/`origin_char`/`origin_word`, the
/// same (file, line, col) stable lookup key used for the live rebuild-on-edit path.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSymbolGroup {
    pub origin_path: String,
    pub origin_line: u32,
    pub origin_char: u32,
    pub origin_word: Option<String>,
    pub qualified_name: String,
    pub color: [u8; 3],
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum PaneEntry {
    #[serde(rename = "file_preview")]
    FilePreview {
        path: String,
        scroll: usize,
        pane_size: SerializablePaneSize,
    },

    #[serde(rename = "terminal")]
    Terminal {
        command: Option<String>,
        cwd: Utf8PathBuf,
        is_command_pane: bool,
        pane_size: SerializablePaneSize,
    },

    #[serde(rename = "git_log")]
    GitLog {
        id: usize,
        source: LogSource,
        query: String,
        scroll: usize,
        pane_size: SerializablePaneSize,
    },

    #[serde(rename = "commit_diff")]
    CommitDiff {
        hash: String,
        pane_size: SerializablePaneSize,
    },

    #[serde(rename = "conversation_tree")]
    ConversationTree { pane_size: SerializablePaneSize },

    #[serde(rename = "discussion")]
    Discussion {
        /// Node shown; `None` for a new conversation not sent yet.
        node: Option<String>,
        #[serde(default)]
        draft: String,
        #[serde(default)]
        mode: crate::agent::Mode,
        pane_size: SerializablePaneSize,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerializablePaneSize {
    Full,
    Half,
    Third,
    Quarter,
}

#[derive(Clone, Debug)]
pub struct TerminalRestoreInfo {
    pub command: Option<String>,
    pub cwd: Utf8PathBuf,
    pub is_command_pane: bool,
}

impl From<PaneSize> for SerializablePaneSize {
    fn from(value: PaneSize) -> Self {
        match value {
            PaneSize::Full => Self::Full,
            PaneSize::Half => Self::Half,
            PaneSize::Third => Self::Third,
            PaneSize::Quarter => Self::Quarter,
        }
    }
}

impl From<SerializablePaneSize> for PaneSize {
    fn from(value: SerializablePaneSize) -> Self {
        match value {
            SerializablePaneSize::Full => Self::Full,
            SerializablePaneSize::Half => Self::Half,
            SerializablePaneSize::Third => Self::Third,
            SerializablePaneSize::Quarter => Self::Quarter,
        }
    }
}

impl Session {
    pub fn from_state(state: &AppState) -> Self {
        let snapshot = state.files.load();
        let panes: Vec<PaneEntry> = state
            .pane_manager
            .window_stack
            .iter()
            .filter_map(|window| pane_entry_from_window(window, &snapshot, state))
            .collect();

        let mut highlight_ranges = HashMap::new();
        for (key, ranges) in &state.highlight_ranges {
            if let Some(path) = path_for_file_key(&snapshot, *key)
                && let Ok(rel) = path.strip_prefix(&state.root)
            {
                highlight_ranges.insert(rel.to_string(), ranges.clone());
            }
        }

        let symbol_highlights: Vec<SessionSymbolGroup> = state
            .symbol_highlights
            .iter()
            .filter_map(|group| {
                let path = path_for_file_key(&snapshot, group.origin_file)?;
                let rel = path.strip_prefix(&state.root).ok()?;
                Some(SessionSymbolGroup {
                    origin_path: rel.to_string(),
                    origin_line: group.origin_line,
                    origin_char: group.origin_char,
                    origin_word: group.origin_word.clone(),
                    qualified_name: group.qualified_name.clone(),
                    color: [group.color.red, group.color.green, group.color.blue],
                })
            })
            .collect();

        let mut conversation_expanded: Vec<String> = state
            .tree_view
            .expanded
            .iter()
            .map(|id| id.as_str().to_string())
            .collect();
        conversation_expanded.sort();

        let discussion_highlights = state
            .discussion_highlights
            .iter()
            .map(|(node, highlights)| (node.as_str().to_string(), highlights.clone()))
            .collect();

        Self {
            version: SESSION_VERSION,
            repo_root: state.root.clone(),
            panes,
            highlight_ranges,
            symbol_highlights,
            time_active_secs: state.time.project_active().as_secs(),
            time_total_secs: state.time.project_total().as_secs(),
            cargo_features: state.cargo_features.clone(),
            theme: Some(state.theme.name().to_string()),
            conversation_expanded,
            discussion_highlights,
        }
    }

    pub fn apply(self, state: &mut AppState) {
        let canon = |p: &Utf8Path| -> Utf8PathBuf {
            std::fs::canonicalize(p)
                .ok()
                .and_then(|c| Utf8PathBuf::from_path_buf(c).ok())
                .unwrap_or_else(|| p.to_owned())
        };
        if canon(&self.repo_root) != canon(&state.root) {
            return;
        }

        state.cargo_features = self.cargo_features.clone();

        if let Some(name) = &self.theme
            && let Some(theme) = crate::tui::HelixTheme::from_name(name)
        {
            state.theme = theme.clone();
            state.saved_theme = theme;
        }

        let mut missing_paths: Vec<Utf8PathBuf> = Vec::new();
        let snapshot_before = state.files.load();
        for entry in &self.panes {
            if let PaneEntry::FilePreview { path, .. } = entry {
                let abs = state.root.join(path);
                if file_key_for_path(&snapshot_before, &abs).is_none() {
                    missing_paths.push(abs);
                }
            }
        }
        if !missing_paths.is_empty() {
            missing_paths.sort();
            missing_paths.dedup();
            let mut files: Vec<Arc<LoadedFile>> = snapshot_before.iter().map(Arc::clone).collect();
            for abs in &missing_paths {
                files.push(LoadedFile::stub(abs.clone()));
            }
            state.files.store(Arc::new(files));
        }

        let snapshot_after = state.files.load();
        let mut new_stack = Vec::new();
        let mut new_states = HashMap::new();

        for entry in self.panes {
            match entry {
                PaneEntry::FilePreview {
                    path,
                    scroll,
                    pane_size,
                } => {
                    let abs = state.root.join(&path);
                    if let Some(key) = file_key_for_path(&snapshot_after, &abs) {
                        snapshot_after[key.0]
                            .needs_full_load
                            .store(true, Ordering::Relaxed);
                        let window = Window::FilePreview(key);
                        new_stack.push(window);
                        new_states.insert(
                            window,
                            WindowState {
                                scroll,
                                pane_size: pane_size.into(),
                                ..Default::default()
                            },
                        );
                    }
                }

                PaneEntry::Terminal {
                    command,
                    cwd,
                    is_command_pane,
                    pane_size,
                } => {
                    let id = state.next_terminal_id;
                    state.next_terminal_id += 1;
                    let window = Window::Terminal(id);
                    new_stack.push(window);
                    new_states.insert(
                        window,
                        WindowState {
                            pane_size: pane_size.into(),
                            ..Default::default()
                        },
                    );
                    state.pending_terminals.insert(
                        id,
                        TerminalRestoreInfo {
                            command,
                            cwd,
                            is_command_pane,
                        },
                    );
                }

                PaneEntry::GitLog {
                    id,
                    source,
                    query,
                    scroll,
                    pane_size,
                } => {
                    let window = Window::GitLog(id);
                    new_stack.push(window);
                    new_states.insert(
                        window,
                        WindowState {
                            scroll,
                            pane_size: pane_size.into(),
                            ..Default::default()
                        },
                    );
                    state.next_git_log_id = state.next_git_log_id.max(id.saturating_add(1));
                    let mut win = GitLogWindowState {
                        source: source.clone(),
                        query: query.clone(),
                        ..Default::default()
                    };
                    if !query.is_empty() {
                        crate::tui::git_log_pane::apply_filter(&mut win);
                    }
                    state.git_logs.insert(id, win);
                    state.pending_git_logs.push(id);
                }

                PaneEntry::CommitDiff { hash, pane_size } => {
                    let Ok(bytes) = hex::decode(&hash) else {
                        continue;
                    };
                    let Ok(id) = <[u8; 20]>::try_from(bytes.as_slice()) else {
                        continue;
                    };
                    let window = Window::CommitDiff(id);
                    new_stack.push(window);
                    new_states.insert(
                        window,
                        WindowState {
                            pane_size: pane_size.into(),
                            ..Default::default()
                        },
                    );
                    state.pending_diffs.push(id);
                }

                PaneEntry::ConversationTree { pane_size } => {
                    let window = Window::ConversationTree;
                    new_stack.push(window);
                    new_states.insert(
                        window,
                        WindowState {
                            pane_size: pane_size.into(),
                            ..Default::default()
                        },
                    );
                }

                PaneEntry::Discussion {
                    node,
                    draft,
                    mode,
                    pane_size,
                } => {
                    let id = state.next_discussion_id;
                    state.next_discussion_id += 1;
                    let window = Window::Discussion(id);
                    new_stack.push(window);
                    new_states.insert(
                        window,
                        WindowState {
                            pane_size: pane_size.into(),
                            ..Default::default()
                        },
                    );
                    state.discussions.insert(
                        id,
                        DiscussionState {
                            loaded: node.is_none(),
                            node: node.as_deref().map(NodeId::new),
                            draft,
                            mode,
                            ..Default::default()
                        },
                    );
                }
            }
        }

        state.tree_view.expanded = self
            .conversation_expanded
            .iter()
            .map(|id| NodeId::new(id))
            .collect();
        state.discussion_highlights = self
            .discussion_highlights
            .into_iter()
            .map(|(node, highlights)| (NodeId::new(&node), highlights))
            .collect();

        if !new_stack.is_empty() {
            state.pane_manager.window_stack = new_stack;
            state.pane_manager.window_states = new_states;
            state.pane_manager.focused_window = state.pane_manager.window_stack.first().copied();
        }

        let snapshot = state.files.load();
        state.highlight_ranges.clear();
        for (rel, ranges) in self.highlight_ranges {
            let abs = state.root.join(&rel);
            if let Some(key) = file_key_for_path(&snapshot, &abs) {
                state.highlight_ranges.insert(key, ranges);
            }
        }

        // Locations are never restored (they'd be stale); only origin identity is
        // kept. `needs_rebuild` triggers re-resolution against a running LSP worker,
        // same as the modified-origin-file rebuild path.
        state.symbol_highlights.clear();
        for entry in self.symbol_highlights {
            let abs = state.root.join(&entry.origin_path);
            if let Some(origin_file) = file_key_for_path(&snapshot, &abs) {
                state.symbol_highlights.push(SymbolHighlightGroup {
                    qualified_name: entry.qualified_name,
                    origin_file,
                    origin_line: entry.origin_line,
                    origin_char: entry.origin_char,
                    origin_word: entry.origin_word,
                    origin_byte_start: None,
                    origin_byte_end: None,
                    color: RgbValue::from_u8(entry.color[0], entry.color[1], entry.color[2]),
                    locations: Vec::new(),
                    needs_rebuild: true,
                });
            }
        }
        state.next_palette_index = state.symbol_highlights.len() % SYMBOL_PALETTE.len();
    }
}

fn pane_entry_from_window(
    window: &Window,
    files: &[Arc<LoadedFile>],
    state: &AppState,
) -> Option<PaneEntry> {
    let root = &state.root;
    let window_state = state
        .pane_manager
        .window_states
        .get(window)
        .cloned()
        .unwrap_or_default();
    match window {
        Window::FilePreview(key) => {
            let path = path_for_file_key(files, *key)?;
            let rel = path.strip_prefix(root).ok()?;
            Some(PaneEntry::FilePreview {
                path: rel.to_string(),
                scroll: window_state.scroll,
                pane_size: window_state.pane_size.into(),
            })
        }
        Window::WindowPicker | Window::ThemePicker | Window::ModelPicker => None,
        Window::GitLog(id) => {
            let win = state.git_logs.get(id)?;
            Some(PaneEntry::GitLog {
                id: *id,
                source: win.source.clone(),
                query: win.query.clone(),
                scroll: window_state.scroll,
                pane_size: window_state.pane_size.into(),
            })
        }
        Window::CommitDiff(id) => Some(PaneEntry::CommitDiff {
            hash: hex::encode(id),
            pane_size: window_state.pane_size.into(),
        }),
        Window::ConversationTree => Some(PaneEntry::ConversationTree {
            pane_size: window_state.pane_size.into(),
        }),
        Window::Discussion(id) => {
            let discussion = state.discussions.get(id)?;
            Some(PaneEntry::Discussion {
                node: discussion
                    .node
                    .as_ref()
                    .map(|node| node.as_str().to_string()),
                draft: discussion.draft.clone(),
                mode: discussion.mode,
                pane_size: window_state.pane_size.into(),
            })
        }
        Window::Terminal(id) => {
            // Editor terminals save as FilePreview so restore brings back the preview.
            if let Some(file_key) = state.terminal_to_preview.get(id)
                && let Some(path) = path_for_file_key(files, *file_key)
                && let Ok(rel) = path.strip_prefix(root)
            {
                return Some(PaneEntry::FilePreview {
                    path: rel.to_string(),
                    scroll: 0,
                    pane_size: window_state.pane_size.into(),
                });
            }
            let pane = state.terminal_panes.get(id)?.lock();
            if pane.exited {
                return None;
            }
            Some(PaneEntry::Terminal {
                command: pane.command.clone(),
                cwd: pane.cwd.clone(),
                is_command_pane: pane.is_command_pane,
                pane_size: window_state.pane_size.into(),
            })
        }
    }
}

#[derive(Debug)]
pub enum SessionError {
    Io(std::io::Error),
    Serialize(serde_json::Error),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Io(e) => write!(f, "io error: {e}"),
            SessionError::Serialize(e) => write!(f, "serialize error: {e}"),
        }
    }
}

impl std::error::Error for SessionError {}

pub fn load_session(repo_root: &Utf8Path) -> Option<Session> {
    let path = session_file_path(repo_root)?;
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!("failed to read session: {e}");
            return None;
        }
    };
    let session: Session = match serde_json::from_str(&content) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("failed to parse session: {e}");
            return None;
        }
    };
    if session.version > SESSION_VERSION {
        tracing::debug!(
            "session version too new ({} > {}), discarding",
            session.version,
            SESSION_VERSION
        );
        return None;
    }
    Some(session)
}

pub fn save_session(repo_root: &Utf8Path, state: &AppState) -> Result<(), SessionError> {
    let path = session_file_path(repo_root).ok_or_else(|| {
        SessionError::Io(std::io::Error::other("could not determine session path"))
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(SessionError::Io)?;
    }
    let session = Session::from_state(state);
    let content = serde_json::to_string_pretty(&session).map_err(SessionError::Serialize)?;
    let tmp_path: Utf8PathBuf = format!("{}.tmp", path.as_str()).into();
    std::fs::write(&tmp_path, content).map_err(SessionError::Io)?;
    std::fs::rename(&tmp_path, &path).map_err(SessionError::Io)
}

pub fn session_dir() -> Option<Utf8PathBuf> {
    directories::ProjectDirs::from("", "", "explorer")
        .and_then(|dirs| Utf8PathBuf::from_path_buf(dirs.data_dir().join("sessions")).ok())
}

/// Stable 16-hex identifier of a repository, derived from its canonical root path.
pub fn repo_hash(repo_root: &Utf8Path) -> Option<String> {
    let canonical = std::fs::canonicalize(repo_root).ok()?;
    let canonical = Utf8PathBuf::from_path_buf(canonical).ok()?;
    let hash = Sha256::digest(canonical.as_str().as_bytes());
    Some(hex::encode(&hash[..8]))
}

pub fn session_file_path(repo_root: &Utf8Path) -> Option<Utf8PathBuf> {
    let dir = session_dir()?;
    let prefix = repo_hash(repo_root)?;
    Some(dir.join(format!("{prefix}.json")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::Keymap;
    use crate::loader::FileKey;
    use crate::tui::HelixTheme;
    use crate::tui::TerminalPane;
    use crate::tui::pane_manager::{PaneSize, Window, WindowState};
    use arc_swap::ArcSwap;
    use camino::Utf8PathBuf;
    use parking_lot::Mutex;
    use r3bl_tui::{OfsBufVT100, Size};
    use std::sync::Arc;
    use std::time::Instant;
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    fn temp_root() -> (TempDir, Utf8PathBuf) {
        let dir = TempDir::new().expect("temp dir");
        let root = Utf8PathBuf::from_path_buf(dir.path().canonicalize().unwrap()).unwrap();
        (dir, root)
    }

    fn empty_state(root: Utf8PathBuf) -> AppState {
        let files = Arc::new(ArcSwap::from_pointee(Vec::<Arc<LoadedFile>>::new()));
        AppState::new(files, root, HelixTheme::default(), Keymap::default())
    }

    fn dummy_terminal_pane(cwd: Utf8PathBuf, command: Option<String>) -> TerminalPane {
        let (pty_input_tx, _) = mpsc::channel(1);
        TerminalPane {
            ofs_buf: OfsBufVT100::new_empty(Size::default()),
            title: None,
            pty_input_tx: Arc::new(pty_input_tx),
            child_killer: None,
            last_size: Size::default(),
            is_command_pane: command.is_some(),
            exited: false,
            exit_code: None,
            exit_signal: None,
            scroll_offset: 0,
            cwd,
            command,
            last_output: Instant::now(),
        }
    }

    #[test]
    fn session_serialization_round_trip() {
        let session = Session {
            version: SESSION_VERSION,
            repo_root: Utf8PathBuf::from("/tmp/repo"),
            panes: vec![
                PaneEntry::FilePreview {
                    path: "src/main.rs".to_string(),
                    scroll: 5,
                    pane_size: SerializablePaneSize::Half,
                },
                PaneEntry::Terminal {
                    command: Some("cargo test".to_string()),
                    cwd: Utf8PathBuf::from("/tmp/repo"),
                    is_command_pane: true,
                    pane_size: SerializablePaneSize::Quarter,
                },
            ],
            highlight_ranges: {
                let mut m = HashMap::new();
                m.insert("src/lib.rs".to_string(), vec![(1, 10), (20, 30)]);
                m
            },
            symbol_highlights: vec![SessionSymbolGroup {
                origin_path: "src/lib.rs".to_string(),
                origin_line: 3,
                origin_char: 7,
                origin_word: Some("add_numbers".to_string()),
                qualified_name: "crate::lib::add_numbers".to_string(),
                color: [255, 100, 100],
            }],
            time_active_secs: 1230,
            time_total_secs: 4560,
            cargo_features: crate::lsp::CargoFeatureSelection::Explicit {
                no_default_features: true,
                features: vec!["foo".to_string()],
            },
            theme: None,
            conversation_expanded: Vec::new(),
            discussion_highlights: HashMap::new(),
        };

        let value = serde_json::to_value(&session).unwrap();
        let restored: Session = serde_json::from_value(value.clone()).unwrap();
        let value2 = serde_json::to_value(&restored).unwrap();
        assert_eq!(value, value2);
        assert_eq!(restored.time_active_secs, 1230);
        assert_eq!(restored.time_total_secs, 4560);
    }

    #[test]
    fn session_deserializes_without_symbol_highlights_field() {
        // Older session files (or any writer that omits the field) must still load,
        // defaulting to no persisted symbol highlights.
        let json = serde_json::json!({
            "version": SESSION_VERSION,
            "repo_root": "/tmp/repo",
            "panes": [],
            "highlight_ranges": {},
        });
        let session: Session = serde_json::from_value(json).unwrap();
        assert!(session.symbol_highlights.is_empty());
    }

    #[test]
    fn session_from_state_preserves_window_stack_order() {
        let (_dir, root) = temp_root();

        let file_path = root.join("src/main.rs");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, "fn main() {}\n").unwrap();

        let loaded = LoadedFile::load(file_path.into()).unwrap();
        let files = Arc::new(ArcSwap::from_pointee(vec![loaded]));
        let theme = HelixTheme::default();
        let mut state = AppState::new(files, root.clone(), theme, Keymap::default());

        state.pane_manager.window_stack =
            vec![Window::FilePreview(FileKey(0)), Window::Terminal(1)];
        state.pane_manager.window_states.insert(
            Window::FilePreview(FileKey(0)),
            WindowState {
                scroll: 3,
                pane_size: PaneSize::Half,
                ..Default::default()
            },
        );
        state.pane_manager.window_states.insert(
            Window::Terminal(1),
            WindowState {
                pane_size: PaneSize::Quarter,
                ..Default::default()
            },
        );
        state.terminal_panes.insert(
            1,
            Arc::new(Mutex::new(dummy_terminal_pane(
                root.clone(),
                Some("cargo test".to_string()),
            ))),
        );

        let session = Session::from_state(&state);
        let kinds: Vec<_> = session
            .panes
            .iter()
            .map(|entry| match entry {
                PaneEntry::FilePreview { .. } => "file_preview",
                PaneEntry::Terminal { .. } => "terminal",
                PaneEntry::GitLog { .. } => "git_log",
                PaneEntry::CommitDiff { .. } => "commit_diff",
                PaneEntry::ConversationTree { .. } => "conversation_tree",
                PaneEntry::Discussion { .. } => "discussion",
            })
            .collect();
        assert_eq!(kinds, vec!["file_preview", "terminal"]);

        assert!(matches!(
            session.panes[0],
            PaneEntry::FilePreview {
                path: _,
                scroll: 3,
                pane_size: SerializablePaneSize::Half,
            }
        ));
        assert!(matches!(
            session.panes[1],
            PaneEntry::Terminal {
                command: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn session_apply_creates_stubs_and_restores_highlights() {
        let (_dir, root) = temp_root();
        let mut state = empty_state(root.clone());

        let session = Session {
            version: SESSION_VERSION,
            repo_root: root.clone(),
            panes: vec![PaneEntry::FilePreview {
                path: "missing.rs".to_string(),
                scroll: 0,
                pane_size: SerializablePaneSize::Full,
            }],
            highlight_ranges: {
                let mut m = HashMap::new();
                m.insert("missing.rs".to_string(), vec![(1, 5)]);
                m
            },
            symbol_highlights: Vec::new(),
            time_active_secs: 0,
            time_total_secs: 0,
            cargo_features: crate::lsp::CargoFeatureSelection::default(),
            theme: None,
            conversation_expanded: Vec::new(),
            discussion_highlights: HashMap::new(),
        };

        session.apply(&mut state);

        assert_eq!(state.pane_manager.window_stack.len(), 1);
        assert!(matches!(
            state.pane_manager.window_stack[0],
            Window::FilePreview(FileKey(0))
        ));

        let snapshot = state.files.load();
        assert_eq!(snapshot.len(), 1);
        assert!(
            snapshot[0]
                .removed
                .load(std::sync::atomic::Ordering::Relaxed)
        );
        assert_eq!(snapshot[0].path, root.join("missing.rs"));

        assert_eq!(state.highlight_ranges.len(), 1);
        assert_eq!(state.highlight_ranges.get(&FileKey(0)), Some(&vec![(1, 5)]));
    }

    #[test]
    fn session_apply_batches_stubs_so_multiple_missing_files_get_distinct_keys() {
        let (_dir, root) = temp_root();

        // Create one real file so we have a non-empty files list.
        let existing_path = root.join("b.rs");
        std::fs::create_dir_all(existing_path.parent().unwrap()).unwrap();
        std::fs::write(&existing_path, "// b.rs content\n").unwrap();
        let loaded = LoadedFile::load(existing_path.into()).unwrap();
        let files = Arc::new(ArcSwap::from_pointee(vec![loaded]));
        let theme = HelixTheme::default();
        let mut state = AppState::new(files, root.clone(), theme, Keymap::default());

        // Session has two missing-file previews whose paths sort differently
        // from the order they appear in the session.
        let session = Session {
            version: SESSION_VERSION,
            repo_root: root.clone(),
            panes: vec![
                PaneEntry::FilePreview {
                    path: "missing_x.rs".to_string(),
                    scroll: 0,
                    pane_size: SerializablePaneSize::Half,
                },
                PaneEntry::FilePreview {
                    path: "missing_a.rs".to_string(),
                    scroll: 10,
                    pane_size: SerializablePaneSize::Half,
                },
            ],
            highlight_ranges: HashMap::new(),
            symbol_highlights: Vec::new(),
            time_active_secs: 0,
            time_total_secs: 0,
            cargo_features: crate::lsp::CargoFeatureSelection::default(),
            theme: None,
            conversation_expanded: Vec::new(),
            discussion_highlights: HashMap::new(),
        };

        session.apply(&mut state);

        assert_eq!(state.pane_manager.window_stack.len(), 2);
        // After sorting: b.rs(0), missing_a.rs(1), missing_x.rs(2).
        // Windows appear in session order (missing_x first, missing_a second).
        assert_eq!(
            state.pane_manager.window_stack[0],
            Window::FilePreview(FileKey(2)),
            "missing_x.rs should be at FileKey(2) after sorting"
        );
        assert_eq!(
            state.pane_manager.window_stack[1],
            Window::FilePreview(FileKey(1)),
            "missing_a.rs should be at FileKey(1) after sorting"
        );

        // The two FileKeys must be distinct.
        assert_ne!(
            state.pane_manager.window_stack[0],
            state.pane_manager.window_stack[1],
        );
    }

    #[test]
    fn session_apply_stores_terminal_restore_infos() {
        let (_dir, root) = temp_root();
        let mut state = empty_state(root.clone());

        let session = Session {
            version: SESSION_VERSION,
            repo_root: root.clone(),
            panes: vec![PaneEntry::Terminal {
                command: Some("cargo test".to_string()),
                cwd: root.clone(),
                is_command_pane: true,
                pane_size: SerializablePaneSize::Quarter,
            }],
            highlight_ranges: HashMap::new(),
            symbol_highlights: Vec::new(),
            time_active_secs: 0,
            time_total_secs: 0,
            cargo_features: crate::lsp::CargoFeatureSelection::default(),
            theme: None,
            conversation_expanded: Vec::new(),
            discussion_highlights: HashMap::new(),
        };

        session.apply(&mut state);

        assert_eq!(state.pending_terminals.len(), 1);
        let pending = state.pending_terminals.get(&0).unwrap();
        assert_eq!(pending.command, Some("cargo test".to_string()));
        assert_eq!(pending.cwd, root);
        assert!(pending.is_command_pane);
    }

    #[test]
    fn session_apply_skips_when_repo_root_mismatches() {
        let (_dir_a, root_a) = temp_root();
        let (_dir_b, root_b) = temp_root();
        let mut state = empty_state(root_b.clone());

        let session = Session {
            version: SESSION_VERSION,
            repo_root: root_a,
            panes: vec![PaneEntry::FilePreview {
                path: "src/main.rs".to_string(),
                scroll: 10,
                pane_size: SerializablePaneSize::Half,
            }],
            highlight_ranges: HashMap::new(),
            symbol_highlights: Vec::new(),
            time_active_secs: 0,
            time_total_secs: 0,
            cargo_features: crate::lsp::CargoFeatureSelection::default(),
            theme: None,
            conversation_expanded: Vec::new(),
            discussion_highlights: HashMap::new(),
        };

        session.apply(&mut state);

        assert_eq!(state.pane_manager.window_stack.len(), 1);
        assert!(matches!(
            state.pane_manager.window_stack[0],
            Window::WindowPicker
        ));
        assert!(state.highlight_ranges.is_empty());
        assert!(state.pending_terminals.is_empty());
        assert!(state.symbol_highlights.is_empty());
    }

    #[test]
    fn session_apply_restores_symbol_groups() {
        let (_dir, root) = temp_root();

        let file_path = root.join("src/util.rs");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(
            &file_path,
            "fn add_numbers(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
        )
        .unwrap();
        let loaded = LoadedFile::load(file_path.into()).unwrap();
        let files = Arc::new(ArcSwap::from_pointee(vec![loaded]));
        let mut state = AppState::new(
            files,
            root.clone(),
            HelixTheme::default(),
            Keymap::default(),
        );

        let session = Session {
            version: SESSION_VERSION,
            repo_root: root.clone(),
            panes: Vec::new(),
            highlight_ranges: HashMap::new(),
            symbol_highlights: vec![
                SessionSymbolGroup {
                    origin_path: "src/util.rs".to_string(),
                    origin_line: 0,
                    origin_char: 3,
                    origin_word: Some("add_numbers".to_string()),
                    qualified_name: "symtest::util::add_numbers".to_string(),
                    color: [255, 100, 100],
                },
                // Origin file doesn't exist in this repo: must be skipped, not
                // stubbed like a file-preview pane (the group has nothing to
                // rebuild without a real file to re-query).
                SessionSymbolGroup {
                    origin_path: "src/missing.rs".to_string(),
                    origin_line: 0,
                    origin_char: 0,
                    origin_word: None,
                    qualified_name: "symtest::missing::gone".to_string(),
                    color: [100, 200, 255],
                },
            ],
            time_active_secs: 0,
            time_total_secs: 0,
            cargo_features: crate::lsp::CargoFeatureSelection::default(),
            theme: None,
            conversation_expanded: Vec::new(),
            discussion_highlights: HashMap::new(),
        };

        session.apply(&mut state);

        assert_eq!(state.symbol_highlights.len(), 1);
        let group = &state.symbol_highlights[0];
        assert_eq!(group.qualified_name, "symtest::util::add_numbers");
        assert_eq!(group.origin_line, 0);
        assert_eq!(group.origin_char, 3);
        assert_eq!(group.origin_word.as_deref(), Some("add_numbers"));
        assert_eq!(group.color, RgbValue::from_u8(255, 100, 100));
        assert!(group.needs_rebuild);
        assert!(group.locations.is_empty());
        assert!(group.origin_byte_start.is_none());
        assert!(group.origin_byte_end.is_none());

        assert_eq!(state.next_palette_index, 1);
    }

    #[test]
    fn conversation_tree_pane_round_trips_with_expanded_set() {
        let (_dir, root) = temp_root();
        let mut state = empty_state(root.clone());
        state.pane_manager.window_stack = vec![Window::ConversationTree];
        state
            .tree_view
            .expanded
            .insert(NodeId::new("2026-10-05T14-32-07Z"));

        let session = Session::from_state(&state);
        assert_eq!(session.conversation_expanded, ["2026-10-05T14-32-07Z"]);
        let json = serde_json::to_string(&session).expect("serialize session");
        let restored: Session = serde_json::from_str(&json).expect("deserialize session");

        let mut fresh = empty_state(root);
        restored.apply(&mut fresh);
        assert_eq!(
            fresh.pane_manager.window_stack,
            vec![Window::ConversationTree]
        );
        assert!(
            fresh
                .tree_view
                .expanded
                .contains(&NodeId::new("2026-10-05T14-32-07Z"))
        );
    }

    #[test]
    fn discussion_highlights_round_trip_through_session() {
        let (_dir, root) = temp_root();
        let mut state = empty_state(root.clone());
        let node = NodeId::new("2026-10-05T14-32-07Z");
        let highlights = NodeHighlights {
            prompt: vec![(0, 4)],
            reply: vec![(3, 9), (20, 31)],
        };
        state
            .discussion_highlights
            .insert(node.clone(), highlights.clone());

        let session = Session::from_state(&state);
        let json = serde_json::to_string(&session).expect("serialize session");
        let restored: Session = serde_json::from_str(&json).expect("deserialize session");

        let mut fresh = empty_state(root);
        restored.apply(&mut fresh);
        assert_eq!(fresh.discussion_highlights.len(), 1);
        assert_eq!(fresh.discussion_highlights.get(&node), Some(&highlights));
    }

    #[test]
    fn commit_diff_pane_round_trips_through_session() {
        let (_dir, root) = temp_root();
        let mut state = empty_state(root.clone());
        let id = [0x42u8; 20];
        state.pane_manager.window_stack = vec![Window::CommitDiff(id)];
        state.pane_manager.window_states.insert(
            Window::CommitDiff(id),
            WindowState {
                pane_size: PaneSize::Half,
                ..Default::default()
            },
        );

        let session = Session::from_state(&state);
        assert_eq!(session.version, SESSION_VERSION);
        let kinds: Vec<_> = session
            .panes
            .iter()
            .map(|entry| match entry {
                PaneEntry::FilePreview { .. } => "file_preview",
                PaneEntry::Terminal { .. } => "terminal",
                PaneEntry::GitLog { .. } => "git_log",
                PaneEntry::CommitDiff { .. } => "commit_diff",
                PaneEntry::ConversationTree { .. } => "conversation_tree",
                PaneEntry::Discussion { .. } => "discussion",
            })
            .collect();
        assert_eq!(kinds, vec!["commit_diff"]);
        let PaneEntry::CommitDiff { hash, .. } = &session.panes[0] else {
            panic!("expected commit_diff entry");
        };
        assert_eq!(hash, &hex::encode([0x42u8; 20]));

        let mut restored = empty_state(root.clone());
        session.apply(&mut restored);
        assert_eq!(
            restored.pane_manager.window_stack,
            vec![Window::CommitDiff(id)]
        );
        assert_eq!(restored.pending_diffs, vec![id]);
    }

    #[test]
    fn git_log_pane_round_trips_with_branch_and_query() {
        let (_dir, root) = temp_root();
        let mut state = empty_state(root.clone());
        state.pane_manager.window_stack = vec![Window::GitLog(7)];
        state.pane_manager.window_states.insert(
            Window::GitLog(7),
            WindowState {
                scroll: 4,
                pane_size: PaneSize::Third,
                ..Default::default()
            },
        );
        let win = GitLogWindowState {
            source: LogSource::Branch("origin/main".to_string()),
            query: "fix:".to_string(),
            ..Default::default()
        };
        state.git_logs.insert(7, win);
        state.next_git_log_id = 8;

        let session = Session::from_state(&state);
        let PaneEntry::GitLog {
            id,
            source,
            query,
            scroll,
            ..
        } = &session.panes[0]
        else {
            panic!("expected git_log entry");
        };
        assert_eq!(id, &7);
        assert_eq!(source, &LogSource::Branch("origin/main".to_string()));
        assert_eq!(query, "fix:");
        assert_eq!(scroll, &4);

        let mut restored = empty_state(root.clone());
        session.apply(&mut restored);
        assert_eq!(restored.pane_manager.window_stack, vec![Window::GitLog(7)]);
        assert_eq!(restored.next_git_log_id, 8);
        assert_eq!(restored.pending_git_logs, vec![7]);
        let restored_win = restored.git_logs.get(&7).expect("window state");
        assert_eq!(
            restored_win.source,
            LogSource::Branch("origin/main".to_string())
        );
        assert_eq!(restored_win.query, "fix:");
    }

    #[test]
    fn session_file_path_is_deterministic_and_under_sessions_dir() {
        let (_dir, root) = temp_root();

        let path1 = session_file_path(&root).unwrap();
        let path2 = session_file_path(&root).unwrap();
        assert_eq!(path1, path2);
        assert!(path1.starts_with(session_dir().unwrap()));

        let name = path1.file_name().unwrap();
        assert!(name.ends_with(".json"));
        assert_eq!(name, path2.file_name().unwrap());
    }

    #[test]
    fn save_session_writes_a_file() {
        let (_dir, root) = temp_root();

        let file_path = root.join("src/main.rs");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, "fn main() {}\n").unwrap();

        let loaded = LoadedFile::load(file_path.into()).unwrap();
        let files = Arc::new(ArcSwap::from_pointee(vec![loaded]));
        let theme = HelixTheme::default();
        let mut state = AppState::new(files, root.clone(), theme, Keymap::default());
        state
            .pane_manager
            .push_window(Window::FilePreview(FileKey(0)));

        save_session(&root, &state).unwrap();

        let path = session_file_path(&root).unwrap();
        assert!(path.exists(), "session file should exist at {path}");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.is_empty());

        let parsed: Session = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.panes.len(), 1);
        assert!(matches!(
            &parsed.panes[0],
            PaneEntry::FilePreview { path, .. } if path == "src/main.rs",
        ));

        // Clean up.
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_session_accepts_older_versions() {
        let (_dir, root) = temp_root();
        let path = session_file_path(&root).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{{\"version\": {}}}", SESSION_VERSION - 1)).unwrap();

        let session = load_session(&root).expect("older version must load");
        assert_eq!(session.version, SESSION_VERSION - 1);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_session_discards_newer_versions() {
        let (_dir, root) = temp_root();
        let path = session_file_path(&root).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{{\"version\": {}}}", SESSION_VERSION + 1)).unwrap();

        assert!(load_session(&root).is_none());

        let _ = std::fs::remove_file(&path);
    }
}
