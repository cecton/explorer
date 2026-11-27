---
title: The rust-analyzer thread and its warmup
summary: The rust-analyzer thread: the warmup, the restart that never loses a feature selection, and the two rules that stop a bad request from looping.
refs: src/lsp/worker.rs, src/lsp/mod.rs, LspInput, CargoFeatureSelection, health_check, cargo_features, set_cargo_features, accept_buffered, held_requests
---

`src/lsp/worker.rs` is the rust-analyzer thread: it spawns the server, drains the requests it was given, waits until the server is ready and then answers what comes back. It is driven by r3bl's RRT machinery with an interrupt that does nothing, because the worker polls its own sources.

Warmup is the part that makes opening a file instant rather than merely fast: once the server is idle and no user request is pending, the worker sends one file's semantic-token request at a time, in walk order. A large file is asked for its first screenful only, an empty answer is retried a few times, and a warmup with no progress for a minute clears the queue rather than waiting forever.

A change of cargo features needs a restart, since the server does not reload them, and the selection survives a restart it did not cause: `cargo_features` keeps the last selection in a process-wide store written when the command is issued, not when a spawn succeeds, so a selection rust-analyzer refuses to start under is kept and retried by the next restart instead of silently reverting to the config. The worker starts the new server before killing the old one, so a failed handshake keeps the colors that work.

Requests sent while no worker is running go into a bounded buffer the worker drains itself when it starts, so nothing on the main thread retries. Two rules keep a bad request from taking a run down: a buffered request whose send fails is dropped rather than requeued, which ends a loop where every new worker retried it; and a worker on its way out hands back only its _held_ requests, which were never sent — a query already in flight is the one that may have killed the server, so retrying it would take the next one down too.
