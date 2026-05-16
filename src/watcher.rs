use crate::tui::state::AppSignal;
use camino::{Utf8Path, Utf8PathBuf};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use r3bl_tui::TerminalWindowMainThreadSignal;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::Duration;

const DEBOUNCE_MS: u64 = 50;

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

pub fn start_watcher(
    root: &Utf8Path,
    signal_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
) -> notify::Result<()> {
    let (raw_tx, raw_rx) = mpsc::unbounded_channel::<notify::Result<notify::Event>>();

    let mut watcher = RecommendedWatcher::new(raw_tx, notify::Config::default())?;
    watcher.watch(root.as_std_path(), RecursiveMode::Recursive)?;

    tokio::spawn(debounce_task(root.to_owned(), raw_rx, signal_tx, watcher));
    Ok(())
}

async fn debounce_task(
    root: Utf8PathBuf,
    mut raw_rx: mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
    signal_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
    // Kept alive for the duration of the task.
    _watcher: RecommendedWatcher,
) {
    let mut pending: HashMap<Utf8PathBuf, RawKind> = HashMap::new();

    loop {
        match tokio::time::timeout(Duration::from_millis(DEBOUNCE_MS), raw_rx.recv()).await {
            Ok(Some(Ok(event))) => {
                let kind = match event.kind {
                    EventKind::Create(_) => RawKind::Created,
                    EventKind::Modify(_) => RawKind::Modified,
                    EventKind::Remove(_) => RawKind::Removed,
                    _ => continue,
                };
                for path in event.paths {
                    let Ok(utf8) = Utf8PathBuf::from_path_buf(path) else {
                        continue;
                    };
                    // Filter out .git and target subtrees.
                    let Ok(rel) = utf8.strip_prefix(&root) else {
                        continue;
                    };
                    let first = rel.components().next().map(|c| c.as_str());
                    if matches!(first, Some(".git") | Some("target")) {
                        continue;
                    }
                    let entry = pending.entry(utf8).or_insert(kind);
                    // Conflict resolution: removed wins; created+modified collapses to created.
                    match (*entry, kind) {
                        (_, RawKind::Removed) => *entry = RawKind::Removed,
                        (RawKind::Created, RawKind::Modified) => {}
                        (RawKind::Modified, RawKind::Created) => *entry = RawKind::Created,
                        _ => *entry = kind,
                    }
                }
            }
            Ok(Some(Err(_))) | Ok(None) => return,
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
                if signal_tx
                    .send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                        AppSignal::FilesChanged(Arc::new(batch)),
                    ))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}
