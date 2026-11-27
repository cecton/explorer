---
title: Startup and subcommand dispatch
summary: The subcommands and the order in which `main` and `run` do their work, from argv to the event loop.
refs: src/main.rs, src/cli.rs, print_ignoring_broken_pipe, parse_args
---

`main` reads `argv[1]` and dispatches `sandbox-exec` and `sandbox-child` before the tokio runtime exists, because `unshare` into a user namespace refuses once a thread has been started. Everything else runs inside the multi-thread runtime, in `run`.

`run` parses `src/cli.rs` arguments, installs the log file when `--log-file` is given, and dispatches the subcommands in the order that lets each fail clearly: `agent` (the own agent loop in a child process) needs nothing, `mcp-serve` resolves the repository root before `find_git_root` would panic on one, `--scan` prints a report, `fmt-md` formats markdown. Only then does it load the config, spawn a thread that runs the sandbox check and sweeps stale agent workspaces, build the theme and keymap, load every file of the repository into an `ArcSwap`, build the `AppState`, apply the saved session and hand over to `tui::app::run`.

`print_ignoring_broken_pipe` is why `explorer --scan | head` is not an error.
