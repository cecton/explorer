use crate::loader::{FileKey, LoadedFile, file_key_for_path, path_for_file_key};
use crate::tui::AppState;
use crate::tui::TerminalPane;
use crate::tui::pane_manager::{PaneSize, Window, WindowState};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

pub const SESSION_VERSION: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub repo_root: Utf8PathBuf,
    pub panes: Vec<PaneEntry>,
    pub highlight_ranges: HashMap<String, Vec<(usize, usize)>>,
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
            .filter_map(|window| {
                pane_entry_from_window(
                    window,
                    &snapshot,
                    &state.root,
                    &state.pane_manager.window_states,
                    &state.terminal_panes,
                    &state.terminal_to_preview,
                )
            })
            .collect();

        let mut highlight_ranges = HashMap::new();
        for (key, ranges) in &state.highlight_ranges {
            if let Some(path) = path_for_file_key(&snapshot, *key)
                && let Ok(rel) = path.strip_prefix(&state.root)
            {
                highlight_ranges.insert(rel.to_string(), ranges.clone());
            }
        }

        Self {
            version: SESSION_VERSION,
            repo_root: state.root.clone(),
            panes,
            highlight_ranges,
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
            }
        }

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

        if state
            .pane_manager
            .window_stack
            .contains(&Window::FileNamePicker)
        {
            state.recompute_file_name_picker_results();
        }
    }
}

fn pane_entry_from_window(
    window: &Window,
    files: &[Arc<LoadedFile>],
    root: &Utf8Path,
    window_states: &HashMap<Window, WindowState>,
    terminal_panes: &HashMap<usize, Arc<Mutex<TerminalPane>>>,
    terminal_to_preview: &HashMap<usize, FileKey>,
) -> Option<PaneEntry> {
    let window_state = window_states.get(window).cloned().unwrap_or_default();
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
        Window::FileNamePicker | Window::ThemePicker => None,
        Window::Terminal(id) => {
            // Editor terminals save as FilePreview so restore brings back the preview.
            if let Some(file_key) = terminal_to_preview.get(id)
                && let Some(path) = path_for_file_key(files, *file_key)
                && let Ok(rel) = path.strip_prefix(root)
            {
                return Some(PaneEntry::FilePreview {
                    path: rel.to_string(),
                    scroll: 0,
                    pane_size: window_state.pane_size.into(),
                });
            }
            let pane = terminal_panes
                .get(id)?
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
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
    if session.version != SESSION_VERSION {
        tracing::debug!(
            "session version mismatch ({} != {}), discarding",
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

pub fn session_file_path(repo_root: &Utf8Path) -> Option<Utf8PathBuf> {
    let dir = session_dir()?;
    let canonical = std::fs::canonicalize(repo_root).ok()?;
    let canonical = Utf8PathBuf::from_path_buf(canonical).ok()?;
    let hash = Sha256::digest(canonical.as_str().as_bytes());
    let prefix = hex::encode(&hash[..8]);
    Some(dir.join(format!("{prefix}.json")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::FileKey;
    use crate::tui::HelixTheme;
    use crate::tui::pane_manager::{PaneSize, Window, WindowState};
    use arc_swap::ArcSwap;
    use camino::Utf8PathBuf;
    use r3bl_tui::{OfsBufVT100, Size};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    fn temp_root() -> (TempDir, Utf8PathBuf) {
        let dir = TempDir::new().expect("temp dir");
        let root = Utf8PathBuf::from_path_buf(dir.path().canonicalize().unwrap()).unwrap();
        (dir, root)
    }

    fn empty_state(root: Utf8PathBuf) -> AppState {
        let files = Arc::new(ArcSwap::from_pointee(Vec::<Arc<LoadedFile>>::new()));
        AppState::new(files, root, HelixTheme::default())
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
        };

        let value = serde_json::to_value(&session).unwrap();
        let restored: Session = serde_json::from_value(value.clone()).unwrap();
        let value2 = serde_json::to_value(&restored).unwrap();
        assert_eq!(value, value2);
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
        let mut state = AppState::new(files, root.clone(), theme);

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
        let mut state = AppState::new(files, root.clone(), theme);

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
        };

        session.apply(&mut state);

        assert_eq!(state.pane_manager.window_stack.len(), 1);
        assert!(matches!(
            state.pane_manager.window_stack[0],
            Window::FileNamePicker
        ));
        assert!(state.highlight_ranges.is_empty());
        assert!(state.pending_terminals.is_empty());
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
        let mut state = AppState::new(files, root.clone(), theme);
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
}
