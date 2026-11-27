---
title: The workspace of a feedback turn
summary: The reflink copy a feedback turn works in, when it is skipped, what the walk refuses, and the sandbox a commit turn uses instead.
refs: src/agent/workspace.rs, plan, Plan, Workspace, in_place_reason, Sandbox, sweep, warm, walk, names_a_copied_git_dir, shared_git_data
---

A feedback turn works in a copy of the repository when it is worth it: `.git` and `target/` included, made with reflinks so it costs metadata rather than the gigabytes of a build cache. The copy lives under `<root>/target/explorer` and the agent runs in it through `explorer sandbox-exec`, which shows it at the repository's own path, so an accidental write cannot reach the repository and a build in it finds every artifact fresh.

Copying is skipped — the turn runs in place (`Plan::InPlace`, with `in_place_reason` on the node) — when there is no build cache yet, when `CARGO_TARGET_DIR` or `CARGO_BUILD_TARGET_DIR` is set or the cache is not under `target/` (a `.cargo/config` target-dir), when `target/` is a link or not a directory, when a `core.worktree` points outside the copy, when the host has no user namespaces, or when a previous copy failed: that failure is remembered and the next turn runs in place. A filesystem without reflinks is not known until the first clone, and that failure fails that turn rather than falling back, since another turn may already be copying.

Every file keeps its own mtime to the nanosecond: cargo takes a build for fresh by comparing them, and a file that lost its timestamp would rebuild the world. A copy is built under a `.building-<pid>-<node>` name and renamed into place, so no turn ever reads a partial one, and `sweep` removes the copies of dead processes at startup.

The walk refuses anything that would leave the copy sharing git's data: a `.git` naming a directory outside the copy; a symlink resolving outside it under a git directory or on the way to one; and an `alternates` or `commondir` file, judged by name alone. Symlinks are judged second, once every `gitdir:` file is read.

A commit turn never has a copy: it works in the repository itself, under a sandbox that mounts `docs/knowledge` read-only. Where that sandbox cannot be set up, the turn runs unprotected, with a warning in the log rather than a failure.
