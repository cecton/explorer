---
title: Endpoints, providers and model specs
summary: What an endpoint is, the five provider kinds plus the local `claude` CLI, and how `endpoint/model/effort` is parsed.
refs: src/endpoint.rs, src/model_spec.rs, Provider, EndpointKind, ModelSpec
---

An endpoint is one model server named in `config.kdl`. `Provider` (`src/endpoint.rs`) is what kind of server it is: `OpenAi` (a plain OpenAI-compatible server that reports no usage), `OpenCodeGo`, `KimiCode`, `Cline`, `OpenRouter`. The last four are OpenAI-compatible too, and each also answers a usage endpoint that `src/usage.rs` polls; `Provider::models_path` exists because Cline lists its models somewhere else than `/models`. A fifth kind names no provider at all: `claude-code`, which runs the local `claude` CLI.

`EndpointKind` is either `Http { url, token, provider }` or `ClaudeCode { models }`. `Endpoint::resolved` fills the token from `auth.kdl` and drops an `Http` endpoint whose token is missing, with a warning, rather than failing the whole config; a `claude-code` endpoint needs none.

`ModelSpec` (`src/model_spec.rs`) is `endpoint/model/effort`, where the model may itself contain `/`: the last segment is read as an effort only when it is `default`, `low`, `medium` or `high`, and stays part of the model otherwise; a spec with no effort segment parses as `default`. The effort is always written out by `Display`, and a default effort is hidden in the short label that discussion windows show.
