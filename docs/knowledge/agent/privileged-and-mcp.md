---
title: The turn's socket and the MCP client
summary: The two ways a turn reaches tools it does not host itself: the turn's private socket, and the MCP client for the repository's servers.
refs: src/agent/privileged.rs, src/agent/mcp.rs, serves, McpHost, McpTools
---

A turn's tools run in the agent process, which cannot see what only Explorer holds: the conversation tree current down to nodes not yet flushed, and the real knowledge store — which a feedback turn's copy of the repository stands for but is not. `src/agent/privileged.rs` is the socket that closes that gap: a private Unix socket in a directory removed with its guard, one JSON line per request and one per reply, serving exactly `conversation` and the mutation half of `knowledge`.

A turn opens one socket, and what it offers is no more than the turn's own tool list already allows, so the socket is not a way around a mode's limits.

The other direction is `src/agent/mcp.rs`, the MCP client: the repository's servers are declared in its session (the `/mcp` command line) and started in the background, with Explorer and on `/mcp add`; a turn waits up to 10 s for those still starting. Their tools are offered to the model as `mcp__<server>__<tool>`. A tool name is dropped at launch when it is too long, empty, or holds anything but letters, digits, `_` and `-`, because an endpoint refuses the whole request over one name it rejects.
