/// Signal sent to a process group Explorer started.
pub(crate) const KILL: libc::c_int = libc::SIGKILL;

/// Sends `signal` to the process group led by `pid`. Plain syscall on a group this
/// process created; a pid that is already gone is ignored by the kernel.
pub(crate) fn kill_group(pid: u32, signal: libc::c_int) {
    // SAFETY: plain syscall on a process group this process created.
    unsafe {
        libc::killpg(pid as libc::pid_t, signal);
    }
}

/// Kills a command's process group unless disarmed: covers timeouts and a cancelled
/// turn dropping the future mid-command.
pub(crate) struct GroupGuard {
    pid: Option<u32>,
    signal: libc::c_int,
}

impl GroupGuard {
    /// A guard that sends `signal` when dropped.
    pub(crate) fn new(pid: u32, signal: libc::c_int) -> Self {
        Self {
            pid: Some(pid),
            signal,
        }
    }

    /// Disarms the guard: the command ended on its own.
    pub(crate) fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            kill_group(pid, self.signal);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Above `PID_MAX_LIMIT`, so no process group can ever carry it; a pid of 0 or a
    /// negative one would name a live group (ours, or every process we may signal).
    const NOBODY: u32 = i32::MAX as u32;

    #[test]
    fn a_disarmed_guard_sends_nothing() {
        let mut guard = GroupGuard::new(NOBODY, KILL);
        guard.disarm();
        drop(guard);
    }

    #[test]
    fn dropping_a_guard_on_a_missing_group_does_not_panic() {
        drop(GroupGuard::new(NOBODY, KILL));
        drop(GroupGuard::new(NOBODY, libc::SIGTERM));
    }
}
