//! The workspace of a feedback turn: a copy of the repository, `.git` and `target/`
//! included, made with reflinks so it costs metadata and not the gigabytes of a build
//! cache. The turn's agent runs in it through `explorer sandbox-exec`, which shows it at
//! the repository's own path, so an accidental write from the agent cannot reach the
//! repository, and a build in it finds every artifact fresh.
//!
//! The copy lives under `<root>/target/explorer`, which it leaves out of itself, and is
//! only made where it pays: a repository with no build cache yet, a cache that is not
//! under `target/`, a filesystem without reflinks or a host without user namespaces
//! run the turn in place, and say why.
//!
//! Cargo takes a build for fresh by comparing mtimes, so every file keeps its own, to the
//! nanosecond; a file that did not would rebuild the world.

use std::fs::File;
use std::io;

const DIR: &str = "target/explorer";

/// A copy that is still being made; no turn ever sees one.
const BUILDING: &str = ".building-";

#[derive(Debug)]
pub enum Workspace {
    Cloned(camino::Utf8PathBuf),
    InPlace { reason: String },
}

#[derive(Clone, Debug)]
pub struct Sandbox {
    pub root: camino::Utf8PathBuf,
    pub workspace: camino::Utf8PathBuf,
}

impl Sandbox {
    /// Starts in `/`: an inherited cwd would name the real repository.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(explorer());
        command
            .args(["sandbox-exec", "--root"])
            .arg(&self.root)
            .arg("--workspace")
            .arg(&self.workspace)
            .arg("--")
            .arg(program)
            .current_dir("/");
        command
    }
}

fn explorer() -> std::path::PathBuf {
    // The program of this process, even if the file was replaced since it started.
    #[cfg(not(test))]
    return std::path::PathBuf::from("/proc/self/exe");
    #[cfg(test)]
    return crate::sandbox::built_explorer();
}

impl Workspace {
    pub fn prepare(root: &camino::Utf8Path, node: &str) -> Self {
        match copy_for(root, node) {
            Ok(path) => Self::Cloned(path),
            Err(reason) => Self::InPlace {
                reason: crate::util::text::single_line(&reason),
            },
        }
    }

    pub fn sandbox(&self, root: &camino::Utf8Path) -> Option<Sandbox> {
        match self {
            Self::Cloned(workspace) => Some(Sandbox {
                root: root.to_path_buf(),
                workspace: workspace.clone(),
            }),
            Self::InPlace { .. } => None,
        }
    }

    pub fn in_place_reason(&self) -> Option<&str> {
        match self {
            Self::Cloned(_) => None,
            Self::InPlace { reason } => Some(reason),
        }
    }

    /// A deletion cut short is finished by `sweep` at the next start.
    pub fn release(self) {
        if let Self::Cloned(path) = self {
            std::thread::spawn(move || {
                let _ = remove_copy(&path);
            });
        }
    }
}

fn eligible(root: &camino::Utf8Path) -> Result<(), String> {
    use std::sync::OnceLock;
    let target = root.join("target");
    let meta = std::fs::symlink_metadata(&target);
    if meta.is_err() {
        return Err("no target/ directory, so nothing is built to reuse".to_string());
    }
    // A symlink here would send the agent's builds outside the copy.
    if !meta.is_ok_and(|meta| meta.is_dir()) {
        return Err("target/ is not a real directory, so builds do not use it".to_string());
    }
    // A link here would put the copies, and the deletion of what is left, outside the repository.
    let workspaces = root.join(DIR);
    if std::fs::symlink_metadata(&workspaces).is_ok_and(|meta| !meta.is_dir()) {
        return Err(
            "target/explorer is not a directory, so copies would leave the repository".to_string(),
        );
    }
    let cached = std::fs::read_dir(&target)
        .map_err(|e| format!("cannot read target/: {e}"))?
        .filter_map(Result::ok)
        .any(|entry| entry.file_name() != "explorer");
    if !cached {
        return Err("target/ holds no build yet, so a copy would save nothing".to_string());
    }
    // A git config that names the real tree would make git report it instead of the copy.
    if names_a_worktree_outside(root)? {
        return Err(
            "core.worktree names a tree outside the copy, which git would report instead"
                .to_string(),
        );
    }
    // The namespaces work or not for the whole process.
    static SANDBOX: OnceLock<Result<(), String>> = OnceLock::new();
    SANDBOX
        .get_or_init(|| sandbox_check(root))
        .clone()
        .map_err(|reason| format!("the sandbox does not work here: {reason}"))
}

