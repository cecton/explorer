//! What a turn does to the working tree when it ends: a commit turn keeps its commits and
//! loses what it left uncommitted, a feedback turn loses both. A feedback turn that worked
//! in a copy of the repository leaves the repository as it was: the copy is what goes.

/// Discards what a turn left uncommitted in the repository; keeps a commit turn's commits.
pub(super) async fn close(
    request: &crate::agent::turn::TurnRequest,
    head_after: Option<String>,
    workspace: Option<&crate::agent::workspace::Workspace>,
) -> (Vec<String>, bool) {
    use crate::agent::Mode;
    use crate::agent::workspace::{Plan, Workspace};
    match request.mode {
        Mode::Feedback if let Some(Workspace::Cloned(path)) = workspace => {
            let path = path.clone();
            let left_work = tokio::task::spawn_blocking(move || left_uncommitted(&path))
                .await
                .unwrap_or(true);
            (Vec::new(), left_work)
        }
        // A turn in a copy that could not be made, and a compact turn, never touched the tree.
        Mode::Feedback if request.compact.is_some() || request.workspace == Some(Plan::Copy) => {
            (Vec::new(), false)
        }
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
                // A change in flight holds the store's mutex until its files are staged.
                crate::knowledge::settle();
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

/// Commits what an update turn staged under `docs/knowledge/`, which never runs git itself.
pub(super) fn commit_staged_knowledge(root: &camino::Utf8Path) -> Result<(), String> {
    // A change in flight holds the store's mutex until its files are staged.
    crate::knowledge::settle();
    let staged = crate::git::git_output(
        root,
        &[
            "diff",
            "--cached",
            "--name-status",
            "--no-renames",
            "--",
            crate::knowledge::DIR,
        ],
    )?;
    if staged.trim().is_empty() {
        return Ok(());
    }
    let message = knowledge_message(&staged);
    crate::git::git_output(
        root,
        &["commit", "-m", &message, "--", crate::knowledge::DIR],
    )
    .map(|_| ())
    .map_err(|error| {
        format!(
            "the knowledge changes could not be committed, so they were discarded with \
                 the rest of the tree: {error}"
        )
    })
}

/// The message of the commit that carries `staged`, the `--name-status` lines of the index.
fn knowledge_message(staged: &str) -> String {
    let (mut added, mut changed, mut removed) = (0, 0, 0);
    for line in staged.lines() {
        match line.chars().next() {
            Some('A') => added += 1,
            Some('D') => removed += 1,
            Some(_) => changed += 1,
            None => {}
        }
    }
    let count = |n: usize, one: &str, many: &str| match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    };
    let summary: Vec<String> = [
        count(added, "entry added", "entries added"),
        count(changed, "entry changed", "entries changed"),
        count(removed, "entry removed", "entries removed"),
    ]
    .into_iter()
    .flatten()
    .collect();
    match summary.is_empty() {
        true => format!("docs(knowledge): update\n\n{}", staged.trim_end()),
        false => format!(
            "docs(knowledge): update\n\n{}\n\n{}",
            summary.join(", "),
            staged.trim_end()
        ),
    }
}

/// Whether a copy holds work its turn left uncommitted; unknown counts as yes.
fn left_uncommitted(copy: &camino::Utf8Path) -> bool {
    match crate::git::status_short(copy) {
        Ok(status) => !status.trim().is_empty(),
        Err(e) => {
            tracing::warn!("cannot check the copy after the turn: {e}");
            true
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
    use crate::util::test_support::commit;
    use crate::util::test_support::git;
    use crate::util::test_support::temp_repo;

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

    /// A repository with a commit, and a knowledge entry staged for it.
    fn repo_with_a_staged_entry() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let (tmp, root) = temp_repo();
        std::fs::create_dir_all(root.join("src")).unwrap();
        commit(&root, "src/main.rs", "fn main() {}\n", "init");
        let dir = root.join(crate::knowledge::DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("an-entry.md"),
            "---\ntitle: An entry\nsummary: A summary.\n---\n",
        )
        .unwrap();
        git(&root, &["add", "-A"]);
        (tmp, root)
    }

    #[test]
    fn closing_an_update_turn_commits_the_knowledge_it_staged() {
        let (_tmp, root) = repo_with_a_staged_entry();
        let before = git(&root, &["rev-parse", "HEAD"]);
        commit_staged_knowledge(&root).unwrap();

        assert_eq!(
            git(&root, &["log", "-1", "--format=%s"]),
            "docs(knowledge): update"
        );
        assert_eq!(
            git(&root, &["log", "-1", "--format=%b"]),
            "1 entry added\n\nA\tdocs/knowledge/an-entry.md"
        );
        assert_eq!(
            git(&root, &["show", "--name-only", "--format=", "HEAD"]),
            "docs/knowledge/an-entry.md"
        );
        assert_eq!(git(&root, &["rev-parse", "HEAD~1"]), before);
        assert_eq!(crate::git::status_short(&root).unwrap(), "");
    }

    #[test]
    fn closing_commits_only_the_knowledge_and_leaves_other_work_uncommitted() {
        let (_tmp, root) = repo_with_a_staged_entry();
        std::fs::write(root.join("src/main.rs"), "fn main() { }\n").unwrap();
        std::fs::write(root.join("note.txt"), "scratch").unwrap();
        git(&root, &["add", "-A"]);
        commit_staged_knowledge(&root).unwrap();

        assert_eq!(
            git(&root, &["show", "--name-only", "--format=", "HEAD"]),
            "docs/knowledge/an-entry.md"
        );
        // The rest is still the turn's to discard.
        let left = crate::git::status_short(&root).unwrap();
        assert!(left.contains("src/main.rs"), "{left}");
        assert!(left.contains("note.txt"), "{left}");
        assert!(discard_turn_work(&root));
        assert_eq!(crate::git::status_short(&root).unwrap(), "");
    }

    #[test]
    fn closing_with_nothing_staged_makes_no_commit() {
        let (_tmp, root) = temp_repo();
        let head = commit(&root, "a.txt", "1", "init");
        commit_staged_knowledge(&root).unwrap();
        assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
        // Work outside the store is not the store's to commit.
        std::fs::write(root.join("a.txt"), "2").unwrap();
        git(&root, &["add", "-A"]);
        commit_staged_knowledge(&root).unwrap();
        assert_eq!(git(&root, &["rev-parse", "HEAD"]), head);
    }

    #[test]
    fn a_message_says_what_the_staged_knowledge_holds() {
        let staged = "A\tdocs/knowledge/new.md\nM\tdocs/knowledge/a.md\n\
                      M\tdocs/knowledge/a/b.md\nD\tdocs/knowledge/old.md";
        assert_eq!(
            knowledge_message(staged),
            "docs(knowledge): update\n\n1 entry added, 2 entries changed, 1 entry removed\n\n\
             A\tdocs/knowledge/new.md\nM\tdocs/knowledge/a.md\nM\tdocs/knowledge/a/b.md\n\
             D\tdocs/knowledge/old.md"
        );
        assert_eq!(
            knowledge_message("M\tdocs/knowledge/a.md\n"),
            "docs(knowledge): update\n\n1 entry changed\n\nM\tdocs/knowledge/a.md"
        );
    }

    #[test]
    fn a_hook_that_rejects_the_commit_fails_with_the_knowledge_said_to_be_discarded() {
        let (_tmp, root) = repo_with_a_staged_entry();
        let hook = root.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

        let error = commit_staged_knowledge(&root).unwrap_err();
        assert!(error.contains("discarded with the rest"), "{error}");
        // Nothing was committed, and the change is still staged for the caller to discard.
        assert_eq!(git(&root, &["log", "-1", "--format=%s"]), "init",);
        assert!(!crate::git::status_short(&root).unwrap().is_empty());
    }
}
