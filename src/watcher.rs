use camino::{Utf8Path, Utf8PathBuf};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::time::Duration;

const DEBOUNCE_MS: u64 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawKind {
    Created,
    Modified,
    Removed,
}

pub struct BatchedWatchEvent {
    pub modified: Vec<Utf8PathBuf>,
    pub created: Vec<Utf8PathBuf>,
    pub removed: Vec<Utf8PathBuf>,
}

pub fn start_watcher(root: &Utf8Path) -> mpsc::Receiver<BatchedWatchEvent> {
    let (batch_tx, batch_rx) = mpsc::channel(32);
    let (raw_tx, raw_rx) = std::sync::mpsc::channel::<(Utf8PathBuf, RawKind)>();
    // Bridge: std::thread does a blocking recv loop and forwards into tokio mpsc.
    let (bridge_tx, bridge_rx) = mpsc::channel::<(Utf8PathBuf, RawKind)>(128);

    let root_clone = root.to_owned();
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            let kind = match event.kind {
                EventKind::Create(_) => RawKind::Created,
                EventKind::Modify(_) => RawKind::Modified,
                EventKind::Remove(_) => RawKind::Removed,
                _ => return,
            };
            for path in event.paths {
                let Ok(utf8) = Utf8PathBuf::from_path_buf(path) else {
                    continue;
                };
                // Filter out .git and target subtrees.
                let Ok(rel) = utf8.strip_prefix(&root_clone) else {
                    continue;
                };
                let first = rel.components().next().map(|c| c.as_str());
                if matches!(first, Some(".git") | Some("target")) {
                    continue;
                }
                let _ = raw_tx.send((utf8, kind));
            }
        },
        notify::Config::default(),
    )
    .expect("failed to create watcher");

    watcher
        .watch(root.as_std_path(), RecursiveMode::Recursive)
        .expect("failed to watch root");

    // Blocking bridge thread: parks when no events, zero CPU when idle.
    std::thread::spawn(move || {
        while let Ok(event) = raw_rx.recv() {
            if bridge_tx.blocking_send(event).is_err() {
                break;
            }
        }
    });

    tokio::spawn(debounce_task(bridge_rx, batch_tx, watcher));

    batch_rx
}

async fn debounce_task(
    tokio_rx: mpsc::Receiver<(Utf8PathBuf, RawKind)>,
    batch_tx: mpsc::Sender<BatchedWatchEvent>,
    // Kept alive for the duration of the task.
    _watcher: RecommendedWatcher,
) {
    let mut pending: HashMap<Utf8PathBuf, RawKind> = HashMap::new();
    let mut tokio_rx = tokio_rx;

    loop {
        match tokio::time::timeout(Duration::from_millis(DEBOUNCE_MS), tokio_rx.recv()).await {
            Ok(Some((path, kind))) => {
                let entry = pending.entry(path).or_insert(kind);
                // Conflict resolution: removed wins; created+modified collapses to created.
                match (*entry, kind) {
                    (_, RawKind::Removed) => *entry = RawKind::Removed,
                    (RawKind::Created, RawKind::Modified) => {}
                    (RawKind::Modified, RawKind::Created) => *entry = RawKind::Created,
                    _ => *entry = kind,
                }
            }
            Ok(None) => return,
            Err(_timeout) => {
                if pending.is_empty() {
                    continue;
                }
                let mut batch = BatchedWatchEvent {
                    modified: vec![],
                    created: vec![],
                    removed: vec![],
                };
                for (path, kind) in pending.drain() {
                    match kind {
                        RawKind::Created => batch.created.push(path),
                        RawKind::Modified => batch.modified.push(path),
                        RawKind::Removed => batch.removed.push(path),
                    }
                }
                if batch_tx.send(batch).await.is_err() {
                    return;
                }
            }
        }
    }
}
