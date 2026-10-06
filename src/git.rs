use gix::ObjectId;
use gix::diff::blob::Algorithm;
use gix::object::tree::diff::ChangeDetached;
use gix::objs::tree::EntryKind;
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffKind {
    FileHeader,
    File,
    Hunk,
    Add,
    Del,
    Context,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub id: [u8; 20],
    pub time_secs: i64,
    pub time_offset_secs: i32,
    pub summary: String,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum RefKind {
    Branch,
    RemoteBranch,
    Tag,
}

#[derive(Clone, Debug)]
pub struct GitRef {
    pub kind: RefKind,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct LogHistory {
    pub entries: Vec<LogEntry>,
    pub truncated: bool,
    pub refs: HashMap<[u8; 20], Vec<GitRef>>,
}

pub const MAX_LOG_ENTRIES: usize = 10_000;

pub const COMMIT_TYPES: &[&str] = &[
    "feat", "fix", "docs", "style", "refactor", "perf", "test", "build", "ci", "chore", "revert",
];

pub fn commit_type_prefix(summary: &str) -> Option<(&'static str, usize)> {
    let colon = summary.find(':')?;
    let head = &summary[..colon];
    if head.contains(' ') {
        return None;
    }
    let head = head.strip_suffix('!').unwrap_or(head);
    if let Some(open) = head.find('(') {
        if !head.ends_with(')') {
            return None;
        }
        let ty = head.get(..open)?;
        let ty = COMMIT_TYPES.iter().find(|candidate| **candidate == ty)?;
        Some((ty, colon))
    } else {
        let ty = COMMIT_TYPES.iter().find(|candidate| **candidate == head)?;
        Some((ty, colon))
    }
}

pub fn format_log_time(time_secs: i64, time_offset_secs: i32) -> String {
    let local_secs = time_secs + time_offset_secs as i64;
    let days = local_secs.div_euclid(86_400);
    let secs_of_day = local_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

pub fn classify_diff(text: &str) -> Vec<DiffLine> {
    text.lines()
        .map(|line| {
            let kind = if line.starts_with("diff --git ") || line.starts_with("Binary files ") {
                DiffKind::FileHeader
            } else if line.starts_with("@@") {
                DiffKind::Hunk
            } else if line.starts_with("+++") || line.starts_with("---") {
                DiffKind::File
            } else if line.starts_with('+') {
                DiffKind::Add
            } else if line.starts_with('-') {
                DiffKind::Del
            } else {
                DiffKind::Context
            };
            DiffLine {
                kind,
                text: line.to_string(),
            }
        })
        .collect()
}

pub fn short_id(id: &[u8; 20]) -> String {
    format!("{}{:x}", hex::encode(&id[..3]), id[3] >> 4)
}

/// Days since the Unix epoch for a proleptic Gregorian date; inverse of `civil_from_days`.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn load_log(
    repo: &gix::ThreadSafeRepository,
    branch: Option<&str>,
) -> Result<LogHistory, String> {
    let repo = repo.to_thread_local();
    let commit = match branch {
        Some(name) => repo
            .rev_parse_single(name)
            .map_err(|e| format!("resolve {name}: {e}"))?
            .object()
            .map_err(|e| format!("read {name}: {e}"))?
            .peel_to_commit()
            .map_err(|e| format!("peel {name}: {e}"))?,
        None => {
            if let Ok(head) = repo.head()
                && head.is_unborn()
            {
                return Ok(LogHistory {
                    entries: Vec::new(),
                    truncated: false,
                    refs: HashMap::new(),
                });
            }
            repo.head_commit()
                .map_err(|e| format!("resolve HEAD: {e}"))?
        }
    };
    let walk = commit
        .ancestors()
        .first_parent_only()
        .all()
        .map_err(|e| format!("revision walk: {e}"))?;

    let mut entries = Vec::new();
    let mut truncated = false;
    for info in walk {
        if entries.len() == MAX_LOG_ENTRIES {
            truncated = true;
            break;
        }
        let info = info.map_err(|e| format!("revision walk: {e}"))?;
        let commit = info
            .object()
            .map_err(|e| format!("read commit {}: {e}", info.id))?;
        let time = commit
            .committer()
            .ok()
            .and_then(|sig| sig.time().ok())
            .unwrap_or_default();
        let first_line = commit
            .message_raw_sloppy()
            .split(|&b| b == b'\n')
            .next()
            .unwrap_or(b"");
        let mut id = [0u8; 20];
        id.copy_from_slice(info.id.as_bytes());
        entries.push(LogEntry {
            id,
            time_secs: time.seconds,
            time_offset_secs: time.offset,
            summary: String::from_utf8_lossy(first_line).into_owned(),
        });
    }
    let refs = collect_refs(&repo)?;
    Ok(LogHistory {
        entries,
        truncated,
        refs,
    })
}

fn classify_ref_name(bytes: &[u8]) -> Option<(RefKind, String)> {
    if bytes == b"HEAD" {
        return None;
    }
    if let Some(short) = bytes.strip_prefix(b"refs/heads/".as_slice()) {
        return Some((RefKind::Branch, String::from_utf8_lossy(short).into_owned()));
    }
    if let Some(short) = bytes.strip_prefix(b"refs/remotes/".as_slice()) {
        return Some((
            RefKind::RemoteBranch,
            String::from_utf8_lossy(short).into_owned(),
        ));
    }
    if let Some(short) = bytes.strip_prefix(b"refs/tags/".as_slice()) {
        return Some((RefKind::Tag, String::from_utf8_lossy(short).into_owned()));
    }
    None
}

fn collect_refs(repo: &gix::Repository) -> Result<HashMap<[u8; 20], Vec<GitRef>>, String> {
    let mut refs: HashMap<[u8; 20], Vec<GitRef>> = HashMap::new();
    let platform = repo
        .references()
        .map_err(|e| format!("list references: {e}"))?;
    let iter = platform
        .all()
        .map_err(|e| format!("list references: {e}"))?;
    for reference in iter {
        let mut reference = reference.map_err(|e| format!("read reference: {e}"))?;
        let Some((kind, name)) = classify_ref_name(reference.name().as_bstr()) else {
            continue;
        };
        let id = reference
            .peel_to_id()
            .map_err(|e| format!("peel ref: {e}"))?;
        let Ok(id20) = <[u8; 20]>::try_from(id.as_bytes()) else {
            continue;
        };
        refs.entry(id20).or_default().push(GitRef { kind, name });
    }
    for list in refs.values_mut() {
        list.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
    }
    Ok(refs)
}

pub fn load_refs(repo: &gix::ThreadSafeRepository) -> Result<Vec<GitRef>, String> {
    let repo = repo.to_thread_local();
    let platform = repo
        .references()
        .map_err(|e| format!("list references: {e}"))?;
    let iter = platform
        .all()
        .map_err(|e| format!("list references: {e}"))?;
    let mut refs = Vec::new();
    for reference in iter {
        let reference = reference.map_err(|e| format!("read reference: {e}"))?;
        let Some((kind, name)) = classify_ref_name(reference.name().as_bstr()) else {
            continue;
        };
        refs.push(GitRef { kind, name });
    }
    refs.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
    Ok(refs)
}

/// Full hex id of the commit HEAD points at; `None` on an unborn branch.
pub fn head_id(repo: &gix::ThreadSafeRepository) -> Option<String> {
    repo.to_thread_local()
        .head_id()
        .ok()
        .map(|id| id.to_string())
}

/// `<short hash> <title>` of the commits reachable from `to` and not from `from`,
/// newest first, at most `cap`. Shells out to `git`; empty on any failure.
pub fn log_oneline(root: &camino::Utf8Path, from: &str, to: &str, cap: usize) -> Vec<String> {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(["log", "--format=%h %s", "-n"])
        .arg(cap.to_string())
        .arg(format!("{from}..{to}"))
        .output();
    match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn git_output(root: &camino::Utf8Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {}: {}", args.join(" "), stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `git status --short`, untracked files included. Empty means a clean working tree.
pub fn status_short(root: &camino::Utf8Path) -> Result<String, String> {
    git_output(root, &["status", "--short", "--untracked-files=all"])
}

/// Full ids of the commits reachable from `after` and not from `before`, oldest first.
/// Covers amended and rebased history; with no `before`, everything up to `after`.
pub fn commits_between(
    root: &camino::Utf8Path,
    before: Option<&str>,
    after: &str,
) -> Result<Vec<String>, String> {
    let range = match before {
        Some(before) => format!("{before}..{after}"),
        None => after.to_string(),
    };
    let listed = git_output(root, &["rev-list", "--reverse", &range])?;
    Ok(listed.lines().map(str::to_string).collect())
}

/// Stashes every change, untracked files included, and returns the stash commit id.
pub fn stash_push(root: &camino::Utf8Path, message: &str) -> Result<String, String> {
    git_output(
        root,
        &["stash", "push", "--include-untracked", "--message", message],
    )?;
    let id = git_output(root, &["rev-parse", "refs/stash"])?;
    Ok(id.trim().to_string())
}

/// Hard-resets tracked files and removes untracked ones. Ignored files are kept.
/// Works on a repository without commits, where there is nothing to reset.
pub fn discard_worktree(root: &camino::Utf8Path) -> Result<(), String> {
    if git_output(root, &["rev-parse", "--verify", "HEAD"]).is_ok() {
        git_output(root, &["reset", "--hard", "HEAD"])?;
    }
    git_output(root, &["clean", "-fd"]).map(|_| ())
}

/// Whether the repository has uncommitted changes (staged or unstaged). Untracked
/// files do not count, matching `gix::Repository::is_dirty`'s own definition.
pub fn is_dirty(repo: &gix::ThreadSafeRepository) -> Result<bool, String> {
    repo.to_thread_local()
        .is_dirty()
        .map_err(|e| format!("check status: {e}"))
}

pub fn load_commit_diff(repo: &gix::ThreadSafeRepository, id: [u8; 20]) -> Result<String, String> {
    let repo = repo.to_thread_local();
    let commit = repo
        .find_commit(ObjectId::from(id))
        .map_err(|e| format!("find commit: {e}"))?;

    let parent_tree = match commit.parent_ids().next() {
        Some(parent_id) => {
            let parent = repo
                .find_commit(parent_id)
                .map_err(|e| format!("parent tree: {e}"))?;
            parent.tree().map_err(|e| format!("parent tree: {e}"))?
        }
        None => repo.empty_tree(),
    };
    let tree = commit.tree().map_err(|e| format!("commit tree: {e}"))?;

    let mut options = gix::diff::Options::default();
    options.track_rewrites(None);
    let changes = repo
        .diff_tree_to_tree(Some(&parent_tree), Some(&tree), Some(options))
        .map_err(|e| format!("tree diff: {e}"))?;
    let algorithm = repo
        .diff_algorithm()
        .map_err(|e| format!("diff algorithm: {e}"))?;

    let mut out = String::new();
    for change in changes {
        let (location, source_location, old_id, new_id, entry_mode) = match change {
            ChangeDetached::Modification {
                location,
                previous_id,
                id,
                entry_mode,
                ..
            } => (location, None, Some(previous_id), Some(id), entry_mode),
            ChangeDetached::Addition {
                location,
                id,
                entry_mode,
                ..
            } => (location, None, None, Some(id), entry_mode),
            ChangeDetached::Deletion {
                location,
                id,
                entry_mode,
                ..
            } => (location, None, Some(id), None, entry_mode),
            ChangeDetached::Rewrite {
                location,
                source_location,
                source_id,
                id,
                entry_mode,
                ..
            } => (
                location,
                Some(source_location),
                Some(source_id),
                Some(id),
                entry_mode,
            ),
        };
        let path = String::from_utf8_lossy(&location).into_owned();
        if matches!(
            entry_mode.kind(),
            EntryKind::Blob | EntryKind::BlobExecutable
        ) {
            let source_path = source_location
                .as_deref()
                .map(|p| String::from_utf8_lossy(p).into_owned());
            file_diff(
                &repo,
                algorithm,
                &path,
                source_path.as_deref(),
                old_id,
                new_id,
                &mut out,
            )?;
        }
    }
    Ok(out)
}

fn file_diff(
    repo: &gix::Repository,
    algorithm: Algorithm,
    path: &str,
    source_path: Option<&str>,
    old_id: Option<ObjectId>,
    new_id: Option<ObjectId>,
    out: &mut String,
) -> Result<(), String> {
    let old_data = match old_id {
        Some(id) => blob_data(repo, id)?,
        None => Vec::new(),
    };
    let new_data = match new_id {
        Some(id) => blob_data(repo, id)?,
        None => Vec::new(),
    };

    let old_label = if old_id.is_none() {
        "/dev/null".to_string()
    } else {
        format!("a/{}", source_path.unwrap_or(path))
    };
    let new_label = if new_id.is_none() {
        "/dev/null".to_string()
    } else {
        format!("b/{path}")
    };
    out.push_str(&format!(
        "diff --git a/{} b/{path}\n",
        source_path.unwrap_or(path)
    ));
    out.push_str(&format!("--- {old_label}\n"));
    out.push_str(&format!("+++ {new_label}\n"));

    let binary = old_data.contains(&0)
        || new_data.contains(&0)
        || std::str::from_utf8(&old_data).is_err()
        || std::str::from_utf8(&new_data).is_err();
    if binary {
        out.push_str("Binary files differ\n");
        return Ok(());
    }

    let input = gix::diff::blob::InternedInput::new(old_data.as_slice(), new_data.as_slice());
    let diff = gix::diff::blob::diff_with_slider_heuristics(algorithm, &input);
    let unified = gix::diff::blob::UnifiedDiff::new(
        &diff,
        &input,
        gix::diff::blob::unified_diff::ConsumeBinaryHunk::new(String::new(), "\n"),
        gix::diff::blob::unified_diff::ContextSize::default(),
    );
    let hunks = unified.consume().map_err(|e| format!("render diff: {e}"))?;
    out.push_str(&hunks);
    Ok(())
}

fn blob_data(repo: &gix::Repository, id: ObjectId) -> Result<Vec<u8>, String> {
    Ok(repo
        .find_object(id)
        .map_err(|e| format!("find object {id}: {e}"))?
        .try_into_blob()
        .map_err(|e| format!("load blob {id}: {e}"))?
        .data
        .clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_log_time_renders_local_datetime() {
        assert_eq!(format_log_time(0, 0), "1970-01-01 00:00");
        assert_eq!(format_log_time(1_700_000_000, 0), "2023-11-14 22:13");
        assert_eq!(format_log_time(-1, 0), "1969-12-31 23:59");
        assert_eq!(format_log_time(0, 7_200), "1970-01-01 02:00");
        assert_eq!(format_log_time(0, -3_600), "1969-12-31 23:00");
    }

    #[test]
    fn classify_diff_assigns_kinds_by_prefix() {
        let lines = classify_diff(
            "diff --git a/src/lib.rs b/src/lib.rs\n\
             --- a/src/lib.rs\n\
             +++ b/src/lib.rs\n\
             @@ -1,3 +1,4 @@\n\
             \x20context line\n\
             -removed line\n\
             +added line\n\
             \\ No newline at end of file\n",
        );
        let kinds: Vec<DiffKind> = lines.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffKind::FileHeader,
                DiffKind::File,
                DiffKind::File,
                DiffKind::Hunk,
                DiffKind::Context,
                DiffKind::Del,
                DiffKind::Add,
                DiffKind::Context,
            ]
        );
        assert_eq!(lines[0].text, "diff --git a/src/lib.rs b/src/lib.rs");
        assert_eq!(lines[5].text, "-removed line");
    }

    #[test]
    fn classify_diff_handles_empty_and_added_files() {
        assert!(classify_diff("").is_empty());
        let lines = classify_diff("Binary files a/img.png and b/img.png\n");
        assert_eq!(lines[0].kind, DiffKind::FileHeader);
    }

    #[test]
    fn short_id_is_seven_hex_chars() {
        let id = [0xabu8; 20];
        assert_eq!(short_id(&id), "abababa");
    }

    fn temp_repo() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        git_output(&root, &["init", "-q"]).unwrap();
        git_output(&root, &["config", "user.name", "test"]).unwrap();
        git_output(&root, &["config", "user.email", "t@example.com"]).unwrap();
        (tmp, root)
    }

    fn commit(root: &camino::Utf8Path, file: &str, content: &str, message: &str) -> String {
        std::fs::write(root.join(file), content).unwrap();
        git_output(root, &["add", "-A"]).unwrap();
        git_output(root, &["commit", "-q", "-m", message]).unwrap();
        git_output(root, &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string()
    }

    #[test]
    fn discard_worktree_restores_head_and_removes_untracked_but_not_ignored() {
        let (_tmp, root) = temp_repo();
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        let head = commit(&root, "a.txt", "one", "init");
        std::fs::write(root.join("a.txt"), "changed").unwrap();
        std::fs::write(root.join("scratch.txt"), "untracked").unwrap();
        std::fs::write(root.join("ignored.txt"), "build output").unwrap();

        discard_worktree(&root).unwrap();

        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "one");
        assert!(!root.join("scratch.txt").exists());
        assert!(root.join("ignored.txt").exists());
        assert_eq!(
            git_output(&root, &["rev-parse", "HEAD"]).unwrap().trim(),
            head
        );
        assert_eq!(status_short(&root).unwrap(), "");
    }

    #[test]
    fn discard_worktree_works_without_any_commit() {
        let (_tmp, root) = temp_repo();
        std::fs::write(root.join("a.txt"), "never committed").unwrap();

        discard_worktree(&root).unwrap();

        assert!(!root.join("a.txt").exists());
        assert_eq!(status_short(&root).unwrap(), "");
    }

    #[test]
    fn discard_worktree_leaves_a_clean_tree_alone() {
        let (_tmp, root) = temp_repo();
        let head = commit(&root, "a.txt", "one", "init");

        discard_worktree(&root).unwrap();

        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "one");
        assert_eq!(
            git_output(&root, &["rev-parse", "HEAD"]).unwrap().trim(),
            head
        );
    }

    #[test]
    fn load_log_walks_this_repository_head() {
        let root = camino::Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo = gix::ThreadSafeRepository::open(root.as_std_path()).expect("open repo");
        let history = load_log(&repo, None).expect("load_log");
        let entries = &history.entries;
        assert!(!entries.is_empty(), "this repo has commits");
        assert!(
            entries.iter().all(|e| !e.summary.is_empty()),
            "every entry has a non-empty summary"
        );
        assert!(entries[0].id != [0u8; 20]);
    }

    #[test]
    fn load_commit_diff_renders_unified_text_for_head() {
        let root = camino::Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo = gix::ThreadSafeRepository::open(root.as_std_path()).expect("open repo");
        let history = load_log(&repo, None).expect("load_log");
        let diff = load_commit_diff(&repo, history.entries[0].id).expect("load_commit_diff");
        let lines = classify_diff(&diff);
        if !lines.is_empty() {
            assert_eq!(lines[0].kind, DiffKind::FileHeader);
            assert!(lines[0].text.starts_with("diff --git"));
        }
    }

    #[test]
    fn commit_type_prefix_parses_conventional_summaries() {
        assert_eq!(commit_type_prefix("feat: add pane"), Some(("feat", 4)));
        assert_eq!(
            commit_type_prefix("feat(tui)!: add pane"),
            Some(("feat", 10))
        );
        assert_eq!(commit_type_prefix("fix(scope): x"), Some(("fix", 10)));
        assert_eq!(commit_type_prefix("chore!: x"), Some(("chore", 6)));
        assert_eq!(commit_type_prefix("weird: x"), None);
        assert_eq!(commit_type_prefix("notatype: x"), None);
        assert_eq!(commit_type_prefix("no colon here"), None);
        assert_eq!(commit_type_prefix("feat (x): y"), None);
        assert_eq!(commit_type_prefix(": empty type"), None);
    }

    #[test]
    fn load_log_reports_no_truncation_for_this_repo() {
        let root = camino::Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo = gix::ThreadSafeRepository::open(root.as_std_path()).expect("open repo");
        let history = load_log(&repo, None).expect("load_log");
        assert!(!history.truncated);
        assert!(history.entries.len() < MAX_LOG_ENTRIES);
        assert!(!history.refs.is_empty());
        let head_refs = history.refs.get(&history.entries[0].id).expect("HEAD refs");
        assert!(
            head_refs
                .iter()
                .any(|r| r.kind == RefKind::Branch && r.name == "main")
        );
    }

    #[test]
    fn load_log_branch_resolves_and_load_refs_lists() {
        let root = camino::Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo = gix::ThreadSafeRepository::open(root.as_std_path()).expect("open repo");
        let head = load_log(&repo, None).expect("load_log");
        let main = load_log(&repo, Some("main")).expect("load_log main");
        assert_eq!(head.entries.len(), main.entries.len());
        assert_eq!(head.entries[0].id, main.entries[0].id);
        assert!(load_log(&repo, Some("no-such-ref-xyz")).is_err());
        let refs = load_refs(&repo).expect("load_refs");
        assert!(
            refs.iter()
                .any(|r| r.kind == RefKind::Branch && r.name == "main")
        );
    }

    #[test]
    fn load_log_resolves_a_commit_hash_as_starting_point() {
        let root = camino::Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo = gix::ThreadSafeRepository::open(root.as_std_path()).expect("open repo");
        let head = load_log(&repo, None).expect("load_log");
        let hex = hex::encode(head.entries[5].id);
        for take in [7, 12, 40] {
            let abbrev: String = hex.chars().take(take).collect();
            let at = load_log(&repo, Some(&abbrev))
                .unwrap_or_else(|e| panic!("load_log at {take}-char prefix: {e}"));
            assert_eq!(at.entries[0].id, head.entries[5].id);
            assert!(at.entries.len() < head.entries.len());
        }
    }

    #[test]
    fn load_log_returns_empty_for_unborn_head() {
        let dir = tempfile::TempDir::new().expect("temp dir");
        gix::init(dir.path()).expect("init");
        let root = camino::Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf8");
        let repo = gix::ThreadSafeRepository::open(root.as_std_path()).expect("open repo");
        let history = load_log(&repo, None).expect("empty log");
        assert!(history.entries.is_empty());
        assert!(history.refs.is_empty());
    }
}
