use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use jwalk::WalkDir;
use std::ffi::OsString;
use std::sync::Arc;

mod cli;
mod config;
mod loader;
mod lsp;
mod tui;
mod watcher;

use loader::{LoadedFile, find_git_root};

#[tokio::main]
async fn main() {
    let args = cli::parse_args();

    if let Some(path) = &args.log_file {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("failed to open log file");

        tracing_subscriber::fmt()
            .with_max_level(args.log_level)
            .with_writer(file)
            .with_ansi(false)
            .compact()
            .init();
    }

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
            LoadedFile::load(entry.path())
        })
        .collect();

    let mut files = files;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let files = Arc::new(ArcSwap::from_pointee(files));

    let config = match config::Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    let theme_name = args
        .theme
        .or(config.and_then(|c| c.theme))
        .unwrap_or_else(|| {
            tui::HelixTheme::theme_names()
                .next()
                .unwrap_or("catppuccin_mocha")
                .to_string()
        });

    let theme = if let Some(t) = tui::HelixTheme::from_name(&theme_name) {
        t
    } else {
        eprintln!("unknown theme '{theme_name}', using default");
        tui::HelixTheme::default()
    };

    let initial_state = tui::build_state(Arc::clone(&files), root.clone(), theme);

    tui::run(initial_state, files, root)
        .await
        .expect("TUI error");
}
