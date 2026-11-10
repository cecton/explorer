//! Applying a watcher batch to the files the repository is held as, in memory.
//!
//! The batch names paths; the answer is which files changed identity and which groups of
//! symbol locations went stale with them. Doing that here rather than in the signal handler
//! keeps the handler about *when* to act and this about *what* changed.

/// Folds `batch` into `files` in place: marks the removed, reloads the modified and the
/// created, appends the created ones. The list is append-only, so `FileKey`s stay valid.
pub(crate) fn apply(
    files: &arc_swap::ArcSwap<Vec<std::sync::Arc<crate::loader::LoadedFile>>>,
    batch: &crate::signal::FilesChanged,
) {
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

    for path in &batch.removed {
        let Some(&file_idx) = index.get(path.as_path()) else {
            tracing::debug!("FilesChanged: removed no match for path={path}");
            continue;
        };
        snapshot[file_idx].removed.store(true, Ordering::Relaxed);
        crate::lsp::send_file_request(file_idx);
    }

    for path in &batch.modified {
        match index.get(path.as_path()) {
            Some(&file_idx) if !snapshot[file_idx].removed.load(Ordering::Relaxed) => {
                snapshot[file_idx].reload();
                crate::lsp::send_file_request(file_idx);
            }
            _ => tracing::debug!("FilesChanged: modified no match for path={path}"),
        }
    }

    let mut new_files: Vec<std::sync::Arc<LoadedFile>> = Vec::new();
    for path in &batch.created {
        match index.get(path.as_path()) {
            Some(&file_idx) => {
                snapshot[file_idx].removed.store(false, Ordering::Relaxed);
                snapshot[file_idx].reload();
                crate::lsp::send_file_request(file_idx);
            }
            None => match LoadedFile::load(path.clone().into_std_path_buf()) {
                Some(loaded) => {
                    new_files.push(loaded);
                }
                None => tracing::debug!("FilesChanged: created no match for path={path}"),
            },
        }
    }

    if !new_files.is_empty() {
        let mut next: Vec<std::sync::Arc<LoadedFile>> =
            snapshot.iter().map(std::sync::Arc::clone).collect();
        next.extend(new_files);
        files.store(std::sync::Arc::new(next));
    }
}
