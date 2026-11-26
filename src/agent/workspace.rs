//! The workspace of a feedback turn: a copy of the repository, `.git` and `target/`
//! included, made with reflinks so it costs metadata and not the gigabytes of a build
//! cache. The turn's agent runs in it through `explorer sandbox-exec`, which shows it at
//! the repository's own path, so an accidental write from the agent cannot reach the
//! repository, and a build in it finds every artifact fresh.
//!
//! The copy lives under `<root>/target/explorer`, which it leaves out of itself, and is
//! only made where it pays: a repository with no build cache yet, a cache that is not
//! under `target/`, or a host without user namespaces run the turn in place, and record
//! why (the node's `in-place`, and a log line).
//! A filesystem without reflinks is not known until the first clone: that copy fails the
//! turn rather than fall back, since another turn may already be copying the repository.
//!
//! Cargo takes a build for fresh by comparing mtimes, so every file keeps its own, to the
//! nanosecond; a file that did not would rebuild the world.

use std::fs::File;
use std::io;

const DIR: &str = "target/explorer";

/// A copy that is still being made; no turn ever sees one.
const BUILDING: &str = ".building-";

/// What a feedback turn does about the repository, decided when it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    Copy,
    InPlace {
        reason: String,
        /// The failure this plan was made around, when the last copy's was what forced it.
        failure: Option<u64>,
    },
}

#[derive(Debug)]
pub enum Workspace {
    Cloned(camino::Utf8PathBuf),
    InPlace { reason: String },
}

/// Identifies a failure, so a start can spend the one its plan saw and no other.
static NEXT_FAILURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Why the last copy failed, and which failure it is, until the turn planned around it starts.
static FAILED: parking_lot::Mutex<Option<(u64, String)>> = parking_lot::Mutex::new(None);

/// The copies this process is using: their turns are running, so a sweep must leave them.
static IN_USE: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashSet<camino::Utf8PathBuf>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashSet::new()));

/// Marks `path` as a copy in use, so a sweep leaves it alone.
fn keep(path: &camino::Utf8Path) {
    IN_USE.lock().insert(path.to_path_buf());
}

/// Forgets a copy: it is released, or the turn that made it never ran.
fn forget(path: &camino::Utf8Path) {
    IN_USE.lock().remove(path);
}

/// Whether this process holds `path` for a turn that is still running.
fn held(path: &camino::Utf8Path) -> bool {
    IN_USE.lock().contains(path)
}

/// A copy failed after admission: the next started turn is planned in place.
pub fn copy_failed(reason: &str) {
    let id = NEXT_FAILURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    *FAILED.lock() = Some((id, reason.to_string()));
}

/// Spends the failure `plan` was made around, and no other.
pub fn started(plan: &Plan) {
    started_with(plan, &FAILED);
}

fn started_with(plan: &Plan, failed: &parking_lot::Mutex<Option<(u64, String)>>) {
    let Plan::InPlace {
        failure: Some(id), ..
    } = plan
    else {
        return;
    };
    let mut failed = failed.lock();
    if failed.as_ref().is_some_and(|(current, _)| current == id) {
        failed.take();
    }
}

/// The failure the process last saw, for a test that has to leave the marker clean.
#[cfg(test)]
pub(crate) fn clear_failed() {
    FAILED.lock().take();
}

#[derive(Clone, Debug)]
pub struct Sandbox {
    pub root: camino::Utf8PathBuf,
    /// A copy shown at `root`; `None` for a turn that works in the repository itself.
    pub workspace: Option<camino::Utf8PathBuf>,
    /// Directories the agent cannot write.
    pub read_only: Vec<camino::Utf8PathBuf>,
}

