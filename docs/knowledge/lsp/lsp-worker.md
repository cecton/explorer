---
title: The rust-analyzer thread and its warmup
summary: The rust-analyzer thread: spawning, the warmup that colors files ahead of time, restarts, and the buffer for early requests.
refs: src/lsp/worker.rs, src/lsp/mod.rs, LspInput, CargoFeatureSelection, health_check
---

`src/lsp/worker.rs` is the rust-analyzer thread: it spawns the server, drains the requests it was given, waits until the server is ready and then answers what comes back. It is driven by r3bl's RRT machinery with an interrupt that does nothing, because the worker polls its own sources.

Warmup is the part that makes opening a file instant rather than merely fast: once the server is idle and no user request is pending, the worker sends one file's semantic-token request at a time, in walk order, so that by the time a file is opened the server has already computed it. A large file is asked for its first screenful only, an empty answer is retried a few times, and a warmup with no progress for a minute clears the queue rather than waiting forever.

A change of cargo features needs a restart, since the server does not reload them; a restart starts the new server before killing the old one, so a failed handshake keeps the colors that work. Requests sent while no worker is running go into a bounded buffer the worker drains itself when it starts, so nothing on the main thread retries.
