#![warn(clippy::wildcard_imports)]
#![warn(clippy::clone_on_ref_ptr)]

use std::sync::Arc;

mod agent;
mod cli;
mod config;
mod conversation;
mod endpoint;
mod git;
mod highlight;
mod loader;
mod lsp;
mod markdown_format;
mod model_spec;
mod scan;
mod signal;
mod tui;
mod usage;
mod util;
mod watcher;

/// A reader that closes the pipe early (`| head`) got what it asked for: not an error.
fn print_ignoring_broken_pipe(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    match std::io::stdout().lock().write_all(text.as_bytes()) {
        Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => Err(error),
        _ => Ok(()),
    }
}

/// The `fmt-md` subcommand: prints the paths it rewrote and returns the exit code.
fn fmt_md(root: &camino::Utf8Path, paths: &[camino::Utf8PathBuf]) -> i32 {
    let relatives = if paths.is_empty() {
        git::working_tree_changes(root)
    } else {
        paths
            .iter()
            .map(|path| {
                let absolute = std::env::current_dir()
                    .map_err(|e| e.to_string())
                    .map(|cwd| cwd.join(path))?;
                let absolute = camino::Utf8PathBuf::from_path_buf(absolute)
                    .map_err(|p| format!("{} is not valid UTF-8", p.display()))?;
                // Lexically resolves `..`, so a path leaving the repository is refused.
                let resolved = util::fs::resolve_in(root, absolute.as_str())
                    .map_err(|_| format!("{path} is outside the repository"))?;
                resolved
                    .strip_prefix(root)
                    .map(|relative| relative.to_path_buf())
                    .map_err(|_| format!("{path} is outside the repository"))
            })
            .collect()
    };
    let result = relatives
        .and_then(|relatives| markdown_format::expand_directories(root, relatives))
        .and_then(|relatives| markdown_format::format_files(root, &relatives));
    match result {
        Ok(changed) => {
            let lines: String = changed.iter().map(|path| format!("{path}\n")).collect();
            match print_ignoring_broken_pipe(&lines) {
                Ok(()) => 0,
                Err(error) => {
                    eprintln!("failed to print the changed paths: {error}");
                    1
                }
            }
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

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

    let root = camino::Utf8PathBuf::from_path_buf(loader::find_git_root())
        .expect("repository root path is not valid UTF-8");

    if args.scan {
        let report = scan::scan(&root).to_markdown();
        if let Err(error) = print_ignoring_broken_pipe(&report) {
            eprintln!("failed to print the scan report: {error}");
            std::process::exit(1);
        }
        return;
    }

    if let Some(paths) = &args.fmt_md {
        std::process::exit(fmt_md(&root, paths));
    }

    let files = Arc::new(arc_swap::ArcSwap::from_pointee(loader::load_repo(&root)));

    let config = match config::Config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    let theme_name = args.theme.clone().or(config.theme).unwrap_or_else(|| {
        tui::theme::HelixTheme::theme_names()
            .next()
            .unwrap_or("catppuccin_mocha")
            .to_string()
    });

    let theme = if let Some(t) = tui::theme::HelixTheme::from_name(&theme_name) {
        t
    } else {
        eprintln!("unknown theme '{theme_name}', using default");
        tui::theme::HelixTheme::default()
    };

    let mut keymap = tui::keymap::Keymap::default();
    if let Err(e) = keymap.apply_overrides(&config.keybindings) {
        eprintln!("invalid keybindings in config: {e}");
        std::process::exit(1);
    }

    let mut initial_state =
        tui::state::AppState::new(Arc::clone(&files), root.clone(), theme.clone(), keymap);
    initial_state.after_agents_grace =
        std::time::Duration::from_secs(config.after_agents.grace_seconds);
    initial_state.config_aliases = config.aliases;
    initial_state.agent = Arc::new(agent::AgentConfig::resolve(
        config.agent,
        &config::load_auth(),
        agent::commands::load(),
        agent::subagent::load_roles(),
    ));
    initial_state.usage = usage::load();

    if let Some(session) = tui::session::load_session(&root) {
        session.apply(&mut initial_state);
        // Locations aren't persisted (they'd be stale) — re-resolve each restored
        // group against a running LSP worker using its saved origin identity, same
        // as the rebuild-on-edit path. Requests buffer until the worker is up.
        for (idx, group) in initial_state.symbol_highlights.iter().enumerate() {
            lsp::send_symbol_request(
                group.origin_file.0,
                group.origin_line,
                group.origin_char,
                group.origin_word.clone(),
                Some(idx),
            );
        }
    }

    if args.theme.is_some() {
        // An explicit --theme flag wins over whatever the session had saved.
        initial_state.theme = theme.clone();
        initial_state.saved_theme = theme;
    }

    crate::tui::app::run(initial_state, files, root)
        .await
        .expect("TUI error");
}
