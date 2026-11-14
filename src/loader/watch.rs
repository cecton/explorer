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
    /// The length of the list this batch was read against. A path the read did not find there
    /// can only have come back in the tail grown since.
    read_len: usize,
    removed: Vec<(Option<crate::loader::FileKey>, camino::Utf8PathBuf)>,
    /// New data for files the list already holds and that are still there.
    reloaded: Vec<(
        Option<crate::loader::FileKey>,
        camino::Utf8PathBuf,
        crate::loader::FileData,
    )>,
    /// A file that went away and came back, or a new one: the key the read found, when it did.
    created: Vec<(
        Option<crate::loader::FileKey>,
        camino::Utf8PathBuf,
        crate::loader::FileData,
    )>,
}

/// The key a path of the batch names once the list has grown.
///
/// A key the read found still names the file it named: the list is append-only, so nothing
/// before it moves. A path the read did not find can only have come back in the tail appended
/// since, by a batch published while this one was being read.
fn resolve_key(
    files: &[std::sync::Arc<crate::loader::LoadedFile>],
    read_len: usize,
    known: Option<crate::loader::FileKey>,
    path: &camino::Utf8Path,
) -> Option<crate::loader::FileKey> {
    known.or_else(|| {
        let from = read_len.min(files.len());
        files[from..]
            .iter()
            .position(|file| file.path == path)
            .map(|offset| crate::loader::FileKey(from + offset))
    })
}

