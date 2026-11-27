---
title: Serving the tools to other MCP clients
summary: `mcp-serve`: which tools it offers to another MCP client, why it offers no shell, and how it copes with a long-lived client.
refs: src/agent/serve.rs, serve, claude_server, src/main.rs
---

`explorer mcp-serve [root] [--own-tools-only] [--privileged-socket P] [--knowledge-read-only]` offers Explorer's own tools to another MCP client over standard input and output. It serves only what a client cannot do itself — `conversation`, `format_markdown` and `knowledge` — plus the MCP servers the repository's session lists (`--own-tools-only` leaves those out). Read, glob, grep, edit and shell are the client's business: serving them would put a shell behind anything able to start this process.

The server reloads the tree from disk on every call, because a client outlives the turns of the window that started it, and its node id is `external`; calls made through `--privileged-socket` skip that reload. Input is line-oriented and split on bytes so that a line which is not UTF-8 does not end the server; a line that is not UTF-8 or not JSON is skipped.

In `main::run` it is dispatched before the root lookup that panics outside a repository, so a client started in the wrong directory gets a message and exit code 2 instead of a crash it cannot see the reason for. `claude_server` builds the `mcpServers` entry the Claude backend passes to the CLI, so a Claude turn reaches the same tools.
