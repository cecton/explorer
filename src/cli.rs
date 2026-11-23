pub struct Args {
    pub log_file: Option<String>,
    pub log_level: tracing_core::LevelFilter,
    pub theme: Option<String>,
    /// Scan the repository, print the report and exit, without starting the UI.
    pub scan: bool,
    /// `fmt-md [paths]`: formats the markdown and exits; `Some(empty)` means the working tree.
    pub fmt_md: Option<Vec<camino::Utf8PathBuf>>,
    /// `mcp-serve [root] [--own-tools-only] [--privileged-socket <path>]`: serves Explorer's
    /// own agent tools over stdio as an MCP server.
    pub mcp_serve: Option<McpServe>,
    /// `agent`: runs the own agent loop of the turn that started it (`src/agent/child.rs`).
    pub agent: bool,
}

/// What an `mcp-serve` subcommand was asked for.
pub struct McpServe {
    /// The repository the tools act on; `None` means the one above the working directory.
    pub root: Option<camino::Utf8PathBuf>,
    /// Serve only Explorer's own tools, without starting the repository's MCP servers:
    /// the client already has them from its own configuration.
    pub own_tools_only: bool,
    /// The turn's socket: `conversation` is asked of that turn, not read from disk.
    pub privileged_socket: Option<camino::Utf8PathBuf>,
}

pub fn parse_args() -> Args {
    let mut args = pico_args::Arguments::from_env();
    let subcommand = args.subcommand().unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2);
    });
    let (fmt_md, serving) = match subcommand.as_deref() {
        Some("fmt-md") => (true, false),
        Some("mcp-serve") => (false, true),
        Some("agent") => (false, false),
        Some(other) => {
            eprintln!("unknown subcommand '{other}'");
            std::process::exit(2);
        }
        None => (false, false),
    };
    let agent = subcommand.as_deref() == Some("agent");
    let log_file: Option<String> = args
        .opt_value_from_str("--log-file")
        .expect("invalid --log-file");
    let log_level: Option<String> = args
        .opt_value_from_str("--log-level")
        .expect("invalid --log-level");
    let log_level = match log_level.as_deref() {
        Some("error") => tracing_core::LevelFilter::ERROR,
        Some("warn") => tracing_core::LevelFilter::WARN,
        Some("info") => tracing_core::LevelFilter::INFO,
        Some("debug") | None => tracing_core::LevelFilter::DEBUG,
        Some("trace") => tracing_core::LevelFilter::TRACE,
        Some(other) => {
            eprintln!("unknown log level '{other}', defaulting to debug");
            tracing_core::LevelFilter::DEBUG
        }
    };
    let theme: Option<String> = args.opt_value_from_str("--theme").expect("invalid --theme");
    let scan = args.contains("--scan");
    // Both subcommands take their own options after their name, so they are read from
    // what is left once the global ones are gone. Only `mcp-serve` knows this one.
    let own_tools_only = serving && args.contains("--own-tools-only");
    let privileged_socket: Option<camino::Utf8PathBuf> = match serving {
        true => args
            .opt_value_from_str("--privileged-socket")
            .expect("invalid --privileged-socket"),
        false => None,
    };
    let rest: Vec<std::ffi::OsString> = args.finish();
    let utf8 = |arg: std::ffi::OsString| {
        camino::Utf8PathBuf::from_path_buf(arg.into()).unwrap_or_else(|arg| {
            eprintln!("{} is not valid UTF-8", arg.display());
            std::process::exit(2)
        })
    };
    let fmt_md = fmt_md.then(|| {
        rest.iter()
            // A `--` ends the options; any other option is refused, not read as a path.
            .filter(|path| path.to_str() != Some("--"))
            .map(|path| {
                if path.to_str().is_some_and(|text| text.starts_with('-')) {
                    eprintln!("unknown option {}", path.display());
                    std::process::exit(2);
                }
                utf8(path.clone())
            })
            .collect()
    });
    let mcp_serve = serving.then(|| McpServe {
        root: rest
            .into_iter()
            // Not filtered on UTF-8: `utf8` must refuse a non-UTF-8 path, not skip it.
            .find(|arg| !arg.as_encoded_bytes().starts_with(b"-"))
            .map(utf8),
        own_tools_only,
        privileged_socket,
    });
    Args {
        log_file,
        log_level,
        theme,
        scan,
        fmt_md,
        mcp_serve,
        agent,
    }
}
