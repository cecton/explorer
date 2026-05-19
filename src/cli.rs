use tracing_core::LevelFilter;

pub struct Args {
    pub log_file: Option<String>,
    pub log_level: LevelFilter,
    pub theme: Option<String>,
}

pub fn parse_args() -> Args {
    let mut args = pico_args::Arguments::from_env();
    let log_file: Option<String> = args
        .opt_value_from_str("--log-file")
        .expect("invalid --log-file");
    let log_level: Option<String> = args
        .opt_value_from_str("--log-level")
        .expect("invalid --log-level");
    let log_level = match log_level.as_deref() {
        Some("error") => LevelFilter::ERROR,
        Some("warn") => LevelFilter::WARN,
        Some("info") => LevelFilter::INFO,
        Some("debug") | None => LevelFilter::DEBUG,
        Some("trace") => LevelFilter::TRACE,
        Some(other) => {
            eprintln!("unknown log level '{other}', defaulting to debug");
            LevelFilter::DEBUG
        }
    };
    let theme: Option<String> = args.opt_value_from_str("--theme").expect("invalid --theme");
    Args {
        log_file,
        log_level,
        theme,
    }
}
