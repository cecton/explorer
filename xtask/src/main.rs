mod update_themes;

use std::process::Command;
use xtask_watch::{anyhow::Result, clap};

#[derive(clap::Parser)]
enum Opt {
    Watch(xtask_watch::Watch),
    UpdateThemes,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let opt: Opt = clap::Parser::parse();
    match opt {
        Opt::Watch(watch) => watch.run({
            let mut run_command = Command::new("cargo");
            run_command.args(["run", "--release", "--package", "explorer"]);
            run_command
        }),
        Opt::UpdateThemes => update_themes::run(),
    }
}