impl Sandbox {
    /// Starts in `/`: an inherited cwd would name the real repository.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(explorer());
        command.args(["sandbox-exec", "--root"]).arg(&self.root);
        if let Some(workspace) = &self.workspace {
            command.arg("--workspace").arg(workspace);
        }
        for path in &self.read_only {
            command.arg("--ro").arg(path);
        }
        command.arg("--").arg(program).current_dir("/");
        command
    }

    /// The sandbox of a commit turn, which works in the repository itself and cannot write
    /// the knowledge directory; `None` where the host has no namespaces, or where
    /// `docs/knowledge` is not a directory it can protect: the turn then runs unprotected.
    /// Blocking: the first call runs the namespace check.
    pub fn commit(root: &camino::Utf8Path) -> Option<Self> {
        if let Err(reason) = sandbox_works(root) {
            tracing::warn!(
                "a commit turn runs unprotected, the sandbox does not work here: {reason}"
            );
            return None;
        }
        // A linked parent would have the directory made, and protected, outside the repository.
        let docs = root.join("docs");
        if std::fs::symlink_metadata(&docs).is_ok_and(|meta| !meta.is_dir()) {
            tracing::warn!("a commit turn runs unprotected, {KNOWLEDGE} is not under a real docs/");
            return None;
        }
        let knowledge = root.join(KNOWLEDGE);
        let usable = match std::fs::symlink_metadata(&knowledge) {
            Ok(meta) => meta.is_dir(),
            Err(_) => std::fs::create_dir_all(&knowledge).is_ok(),
        };
        if !usable {
            tracing::warn!("a commit turn runs unprotected, {KNOWLEDGE} is not a directory");
            return None;
        }
        // The check remounts the repository, which says nothing about a mount below it.
        if !same_device(root, &knowledge) {
            tracing::warn!(
                "a commit turn runs unprotected, {KNOWLEDGE} is on another filesystem than the \
                 repository"
            );
            return None;
        }
        Some(Self {
            root: root.to_path_buf(),
            workspace: None,
            read_only: vec![knowledge],
        })
    }
}

/// The directory of knowledge entries, which the agent of a commit turn cannot write.
const KNOWLEDGE: &str = "docs/knowledge";

/// Whether `path` is on the filesystem of `root`, and so remounted read-only by a bind of it.
fn same_device(root: &camino::Utf8Path, path: &camino::Utf8Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(root), std::fs::metadata(path)) {
        (Ok(root), Ok(path)) => root.dev() == path.dev(),
        _ => false,
    }
}

fn explorer() -> std::path::PathBuf {
    // The program of this process, even if the file was replaced since it started.
    #[cfg(not(test))]
    return std::path::PathBuf::from("/proc/self/exe");
    #[cfg(test)]
    return crate::sandbox::built_explorer();
}

/// Runs on the UI thread: stats and small config files only, the sandbox check is cached.
pub fn plan(root: &camino::Utf8Path) -> Plan {
    plan_with(root, &FAILED)
}

pub(crate) fn plan_with(
    root: &camino::Utf8Path,
    failed: &parking_lot::Mutex<Option<(u64, String)>>,
) -> Plan {
    let failure = failed.lock().clone();
    let reason = failure
        .as_ref()
        .map(|(_, reason)| format!("the last copy failed: {reason}"))
        .or_else(|| cargo_target_dir_set().then(|| CARGO_TARGET.to_string()))
        .or_else(|| cargo_config_target_dir_set(root).then(|| CARGO_CONFIG_TARGET.to_string()))
        .or_else(|| eligible(root).err());
    match reason {
        None => Plan::Copy,
        Some(reason) => Plan::InPlace {
            reason: crate::util::text::single_line(&reason),
            failure: failure.map(|(id, _)| id),
        },
    }
}

const CARGO_TARGET: &str = "a cargo target directory is set, so builds do not use target/";
const CARGO_CONFIG_TARGET: &str =
    "a cargo config redirects the build directory, so builds do not use target/";

fn cargo_target_dir_set() -> bool {
    ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"]
        .iter()
        .any(|var| std::env::var_os(var).is_some())
}

