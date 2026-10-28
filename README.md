# Explorer

A terminal workspace for one git repository, built for a way of working where an AI
agent writes most of the code and the developer reads it, understands it and decides
what comes next.

Explorer puts what that work needs into one program: tiled windows, terminals, a fast
code reader, the git history, and a coding agent whose conversations form a tree instead
of a line. It does not edit text. Your editor does that, in a pane.

> A personal tool in early development. It is shaped by its owner's workflow, it changes
> without notice, and nothing is kept compatible from one version to the next.

## Why

Development tools are built around the editor, because typing code used to be the work.
With an agent it no longer is. The time goes into reading what was written, following
how the pieces connect, judging a design and choosing the next step. The editor becomes
something you open for a minute, and everything around it becomes the main thing.

Explorer is that "everything around it", designed as one tool instead of a window
manager, tmux, a file manager, `tig` and an agent CLI held together by habit:

- **Reading comes first.** Opening a file, following a symbol through the repository,
  marking the lines that matter and reading a commit are the primary actions, and they
  are instant. This is the one thing Explorer takes from an IDE: a language server,
  semantic colors, hover, go-to-references. It takes none of the editor. Real editing is
  delegated to helix, vim or whatever `$EDITOR` is, and Explorer will not grow an editor
  of its own.
- **One repository, one workspace.** Explorer starts from the nearest `.git` and
  everything in it belongs to that repository: its files, its terminals, its history,
  its conversations.
- **The whole repository in memory.** Every text file is loaded at startup and kept up
  to date by a file watcher, so opening, searching and jumping never wait for the disk.
- **Back to work in a second.** The layout, the open files and their scroll positions,
  the terminals, the highlights, the drafts of unsent prompts: all of it is saved per
  repository and restored at the next start.
- **You decide, the agent executes.** The agent explains and advises until you have
  decided, and only then changes the code, in a commit that stays linked to the request
  behind it.

## What is in it

### The workspace

Everything is a window: a file, a terminal, a git log, a commit diff, a conversation.
Windows live in one stack, most recent first, and Explorer tiles the top of that stack
by itself: as many columns as fit at 100 cells minimum each, each window a full, a half,
a third or a quarter of a column. There are no splits to manage. You resize a window,
move it in the stack or send it to the back, and the layout follows. Windows that do not
fit stay open behind the others.

The window picker (`<leader>Space`) lists the open windows and, once you type, every
file of the repository by fuzzy name. It is the way to open a file and the way to get
back to anything.

### Reading code

A file opens as a read-only view with line numbers.

- **Search**: `/` and `?` search by regular expression, `n` and `N` move between
  matches.
- **Line highlights**: `Shift`+drag marks lines, `Ctrl`+drag unmarks them, and `:`
  edits the marked ranges as text (`12-40, 87`). The marks are saved with the session:
  they are the notes you leave while reading.
- **Symbol highlights**: double-click a symbol and every reference to it across the
  repository takes the same color, one color per symbol, also saved. Double-click it
  again to remove it. This is how a value or a type is followed through the code.
- **Selection**: drag to select and copy, by word; triple-click takes the line.
- **Editing**: `e` opens the file in `$VISUAL` or `$EDITOR` in a terminal window.
  `:!cmd` runs a command in a new terminal, with `%` standing for the file, `%dir` for
  its directory, `%crate` and `%repo` for the nearest `Cargo.toml` and `.git`, and
  `%project` for the name of the repository directory; `%%` is a literal `%`.

The syntax colors and the symbol references come from rust-analyzer, through an LSP
client in `src/lsp.rs`. That client is what an IDE's code intelligence runs on, and here
it serves reading only: Explorer asks for semantic tokens for coloring, hover to name
the symbol under the cursor, and references to find every use of it in the repository.
It asks for nothing else — no diagnostics, no completion, no rename — and only
rust-analyzer is wired in, so only Rust files get colors and references. Other files are
plain text. In a file window, `:all-features` and `:features ...` tell rust-analyzer
which cargo features to analyze.

### Terminals

`<leader>t` opens a shell in a window like any other, with real terminal emulation, a
scrollback, and mouse selection. Keys go to the shell while the terminal is grabbed;
scrolling releases it, and a click, `Enter` or `q` grabs it again. At the next start
every terminal is reopened in its directory, and a terminal that was started for a
command runs that command again. Processes do not survive quitting Explorer: there is
no detach.

