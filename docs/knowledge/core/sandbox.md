---
title: The sandbox behind a feedback turn
summary: `sandbox-exec`: the user and mount namespaces, why it re-execs, what it does and does not confine.
refs: src/sandbox.rs, run, child
---

`explorer sandbox-exec --root <repo> [--workspace <dir>] [--ro <path>]... -- <cmd>` runs a command in a user and mount namespace where, given a workspace, the repository's own path is replaced by the workspace, and where each `--ro` path cannot be written.

It guards against accidents, not against a hostile agent: an agent that writes to the repository path writes the workspace, and the workspace is deleted when the turn ends; one that writes a read-only path gets `EROFS`; everything else on the host is still reachable.

Unsharing into a user namespace must happen in a single-threaded process, so `sandbox-exec` re-executes itself as `sandbox-child` and that child enters the namespaces, binds, remounts and `exec`s the command. The command then keeps the child's pid, process group and standard streams, so the caller's signal handling and process-group kills work exactly as without a sandbox. Ids are mapped to themselves rather than to root. Exit codes follow the shell's convention: 125 for a setup failure, 126 when the command is not executable, 127 when it is not found.
