use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use jwalk::WalkDir;
use r3bl_tui::log::{TracingConfig, WriterConfig, try_initialize_logging_global};
use std::ffi::OsString;
use std::sync::Arc;

mod cli;
mod loader;
mod lsp;
mod tui;
mod watcher;

use loader::{LoadedFile, find_git_root};

#[tokio::main]
async fn main() {
    let args = cli::parse_args();

    if let Some(ref path) = args.log_file {
        let config = TracingConfig {
            level_filter: args.log_level,
            writer_config: WriterConfig::File(path.clone()),
        };
        _ = try_initialize_logging_global(config);
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

    let files = Arc::new(ArcSwap::from_pointee(files));

    let initial_state = tui::build_state(Arc::clone(&files), root.clone());

    tui::run(initial_state, files, root)
        .await
        .expect("TUI error");
}
