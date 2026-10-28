use crate::tui::*;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tokio::sync::mpsc;

type MainTx = mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>;

pub const NAME: &str = "after-agents";

/// The shell command typed with `after-agents`, run once every agent turn has ended.
#[derive(Clone)]
pub enum AfterAgents {
    /// Waiting for a turn to run and the queue to empty.
    Pending(String),
    /// The agents are done: any input but focus and resize cancels, the command runs at `deadline`.
    Countdown { command: String, deadline: Instant },
    Running {
        command: String,
        run: u64,
        _group: Arc<Group>,
    },
}

impl AfterAgents {
    pub fn command(&self) -> &str {
        match self {
            Self::Pending(command)
            | Self::Countdown { command, .. }
            | Self::Running { command, .. } => command,
        }
    }
}

/// The process group of a running command, killed when the last holder drops it.
pub struct Group {
    pid: Option<u32>,
    reaped: Arc<AtomicBool>,
}

impl Drop for Group {
    fn drop(&mut self) {
        if let Some(pid) = self.pid
            && !self.reaped.load(Ordering::SeqCst)
        {
            // SAFETY: plain syscall on a process group this process created.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Tick {
    Nothing,
    Revert,
    Fire,
}

/// What the 1s tick does to `after`: `idle` means no turn runs and none is queued.
pub fn tick_action(after: &AfterAgents, idle: bool, now: Instant) -> Tick {
    match after {
        AfterAgents::Countdown { .. } if !idle => Tick::Revert,
        AfterAgents::Countdown { deadline, .. } if now >= *deadline => Tick::Fire,
        _ => Tick::Nothing,
    }
}

/// Whole seconds left before the command runs, rounded up.
pub fn seconds_left(deadline: Instant, now: Instant) -> u64 {
    deadline.saturating_duration_since(now).as_secs_f64().ceil() as u64
}

pub fn is_idle(state: &AppState) -> bool {
    state.running_turns.is_empty() && state.queued_turns.is_empty()
}

/// A turn ended: when nothing else runs or waits, the pending command starts its countdown.
pub fn arm(state: &mut AppState, main_tx: MainTx) {
    if !is_idle(state) {
        return;
    }
    let grace = state.after_agents_grace;
    if let Some(AfterAgents::Pending(command)) = &state.after_agents
        && let Some(deadline) = Instant::now().checked_add(grace)
    {
        let command = command.clone();
        state.after_agents = Some(AfterAgents::Countdown { command, deadline });
    }
    tick(state, main_tx);
}

pub fn tick(state: &mut AppState, main_tx: MainTx) {
    let Some(after) = state.after_agents.as_ref() else {
        return;
    };
    match tick_action(after, is_idle(state), Instant::now()) {
        Tick::Nothing => {}
        Tick::Revert => {
            let command = after.command().to_string();
            state.after_agents = Some(AfterAgents::Pending(command));
        }
        Tick::Fire => {
            let command = after.command().to_string();
            state.next_after_agents_run += 1;
            let run = state.next_after_agents_run;
            state.after_agents = spawn(&command, run, &state.root, main_tx);
        }
    }
}

/// `None` when `sh` cannot start: the command is then gone, as a run that ended.
fn spawn(command: &str, run: u64, root: &camino::Utf8Path, main_tx: MainTx) -> Option<AfterAgents> {
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!("after-agents: failed to start sh: {e}");
            return None;
        }
    };
    let pid = child.id();
    let reaped = Arc::new(AtomicBool::new(false));
    let task_reaped = Arc::clone(&reaped);
    let task_command = command.to_string();
    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) if status.success() => {
                tracing::info!("after-agents: `{task_command}` finished")
            }
            Ok(status) => tracing::warn!("after-agents: `{task_command}` failed: {status}"),
            Err(e) => tracing::warn!("after-agents: `{task_command}` could not be awaited: {e}"),
        }
        task_reaped.store(true, Ordering::SeqCst);
        let _ = main_tx
            .send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                AppSignal::AfterAgentsExited { run },
            ))
            .await;
    });
    Some(AfterAgents::Running {
        command: command.to_string(),
        run,
        _group: Arc::new(Group { pid, reaped }),
    })
}

/// The command ended; a stale exit, of a command replaced meanwhile, changes nothing.
pub fn exited(state: &mut AppState, finished: u64) {
    if matches!(&state.after_agents, Some(AfterAgents::Running { run, .. }) if *run == finished) {
        state.after_agents = None;
    }
}

/// Presence of the user during the countdown cancels the command; true when the event is consumed.
pub fn cancel_on_input(state: &mut AppState, input_event: &InputEvent) -> bool {
    if !matches!(state.after_agents, Some(AfterAgents::Countdown { .. })) {
        return false;
    }
    match input_event {
        InputEvent::Keyboard(_) | InputEvent::BracketedPaste(_) => {
            state.after_agents = None;
            true
        }
        InputEvent::Mouse(_) => {
            state.after_agents = None;
            false
        }
        _ => false,
    }
}

/// The status-bar entry: its text and whether it warrants the warning color.
pub fn status_text(after: &AfterAgents, now: Instant) -> (String, bool) {
    match after {
        AfterAgents::Pending(_) => (format!("│ {NAME} "), false),
        AfterAgents::Countdown { deadline, .. } => {
            (format!("│ {NAME} {}s ", seconds_left(*deadline, now)), true)
        }
        AfterAgents::Running { .. } => (format!("│ {NAME} running "), false),
    }
}

