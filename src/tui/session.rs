use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Session {
    #[serde(default)]
    pub panes: Vec<PaneRecord>,
    #[serde(default)]
    pub highlight_ranges: HashMap<String, Vec<(usize, usize)>>,
    #[serde(default)]
    pub symbol_highlights: Vec<SessionSymbolGroup>,
    /// Which Cargo features rust-analyzer analyzes for this project.
    #[serde(default)]
    pub cargo_features: crate::lsp::CargoFeatureSelection,
    /// Selected UI theme name for this project.
    #[serde(default)]
    pub theme: Option<String>,
    /// Forks and threads expanded in the Conversation Tree window.
    #[serde(default)]
    pub conversation_expanded: Vec<String>,
    /// Filter of the Conversation Tree window.
    #[serde(default)]
    pub conversation_query: String,
    /// Highlights in the prompts and replies of conversation nodes, by node id.
    #[serde(default)]
    pub discussion_highlights: HashMap<String, crate::tui::state::NodeHighlights>,
    /// MCP servers of this repository, started with Explorer.
    #[serde(default)]
    pub mcp_servers: Vec<crate::agent::mcp::McpServerDecl>,
    /// Aliases of this repository.
    #[serde(default)]
    pub aliases: Vec<crate::config::Alias>,
    /// The user allowed agent turns in this repository. No turn runs without it.
    #[serde(default)]
    pub trusted: bool,
}

/// Persisted symbol-highlight identity. Byte-offset locations are never saved —
/// they go stale the moment a file changes outside the app — so restore re-queries
/// rust-analyzer via `origin_path`/`origin_line`/`origin_char`/`origin_word`, the
/// same (file, line, col) stable lookup key used for the live rebuild-on-edit path.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SessionSymbolGroup {
    pub origin_path: String,
    pub origin_line: u32,
    pub origin_char: u32,
    pub origin_word: Option<String>,
    pub qualified_name: String,
    pub color: [u8; 3],
}

/// One window of the saved stack: its size, and what it shows.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PaneRecord {
    pub pane_size: crate::tui::pane_manager::PaneSize,
    #[serde(flatten)]
    pub entry: PaneEntry,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum PaneEntry {
    #[serde(rename = "file_preview")]
    FilePreview { path: String, scroll: usize },

    #[serde(rename = "terminal")]
    Terminal {
        command: Option<String>,
        cwd: camino::Utf8PathBuf,
        is_command_pane: bool,
    },

    #[serde(rename = "git_log")]
    GitLog {
        source: crate::tui::state::LogSource,
        query: String,
        scroll: usize,
    },

    #[serde(rename = "commit_diff")]
    CommitDiff { hash: String },

    #[serde(rename = "conversation_tree")]
    ConversationTree,

    #[serde(rename = "discussion")]
    Discussion {
        /// Node shown; `None` for a new conversation not sent yet.
        node: Option<String>,
        /// Sub-agent run of `node` shown instead of the node itself.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<u32>,
        #[serde(default)]
        draft: String,
    },
}

#[derive(Clone, Debug)]
pub struct TerminalRestoreInfo {
    pub command: Option<String>,
    pub cwd: camino::Utf8PathBuf,
    pub is_command_pane: bool,
}

