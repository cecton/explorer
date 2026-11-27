---
title: The command line
summary: The `:` line: its ten commands and the palette they feed, `!`, the `%` placeholders, and how config and project aliases relate.
refs: src/tui/command_line.rs, COMMANDS, expand_placeholders, run_command_line, set_alias
---

`src/tui/command_line.rs` is the `:` line over the status bar. Its commands are a table of ten — `after-agents`, `alias`, `all-features`, `default-features`, `features`, `highlight`, `mcp`, `scan`, `trust`, `untrust` — plus `!<command>` to open a terminal running one, and a bare alias name to run an alias. The table is also the palette: the line completes command names and shows each one's argument and description as a hint.

The `%` placeholders are expanded when Enter is pressed, and shell-quoted: `%` is the file in focus, `%dir` its directory, `%crate` and `%repo` the nearest ancestor directory holding a `Cargo.toml` or a `.git` (absolute paths), `%project` the name of the repository directory, and `%%` a literal `%`. `%d`, `%c` and `%r` are the short forms. Every placeholder except `%project` and `%%` needs a file preview in focus. A name only matches at a word end, so `%crate` is never read as `%c` followed by `rate`.

Aliases come in two kinds: the config's (`alias` lines in `config.kdl`) and the repository's own, set with `alias name <command>` and saved in its session. The repository's wins over the config's, so `set_alias` can shadow a config alias; it refuses only removing one, so the file, not the session, stays the source of a shared alias.

`trust` and `untrust` are the one gate on the agent: a repository is untrusted until the user allows turns in it, and the scan report is what they read before deciding.
