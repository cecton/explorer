use camino::{Utf8Path, Utf8PathBuf};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant, sleep_until};

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

    tokio::spawn(debounce_task(raw_rx, batch_tx, watcher));

    batch_rx
}

async fn debounce_task(
    raw_rx: std::sync::mpsc::Receiver<(Utf8PathBuf, RawKind)>,
    batch_tx: mpsc::Sender<BatchedWatchEvent>,
    // Kept alive for the duration of the task.
    _watcher: RecommendedWatcher,
) {
    let mut pending: HashMap<Utf8PathBuf, RawKind> = HashMap::new();
    let mut deadline: Option<Instant> = None;

    loop {
        let until = deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));

        tokio::select! {
            biased;

            _ = sleep_until(until), if deadline.is_some() => {
                deadline = None;
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

            // Poll raw events from the sync watcher channel without blocking.
            _ = tokio::task::yield_now() => {
                let mut got_any = false;
                for (path, kind) in raw_rx.try_iter() {
                    got_any = true;
                    let entry = pending.entry(path).or_insert(kind);
                    // Conflict resolution: removed wins; created+modified collapses to created.
                    match (*entry, kind) {
                        (_, RawKind::Removed) => *entry = RawKind::Removed,
                        (RawKind::Created, RawKind::Modified) => {}
                        (RawKind::Modified, RawKind::Created) => *entry = RawKind::Created,
                        _ => *entry = kind,
                    }
                }
                if got_any && deadline.is_none() {
                    deadline = Some(Instant::now() + Duration::from_millis(DEBOUNCE_MS));
                }
            }
        }
    }
}
