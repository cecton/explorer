use std::sync::Arc;

/// The files a watcher batch touched, by kind, as by the keys the file list holds them
/// under: what the symbol groups check themselves against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilesChanged {
    pub modified: Vec<crate::loader::FileKey>,
    pub created: Vec<crate::loader::FileKey>,
    pub removed: Vec<crate::loader::FileKey>,
}

/// The answer to a symbol request: where a name is defined and everywhere it is used.
#[derive(Clone, Debug)]
pub struct SymbolHighlight {
    pub qualified_name: String,
    pub group_id: Option<usize>,
    pub origin: crate::loader::FileKey,
    pub origin_line: u32,
    pub origin_char: u32,
    pub origin_word: Option<String>,
    pub origin_locations: Vec<SymbolRefLocation>,
    pub reference_locations: Vec<SymbolRefLocation>,
}

/// One place a symbol appears, as byte offsets into a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SymbolRefLocation {
    pub file_key: crate::loader::FileKey,
    pub start_byte: usize,
    pub end_byte: usize,
}

/// The channel every thread sends the main loop's signals through.
pub type MainTx = tokio::sync::mpsc::Sender<r3bl_tui::TerminalWindowMainThreadSignal<AppSignal>>;

/// Awaits the send in a task of its own, so the caller never blocks on a full channel.
pub fn send(tx: &MainTx, signal: AppSignal) {
    let tx = tx.clone();
    tokio::spawn(async move {
        let _ = tx
            .send(r3bl_tui::TerminalWindowMainThreadSignal::ApplyAppSignal(
                signal,
            ))
            .await;
    });
}

/// Drops the signal rather than wait for room: for periodic and best-effort ones.
pub fn try_send(tx: &MainTx, signal: AppSignal) {
    let _ = tx.try_send(r3bl_tui::TerminalWindowMainThreadSignal::ApplyAppSignal(
        signal,
    ));
}

#[derive(Default, Clone, Debug)]
#[non_exhaustive]
pub enum AppSignal {
    /// What a watcher batch read, for the main thread to publish: the reads happened on the
    /// watcher's own thread, and the swap-in invalidates what they made stale.
    FilesChanged(Arc<crate::loader::watch::Changes>),
    /// Open a terminal pane. `cmd = None` means an interactive shell; `cmd = Some(s)` runs
    /// `/bin/sh -c s`. `cwd` is the working directory for the child process.
    OpenTerminal {
        cmd: Option<String>,
        cwd: camino::Utf8PathBuf,
    },
    /// Open an embedded editor terminal that replaces the FilePreview in-place.
    OpenEditor {
        cmd: String,
        cwd: camino::Utf8PathBuf,
        file_key: crate::loader::FileKey,
    },
    SymbolHighlight(SymbolHighlight),
    /// A terminal pane produced output and should be re-painted. A flooding pane drives
    /// this many times a second, so it does nothing but render.
    TerminalOutput,
    /// Change which Cargo features rust-analyzer analyzes. rust-analyzer doesn't
    /// hot-reload `cargo.*` settings, so applying this sends `LspInput::Restart` and the
    /// worker replaces its own process, keeping the old one on a failed handshake.
    SetCargoFeatures(crate::config::CargoFeatureSelection),
    /// Result of an async `git::load_log` run for git log window `id`.
    GitLogLoaded {
        id: usize,
        result: Result<Arc<crate::git::LogHistory>, String>,
    },
    /// Result of an async `git::load_refs` run (branch picker list).
    GitRefsLoaded(Result<Arc<Vec<crate::git::GitRef>>, String>),
    /// Result of an async `git::load_commit_diff` run for `id`.
    CommitDiffLoaded {
        id: [u8; 20],
        result: Result<Arc<Vec<crate::git::DiffLine>>, String>,
    },
    /// Result of an async `git::is_dirty` check.
    DirtyStatusLoaded(Result<bool, String>),
    /// The watcher saw `.git/HEAD`, `.git/refs/**` or `.git/packed-refs` change:
    /// the git history or the refs moved since the last knowledge of it, the git
    /// log windows and the branch picker list are stale until reloaded.
    GitHistoryChanged,
    /// The startup load filled `AppState::conversations`.
    ConversationsLoaded,
    /// Result of fetching the model list of `endpoint`.
    ModelsFetched {
        endpoint: String,
        result: Result<Vec<String>, String>,
    },
    /// How much of the spending behind `endpoint` is used: as known when a turn that
    /// spent some of it ended, or as just polled for an OpenRouter key.
    UsageFetched {
        endpoint: String,
        usage: crate::usage::Usage,
    },
    /// The shared spinner advances one frame (`SPINNER_INTERVAL`); renders only while
    /// a turn runs.
    TurnTick,
    /// The turn of `node` started a sub-agent, already recorded in the tree.
    SubagentStarted {
        node: crate::conversation::NodeId,
    },
    /// Sub-agent run `index` of `node` ended; its outcome is in the tree.
    SubagentFinished {
        node: crate::conversation::NodeId,
        index: u32,
    },
    /// The MCP server `name` started, failed or is starting again.
    McpStatus {
        name: String,
    },
    /// The `after-agents` command of run `run` ended.
    AfterAgentsExited {
        run: u64,
    },
    /// The turn of `node` ended.
    TurnFinished {
        node: crate::conversation::NodeId,
        outcome: crate::conversation::metadata::TurnOutcome,
    },
    /// `title.md` of `node` was written.
    NodeTitleReady {
        node: crate::conversation::NodeId,
        title: String,
    },
    /// The new attempt at the title and summary of `node` ended, whatever its outcome.
    NodeRetitleEnded {
        node: crate::conversation::NodeId,
    },
    /// `title.md` of sub-agent run `index` of `node` was written.
    AgentTitleReady {
        node: crate::conversation::NodeId,
        index: u32,
        title: String,
    },
    /// `summary.md` of `node` was written.
    NodeSummaryReady {
        node: crate::conversation::NodeId,
        summary: String,
    },
    /// Prompt and reply of `node`, read from disk for discussion window `window_id`.
    NodeContentLoaded {
        window_id: usize,
        node: crate::conversation::NodeId,
        prompt: String,
        reply: String,
        work_log: Vec<String>,
    },
    /// Repaint, nothing else.
    #[default]
    Redraw,
    /// Every second: ticks the `after-agents` countdown, and repaints only when the clock
    /// minute changed, a countdown is active or a terminal pane has output no signal
    /// announced.
    Tick,
    /// Every 5 seconds: session save, working-tree dirty check, LSP health log. Never
    /// repaints; the results come back as their own signals.
    Housekeeping,
    /// Every minute: polls what the OpenRouter keys have spent. Never repaints.
    UsagePollDue,
}
