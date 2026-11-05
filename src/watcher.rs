use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

const DEBOUNCE: Duration = Duration::from_millis(50);

pub static WATCHER_RRT: r3bl_tui::RRT<WatcherWorker> = r3bl_tui::RRT::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawKind {
    Created,
    Modified,
    Removed,
}

#[derive(Debug)]
pub struct NoOpInterrupt;

impl r3bl_tui::RRTSoftwareInterrupt for NoOpInterrupt {
    fn trigger_software_interrupt(&self) {}
}

#[derive(Clone)]
pub struct WatcherConfig {
    pub root: camino::Utf8PathBuf,
    pub app_tx: crate::signal::MainTx,
    pub files: Arc<arc_swap::ArcSwap<Vec<Arc<crate::loader::LoadedFile>>>>,
}

impl Debug for WatcherConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatcherConfig")
            .field("root", &self.root)
            .finish()
    }
}

pub struct WatcherWorker {
    root: camino::Utf8PathBuf,
    app_tx: crate::signal::MainTx,
    files: Arc<arc_swap::ArcSwap<Vec<Arc<crate::loader::LoadedFile>>>>,
    rx: mpsc::Receiver<notify::Result<notify::Event>>,
    pending: HashMap<camino::Utf8PathBuf, RawKind>,
    // Set when notify reports an event-queue overflow (`need_rescan`). On the
    // next flush we re-walk the tree and reconcile, recovering dropped events.
    pending_rescan: bool,
    // A git history trigger fired since the last flush, sent as one GitHistoryChanged on it.
    pending_git: bool,
    // Held to keep the watcher alive for the duration of the worker.
    _watcher: notify::RecommendedWatcher,
}

impl Debug for WatcherWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatcherWorker")
            .field("root", &self.root)
            .finish()
    }
}

impl r3bl_tui::RRTWorker for WatcherWorker {
    type Config = WatcherConfig;
    type Output = ();
    type Interrupt = NoOpInterrupt;

