//! `explorer sandbox-exec --root <repo> [--workspace <dir>] [--ro <path>]... -- <cmd> [args]`:
//! runs a command in a user and mount namespace where, given a workspace, the repository
//! path is replaced by it, and where the `--ro` paths cannot be written. It guards against
//! accidents: an agent that writes to the repository path writes the workspace, and its
//! edits are thrown away with it; one that writes a read-only path gets `EROFS`. It does
//! not confine the agent, which can still reach everything else on the host.
//!
//! `unshare` into a user namespace must happen in a single-threaded process, so
//! `sandbox-exec` re-execs itself as `sandbox-child` and that child enters the
//! namespaces, binds the workspace over the root if there is one, remounts the `--ro` paths
//! and `exec`s the command. The command
//! then runs with the child process's pid, process group and standard streams, so the
//! caller's signal and group handling works as without a sandbox.

/// The exit code of a failed setup, so the caller can tell it from the command's own (126, 127 below).
const SETUP_FAILED: i32 = 125;
/// The exit code when the command exists but cannot be run, as `env` and `timeout` use it.
const NOT_EXECUTABLE: i32 = 126;
/// The exit code when the command is not found.
const NOT_FOUND: i32 = 127;

fn fail(message: &str) -> ! {
    eprintln!("sandbox-exec: {message}");
    std::process::exit(SETUP_FAILED);
}

fn enter_namespaces() {
    use rustix::io::Errno;
    use rustix::thread::UnshareFlags;
    use std::io::ErrorKind;
    // The real ids are read before `unshare`, which makes the kernel report id 65534.
    let (uid, gid) = (
        rustix::process::getuid().as_raw(),
        rustix::process::getgid().as_raw(),
    );
    if let Err(error) = rustix::thread::unshare(UnshareFlags::NEWUSER | UnshareFlags::NEWNS) {
        let hint = match error {
            Errno::PERM | Errno::NOSYS | Errno::NOSPC => {
                " (unprivileged user namespaces are disabled: enable \
                 kernel.unprivileged_userns_clone or user.max_user_namespaces)"
            }
            _ => "",
        };
        fail(&format!("unshare failed: {error}{hint}"));
    }
    // Unmapped ids show as overflow 65534, so a failed setgroups or map write stops the run.
    match std::fs::write("/proc/self/setgroups", "deny") {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => fail(&format!("could not deny setgroups: {error}")),
    }
    // Map each id to itself, not to 0, so the command is not root and its files stay its own.
    for (file, inner) in [("uid_map", uid), ("gid_map", gid)] {
        let line = format!("{inner} {inner} 1");
        let path = format!("/proc/self/{file}");
        if let Err(error) = std::fs::write(&path, line) {
            fail(&format!("could not write {path}: {error}"));
        }
    }
}

fn make_private() {
    use rustix::mount::MountPropagationFlags;
    if let Err(error) = rustix::mount::mount_change(
        "/",
        MountPropagationFlags::REC | MountPropagationFlags::PRIVATE,
    ) {
        fail(&format!("could not make the mounts private: {error}"));
    }
}

fn shadow(root: &camino::Utf8Path, workspace: &camino::Utf8Path) {
    if let Err(error) = rustix::mount::mount_bind(workspace.as_std_path(), root.as_std_path()) {
        fail(&format!("could not bind {workspace} over {root}: {error}"));
    }
}

/// Makes `path` read-only for this namespace: bound onto itself, then remounted with the
/// flags its mount already has, since an unprivileged remount cannot drop a locked one.
fn read_only(path: &camino::Utf8Path) {
    use rustix::fs::StatVfsMountFlags;
    use rustix::mount::MountFlags;
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => fail(&format!("{path} is not a directory")),
        Err(error) => fail(&format!("cannot read {path}: {error}")),
    }
    if let Err(error) = rustix::mount::mount_bind(path.as_std_path(), path.as_std_path()) {
        fail(&format!("could not bind {path} onto itself: {error}"));
    }
    let have = match rustix::fs::statvfs(path.as_std_path()) {
        Ok(stat) => stat.f_flag,
        Err(error) => fail(&format!(
            "could not read the mount flags of {path}: {error}"
        )),
    };
    let mut flags = MountFlags::BIND | MountFlags::RDONLY;
    for (from, to) in [
        (StatVfsMountFlags::NOSUID, MountFlags::NOSUID),
        (StatVfsMountFlags::NODEV, MountFlags::NODEV),
        (StatVfsMountFlags::NOEXEC, MountFlags::NOEXEC),
        (StatVfsMountFlags::NOATIME, MountFlags::NOATIME),
        (StatVfsMountFlags::NODIRATIME, MountFlags::NODIRATIME),
        (StatVfsMountFlags::RELATIME, MountFlags::RELATIME),
    ] {
        if have.contains(from) {
            flags |= to;
        }
    }
    if let Err(error) = rustix::mount::mount_remount(path.as_std_path(), flags, "") {
        fail(&format!("could not make {path} read-only: {error}"));
    }
}