fn sandbox_check(root: &camino::Utf8Path) -> Result<(), String> {
    // Only namespaces are checked here, so any existing directory inside the root will do.
    let output = std::process::Command::new(explorer())
        .args(["sandbox-exec", "--root"])
        .arg(root)
        .arg("--workspace")
        .arg(root.join("target"))
        .arg("--check")
        .current_dir("/")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(stderr
        .trim()
        .trim_start_matches("sandbox-exec: ")
        .to_string())
}

fn copy_for(root: &camino::Utf8Path, node: &str) -> Result<camino::Utf8PathBuf, String> {
    if ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"]
        .iter()
        .any(|var| std::env::var_os(var).is_some())
    {
        return Err("a cargo target directory is set, so builds do not use target/".to_string());
    }
    eligible(root)?;
    copy_into_place(root, node, &clone_file)
}

/// Built as `.building-…` and renamed, so no turn reads a partial copy.
fn copy_into_place(
    root: &camino::Utf8Path,
    node: &str,
    clone: &(dyn Fn(&File, &File) -> io::Result<()> + Sync),
) -> Result<camino::Utf8PathBuf, String> {
    let parent = root.join(DIR);
    std::fs::create_dir_all(&parent).map_err(|e| format!("cannot create {parent}: {e}"))?;
    let name = format!("{}-{node}", std::process::id());
    let building = parent.join(format!("{BUILDING}{name}"));
    let done = parent.join(&name);
    let _ = remove_copy(&building);
    let copied = clone_tree(root, &building, clone).and_then(|()| {
        std::fs::rename(&building, &done).map_err(|e| format!("cannot move the copy: {e}"))
    });
    match copied {
        Ok(()) => Ok(done),
        Err(reason) => {
            let _ = remove_copy(&building);
            Err(reason)
        }
    }
}

/// Deletes a copy; read-only directories in it are made writable first, or their entries stay.
fn remove_copy(path: &camino::Utf8Path) -> io::Result<()> {
    make_writable(path);
    std::fs::remove_dir_all(path)
}

fn make_writable(path: &camino::Utf8Path) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(meta.mode() | 0o700));
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let Ok(child) = camino::Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        make_writable(&child);
    }
}

fn clone_file(dest: &File, source: &File) -> io::Result<()> {
    rustix::fs::ioctl_ficlone(dest, source).map_err(io::Error::from)
}

struct Found {
    dirs: Vec<camino::Utf8PathBuf>,
    files: Vec<camino::Utf8PathBuf>,
    links: Vec<camino::Utf8PathBuf>,
}

fn walk(root: &camino::Utf8Path) -> Result<Found, String> {
    let target = root.join("target").into_std_path_buf();
    let mut found = Found {
        dirs: Vec::new(),
        files: Vec::new(),
        links: Vec::new(),
    };
    let walker = jwalk::WalkDir::new(root)
        .skip_hidden(false)
        .follow_links(false)
        .process_read_dir(move |_, dir, _, children| {
            if dir == target {
                children.retain(|entry| entry.as_ref().map_or(true, |e| e.file_name != "explorer"));
            }
        });
    for entry in walker {
        let entry = entry.map_err(|e| format!("cannot walk the repository: {e}"))?;
        let kind = entry.file_type();
        let path = camino::Utf8PathBuf::from_path_buf(entry.path())
            .map_err(|path| format!("{} is not valid UTF-8", path.display()))?;
        let relative = path
            .strip_prefix(root)
            .expect("the walk stays under its root")
            .to_path_buf();
        if relative.as_str().is_empty() {
            continue;
        }
        if !kind.is_dir() && entry.file_name() == ".git" && !names_a_copied_git_dir(root, &path) {
            return Err(format!(
                "{relative} names a git directory outside the copy, which would share its data"
            ));
        }
        if kind.is_dir() {
            found.dirs.push(relative);
        } else if kind.is_symlink() {
            found.links.push(relative);
        } else if kind.is_file() {
            found.files.push(relative);
        }
    }
    Ok(found)
}

