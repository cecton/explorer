use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Stable index into the file list.
///
/// The underlying `Vec<Arc<LoadedFile>>` is append-only: files are never
/// removed or reordered, only marked `removed = true`. This makes the
/// raw `usize` permanently stable for the lifetime of `AppState`.
/// The newtype exists to prevent accidental confusion with other `usize` values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileKey(pub usize);

pub struct FileData {
    pub content: String,
    pub line_starts: Vec<usize>,
}

impl FileData {
    pub fn line(&self, idx: usize) -> &str {
        let start = self.line_starts.get(idx).copied().unwrap_or(0);
        let end = self
            .line_starts
            .get(idx + 1)
            .map(|&e| e.saturating_sub(1))
            .unwrap_or(self.content.len());
        if start > end || start > self.content.len() {
            return "";
        }
        &self.content[start..end.min(self.content.len())]
    }

    pub fn extract_text(&self, start_byte: usize, end_byte: usize) -> Option<String> {
        let (start_byte, end_byte) = if start_byte > end_byte {
            (end_byte, start_byte)
        } else {
            (start_byte, end_byte)
        };
        let result = self.content[start_byte..end_byte].to_string();
        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }

    pub(crate) fn char_to_byte(&self, line_idx: usize, char_idx: usize) -> usize {
        let s = self.line(line_idx);
        s.char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(s.len())
    }

    /// The [`crate::util::text::word_bounds`] of the file's content under `cursor_byte`.
    pub fn word_bounds(&self, cursor_byte: usize) -> (usize, usize) {
        crate::util::text::word_bounds(&self.content, cursor_byte)
    }

    pub fn line_bounds(&self, line_idx: usize) -> (usize, usize) {
        let line_byte_start = self.line_starts[line_idx];
        let line = self.line(line_idx);
        (line_byte_start, line_byte_start + line.len())
    }
}

pub struct LoadedFile {
    pub path: camino::Utf8PathBuf,
    pub data: parking_lot::RwLock<FileData>,
    pub colored_lines: parking_lot::Mutex<Vec<crate::highlight::ColoredLine>>,
    pub removed: AtomicBool,
    pub needs_full_load: AtomicBool,
}

impl LoadedFile {
    pub fn load(path: PathBuf) -> Option<Arc<Self>> {
        let path = camino::Utf8PathBuf::from_path_buf(path).ok()?;
        let content = fs::read_to_string(&path).ok()?;
        let line_starts = compute_line_starts(&content);
        Some(Arc::new(Self {
            path,
            data: parking_lot::RwLock::new(FileData {
                content,
                line_starts,
            }),
            colored_lines: parking_lot::Mutex::new(vec![]),
            removed: AtomicBool::new(false),
            needs_full_load: AtomicBool::new(false),
        }))
    }

    /// Re-reads the file from disk, updates content and line_starts, clears colored_lines.
    /// Returns false if the file could not be read.
    pub fn reload(&self) -> bool {
        let Ok(content) = fs::read_to_string(&self.path) else {
            return false;
        };
        let line_starts = compute_line_starts(&content);
        let mut data = self.data.write();
        data.content = content;
        data.line_starts = line_starts;
        drop(data);
        *self.colored_lines.lock() = vec![];
        true
    }
}

fn compute_line_starts(content: &str) -> Vec<usize> {
    let capacity = content.len() / 100 + 1;
    let mut line_starts = Vec::with_capacity(capacity);
    line_starts.push(0usize);
    for (i, &b) in content.as_bytes().iter().enumerate() {
        if b == b'\n' && i + 1 < content.len() {
            line_starts.push(i + 1);
        }
    }
    line_starts.shrink_to_fit();
    line_starts
}

pub fn find_git_root() -> PathBuf {
    let start = env::current_dir().expect("cannot get current directory");
    find_git_root_from(start)
}

fn find_git_root_from(start: PathBuf) -> PathBuf {
    let mut dir = start;
    loop {
        // `.git` is a directory in a normal checkout but a `gitdir:` file in a
        // linked worktree, so only existence can be required here.
        if dir.join(".git").exists() {
            return dir;
        }
        if !dir.pop() {
            panic!("no git repository found (no .git in any parent)");
        }
    }
}

/// Dot-name of the git directory, by the single-source-of-truth convention of
/// [`IGNORED_DIRS`], so the walk, the watcher filter and the trigger check agree.
pub const GIT_DIR: &str = ".git";

/// Directory names skipped everywhere we enumerate or watch repo files.
/// Single source of truth for both the walk (`walk_repo_files`) and the
/// watcher's live event filter (`watcher.rs`).
pub const IGNORED_DIRS: [&str; 2] = ["target", GIT_DIR];

/// Whether a repository-relative path sits under an [`IGNORED_DIRS`] directory at any
/// depth: the walk prunes them everywhere, so the watcher must drop their events too.
pub fn is_ignored(rel: &camino::Utf8Path) -> bool {
    rel.components()
        .any(|component| IGNORED_DIRS.contains(&component.as_str()))
}

