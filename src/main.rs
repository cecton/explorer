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
    let result = paths
        .iter()
        .map(|path| {
            // Joined onto the cwd, an empty path becomes "cwd/", which passes format_paths.
            if path.as_str().is_empty() {
                return Err("a path is empty".to_string());
            }
            let absolute = std::env::current_dir()
                .map_err(|e| e.to_string())
                .map(|cwd| cwd.join(path))?;
            camino::Utf8PathBuf::from_path_buf(absolute)
                .map_err(|p| format!("{} is not valid UTF-8", p.display()))
        })
        .collect::<Result<Vec<_>, String>>()
        .and_then(|absolutes| {
            let absolutes: Vec<&str> = absolutes.iter().map(|path| path.as_str()).collect();
            markdown_format::format_paths(root, &absolutes)
        });
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

/// The repository root above the current directory, for the subcommand that has to report
/// a missing repository rather than panic on it.
fn resolve_git_root() -> Option<camino::Utf8PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let cwd = camino::Utf8PathBuf::from_path_buf(cwd).ok()?;
    loader::find_git_root_from(&cwd)
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
        agent::driver::set_log(path, args.log_level);
    }

    // Its whole input is the start line its parent writes: no repository, no configuration.
    if args.agent {
        std::process::exit(agent::child::main().await);
    }

    // Before the root lookup below, which panics outside a repository: a server started
    // from one must say so instead of dying on a client that cannot see why.
    if let Some(serve) = &args.mcp_serve {
        let Some(root) = serve.root.clone().or_else(resolve_git_root) else {
            eprintln!("no git repository found (no .git in any parent)");
            std::process::exit(2);
        };
        // Absolute, so the tools comparing a path against the root do not read `..` away.
        let root = match root.canonicalize_utf8() {
            Ok(root) => root,
            Err(error) => {
                eprintln!("{root}: {error}");
                std::process::exit(2);
            }
        };
        // The session is read here, not in the server: a client that was given the
        // repository's servers already starts them itself.
        let servers = match serve.own_tools_only {
            true => Vec::new(),
            false => tui::session::session_mcp_servers(&root),
        };
        std::process::exit(
            agent::serve::serve(root, servers, serve.privileged_socket.clone()).await,
        );
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
        tui::theme::HelixTheme::bundled_default()
    };

    let mut keymap = tui::keymap::Keymap::default();
    if let Err(e) = keymap.apply_overrides(&config.keybindings) {
        eprintln!("invalid keybindings in config: {e}");
        std::process::exit(1);
    }

    let files = Arc::new(arc_swap::ArcSwap::from_pointee(loader::load_repo(&root)));

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