fn names_a_worktree_outside(root: &camino::Utf8Path) -> Result<bool, String> {
    // This repository's own config, includes followed; no environment, user or system files.
    let config = gix::open::permissions::Config {
        includes: true,
        ..gix::open::permissions::Config::isolated()
    };
    let permissions = gix::open::Permissions {
        config,
        ..gix::open::Permissions::isolated()
    };
    let options = gix::open::Options::default().permissions(permissions);
    let repo = gix::open_opts(root.as_std_path(), options)
        .map_err(|e| format!("cannot read the repository's config: {e}"))?;
    // A relative path is taken from the git directory, which the copy keeps.
    Ok(repo
        .config_snapshot()
        .string("core.worktree")
        .is_some_and(|value| matches!(value.first(), Some(b'/' | b'~'))))
}

fn names_a_copied_git_dir(root: &camino::Utf8Path, path: &camino::Utf8Path) -> bool {
    use camino::Utf8Component;
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Some(target) = text.trim().strip_prefix("gitdir:").map(str::trim) else {
        return false;
    };
    let target = camino::Utf8Path::new(target);
    if target.is_absolute() {
        return false;
    }
    let mut depth = path
        .parent()
        .and_then(|dir| dir.strip_prefix(root).ok())
        .map_or(0, |dir| dir.components().count());
    for component in target.components() {
        match component {
            Utf8Component::ParentDir if depth == 0 => return false,
            Utf8Component::ParentDir => depth -= 1,
            Utf8Component::Normal(_) => depth += 1,
            _ => {}
        }
    }
    // A link on the way is not lexical: the copy keeps it, so the target is resolved too.
    let Some(dir) = path.parent() else {
        return false;
    };
    match (
        std::fs::canonicalize(root),
        std::fs::canonicalize(dir.join(target)),
    ) {
        (Ok(root), Ok(git_dir)) => git_dir.starts_with(root),
        _ => true,
    }
}

fn timestamps(meta: &std::fs::Metadata) -> rustix::fs::Timestamps {
    use rustix::fs::{Timespec, Timestamps};
    use std::os::unix::fs::MetadataExt;
    Timestamps {
        last_access: Timespec {
            tv_sec: meta.atime(),
            tv_nsec: meta.atime_nsec(),
        },
        last_modification: Timespec {
            tv_sec: meta.mtime(),
            tv_nsec: meta.mtime_nsec(),
        },
    }
}

