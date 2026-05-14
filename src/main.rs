use camino::Utf8PathBuf;
use jwalk::WalkDir;
use simplelog::{Config, LevelFilter, WriteLogger};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

mod lsp;
mod tui;

pub struct LoadedFile {
    pub path: Utf8PathBuf,
    pub content: String,
    pub line_starts: Vec<usize>,
    pub colored_lines: Mutex<Option<Vec<lsp::ColoredLine>>>,
}

fn find_git_root() -> PathBuf {
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

fn load_file(path: PathBuf) -> Option<LoadedFile> {
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
    Some(LoadedFile {
        path,
        content,
        line_starts,
        colored_lines: Mutex::new(None),
    })
}

#[tokio::main]
async fn main() {
    let log_file = fs::File::create("/tmp/explorer.log").expect("cannot create log file");
    WriteLogger::init(LevelFilter::Debug, Config::default(), log_file)
        .expect("cannot initialize logger");

    let root = Utf8PathBuf::from_path_buf(find_git_root())
        .expect("repository root path is not valid UTF-8");

    let skip: [OsString; 2] = [OsString::from("target"), OsString::from(".git")];

    let files: Vec<LoadedFile> = WalkDir::new(&root)
        .process_read_dir(move |_, _, _, children| {
            children.retain(|entry| {
                entry
                    .as_ref()
                    .map_or(true, |e| !skip.contains(&e.file_name))
            });
        })
        .into_iter()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if !entry.file_type().is_file() {
                return None;
            }
            load_file(entry.path())
        })
        .collect();

    let files = Arc::new(files);
    let warmup_ms: Arc<Mutex<Option<u128>>> = Arc::new(Mutex::new(None));
    let (lsp_tx, lsp_rx) = tokio::sync::mpsc::channel(32);

    let initial_state = tui::build_state(
        Arc::clone(&files),
        root.clone(),
        lsp_tx,
        Arc::clone(&warmup_ms),
    );
    let notify_tx = Arc::clone(&initial_state.notify_tx);

    let lsp_files = Arc::clone(&files);
    tokio::spawn(async move {
        lsp::run(root.clone(), lsp_files, lsp_rx, notify_tx, warmup_ms).await;
    });

    tui::run(initial_state).await.expect("TUI error");
}
