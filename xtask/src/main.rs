use std::process::Command;
use xtask_watch::{anyhow::Result, clap};

#[derive(clap::Parser)]
enum Opt {
    Watch(xtask_watch::Watch),
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let opt: Opt = clap::Parser::parse();
    let mut run_command = Command::new("cargo");
    run_command.args(["run", "--release", "--package", "explorer"]);
    match opt {
        Opt::Watch(watch) => watch.run(run_command),
    }
}
