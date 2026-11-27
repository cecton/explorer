---
title: Trusting a repository before an agent runs
summary: The gate on the agent: no turn runs in a repository until the user trusts it, how that is saved, and what refuses a launch.
refs: src/tui/panes/discussion/launch.rs, Refusal, src/tui/session.rs, src/tui/panes/conversation_tree/mod.rs, src/tui/command_line.rs
---

No turn runs in a repository the user has not allowed. `AppState::trusted` starts false, is saved in the repository's session, and the `trust` and `untrust` command-line commands are the only ways to change it. Until it is set, a launch is refused with `Refusal::Untrusted`, whose message points at the scan: the report is what the user reads before deciding that this repository's files — including whatever instructions they carry — may be handed to an agent with a shell.

The gate is checked in `start_turn` (`src/tui/panes/discussion/launch.rs`), so every kind of turn passes it: a prompt, a replay, `/commit`, `/continue`, `/update`. The Conversation Tree window says `(untrusted)` in its title while the gate is closed, so the state is visible without trying to send anything.
