//! Applying a watcher batch to the files the repository is held as, in memory.
//!
//! It happens in two halves, on two threads. [`read`] runs on the watcher thread and does the
//! I/O: a checkout or a rebase rewrites hundreds of files and none of that belongs on the UI
//! thread. [`swap_in`] runs on the main thread and publishes what was read, in the same turn
//! that invalidates the offsets computed against the old content. A frame must never paint new
//! content while the offsets naming into it still point at the old one.

/// The paths a watcher batch touched, by kind: what [`read`] reads.
#[derive(Debug, Clone, Default)]
pub(crate) struct Batch {
    pub modified: Vec<camino::Utf8PathBuf>,
    pub created: Vec<camino::Utf8PathBuf>,
    pub removed: Vec<camino::Utf8PathBuf>,
}

/// What reading a batch found, held until the main thread publishes it.
///
/// A one-shot handoff rather than a plain value: the signal carrying it is handed to the app
/// by reference and has to be `Clone`, so both halves reach the same cell and [`swap_in`]
/// empties it. A batch is published exactly once, whatever the signal did on the way.
pub(crate) struct Changes {
    inner: parking_lot::Mutex<Option<Pending>>,
}

struct Pending {
    removed: Vec<crate::loader::FileKey>,
    /// New data for files the list already holds and that are still there.
    reloaded: Vec<(crate::loader::FileKey, crate::loader::FileData)>,
    /// A file that went away and came back: the key it already had, and its new data.
    resurrected: Vec<(crate::loader::FileKey, crate::loader::FileData)>,
    /// Files the list does not hold yet, already read and built.
    appended: Vec<std::sync::Arc<crate::loader::LoadedFile>>,
}

impl std::fmt::Debug for Changes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.inner.lock();
        match guard.as_ref() {
            Some(pending) => f
                .debug_struct("Changes")
                .field("removed", &pending.removed.len())
                .field("reloaded", &pending.reloaded.len())
                .field("resurrected", &pending.resurrected.len())
                .field("appended", &pending.appended.len())
                .finish(),
            None => f.debug_struct("Changes").field("published", &true).finish(),
        }
    }
}

impl Changes {
    fn new(pending: Pending) -> Self {
        Self {
            inner: parking_lot::Mutex::new(Some(pending)),
        }
    }

    fn take(&self) -> Option<Pending> {
        self.inner.lock().take()
    }
}

/// Reads everything `batch` names without touching the shared list or any file in it: the
/// answer is what [`swap_in`] publishes. Runs on the watcher thread.
pub(crate) fn read(
    files: &arc_swap::ArcSwap<Vec<std::sync::Arc<crate::loader::LoadedFile>>>,
    batch: &Batch,
) -> Changes {
    use crate::loader::FileKey;
    use crate::loader::LoadedFile;
    use std::sync::atomic::Ordering;

    let snapshot = files.load_full();
    // Index path -> position once. A rescan (post-overflow) batch can carry hundreds of
    // thousands of paths; a linear `snapshot.iter().find()` per path would be quadratic.
    let index: std::collections::HashMap<&camino::Utf8Path, usize> = snapshot
        .iter()
        .enumerate()
        .map(|(i, file)| (file.path.as_path(), i))
        .collect();

    let mut pending = Pending {
        removed: Vec::new(),
        reloaded: Vec::new(),
        resurrected: Vec::new(),
        appended: Vec::new(),
    };

    for path in &batch.removed {
        let Some(&file_idx) = index.get(path.as_path()) else {
            tracing::debug!("FilesChanged: removed no match for path={path}");
            continue;
        };
        pending.removed.push(FileKey(file_idx));
    }

    for path in &batch.modified {
        match index.get(path.as_path()) {
            Some(&file_idx) if !snapshot[file_idx].removed.load(Ordering::Relaxed) => {
                match crate::loader::read_file_data(path) {
                    Some(data) => pending.reloaded.push((FileKey(file_idx), data)),
                    None => tracing::debug!("FilesChanged: modified unreadable for path={path}"),
                }
            }
            _ => tracing::debug!("FilesChanged: modified no match for path={path}"),
        }
    }

    for path in &batch.created {
        match index.get(path.as_path()) {
            Some(&file_idx) => match crate::loader::read_file_data(path) {
                Some(data) => pending.resurrected.push((FileKey(file_idx), data)),
                None => tracing::debug!("FilesChanged: created unreadable for path={path}"),
            },
            None => match LoadedFile::load(path.clone().into_std_path_buf()) {
                Some(loaded) => pending.appended.push(loaded),
                None => tracing::debug!("FilesChanged: created no match for path={path}"),
            },
        }
    }

    Changes::new(pending)
}

