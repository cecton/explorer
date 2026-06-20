use crate::tui::AppSignal;
use camino::Utf8PathBuf;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use r3bl_tui::{Continuation, RRT, RRTEvent, RRTSoftwareInterrupt, RRTWorker, RestartPolicy};
use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::mpsc;
use std::time::Duration;
use tokio::sync::broadcast::Sender;

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

pub struct WatcherWorker {
    root: Utf8PathBuf,
    rx: mpsc::Receiver<notify::Result<notify::Event>>,
    pending: HashMap<Utf8PathBuf, RawKind>,
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
    type Config = Utf8PathBuf;
    type Output = AppSignal;
    type Interrupt = NoOpInterrupt;

    fn create_and_register_os_sources(
        config: Self::Config,
        _receiver: tokio::sync::broadcast::Receiver<Self::Input>,
    ) -> miette::Result<(Self, Self::Interrupt)> {
        let root = config;

        let (tx, rx) = mpsc::channel();
        let mut watcher = RecommendedWatcher::new(tx, notify::Config::default())
            .map_err(|e| miette::miette!("notify watcher error: {e}"))?;
        watcher
            .watch(root.as_std_path(), RecursiveMode::Recursive)
            .map_err(|e| miette::miette!("notify watch error: {e}"))?;

        Ok((
            Self {
                root,
                rx,
                pending: HashMap::new(),
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
        sender: &Sender<RRTEvent<Self::Output>>,
    ) -> Continuation {
        match self.rx.recv_timeout(DEBOUNCE) {
            Ok(Ok(event)) => {
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
                    if matches!(first, Some(".git") | Some("target")) {
                        continue;
                    }
                    let entry = self.pending.entry(utf8).or_insert(kind);
                    match (*entry, kind) {
                        (_, RawKind::Removed) => *entry = RawKind::Removed,
                        (RawKind::Created, RawKind::Modified) => {}
                        (RawKind::Modified, RawKind::Created) => *entry = RawKind::Created,
                        _ => *entry = kind,
                    }
                }
                Continuation::Continue
            }
            Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => Continuation::Restart,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if self.pending.is_empty() {
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
                let signal = AppSignal::FilesChanged(std::sync::Arc::new(batch));
                if sender.send(RRTEvent::Worker(signal)).is_err() {
                    return Continuation::Stop;
                }
                Continuation::Continue
            }
        }
    }
}
