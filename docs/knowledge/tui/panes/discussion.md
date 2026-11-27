---
title: The Discussion window
summary: A node open as a window: the texts it lays out and their cache key, what Enter and Esc mean, the queue, and why a launch is refused.
refs: src/tui/panes/discussion/mod.rs, src/tui/panes/discussion/document.rs, DocCache, src/tui/panes/discussion/launch.rs, TurnKind, Refusal, src/tui/panes/discussion/queue.rs, src/tui/panes/discussion/status.rs, src/tui/panes/discussion/selection.rs
---

`src/tui/panes/discussion/` is one node of the tree, open as a window: the prompt, the thinking, the work log of the tool calls and the reply, then an input for the next prompt.

`document.rs` lays the texts out, each on its own, so that a running turn re-wraps only the text that changed; the drag offsets are byte offsets into each text, not screen cells. `DocCache` remembers not only the width and the lengths of the four texts but the **theme name**: every laid-out span carries the colour it was rendered with, so a new theme is a different document, and keying only on width and lengths left an open window painting the old theme until its text moved — which is exactly what a live theme preview would show. `launch.rs` decides what sending means: a `TurnKind` (`Prompt` in a mode, `NoSystemPrompt` for a custom command without the mode's prompt, `Compact`, `Continue`, `Update`), a replay of an existing node, and whether a node resumes rather than starting a child. `queue.rs` holds the prompts that had to wait, in order. `status.rs` is the status line: model, tokens, elapsed, how a finished turn ended and the hint for resuming it. `selection.rs` maps a column to a source offset for selection and highlight drags.

Enter sends the draft; an empty one sends nothing. A plain message sent to a node whose status is `Failed` or `Cancelled` (never a compact node) resumes that turn instead of starting a child, because the reasoning is on disk and is what the message builds on — `/continue` does the same with nothing to add. A slash command keeps its own meaning. Esc takes back a queued prompt, or cancels the running turn (a first Esc only takes back a message the turn has not read yet).

A launch can be refused, and `Refusal` says why: the repository is not trusted yet, the working tree is dirty, the parent node is still running or is not complete enough to build on, another turn holds the repository, or the endpoint has no token. A refused window is left exactly as it was.
