use camino::Utf8PathBuf;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::lsp;

pub struct LoadedFile {
    pub path: Utf8PathBuf,
    pub content: String,
    pub line_starts: Vec<usize>,
    pub colored_lines: Mutex<Vec<lsp::ColoredLine>>,
}

impl LoadedFile {
    pub fn load(path: PathBuf) -> Option<Self> {
        let path = Utf8PathBuf::from_path_buf(path).ok()?;
        let content = fs::read_to_string(&path).ok()?;
        let capacity = content.len() / 100 + 1;
        let mut line_starts = Vec::with_capacity(capacity);
        line_starts.push(0usize);
        for (i, &b) in content.as_bytes().iter().enumerate() {
            if b == b'\n' && i + 1 < content.len() {
                line_starts.push(i + 1);
            }
        }
        line_starts.shrink_to_fit();
        Some(Self {
            path,
            content,
            line_starts,
            colored_lines: Mutex::new(vec![]),
        })
    }
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
