pub struct Args {
    pub log_file: Option<String>,
    pub log_level: tracing_core::LevelFilter,
    pub theme: Option<String>,
    /// Scan the repository, print the report and exit, without starting the UI.
    pub scan: bool,
    /// `fmt-md [paths]`: formats the markdown and exits; `Some(empty)` means the working tree.
    pub fmt_md: Option<Vec<camino::Utf8PathBuf>>,
}

pub fn parse_args() -> Args {
    let mut args = pico_args::Arguments::from_env();
    let subcommand = args.subcommand().unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2);
    });
    let fmt_md = match subcommand.as_deref() {
        Some("fmt-md") => true,
        Some(other) => {
            eprintln!("unknown subcommand '{other}'");
            std::process::exit(2);
        }
        None => false,
    };
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
    let fmt_md = fmt_md.then(|| {
        args.finish()
            .into_iter()
            // A `--` ends the options; any other option is refused, not read as a path.
            .filter(|path| path.to_str() != Some("--"))
            .map(|path| {
                if path.to_str().is_some_and(|text| text.starts_with('-')) {
                    eprintln!("unknown option {}", path.display());
                    std::process::exit(2);
                }
                camino::Utf8PathBuf::from_path_buf(path.into()).unwrap_or_else(|path| {
                    eprintln!("{} is not valid UTF-8", path.display());
                    std::process::exit(2)
                })
            })
            .collect()
    });
    Args {
        log_file,
        log_level,
        theme,
        scan,
        fmt_md,
    }
}
