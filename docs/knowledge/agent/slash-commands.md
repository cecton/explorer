---
title: Slash commands
summary: Custom slash commands and the four built-in ones, and what `system-prompt: off` changes about how a turn closes.
refs: src/agent/commands.rs, Command, Expansion, expand, load
---

`src/agent/commands.rs` is the slash commands. A custom command is a markdown file in `<config_dir>/commands/`, named after the command, with an optional `---` block holding `system-prompt: off`, `argument` and `description`; typing `/name something` sends the file's trimmed prompt, a blank line, then `something` as typed. The two metadata fields are what the input hints at while the draft is still the command name, and the placeholder hint shows only when nothing follows the name; the built-ins have fixed hints.

`system-prompt: off` is the interesting one: the turn gets no mode prompt and no feedback note, and it closes like a commit turn instead of being reset, so a `/squash` that runs `git reset --soft` keeps its work. The flag is stored with the node, so replaying the node later behaves the same.

`Expansion` is what a typed line turned into, and four names are built in rather than read from a file: `compact`, `commit`, `continue` and `update`. An unknown name is left as typed, so a prompt that begins with a path is harmless. `read` refuses a file that would shadow a built-in name or has no prompt.