    fn create_and_register_os_sources(
        config: Self::Config,
        _receiver: tokio::sync::broadcast::Receiver<Self::Input>,
    ) -> miette::Result<(Self, Self::Interrupt)> {
        use notify::Watcher;
        let WatcherConfig {
            root,
            app_tx,
            files,
        } = config;

        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::RecommendedWatcher::new(tx, notify::Config::default())
            .map_err(|e| miette::miette!("notify watcher error: {e}"))?;
        watcher
            .watch(root.as_std_path(), notify::RecursiveMode::Recursive)
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
                pending_git: false,
                _watcher: watcher,
            },
            NoOpInterrupt,
        ))
    }

    fn restart_policy() -> r3bl_tui::RestartPolicy {
        r3bl_tui::RestartPolicy::default()
    }

    fn block_until_ready_then_dispatch(
        &mut self,
        _sender: &tokio::sync::broadcast::Sender<r3bl_tui::RRTEvent<Self::Output>>,
    ) -> r3bl_tui::Continuation {
        use crate::signal::AppSignal;
        use crate::signal::FilesChanged;
        match self.rx.recv_timeout(DEBOUNCE) {
            Ok(Ok(event)) => {
                // Event-queue overflow: the kernel dropped events, so per-path
                // deltas are unreliable. Flag a full rescan on the next flush
                // instead of trying to interpret this event's paths.
                if event.need_rescan() {
                    tracing::warn!("watcher: event-queue overflow, scheduling rescan");
                    self.pending_rescan = true;
                    self.pending_git = true;
                    return r3bl_tui::Continuation::Continue;
                }
                let kind = match event.kind {
                    notify::EventKind::Create(_) => RawKind::Created,
                    notify::EventKind::Modify(_) => RawKind::Modified,
                    notify::EventKind::Remove(_) => RawKind::Removed,
                    _ => return r3bl_tui::Continuation::Continue,
                };
                for path in event.paths {
                    let Ok(utf8) = camino::Utf8PathBuf::from_path_buf(path) else {
                        continue;
                    };
                    let Ok(rel) = utf8.strip_prefix(&self.root) else {
                        continue;
                    };
                    // Ahead of the ignored filter: the paths that reload the git log are
                    // under `.git`, which `is_ignored` drops.
                    if is_git_history_trigger(rel.components()) {
                        self.pending_git = true;
                        continue;
                    }
                    if crate::loader::is_ignored(rel) {
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
                r3bl_tui::Continuation::Continue
            }
            Ok(Err(e)) => {
                tracing::warn!("watcher: notify error: {:?}", e);
                r3bl_tui::Continuation::Restart
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                tracing::warn!("watcher: channel disconnected, restarting");
                r3bl_tui::Continuation::Restart
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if self.pending_git {
                    self.pending_git = false;
                    if self
                        .app_tx
                        .blocking_send(r3bl_tui::TerminalWindowMainThreadSignal::ApplyAppSignal(
                            AppSignal::GitHistoryChanged,
                        ))
                        .is_err()
                    {
                        tracing::warn!("watcher: app_tx closed, stopping");
                        return r3bl_tui::Continuation::Stop;
                    }
                }
                if self.pending.is_empty() && !self.pending_rescan {
                    return r3bl_tui::Continuation::Continue;
                }
                let mut batch = FilesChanged {
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
                let signal = AppSignal::FilesChanged(Arc::new(batch));
                if self
                    .app_tx
                    .blocking_send(r3bl_tui::TerminalWindowMainThreadSignal::ApplyAppSignal(
                        signal,
                    ))
                    .is_err()
                {
                    tracing::warn!("watcher: app_tx closed, stopping");
                    return r3bl_tui::Continuation::Stop;
                }
                r3bl_tui::Continuation::Continue
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
    fn reconcile_into(&self, batch: &mut crate::signal::FilesChanged) {
        let on_disk: HashSet<camino::Utf8PathBuf> = crate::loader::walk_repo_files(&self.root)
            .into_iter()
            .filter_map(|p| camino::Utf8PathBuf::from_path_buf(p).ok())
            .collect();

        let snapshot = self.files.load();

        // Paths already accounted for by this batch's coalesced events, so the
        // rescan delta doesn't duplicate them.
        let mut seen: HashSet<camino::Utf8PathBuf> = batch
            .created
            .iter()
            .chain(batch.modified.iter())
            .chain(batch.removed.iter())
            .cloned()
            .collect();

        // Active (non-removed) paths currently in the list.
        let active: HashSet<&camino::Utf8PathBuf> = snapshot
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

/// Whether a path under `.git`, relative to it, can change what the git log
/// windows show: `HEAD`, `refs/**` or `packed-refs`. Deliberately not
/// `objects/**` (written by every commit and many non-history operations, the
/// ref update is the authoritative event) and not `index` (git status rewrites
/// it for its stat cache; nothing the log renders depends on it).
fn is_git_history_trigger(components: camino::Utf8Components<'_>) -> bool {
    let mut comps = components.map(|c| c.as_os_str());
    let Some(first) = comps.next() else {
        return false;
    };
    if first != crate::loader::GIT_DIR {
        return false;
    }
    match comps.next() {
        Some(second) => second == "HEAD" || second == "packed-refs" || second == "refs",
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;

    #[test]
    fn events_under_ignored_dirs_are_dropped_at_any_depth() {
        for path in [
            "target/debug/explorer",
            "crates/xtask/target/debug/build.rs",
            ".git/config",
            "vendor/x/.git/HEAD",
        ] {
            assert!(crate::loader::is_ignored(Utf8Path::new(path)), "{path}");
        }
        for path in [
            "src/target_edit.rs",
            "docs/targeting.md",
            "xtask/src/main.rs",
        ] {
            assert!(!crate::loader::is_ignored(Utf8Path::new(path)), "{path}");
        }
    }

    #[test]
    fn the_git_history_trigger_is_checked_before_the_ignored_dir_filter() {
        // Both predicates hold for these, so only the order of the two checks in the
        // dispatch loop keeps a ref update reloading the git log instead of dropping it.
        for path in [".git/HEAD", ".git/refs/heads/main", ".git/packed-refs"] {
            let rel = Utf8Path::new(path);
            assert!(is_git_history_trigger(rel.components()), "{path}");
            assert!(crate::loader::is_ignored(rel), "{path}");
        }
    }

    #[test]
    fn git_history_trigger_matches_head_refs_and_packed_refs() {
        for path in [".git/refs", ".git/refs/heads/main", ".git/packed-refs"] {
            let rel = camino::Utf8PathBuf::from(path);
            assert!(
                is_git_history_trigger(rel.components()),
                "expected trigger: {path}"
            );
        }
        for path in [
            ".git",
            ".git/config",
            ".git/objects/ab/cdef",
            ".git/logs/HEAD",
            ".git/index",
            ".git/refs.lock",
            "src/main.rs",
        ] {
            let rel = camino::Utf8PathBuf::from(path);
            assert!(
                !is_git_history_trigger(rel.components()),
                "expected no trigger: {path}"
            );
        }
    }
}