/// Whether a cargo config of the repository redirects builds; cargo reads the one at the root.
fn cargo_config_target_dir_set(root: &camino::Utf8Path) -> bool {
    let config = root.join(".cargo").join("config.toml");
    let legacy = root.join(".cargo").join("config");
    [config, legacy].iter().any(|path| {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| text.parse::<toml::Value>().ok())
            .is_some_and(|value| {
                value
                    .get("build")
                    .and_then(|build| build.get("target-dir"))
                    .is_some()
            })
    })
}

/// Runs the sandbox check now, so the first turn does not wait for it.
pub fn warm(root: &camino::Utf8Path) {
    if let Err(reason) = sandbox_works(root) {
        tracing::warn!(
            "the sandbox does not work here ({reason}): feedback turns run one at a time in the \
             repository, and a commit turn does not protect {KNOWLEDGE}"
        );
    }
}

impl Workspace {
    pub fn copy(root: &camino::Utf8Path, node: &str) -> Result<Self, String> {
        copy_into_place(root, node, &clone_file)
            .map(Self::Cloned)
            .map_err(|reason| crate::util::text::single_line(&reason))
    }

    pub fn sandbox(&self, root: &camino::Utf8Path) -> Option<Sandbox> {
        match self {
            Self::Cloned(workspace) => Some(Sandbox {
                root: root.to_path_buf(),
                workspace: Some(workspace.clone()),
                read_only: Vec::new(),
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

    /// A deletion cut short is finished by `sweep`, at startup or before the next copy.
    pub fn release(self) {
        if let Self::Cloned(path) = &self {
            let path = path.clone();
            std::thread::spawn(move || {
                let _ = remove_copy(&path);
            });
        }
    }
}

/// A dropped `Workspace` leaves `IN_USE`, however the turn ended.
impl Drop for Workspace {
    fn drop(&mut self) {
        if let Self::Cloned(path) = self {
            forget(path);
        }
    }
}

fn eligible(root: &camino::Utf8Path) -> Result<(), String> {
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
    sandbox_works(root).map_err(|reason| format!("the sandbox does not work here: {reason}"))
}

fn sandbox_works(root: &camino::Utf8Path) -> Result<(), String> {
    use std::sync::OnceLock;
    // The namespaces work or not for the whole process.
    static SANDBOX: OnceLock<Result<(), String>> = OnceLock::new();
    SANDBOX.get_or_init(|| sandbox_check(root)).clone()
}

fn sandbox_check(root: &camino::Utf8Path) -> Result<(), String> {
    // The root as the read-only path exercises the namespaces, the id maps and the remount
    // on the filesystem that holds the repository, whatever the repository holds.
    let output = std::process::Command::new(explorer())
        .args(["sandbox-exec", "--root"])
        .arg(root)
        .arg("--ro")
        .arg(root)
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

/// A copy's two names, held while it is built and given up however that ends.
struct Held {
    building: camino::Utf8PathBuf,
    done: camino::Utf8PathBuf,
    /// The finished copy passes to a `Workspace`, whose own drop gives the name up.
    keep_done: bool,
}

impl Held {
    /// Held before either name exists, so a concurrent sweep in this process never takes it.
    fn new(building: camino::Utf8PathBuf, done: camino::Utf8PathBuf) -> Self {
        keep(&building);
        keep(&done);
        Self {
            building,
            done,
            keep_done: false,
        }
    }

    /// The copy is finished: the name stays held until its `Workspace` goes.
    fn finished(mut self) {
        self.keep_done = true;
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        forget(&self.building);
        if !self.keep_done {
            forget(&self.done);
        }
    }
}

/// Built as `.building-…` and renamed, so no turn reads a partial copy.
fn copy_into_place(
    root: &camino::Utf8Path,
    node: &str,
    clone: &(dyn Fn(&File, &File) -> io::Result<()> + Sync),
) -> Result<camino::Utf8PathBuf, String> {
    let parent = root.join(DIR);
    // Before this copy's names are held: leftovers go first.
    sweep(root);
    std::fs::create_dir_all(&parent).map_err(|e| format!("cannot create {parent}: {e}"))?;
    let name = format!("{}-{node}", std::process::id());
    let building = parent.join(format!("{BUILDING}{name}"));
    let done = parent.join(&name);
    let _ = remove_copy(&building);
    let held = Held::new(building.clone(), done.clone());
    let copied = clone_tree(root, &building, clone).and_then(|()| {
        std::fs::rename(&building, &done).map_err(|e| format!("cannot move the copy: {e}"))
    });
    match copied {
        Ok(()) => {
            held.finished();
            Ok(done)
        }
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
        // Git follows a link to one of its own files, so it is judged like the file.
        if !kind.is_dir()
            && let Some(reason) = shared_git_data(root, &path, &relative)
        {
            return Err(reason);
        }
        // Links are judged in a second pass, once every `gitdir:` file is read.
        if kind.is_dir() {
            found.dirs.push(relative);
        } else if kind.is_symlink() {
            found.links.push(relative);
        } else if kind.is_file() {
            found.files.push(relative);
        }
    }
    // A git directory is a `.git` component, or the target of a `gitdir:` file.
    let mut git_dirs: Vec<camino::Utf8PathBuf> = found
        .dirs
        .iter()
        .filter(|dir| under_a_git_directory(dir))
        .cloned()
        .collect();
    for file in found
        .files
        .iter()
        .filter(|file| file.file_name() == Some(".git"))
    {
        let Some(dir) = gitdir_target(root, file) else {
            continue;
        };
        git_dirs.push(dir);
    }
    for link in &found.links {
        // A link that stays inside the copy, or leads nowhere, shares nothing.
        let Ok(target) = std::fs::canonicalize(root.join(link)) else {
            continue;
        };
        let Ok(target) = camino::Utf8PathBuf::from_path_buf(target) else {
            continue;
        };
        if resolves_inside(root, link, &target) {
            continue;
        }
        if under_a_git_directory(link) || git_dirs.iter().any(|dir| link.starts_with(dir)) {
            return Err(format!(
                "{link} names a git directory outside the copy, which would share its data"
            ));
        }
    }
    Ok(found)
}

/// Where a `gitdir:` file sends git, as a path relative to the copy, when it stays inside it.
fn gitdir_target(root: &camino::Utf8Path, file: &camino::Utf8Path) -> Option<camino::Utf8PathBuf> {
    let text = std::fs::read_to_string(root.join(file)).ok()?;
    let target = text.trim().strip_prefix("gitdir:").map(str::trim)?;
    let target = camino::Utf8Path::new(target);
    if target.is_absolute() {
        return None;
    }
    let mut depth = file.parent().map_or(0, |dir| dir.components().count());
    for component in target.components() {
        match component {
            camino::Utf8Component::ParentDir if depth == 0 => return None,
            camino::Utf8Component::ParentDir => depth -= 1,
            camino::Utf8Component::Normal(_) => depth += 1,
            _ => {}
        }
    }
    let joined = file.parent()?.join(target);
    let resolved = std::fs::canonicalize(root.join(&joined)).ok()?;
    let resolved = camino::Utf8PathBuf::from_path_buf(resolved).ok()?;
    let root = std::fs::canonicalize(root).ok()?;
    let root = camino::Utf8PathBuf::from_path_buf(root).ok()?;
    resolved
        .strip_prefix(&root)
        .ok()
        .map(camino::Utf8Path::to_path_buf)
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

/// Whether `target`, named in `base`, stays inside `root` once links are resolved.
fn resolves_inside(
    root: &camino::Utf8Path,
    base: &camino::Utf8Path,
    target: &camino::Utf8Path,
) -> bool {
    use camino::Utf8Component;
    let joined = if target.is_absolute() {
        target.to_path_buf()
    } else {
        base.join(target)
    };
    match (std::fs::canonicalize(root), std::fs::canonicalize(&joined)) {
        (Ok(root), Ok(resolved)) => resolved.starts_with(&root),
        // A path that does not resolve leads git nowhere real: judge it as written.
        _ => {
            let mut resolved = camino::Utf8PathBuf::new();
            for component in joined.components() {
                match component {
                    Utf8Component::CurDir => {}
                    Utf8Component::ParentDir => {
                        resolved.pop();
                    }
                    other => resolved.push(other.as_str()),
                }
            }
            resolved.starts_with(root)
        }
    }
}

/// Whether `relative` sits inside a git directory, where a link is data git reads.
fn under_a_git_directory(relative: &camino::Utf8Path) -> bool {
    relative
        .components()
        .any(|component| component.as_str() == ".git")
}

/// The git files naming another directory (`alternates`, `commondir`) that leave the copy.
fn shared_git_data(
    root: &camino::Utf8Path,
    path: &camino::Utf8Path,
    relative: &camino::Utf8Path,
) -> Option<String> {
    let name = path.file_name()?;
    // By name alone: a link to the directory holding it is opened by git all the same.
    let alternates = name == "alternates";
    if !alternates && name != "commondir" {
        return None;
    }
    let outside =
        format!("{relative} names a git directory outside the copy, which would share its data");
    // A link is read through, so one that leaves the copy reads the data it points at.
    if let Ok(file) = std::fs::canonicalize(path)
        && let Ok(file) = camino::Utf8PathBuf::from_path_buf(file)
        && !resolves_inside(root, path, &file)
    {
        return Some(outside);
    }
    // Git reads the bytes, so a file it can follow is judged as though it followed it.
    let Ok(text) = std::fs::read_to_string(path) else {
        return Some(outside);
    };
    let base = path.parent()?;
    if name == "commondir" {
        let target = camino::Utf8Path::new(text.trim());
        return (!target.as_str().is_empty() && !resolves_inside(root, base, target))
            .then_some(outside);
    }
    // Entries are read from the objects directory, which is the parent of `info`.
    let objects = match base.file_name() {
        Some("info") => base.parent()?,
        _ => base,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !resolves_inside(root, objects, camino::Utf8Path::new(line)) {
            return Some(outside);
        }
    }
    None
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
    let mine = std::process::id();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let owner = name.strip_prefix(BUILDING).unwrap_or(&name);
        let Some(pid) = owner.split('-').next().and_then(|pid| pid.parse().ok()) else {
            continue;
        };
        // Another live Explorer's copies are left alone; this process's only while held.
        let stale = if pid == mine {
            !held(&dir.join(name.as_ref()))
        } else {
            !alive(pid)
        };
        if stale {
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
    fn a_git_object_store_shared_with_another_directory_is_refused() {
        let (_tmp, root) = repository();
        let alternates = root.join(".git/objects/info/alternates");
        std::fs::create_dir_all(alternates.parent().unwrap()).unwrap();
        // A `--shared` clone keeps an absolute path, or one climbing out of the copy.
        for text in ["/elsewhere/.git/objects\n", "../../../../outside\n"] {
            std::fs::write(&alternates, text).unwrap();
            let Err(reason) = walk(&root) else {
                panic!("{text:?} was walked");
            };
            assert!(reason.contains("objects/info/alternates"), "{reason}");
        }
        // One entry outside is enough, and a comment names nothing.
        std::fs::write(&alternates, "# a note\n../objects\n").unwrap();
        assert!(walk(&root).is_ok());
        // An entry that stays inside the copy is copied, as git finds it there.
        std::fs::create_dir_all(root.join(".git/objects/pack")).unwrap();
        std::fs::write(&alternates, "../../pack\n/elsewhere\n").unwrap();
        assert!(walk(&root).is_err());
        std::fs::write(&alternates, "pack\n").unwrap();
        assert!(walk(&root).is_ok());

        // Git follows the chain: an inside store naming one outside shares it too.
        let store = root.join("shared-store");
        std::fs::create_dir_all(store.join("info")).unwrap();
        std::fs::write(store.join("info/alternates"), "/elsewhere/.git/objects\n").unwrap();
        std::fs::write(&alternates, "../../shared-store\n").unwrap();
        assert!(
            walk(&root).is_err(),
            "the chain to the outside store was walked"
        );
        std::fs::remove_dir_all(&store).unwrap();
        std::fs::write(&alternates, "pack\n").unwrap();

        // A link to an alternates file is read through by git, so it is judged as its target.
        std::fs::remove_file(&alternates).unwrap();
        std::os::unix::fs::symlink("/elsewhere/alternates", &alternates).unwrap();
        assert!(walk(&root).is_err(), "a linked alternates was walked");
        std::fs::remove_file(&alternates).unwrap();

        // The directory holding it may be a link to one spelled otherwise, which git opens.
        let meta = root.join("meta");
        std::fs::create_dir_all(&meta).unwrap();
        std::fs::write(meta.join("alternates"), "/elsewhere/.git/objects\n").unwrap();
        assert!(
            walk(&root).is_err(),
            "an alternates under any directory was walked"
        );
        std::fs::write(meta.join("alternates"), ".\n").unwrap();
        assert!(walk(&root).is_ok());
        std::fs::remove_dir_all(&meta).unwrap();

        // A linked worktree names its main git directory this way.
        std::fs::write(root.join(".git/commondir"), "/elsewhere/.git\n").unwrap();
        let Err(reason) = walk(&root) else {
            panic!("an absolute commondir was walked");
        };
        assert!(reason.contains("commondir"), "{reason}");
        std::fs::write(root.join(".git/commondir"), "../..\n").unwrap();
        assert!(walk(&root).is_err());
        // A link standing in for the object store itself reads it just as well.
        std::fs::write(root.join(".git/commondir"), ".\n").unwrap();
        assert!(walk(&root).is_ok());
        let objects = root.join(".git/objects");
        let (_outside, elsewhere) = temp_root();
        std::fs::create_dir_all(elsewhere.join("objects")).unwrap();
        std::fs::remove_dir_all(&objects).unwrap();
        std::os::unix::fs::symlink(elsewhere.join("objects"), &objects).unwrap();
        let Err(reason) = walk(&root) else {
            panic!("a linked objects directory was walked");
        };
        assert!(reason.contains("objects"), "{reason}");
        std::fs::remove_file(&objects).unwrap();
        std::fs::create_dir_all(&objects).unwrap();
        // A link that stays inside the copy names nothing outside it.
        std::fs::create_dir_all(root.join("store")).unwrap();
        std::fs::remove_dir_all(&objects).unwrap();
        std::os::unix::fs::symlink(root.join("store"), &objects).unwrap();
        assert!(walk(&root).is_ok());
        std::fs::remove_file(&objects).unwrap();
        std::fs::create_dir_all(&objects).unwrap();
        // And at whatever depth inside a git directory: a loose object, a ref, a pack.
        let loose = root.join(".git/objects/ab");
        std::fs::create_dir_all(&loose).unwrap();
        std::fs::write(elsewhere.join("obj"), "x").unwrap();
        std::os::unix::fs::symlink(elsewhere.join("obj"), loose.join("cdef")).unwrap();
        let Err(reason) = walk(&root) else {
            panic!("a linked loose object was walked");
        };
        assert!(reason.contains("objects/ab/cdef"), "{reason}");
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
    fn a_link_inside_a_gitdir_file_s_directory_is_refused() {
        let (_tmp, root) = repository();
        let (_outside, elsewhere) = temp_root();
        std::fs::write(elsewhere.join("obj"), "x").unwrap();
        // A `gitdir:` target has no `.git` component: what it holds still comes from git's data.
        std::fs::remove_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("storage/objects/ab")).unwrap();
        std::fs::write(root.join(".git"), "gitdir: storage\n").unwrap();
        std::os::unix::fs::symlink(elsewhere.join("obj"), root.join("storage/objects/ab/cd"))
            .unwrap();
        let Err(reason) = walk(&root) else {
            panic!("a link inside the gitdir's store was walked");
        };
        assert!(reason.contains("storage/objects/ab/cd"), "{reason}");
        // One that stays inside the copy shares nothing.
        std::fs::write(root.join("storage/store"), "x").unwrap();
        std::fs::remove_file(root.join("storage/objects/ab/cd")).unwrap();
        std::os::unix::fs::symlink(
            root.join("storage/store"),
            root.join("storage/objects/ab/cd"),
        )
        .unwrap();
        assert!(walk(&root).is_ok());
    }

    #[test]
    fn a_panicking_clone_holds_neither_name_after_it_unwinds() {
        let (_tmp, root) = repository();
        let panicking = |_: &File, _: &File| -> io::Result<()> { panic!("the clone blew up") };
        let dir = root.join(DIR);
        let name = format!("{}-n1", std::process::id());
        let building = dir.join(format!("{BUILDING}{name}"));
        let done = dir.join(&name);
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            copy_into_place(&root, "n1", &panicking)
        }));
        assert!(unwound.is_err(), "the panic was swallowed");
        assert!(!held(&building), "{} stays held", building);
        assert!(!held(&done), "{done} stays held");
        // Whatever the panic left goes with the next sweep, not the next restart.
        sweep(&root);
        assert!(!building.exists(), "{} was left", building);
        assert!(!done.exists(), "{done} was left");
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
            workspace: Some(camino::Utf8PathBuf::from("/repo/target/explorer/1-n")),
            read_only: Vec::new(),
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
    fn a_commit_turns_sandbox_has_no_copy_and_a_read_only_knowledge_directory() {
        let sandbox = Sandbox {
            root: camino::Utf8PathBuf::from("/repo"),
            workspace: None,
            read_only: vec![camino::Utf8PathBuf::from("/repo/docs/knowledge")],
        };
        let command = sandbox.command("claude");
        let args: Vec<_> = command
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "sandbox-exec",
                "--root",
                "/repo",
                "--ro",
                "/repo/docs/knowledge",
                "--",
                "claude"
            ]
        );
    }

    #[test]
    fn the_commit_sandbox_creates_the_knowledge_directory_and_a_copy_less_plan_gets_none() {
        let (_tmp, root) = temp_root();
        if let Some(sandbox) = Sandbox::commit(&root) {
            assert_eq!(sandbox.workspace, None);
            assert_eq!(sandbox.read_only, [root.join(KNOWLEDGE)]);
            assert!(root.join(KNOWLEDGE).is_dir());
        }
        let in_place = Workspace::InPlace {
            reason: "no build".to_string(),
        };
        assert!(in_place.sandbox(&root).is_none());
    }

    #[test]
    fn the_commit_sandbox_is_refused_where_knowledge_is_not_a_directory() {
        let (_tmp, root) = temp_root();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join(KNOWLEDGE), "a file").unwrap();
        assert!(Sandbox::commit(&root).is_none());
    }

    #[test]
    fn the_commit_sandbox_is_refused_behind_a_linked_docs_directory() {
        let (_tmp, root) = temp_root();
        let (_other, outside) = temp_root();
        std::os::unix::fs::symlink(&outside, root.join("docs")).unwrap();
        assert!(Sandbox::commit(&root).is_none());
        // Nothing was made where the link pointed.
        assert!(!outside.join("knowledge").exists());
    }

    #[test]
    fn the_sweep_removes_what_a_dead_process_left_and_what_this_one_does_not_hold() {
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
        // A copy this process holds for a running turn, and a name that is no pid, stay.
        keep(&dir.join(&names[2]));
        sweep(&root);
        forget(&dir.join(&names[2]));
        let mut left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let mut expected = vec![names[2].clone(), names[4].clone()];
        expected.sort();
        assert_eq!(left, expected);

        // Released, the same directory goes on the next sweep.
        sweep(&root);
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, [names[4].clone()]);
    }

    #[test]
    fn the_sweep_leaves_a_copy_another_live_explorer_holds() {
        let (_tmp, root) = temp_root();
        // Another live process stands in for a second Explorer: its copy is not ours to delete.
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = other.id();
        let dir = root.join(DIR);
        std::fs::create_dir_all(dir.join(format!("{pid}-n1"))).unwrap();
        sweep(&root);
        assert!(dir.join(format!("{pid}-n1")).is_dir());

        let _ = other.kill();
        let _ = other.wait();
        sweep(&root);
        assert!(!dir.join(format!("{pid}-n1")).exists());
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
    fn a_dropped_copy_is_no_longer_held_by_the_sweep() {
        let (_tmp, root) = temp_root();
        let dir = root.join(DIR);
        let name = format!("{}-n9", std::process::id());
        let copy = dir.join(&name);
        std::fs::create_dir_all(&copy).unwrap();
        keep(&copy);
        assert!(held(&copy));
        // Whatever a turn ends by — release, a panic, a dropped future — the entry goes.
        drop(Workspace::Cloned(copy.clone()));
        assert!(!held(&copy));
        sweep(&root);
        assert!(!copy.exists());
    }

    #[test]
    fn a_cargo_config_that_redirects_the_build_directory_is_not_copied() {
        let (_tmp, root) = temp_root();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        assert!(!cargo_config_target_dir_set(&root));

        std::fs::create_dir_all(root.join(".cargo")).unwrap();
        std::fs::write(
            root.join(".cargo/config.toml"),
            "[alias]\nxtask = \"run\"\n",
        )
        .unwrap();
        assert!(!cargo_config_target_dir_set(&root));

        std::fs::write(
            root.join(".cargo/config.toml"),
            "[build]\ntarget-dir = \"/elsewhere\"\n",
        )
        .unwrap();
        assert!(cargo_config_target_dir_set(&root));

        // The legacy name, without the extension, is read too.
        std::fs::remove_file(root.join(".cargo/config.toml")).unwrap();
        std::fs::write(root.join(".cargo/config"), "[build]\ntarget-dir = \"x\"\n").unwrap();
        assert!(cargo_config_target_dir_set(&root));
    }

    #[test]
    fn a_failed_copy_plans_the_next_started_turn_in_place() {
        let (_tmp, root) = temp_root();
        let failed = parking_lot::Mutex::new(Some((7, "no space left".to_string())));
        // A start that is refused plans again and finds the same failure.
        for _ in 0..2 {
            let Plan::InPlace { reason, failure } = plan_with(&root, &failed) else {
                panic!("planned to copy");
            };
            assert!(
                reason.contains("the last copy failed: no space left"),
                "{reason}"
            );
            assert_eq!(failure, Some(7));
        }
        // The turn that starts on that plan spends it; the repository decides again.
        let plan = plan_with(&root, &failed);
        started_with(&plan, &failed);
        assert!(failed.lock().is_none());
        let Plan::InPlace { reason, failure } = plan_with(&root, &failed) else {
            panic!("planned to copy");
        };
        assert!(!reason.contains("last copy"), "{reason}");
        assert_eq!(failure, None);
    }

    #[test]
    fn a_failure_raising_between_the_plan_and_the_start_is_not_spent() {
        let (_tmp, root) = temp_root();
        let failed = parking_lot::Mutex::new(Some((7, "no space left".to_string())));
        let plan = plan_with(&root, &failed);
        // Another turn's copy fails after this one was planned around the older failure.
        *failed.lock() = Some((8, "quota exceeded".to_string()));
        started_with(&plan, &failed);
        // The newer failure stays, so the turn after this one is planned in place too.
        let next = failed.lock().clone();
        assert_eq!(next.map(|(id, _)| id), Some(8));
    }

    #[test]
    fn a_plan_not_made_around_a_failure_spends_none() {
        let failed = parking_lot::Mutex::new(Some((7, "no space left".to_string())));
        started_with(&Plan::Copy, &failed);
        assert!(failed.lock().is_some());
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