fn clone_tree(
    root: &camino::Utf8Path,
    dest: &camino::Utf8Path,
    clone: &(dyn Fn(&File, &File) -> io::Result<()> + Sync),
) -> Result<(), String> {
    use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let found = walk(root)?;
    let at = |what: &str, path: &camino::Utf8Path, error: io::Error| {
        format!("cannot {what} {path}: {error}")
    };
    std::fs::create_dir(dest).map_err(|e| at("create", dest, e))?;
    for dir in &found.dirs {
        let to = dest.join(dir);
        std::fs::create_dir(&to).map_err(|e| at("create", &to, e))?;
    }
    found.files.par_iter().try_for_each(|file| {
        let from = root.join(file);
        let to = dest.join(file);
        let source = File::open(&from).map_err(|e| at("open", &from, e))?;
        let meta = source.metadata().map_err(|e| at("read", &from, e))?;
        let copy = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&to)
            .map_err(|e| at("create", &to, e))?;
        clone(&copy, &source).map_err(|e| at("clone", &from, e))?;
        copy.set_permissions(std::fs::Permissions::from_mode(meta.mode() & 0o7777))
            .map_err(|e| at("set the mode of", &to, e))?;
        rustix::fs::futimens(&copy, &timestamps(&meta)).map_err(|e| at("date", &to, e.into()))
    })?;
    for link in &found.links {
        let from = root.join(link);
        let to = dest.join(link);
        let target = std::fs::read_link(&from).map_err(|e| at("read the link", &from, e))?;
        std::os::unix::fs::symlink(&target, &to).map_err(|e| at("link", &to, e))?;
        let meta = std::fs::symlink_metadata(&from).map_err(|e| at("read", &from, e))?;
        rustix::fs::utimensat(
            rustix::fs::CWD,
            to.as_std_path(),
            &timestamps(&meta),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|e| at("date", &to, e.into()))?;
    }
    // Last, and deepest first: making an entry changes the mtime of its directory.
    let dirs = found
        .dirs
        .iter()
        .rev()
        .map(camino::Utf8PathBuf::as_path)
        .chain([camino::Utf8Path::new("")]);
    for dir in dirs {
        let (from, to) = (root.join(dir), dest.join(dir));
        let meta = std::fs::metadata(&from).map_err(|e| at("read", &from, e))?;
        std::fs::set_permissions(&to, std::fs::Permissions::from_mode(meta.mode() & 0o7777))
            .map_err(|e| at("set the mode of", &to, e))?;
        rustix::fs::utimensat(
            rustix::fs::CWD,
            to.as_std_path(),
            &timestamps(&meta),
            rustix::fs::AtFlags::empty(),
        )
        .map_err(|e| at("date", &to, e.into()))?;
    }
    Ok(())
}

fn alive(pid: u32) -> bool {
    use rustix::io::Errno;
    use rustix::process::Pid;
    let Some(pid) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return false;
    };
    !matches!(rustix::process::test_kill_process(pid), Err(Errno::SRCH))
}

