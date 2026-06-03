mod update_themes;

use std::process::Command;
use xtask_watch::{anyhow::Result, clap};

#[derive(clap::Parser)]
enum Opt {
    Start(Start),
    Watch(xtask_watch::Watch),
    UpdateThemes,
}

#[derive(clap::Args)]
struct Start {
    #[command(flatten)]
    watch: xtask_watch::Watch,

    /// Arguments to forward to the `explorer` binary.
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let opt: Opt = clap::Parser::parse();
    match opt {
        Opt::Start(Start { watch, args }) => {
            if watch.shell_commands.is_empty() && watch.cargo_commands.is_empty() {
                watch.run({
                    let mut run_command = Command::new("cargo");
                    run_command.args([
                        "run",
                        "--profile",
                        "release-with-debug",
                        "--package",
                        "explorer",
                        "--",
                    ]);
                    run_command.args(args);
                    run_command
                })
            } else {
                watch.run([])
            }
        }
        Opt::Watch(watch) => {
            if watch.shell_commands.is_empty() && watch.cargo_commands.is_empty() {
                watch.run({
                    let mut run_command = Command::new("cargo");
                    run_command.arg("check");
                    run_command
                })
            } else {
                watch.run([])
            }
        }
        Opt::UpdateThemes => update_themes::run(),
    }
}