### Git

`<leader>g` opens a log of HEAD, or of any branch or tag (`b`), with a live fuzzy filter
(`/`), branch and tag decorations and conventional-commit coloring. `Enter` opens the
diff of a commit. Several logs can be open at once. Status and blame are planned, to
finish replacing `tig`.

### The agent

`<leader>c` opens the Conversation Tree, and each node of it opens in a Discussion
window: the prompt, the reply rendered as Markdown, and an input for the next prompt.

**A tree, not a line.** Agent CLIs keep a conversation as one flat session. Work does
not go in a line: it starts from a framing prompt and splits into a feature here, a bug
there, a design question that leads nowhere. In Explorer every prompt is a node, and any
node can be continued, so any node can be forked. Going back to an earlier point and
taking another direction keeps the first one. The same prompt can be replayed with
another model as a sibling, for a second opinion. Each node gets a generated title and
summary, so the tree of a repository reads like the history of how it was thought out.

**Two kinds of turn.** A prompt is a *feedback* turn unless you say otherwise. The agent
is then a teacher: it explains, gives its opinion, disagrees when it should, and may
edit, build and run the tests to check what it claims. Whatever it changed is discarded
when the turn ends. Once the discussion has settled on something, `/commit` starts a
*commit* turn: the agent carries out what was decided and commits it. You choose which
one a prompt is; Explorer never guesses.

**Every change is a commit, and every commit has a prompt.** The tree records the
commits each node produced, so any line of code can be traced, through `git blame`, back
to the sentence that asked for it. The commits themselves stay clean: no trailer, no
link, no mention of a tool. Since everything the agent does either ends up in git or is
thrown away, it works without permission prompts. The price is two rules: a prompt needs
a clean working tree, and one turn runs at a time. Prompts sent meanwhile are queued.

**Run something when the agents are done.** `<leader>:` opens a command line over the
status bar. `!<command>` runs a command in a new terminal, and
`after-agents <command>` keeps one for later: once a turn has run and the whole queue
has finished, it runs in the background, with no terminal and no focus change, so it
cannot ask for input. A countdown (5 seconds by default) shows in the status bar first,
and any key press, paste or mouse event cancels it. `after-agents notify-send "agents
done"` is the typical use. The command is one-shot and kept in memory only: a new
`after-agents` replaces the pending one, a bare `after-agents` clears it, and a command
already running is killed when it is replaced or cleared, or when Explorer exits. `%`
placeholders work as in `:!` (plus `%project`, the name of the repository directory, which
needs no file) and are filled in when you press Enter. Aliases from the config
(`alias poweroff "systemctl poweroff"`) stand for a command: what you type after the
alias is appended to it as is.

**Plain files.** The tree is a directory tree of Markdown files, kept outside the
repository in `~/.local/share/explorer/conversations/`. It can be read, searched and
backed up without Explorer, and the agent itself can consult all of it, not only the
branch it is in, to find an earlier decision and the reason behind it.

**Any model.** An endpoint is an OpenAI-compatible server, OpenCode Go, or a Claude
subscription through the local `claude` CLI. The model is picked per prompt
(`<leader>m`) as `endpoint/model/effort`, and a branch can change model on the way. The
status bar shows how much of the subscription behind the focused conversation is used.

Also there: custom slash commands (one Markdown file each), `/compact` to summarize a
long branch and continue from the summary, a work log of the tools the agent called, and
highlights in prompts and replies that persist like the ones in files.

## Getting started

Requirements:

- Linux, and a terminal with mouse reporting.
- A nightly Rust toolchain. `flake.nix` provides one (`nix develop`).
- `git` on `PATH`.
- Optional: `rust-analyzer` for colors and symbol references in Rust code, and the
  `claude` CLI, logged in, for `claude-code` endpoints.

```sh
git clone --recursive https://github.com/cecton/explorer
cd explorer
cargo build --release
```

The `--recursive` matters: the TUI framework Explorer is built on is vendored as a
submodule in `vendor/r3bl-open-core`. Run the binary from anywhere inside a git
repository; `--theme <name>` overrides the theme.