pub(crate) fn run(arguments: &[std::ffi::OsString]) -> ! {
    use std::ffi::OsString;
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    let mut rest = vec![OsString::from("sandbox-child")];
    rest.extend_from_slice(arguments);
    let error = Command::new("/proc/self/exe").args(&rest).exec();
    fail(&format!("could not re-exec: {error}"));
}

pub(crate) fn child(arguments: &[std::ffi::OsString]) -> ! {
    use camino::Utf8PathBuf;
    use std::ffi::OsStr;
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    // The command after `--` is not an option, so pico-args only sees what comes before it.
    let (options, command) = match arguments.iter().position(|arg| arg == "--") {
        Some(at) => (&arguments[..at], arguments[at + 1..].to_vec()),
        None => (arguments, Vec::new()),
    };
    let mut args = pico_args::Arguments::from_vec(options.to_vec());
    let path = |arg: &OsStr| {
        Utf8PathBuf::from_path_buf(arg.into())
            .map_err(|path| format!("{} is not valid UTF-8", path.display()))
    };
    let root = args
        .opt_value_from_os_str("--root", path)
        .unwrap_or_else(|error| fail(&error.to_string()));
    let workspace = args
        .opt_value_from_os_str("--workspace", path)
        .unwrap_or_else(|error| fail(&error.to_string()));
    let mut read_only_paths = Vec::new();
    while let Some(ro) = args
        .opt_value_from_os_str("--ro", path)
        .unwrap_or_else(|error| fail(&error.to_string()))
    {
        read_only_paths.push(ro);
    }
    let check = args.contains("--check");
    if let Some(other) = args.finish().first() {
        fail(&format!("unknown option {}", other.to_string_lossy()));
    }
    let Some(root) = root else {
        fail(
            "usage: sandbox-exec --root <repo> [--workspace <dir>] [--ro <path>]... [--check] \
             -- <cmd> [args]",
        );
    };
    if !root.is_absolute()
        || !workspace
            .iter()
            .chain(&read_only_paths)
            .all(|p| p.is_absolute())
    {
        fail("the root, the workspace and the read-only paths must be absolute paths");
    }
    // A workspace inside the root is bound over it; the other way round would shadow the root.
    if workspace
        .as_ref()
        .is_some_and(|workspace| root.starts_with(workspace))
    {
        fail("the root must not be inside the workspace");
    }
    if !check && command.is_empty() {
        fail("no command after --");
    }
    enter_namespaces();
    make_private();
    if let Some(workspace) = &workspace {
        shadow(&root, workspace);
    }
    for path in &read_only_paths {
        read_only(path);
    }
    if check {
        std::process::exit(0);
    }
    // The cwd is set by path after the bind: an earlier descriptor would point under the mount.
    if let Err(error) = rustix::process::chdir(root.as_std_path()) {
        fail(&format!("could not chdir to {root}: {error}"));
    }
    // The inherited PWD names the caller's directory, not the root the cwd was just set to.
    let error = Command::new(&command[0])
        .args(&command[1..])
        .env("PWD", root.as_str())
        .exec();
    eprintln!(
        "sandbox-exec: could not exec {}: {error}",
        command[0].to_string_lossy()
    );
    let code = if error.kind() == std::io::ErrorKind::NotFound {
        NOT_FOUND
    } else {
        NOT_EXECUTABLE
    };
    std::process::exit(code);
}

