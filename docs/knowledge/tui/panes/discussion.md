---
title: The Discussion window
summary: A node open as a window: the texts it lays out, what Enter and Esc mean, when a message resumes a stopped turn, the queue and why a launch is refused.
refs: src/tui/panes/discussion/mod.rs, src/tui/panes/discussion/document.rs, src/tui/panes/discussion/launch.rs, src/tui/panes/discussion/queue.rs, src/tui/panes/discussion/status.rs, src/tui/panes/discussion/selection.rs, TurnCopyMade
---

A launch can be refused, and `Refusal` says why: the repository is not trusted yet, the working tree is dirty, the parent node is still running or is not complete enough to build on, another turn holds the repository, or the endpoint has no token. A refused window is left exactly as it was. A `Busy` refusal is the only one that queues: `queue.rs` starts the head of the queue again on `TurnFinished` and on `TurnCopyMade`, the signal a copy turn sends once its copy is made, so a prompt held only for a copy still being walked goes as soon as the walk ends.