The leader key is `` Alt+` ``. Press it and the status bar lists every shortcut of the
window in focus; `Esc` closes it without doing anything. Four of them are enough to
begin: `<leader>Space` for the window picker (files included), `<leader>t` for a
terminal, `<leader>g` for the git log, `<leader>c` for the conversation tree.

### Configuration

`~/.config/explorer/config.kdl`, all of it optional:

```kdl
theme "ayu_evolve"

keybindings {
    open-git-log "ctrl+g"
}

after-agents {
    grace-seconds 5
    alias poweroff "systemctl poweroff"
    alias notify "notify-send %project"
}

agent {
    endpoint "claude" {
        kind "claude-code"
        models "opus" "sonnet"
    }
    endpoint "kimi" {
        url "https://api.moonshot.ai/v1"
    }
    endpoint "opencode-go" {
        kind "opencode-go"
    }

    summary-model "kimi/kimi-k2/low"
}
```

- `theme`: one of the bundled Helix themes. `<leader>T` picks one with a live preview
  and saves it for the repository.
- `keybindings`: rebinds a shortcut by the name of its action (`open-git-log`,
  `send-to-back`, …). The names are listed at the top of `src/keymap.rs`.
- `after-agents`: `grace-seconds` is the time during which a key press, paste or mouse
  event cancels the command once the agents are done. The default is 5, and 0 runs it at once.
  Each `alias <name> "<command>"` line names a command for `after-agents`: the text typed
  after the name is appended unquoted, so `after-agents notify build done` runs
  `notify-send 'repo' build done`. An alias shadows a program of the same name.
- `agent`: the endpoints, and the model that writes the titles and summaries of the
  tree. Without `summary-model` a node is titled by the first line of its prompt.

Tokens stay out of the configuration, in `~/.local/share/explorer/auth.kdl`, one line
per endpoint name. A `claude-code` endpoint needs none.

```kdl
kimi "sk-..."
opencode-go "sk-..."
```

### Custom slash commands

A command is a Markdown file in `~/.config/explorer/commands/`, named after the command.
Typing `/name something` sends the file's prompt with `something` appended, so the file
can end mid-sentence. This is the `squash.md` in use here:

```markdown
---
system-prompt: off
argument: commit-like
description: Squash commits since a given ref into one with a new detailed message
---
Squash all commits into a single commit since <ref>. Discard the existing commit
messages. Generate a new detailed, structured commit message that summarizes all
changes made in the squashed commits. Use `git reset --soft <ref>` and `git commit` to
accomplish this. Squash all commits since:
```

`/squash HEAD~3` then reads as one instruction. The two metadata fields are what the
input hints at while the draft is only the command name: `<commit-like>` and the
description, so the command documents itself where it is used.

`system-prompt: off` is what makes this one work. The turn gets no mode prompt and no
workspace note, and it closes like a commit turn instead of being reset, so the
`git reset --soft` it runs sticks and the squashed commit is recorded to the node. The
flag is stored with the node, so replaying it later behaves the same. A command without
it is an ordinary feedback turn: the agent may try things, and they are discarded.

## Status

Explorer is used every day by its owner and by nobody else. Know this before trying it:

- The agent runs every tool call without asking, the shell included. What protects you
  is git, not a sandbox.
- Code intelligence is Rust only.
- There is no file tree: files are opened by name.
- Not built yet: git status and blame panes, a filter in the Conversation Tree, an
  export of the tree into the repository, and a tool for the agent to walk the
  architecture of a codebase without reading all of it.

## Further reading

- [`docs/superpowers/specs/2026-10-05-conversation-agent-design.md`](docs/superpowers/specs/2026-10-05-conversation-agent-design.md):
  the design of the agent, with the reasoning behind each decision.
- [`AGENTS.md`](AGENTS.md): the map of the source code, the build commands and the rules
  for working on it.
- [`prompts/`](prompts): the system prompts of the two modes.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in this work, as defined in the Apache-2.0 license, shall be dual licensed as
above, without any additional terms or conditions.

Two directories hold other people's work: `vendor/r3bl-open-core`, the TUI framework,
which is Apache-2.0, and `themes/`, copied from Helix, where each theme keeps its own
license and names it in its header when it has one.