/// The command line's placeholder once `after-agents` is typed.
pub fn hint(after: Option<&AfterAgents>, aliases: &[(String, String)]) -> String {
    let first_line = |command: &str| {
        let line = command.lines().next().unwrap_or_default();
        let cut = crate::tui::text_layout::truncate(line, 60);
        if cut.len() < command.trim_end().len() {
            format!("{cut}…")
        } else {
            cut
        }
    };
    let mut hint = match after {
        None => "<shell command>".to_string(),
        Some(AfterAgents::Running { command, .. }) => {
            format!("running: {} · Enter kills", first_line(command))
        }
        Some(other) => format!(
            "<shell command: {}> · Enter clears",
            first_line(other.command())
        ),
    };
    if !aliases.is_empty() {
        let names: Vec<&str> = aliases.iter().map(|(name, _)| name.as_str()).collect();
        hint.push_str(&format!(" · aliases: {}", names.join(", ")));
    }
    hint
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn countdown(deadline: Instant) -> AfterAgents {
        AfterAgents::Countdown {
            command: "git push".into(),
            deadline,
        }
    }

    #[test]
    fn countdown_fires_at_its_deadline_when_idle() {
        let now = Instant::now();
        assert_eq!(
            tick_action(&countdown(now + Duration::from_secs(3)), true, now),
            Tick::Nothing
        );
        assert_eq!(
            tick_action(&countdown(now), true, now + Duration::from_secs(1)),
            Tick::Fire
        );
    }

    #[test]
    fn countdown_reverts_when_a_turn_starts() {
        let now = Instant::now();
        assert_eq!(tick_action(&countdown(now), false, now), Tick::Revert);
    }

    #[test]
    fn pending_and_running_do_nothing_on_a_tick() {
        let now = Instant::now();
        let pending = AfterAgents::Pending("x".into());
        assert_eq!(tick_action(&pending, true, now), Tick::Nothing);
        let running = AfterAgents::Running {
            command: "x".into(),
            run: 1,
            _group: Arc::new(Group {
                pid: None,
                reaped: Arc::default(),
            }),
        };
        assert_eq!(tick_action(&running, true, now), Tick::Nothing);
    }

    #[test]
    fn seconds_left_rounds_up() {
        let now = Instant::now();
        assert_eq!(seconds_left(now + Duration::from_millis(4200), now), 5);
        assert_eq!(seconds_left(now, now + Duration::from_secs(1)), 0);
    }

    #[test]
    fn user_input_during_the_countdown_cancels() {
        let armed = || AppState {
            after_agents: Some(countdown(Instant::now())),
            ..AppState::default()
        };
        let mouse = |kind| {
            InputEvent::Mouse(MouseInput {
                pos: col(0) + row(0),
                kind,
                maybe_modifier_keys: None,
            })
        };
        let key = InputEvent::Keyboard(KeyPress::Plain {
            key: Key::Character('a'),
        });

        let mut state = armed();
        assert!(cancel_on_input(&mut state, &key));
        assert!(state.after_agents.is_none());
        assert!(!cancel_on_input(&mut state, &key));

        let mut state = armed();
        assert!(cancel_on_input(
            &mut state,
            &InputEvent::BracketedPaste("x".into())
        ));
        assert!(state.after_agents.is_none());

        for kind in [
            MouseInputKind::MouseMove,
            MouseInputKind::MouseDown(Button::Left),
            MouseInputKind::ScrollDown,
        ] {
            let mut state = armed();
            assert!(!cancel_on_input(&mut state, &mouse(kind)));
            assert!(state.after_agents.is_none());
        }

        let mut state = armed();
        assert!(!cancel_on_input(
            &mut state,
            &InputEvent::Focus(FocusEvent::Gained)
        ));
        assert!(!cancel_on_input(
            &mut state,
            &InputEvent::Resize(Size::default())
        ));
        assert!(state.after_agents.is_some());
    }

    #[test]
    fn a_stale_exit_leaves_a_newer_run_alone() {
        let mut state = AppState::default();
        state.after_agents = Some(AfterAgents::Running {
            command: "x".into(),
            run: 2,
            _group: Arc::new(Group {
                pid: None,
                reaped: Arc::default(),
            }),
        });
        exited(&mut state, 1);
        assert!(state.after_agents.is_some());
        exited(&mut state, 2);
        assert!(state.after_agents.is_none());
    }

    #[test]
    fn arming_leaves_a_running_command_alone() {
        let (main_tx, _main_rx) = mpsc::channel(1);
        let mut state = AppState {
            after_agents: Some(AfterAgents::Running {
                command: "x".into(),
                run: 1,
                _group: Arc::new(Group {
                    pid: None,
                    reaped: Arc::default(),
                }),
            }),
            ..AppState::default()
        };
        arm(&mut state, main_tx);
        assert!(matches!(
            state.after_agents,
            Some(AfterAgents::Running { run: 1, .. })
        ));
    }

    #[test]
    fn an_overflowing_grace_leaves_the_command_pending() {
        let (main_tx, _main_rx) = mpsc::channel(1);
        let mut state = AppState {
            after_agents: Some(AfterAgents::Pending("x".into())),
            after_agents_grace: Duration::from_secs(u64::MAX),
            ..AppState::default()
        };
        arm(&mut state, main_tx);
        assert!(matches!(state.after_agents, Some(AfterAgents::Pending(_))));
    }

    #[test]
    fn hint_names_the_command() {
        assert_eq!(hint(None, &[]), "<shell command>");
        let pending = AfterAgents::Pending("git push\nmore".into());
        assert_eq!(
            hint(Some(&pending), &[]),
            "<shell command: git push…> · Enter clears"
        );
        let aliases = [
            ("a".to_string(), "x".to_string()),
            ("b".to_string(), "y".to_string()),
        ];
        assert_eq!(hint(None, &aliases), "<shell command> · aliases: a, b");
    }
}
