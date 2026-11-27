---
title: The workspace of a feedback turn
summary: The reflink copy a feedback turn works in, when it is skipped, and the sandbox a commit turn runs under instead.
refs: src/agent/workspace.rs, plan, Workspace, Sandbox, sweep, warm
---

A feedback turn works in a copy of the repository when it is worth it: `.git` and `target/` included, made with reflinks so it costs metadata rather than the gigabytes of a build cache. The copy lives under `<root>/target/explorer` and the agent runs in it through `explorer sandbox-exec`, which shows it at the repository's own path, so an accidental write cannot reach the repository and a build in it finds every artifact fresh.

Copying is skipped — the turn runs in place (`Plan::InPlace`, with `in_place_reason` recorded on the node) — when there is no build cache yet, when `CARGO_TARGET_DIR` or `CARGO_BUILD_TARGET_DIR` is set or the cache is not under `target/` (a `.cargo/config` target-dir), when `target/` is a link or not a directory, when a `core.worktree` points outside the copy, when the host has no user namespaces, or when a previous copy failed: that failure is remembered and the next turn runs in place. A filesystem without reflinks is not known until the first clone, and that failure fails that turn rather than falling back, since another turn may already be copying the repository.

Every file keeps its own mtime to the nanosecond: cargo takes a build for fresh by comparing them, and a file that lost its timestamp would rebuild the world. A copy is built under a `.building-<pid>-<node>` name and renamed into place, so no turn ever reads a partial one, and `sweep` removes the copies of dead processes at startup.

A commit turn never has a copy: it works in the repository itself, under a sandbox that mounts `docs/knowledge` read-only so the store only changes through the knowledge tool, whose staging the turn then commits. Where that sandbox cannot be set up — no user namespaces, `docs/` a link, the store on another device — the turn runs unprotected, with a warning in the log rather than a failure.
