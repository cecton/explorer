---
title: The keymap
summary: Actions and bindings: the two tables, the config override by action slug, and the two actions that cannot be rebound.
refs: src/tui/keymap.rs, Action, Keymap, apply_overrides
---

`src/tui/keymap.rs` binds keys to `Action`s — what a key does, not which key does it — in two tables: the leader table and the global one. `config.kdl`'s `keybindings` block rebinds by the action's slug (`open-git-log`, `send-to-back`, …), so the dispatch in `src/tui/app/input.rs` resolves a key to an action and no shortcut is hardcoded at a call site. Rebinding replaces that action's key in every table the action appears in (most actions are only in the leader table), and the special slug `leader` sets the leader key itself.

The leader key defaults to Alt+backtick. Pressing it again while a terminal is focused grabs that terminal, so `GrabTerminal` and `DismissLeader` are deliberately not rebindable and have no slug. An unknown slug or an unparsable key fails startup rather than being ignored.

The tables are `Vec<(KeyPress, Action)>` scanned linearly, because `KeyPress` does not implement `Hash` and the tables have a handful of entries. `hint` renders a key the way the status bar spells it, which is how a rebound key appears there too.
