use camino::{Utf8Path, Utf8PathBuf};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use crate::lsp;

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

    /// Returns the byte range `(start, end)` of the "word" under `cursor_byte`.
    ///
    /// The definition of "word" depends on what the cursor is sitting on:
    ///
    /// 1. **Whitespace** — the entire contiguous run of whitespace characters
    ///    (spaces, tabs, newlines, etc.).
    /// 2. **URL** — if the non-whitespace span parses as a `url::Url`, the full
    ///    URL is returned. URL boundaries are whitespace and the characters
    ///    `"`, `)`, `]`, `}`.
    /// 3. **Alphanumeric word** — otherwise, a run of `[A-Za-z0-9_]` characters.
    ///
    /// # Panics
    ///
    /// Panics in debug builds if `cursor_byte` is not a valid byte index into
    /// `self.content` (i.e. `cursor_byte > self.content.len()`). Callers are
    /// expected to keep the cursor aligned to character boundaries.
    ///
    /// # Edge cases
    ///
    /// * Empty content → `(0, 0)`.
    /// * Cursor on punctuation that is not part of a URL → returns the single
    ///   character at that position.
    pub fn word_bounds(&self, cursor_byte: usize) -> (usize, usize) {
        let content = &self.content;
        if content.is_empty() {
            return (0, 0);
        }
        debug_assert!(
            cursor_byte <= content.len(),
            "cursor_byte {cursor_byte} out of bounds for content of length {}",
            content.len()
        );
        let cursor_byte = cursor_byte.min(content.len() - 1);
        let c = content[cursor_byte..].chars().next().unwrap_or('\0');

        macro_rules! scan_backward {
            ($i:ident, $char:ident => $take_while:expr) => {{
                content[..(cursor_byte + c.len_utf8())]
                    .char_indices()
                    .rev()
                    .take_while(|&($i, $char)| $take_while)
                    .last()
                    .map(|(i, _)| i)
                    .unwrap_or(cursor_byte)
            }};
        }

        macro_rules! scan_forward {
            ($i:ident, $char:ident => $take_while:expr) => {{
                content[cursor_byte..]
                    .char_indices()
                    .take_while(|&($i, $char)| $take_while)
                    .last()
                    .map(|(i, c)| cursor_byte + i + c.len_utf8())
                    .unwrap_or(cursor_byte + c.len_utf8())
            }};
        }

        if c.is_whitespace() {
            let start = scan_backward!(_i, c => c.is_whitespace());
            let end = scan_forward!(_i, c => c.is_whitespace());
            (start, end)
        } else {
            let is_url_boundary = |c: char| c.is_whitespace() || matches!(c, '"' | ')' | ']' | '}');
            let end = scan_forward!(_i, c => !is_url_boundary(c));
            let mut url = None;
            let _start = scan_backward!(i, c => {
                let res = !is_url_boundary(c);
                if res && content[i..end].contains("://") && url::Url::parse(&content[i..end]).is_ok() {
                    url = Some((i, end));
                }
                res
            });
            if let Some((start, end)) = url {
                (start, end)
            } else {
                let is_word = |c: char| c.is_alphanumeric() || c == '_';
                let start = scan_backward!(_i, c => is_word(c));
                let end = scan_forward!(_i, c => is_word(c));
                (start, end)
            }
        }
    }

    pub fn line_bounds(&self, line_idx: usize) -> (usize, usize) {
        let line_byte_start = self.line_starts[line_idx];
        let line = self.line(line_idx);
        (line_byte_start, line_byte_start + line.len())
    }
}

pub struct LoadedFile {
    pub path: Utf8PathBuf,
    pub data: Mutex<FileData>,
    pub colored_lines: Mutex<Vec<lsp::ColoredLine>>,
    pub removed: AtomicBool,
    pub needs_full_load: AtomicBool,
}

