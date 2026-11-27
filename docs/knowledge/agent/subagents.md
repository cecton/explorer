---
title: Sub-agents
summary: The `agent` tool: roles as markdown files, how a model is chosen for a sub-agent, and what a sub-agent is not allowed to do.
refs: src/agent/subagent.rs, Role, agents, Subagents, prompts/subagent.md
---

`src/agent/subagent.rs` is the `agent` tool: a sub-agent is a fresh run that sees nothing of the conversation and returns only its final answer. A `Role` is a markdown file in `<config_dir>/agents/<name>.md` with a description, an optional model and an optional time budget, and the tool's `role` arguments are the names of the roles that exist.

A role whose model cannot run is left out of the tool rather than failing the call. Without a role the sub-agent runs on the configured `subagent-model`, or on the caller's own model. If a delegate runs on another model and fails, it is rerun once on the caller's model, and the answer says so when that rerun succeeds.

A sub-agent may not start sub-agents, and it is told what the parent's mode means — a feedback turn's sub-agent must not commit either. A run that hits its time budget returns its answer prefixed with a note that it may be partial. Roles, models and budgets are serialized into `Start`, so the agent process needs no config of its own.
