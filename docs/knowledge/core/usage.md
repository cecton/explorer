---
title: Subscription and key usage
summary: What is remembered about subscription spend and OpenRouter key credit, where it is stored, and what polls it.
refs: src/usage.rs, Usage, fetch, polls_the_key, src/tui/app/status_bar.rs, poll_openrouter_usage, src/agent/mod.rs, report_usage
---

`src/usage.rs` remembers how much of each subscription is spent, in `<data_dir>/usage.json`, keyed by endpoint name: a subscription is the same in every repository, so this file is not per repository. A `Usage` holds a list of `UsageWindow` (a `Span` of five hours, a week or a month, a percentage and when it resets) and, for an OpenRouter key, a `KeyMoney`: what the key spent, the limit it enforces, what is left of it, how often it resets, and the account's `Credit` when the key has no limit. Money is kept in integral micro-dollars, the finest unit the API reports.

`fetch` asks each provider's own path — `usage` for OpenCode Go, `usages` for Kimi, `users/me/plan/usage-limits` for Cline, `key` and `credits` for OpenRouter — and a plain OpenAI endpoint reports nothing. A `claude-code` endpoint reports its windows itself, from what the CLI streams, and nothing is fetched for it.

Polling happens on the 60 s `UsagePollDue` signal (`status_bar::poll_openrouter_usage`, which goes through `report_usage`), for providers where `polls_the_key` is true, and a turn also reports usage when it ends. A window whose reset time has passed reads as 0 % with no reset. The status bar colors usage as a warning from 75 % and an error from 90 %.
