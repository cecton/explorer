# Conversation Agent — Design

Date: 2026-10-05
Status: draft

## Goal

An agentic AI chat inside Explorer whose history is a **tree**, not a flat session. Every
user prompt is a node; any node can be continued, so any node can be forked. The tree is
stored as plain files, survives restarts, and links each node to the git commits the
agent produced for it.

Every prompt is sent in one of two modes chosen by the user: **feedback** (the agent
teaches and advises, changes nothing) or **commit** (the agent executes an instruction
and commits the result).

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
  one. It is invited to be talkative. It changes nothing in the repository. (opencode
  calls the equivalent mode "plan".)
- **commit — the executor.** The reasoning has been done. The agent follows the
  instruction as given, speaks up only for an unforeseen hurdle, and commits. (opencode:
  "build".)

The user picks the mode for each prompt; Explorer never guesses it.

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
| Mode | Chosen by the user per prompt with `Shift+Tab`, saved in the node, and selects the system prompt, the tools and the git rules. |
| Fork versus git | Forks are conversation-only. The repository stays at current HEAD; the agent is told which commits happened since the node it continues from. |
| Context replay | Full transcript (tool calls and results) stored per node and replayed exactly. |
| Backend switch | Allowed. Crossing between `claude-code` and `openai` replays ancestors as prompt and reply text only (lossy handover). |
| Storage | `~/.local/share/explorer/conversations/<repo-hash>/`, never inside the repository. Tokens in `~/.local/share/explorer/auth.kdl`. |
| Committer | The agent commits; Explorer verifies after the turn and records the hashes. Amend and rebase are allowed. |
| Dirty tree | A **commit** prompt is refused while the working tree has uncommitted changes. Feedback prompts are never refused for this. |
| Forgotten commit | Explorer stays in the loop and sends a hidden nudge asking the agent to commit. |
| Leftovers | Only when nudging is impossible or exhausted, Explorer stashes what is left and flags the node. |
| Locking | Like a read-write lock per repository: one commit turn excludes everything; any number of feedback turns may run together. |
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
| `metadata.md` | Flat `key: value` lines, see below. | On send; rewritten when the turn ends and when favorite toggles. |
| `reply.md` | All assistant text of the turn, in order, without tool calls. | Flushed about once per second while streaming, and at the end. |
| `transcript.jsonl` | One message per line, exactly as replayed: the user message as sent, assistant messages with tool calls, tool results, and any message Explorer injected. Absent for `claude-code` nodes. | Appended as each message completes. |
| `title.md` | One line, at most 100 characters (a display limit). | When the title call returns. |
| `summary.md` | About 50 words summarizing the prompt; shorter for short prompts. | When the summary call returns. |

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
favorite: true                only when set
command: review               only when a slash command was expanded
session-id: <uuid>            claude-code only
tokens-in: 84210
error: <one line>             only for failed
```

`type` is the mode the prompt was sent in, or `compact` for the third kind of node
(section 10). A commit node may have no commits when the agent had nothing to change.

Rules:

- **Startup** reads, once and off the UI thread, every node's `metadata.md`, `title.md`
  and `summary.md`, and builds the tree plus a commit-hash → node index. `prompt.md`,
  `reply.md` and `transcript.jsonl` are **never** kept for the whole tree: they are read
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

    command "review" {
        prompt "Review the change described below for correctness and style."
    }
}
```

- `endpoint "<name>"` — the name is the first segment of a model spec and must not
  contain `/`.
  - `kind` — `openai` (default) or `claude-code`.
  - `url` — `openai` only: base URL to which `/chat/completions` and `/models` are
    appended.
  - `models` — `claude-code` only, since the CLI has no model-list route. Defaults to
    `opus`, `sonnet`, `haiku`.
- `summary-model` — model spec used for both titles and summaries. If absent, neither is
  generated.
