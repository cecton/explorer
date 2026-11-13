//! What a turn does to the working tree when it ends: a commit turn keeps its commits and
//! loses what it left uncommitted, a feedback turn loses both.

/// Whatever a turn left uncommitted is discarded, and a commit turn's commits are
/// collected; the tree was clean when the turn started.
pub(super) async fn close(
    request: &crate::agent::turn::TurnRequest,
    head_after: Option<String>,
) -> (Vec<String>, bool) {
    use crate::agent::Mode;
    match request.mode {
        Mode::Feedback => {
            let root = request.root.clone();
            let reset = tokio::task::spawn_blocking(move || discard_turn_work(&root))
                .await
                .unwrap_or(false);
            (Vec::new(), reset)
        }
        Mode::Commit => {
            let root = request.root.clone();
            let node = request.node.clone();
            let before = request.head.clone();
            let (commits, reset) = tokio::task::spawn_blocking(move || {
                close_commit_turn(
                    &root,
                    node.as_str(),
                    before.as_deref(),
                    head_after.as_deref(),
                )
            })
            .await
            .unwrap_or_default();
            (commits, reset)
        }
    }
}

/// The commits a commit turn made; what it left uncommitted is discarded.
fn close_commit_turn(
    root: &camino::Utf8Path,
    node: &str,
    head_before: Option<&str>,
    head_after: Option<&str>,
) -> (Vec<String>, bool) {
    let commits = crate::git::commits_since(root, head_before, head_after);
    if commits.is_empty() && head_before != head_after {
        tracing::warn!("commit turn {node} moved HEAD but listed no commits");
    }
    (commits, discard_turn_work(root))
}

/// Discards what the turn left uncommitted; the tree was clean when the turn started.
fn discard_turn_work(root: &camino::Utf8Path) -> bool {
    match crate::git::status_short(root) {
        Ok(status) if !status.trim().is_empty() => crate::git::discard_worktree(root)
            .map_err(|e| tracing::warn!("cannot reset the tree after the turn: {e}"))
            .is_ok(),
        Ok(_) => false,
        Err(e) => {
            tracing::warn!("cannot check the tree after the turn: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &camino::Utf8Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    fn temp_repo() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "test"]);
        git(&root, &["config", "user.email", "test@example.com"]);
        git(&root, &["config", "commit.gpgsign", "false"]);
        (tmp, root)
    }

    fn commit(root: &camino::Utf8Path, file: &str, content: &str, message: &str) -> String {
        std::fs::write(root.join(file), content).unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", message]);
        git(root, &["rev-parse", "HEAD"])
    }

    #[test]
    fn closing_a_clean_turn_lists_its_commits_oldest_first() {
        let (_tmp, root) = temp_repo();
        let before = commit(&root, "a.txt", "1", "init");
        let first = commit(&root, "a.txt", "2", "feat: one");
        let second = commit(&root, "b.txt", "x", "feat: two");

        let (commits, reset) = close_commit_turn(&root, "node", Some(&before), Some(&second));
        assert_eq!(commits, [first, second]);
        assert!(!reset);
    }

    #[test]
    fn closing_a_turn_that_changed_nothing_records_nothing() {
        let (_tmp, root) = temp_repo();
        let head = commit(&root, "a.txt", "1", "init");
        let (commits, reset) = close_commit_turn(&root, "node", Some(&head), Some(&head));
        assert!(commits.is_empty());
        assert!(!reset);
    }

    #[test]
    fn closing_records_the_new_commit_of_an_amend() {
        let (_tmp, root) = temp_repo();
        let base = commit(&root, "a.txt", "1", "init");
        let before = commit(&root, "a.txt", "2", "feat: one");
        std::fs::write(root.join("a.txt"), "3").unwrap();
        git(
            &root,
            &["commit", "-q", "-a", "--amend", "-m", "feat: one, amended"],
        );
        let amended = git(&root, &["rev-parse", "HEAD"]);
        assert_ne!(amended, before);

        let (commits, _) = close_commit_turn(&root, "node", Some(&before), Some(&amended));
        assert_eq!(commits, [amended]);
        assert_eq!(git(&root, &["rev-parse", "HEAD~1"]), base);
    }

    #[test]
    fn closing_a_clean_feedback_turn_touches_nothing() {
        let (_tmp, root) = temp_repo();
        let head = commit(&root, "a.txt", "1", "init");

        assert!(!discard_turn_work(&root));
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "1");
        assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    }

    #[test]
    fn closing_a_dirty_feedback_turn_discards_its_scratch() {
        let (_tmp, root) = temp_repo();
        let head = commit(&root, "a.txt", "1", "init");
        std::fs::write(root.join("a.txt"), "scratch").unwrap();
        std::fs::write(root.join("note.txt"), "untracked").unwrap();

        assert!(discard_turn_work(&root));
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "1");
        assert!(!root.join("note.txt").exists());
        assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
        assert_eq!(crate::git::status_short(&root).unwrap(), "");
    }

    #[test]
    fn closing_a_dirty_turn_discards_tracked_and_untracked_leftovers() {
        let (_tmp, root) = temp_repo();
        let before = commit(&root, "a.txt", "1", "init");
        std::fs::write(root.join("a.txt"), "changed").unwrap();
        std::fs::write(root.join("new.txt"), "untracked").unwrap();

        let (commits, reset) = close_commit_turn(&root, "node-7", Some(&before), Some(&before));
        assert!(commits.is_empty());
        assert!(reset);
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "1");
        assert!(!root.join("new.txt").exists());
        assert_eq!(git(&root, &["rev-parse", "HEAD"]), before);
        assert_eq!(crate::git::status_short(&root).unwrap(), "");
        assert!(git(&root, &["stash", "list"]).is_empty());
    }

    #[test]
    fn closing_a_turn_that_committed_keeps_the_commit_and_discards_the_rest() {
        let (_tmp, root) = temp_repo();
        let before = commit(&root, "a.txt", "1", "init");
        std::fs::write(root.join("a.txt"), "2").unwrap();
        git(&root, &["commit", "-q", "-a", "-m", "feat: one"]);
        let after = git(&root, &["rev-parse", "HEAD"]);
        std::fs::write(root.join("a.txt"), "3").unwrap();
        std::fs::write(root.join("new.txt"), "untracked").unwrap();

        let (commits, reset) = close_commit_turn(&root, "node", Some(&before), Some(&after));
        assert_eq!(commits, std::slice::from_ref(&after));
        assert!(reset);
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "2");
        assert!(!root.join("new.txt").exists());
        assert_eq!(git(&root, &["rev-parse", "HEAD"]), after);
        assert_eq!(crate::git::status_short(&root).unwrap(), "");
    }

    #[test]
    fn a_commit_turn_cut_short_keeps_its_commits() {
        use crate::conversation::NodeStatus;
        let (_tmp, root) = temp_repo();
        let before = commit(&root, "a.txt", "1", "init");
        let made = commit(&root, "a.txt", "2", "feat: one");

        let (commits, _) = close_commit_turn(&root, "node", Some(&before), Some(&made));
        let (status, error) = crate::conversation::metadata::cut_short_outcome("", &commits);
        assert_eq!(commits, [made]);
        assert_eq!(status, NodeStatus::Cancelled);
        assert_eq!(error, None);
    }

    #[test]
    fn closing_the_first_commit_of_a_repository_lists_it() {
        let (_tmp, root) = temp_repo();
        let first = commit(&root, "a.txt", "1", "init");
        let (commits, _) = close_commit_turn(&root, "node", None, Some(&first));
        assert_eq!(commits, [first]);
    }
}