impl LoadedFile {
    pub fn load(path: PathBuf) -> Option<Arc<Self>> {
        let path = Utf8PathBuf::from_path_buf(path).ok()?;
        let content = fs::read_to_string(&path).ok()?;
        let line_starts = compute_line_starts(&content);
        Some(Arc::new(Self {
            path,
            data: Mutex::new(FileData {
                content,
                line_starts,
            }),
            colored_lines: Mutex::new(vec![]),
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
        let mut data = self
            .data
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        data.content = content;
        data.line_starts = line_starts;
        drop(data);
        *self
            .colored_lines
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = vec![];
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

pub fn path_for_file_key(files: &[Arc<LoadedFile>], key: FileKey) -> Option<&Utf8Path> {
    files.get(key.0).map(|f| f.path.as_ref())
}

pub fn file_key_for_path(files: &[Arc<LoadedFile>], path: &Utf8Path) -> Option<FileKey> {
    files.iter().position(|f| f.path == path).map(FileKey)
}

impl LoadedFile {
    pub fn stub(path: Utf8PathBuf) -> Arc<Self> {
        Arc::new(Self {
            path,
            data: Mutex::new(FileData {
                content: String::new(),
                line_starts: vec![0],
            }),
            colored_lines: Mutex::new(vec![]),
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
    fn word_bounds_whitespace() {
        let data = fd("foo    bar");
        let (start, end) = data.word_bounds(3);
        assert_eq!(data.extract_text(start, end).as_deref(), Some("    "));

        let data = fd("foo  \n  bar");
        let (start, end) = data.word_bounds(3);
        assert_eq!(data.extract_text(start, end).as_deref(), Some("  \n  "));

        let data = fd("foo\t\tbar");
        let (start, end) = data.word_bounds(3);
        assert_eq!(data.extract_text(start, end).as_deref(), Some("\t\t"));
    }

    #[test]
    fn word_bounds_url() {
        let data = fd("url=file://url?foo#bar baz");
        let (start, end) = data.word_bounds(6);
        assert_eq!(
            data.extract_text(start, end).as_deref(),
            Some("file://url?foo#bar")
        );

        let data = fd("format!(\"file://root\").parse().unwrap()");
        let (start, end) = data.word_bounds(15);
        assert_eq!(
            data.extract_text(start, end).as_deref(),
            Some("file://root")
        );

        let data = fd("see also https://example.com/path?q=1");
        let (start, end) = data.word_bounds(10);
        assert_eq!(
            data.extract_text(start, end).as_deref(),
            Some("https://example.com/path?q=1")
        );

        let data = fd("[https://a.b]");
        let (start, end) = data.word_bounds(5);
        assert_eq!(
            data.extract_text(start, end).as_deref(),
            Some("https://a.b")
        );

        let data = fd("{https://a.b}");
        let (start, end) = data.word_bounds(5);
        assert_eq!(
            data.extract_text(start, end).as_deref(),
            Some("https://a.b")
        );
    }

    #[test]
    fn word_bounds_alphanumeric() {
        let data = fd("fn foo_bar() {}");
        let (start, end) = data.word_bounds(6);
        assert_eq!(data.extract_text(start, end).as_deref(), Some("foo_bar"));

        let data = fd("123abc");
        let (start, end) = data.word_bounds(3);
        assert_eq!(data.extract_text(start, end).as_deref(), Some("123abc"));

        let data = fd("use serde_json::Value;");
        let (start, end) = data.word_bounds(12);
        assert_eq!(data.extract_text(start, end).as_deref(), Some("serde_json"));
    }

    #[test]
    fn word_bounds_multibyte() {
        let data = fd("héllo");
        let (start, end) = data.word_bounds(3);
        assert_eq!(data.extract_text(start, end).as_deref(), Some("héllo"));
    }

    #[test]
    fn word_bounds_empty() {
        let data = fd("");
        let (start, end) = data.word_bounds(0);
        assert_eq!(start, 0);
        assert_eq!(end, 0);
    }

    #[test]
    fn word_bounds_trailing_punctuation() {
        let data = fd("foo.bar");
        let (start, end) = data.word_bounds(3);
        assert_eq!(data.extract_text(start, end).as_deref(), Some("."));
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
        let stub = LoadedFile::stub(Utf8PathBuf::from("/repo/missing.rs"));
        assert!(stub.removed.load(std::sync::atomic::Ordering::Relaxed));
        assert!(stub.data.lock().unwrap().content.is_empty());
    }
}
