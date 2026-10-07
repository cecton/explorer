# Conversation Agent — Design

Date: 2026-10-05
Status: implemented

## Goal

An agentic AI chat inside Explorer whose history is a **tree**, not a flat session. Every
user prompt is a node; any node can be continued, so any node can be forked. The tree is
stored as plain files, survives restarts, and links each node to the git commits the
agent produced for it.

Every prompt is sent in one of two modes chosen by the user: **feedback** (the agent
teaches and advises; whatever it tries in the working tree is discarded when its turn
ends) or **commit** (the agent executes an instruction and commits the result).

Three windows make up the feature:

- **Conversation Tree** — the whole tree of the repository; navigation, fork points,
  favorites, recovery, deletion.
- **Discussion** — one node: its prompt, its reply, and an input field for the next
  prompt.
- **Model Picker** — fuzzy list of `endpoint/model/effort` entries.

The agent talks to OpenAI-compatible endpoints and to the local Claude Code CLI (for
Claude subscription users).

## 1. Motivation and reasoning

### Why a tree

Agentic CLIs (Claude Code, Kimi, opencode) give context through flat sessions: a
conversation is a line. Real work is not a line. It starts from one framing prompt ("this
project's goals are …, these are the technologies, write down the architecture") and
splits wherever it makes sense: a feature here, a bug there, a design question that goes
nowhere. Forums and email solved this long ago with threads. The conversation tree is the
same idea: one root per topic, a branch per direction, and the freedom to go back to any
earlier point and take another path without losing the first one.

### The role of the user and of the agent

The user is the creative component: ideas, direction, decisions. The agent is to code
what a spell checker is to prose: it handles the mechanics so the user can stay on the
ideas. But creativity needs inspiration, and inspiration needs knowledge, which again
comes from the agent. This gives the two modes, and two very different agents:

- **feedback — the teacher.** The agent shares knowledge: it explains, gives opinions,
  brings best practices and what is common elsewhere, and says when an idea is a bad
  one. It is invited to be talkative. It may experiment in the working tree to verify
  what it says, but leaves nothing behind: the tree is reset when its turn ends, and it
  never commits. (opencode calls the equivalent mode "plan".)
- **commit — the executor.** The reasoning has been done. The agent follows the
  instruction as given, speaks up only for an unforeseen hurdle, and commits. (opencode:
  "build".)

The user picks the mode for each prompt; Explorer never guesses it: every prompt is
feedback, and `/commit` starts a commit turn.

### Why a commit for every editing prompt

Most people dislike agents committing for them. Here it is the point. One prompt, one
recorded outcome: every change the agent makes is a commit, and every commit can be
looked up in the tree to find the prompt that caused it. Any decision or path can be
traced to its origin, like a `git blame` that ends at a sentence written by the user
instead of at a diff. It also makes "allow everything" a reasonable permission model:
whatever the agent did is in git and can be reverted.

Commits themselves stay clean: no conversation id, no URL, no tool trailer. The link
lives in the node's metadata only.

### Why plain files

The tree is a directory tree of Markdown files. It can be read, grepped and backed up
without Explorer, and the agent itself can consult it to recall past decisions and the
reasons behind them.

## 2. Decisions taken during design

| Topic | Decision |
|---|---|
| LLM library | `genai` as wire client; Explorer owns the agent loop, the tools and the tree. Not adk-rust (its runner owns session history, which fights a tree). MiniMax Code is TypeScript: reference only, like opencode. |
| Endpoint kinds | `openai` (Chat Completions with URL and token, default) and `claude-code` (spawns the local `claude` CLI; the only sanctioned way to use a Claude subscription). No Anthropic API-key kind. |
| Mode | Feedback for every prompt; the built-in `/commit` starts a commit turn. Saved in the node, it selects the system prompt and the git rules. |
| Fork versus git | Forks are conversation-only. The repository stays at current HEAD; the agent is told which commits happened since the node it continues from. |
| Context replay | Full transcript (tool calls and results) stored per node and replayed exactly. |
| Backend switch | Allowed. Crossing between `claude-code` and `openai` replays ancestors as prompt and reply text only (lossy handover). |
| Storage | `~/.local/share/explorer/conversations/<repo-hash>/`, never inside the repository. Tokens in `~/.local/share/explorer/auth.kdl`. |
| Committer | The agent commits; Explorer verifies after the turn and records the hashes. Amend and rebase are allowed. |
| Dirty tree | Every prompt, whatever its mode, is refused while the working tree has uncommitted changes. |
| Feedback scratch work | A feedback turn may edit, build and test freely; Explorer hard-resets the working tree when the turn ends, and a commit it makes is not recorded to the conversation, so it must not commit. |
| Forgotten commit | Explorer stays in the loop and sends a hidden nudge asking the agent to commit. |
| Leftovers | Only when nudging is impossible or exhausted, Explorer stashes what is left and flags the node. |
| Locking | One turn at a time per repository, whatever its mode. |
| Reply and work log | The reply is the text of the answers that called no tools. The narration between tool calls and the tool calls themselves go to a display-only work log. |
| Permissions | None in v1: every tool call is allowed, file tools are confined to the repository root. |
| Model list | Fetched from each endpoint at startup; a fixed effort set is offered for every model. |
| Context overflow | Manual `/compact` built-in command. No automatic compaction. |
| Agent access to the tree | A native `conversation` tool in Explorer's own loop, serving the table of contents as small linked pages. Not available to `claude-code` endpoints. |
| Discussion windows | One per opened node. |
| Memory | Only metadata, titles and summaries are held in memory; prompts, replies and transcripts are read from disk when needed. |

## 3. Vocabulary

- **Node** — one user prompt and everything it produced. A directory on disk.
- **Root** — a node without parent; the start of a conversation.
- **Fork** — sending a prompt from a node that already has a child. There is no fork
  command: it is the same action as continuing.
- **Turn** — the agent's work for one node, from prompt to final reply, including all
  tool calls.
- **Mode** — `feedback` or `commit`, chosen before sending.
- **Prompt** — the user's text for a node.
- **System prompt** — the instructions Explorer sends ahead of the conversation. This is
  the standard term for what was first called the "base prompt"; there is one per mode.
- **Answer** — one response of the model within a turn. Every answer but the last ends
  by calling tools.
- **Reply** — the text of the answers that called no tools: what the turn has to say.
- **Work log** — what led to the reply: the text of the answers that called tools, and
  the tool calls. Shown to the user, never sent to a model.
- **Transcript** — the exact message list of a turn as exchanged with the model.
- **Model spec** — the string `endpoint/model/effort`.

## 4. Storage

Root directory: `<data_dir>/conversations/<repo-hash>/`, where `<repo-hash>` is the same
16-hex prefix `session_file_path` uses (SHA-256 of the canonical repository root).

Each node is a directory named after its creation time in UTC:
`2026-10-05T14-32-07Z`. Colons are avoided because they are illegal on Windows
filesystems and awkward in shells; this form still sorts chronologically. The name must
be unique in the whole tree, not only among siblings; on a clash `-2`, `-3`, … is
appended. The directory name is the **node id**.

```
conversations/3fa9c1d2e4b5a6f7/
  2026-10-05T14-32-07Z/            root
    metadata.md
    prompt.md
    reply.md
    activity.md
    title.md
    summary.md
    transcript.jsonl
    2026-10-05T14-51-12Z/          child
      …
      2026-10-05T15-20-40Z/        grandchild
    2026-10-06T09-02-55Z/          second child = a fork
```

| File | Content | Written |
|---|---|---|
| `prompt.md` | The exact prompt sent, after slash-command expansion. | Immediately on send, before any network call. |
| `input.md` | The prompt as typed, before slash-command expansion. This is the text given back for editing (`e` in the tree). Absent when the prompt was sent as typed: the typed text is then `prompt.md`. Never sent to a model. | With `prompt.md`, only when a custom command or `/commit` was expanded. |
| `metadata.md` | Flat `key: value` lines, see below. | On send; rewritten when the turn ends and when favorite toggles. |
| `reply.md` | The text of the answers that called no tools, in order: the reply proper. | Flushed about once per second while streaming, when an answer turns out to call tools, and at the end. |
| `activity.md` | The work log, in order: the text of each answer that called tools, and one `- <tool> <detail>` line per tool call. Display-only: never replayed and never sent to a model. Absent when the turn called no tools. | Appended as each answer calls its tools. |
| `transcript.jsonl` | One message per line, exactly as replayed: the user message as sent, assistant messages with tool calls, tool results, and any message Explorer injected. Absent for `claude-code` nodes. | Appended as each message completes. |
| `title.md` | One line like a commit subject, asked at 72 characters and cut at 100. | When the post-turn call returns. |
| `summary.md` | About 50 words, like a commit body: what was asked and done. | When the post-turn call returns. |

`metadata.md` keys:

```
type: commit                  feedback | commit | compact (known when sending)
endpoint: kimi
kind: openai
model: kimi-k2
effort: high
created: 2026-10-05T14:32:07Z
status: done                  running | done | failed | cancelled
head-before: <full hash>
head-after: <full hash>
commits: <full hash> <full hash>     commit nodes only; oldest first
stash: <full hash>            only when leftovers were stashed
scratch-reset: true           only when a feedback turn's changes were discarded
favorite: true                only when set
command: review               only when a custom command was expanded
system-prompt: off            only for a command sent without the mode prompt
session-id: <uuid>            claude-code only
tokens-in: 84210
error: <one line>             only for failed
```

`type` is the mode the prompt was sent in, or `compact` for the third kind of node
(section 10). A commit node may have no commits when the agent had nothing to change.

Rules:

- **Startup** reads, once and off the UI thread, every node's `metadata.md`, `title.md`
  and `summary.md`, and builds the tree. `prompt.md`, `reply.md`, `activity.md` and
  `transcript.jsonl` are **never** kept for the whole tree: they are read
  from disk when a Discussion window opens, when a context is assembled, or when the
  `conversation` tool asks, and dropped afterwards.
- After startup, memory is the source of truth for the tree shape and metadata, and
  every mutation writes through. The directory is not watched.
- Any file may be missing. A directory containing only `prompt.md` is a valid node.
  A missing title falls back to the first line of the prompt, read at startup for those
  nodes only.
- A node found with `status: running` at startup was interrupted; it is rewritten as
  `failed` with `error: interrupted`.
- Writes use the write-to-temp-then-rename pattern already used by sessions.

## 5. Configuration

### `config.kdl`

```kdl
agent {
    endpoint "kimi" {
        url "https://api.moonshot.ai/v1"
    }
    endpoint "claude" {
        kind "claude-code"
        models "opus" "sonnet" "haiku"
    }

    summary-model "kimi/kimi-k2/low"
}
```

- `endpoint "<name>"` — the name is the first segment of a model spec and must not
  contain `/`.
  - `kind` — `openai` (default) or `claude-code`.
  - `url` — `openai` only: base URL to which `/chat/completions` and `/models` are
    appended.
    Every chat request carries an `x-opencode-session` header holding the id of the
    conversation's root node: opencode refuses requests without it, and uses it for
    routing and prompt caching; other servers ignore it.
  - `models` — `claude-code` only, since the CLI has no model-list route. Defaults to
    `opus`, `sonnet`, `haiku`.
- `summary-model` — model spec used for both titles and summaries. If absent, neither is
  generated.

Custom slash commands are not in `config.kdl`: each is a file of
`<config_dir>/commands/`, see section 10.

### `auth.kdl`

Tokens are kept out of `config.kdl` so that the config can be versioned. They live in
`<data_dir>/auth.kdl`, one node per endpoint name:

```kdl
kimi "sk-…"
openrouter "sk-or-…"
```

An `openai` endpoint without a token in `auth.kdl` (or with the file missing) produces a
`tracing::warn!` and is skipped: no models are fetched or listed for it, and nothing
else is blocked. `claude-code` endpoints need no entry; the CLI uses its own login.

### Model spec

`endpoint/model/effort`. `/` is the common separator (opencode uses `provider/model`).
Model ids may themselves contain `/` (OpenRouter), so parsing is: first segment =
endpoint, last segment = effort, everything between = model id.

Efforts offered for every model: `default`, `low`, `medium`, `high`. `default` sends no
effort parameter, which is the only working choice for models that reject one.

### Model list

At startup Explorer fetches `GET <url>/models` from every `openai` endpoint that has a
token, in the background. A failed fetch is retried when the picker opens. The picker
lists every `endpoint × model × effort` combination.

### Which model a prompt uses

There is no configured default and nothing about the model is saved in the session.

- **Continuing or forking from a node**: the Discussion window starts with that node's
  model spec. The user may change it with `Ctrl+P` before sending.
- **New root**: the window starts with the model last picked during this run of
  Explorer. After startup nothing has been picked yet, so the first new conversation of
  the day requires a pick; sending without one opens the picker.
- A pick applies to the window it was made in, and becomes the model offered to new
  roots for the rest of the run.

A conversation therefore keeps its model along a branch until the user changes it, and a
changed model carries on from the node that used it.

## 6. Modes

| | feedback | commit |
|---|---|---|
| System prompt | `prompts/feedback.md` | `prompts/commit.md` |
| Role | teacher: explains, advises, disagrees | executor: follows the instruction |
| Tools | all of them | all of them |
| Working tree | a lab: edits are allowed and discarded | the deliverable: edits are committed |
| Dirty tree on send | refused | refused |
| Lock | exclusive | exclusive |
| After the turn | hard reset of the working tree | verify, nudge, record commits |

- A window has no mode of its own. A prompt is sent in feedback mode, and the built-in
  `/commit` (section 10) is the only way to start a commit turn, so the mode travels
  with the text of the prompt: through the queue, and back into the input when a turn
  fails or is cancelled. The input has no label.
- In feedback mode the working tree is a lab. The shell and the editing tools are
  available on both kinds of endpoint, so the agent can apply a
  candidate fix, build and run the tests to verify what it claims. Nothing of it
  survives: when the turn ends, however it ended, Explorer runs `git reset --hard HEAD`
  and `git clean -fd` (ignored files are kept) and records `scratch-reset: true` if
  there was something to discard. This is safe because the tree was clean when the
  turn started, so only the turn's own changes can be lost.
- The reset does not undo commits. The feedback prompt forbids committing and moving or
  rewriting history, and nothing else enforces it: a commit made by a feedback turn
  stays, and is not recorded in the node.

### Locking

One turn runs at a time per repository, whatever its mode: a feedback turn experiments
in the working tree and a commit turn needs it clean and to itself, so two turns can
never share it. A prompt sent while any turn is running, `/compact` included, is
**queued** and starts as soon as the lock is free.

**Queue.** The queue is global, in order of sending, and lives in memory only: it is
not part of the session, and after a restart the draft is back in the input, to be
sent again by hand. An entry is a window and the node it showed, never a copy of the
text: the queued prompt stays the draft of its window, so it can be edited, and its
model changed, until the turn starts. Slash commands are expanded at that
moment. A window holds at most one queued prompt; `Enter` does nothing more while it
waits. Only what time fixes is queued: a prompt sent while another turn runs, or below
a node whose own turn is still running. Every other refusal is immediate.

- The status line shows `queued`, the place in the queue and the keys below. When the
  window's own node is running, this follows its progress.
- `Esc` takes the prompt back out of the queue; the text stays in the input.
- `Ctrl+Plus` and `Ctrl+Minus` move it one place earlier or later. They rely on the
  terminal reporting modified keys (xterm `modifyOtherKeys`).
- When a turn ends, entries start next first for as long as no turn is running. An
  entry that can no longer be sent (endpoint or model gone, empty draft) leaves the
  queue with the reason in its window. A dirty working tree holds the whole queue
  back, with the reason in the window of the next entry: `Enter` in any Discussion
  window tries again, and no prompt overtakes the queue meanwhile.
- A queued follow-up to a node whose turn fails or is cancelled is dropped, and its
  text replaced as described under Cancel in 8.2.
- Closing a window, or deleting its node, removes its entry.

Recovering a node and replaying one with another model (section 9) start from the tree,
not from an input: they are not queued and are refused while a turn runs.

A refused send keeps the input and shows the reason on the status line.

## 7. Backends

New module tree `src/agent/`:

- `mod.rs` — `Mode`, `ModelSpec`, `Endpoint`, `Backend`, the transcript `Message`.
- `turn.rs` — the turn lifecycle off the UI thread, for both backends.
- `llm.rs` — the only file that imports `genai`; converts between our message type and
  genai's, resolves endpoint URL and token per call.
- `agent_loop.rs` — Explorer's own loop for `openai` endpoints.
- `tools.rs` — tool definitions and execution.
- `claude_code.rs` — `claude` subprocess driver.
- `context.rs` — system prompts, ancestor replay, and the notes Explorer adds.
- `commands.rs` — slash command expansion.
- `models.rs` — model list fetch.

A running turn shares a `LiveTurn` (reply so far, work log, current tool, token count,
session id) with the UI through its `TurnHandle`, the same way a terminal pane shares
its buffer.
Discussion windows read it at render time; the task only sends `TurnProgress` to ask
for a render and `TurnFinished` with the outcome.

### 7.1 Own loop (`openai`)

Request = system prompt + replayed ancestors + new user message + tool definitions.
Stream the turn; when the model asks for tools, run them, append the results, call the
model again; the turn is over when a response contains no tool call.

- **System prompt**: the mode's prompt + `prompts/conversation_tool.md` + the
  repository's `AGENTS.md` if present + an Environment section built at runtime
  (repository root, date, current HEAD).
- **Replay**: for each ancestor from root to parent, the messages of its
  `transcript.jsonl`, read from disk. The walk starts at the nearest `compact` ancestor
  if there is one. Ancestors keep the system prompt of the *new* turn's mode: only one
  system prompt is sent per request.
- **Caching** is left to the endpoint: OpenAI-style servers cache by prefix. A straight
  line of nodes in the same mode re-reads the cache; a fork or a mode switch pays once.

### 7.2 Tools (own loop)

| Tool | Behavior |
|---|---|
| `read` | Read a file, optional line range. |
| `write` | Create or overwrite a file. |
| `edit` | Replace an exact string in a file; fails if the string is absent or ambiguous. |
| `glob` | List paths matching a pattern. |
| `grep` | Regex search over file contents. |
| `shell` | Run `sh -c` in the repository root. Default timeout 120 s, maximum 600 s; output truncated to head and tail beyond a size cap. |
| `conversation` | Table of contents and node reading, see 7.3. |

- File tools resolve paths against the repository root and refuse anything outside it.
  They read and write the disk directly; walks prune `.git` and `target` like the loader.
- No permission prompts. `shell` is unrestricted.
- `git` on `PATH` is required: the agent commits through `shell`.

### 7.3 The `conversation` tool

A full table of contents of the tree is too large to hand to a model, so it is served as
**pages of bounded size that link to each other** through node ids. The agent can walk
up, down or sideways without ever loading everything.

- `toc` (no node) — the roots, newest first, 30 per page: id, date, title, summary,
  number of nodes below. A `page` argument continues the list.
- `toc node=<id>` — one page about that node:
  - **Path**: its ancestors from the root, id and title only;
  - **Node**: id, date, type, commit short hashes, title, summary;
  - **Continues**: the nodes following it while each has a single child, up to 20, each
    with id, type, commits, title and summary; if the line goes on, the id to ask next;
  - **Branches**: where the line forks, each child's id, title, summary and subtree
    size.
- `read node=<id>` — the node's `prompt.md` and `reply.md`, from disk.
- `commit hash=<hash>` — the node whose metadata lists that commit.

Pages are generated on demand from the in-memory tree, which already holds every title
and summary; they are not files, so they can never be stale. The node the agent is
running in is marked on every page that shows it.

### 7.4 `claude-code`

Explorer spawns the local CLI in the repository root:

```
claude -p --output-format stream-json --verbose --include-partial-messages \
       --model <model> --effort <effort> \
       --append-system-prompt <mode prompt + Environment> \
       --system-prompt-snapshot off \
       --permission-mode bypassPermissions \
       --settings '{"attribution":{"commit":"","pr":""}}' \
       [--resume <parent session-id> --fork-session]         child nodes
```

- The prompt is written to stdin. Text deltas, tool activity, usage and the final result
  are parsed from the stream-json events. An answer is known to call tools when a
  `tool_use` block starts; its text then moves to the work log (section 8). The new
  session id is read from the stream and stored in `metadata.md`.
- The CLI returns no reasoning text (thinking blocks arrive empty, with a signature
  only), so there is nothing to show for it.
- Prompt snapshotting is turned off: by default a resumed session keeps the system
  prompt of its first request, so a commit turn forked from a feedback node would still
  be told it is in feedback mode.
- Claude Code runs its own loop and its own tools, and keeps its own transcript; no
  `transcript.jsonl` is written. Its default system prompt is kept; ours is appended.
- The attribution setting removes Claude Code's own commit trailer, in line with the
  clean-commit rule.
- **Forking**: every child resumes its parent's session with `--fork-session`. A
  session is therefore never appended to after its turn ends and stays an exact snapshot
  of its node, so any node can be continued any number of times.
- With effort `default` the `--effort` flag is omitted.
- The `conversation` tool is not available to these nodes.
- If the parent's session cannot be resumed (Claude Code prunes old sessions), the turn
  falls back to a lossy handover in a fresh session.

### 7.5 Lossy handover

Used when a child and its parent are on different sides of the `claude-code` boundary, or
when a Claude Code session is gone. Ancestors are replayed as text only: for the own
loop, alternating user and assistant messages built from `prompt.md` and `reply.md`; for
`claude-code`, one preamble at the top of the first message of a fresh session. The tree
marks such a node.

## 8. Turn lifecycle

On `Enter` in a Discussion window:

1. **Guards.** The prompt is not empty; the window has a model (otherwise the picker
   opens); the node shown is `done` (or the window is a new root); no other turn is
   running; the working tree is clean, untracked non-ignored files included, whatever
   the mode. While a turn is running the prompt is queued instead (section 6,
   Locking), and these steps happen when its turn comes. From a window showing a
   failed or cancelled node, the prompt is sent as a **sibling** of that node, below
   its parent.
2. **Expand** a slash command if the prompt starts with one.
3. **Persist.** Create the node directory; write `prompt.md`, `input.md` when a
   command was expanded, and `metadata.md` (`type`, `status: running`, `head-before`).
   The window now shows the new node and the tree selection follows it.
4. **Start** the turn. The title and the summary are asked once it has ended (8.2).
5. **Stream.** Reply text is flushed to `reply.md`; transcript lines are appended as
   messages complete. A turn is a series of model answers, and all but the last end by
   calling tools. The text of an answer streams into the reply; when the answer calls
   tools, that text was narration and moves out of the reply into the work log
   (`activity.md`), followed by one line per tool call. The reply is thus what the
   tool-free answers said, in both modes and for both backends, and
   `transcript.jsonl` is unaffected.
6. **Close**, by mode:
   - **feedback / compact**: record `head-after`, then discard whatever the turn left
     in the working tree (section 6) and record `scratch-reset` when something was
     discarded.
   - **commit**: see 8.1.
7. **Finish.** Rewrite `metadata.md`; the turn no longer counts as running, so the next
   prompt can be sent; open Git Log windows and the dirty flag refresh.

### 8.1 Closing a commit turn

When the model stops without asking for a tool, Explorer checks the working tree.

- **Clean** — done. `head-after` = HEAD; `commits` = the commits reachable from
  `head-after` and not from `head-before`, oldest first. This definition also covers
  amended and rebased history.
- **Dirty** — the turn is not over. Explorer appends a short user message ("The working
  tree still has uncommitted changes: `<git status --short>`. Commit them now.") and
  calls the model again, staying in the same loop. This is how agent loops work: the
  loop only ends when the caller decides so, and an extra user message is a normal way
  to keep it going. At most **two** nudges per turn.
  - The nudge is **hidden from the user**: it is not in `prompt.md`, and the Discussion
    window never shows it. What the agent answers to it is appended to the reply.
  - The nudge **is kept in `transcript.jsonl`**, marked as injected. It has to be: the
    transcript is what child nodes replay, and without it the model would later see
    itself committing for no visible reason, with a history that differs from what was
    actually exchanged.
  - On `claude-code`, the nudge is a second `claude -p --resume <this node's session>`
    call without `--fork-session`.
- **Still dirty after two nudges, or the turn failed or was cancelled** — no model can
  be asked. Explorer runs `git stash push --include-untracked` with a message naming the
  node, stores the stash commit hash in `stash:`, and flags the node as uncommitted.
  Stashing shells out to `git` (gix has no stash writer).

Amended or rebased-away commits remain recorded in older nodes' metadata, but git may
eventually garbage-collect the objects themselves.

### 8.2 Other rules

**Repository moved note.** If the parent's `head-after` differs from HEAD when a prompt
is sent (a fork from an old node, commits made by hand, or rewritten history), a note is
prepended to the user message as sent: the commits reachable from HEAD and not from the
parent's `head-after`, short hash and title, capped at 20. Like a nudge, the note is in
the transcript but not in `prompt.md`.

**Workspace note.** Every feedback and `/compact` message carries a note saying that
the tree was clean when the turn started and that everything changed in it is
hard-reset when the turn ends, so the agent neither holds back from experimenting nor
spends tokens reverting. Like the other notes it is in the transcript but not in
`prompt.md`.

**Cancel.** `Esc` in the Discussion window of a running node aborts the HTTP stream or
kills the child process group (including a running `shell` command). Status becomes
`cancelled`; the partial reply and the work log are kept; the turn is closed as
described above for its mode, so a cancelled or failed feedback turn is reset too.
When the window also holds a queued follow-up, the first `Esc` takes that back and the
second cancels the turn.

**Prompt given back.** When a turn ends `cancelled` or `failed`, every window showing
its node gets the prompt back in its input, as it was typed (`input.md`, else
`prompt.md`), replacing whatever the input held. The window keeps showing the node, its
error and its partial reply; sending from it creates a sibling.

**Quit.** Quitting Explorer cancels running turns the same way. A running turn is not
resumable and is not part of session restore.

**Title and summary.** Until a turn ends, a node has no title or summary files and the
tree shows the first line of its prompt. When a turn ends `done` (new nodes, reruns and
recovered nodes alike), one non-streaming call to `summary-model` without tools
(`prompts/title_summary.md`; on `claude-code`, `claude -p --tools ""`) reads a record of
the turn: the prompt in `<prompt>`, the reply (cut at 8000 characters) in `<reply>` and,
in commit mode, the subjects of the commits made in `<commits>`, closed by one line asking
for the title and the summary. The tags and that line keep the model from answering the
prompt instead of describing it. It answers in the shape of a commit message: a title of
at most 72 characters, a blank line, then a summary of about 50 words. The first line
is the title (hard cut at 100 characters), the rest the summary; they replace `title.md`
and `summary.md`. The call
starts after the turn's outcome is applied, so it never delays the node. Failed and
cancelled turns keep the prompt-based display, and failures are silent: the files stay as
they were. Without a `summary-model` the call is skipped and nodes keep the prompt-based
display.

## 9. Recovering, replaying and editing nodes

A node whose status is not `done` (failed, cancelled, interrupted) is **incomplete**.
Incomplete nodes are always leaves: a prompt sent from a window showing one becomes its
sibling. Three tree keys
deal with them and with second opinions:

- **`Enter` — open, and recover if incomplete.** Opens the node in a Discussion window.
  If the node is incomplete, its turn is run again **in place**: same directory, same
  prompt, same mode, and the endpoint, model and effort it was originally asked with.
  `reply.md`, `activity.md` and `transcript.jsonl` start over. The guards of section 8
  apply; if one fails (dirty tree, another turn running, endpoint or model no longer
  available) the window opens on the old partial reply and shows the reason.
- **`m` — same prompt, another model.** Opens the Model Picker; on `Enter` Explorer
  creates a **sibling** of the node with the same prompt and mode, run with the chosen
  model spec, and opens it. The pick belongs to that sibling only; it does not become
  the model offered to new roots. Works on any node, complete or not.
- **`e` — edit as a new sibling.** Opens a Discussion window on the node's **parent**
  with the input prefilled with the node's prompt as typed, `/commit` included. Nothing is
  created until the user sends. On a root, this is a new-root window.

Since forks are conversation-only, a sibling of a commit node runs on a repository that
already contains the first node's commits; the repository-moved note tells the agent so.
Comparing two models on the same code change requires the user to reset the branch
first.

## 10. Slash commands

A prompt that starts with `/name`, where `name` is `[a-z0-9-]+` followed by whitespace or
the end of the text, is looked up among the commands:

- **Custom command**: a file `<config_dir>/commands/<name>.md`, read at startup. The
  file name is the command name and the body is its base prompt. The prompt becomes
  the base prompt, then two newlines, then the rest of the typed text. With no rest,
  just the base prompt.
  Expansion happens on send; `prompt.md` stores the expanded text, `input.md` the text
  as typed and `metadata.md` the command name. A node replayed with another model
  carries the `input.md` of its source. An untitled node shows the first line of the
  typed text in the tree.

  The file may open with a metadata block: `---` on the first line, flat `key: value`
  lines, and a closing `---`. Unknown keys are ignored, and a block that is never closed
  is part of the prompt.

  ```markdown
  ---
  system-prompt: off
  argument: commit-like
  description: Squash the commits since a ref into one
  ---
  Squash all commits into a single commit since:
  ```

  - `system-prompt: on | off` (default `on`). With `on`, the command is a feedback turn
    like any prompt. With `off`, the turn gets no mode prompt on either kind of endpoint
    (the rest of the system prompt stays) and no workspace note. Since nothing tells it
    that the tree is a lab, its work is kept: the turn runs and closes as a commit turn
    (commits recorded, nudge, leftovers stashed) and its node has type `commit`. The
    node records `system-prompt: off`, so a replay or a rerun leaves the mode prompt
    out again.
  - `argument` and `description` are display only, see the hint below.

  A file whose name is not `[a-z0-9-]+`, that takes the name of a built-in, or that has
  no prompt is logged and skipped.
- **Unknown name**: the text is sent as typed. A prompt beginning with a path such as
  `/home/…` is therefore harmless.
- **`/commit [instructions]`** (built-in, cannot be overridden): starts a commit turn.
  `prompt.md` is `OK, go ahead.`, followed, when instructions were typed, by a blank
  line, `Additional instructions:`, a blank line and the instructions; `input.md` holds
  the text as typed. `prompts/commit.md` tells the agent that a go-ahead stands for the
  instruction closing the previous reply, or else for the plan the conversation settled
  on, and that additional instructions win on conflict. A bare `/commit` in a new
  conversation is refused with a notice: it has nothing to go ahead with.
- **`/compact [instructions]`** (built-in, cannot be overridden): creates a child node of
  type `compact`. Its turn is one call to the window's model
  with the branch so far as context and `prompts/compact.md` as instruction, plus any
  typed instructions; no tools. It is guarded and closed like a feedback turn: clean
  tree, no other turn running. Its `reply.md` is the summary. Descendants
  of a compact node replay context starting from it, with that summary as the opening
  message. On `claude-code`, the summary is produced in a fork of the parent session and
  descendants start a fresh session from it.

**Hint.** While the draft of a Discussion window is a known command name with nothing
typed after it, the input shows a placeholder in the `comment` color (section 11): what
the command expects, continuing the draft (`<argument>` for a custom command), then its
description on the next row. The built-ins have one too: `/commit` with
`[additional instructions]` and `/compact` with `[instructions]`. The hint doubles as
the sign that the name was recognized, since an unknown name is sent as typed. It
disappears once anything is typed after the name.

The Discussion window shows the input-token count of the node's last request so the user
can see a branch growing. A context-length error from the endpoint is shown as such and
suggests `/compact` from the parent node.

## 11. User interface

### 11.1 Conversation Tree window

`Window::ConversationTree`, unique, opened with `<leader>c` (`open-conversations`).

- One row per node: date in the gutter, title cut to the pane width, and on the right
  the short hash of the node's commit (first of several, with `+n`).
- The **selected** node is expanded in place: its title is shown in full, wrapped over
  as many lines as it needs, and its summary is shown just below, wrapped as well.
  Moving the selection collapses the previous node and expands the new one, so browsing
  the tree with the arrows reads like a preview of each prompt.
- **Layout of the tree**: roots are listed oldest first, like the nodes inside a
  conversation, so the whole pane reads from top to bottom; with nothing selected, the
  pane selects the newest root, the last one. The descendants of a root sit one level
  below it, which sets conversations apart. After that, a node with a single child keeps
  that child at the **same** indentation, so a straight line of prompts reads as a flat
  list. Indentation grows only below a node with several children, one level per fork. Each row draws a
  box-drawing prefix: one `│` rail per fork above whose later branches still follow, an
  arm (`├─`, or `└─` for a fork's last branch) at the start of a branch, and the fold
  column (`+` collapsed, `-` expanded, `─` for a leaf that starts a branch). A fork's
  arms and rail sit in the column of its fold marker, so they hang right under it. The
  first row below a root is a branch too and gets a `└─` arm, which ties a
  single-thread conversation to its root. The wrapped title and summary lines of the
  selected row repeat the rails, with a `│` in the fold column when the next row hangs
  one level deeper. Without the flat rule a 50-prompt conversation would be indented 50
  levels.
- **Threads**: a thread starts at a root or at a branch of a fork and runs through
  single replies until a leaf or a fork. It folds into its head, its first node: a
  collapsed thread shows only the head, so a linear conversation is one row. The fork
  ending a thread is not part of what the head hides: it stays visible, with its own
  fold, whether the thread is collapsed or not.
- Threads and forks are collapsed until expanded, marked `+` or `-` in the fold column
  of the thread head or of the fork. Sending a prompt expands the folds on the way to
  the new node. The expanded set is saved in the session.
- Feedback and commit nodes have distinct colors. Other states get a color and a
  one-character marker in a fixed gutter column right after the date: running (a
  braille spinner, `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`), failed `✗`, cancelled `■`, uncommitted (stashed)
  `±`, compact `≡`. All of them exist in the Terminus font. While a turn runs it asks
  for a redraw every 100 ms, one spinner frame; the frame is computed from the time
  the turn started. Favorites are highlighted across the whole row. Rails and arms are
  drawn in `ui.linenr`, never bold; on the selected row the date and rails, including
  those of its wrapped lines, use `ui.linenr.selected`, falling back to `ui.text`,
  since in ayu-derived themes `ui.linenr` equals the selection background.
- Keys:
  - `Up` `Down` `PgUp` `PgDn` `Home` `End` — move the selection.
  - Mouse click — moves the selection only. Double click — same as `Enter`.
  - `Left` / `Right` — collapse / expand a thread or a fork. `Right` on a thread head
    also moves the selection to the thread's last reply (the one before the fork when
    the thread ends in one), whether the thread was collapsed or already open: that is
    the node to read the outcome from and to continue. `Left` elsewhere jumps to the
    thread head or fork the row hangs from. The expanded set is saved in the session.
  - `Enter` — open the node; recover it if incomplete (section 9).
  - `m` — same prompt with another model (section 9).
  - `e` — edit as a new sibling (section 9).
  - `n` — new root: opens a Discussion window with no node, only the input.
  - `f` — toggle favorite. `<leader>f` (`toggle-favorite`) does the same.
  - `c` — open Commit Diff windows for the node's commits (existing `Window::CommitDiff`).
  - `Ctrl+D` — first press arms deletion of the node and its descendants and the status
    bar shows the count; a second `Ctrl+D` on the same node deletes. Any other key or a
    selection change disarms.
- **Deletion** removes the directories and closes Discussion windows showing deleted
  nodes; their highlights go with them. Commits and stashes are not touched. A running node and its ancestors cannot be
  deleted.

### 11.2 Discussion window

`Window::Discussion(usize)`, state in `AppState.discussions`; several can be open. A
window holds its node's prompt, reply and work log in memory only while it is open.

- Top to bottom: title row (node title, the window's model spec on the right); the node's
  prompt, visually set apart; the work log when shown; the reply; a status line; the
  input, without a label.
- Prompt, work log and reply form one scrollable document owned by the PaneManager
  (scrollbar, wheel and drag for free). Sending a prompt puts the view at the top, where
  it stays while the reply streams. A view scrolled to the bottom (and not at the top)
  sticks to the bottom as the document or the input grows.
- The prompt is plain wrapped text. The reply is rendered as Markdown by
  `src/tui/markdown.rs` (`pulldown-cmark`): headings, emphasis, strikethrough, inline
  and fenced code, block quotes, nested and task lists, links (text only, underlined),
  rules and tables, wrapped to the pane width. Colors come from the theme's `markup.*`
  scopes and attributes from the Markdown element; code blocks get a background and no
  syntax highlighting.
- A table is laid out as aligned columns under a bold, ruled header. Columns too wide
  for the pane shrink, widest first, and their cells wrap; no column shrinks below its
  longest word (capped at 12 columns). When even that does not fit, the table is
  stacked instead: one `Header: value` line per cell, with a rule between rows.
- The work log is shown dimmed between the prompt and the reply while the turn runs,
  and for any node that is not done or has an empty reply, so a failed or cancelled
  turn still shows what it did. A done node with a reply hides it; `Ctrl+O` shows or
  hides it either way, and the status line of a node that has one says so.
- Status line: running (the same spinner as in the tree, current tool, elapsed time),
  done (type, short hashes, stash or discarded scratch work, tokens), failed (error),
  cancelled, or the reason a send was refused.
- The input is always active, also while a turn runs: the next prompt can be drafted,
  but `Enter` is refused until the node is done. Drafts are kept per window and saved in
  the session.
- On send the window switches to the new node: the sent prompt appears on top, the reply
  streams below, the input is empty. The previous node is reachable through the tree
  only.
- Sending from a node that already has children forks silently.
- Mouse, over the prompt, the work log and the reply, with the gestures of the file
  preview. A drag starts on the word under the click and extends to the word under the
  cursor, across rows too: it never takes whole lines. It stays in the text it started
  in.
  - Drag — select; on release the selection is copied to the clipboard and cleared. What
    is copied is the source text (`prompt.md`, `reply.md`), so a wrapped paragraph is one
    line and Markdown markup inside the selection comes along.
  - `Shift`+drag — add a highlight. `Ctrl`+drag — remove what it covers.
  - Highlights are byte ranges of the node's prompt and reply, kept per node and saved
    in the session, so they survive a resize, closing the window and a restart. They
    are refused while the node's turn runs (its reply is still changing) and on the work
    log, and dropped when the node is deleted or run again in place.
  - Selected text is painted on the regular selection color, `ui.selection`, with its
    foreground auto-contrasted, like the file preview. A highlight is painted on the
    theme's `warning` color mixed at 40% over the pane background (`marker_bg`), keeping
    the markdown foreground colors — the point of the 40% mix. Helix themes have no
    search or highlight color, and their `ui.selection` is too close to the background
    in about half of them to mark a few words. The file preview keeps `ui.selection`.
- Keys:
  - `Enter` — send.
  - `Shift+Enter` — insert a newline.
  - `Ctrl+P` — Model Picker.
  - `Esc` — take the queued prompt back, else cancel the running turn, else nothing.
  - `Ctrl+Plus` / `Ctrl+Minus` — move the queued prompt earlier or later in the queue.
  - `Ctrl+O` — show or hide the work log.
  - `<leader>f` (`toggle-favorite`) — toggle the favorite of the node shown; nothing
    while the window has no node. A favorite node's title starts with `* `, in the
    title bar and in the window picker.
  - `PgUp` / `PgDn` — scroll the document.
  - All existing Emacs bindings of the input, `Ctrl+C` to clear it included. The window
    closes with `<leader>x`.

### 11.3 Input component

`InputLine` is reused and extended, for every user of it:

- soft wrapping at the pane width;
- `Up` / `Down` move the cursor between lines;
- a height cap (half the pane) with internal scrolling beyond it.

### 11.4 Model Picker window

`Window::ModelPicker`, unique, built like the theme picker (`FuzzyPicker` + `InputLine`,
nucleo matching over the model spec strings). `Esc` cancels. `Enter` depends on how the
picker was opened:

- from a Discussion window (`Ctrl+P`): sets that window's model (section 5) and returns
  focus to it. It never affects a turn already running.
- from the tree (`m`): creates the sibling described in section 9.

## 12. State, session, keymap

- `src/conversation.rs` — tree model (`Node`, `NodeId`), disk format, startup load,
  on-demand reads, write-through mutations, delete, table-of-contents pages. The node
  of a commit is found by scanning the nodes' metadata, which is fast enough not to
  need an index.
- `src/config.rs` — `agent` block; new `auth.kdl` loader; `load_commands` reads the
  command files.
- `src/tui/markdown.rs` — Markdown to theme-colored styled lines, for replies.
- `AppState` gains the tree, `discussions` (each with its model spec), the model list,
  the model last picked in this run and the running-turn handles, which are also the
  lock: a prompt is refused while there is one.
- `AppSignal` gains the tree-loaded, models-fetched, turn-event and title/summary-ready
  variants.
- `Action::OpenConversations`, slug `open-conversations`, default `<leader>c`.
- Session: `PaneEntry::ConversationTree`; `PaneEntry::Discussion { node, draft }`;
  expanded set; the highlights of nodes, by node id. No model is saved. The Model Picker is not saved. `SESSION_VERSION`
  3 → 4.
- New dependencies: `genai`; `reqwest` for the model-list fetch, which genai only does
  against a provider's default endpoint; `futures-util` to read genai's stream;
  `pulldown-cmark` to parse replies.

## 13. Error handling

- Endpoint errors (auth, rate limit, context length, network) end the turn as `failed`
  with the provider's message on the status line and in `metadata.md`. No automatic
  retry; the node is recovered with `Enter` in the tree.
- A tool error is returned to the model as the tool result, not raised.
- `claude` missing from `PATH`, or not logged in: the turn fails with that message.
- Config errors (unknown kind, missing url, malformed model spec) are reported like
  existing config errors at startup. A missing token is a warning, not an error.
- Disk write failures are shown on the status line; the in-memory node is kept.

## 14. Testing

Hermetic unit tests:

- model spec parsing, including model ids containing `/`;
- `auth.kdl` parsing; endpoint skipped when its token is missing;
- node directory naming and tree-wide collision suffix;
- tree load from a temp directory with missing files and an interrupted node; prompts
  and replies not retained;
- `metadata.md` round trip;
- lock: any running turn excludes every prompt;
- slash expansion: custom, unknown, path-like, `/compact`, `/commit`; command file
  parsing and the hint;
- context assembly: ancestor walk, compact cut-off, handover, repository-moved note,
  injected messages preserved;
- commit-turn closing on a temporary git repository: clean, nudge then clean, nudges
  exhausted then stash, amended history;
- feedback-turn closing: scratch work discarded, ignored files kept, a clean tree and a
  repository without commits left alone;
- recorder: the text of an answer that calls tools moves from the reply to the work
  log, with one line per tool call;
- Markdown rendering: styles, wrapping, nested and tight lists, code blocks;
- recover in place, sibling with another model, edit-as-sibling target;
- `conversation` pages: roots paging, path, straight-line segment cap, branches, commit
  lookup;
- tree row layout (indentation only at forks, selected node expanded with wrapped title
  and summary) and the delete arming state machine;
- model resolution: node's model when continuing, last pick for a new root, unset after
  startup;
- title and summary: answer parsing, title cut at 100 characters, reply cut and commits
  listed in the record sent;
- stream-json parsing from a recorded `claude` fixture;
- file tools refusing paths outside the root.

Real endpoints and the `claude` CLI are verified manually.

## 15. Future work

- Architecture-walking tool (MCP or native) so the agent explores structure without
  reading code.
- `conversation` tool for `claude-code` nodes.
- Export of the tree into the repository.
- Filter (`/`) in the tree; jump from a Git Log row to its node through the commit
  index.
- `$ARGUMENTS` placeholders and autocomplete for slash commands.
- Permission rules for `shell`.

## 16. Verified during implementation

- genai 0.6: a `ServiceTarget` (endpoint URL, token, `AdapterKind::OpenAI`, model) is
  passed directly to each call, tool calls and usage are captured at the end of the
  stream, and the effort is sent as `reasoning_effort`. With effort `default` genai
  still infers one from a model name ending in `-low`, `-high` and the like.
- The `claude` CLI (2.1): the forked session id arrives in the `system/init` event;
  `--resume` of an unknown session ends with an error result and no turn, which
  triggers the lossy handover; the attribution setting removes the commit trailer;
  `--tools ""` and
  `--no-session-persistence` serve titles and summaries. `--bare` must not be used: it
  disables the subscription login.

## 17. Open points

Defaults chosen without an explicit answer; change freely.

1. `default` was added to the effort set (`low` / `medium` / `high` was the answer) so
   that models rejecting an effort parameter stay usable.
2. Two nudges at most, then stash.
3. The nudge and the repository-moved note are kept in `transcript.jsonl` although
   hidden from the user (the request was to save the nudge nowhere).
4. `conversation` pages are generated on demand from memory rather than maintained as
   files.
5. Incomplete nodes cannot be continued; they are recovered, edited as a sibling, or
   re-run with another model.
6. `Enter` re-runs a **cancelled** node too, although it was stopped on purpose.
7. A pick made through `m` is not offered to new roots afterwards.
8. A commit node with no commits (nothing to change, or a question back) is valid.
9. `metadata.md` is flat `key: value` lines rather than KDL or JSON.
10. Slash expansion happens on send, not live in the input.
11. One `summary-model` for both titles and summaries, asked in one call after the
    turn; without it nodes keep the first line of their prompt as title.
12. A new root reuses the model last picked in this run instead of asking every time.
13. Untracked files count as a dirty tree, and a dirty tree refuses feedback prompts
    too: it is what makes the reset at the end of a feedback turn safe.
14. `AGENTS.md` is injected into the system prompt of the own loop.
15. `claude-code` runs with `--permission-mode bypassPermissions`.
16. A turn found dirty after a commit turn is stashed with
    `git stash push --include-untracked`; the stash of an earlier attempt of the same
    node is no longer referenced by its metadata once the node is run again.
17. Key choices: `<leader>c`, `n`, `f`, `c`, `Ctrl+O`; in the Discussion window `Esc`
    cancels or takes back and `Ctrl+C` only clears the input. The favorite of the node
    in focus is on `<leader>f` rather than `Ctrl+F`, which stays the input's forward-char.
18. Turns are serialized even when both are feedback turns, since each may use the
    working tree.
19. A commit made by a feedback turn is neither prevented nor undone; only the prompt
    forbids it.
20. The work log is kept out of `reply.md` rather than filtered at replay, and the
    reasoning of the model is not shown.
