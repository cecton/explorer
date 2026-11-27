---
title: The user's prompt fragments
summary: The user's own prompt files: where they live, which mode they apply to, and how the `enabled` flag is read.
refs: src/agent/fragments.rs, block
---

`src/agent/fragments.rs` is the user's own instructions for a turn's system prompt: the `.md` files of `<config_dir>/prompts/` apply to both modes, and those of `<config_dir>/prompts/<mode>/` to one. File names must be lowercase letters, digits and dashes. Each file may start with a `---` block whose `enabled` must be exactly `true` or `false` — a typo must not keep a fragment the user meant to switch off; a missing `enabled` means true.

The fragments are read on every turn and appended after the project's instructions, so the user's rules win on a conflict: the shared ones first, then the mode's, each sorted by name, all under one `## User instructions` heading. They apply only to turns that carry a system prompt. They are read once per run rather than once per process, so that a sub-agent cannot get a different set of rules than its parent if the config is edited in between.

A file that cannot be read as a fragment is logged and skipped rather than failing the turn.
