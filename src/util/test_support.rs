//! Builders shared by the test modules of every layer.

/// A fresh temporary directory and its canonical UTF-8 path.
pub(crate) fn temp_root() -> (tempfile::TempDir, camino::Utf8PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = camino::Utf8PathBuf::from_path_buf(tmp.path().canonicalize().unwrap()).unwrap();
    (tmp, root)
}

/// Runs `git` in `root`, asserting it succeeds, and returns its trimmed output.
pub(crate) fn git(root: &camino::Utf8Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// A fresh repository without commits.
pub(crate) fn temp_repo() -> (tempfile::TempDir, camino::Utf8PathBuf) {
    let (tmp, root) = temp_root();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.name", "test"]);
    git(&root, &["config", "user.email", "test@example.com"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    (tmp, root)
}

/// Commits `content` as `file` and returns the new commit's id.
pub(crate) fn commit(root: &camino::Utf8Path, file: &str, content: &str, message: &str) -> String {
    std::fs::write(root.join(file), content).unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", message]);
    git(root, &["rev-parse", "HEAD"])
}