- `command "<name>" { prompt "…" }` — custom slash command, see section 10.

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
| Tools (own loop) | `read`, `glob`, `grep`, `shell`, `conversation` | all of them, plus `write` and `edit` |
| Dirty tree on send | allowed | refused |
| Lock | shared | exclusive |
| After the turn | no git check | verify, nudge, record commits |

- `Shift+Tab` in a Discussion window toggles the mode of the prompt being written. The
  mode is shown as the input's label (`feedback>` / `commit>`, colored).
- A window starts in the mode of the node it shows (a `compact` node counts as
  feedback); a new root starts in feedback. The window's mode is saved in the session.
- In feedback mode the shell is available so the agent can run a build, tests or
  `git log`; nothing enforces that it stays read-only beyond the system prompt and the
  missing `write` and `edit` tools.

### Locking

One lock per repository, with read-write semantics:

- a **commit** turn needs the lock exclusively: it cannot start while any turn runs, and
  while it runs every other send is refused;
- **feedback** and `/compact` turns share the lock: any number may run together, and
  they block only commit turns.

A refused send keeps the input and shows the reason on the status line.

## 7. Backends

New module tree `src/agent/`:

- `mod.rs` — `TurnRequest`, `TurnEvent`, dispatch on endpoint kind.
- `llm.rs` — the only file that imports `genai`; converts between our message type and
  genai's, resolves endpoint URL and token per call.
- `agent_loop.rs` — Explorer's own loop for `openai` endpoints.
- `tools.rs` — tool definitions and execution.
- `claude_code.rs` — `claude` subprocess driver.
- `context.rs` — builds the message list for a node from its ancestors.

A turn emits `TurnEvent`s to the UI through `AppSignal`: `TextDelta`, `ToolStarted`,
`ToolFinished`, `Usage`, `Finished`, `Failed`.

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
| `write` | Create or overwrite a file. Commit mode only. |
| `edit` | Replace an exact string in a file; fails if the string is absent or ambiguous. Commit mode only. |
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
       --permission-mode bypassPermissions \
       --settings '{"attribution":{"commit":"","pr":""}}' \
       [--disallowedTools "Edit Write NotebookEdit"]        feedback mode
       [--resume <parent session-id> --fork-session]         child nodes
