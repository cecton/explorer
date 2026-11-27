---
title: `after-agents`
summary: The one-shot command that runs when the agents are done: its three states, the countdown that cancels it, and how it is replaced or killed.
refs: src/tui/after_agents.rs, AfterAgents, cancel_on_input, Group
---

`src/tui/after_agents.rs` is the command that runs once the agents are done: `after-agents <command>` keeps one for later, and when a turn ends and nothing runs or is queued, it runs in the background, with no terminal and no focus change, so it cannot ask for input.

It has three states: pending, counting down (a deadline of `grace-seconds`, 5 by default, during which a key press or paste is consumed and cancels it, and a mouse event only cancels), and running. A turn starting or being queued during the countdown reverts the command to pending, so it is armed again when the queue next empties. The command is one-shot and kept in memory only: a new `after-agents` replaces the pending one, a bare `after-agents` clears it, and a running command is killed when it is replaced or cleared, or when Explorer exits.

The command runs in its own process group through `sh -c`, and the group guard kills the whole group on drop, so a replaced command leaves no children behind. `%` placeholders are filled in when Enter is pressed, as in `!`, and an alias stands for a command with what is typed after it appended.
