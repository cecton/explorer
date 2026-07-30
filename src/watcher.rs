use crate::loader::{LoadedFile, walk_repo_files};
use crate::tui::AppSignal;
use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use r3bl_tui::{
    Continuation, RRT, RRTEvent, RRTSoftwareInterrupt, RRTWorker, RestartPolicy,
    TerminalWindowMainThreadSignal,
};
use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;

const DEBOUNCE: Duration = Duration::from_millis(50);

pub static WATCHER_RRT: RRT<WatcherWorker> = RRT::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawKind {
    Created,
    Modified,
    Removed,
}

#[derive(Debug, Clone)]
pub struct BatchedWatchEvent {
    pub modified: Vec<Utf8PathBuf>,
    pub created: Vec<Utf8PathBuf>,
    pub removed: Vec<Utf8PathBuf>,
}

#[derive(Debug)]
pub struct NoOpInterrupt;

impl RRTSoftwareInterrupt for NoOpInterrupt {
    fn trigger_software_interrupt(&self) {}
}

#[derive(Clone)]
pub struct WatcherConfig {
    pub root: Utf8PathBuf,
    pub app_tx: tokio_mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
    pub files: Arc<ArcSwap<Vec<Arc<LoadedFile>>>>,
}

impl Debug for WatcherConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatcherConfig")
            .field("root", &self.root)
            .finish()
    }
}

pub struct WatcherWorker {
    root: Utf8PathBuf,
    app_tx: tokio_mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
    files: Arc<ArcSwap<Vec<Arc<LoadedFile>>>>,
    rx: mpsc::Receiver<notify::Result<notify::Event>>,
    pending: HashMap<Utf8PathBuf, RawKind>,
    // Set when notify reports an event-queue overflow (`need_rescan`). On the
    // next flush we re-walk the tree and reconcile, recovering dropped events.
    pending_rescan: bool,
    // Held to keep the watcher alive for the duration of the worker.
    _watcher: RecommendedWatcher,
}

impl Debug for WatcherWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatcherWorker")
            .field("root", &self.root)
            .finish()
    }
}

impl RRTWorker for WatcherWorker {
    type Config = WatcherConfig;
    type Output = AppSignal;
    type Interrupt = NoOpInterrupt;

    fn create_and_register_os_sources(
        config: Self::Config,
        _receiver: tokio::sync::broadcast::Receiver<Self::Input>,
    ) -> miette::Result<(Self, Self::Interrupt)> {
        let WatcherConfig {
            root,
            app_tx,
            files,
        } = config;

        let (tx, rx) = mpsc::channel();
        let mut watcher = RecommendedWatcher::new(tx, notify::Config::default())
            .map_err(|e| miette::miette!("notify watcher error: {e}"))?;
        watcher
            .watch(root.as_std_path(), RecursiveMode::Recursive)
            .map_err(|e| miette::miette!("notify watch error: {e}"))?;
        tracing::info!("watcher created: root={}", root);

        Ok((
            Self {
                root,
                app_tx,
                files,
                rx,
                pending: HashMap::new(),
                pending_rescan: false,
                _watcher: watcher,
            },
            NoOpInterrupt,
        ))
    }

    fn restart_policy() -> RestartPolicy {
        RestartPolicy::default()
    }