```

- The prompt is written to stdin. Text deltas, tool activity, usage and the final result
  are parsed from the stream-json events. The new session id is read from the stream and
  stored in `metadata.md`.
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
   opens); the node shown is `done` (or the window is a new root); the lock can be taken
   in the prompt's mode; in commit mode the working tree is clean, untracked non-ignored
   files included.
2. **Expand** a slash command if the prompt starts with one.
3. **Persist.** Create the node directory; write `prompt.md` and `metadata.md`
   (`type`, `status: running`, `head-before`). The window now shows the new node and the
   tree selection follows it.
4. **Start**, in parallel: the title and summary calls, and the turn itself.
5. **Stream.** Reply text is flushed to `reply.md`; transcript lines are appended as
   messages complete.
6. **Close**, by mode:
   - **feedback / compact**: record `head-after`; nothing else.
   - **commit**: see 8.1.
7. **Finish.** Rewrite `metadata.md`, release the lock; open Git Log windows and the
   dirty flag refresh.

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

**Cancel.** `Ctrl+C` in the Discussion window of a running node aborts the HTTP stream or
kills the child process group (including a running `shell` command). Status becomes
`cancelled`; the partial reply is kept; a commit turn is closed as described above.

**Quit.** Quitting Explorer cancels running turns the same way. A running turn is not
resumable and is not part of session restore.

**Title and summary.** Both describe the prompt, not the reply. Each is one non-streaming
call to `summary-model` without tools (`prompts/title.md`, `prompts/summary.md`; on
`claude-code`, `claude -p --tools ""`). A title is one line of at most 100 characters,
a limit set for display; a longer answer is cut at 100. A summary is about 50 words. A
single-line prompt of 100 characters or fewer is its own title, and a prompt of 50 words
or fewer is its own summary; no call is made. Failures are silent: the files stay
missing.

## 9. Recovering, replaying and editing nodes

A node whose status is not `done` (failed, cancelled, interrupted) is **incomplete**.
Incomplete nodes are always leaves: a prompt cannot be sent from one. Three tree keys
deal with them and with second opinions:

- **`Enter` — open, and recover if incomplete.** Opens the node in a Discussion window.
  If the node is incomplete, its turn is run again **in place**: same directory, same
  prompt, same mode, and the endpoint, model and effort it was originally asked with.
  `reply.md` and `transcript.jsonl` start over. The guards of section 8 apply; if one
  fails (dirty tree, lock, endpoint or model no longer available) the window opens on
  the old partial reply and shows the reason.
- **`m` — same prompt, another model.** Opens the Model Picker; on `Enter` Explorer
  creates a **sibling** of the node with the same prompt and mode, run with the chosen
  model spec, and opens it. The pick belongs to that sibling only; it does not become
  the model offered to new roots. Works on any node, complete or not.
- **`e` — edit as a new sibling.** Opens a Discussion window on the node's **parent**
  with the input prefilled with the node's prompt and its mode selected. Nothing is
  created until the user sends. On a root, this is a new-root window.

Since forks are conversation-only, a sibling of a commit node runs on a repository that
already contains the first node's commits; the repository-moved note tells the agent so.
Comparing two models on the same code change requires the user to reset the branch
first.

## 10. Slash commands

A prompt that starts with `/name`, where `name` is `[a-z0-9-]+` followed by whitespace or
the end of the text, is looked up among the commands:

- **Custom command** (config): the prompt becomes the command's base prompt, then two
  newlines, then the rest of the typed text. With no rest, just the base prompt.
  Expansion happens on send; `prompt.md` stores the expanded text and `metadata.md` the
  command name.
- **Unknown name**: the text is sent as typed. A prompt beginning with a path such as
  `/home/…` is therefore harmless.
- **`/compact [instructions]`** (built-in, cannot be overridden): creates a child node of
  type `compact`, whatever the mode selected. Its turn is one call to the window's model
  with the branch so far as context and `prompts/compact.md` as instruction, plus any
  typed instructions; no tools, shared lock. Its `reply.md` is the summary. Descendants
  of a compact node replay context starting from it, with that summary as the opening
  message. On `claude-code`, the summary is produced in a fork of the parent session and
  descendants start a fresh session from it.

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
- **Layout of the tree**: roots are listed newest first; inside a conversation, nodes run
  oldest first. A node with a single child keeps that child at the **same** indentation,
  so a straight line of prompts reads as a flat list. Indentation grows only below a node
  with several children, one level per fork. Without this rule a 50-prompt conversation
  would be indented 50 levels.
- Feedback and commit nodes have distinct colors. Other states get a color and a
  one-character ASCII marker: running, failed, cancelled, uncommitted (stashed), compact,
  handover. Favorites are highlighted across the whole row.
- Keys:
  - `Up` `Down` `PgUp` `PgDn` `Home` `End` — move the selection.
  - Mouse click — moves the selection only. Double click — same as `Enter`.
  - `Left` / `Right` — collapse / expand a root or a fork. The expanded set is saved in
    the session.
  - `Enter` — open the node; recover it if incomplete (section 9).
  - `m` — same prompt with another model (section 9).
  - `e` — edit as a new sibling (section 9).
  - `n` — new root: opens a Discussion window with no node, only the input.
  - `f` — toggle favorite.
  - `c` — open Commit Diff windows for the node's commits (existing `Window::CommitDiff`).
  - `Ctrl+D` — first press arms deletion of the node and its descendants and the status
    bar shows the count; a second `Ctrl+D` on the same node deletes. Any other key or a
    selection change disarms.
- **Deletion** removes the directories and closes Discussion windows showing deleted
  nodes. Commits and stashes are not touched. A running node and its ancestors cannot be
  deleted.

### 11.2 Discussion window

`Window::Discussion(u16)`, state in `AppState.discussions`; several can be open. A window
holds its node's prompt and reply in memory only while it is open.

- Top to bottom: title row (node title, the window's model spec on the right); the node's
  prompt, visually set apart; the reply; a status line; the input with its mode label.
- Prompt and reply form one scrollable document owned by the PaneManager (scrollbar,
  wheel and drag for free). The reply is highlighted as Markdown with the existing
  syntect machinery. While streaming, the view follows the end unless the user scrolled
  up.
- Status line: running (current tool, elapsed time), done (type, short hashes, tokens),
  failed (error), cancelled, or the reason a send was refused.
- The input is always active, also while a turn runs: the next prompt can be drafted,
  but `Enter` is refused until the node is done. Drafts are kept per window and saved in
  the session.
- On send the window switches to the new node: the sent prompt appears on top, the reply
  streams below, the input is empty. The previous node is reachable through the tree
  only.
- Sending from a node that already has children forks silently.
- Keys:
  - `Enter` — send.
  - `Shift+Enter` — insert a newline.
  - `Shift+Tab` — toggle feedback / commit. `Shift+Tab` is globally bound to
    `focus-prev`; while a Discussion window is focused the window takes it, and
    `<leader>Shift+Tab` still moves focus.
  - `Ctrl+P` — Model Picker.
  - `Ctrl+C` — cancel the running turn.
  - `PgUp` / `PgDn` — scroll the document.
  - All existing Emacs bindings of the input. `Esc` does nothing; the window closes with
    `<leader>x`.

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

- `src/conversation.rs` — tree model (`Node`, `NodeId`, commit index), disk format,
  startup load, on-demand reads, write-through mutations, delete.
- `src/config.rs` — `agent` block; new `auth.kdl` loader.
- `AppState` gains the tree, `discussions` (each with its model spec), the model list,
  the model last picked in this run, the repository lock and the running-turn handles.
- `AppSignal` gains the tree-loaded, models-fetched, turn-event and title/summary-ready
  variants.
- `Action::OpenConversations`, slug `open-conversations`, default `<leader>c`.
- Session: `PaneEntry::ConversationTree`; `PaneEntry::Discussion { node, draft, mode }`;
  expanded set. No model is saved. The Model Picker is not saved. `SESSION_VERSION`
  3 → 4.
- New dependency: `genai`. `reqwest` directly only if genai does not expose what the
  model-list fetch needs.

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
- lock: exclusive commit, shared feedback, refusal reasons;
- slash expansion: custom, unknown, path-like, `/compact`;
- context assembly: ancestor walk, compact cut-off, handover, repository-moved note,
  injected messages preserved;
- commit-turn closing on a temporary git repository: clean, nudge then clean, nudges
  exhausted then stash, amended history;
- recover in place, sibling with another model, edit-as-sibling target;
- `conversation` pages: roots paging, path, straight-line segment cap, branches, commit
  lookup;
- tree row layout (indentation only at forks, selected node expanded with wrapped title
  and summary) and the delete arming state machine;
- model resolution: node's model when continuing, last pick for a new root, unset after
  startup;
- title limit: cut at 100 characters, short single-line prompt reused;
- stream-json parsing from a recorded `claude` fixture;
- file tools refusing paths outside the root; `write` and `edit` absent in feedback
  mode.

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

## 16. To verify during planning

- genai's support for custom endpoints, effort and streamed tool calls was read from its
  README, not exercised.
- The exact `claude` flag combination in 7.4: reading the forked session id from the
  stream, the attribution setting, the tool names to disallow in feedback mode, and
  resuming a node's own session for a nudge.

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
11. One `summary-model` for both titles and summaries; short prompts skip the call.
12. A new root reuses the model last picked in this run instead of asking every time.
13. Untracked files count as a dirty tree.
14. `AGENTS.md` is injected into the system prompt of the own loop.
15. `claude-code` runs with `--permission-mode bypassPermissions`.
16. Key choices: `<leader>c`, `n`, `f`, `c`, `Ctrl+C`; `Esc` inert in the Discussion
    window.