#[cfg(test)]
/// The test binary is not `explorer`, so the tests that run it build the real one first.
pub(crate) fn built_explorer() -> std::path::PathBuf {
    use serde_json::Value;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::OnceLock;
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            let output = Command::new(env!("CARGO"))
                .args(["build", "--bin", "explorer", "--message-format=json"])
                .args(cfg!(not(debug_assertions)).then_some("--release"))
                .stderr(Stdio::inherit())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "could not build the explorer binary"
            );
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find_map(|message| message["executable"].as_str().map(PathBuf::from))
                .expect("cargo reported no executable")
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::test_support::temp_root;

    /// Whether namespaces work here; the namespace tests skip without them, the setup check does not.
    fn usable() -> bool {
        use std::process::Stdio;
        let (_dirs, root, workspace) = dirs();
        check(&root, &workspace)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    fn dirs() -> (tempfile::TempDir, camino::Utf8PathBuf, camino::Utf8PathBuf) {
        let (tmp, base) = temp_root();
        let (root, workspace) = (base.join("repo"), base.join("workspace"));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(root.join("real"), "real").unwrap();
        std::fs::write(workspace.join("copy"), "copy").unwrap();
        (tmp, root, workspace)
    }

    /// Kills and reaps the child when dropped, so a failed assertion leaves nothing running.
    struct Reaped(std::process::Child);

    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn exec(
        root: &camino::Utf8Path,
        workspace: &camino::Utf8Path,
        command: &[&str],
    ) -> std::process::Command {
        use std::process::Command;
        let mut cmd = Command::new(built_explorer());
        cmd.arg("sandbox-exec")
            .arg("--root")
            .arg(root)
            .arg("--workspace")
            .arg(workspace)
            .arg("--")
            .args(command);
        cmd
    }

    fn check(root: &camino::Utf8Path, workspace: &camino::Utf8Path) -> std::process::Command {
        use std::process::Command;
        let mut cmd = Command::new(built_explorer());
        cmd.arg("sandbox-exec")
            .arg("--root")
            .arg(root)
            .arg("--workspace")
            .arg(workspace)
            .arg("--check");
        cmd
    }

    #[test]
    fn a_file_written_inside_lands_in_the_workspace() {
        if !usable() {
            return;
        }
        let (_dirs, root, workspace) = dirs();
        let touched = root.join("made");
        let output = exec(
            &root,
            &workspace,
            &["sh", "-c", &format!("echo hi > {touched}")],
        )
        .output()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            workspace.join("made").exists(),
            "the write reached the workspace"
        );
        assert!(!touched.exists(), "the real root was untouched");
    }

    #[test]
    fn the_command_sees_the_workspace_at_the_root_path() {
        if !usable() {
            return;
        }
        let (_dirs, root, workspace) = dirs();
        let output = exec(
            &root,
            &workspace,
            &["sh", "-c", "pwd -P; echo \"$PWD\"; ls"],
        )
        .output()
        .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(lines, [root.as_str(), root.as_str(), "copy"], "{stdout}");
    }

    #[test]
    fn sigterm_reaches_the_command() {
        use rustix::process::{Pid, Signal};
        use std::io::{BufRead, BufReader};
        use std::os::unix::process::ExitStatusExt;
        use std::process::Stdio;
        if !usable() {
            return;
        }
        let (_dirs, root, workspace) = dirs();
        let mut child = Reaped(
            exec(&root, &workspace, &["sh", "-c", "echo $$; exec sleep 30"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        // The shell's pid is the command's pid only if the exec kept the spawned process.
        let mut line = String::new();
        BufReader::new(child.0.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let pid = child.0.id();
        assert_eq!(line.trim().parse::<u32>().unwrap(), pid);
        rustix::process::kill_process(Pid::from_raw(pid as i32).unwrap(), Signal::Term).unwrap();
        let status = child.0.wait().unwrap();
        // The exec'd command dies of the signal, so there is no exit code to check.
        assert_eq!(
            status.signal().and_then(Signal::from_raw),
            Some(Signal::Term),
            "{status}"
        );
    }

    #[test]
    fn check_reports_a_working_setup() {
        let (_dirs, root, workspace) = dirs();
        let status = check(&root, &workspace).status().unwrap();
        assert!(status.success(), "{status}");
    }

    #[test]
    fn a_missing_command_after_dash_dash_is_refused() {
        let (_dirs, root, workspace) = dirs();
        let output = exec(&root, &workspace, &[]).output().unwrap();
        assert_eq!(output.status.code(), Some(SETUP_FAILED));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("no command"), "{stderr}");
    }

    #[test]
    fn a_command_that_does_not_exist_exits_127() {
        if !usable() {
            return;
        }
        let (_dirs, root, workspace) = dirs();
        let output = exec(&root, &workspace, &["/nonexistent/command"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(NOT_FOUND));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("could not exec"), "{stderr}");
    }

    fn exec_read_only(
        root: &camino::Utf8Path,
        read_only: &[&camino::Utf8Path],
        command: &[&str],
    ) -> std::process::Command {
        use std::process::Command;
        let mut cmd = Command::new(built_explorer());
        cmd.arg("sandbox-exec").arg("--root").arg(root);
        for path in read_only {
            cmd.arg("--ro").arg(path);
        }
        cmd.arg("--").args(command);
        cmd
    }

    #[test]
    fn a_read_only_path_refuses_writes_while_the_rest_of_the_root_takes_them() {
        if !usable() {
            return;
        }
        let (_dirs, root, _workspace) = dirs();
        let knowledge = root.join("docs").join("knowledge");
        std::fs::create_dir_all(&knowledge).unwrap();
        let script = format!(
            "{{ echo x > {knowledge}/entry.md; }} 2>&1; echo \"write=$?\"; \
             echo x > {root}/free && echo \"other=$?\""
        );
        let output = exec_read_only(&root, &[&knowledge], &["sh", "-c", &script])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Read-only file system"), "{stdout}");
        assert!(stdout.contains("write=1"), "{stdout}");
        assert!(stdout.contains("other=0"), "{stdout}");
        assert!(!knowledge.join("entry.md").exists());
        assert!(root.join("free").exists());
    }

    #[test]
    fn a_read_only_path_is_read_only_beside_a_workspace_too() {
        if !usable() {
            return;
        }
        let (_dirs, root, workspace) = dirs();
        std::fs::create_dir_all(workspace.join("docs")).unwrap();
        let mut cmd = std::process::Command::new(built_explorer());
        cmd.arg("sandbox-exec")
            .arg("--root")
            .arg(&root)
            .arg("--workspace")
            .arg(&workspace)
            .arg("--ro")
            .arg(root.join("docs"))
            .args(["--", "sh", "-c", "{ echo x > docs/f; } 2>&1; echo $?"]);
        let stdout = String::from_utf8_lossy(&cmd.output().unwrap().stdout).into_owned();
        assert!(stdout.contains("Read-only file system"), "{stdout}");
        assert!(!workspace.join("docs").join("f").exists());
    }

    #[test]
    fn a_missing_read_only_path_is_a_setup_failure() {
        if !usable() {
            return;
        }
        let (_dirs, root, _workspace) = dirs();
        let missing = root.join("nothing");
        let output = exec_read_only(&root, &[&missing], &["true"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(SETUP_FAILED));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("cannot read"), "{stderr}");
    }

    #[test]
    fn check_works_without_a_workspace() {
        let (_dirs, root, _workspace) = dirs();
        let status = std::process::Command::new(built_explorer())
            .args(["sandbox-exec", "--root"])
            .arg(&root)
            .arg("--ro")
            .arg(&root)
            .arg("--check")
            .status()
            .unwrap();
        assert!(status.success(), "{status}");
    }

    #[test]
    fn a_root_inside_the_workspace_is_refused() {
        let (_dirs, root, _workspace) = dirs();
        let parent = root.parent().unwrap();
        let output = exec(&root, parent, &["true"]).output().unwrap();
        assert_eq!(output.status.code(), Some(SETUP_FAILED));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("inside the workspace"), "{stderr}");
    }

    #[test]
    fn a_workspace_inside_the_root_is_mounted_over_it() {
        if !usable() {
            return;
        }
        let (_dirs, root, _workspace) = dirs();
        let inner = root.join("target").join("explorer").join("copy");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("copy"), "copy").unwrap();
        let output = exec(&root, &inner, &["cat", "copy"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout), "copy");
    }
}
