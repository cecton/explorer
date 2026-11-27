---
title: Explorer: a TUI workspace for one repository
summary: What the program is: a read-first terminal workspace around one git repository, with windows, terminals, git history and a tree of agent conversations.
refs: README.md, AGENTS.md, Cargo.toml, src/main.rs
---

Explorer is a terminal application for one git repository, built for a workflow where an agent writes the code and the developer reads it. It has no editor: a file opens read-only, and editing is delegated to `$EDITOR` in a terminal window.

Everything is a window — a file, a terminal, a git log, a commit diff, a report, a conversation — tiled by Explorer itself, and the whole repository is read into memory at startup and kept current by a watcher, so opening, searching and jumping never wait for the disk.

It is a personal tool in early development: it breaks freely between versions, keeps no compatibility, and the agent runs its tools without asking, with git as the guard. `README.md` describes the features for a user; `AGENTS.md` is the map for working on the code.