    fn block_until_ready_then_dispatch(
        &mut self,
        _sender: &tokio::sync::broadcast::Sender<RRTEvent<Self::Output>>,
    ) -> Continuation {
        match self.rx.recv_timeout(DEBOUNCE) {
            Ok(Ok(event)) => {
                // Event-queue overflow: the kernel dropped events, so per-path
                // deltas are unreliable. Flag a full rescan on the next flush
                // instead of trying to interpret this event's paths.
                if event.need_rescan() {
                    tracing::warn!("watcher: event-queue overflow, scheduling rescan");
                    self.pending_rescan = true;
                    return Continuation::Continue;
                }
                let kind = match event.kind {
                    EventKind::Create(_) => RawKind::Created,
                    EventKind::Modify(_) => RawKind::Modified,
                    EventKind::Remove(_) => RawKind::Removed,
                    _ => return Continuation::Continue,
                };
                for path in event.paths {
                    let Ok(utf8) = Utf8PathBuf::from_path_buf(path) else {
                        continue;
                    };
                    let Ok(rel) = utf8.strip_prefix(&self.root) else {
                        continue;
                    };
                    let first = rel.components().next().map(|c| c.as_str());
                    if first.is_some_and(|f| crate::loader::IGNORED_DIRS.contains(&f)) {
                        continue;
                    }
                    let entry = self.pending.entry(utf8).or_insert(kind);
                    // Created+Removed or Removed+Created within one debounce window
                    // means an atomic replace (e.g. editor save strategy); treat as Modified.
                    match (*entry, kind) {
                        (RawKind::Created, RawKind::Removed) => *entry = RawKind::Modified,
                        (RawKind::Removed, RawKind::Created) => *entry = RawKind::Modified,
                        (_, RawKind::Removed) => *entry = RawKind::Removed,
                        (RawKind::Created, RawKind::Modified) => {}
                        (RawKind::Modified, RawKind::Created) => {}
                        _ => *entry = kind,
                    }
                }
                Continuation::Continue
            }
            Ok(Err(e)) => {
                tracing::warn!("watcher: notify error: {:?}", e);
                Continuation::Restart
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                tracing::warn!("watcher: channel disconnected, restarting");
                Continuation::Restart
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if self.pending.is_empty() && !self.pending_rescan {
                    return Continuation::Continue;
                }
                let mut batch = BatchedWatchEvent {
                    modified: vec![],
                    created: vec![],
                    removed: vec![],
                };
                for (path, kind) in self.pending.drain() {
                    match kind {
                        RawKind::Created => batch.created.push(path),
                        RawKind::Modified => batch.modified.push(path),
                        RawKind::Removed => batch.removed.push(path),
                    }
                }
                if self.pending_rescan {
                    self.pending_rescan = false;
                    self.reconcile_into(&mut batch);
                }
                tracing::debug!(
                    "watcher: dispatching batch: {} modified, {} created, {} removed",
                    batch.modified.len(),
                    batch.created.len(),
                    batch.removed.len()
                );
                let signal = AppSignal::FilesChanged(std::sync::Arc::new(batch));
                if self
                    .app_tx
                    .blocking_send(TerminalWindowMainThreadSignal::ApplyAppSignal(signal))
                    .is_err()
                {
                    tracing::warn!("watcher: app_tx closed, stopping");
                    return Continuation::Stop;
                }
                Continuation::Continue
            }
        }
    }
}

impl WatcherWorker {
    /// Re-walk the tree and merge the delta versus the current file list into
    /// `batch`. Called after an inotify overflow to recover dropped events:
    /// files on disk but not active in the list become `created` (the app
    /// handler resurrects a `removed` entry or appends a genuinely-new one),
    /// and active entries whose path is gone from disk become `removed`.
    fn reconcile_into(&self, batch: &mut BatchedWatchEvent) {
        let on_disk: HashSet<Utf8PathBuf> = walk_repo_files(&self.root)
            .into_iter()
            .filter_map(|p| Utf8PathBuf::from_path_buf(p).ok())
            .collect();

        let snapshot = self.files.load();

        // Paths already accounted for by this batch's coalesced events, so the
        // rescan delta doesn't duplicate them.
        let mut seen: HashSet<Utf8PathBuf> = batch
            .created
            .iter()
            .chain(batch.modified.iter())
            .chain(batch.removed.iter())
            .cloned()
            .collect();

        // Active (non-removed) paths currently in the list.
        let active: HashSet<&Utf8PathBuf> = snapshot
            .iter()
            .filter(|f| !f.removed.load(Ordering::Relaxed))
            .map(|f| &f.path)
            .collect();

        // On disk but not active in the list -> created (new or resurrect).
        for path in &on_disk {
            if !active.contains(path) && seen.insert(path.clone()) {
                batch.created.push(path.clone());
            }
        }

        // Active in the list but gone from disk -> removed.
        for file in snapshot.iter() {
            if file.removed.load(Ordering::Relaxed) {
                continue;
            }
            if !on_disk.contains(&file.path) && seen.insert(file.path.clone()) {
                batch.removed.push(file.path.clone());
            }
        }

        tracing::info!(
            "watcher: rescan reconciled {} on-disk files -> {} created, {} removed",
            on_disk.len(),
            batch.created.len(),
            batch.removed.len()
        );
    }
}
