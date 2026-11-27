---
title: The agent's tools
summary: The built-in tools, the three groups they fall into, the context they act on, and background shell commands.
refs: src/agent/tools.rs, ToolContext, specs, describe, Background
---

The agent tools come in three groups. The ones only Explorer can answer — `conversation`, and the mutation half of `knowledge` — which go through the turn's private socket. The ones the agent process runs itself: `read`, `write`, `edit`, `glob`, `grep`, `shell`, `format_markdown`, and a reading `knowledge`. And the ones other programs provide: `agent` for sub-agents and every `mcp__<server>__<tool>` of the repository's MCP servers.

`ToolContext` is what a call acts on (the working directory, the turn's socket if it has one, the knowledge access); `Background` holds the shell commands a call left running. `knowledge` is one tool whose description depends on the access the turn has.

`tools::try_run` sends a call to the socket only when the turn has one and the socket serves that call; otherwise a flat match runs the in-process tools. The `agent` tool never reaches `try_run`: the agent loop handles it.

Each tool's description and schema is the only contract the model sees, so the tool's limits (line counts, byte caps, match caps, timeouts) are stated in the description as well as enforced in the code; a model that is told the cap does not waste a call discovering it. A name no group answers is an error naming the tool.