pub fn sweep(root: &camino::Utf8Path) {
    let dir = root.join(DIR);
    if !std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let owner = name.strip_prefix(BUILDING).unwrap_or(&name);
        let Some(pid) = owner.split('-').next().and_then(|pid| pid.parse().ok()) else {
            continue;
        };
        if !alive(pid) {
            let _ = remove_copy(&dir.join(name.as_ref()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::test_support::{git, temp_root, temp_root_in_target};

    /// A clone that copies the bytes, for the tests of everything but the reflink.
    fn plain_copy(dest: &File, source: &File) -> io::Result<()> {
        use std::io::Seek;
        let (mut dest, mut source) = (dest, source);
        source.rewind()?;
        io::copy(&mut source, &mut dest).map(|_| ())
    }

    fn repository() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let (tmp, root) = temp_root();
        let write = |path: &str, text: &str| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("src/main.rs", "fn main() {}");
        write(".git/HEAD", "ref: refs/heads/main");
        write("target/debug/explorer", "binary");
        write("target/debug/deps/a.d", "dep");
        write("target/explorer/other-workspace/file", "not copied");
        write("script.sh", "#!/bin/sh");
        std::fs::set_permissions(
            root.join("script.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::os::unix::fs::symlink("src/main.rs", root.join("relative")).unwrap();
        std::os::unix::fs::symlink("/nowhere/at/all", root.join("dangling")).unwrap();
        // Older than the files around it, as the artifacts of a build are.
        let old = File::open(root.join("src/main.rs")).unwrap();
        let stamp = rustix::fs::Timestamps {
            last_access: rustix::fs::Timespec {
                tv_sec: 1_700_000_000,
                tv_nsec: 123_456_789,
            },
            last_modification: rustix::fs::Timespec {
                tv_sec: 1_700_000_000,
                tv_nsec: 987_654_321,
            },
        };
        rustix::fs::futimens(&old, &stamp).unwrap();
        (tmp, root)
    }

    #[test]
    fn the_copy_keeps_files_links_modes_and_times_and_leaves_out_target_explorer() {
        use std::os::unix::fs::MetadataExt;
        let (_tmp, root) = repository();
        let (_copied, elsewhere) = temp_root();
        let copy = elsewhere.join("copy");
        clone_tree(&root, &copy, &plain_copy).unwrap();

        assert_eq!(
            std::fs::read_to_string(copy.join("src/main.rs")).unwrap(),
            "fn main() {}"
        );
        assert_eq!(
            std::fs::read_to_string(copy.join(".git/HEAD")).unwrap(),
            "ref: refs/heads/main"
        );
        // The binary is `target/debug/explorer`: only `target/explorer` itself is left out.
        assert!(copy.join("target/debug/explorer").is_file());
        assert!(copy.join("target/debug/deps/a.d").is_file());
        assert!(!copy.join("target/explorer").exists());

        assert_eq!(
            std::fs::read_link(copy.join("relative")).unwrap(),
            std::path::Path::new("src/main.rs")
        );
        assert_eq!(
            std::fs::read_link(copy.join("dangling")).unwrap(),
            std::path::Path::new("/nowhere/at/all")
        );
        assert_eq!(
            std::fs::metadata(copy.join("script.sh")).unwrap().mode() & 0o777,
            0o755
        );

        let (from, to) = (
            std::fs::metadata(root.join("src/main.rs")).unwrap(),
            std::fs::metadata(copy.join("src/main.rs")).unwrap(),
        );
        assert_eq!((to.mtime(), to.mtime_nsec()), (1_700_000_000, 987_654_321));
        assert_eq!(
            (from.mtime(), from.mtime_nsec()),
            (to.mtime(), to.mtime_nsec())
        );
        // A directory is dated after it was filled, or it would read as changed.
        let (from, to) = (
            std::fs::metadata(root.join("src")).unwrap(),
            std::fs::metadata(copy.join("src")).unwrap(),
        );
        assert_eq!(
            (from.mtime(), from.mtime_nsec()),
            (to.mtime(), to.mtime_nsec())
        );
    }

    #[test]
    fn a_git_file_naming_a_directory_outside_the_copy_is_refused() {
        for text in [
            "gitdir: /elsewhere/.git/worktrees/w",
            "gitdir: ../outside/.git",
            "not a git file",
        ] {
            let (_tmp, root) = repository();
            std::fs::remove_dir_all(root.join(".git")).unwrap();
            std::fs::write(root.join(".git"), text).unwrap();
            let Err(reason) = walk(&root) else {
                panic!("{text:?} was walked");
            };
            assert!(reason.contains(".git"), "{reason}");
        }
    }

    #[test]
    fn a_git_file_reaching_outside_through_a_link_is_refused() {
        let (_tmp, root) = repository();
        let (_outside, elsewhere) = temp_root();
        std::fs::create_dir_all(elsewhere.join("deep")).unwrap();
        std::fs::create_dir_all(elsewhere.join("gitdir")).unwrap();
        std::fs::create_dir_all(root.join("vendor/sub")).unwrap();
        std::os::unix::fs::symlink(elsewhere.join("deep"), root.join("vendor/sub/link")).unwrap();
        std::fs::write(root.join("vendor/sub/.git"), "gitdir: link/../gitdir\n").unwrap();
        assert!(walk(&root).is_err());
    }

    #[test]
    fn a_submodule_git_file_pointing_inside_the_copy_is_copied() {
        let (_tmp, root) = repository();
        std::fs::create_dir_all(root.join("vendor/sub")).unwrap();
        std::fs::write(
            root.join("vendor/sub/.git"),
            "gitdir: ../../.git/modules/sub\n",
        )
        .unwrap();
        assert!(
            walk(&root)
                .unwrap()
                .files
                .contains(&camino::Utf8PathBuf::from("vendor/sub/.git"))
        );
        std::fs::write(root.join("vendor/sub/.git"), "gitdir: ../../../x\n").unwrap();
        assert!(walk(&root).is_err());
    }

    #[test]
    fn a_failed_clone_leaves_nothing_behind_and_says_why() {
        let (_tmp, root) = repository();
        let refusing = |_: &File, _: &File| Err(io::Error::from_raw_os_error(95));
        let reason = copy_into_place(&root, "n1", &refusing).unwrap_err();
        assert!(reason.contains("cannot clone"), "{reason}");
        let left: Vec<_> = std::fs::read_dir(root.join(DIR))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left, ["other-workspace"]);
    }

    #[test]
    fn a_finished_copy_is_named_for_its_process_and_node() {
        let (_tmp, root) = repository();
        let path = copy_into_place(&root, "n1", &plain_copy).unwrap();
        assert_eq!(
            path,
            root.join(DIR).join(format!("{}-n1", std::process::id()))
        );
        assert!(path.join("src/main.rs").is_file());
    }

    #[test]
    fn a_repository_without_a_build_cache_runs_in_place() {
        let (_tmp, root) = temp_root();
        let reason = eligible(&root).unwrap_err();
        assert!(reason.contains("no target/"), "{reason}");
        std::fs::create_dir_all(root.join("target/explorer")).unwrap();
        let reason = eligible(&root).unwrap_err();
        assert!(reason.contains("no build yet"), "{reason}");
    }

    #[test]
    fn a_target_that_is_a_link_is_not_a_cache_to_copy() {
        let (_tmp, root) = temp_root();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("debug")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("target")).unwrap();
        assert!(
            eligible(&root)
                .unwrap_err()
                .contains("not a real directory")
        );
    }

    #[test]
    fn a_workspace_directory_that_is_a_link_is_not_used() {
        let (_tmp, root) = temp_root();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join(DIR)).unwrap();
        assert!(eligible(&root).unwrap_err().contains("target/explorer"));
    }

    #[test]
    fn the_sweep_does_not_delete_through_a_linked_workspace_directory() {
        let (_tmp, root) = temp_root();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(elsewhere.join(format!("{dead}-n1"))).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join(DIR)).unwrap();
        sweep(&root);
        assert!(elsewhere.join(format!("{dead}-n1")).is_dir());
    }

    #[test]
    fn the_sandbox_command_shows_the_copy_at_the_root_and_starts_in_slash() {
        let sandbox = Sandbox {
            root: camino::Utf8PathBuf::from("/repo"),
            workspace: camino::Utf8PathBuf::from("/repo/target/explorer/1-n"),
        };
        let command = sandbox.command("claude");
        let std = command.as_std();
        let args: Vec<_> = std
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "sandbox-exec",
                "--root",
                "/repo",
                "--workspace",
                "/repo/target/explorer/1-n",
                "--",
                "claude"
            ]
        );
        assert_eq!(std.get_current_dir(), Some(std::path::Path::new("/")));
    }

    #[test]
    fn the_sweep_removes_what_a_dead_process_left_and_nothing_else() {
        let (_tmp, root) = temp_root();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let live = std::process::id();
        let dir = root.join(DIR);
        let names = [
            format!("{dead}-n1"),
            format!("{BUILDING}{dead}-n2"),
            format!("{live}-n3"),
            format!("{BUILDING}{live}-n4"),
            "not-a-pid".to_string(),
        ];
        for name in &names {
            std::fs::create_dir_all(dir.join(name).join("inner")).unwrap();
        }
        sweep(&root);
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let mut kept = vec![names[2].clone(), names[3].clone(), names[4].clone()];
        kept.sort();
        assert_eq!(left, kept);
    }

    #[test]
    fn a_copy_with_a_read_only_directory_is_still_deleted() {
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, root) = temp_root();
        let copy = root.join("copy");
        std::fs::create_dir_all(copy.join("locked")).unwrap();
        std::fs::write(copy.join("locked/file"), "x").unwrap();
        std::fs::set_permissions(copy.join("locked"), std::fs::Permissions::from_mode(0o555))
            .unwrap();
        remove_copy(&copy).unwrap();
        assert!(!copy.exists());
    }

    #[test]
    fn release_deletes_the_copy_in_the_background() {
        let (_tmp, root) = temp_root();
        let copy = root.join("copy");
        std::fs::create_dir_all(copy.join("deep")).unwrap();
        std::fs::write(copy.join("deep/file"), "x").unwrap();
        Workspace::Cloned(copy.clone()).release();
        for _ in 0..100 {
            if !copy.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the copy is still there");
    }

    /// A build in a reflink copy compiles nothing; skipped where the filesystem has no reflinks.
    #[test]
    fn a_build_in_a_reflink_copy_finds_everything_fresh() {
        use std::process::Command;
        let (_tmp, base) = temp_root_in_target("workspace-test-");
        let krate = base.join("krate");
        std::fs::create_dir_all(krate.join("src")).unwrap();
        std::fs::write(
            krate.join("Cargo.toml"),
            "[package]\nname = \"fresh\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(krate.join("src/main.rs"), "fn main() {}").unwrap();
        let build = |dir: &camino::Utf8Path| {
            let output = Command::new(env!("CARGO"))
                .args(["build", "--offline"])
                .current_dir(dir)
                .env_remove("CARGO_TARGET_DIR")
                .env_remove("CARGO_BUILD_TARGET_DIR")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stderr).into_owned()
        };
        assert!(build(&krate).contains("Compiling fresh"));
        let copy = base.join("copy");
        if let Err(reason) = clone_tree(&krate, &copy, &clone_file) {
            assert!(reason.contains("cannot clone"), "{reason}");
            return;
        }
        let stderr = build(&copy);
        assert!(!stderr.contains("Compiling"), "{stderr}");
        assert!(stderr.contains("Finished"), "{stderr}");
    }

    #[test]
    fn a_worktree_outside_the_copy_is_named_by_the_config_and_a_relative_one_is_not() {
        let (_tmp, root) = temp_root();
        git(&root, &["init", "-q"]);
        std::fs::write(root.join(".git/inc"), "[core]\n\tworktree = /elsewhere\n").unwrap();
        let named = |config: &str| {
            std::fs::write(root.join(".git/config"), config).unwrap();
            names_a_worktree_outside(&root).unwrap()
        };
        assert!(named("[core]\n\tworktree = /elsewhere\n"));
        assert!(named("[core]\n\tworktree = \"/elsewhere\"\n"));
        assert!(named("[CORE]\n\tWorkTree = /elsewhere\n"));
        assert!(named("[core] worktree = /elsewhere\n"));
        assert!(named("[core]\n\tworktree = ~/elsewhere\n"));
        assert!(named("[include]\n\tpath = inc\n"));
        assert!(!named("[core]\n\tworktree = ../..\n"));
        assert!(!named("[core \"sub\"]\n\tworktree = /x\n"));
        assert!(!named("[other]\n\tworktree = /x\n"));
        assert!(!named("# worktree = /x\n"));
    }

    #[test]
    fn a_directory_that_is_no_repository_is_an_error_not_a_worktree() {
        let (_tmp, plain) = temp_root();
        let reason = names_a_worktree_outside(&plain).unwrap_err();
        assert!(reason.contains("cannot read the repository"), "{reason}");
    }

    #[test]
    fn a_git_file_naming_a_git_dir_with_an_absolute_worktree_is_refused() {
        let (_tmp, root) = temp_root();
        git(&root, &["init", "-q", "--bare", "modules/x"]);
        std::fs::write(root.join(".git"), "gitdir: modules/x\n").unwrap();
        std::fs::write(
            root.join("modules/x/config"),
            "[core]\n\tbare = true\n\tworktree = /outside\n",
        )
        .unwrap();
        assert!(names_a_worktree_outside(&root).unwrap());
    }
}
