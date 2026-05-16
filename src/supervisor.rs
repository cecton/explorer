use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

/// How long a task must stay alive before its restart counter is reset.
const STABILITY_THRESHOLD: Duration = Duration::from_secs(30);

/// Base delay before the first restart; doubles on each consecutive failure.
const BASE_RESTART_DELAY: Duration = Duration::from_secs(1);

/// Maximum back-off delay regardless of failure count.
const MAX_RESTART_DELAY: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    Restarting,
}

pub type SpawnFn =
    Box<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync + 'static>;

struct Entry {
    name: &'static str,
    spawn: SpawnFn,
    handle: JoinHandle<()>,
    status: TaskStatus,
    restart_attempts: u32,
    started_at: Instant,
    /// Earliest time the next restart may fire.
    restart_after: Option<Instant>,
}

impl Entry {
    fn new(name: &'static str, spawn: SpawnFn) -> Self {
        let handle = tokio::spawn(spawn());
        Self {
            name,
            spawn,
            handle,
            status: TaskStatus::Running,
            restart_attempts: 0,
            started_at: Instant::now(),
            restart_after: None,
        }
    }

    fn restart_delay(&self) -> Duration {
        let shift = self.restart_attempts.min(5);
        (BASE_RESTART_DELAY * (1 << shift)).min(MAX_RESTART_DELAY)
    }
}

/// Lightweight supervisor for a small, fixed set of long-lived async tasks.
///
/// Call [`Supervisor::poll`] regularly (e.g. on every render) to detect
/// failures and apply scheduled restarts. No background task is spawned.
pub struct Supervisor {
    entries: Vec<Entry>,
}

impl Supervisor {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Register a task by name and an async factory that (re)spawns it.
    ///
    /// The factory is called immediately and again on every restart.
    pub fn add(&mut self, name: &'static str, spawn: SpawnFn) {
        self.entries.push(Entry::new(name, spawn));
    }

    /// Poll all tasks. Returns a list of names that changed status this call.
    ///
    /// Call this on every render frame; `is_finished()` is non-blocking.
    pub fn poll(&mut self) -> Vec<(&'static str, TaskStatus)> {
        let now = Instant::now();
        let mut events = Vec::new();

        for entry in &mut self.entries {
            match entry.status {
                TaskStatus::Running => {
                    if entry.handle.is_finished() {
                        // Reset restart counter if task was stable long enough.
                        if entry.started_at.elapsed() >= STABILITY_THRESHOLD {
                            entry.restart_attempts = 0;
                        }
                        let delay = entry.restart_delay();
                        entry.restart_attempts = entry.restart_attempts.saturating_add(1);
                        entry.restart_after = Some(now + delay);
                        entry.status = TaskStatus::Restarting;
                        events.push((entry.name, TaskStatus::Restarting));
                    }
                }
                TaskStatus::Restarting => {
                    if entry.restart_after.is_none_or(|t| now >= t) {
                        entry.handle = tokio::spawn((entry.spawn)());
                        entry.started_at = now;
                        entry.restart_after = None;
                        entry.status = TaskStatus::Running;
                        events.push((entry.name, TaskStatus::Running));
                    }
                }
            }
        }

        events
    }
}
