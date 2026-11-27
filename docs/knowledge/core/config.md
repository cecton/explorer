---
title: Configuration and tokens
summary: config.kdl and auth.kdl: the blocks, their defaults, and which duplicates fail startup.
refs: src/config.rs, Config, load_auth, parse
---

Config and tokens live in two files, both optional. `~/.config/explorer/config.kdl` is read by `Config::load` and parsed by a hand-written walk over the KDL document; `~/.local/share/explorer/auth.kdl` holds one `name "token"` node per endpoint (`config::load_auth`), and a `claude-code` endpoint needs no token at all. An auth file that cannot be read or parsed is logged and treated as empty, so a broken token file leaves no endpoint usable rather than failing startup.

Blocks: `theme` (a fallback only, since the session carries the chosen theme), `keybindings { action "key" }`, `alias name "command"`, `after-agents { grace-seconds N }` (default 5), and one `agent { … }` block with the endpoints, `summary-model`, `subagent-model`, `review-rounds`, `repeat-limit` and `parallel-copies` (default 4).

A second `agent` block, a second endpoint of one name, or a duplicate alias is an error that fails startup and names what is wrong, rather than silently keeping the last one. An endpoint name may not contain `/`, because `endpoint/model/effort` is how a model is named everywhere else. An endpoint's `kind` defaults to `openai`; only `opencode-go` and `openrouter` have a default `url`, and any other HTTP kind without one is an error. A `claude-code` endpoint's `models` default to opus, sonnet and haiku.