/// Publishes what [`read`] found: marks the removed, swaps the data in, appends the new files,
/// and asks the LSP about each of them. Answers the keys it touched, in the same three kinds,
/// which is all the UI has to know.
///
/// Runs on the main thread, in the turn that invalidates the symbol groups, so that the content
/// and the offsets naming into it never disagree for a frame. A `Changes` already published
/// answers nothing.
pub(crate) fn swap_in(
    files: &arc_swap::ArcSwap<Vec<std::sync::Arc<crate::loader::LoadedFile>>>,
    changes: &Changes,
) -> crate::signal::FilesChanged {
    use crate::loader::LoadedFile;
    use crate::signal::FilesChanged;
    use std::sync::atomic::Ordering;

    let Some(pending) = changes.take() else {
        return FilesChanged::default();
    };
    let snapshot = files.load_full();
    let mut touched = FilesChanged::default();

    for key in pending.removed {
        let Some(file) = snapshot.get(key.0) else {
            continue;
        };
        file.removed.store(true, Ordering::Relaxed);
        crate::lsp::send_file_request(key.0);
        touched.removed.push(key);
    }

    for (key, data) in pending.reloaded {
        let Some(file) = snapshot.get(key.0) else {
            continue;
        };
        file.replace_data(data);
        crate::lsp::send_file_request(key.0);
        touched.modified.push(key);
    }

    for (key, data) in pending.resurrected {
        let Some(file) = snapshot.get(key.0) else {
            continue;
        };
        file.removed.store(false, Ordering::Relaxed);
        file.replace_data(data);
        crate::lsp::send_file_request(key.0);
        touched.created.push(key);
    }

    if !pending.appended.is_empty() {
        // Safe because the watcher becomes the one appender once it runs: the session's own
        // additions are made before it is subscribed.
        let base = snapshot.len();
        let mut next: Vec<std::sync::Arc<LoadedFile>> =
            snapshot.iter().map(std::sync::Arc::clone).collect();
        for (offset, file) in pending.appended.into_iter().enumerate() {
            file.removed.store(false, Ordering::Relaxed);
            touched.created.push(crate::loader::FileKey(base + offset));
            crate::lsp::send_file_request(base + offset);
            next.push(file);
        }
        files.store(std::sync::Arc::new(next));
    }

    touched
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::{FileKey, LoadedFile};
    use std::sync::Arc;

    fn list(
        paths: &[&str],
    ) -> (
        tempfile::TempDir,
        Arc<arc_swap::ArcSwap<Vec<Arc<LoadedFile>>>>,
    ) {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut files = Vec::new();
        for name in paths {
            let path = dir.path().join(name);
            std::fs::write(&path, "old\n").expect("write");
            files.push(LoadedFile::load(path).expect("load"));
        }
        (dir, Arc::new(arc_swap::ArcSwap::from_pointee(files)))
    }

    fn path_of(dir: &tempfile::TempDir, name: &str) -> camino::Utf8PathBuf {
        camino::Utf8PathBuf::from_path_buf(dir.path().join(name)).expect("utf8 path")
    }

    /// The whole point of the split: while a batch is being read, the file a window is showing
    /// still holds the content its bounds were computed against. Publishing it out of that
    /// order is what lets a frame paint one against the other.
    #[test]
    fn reading_publishes_nothing_until_the_swap() {
        let (dir, files) = list(&["a.rs"]);
        std::fs::write(dir.path().join("a.rs"), "a much longer new line\n").expect("write");

        let changes = read(
            &files,
            &Batch {
                modified: vec![path_of(&dir, "a.rs")],
                ..Default::default()
            },
        );

        let before = files.load();
        assert_eq!(before[0].data.read().content, "old\n");
        assert_eq!(before[0].data.read().line_starts, [0]);

        let touched = swap_in(&files, &changes);

        assert_eq!(touched.modified, [FileKey(0)]);
        assert_eq!(
            files.load()[0].data.read().content,
            "a much longer new line\n"
        );
    }

    /// A frame drawn between the two halves must not be able to slice the line with an offset
    /// that only fits the content being read. The old content is what a preview sees there.
    #[test]
    fn a_bound_taken_before_the_swap_still_slices_the_old_content() {
        let (dir, files) = list(&["a.rs"]);
        // three bytes per character in the new content, one in the old
        std::fs::write(dir.path().join("a.rs"), "— dash\n").expect("write");
        let changes = read(
            &files,
            &Batch {
                modified: vec![path_of(&dir, "a.rs")],
                ..Default::default()
            },
        );

        let snapshot = files.load();
        let data = snapshot[0].data.read();
        let line = data.line(0);
        assert_eq!(line, "old\n");
        assert_eq!(&line[0..3], "old");

        drop(data);
        drop(snapshot);
        swap_in(&files, &changes);
        assert_eq!(files.load()[0].data.read().line(0), "— dash\n");
    }

    #[test]
    fn swapping_in_replaces_the_content_and_answers_by_key() {
        let (dir, files) = list(&["a.rs", "b.rs"]);
        std::fs::write(dir.path().join("b.rs"), "new\n").expect("write");
        let changes = read(
            &files,
            &Batch {
                modified: vec![path_of(&dir, "b.rs")],
                ..Default::default()
            },
        );

        let touched = swap_in(&files, &changes);

        assert_eq!(touched.modified, [FileKey(1)]);
        assert_eq!(files.load()[1].data.read().content, "new\n");
    }

    /// The handoff is spent by the swap: a signal delivered twice publishes nothing the second
    /// time, so the same batch cannot be applied twice.
    #[test]
    fn a_batch_is_published_once() {
        let (dir, files) = list(&["a.rs"]);
        let changes = read(
            &files,
            &Batch {
                removed: vec![path_of(&dir, "a.rs")],
                ..Default::default()
            },
        );

        assert_eq!(swap_in(&files, &changes).removed, [FileKey(0)]);
        assert_eq!(
            swap_in(&files, &changes),
            crate::signal::FilesChanged::default()
        );
    }

    #[test]
    fn a_created_file_appends_so_the_keys_before_it_keep_their_paths() {
        let (dir, files) = list(&["a.rs"]);
        std::fs::write(dir.path().join("z.rs"), "new\n").expect("write");
        let changes = read(
            &files,
            &Batch {
                created: vec![path_of(&dir, "z.rs")],
                ..Default::default()
            },
        );

        let touched = swap_in(&files, &changes);

        assert_eq!(touched.created, [FileKey(1)]);
        let snapshot = files.load();
        assert_eq!(snapshot[0].path, path_of(&dir, "a.rs"));
        assert_eq!(snapshot[1].path, path_of(&dir, "z.rs"));
        assert!(
            !snapshot[1]
                .removed
                .load(std::sync::atomic::Ordering::Relaxed)
        );
    }

    #[test]
    fn several_created_files_take_the_keys_after_the_list() {
        let (dir, files) = list(&["a.rs"]);
        for name in ["y.rs", "z.rs"] {
            std::fs::write(dir.path().join(name), "new\n").expect("write");
        }
        let changes = read(
            &files,
            &Batch {
                created: vec![path_of(&dir, "y.rs"), path_of(&dir, "z.rs")],
                ..Default::default()
            },
        );

        assert_eq!(swap_in(&files, &changes).created, [FileKey(1), FileKey(2)]);
        assert_eq!(files.load().len(), 3);
    }

    #[test]
    fn a_removed_file_is_marked_and_answered_by_its_key() {
        let (dir, files) = list(&["a.rs", "b.rs"]);
        let changes = read(
            &files,
            &Batch {
                removed: vec![path_of(&dir, "a.rs")],
                ..Default::default()
            },
        );

        let touched = swap_in(&files, &changes);

        assert_eq!(touched.removed, [FileKey(0)]);
        assert!(
            files.load()[0]
                .removed
                .load(std::sync::atomic::Ordering::Relaxed)
        );
    }

    /// A batch of a checkout also names files the walk never loaded: they are dropped rather
    /// than answering a key no window could show.
    #[test]
    fn a_path_the_list_does_not_hold_is_dropped() {
        let (dir, files) = list(&["a.rs"]);
        let changes = read(
            &files,
            &Batch {
                modified: vec![path_of(&dir, "gone.rs")],
                removed: vec![path_of(&dir, "gone.rs")],
                ..Default::default()
            },
        );

        let touched = swap_in(&files, &changes);

        assert_eq!(touched, crate::signal::FilesChanged::default());
        assert_eq!(files.load().len(), 1);
    }

    /// A file that comes back is the key it already had: a session stub it once answered keeps
    /// its window, and nothing is appended for it.
    #[test]
    fn a_file_that_comes_back_keeps_its_key() {
        let (dir, files) = list(&["a.rs"]);
        files.store(Arc::new(vec![LoadedFile::stub(path_of(&dir, "a.rs"))]));
        std::fs::write(dir.path().join("a.rs"), "back\n").expect("write");

        let changes = read(
            &files,
            &Batch {
                created: vec![path_of(&dir, "a.rs")],
                ..Default::default()
            },
        );
        let touched = swap_in(&files, &changes);

        assert_eq!(touched.created, [FileKey(0)]);
        assert!(
            !files.load()[0]
                .removed
                .load(std::sync::atomic::Ordering::Relaxed)
        );
        assert_eq!(files.load()[0].data.read().content, "back\n");
    }

    /// Publishing drops the colors computed against the old content, so no syntax span outlives
    /// the line it names into.
    #[test]
    fn the_swap_drops_the_colors_of_the_old_content() {
        use crate::highlight::TokenKind;
        let (dir, files) = list(&["a.rs"]);
        let old = crate::highlight::ColoredSpan {
            start: 0,
            end: 3,
            kind: TokenKind::Other,
        };
        *files.load()[0].colored_lines.lock() = vec![vec![old]];
        std::fs::write(dir.path().join("a.rs"), "new\n").expect("write");

        let changes = read(
            &files,
            &Batch {
                modified: vec![path_of(&dir, "a.rs")],
                ..Default::default()
            },
        );
        swap_in(&files, &changes);

        assert!(files.load()[0].colored_lines.lock().is_empty());
    }
}