impl std::fmt::Debug for Changes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.inner.lock();
        match guard.as_ref() {
            Some(pending) => f
                .debug_struct("Changes")
                .field("removed", &pending.removed.len())
                .field("reloaded", &pending.reloaded.len())
                .field("created", &pending.created.len())
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
///
/// The list is read once, for its length, but the keys answered here are only a hint: a batch
/// can be read while an earlier one still waits to be published, and the list then holds files
/// this read never saw. [`swap_in`] resolves every path again against the list it publishes
/// into, so the overlap loses no edit, leaves no deleted file behind and appends no path twice.
pub(crate) fn read(
    files: &arc_swap::ArcSwap<Vec<std::sync::Arc<crate::loader::LoadedFile>>>,
    batch: &Batch,
) -> Changes {
    use crate::loader::FileKey;

    let snapshot = files.load_full();
    // Index path -> position once. A rescan (post-overflow) batch can carry hundreds of
    // thousands of paths; a linear `snapshot.iter().find()` per path would be quadratic.
    let index: std::collections::HashMap<&camino::Utf8Path, usize> = snapshot
        .iter()
        .enumerate()
        .map(|(i, file)| (file.path.as_path(), i))
        .collect();

    let mut pending = Pending {
        read_len: snapshot.len(),
        removed: Vec::new(),
        reloaded: Vec::new(),
        created: Vec::new(),
    };

    for path in &batch.removed {
        let known = index.get(path.as_path()).map(|&file_idx| FileKey(file_idx));
        pending.removed.push((known, path.clone()));
    }

    for path in &batch.modified {
        // The removed flag may be stale here: publication decides whether the file is there.
        let known = index.get(path.as_path()).map(|&file_idx| FileKey(file_idx));
        match crate::loader::read_file_data(path) {
            Some(data) => pending.reloaded.push((known, path.clone(), data)),
            None => tracing::debug!("FilesChanged: modified unreadable for path={path}"),
        }
    }

    // A `created` reads a file whether the list holds it or not: which of the two it is only
    // the list at publication decides, and a path the list does not hold yet may be one an
    // earlier batch appended meanwhile.
    for path in &batch.created {
        let known = index.get(path.as_path()).map(|&file_idx| FileKey(file_idx));
        match crate::loader::read_file_data(path) {
            Some(data) => pending.created.push((known, path.clone(), data)),
            None => tracing::debug!("FilesChanged: created unreadable for path={path}"),
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
    use crate::loader::FileKey;
    use crate::loader::LoadedFile;
    use crate::signal::FilesChanged;
    use std::sync::atomic::Ordering;

    let Some(pending) = changes.take() else {
        return FilesChanged::default();
    };
    let snapshot = files.load_full();
    let mut touched = FilesChanged::default();
    let read_len = pending.read_len;
    let key_of = |known, path: &camino::Utf8Path| resolve_key(&snapshot, read_len, known, path);

    for (known, path) in pending.removed {
        let Some(key) = key_of(known, &path) else {
            tracing::debug!("FilesChanged: removed no match for path={path}");
            continue;
        };
        snapshot[key.0].removed.store(true, Ordering::Relaxed);
        crate::lsp::send_file_request(key.0);
        touched.removed.push(key);
    }

    for (known, path, data) in pending.reloaded {
        // By publication the file may be gone: the edit then has no file to land on.
        let Some(key) =
            key_of(known, &path).filter(|key| !snapshot[key.0].removed.load(Ordering::Relaxed))
        else {
            tracing::debug!("FilesChanged: modified no match for path={path}");
            continue;
        };
        snapshot[key.0].replace_data(data);
        crate::lsp::send_file_request(key.0);
        touched.modified.push(key);
    }

    let mut appended: Vec<std::sync::Arc<LoadedFile>> = Vec::new();
    for (known, path, data) in pending.created {
        match key_of(known, &path) {
            Some(key) => {
                snapshot[key.0].removed.store(false, Ordering::Relaxed);
                snapshot[key.0].replace_data(data);
                crate::lsp::send_file_request(key.0);
                touched.created.push(key);
            }
            None => appended.push(LoadedFile::from_data(path, data)),
        }
    }

    if !appended.is_empty() {
        // Safe because the watcher becomes the one appender once it runs: the session's own
        // additions are made before it is subscribed.
        let base = snapshot.len();
        let mut next: Vec<std::sync::Arc<LoadedFile>> =
            snapshot.iter().map(std::sync::Arc::clone).collect();
        for (offset, file) in appended.into_iter().enumerate() {
            touched.created.push(FileKey(base + offset));
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

    /// A batch is read on the watcher thread and published on the main one, so a second batch
    /// can be read before the first is published. A file the first created is edited by the
    /// second: the edit lands on the file the first appended, not on a key only the read of
    /// the second could name.
    #[test]
    fn an_edit_of_a_file_created_by_an_unpublished_batch_is_applied() {
        let (dir, files) = list(&["a.rs"]);
        std::fs::write(dir.path().join("z.rs"), "one\n").expect("write");
        let created = read(
            &files,
            &Batch {
                created: vec![path_of(&dir, "z.rs")],
                ..Default::default()
            },
        );
        std::fs::write(dir.path().join("z.rs"), "two\n").expect("write");
        let edited = read(
            &files,
            &Batch {
                modified: vec![path_of(&dir, "z.rs")],
                ..Default::default()
            },
        );

        assert_eq!(swap_in(&files, &created).created, [FileKey(1)]);
        assert_eq!(swap_in(&files, &edited).modified, [FileKey(1)]);
        assert_eq!(files.load()[1].data.read().content, "two\n");
    }

    /// The same overlap with a deletion: the file must not outlive the batch that saw it go.
    #[test]
    fn a_file_created_and_then_removed_by_an_overlapping_batch_is_marked_removed() {
        let (dir, files) = list(&["a.rs"]);
        std::fs::write(dir.path().join("z.rs"), "one\n").expect("write");
        let created = read(
            &files,
            &Batch {
                created: vec![path_of(&dir, "z.rs")],
                ..Default::default()
            },
        );
        std::fs::remove_file(dir.path().join("z.rs")).expect("remove");
        let removed = read(
            &files,
            &Batch {
                removed: vec![path_of(&dir, "z.rs")],
                ..Default::default()
            },
        );

        assert_eq!(swap_in(&files, &created).created, [FileKey(1)]);
        assert_eq!(swap_in(&files, &removed).removed, [FileKey(1)]);
        assert!(
            files.load()[1]
                .removed
                .load(std::sync::atomic::Ordering::Relaxed)
        );
        assert_eq!(files.load().len(), 2);
    }

    /// And a path both batches call created is appended once: the second finds the file the
    /// first appended, since a key answers a path for the life of the list.
    #[test]
    fn a_path_created_by_two_overlapping_batches_is_appended_once() {
        let (dir, files) = list(&["a.rs"]);
        std::fs::write(dir.path().join("z.rs"), "one\n").expect("write");
        let batch = Batch {
            created: vec![path_of(&dir, "z.rs")],
            ..Default::default()
        };
        let first = read(&files, &batch);
        let second = read(&files, &batch);

        swap_in(&files, &first);
        assert_eq!(swap_in(&files, &second).created, [FileKey(1)]);
        assert_eq!(files.load().len(), 2);
        assert_eq!(files.load()[1].path, path_of(&dir, "z.rs"));
    }

    /// The removed flag the read saw is not the one that decides: a file the list held as gone
    /// is brought back by a batch still waiting to be published, so the edit that follows must
    /// land on it rather than be dropped as an edit of a file that no longer exists.
    #[test]
    fn an_edit_of_a_file_brought_back_by_an_unpublished_batch_is_applied() {
        let (dir, files) = list(&["a.rs", "z.rs"]);
        files.load()[1]
            .removed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        std::fs::write(dir.path().join("z.rs"), "one\n").expect("write");
        let created = read(
            &files,
            &Batch {
                created: vec![path_of(&dir, "z.rs")],
                ..Default::default()
            },
        );
        std::fs::write(dir.path().join("z.rs"), "two\n").expect("write");
        let edited = read(
            &files,
            &Batch {
                modified: vec![path_of(&dir, "z.rs")],
                ..Default::default()
            },
        );

        assert_eq!(swap_in(&files, &created).created, [FileKey(1)]);
        assert_eq!(swap_in(&files, &edited).modified, [FileKey(1)]);
        assert_eq!(files.load()[1].data.read().content, "two\n");
    }
}