impl Session {
    pub fn from_state(state: &crate::tui::state::AppState) -> Self {
        let snapshot = state.files.load();
        let panes: Vec<PaneRecord> = state
            .pane_manager
            .window_stack
            .iter()
            .filter_map(|window| pane_record_from_window(window, &snapshot, state))
            .collect();

        let mut highlight_ranges = HashMap::new();
        for (key, preview) in &state.previews {
            let ranges = &preview.highlights;
            if ranges.is_empty() {
                continue;
            }
            if let Some(path) = crate::loader::path_for_file_key(&snapshot, *key)
                && let Ok(rel) = path.strip_prefix(&state.root)
            {
                highlight_ranges.insert(rel.to_string(), ranges.clone());
            }
        }

        let symbol_highlights: Vec<SessionSymbolGroup> = state
            .symbol_highlights
            .iter()
            .filter_map(|group| {
                let path = crate::loader::path_for_file_key(&snapshot, group.origin_file)?;
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
            panes,
            highlight_ranges,
            symbol_highlights,
            cargo_features: state.cargo_features.clone(),
            theme: Some(state.saved_theme.name().to_string()),
            conversation_expanded,
            conversation_query: state.tree_view.query.text().to_string(),
            discussion_highlights,
            mcp_servers: state.mcp_servers.clone(),
            aliases: state.project_aliases.clone(),
            trusted: state.trusted,
        }
    }

    // Only on a fresh AppState: previews and symbol_highlights are not cleared first.
    pub fn apply(self, state: &mut crate::tui::state::AppState) {
        use crate::conversation::NodeId;
        use crate::loader::LoadedFile;
        use crate::tui::pane_manager::Window;
        use crate::tui::pane_manager::WindowState;
        use crate::tui::state::DiscussionState;
        use crate::tui::state::GitLogWindowState;
        use crate::tui::state::SymbolHighlightGroup;
        use crate::tui::theme::HelixTheme;
        use crate::tui::widgets::input_line::InputLine;
        state.cargo_features = self.cargo_features.clone();

        if let Some(name) = &self.theme
            && let Some(theme) = HelixTheme::from_name(name)
        {
            state.theme = theme.clone();
            state.saved_theme = theme;
        }

        let mut missing_paths: Vec<camino::Utf8PathBuf> = Vec::new();
        let snapshot_before = state.files.load();
        for record in &self.panes {
            if let PaneEntry::FilePreview { path, .. } = &record.entry {
                let abs = state.root.join(path);
                if crate::loader::file_key_for_path(&snapshot_before, &abs).is_none() {
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

        for PaneRecord { pane_size, entry } in self.panes {
            let mut window_state = WindowState {
                pane_size,
                ..Default::default()
            };
            let window = match entry {
                PaneEntry::FilePreview { path, scroll } => {
                    let abs = state.root.join(&path);
                    let Some(key) = crate::loader::file_key_for_path(&snapshot_after, &abs) else {
                        continue;
                    };
                    snapshot_after[key.0]
                        .needs_full_load
                        .store(true, Ordering::Relaxed);
                    window_state.scroll = scroll;
                    Window::FilePreview(key)
                }

                PaneEntry::Terminal {
                    command,
                    cwd,
                    is_command_pane,
                } => {
                    let id = state.next_id();
                    state.pending_terminals.insert(
                        id,
                        TerminalRestoreInfo {
                            command,
                            cwd,
                            is_command_pane,
                        },
                    );
                    Window::Terminal(id)
                }

                PaneEntry::GitLog {
                    source,
                    query,
                    scroll,
                } => {
                    let id = state.next_id();
                    let mut win = GitLogWindowState {
                        source,
                        query: InputLine::with_text(query.as_str()),
                        ..Default::default()
                    };
                    if !query.is_empty() {
                        crate::tui::panes::git_log_pane::apply_filter(&mut win);
                    }
                    state.git_logs.insert(id, win);
                    window_state.scroll = scroll;
                    Window::GitLog(id)
                }

                PaneEntry::CommitDiff { hash } => {
                    let Ok(bytes) = hex::decode(&hash) else {
                        continue;
                    };
                    let Ok(id) = <[u8; 20]>::try_from(bytes.as_slice()) else {
                        continue;
                    };
                    Window::CommitDiff(id)
                }

                PaneEntry::ConversationTree => Window::ConversationTree,

                PaneEntry::Discussion { node, agent, draft } => {
                    let id = state.next_id();
                    state.discussions.insert(
                        id,
                        DiscussionState {
                            loaded: node.is_none(),
                            node: node.as_deref().map(NodeId::new),
                            agent,
                            draft: InputLine::with_text(draft),
                            ..Default::default()
                        },
                    );
                    Window::Discussion(id)
                }
            };
            new_stack.push(window);
            new_states.insert(window, window_state);
        }

        state.tree_view.expanded = self
            .conversation_expanded
            .iter()
            .map(|id| NodeId::new(id))
            .collect();
        state.tree_view.query.set_text(self.conversation_query);
        state.discussion_highlights = self
            .discussion_highlights
            .into_iter()
            .map(|(node, highlights)| (NodeId::new(&node), highlights))
            .collect();
        state.mcp_servers = self.mcp_servers;
        state.project_aliases = self.aliases;
        state.trusted = self.trusted;

        if !new_stack.is_empty() {
            state.pane_manager.window_stack = new_stack;
            state.pane_manager.window_states = new_states;
            state.pane_manager.focused_window = state.pane_manager.window_stack.first().copied();
        }

        let snapshot = state.files.load();
        for (rel, ranges) in self.highlight_ranges {
            let abs = state.root.join(&rel);
            if let Some(key) = crate::loader::file_key_for_path(&snapshot, &abs) {
                state.set_highlights(key, ranges);
            }
        }

        // Locations are never restored (they'd be stale); only origin identity is
        // kept. `needs_rebuild` triggers re-resolution against a running LSP worker,
        // same as the modified-origin-file rebuild path.
        for entry in self.symbol_highlights {
            let abs = state.root.join(&entry.origin_path);
            if let Some(origin_file) = crate::loader::file_key_for_path(&snapshot, &abs) {
                state.symbol_highlights.push(SymbolHighlightGroup {
                    qualified_name: entry.qualified_name,
                    origin_file,
                    origin_line: entry.origin_line,
                    origin_char: entry.origin_char,
                    origin_word: entry.origin_word,
                    origin_byte_start: None,
                    origin_byte_end: None,
                    color: r3bl_tui::RgbValue::from_u8(
                        entry.color[0],
                        entry.color[1],
                        entry.color[2],
                    ),
                    locations: Vec::new(),
                    needs_rebuild: true,
                });
            }
        }
        state.next_palette_index =
            state.symbol_highlights.len() % crate::tui::state::SYMBOL_PALETTE.len();
    }
}

fn pane_record_from_window(
    window: &crate::tui::pane_manager::Window,
    files: &[Arc<crate::loader::LoadedFile>],
    state: &crate::tui::state::AppState,
) -> Option<PaneRecord> {
    use crate::tui::pane_manager::Window;
    let root = &state.root;
    let window_state = state
        .pane_manager
        .window_states
        .get(window)
        .cloned()
        .unwrap_or_default();
    let entry = match window {
        Window::FilePreview(key) => {
            let path = crate::loader::path_for_file_key(files, *key)?;
            let rel = path.strip_prefix(root).ok()?;
            PaneEntry::FilePreview {
                path: rel.to_string(),
                scroll: window_state.scroll,
            }
        }
        // A report is read and gone: it is never part of the session.
        Window::Report(_) => {
            return None;
        }
        Window::GitLog(id) => {
            let win = state.git_logs.get(id)?;
            PaneEntry::GitLog {
                source: win.source.clone(),
                query: win.query.text().to_string(),
                scroll: window_state.scroll,
            }
        }
        Window::CommitDiff(id) => PaneEntry::CommitDiff {
            hash: hex::encode(id),
        },
        Window::ConversationTree => PaneEntry::ConversationTree,
        Window::Discussion(id) => {
            let discussion = state.discussions.get(id)?;
            PaneEntry::Discussion {
                node: discussion
                    .node
                    .as_ref()
                    .map(|node| node.as_str().to_string()),
                agent: discussion.agent,
                draft: discussion.draft.text().to_string(),
            }
        }
        Window::Terminal(id) => 'terminal: {
            // Editor terminals save as FilePreview so restore brings back the preview.
            if let Some(file_key) = state.terminal_to_preview.get(id)
                && let Some(path) = crate::loader::path_for_file_key(files, *file_key)
                && let Ok(rel) = path.strip_prefix(root)
            {
                break 'terminal PaneEntry::FilePreview {
                    path: rel.to_string(),
                    scroll: 0,
                };
            }
            let pane = state.terminal_panes.get(id)?.lock();
            if pane.exited {
                return None;
            }
            PaneEntry::Terminal {
                command: pane.command.clone(),
                cwd: pane.cwd.clone(),
                is_command_pane: pane.is_command_pane,
            }
        }
    };
    Some(PaneRecord {
        pane_size: window_state.pane_size,
        entry,
    })
}

/// The MCP servers a session saves, without reading the rest of it: the tools of a
/// repository are what a client outside Explorer asks for, and nothing else in the
/// session concerns it.
#[derive(Debug, Default, serde::Deserialize)]
struct McpOnly {
    #[serde(default)]
    mcp_servers: Vec<crate::agent::mcp::McpServerDecl>,
}

/// The MCP servers of the repository at `repo_root`, empty when it has no session.
pub fn session_mcp_servers(repo_root: &camino::Utf8Path) -> Vec<crate::agent::mcp::McpServerDecl> {
    let Some(path) = session_file_path(repo_root) else {
        return Vec::new();
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => {
            tracing::warn!("failed to read session: {error}");
            return Vec::new();
        }
    };
    match serde_json::from_str::<McpOnly>(&content) {
        Ok(only) => only.mcp_servers,
        Err(error) => {
            tracing::warn!("failed to parse session: {error}");
            Vec::new()
        }
    }
}

pub fn load_session(repo_root: &camino::Utf8Path) -> Option<Session> {
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
    Some(session)
}

pub fn save_session(state: &crate::tui::state::AppState) -> std::io::Result<()> {
    let path = session_file_path(&state.root)
        .ok_or_else(|| std::io::Error::other("could not determine session path"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content =
        serde_json::to_string_pretty(&Session::from_state(state)).map_err(std::io::Error::other)?;
    crate::util::fs::write_atomic(&path, &content)
}

pub fn session_dir() -> Option<camino::Utf8PathBuf> {
    Some(crate::util::fs::data_dir()?.join("sessions"))
}

pub fn session_file_path(repo_root: &camino::Utf8Path) -> Option<camino::Utf8PathBuf> {
    let dir = session_dir()?;
    let prefix = crate::util::fs::repo_hash(repo_root)?;
    Some(dir.join(format!("{prefix}.json")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::FileKey;
    use crate::tui::pane_manager::{PaneSize, Window, WindowState};
    use crate::tui::state::TerminalPane;
    use crate::util::test_support::temp_root;
    use camino::Utf8PathBuf;
    use parking_lot::Mutex;
    use r3bl_tui::VPSize;
    use std::sync::Arc;

    fn dummy_terminal_pane(cwd: Utf8PathBuf, command: Option<String>) -> TerminalPane {
        let (mut pane, _) = TerminalPane::for_test(VPSize::default());
        pane.is_command_pane = command.is_some();
        pane.cwd = cwd;
        pane.command = command;
        pane
    }

    #[test]
    fn session_serialization_round_trip() {
        use crate::lsp::CargoFeatureSelection;
        let session = Session {
            panes: vec![
                PaneRecord {
                    pane_size: PaneSize::Half,
                    entry: PaneEntry::FilePreview {
                        path: "src/main.rs".to_string(),
                        scroll: 5,
                    },
                },
                PaneRecord {
                    pane_size: PaneSize::Quarter,
                    entry: PaneEntry::Terminal {
                        command: Some("cargo test".to_string()),
                        cwd: Utf8PathBuf::from("/tmp/repo"),
                        is_command_pane: true,
                    },
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
            cargo_features: CargoFeatureSelection::Explicit {
                no_default_features: true,
                features: vec!["foo".to_string()],
            },
            theme: None,
            conversation_expanded: Vec::new(),
            conversation_query: String::new(),
            discussion_highlights: HashMap::new(),
            mcp_servers: Vec::new(),
            aliases: Vec::new(),
            trusted: false,
        };

        let value = serde_json::to_value(&session).unwrap();
        let restored: Session = serde_json::from_value(value.clone()).unwrap();
        let value2 = serde_json::to_value(&restored).unwrap();
        assert_eq!(value, value2);
    }

    #[test]
    fn a_previewed_theme_is_not_saved_until_it_is_confirmed() {
        use crate::tui::state::AppState;
        use crate::tui::theme::HelixTheme;
        let (_dir, root) = temp_root();
        let mut state = AppState::for_test(root);
        let confirmed = state.saved_theme.name().to_string();
        let other = HelixTheme::theme_names()
            .find(|name| *name != confirmed)
            .expect("a second bundled theme");
        state.theme = HelixTheme::from_name(other).expect("bundled theme");
        assert_eq!(
            Session::from_state(&state).theme.as_deref(),
            Some(confirmed.as_str())
        );
    }

    #[test]
    fn session_deserializes_without_symbol_highlights_field() {
        // Older session files (or any writer that omits the field) must still load,
        // defaulting to no persisted symbol highlights.
        let json = serde_json::json!({
            "panes": [],
            "highlight_ranges": {},
        });
        let session: Session = serde_json::from_value(json).unwrap();
        assert!(session.symbol_highlights.is_empty());
    }

    #[test]
    fn session_from_state_preserves_window_stack_order() {
        use crate::loader::LoadedFile;
        use crate::tui::state::AppState;
        let (_dir, root) = temp_root();

        let file_path = root.join("src/main.rs");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, "fn main() {}\n").unwrap();

        let loaded = LoadedFile::load(file_path.into()).unwrap();
        let mut state = AppState::for_test_with_files(vec![loaded], root.clone());

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
            .map(|record| match record.entry {
                PaneEntry::FilePreview { .. } => "file_preview",
                PaneEntry::Terminal { .. } => "terminal",
                PaneEntry::GitLog { .. } => "git_log",
                PaneEntry::CommitDiff { .. } => "commit_diff",
                PaneEntry::ConversationTree => "conversation_tree",
                PaneEntry::Discussion { .. } => "discussion",
            })
            .collect();
        assert_eq!(kinds, vec!["file_preview", "terminal"]);

        assert!(matches!(
            session.panes[0],
            PaneRecord {
                pane_size: PaneSize::Half,
                entry: PaneEntry::FilePreview { path: _, scroll: 3 }
            }
        ));
        assert!(matches!(
            session.panes[1].entry,
            PaneEntry::Terminal {
                command: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn session_apply_creates_stubs_and_restores_highlights() {
        use crate::tui::state::AppState;
        let (_dir, root) = temp_root();
        let mut state = AppState::for_test(root.clone());

        let session = Session {
            panes: vec![PaneRecord {
                pane_size: PaneSize::Full,
                entry: PaneEntry::FilePreview {
                    path: "missing.rs".to_string(),
                    scroll: 0,
                },
            }],
            highlight_ranges: {
                let mut m = HashMap::new();
                m.insert("missing.rs".to_string(), vec![(1, 5)]);
                m
            },
            symbol_highlights: Vec::new(),
            ..Default::default()
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

        assert_eq!(state.previews.len(), 1);
        assert_eq!(state.highlights(FileKey(0)), [(1, 5)]);
    }

    #[test]
    fn session_apply_batches_stubs_so_multiple_missing_files_get_distinct_keys() {
        use crate::loader::LoadedFile;
        use crate::tui::state::AppState;
        let (_dir, root) = temp_root();

        // Create one real file so we have a non-empty files list.
        let existing_path = root.join("b.rs");
        std::fs::create_dir_all(existing_path.parent().unwrap()).unwrap();
        std::fs::write(&existing_path, "// b.rs content\n").unwrap();
        let loaded = LoadedFile::load(existing_path.into()).unwrap();
        let mut state = AppState::for_test_with_files(vec![loaded], root.clone());

        // Session has two missing-file previews whose paths sort differently
        // from the order they appear in the session.
        let session = Session {
            panes: vec![
                PaneRecord {
                    pane_size: PaneSize::Half,
                    entry: PaneEntry::FilePreview {
                        path: "missing_x.rs".to_string(),
                        scroll: 0,
                    },
                },
                PaneRecord {
                    pane_size: PaneSize::Half,
                    entry: PaneEntry::FilePreview {
                        path: "missing_a.rs".to_string(),
                        scroll: 10,
                    },
                },
            ],
            ..Default::default()
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
        use crate::tui::state::AppState;
        let (_dir, root) = temp_root();
        let mut state = AppState::for_test(root.clone());

        let session = Session {
            panes: vec![PaneRecord {
                pane_size: PaneSize::Quarter,
                entry: PaneEntry::Terminal {
                    command: Some("cargo test".to_string()),
                    cwd: root.clone(),
                    is_command_pane: true,
                },
            }],
            ..Default::default()
        };

        session.apply(&mut state);

        assert_eq!(state.pending_terminals.len(), 1);
        let pending = state.pending_terminals.get(&0).unwrap();
        assert_eq!(pending.command, Some("cargo test".to_string()));
        assert_eq!(pending.cwd, root);
        assert!(pending.is_command_pane);
    }

    #[test]
    fn session_apply_restores_symbol_groups() {
        use crate::loader::LoadedFile;
        use crate::tui::state::AppState;
        let (_dir, root) = temp_root();

        let file_path = root.join("src/util.rs");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(
            &file_path,
            "fn add_numbers(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
        )
        .unwrap();
        let loaded = LoadedFile::load(file_path.into()).unwrap();
        let mut state = AppState::for_test_with_files(vec![loaded], root.clone());

        let session = Session {
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
            ..Default::default()
        };

        session.apply(&mut state);

        assert_eq!(state.symbol_highlights.len(), 1);
        let group = &state.symbol_highlights[0];
        assert_eq!(group.qualified_name, "symtest::util::add_numbers");
        assert_eq!(group.origin_line, 0);
        assert_eq!(group.origin_char, 3);
        assert_eq!(group.origin_word.as_deref(), Some("add_numbers"));
        assert_eq!(group.color, r3bl_tui::RgbValue::from_u8(255, 100, 100));
        assert!(group.needs_rebuild);
        assert!(group.locations.is_empty());
        assert!(group.origin_byte_start.is_none());
        assert!(group.origin_byte_end.is_none());

        assert_eq!(state.next_palette_index, 1);
    }

    #[test]
    fn conversation_tree_round_trips_with_its_expanded_set() {
        use crate::conversation::NodeId;
        use crate::tui::state::AppState;
        let (_dir, root) = temp_root();
        let mut state = AppState::for_test(root.clone());
        state.pane_manager.window_stack = vec![Window::ConversationTree];
        state
            .tree_view
            .expanded
            .insert(NodeId::new("2026-10-05T14-32-07Z"));

        let session = Session::from_state(&state);
        assert_eq!(session.conversation_expanded, ["2026-10-05T14-32-07Z"]);
        let json = serde_json::to_string(&session).expect("serialize session");
        let restored: Session = serde_json::from_str(&json).expect("deserialize session");

        let mut fresh = AppState::for_test(root);
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
    fn conversation_tree_filter_round_trips_through_session() {
        use crate::tui::state::AppState;
        let (_dir, root) = temp_root();
        let mut state = AppState::for_test(root.clone());
        state.tree_view.query.set_text("queue esc");

        let session = Session::from_state(&state);
        let json = serde_json::to_string(&session).expect("serialize session");
        let restored: Session = serde_json::from_str(&json).expect("deserialize session");

        let mut fresh = AppState::for_test(root);
        restored.apply(&mut fresh);
        assert_eq!(fresh.tree_view.query.text(), "queue esc");
    }

    #[test]
    fn aliases_round_trip_through_session() {
        use crate::config::Alias;
        use crate::tui::state::AppState;
        let (_dir, root) = temp_root();
        let mut state = AppState::for_test(root.clone());
        let alias = Alias {
            name: "ci".to_string(),
            command: "cargo test".to_string(),
        };
        state.project_aliases.push(alias.clone());

        let json = serde_json::to_string(&Session::from_state(&state)).expect("serialize");
        let restored: Session = serde_json::from_str(&json).expect("deserialize session");

        let mut fresh = AppState::for_test(root);
        restored.apply(&mut fresh);
        assert_eq!(fresh.project_aliases, [alias]);
    }

    #[test]
    fn trust_round_trips_through_session() {
        use crate::tui::state::AppState;
        let (_dir, root) = temp_root();
        let mut state = AppState::for_test(root.clone());
        assert!(
            !state.trusted,
            "a repository is untrusted until the user says so"
        );
        state.trusted = true;

        let json = serde_json::to_string(&Session::from_state(&state)).expect("serialize");
        let restored: Session = serde_json::from_str(&json).expect("deserialize session");

        let mut fresh = AppState::for_test(root.clone());
        restored.apply(&mut fresh);
        assert!(fresh.trusted);

        // An older session file that predates the flag loads as untrusted.
        let path = session_file_path(&root).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}").unwrap();
        let mut old = AppState::for_test(root);
        load_session(&old.root)
            .expect("an older file must load")
            .apply(&mut old);
        assert!(!old.trusted);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn mcp_servers_round_trip_through_session() {
        use crate::agent::mcp::McpServerDecl;
        use crate::tui::state::AppState;
        let (_dir, root) = temp_root();
        let mut state = AppState::for_test(root.clone());
        let egui = McpServerDecl {
            name: "egui".to_string(),
            program: "egui-mcp".to_string(),
        };
        state.mcp_servers.push(egui.clone());

        let json = serde_json::to_string(&Session::from_state(&state)).expect("serialize");
        let restored: Session = serde_json::from_str(&json).expect("deserialize session");

        let mut fresh = AppState::for_test(root);
        restored.apply(&mut fresh);
        assert_eq!(fresh.mcp_servers, [egui]);
    }

    #[test]
    fn discussion_highlights_round_trip_through_session() {
        use crate::conversation::NodeId;
        use crate::tui::state::AppState;
        use crate::tui::state::NodeHighlights;
        let (_dir, root) = temp_root();
        let mut state = AppState::for_test(root.clone());
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

        let mut fresh = AppState::for_test(root);
        restored.apply(&mut fresh);
        assert_eq!(fresh.discussion_highlights.len(), 1);
        assert_eq!(fresh.discussion_highlights.get(&node), Some(&highlights));
    }

    #[test]
    fn commit_diff_pane_round_trips_through_session() {
        use crate::tui::state::AppState;
        let (_dir, root) = temp_root();
        let mut state = AppState::for_test(root.clone());
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
        let kinds: Vec<_> = session
            .panes
            .iter()
            .map(|record| match record.entry {
                PaneEntry::FilePreview { .. } => "file_preview",
                PaneEntry::Terminal { .. } => "terminal",
                PaneEntry::GitLog { .. } => "git_log",
                PaneEntry::CommitDiff { .. } => "commit_diff",
                PaneEntry::ConversationTree => "conversation_tree",
                PaneEntry::Discussion { .. } => "discussion",
            })
            .collect();
        assert_eq!(kinds, vec!["commit_diff"]);
        let PaneEntry::CommitDiff { hash } = &session.panes[0].entry else {
            panic!("expected commit_diff entry");
        };
        assert_eq!(hash, &hex::encode([0x42u8; 20]));

        let mut restored = AppState::for_test(root.clone());
        session.apply(&mut restored);
        assert_eq!(
            restored.pane_manager.window_stack,
            vec![Window::CommitDiff(id)]
        );
    }

    #[test]
    fn git_log_pane_round_trips_with_branch_and_query() {
        use crate::tui::state::AppState;
        use crate::tui::state::GitLogWindowState;
        use crate::tui::state::LogSource;
        use crate::tui::widgets::input_line::InputLine;
        let (_dir, root) = temp_root();
        let mut state = AppState::for_test(root.clone());
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
            query: InputLine::with_text("fix:"),
            ..Default::default()
        };
        state.git_logs.insert(7, win);

        let session = Session::from_state(&state);
        let PaneEntry::GitLog {
            source,
            query,
            scroll,
        } = &session.panes[0].entry
        else {
            panic!("expected git_log entry");
        };
        assert_eq!(source, &LogSource::Branch("origin/main".to_string()));
        assert_eq!(query, "fix:");
        assert_eq!(scroll, &4);

        let mut restored = AppState::for_test(root.clone());
        session.apply(&mut restored);
        // The window takes a fresh id: the saved one means nothing to the new run.
        assert_eq!(restored.pane_manager.window_stack, vec![Window::GitLog(0)]);
        let restored_win = restored.git_logs.get(&0).expect("window state");
        assert_eq!(
            restored_win.source,
            LogSource::Branch("origin/main".to_string())
        );
        assert_eq!(restored_win.query.text(), "fix:");
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
        use crate::loader::LoadedFile;
        use crate::tui::state::AppState;
        let (_dir, root) = temp_root();

        let file_path = root.join("src/main.rs");
        std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        std::fs::write(&file_path, "fn main() {}\n").unwrap();

        let loaded = LoadedFile::load(file_path.into()).unwrap();
        let mut state = AppState::for_test_with_files(vec![loaded], root.clone());
        state
            .pane_manager
            .push_window(Window::FilePreview(FileKey(0)));

        save_session(&state).unwrap();

        let path = session_file_path(&root).unwrap();
        assert!(path.exists(), "session file should exist at {path}");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.is_empty());

        let parsed: Session = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.panes.len(), 1);
        assert!(matches!(
            &parsed.panes[0],
            PaneRecord { entry: PaneEntry::FilePreview { path, .. }, .. } if path == "src/main.rs",
        ));

        // Clean up.
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_session_mcp_servers_are_read_without_the_rest_of_the_file() {
        let (_dir, root) = temp_root();
        let decl = crate::agent::mcp::McpServerDecl {
            name: "demo".to_string(),
            program: "/bin/demo".to_string(),
        };
        let only = serde_json::json!({"mcp_servers": [decl]});
        let path = session_file_path(&root).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, only.to_string()).unwrap();
        assert_eq!(session_mcp_servers(&root), [decl.clone()].as_slice());

        // A file this module cannot parse yields none rather than failing the caller.
        std::fs::write(&path, "{ not json").unwrap();
        assert!(session_mcp_servers(&root).is_empty());

        // A repository with no session has none either.
        std::fs::remove_file(&path).unwrap();
        assert!(session_mcp_servers(&root).is_empty());
    }
}