/// Walk `root` recursively and return every regular-file path, skipping the
/// [`IGNORED_DIRS`]. Shared by the initial load (`main.rs`) and the watcher's
/// overflow rescan so both see the same set of files.
pub fn walk_repo_files(root: &camino::Utf8Path) -> Vec<PathBuf> {
    jwalk::WalkDir::new(root)
        .process_read_dir(move |_, _, _, children| {
            children.retain(|entry| {
                entry.as_ref().map_or(true, |e| {
                    !e.file_name
                        .to_str()
                        .is_some_and(|n| IGNORED_DIRS.contains(&n))
                })
            });
        })
        .into_iter()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if !entry.file_type().is_file() {
                return None;
            }
            Some(entry.path())
        })
        .collect()
}

pub fn path_for_file_key(files: &[Arc<LoadedFile>], key: FileKey) -> Option<&camino::Utf8Path> {
    files.get(key.0).map(|f| f.path.as_ref())
}

pub fn file_key_for_path(files: &[Arc<LoadedFile>], path: &camino::Utf8Path) -> Option<FileKey> {
    files.iter().position(|f| f.path == path).map(FileKey)
}

impl LoadedFile {
    pub fn stub(path: camino::Utf8PathBuf) -> Arc<Self> {
        Arc::new(Self {
            path,
            data: parking_lot::RwLock::new(FileData {
                content: String::new(),
                line_starts: vec![0],
            }),
            colored_lines: parking_lot::Mutex::new(vec![]),
            removed: AtomicBool::new(true),
            needs_full_load: AtomicBool::new(false),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fd(content: &str) -> FileData {
        FileData {
            content: content.to_string(),
            line_starts: compute_line_starts(content),
        }
    }

    #[test]
    fn char_to_byte_ascii() {
        let data = fd("hello");
        assert_eq!(data.char_to_byte(0, 0), 0);
        assert_eq!(data.char_to_byte(0, 1), 1);
        assert_eq!(data.char_to_byte(0, 4), 4);
        assert_eq!(data.char_to_byte(0, 5), 5);
        assert_eq!(data.char_to_byte(0, 100), 5);
    }

    #[test]
    fn char_to_byte_multibyte() {
        let data = fd("héllo");
        assert_eq!(data.char_to_byte(0, 0), 0); // 'h'
        assert_eq!(data.char_to_byte(0, 1), 1); // 'é' starts at byte 1
        assert_eq!(data.char_to_byte(0, 2), 3); // 'l' starts at byte 3
        assert_eq!(data.char_to_byte(0, 5), 6); // past end
    }

    #[test]
    fn extract_text_basic() {
        let data = fd("hello\nworld");
        assert_eq!(data.extract_text(0, 5), Some("hello".to_string()));
        assert_eq!(data.extract_text(6, 11), Some("world".to_string()));
        assert_eq!(data.extract_text(0, 11), Some("hello\nworld".to_string()));
    }

    #[test]
    fn extract_text_swapped() {
        let data = fd("hello");
        assert_eq!(data.extract_text(3, 1), Some("el".to_string()));
    }

    #[test]
    fn extract_text_empty() {
        let data = fd("hello");
        assert_eq!(data.extract_text(2, 2), None);
    }

    #[test]
    fn path_for_file_key_returns_path() {
        let file = LoadedFile::load(PathBuf::from("src/loader.rs"))
            .expect("loader.rs exists and is UTF-8");
        let files = vec![file];
        let key = FileKey(0);
        let path = path_for_file_key(&files, key);
        assert!(path.is_some());
    }

    #[test]
    fn file_key_for_path_finds_matching_file() {
        let file = LoadedFile::load(PathBuf::from("src/loader.rs"))
            .expect("loader.rs exists and is UTF-8");
        let files = vec![file];
        let path = path_for_file_key(&files, FileKey(0)).unwrap();
        assert_eq!(file_key_for_path(&files, path), Some(FileKey(0)));
    }

    #[test]
    fn stub_file_is_marked_removed() {
        let stub = LoadedFile::stub(camino::Utf8PathBuf::from("/repo/missing.rs"));
        assert!(stub.removed.load(std::sync::atomic::Ordering::Relaxed));
        assert!(stub.data.read().content.is_empty());
    }

    #[test]
    fn find_git_root_discovers_a_linked_worktree() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root =
            camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).expect("utf8 temp dir");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(&root)
                .args(args)
                .output()
                .expect("run git");
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "-q", "-b", "main", "main"]);
        git(&["-C", "main", "config", "user.name", "test"]);
        git(&["-C", "main", "config", "user.email", "t@example.com"]);
        git(&["-C", "main", "commit", "-q", "--allow-empty", "-m", "init"]);
        git(&[
            "-C", "main", "worktree", "add", "-q", "../wt", "-b", "topic",
        ]);

        let worktree = root.join("wt").into_std_path_buf();
        assert!(!worktree.join(".git").is_dir(), "worktree .git is a file");
        assert_eq!(find_git_root_from(worktree), root.join("wt"));
    }
}
