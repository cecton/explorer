use camino::Utf8PathBuf;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use crate::lsp;

/// Stable index into the file list.
///
/// The underlying `Vec<LoadedFile>` is append-only: files are never removed or reordered,
/// only marked `removed = true`. This makes the raw `usize` permanently stable. The
/// newtype exists to prevent accidental confusion with other `usize` values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileKey(pub usize);

pub struct FileData {
    pub content: String,
    pub line_starts: Vec<usize>,
}

pub struct LoadedFile {
    pub path: Utf8PathBuf,
    pub data: Mutex<FileData>,
    pub colored_lines: Mutex<Vec<lsp::ColoredLine>>,
    pub removed: AtomicBool,
}

impl LoadedFile {
    pub fn load(path: PathBuf) -> Option<Self> {
        let path = Utf8PathBuf::from_path_buf(path).ok()?;
        let content = fs::read_to_string(&path).ok()?;
        let line_starts = compute_line_starts(&content);
        Some(Self {
            path,
            data: Mutex::new(FileData {
                content,
                line_starts,
            }),
            colored_lines: Mutex::new(vec![]),
            removed: AtomicBool::new(false),
        })
    }

    /// Re-reads the file from disk, updates content and line_starts, clears colored_lines.
    /// Returns false if the file could not be read.
    pub fn reload(&self) -> bool {
        let Ok(content) = fs::read_to_string(&self.path) else {
            return false;
        };
        let line_starts = compute_line_starts(&content);
        let mut data = self.data.lock().unwrap();
        data.content = content;
        data.line_starts = line_starts;
        drop(data);
        *self.colored_lines.lock().unwrap() = vec![];
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
    let mut dir = env::current_dir().expect("cannot get current directory");
    loop {
        if dir.join(".git").is_dir() {
            return dir;
        }
        if !dir.pop() {
            panic!("no git repository found (no .git directory in any parent)");
        }
    }
}
