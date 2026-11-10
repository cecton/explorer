#[allow(clippy::wildcard_imports)] // r3bl is Explorer's shared vocabulary: see AGENTS.md
use r3bl_tui::*;

/// What a terminal pane is started for: a command or an interactive shell, where, and
/// whether it takes an existing window's place or restores a saved one.
pub(crate) struct Request<'a> {
    /// `None` starts the user's shell; `Some(cmd)` runs `sh -c cmd`.
    pub(crate) cmd: Option<String>,
    pub(crate) cwd: Option<camino::Utf8PathBuf>,
    pub(crate) pane_size: Option<crate::tui::pane_manager::PaneSize>,
    /// The id of a terminal being restored from the session: it keeps its id and its place.
    pub(crate) id: Option<usize>,
    /// The window this terminal replaces, when it is opened over a file preview.
    pub(crate) replace_window: Option<crate::tui::pane_manager::Window>,
    /// Where the pane's output events go: the app shell's queue of them.
    pub(crate) event_tx: &'a tokio::sync::mpsc::UnboundedSender<(usize, PtyOutputEvent)>,
}

fn shell_command() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "bash".into())
}

pub(crate) fn open(
    global_data: &mut GlobalData<crate::tui::state::AppState, crate::signal::AppSignal>,
    request: Request<'_>,
) -> CommonResult<EventPropagation> {
    let Request {
        cmd,
        cwd,
        pane_size,
        id,
        replace_window,
        event_tx,
    } = request;
    use crate::signal::AppSignal;
    use crate::tui::pane_manager::Window;
    use crate::tui::state::TerminalPane;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    throws_with_return!({
        let state = &mut global_data.state;
        let restore = id.is_some();
        let id = id.unwrap_or_else(|| {
            let id = state.next_terminal_id;
            state.next_terminal_id += 1;
            id
        });

        let window_size = global_data.window_size;
        // Start with the full window width; the first render will resize to
        // the actual pane slot assigned by the layout.
        let pty_cols = window_size.col_width.as_u16().max(80);
        let pty_rows = window_size.row_height.as_u16().saturating_sub(2);
        let pty_size = VPSize {
            col_width: vp_width(pty_cols),
            row_height: vp_height(pty_rows),
        };

        let is_command_pane = cmd.as_deref().is_some_and(|s| !s.is_empty());
        let mut builder = if is_command_pane {
            PtySessionBuilder::new("/bin/sh").cli_args(["-c", cmd.as_deref().unwrap()])
        } else {
            PtySessionBuilder::new(shell_command())
        };
        if let Some(ref cwd_path) = cwd {
            builder = builder.cwd(cwd_path.as_std_path());
        }
        let mut session = match builder
            .with_config(DefaultPtySessionConfig + PtySessionConfigOption::Size(pty_size))
            .start_async()
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to start PTY: {e}");
                return Ok(EventPropagation::ConsumedRender);
            }
        };

        let ofs_buf = r3bl_tui::OfsBufVT100::new_empty(pty_size);
        let pty_input_tx = Arc::new(session.tx_input_event.clone());
        let child_killer = session.child_process_termination_handle;
        let initial_title = cmd.as_deref().filter(|s| !s.is_empty()).map(|cmd| {
            let mut lines = cmd.lines();
            let first = lines.next().unwrap_or_default();
            match lines.next() {
                Some(_) => format!("{first} …"),
                None => first.to_string(),
            }
        });
        let pane_cwd = cwd.clone().unwrap_or_else(|| state.root.clone());
        let pane_command = if is_command_pane { cmd.clone() } else { None };
        let pane = Arc::new(parking_lot::Mutex::new(TerminalPane {
            ofs_buf,
            title: initial_title,
            pty_input_tx,
            child_killer: Some(child_killer),
            last_size: pty_size,
            is_command_pane,
            exited: false,
            exit_code: None,
            exit_signal: None,
            scroll_offset: 0,
            cwd: pane_cwd,
            command: pane_command,
            flush_pending: Arc::default(),
        }));

        state.terminal_panes.insert(id, Arc::clone(&pane));

        let notify_tx = global_data.main_thread_channel_sender.clone();
        let event_tx = event_tx.clone();
        let flush_pending = Arc::clone(&pane.lock().flush_pending);
        tokio::spawn(async move {
            let mut throttle = Throttle::new();
            while let Some(event) = session.rx_output_event.recv().await {
                let is_exit = matches!(&event, PtyOutputEvent::Exit(_));
                let mut is_buffering_sync = false;
                match event {
                    PtyOutputEvent::Output(bytes) => {
                        let mut pane = pane.lock();
                        match pane.ofs_buf.feed_pty_bytes(&bytes) {
                            Some(result) => {
                                if pane.scroll_offset > 0 {
                                    let true_delta = result
                                        .history_len_after
                                        .saturating_sub(result.history_len_before);
                                    pane.scroll_offset = pane
                                        .scroll_offset
                                        .saturating_add(true_delta)
                                        .min(pane.ofs_buf.get_history_len());
                                }
                                for osc_event in result.osc_events {
                                    if let OscPtyEvent::SetTitleAndTab(title) = osc_event {
                                        pane.title = Some(title);
                                    }
                                }
                                for pty_response in result.pty_response_events {
                                    let _ = pane.pty_input_tx.try_send(PtyInputEvent::Write(
                                        pty_response.to_string().into_bytes(),
                                    ));
                                }
                            }
                            None => {
                                is_buffering_sync = true;
                            }
                        }
                    }
                    PtyOutputEvent::Exit(status) => {
                        if event_tx.send((id, PtyOutputEvent::Exit(status))).is_err() {
                            break;
                        }
                    }
                    _ => {}
                }

                // Exit: always send Redraw (never throttled) so the
                // terminal pane is removed from the UI immediately.
                if is_exit {
                    crate::signal::try_send(&notify_tx, AppSignal::Redraw);
                    break;
                }

                // Synchronized output: skip render signal while the
                // program has requested atomic screen updates via
                // DEC private mode 2026.  OfsBufVT100 buffers raw PTY
                // bytes internally; the first feed_pty_bytes call after
                // sync-off returns Some(result) and triggers a render.
                if is_buffering_sync {
                    continue;
                }

                // A chunk the throttle holds back is in the buffer already: the 1s
                // tick repaints it.
                let announced = throttle.accepts(|| {
                    notify_tx
                        .try_send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                            AppSignal::TerminalOutput,
                        ))
                        .is_ok()
                });
                flush_pending.store(!announced, Ordering::Relaxed);
            }
        });

        let window = Window::Terminal(id);
        if !restore {
            let old = state.pane_manager.focused_window;
            if let Some(old_window) = replace_window {
                if let Window::FilePreview(file_key) = old_window {
                    state.terminal_to_preview.insert(id, file_key);
                }
                state.pane_manager.replace_window(&old_window, window);
            } else {
                state.pane_manager.push_window(window);
            }
            if let Some(size) = pane_size {
                state
                    .pane_manager
                    .window_states
                    .entry(window)
                    .or_default()
                    .pane_size = size;
            }
            state.pane_manager.focused_window = Some(window);
            crate::tui::focus::refocus(state, old);
            state.terminal_grabbed = true;
            state.mark_session_dirty();
        }

        EventPropagation::ConsumedRender
    });
}

/// Sends a repaint for a pane's output, but not for every chunk of it.
///
/// A program that prints continuously produces far more chunks than the screen can show,
/// and one signal per chunk would fill the main thread's channel and starve the loop. The
/// gate closes once a send fails (the channel is full) and stays closed while chunks keep
/// arriving within [`Self::GAP`], so a sustained burst costs one signal, and reopens on the
/// first chunk after a pause. A chunk that was not announced is still in the pane's buffer:
/// the 1s tick repaints it.
///
/// Every chunk must be timed here, suppressed or not: the gate is released by a pause in
/// the *stream*, not by a pause since the last send, so a suppressed chunk that does not
/// stamp the clock leaves a burst able to reopen the gate mid-stream.
pub(crate) struct Throttle {
    last_event: std::time::Instant,
    closed: bool,
}

impl Throttle {
    /// The gap that separates program output (≥10 chunks/s) from interactive typing.
    const GAP: std::time::Duration = std::time::Duration::from_millis(100);

    pub(crate) fn new() -> Self {
        Self {
            last_event: std::time::Instant::now(),
            closed: false,
        }
    }

    /// Whether this chunk should be announced, given whether announcing it succeeded.
    /// Every chunk must be passed here, announced or not, so a burst keeps the gate shut.
    pub(crate) fn accepts(&mut self, announce: impl FnOnce() -> bool) -> bool {
        let now = std::time::Instant::now();
        let gapped = now.duration_since(self.last_event) >= Self::GAP;
        self.last_event = now;
        if self.closed && !gapped {
            return false;
        }
        self.closed = !announce();
        !self.closed
    }
}

fn is_buffer_empty(ofs_buf: &r3bl_tui::OfsBufVT100) -> bool {
    let height = ofs_buf.get_window_size().row_height.as_usize();
    (0..height).all(|r| {
        ofs_buf
            .get_active_screen_buffer()
            .get_row(vp_row(r as u16))
            .unwrap_or(&[])
            .iter()
            .all(|pc| !matches!(pc, PixelChar::PlainText { .. }))
    })
}

/// Applies the exit status of every terminal whose output has ended: a pane whose
/// screen is empty goes at once, an occupied one shows as exited until it is closed.
pub(crate) fn drain_exits(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<(usize, PtyOutputEvent)>,
    state: &mut crate::tui::state::AppState,
) {
    while let Ok((id, event)) = rx.try_recv() {
        if let PtyOutputEvent::Exit(status) = event {
            let exit_code = Some(status.exit_code());
            let exit_signal = status.signal().map(String::from);
            let remove_now = state
                .with_terminal(id, |p| is_buffer_empty(&p.ofs_buf))
                .unwrap_or(false);

            if remove_now {
                crate::tui::focus::close_terminal(state, id);
                state.mark_session_dirty();
            } else {
                state.with_terminal_mut(id, |p| {
                    p.exited = true;
                    p.exit_code = exit_code;
                    p.exit_signal = exit_signal;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Keeps the gate shut on the next call: the tests must not depend on how long they
    /// take, and the throttle's own clock is the only thing a pause changes.
    fn just_now(throttle: &mut Throttle) {
        throttle.last_event = Instant::now();
    }

    #[test]
    fn the_first_chunk_is_announced() {
        assert!(Throttle::new().accepts(|| true));
    }

    #[test]
    fn a_roomy_channel_announces_every_chunk() {
        let mut throttle = Throttle::new();
        for _ in 0..100 {
            assert!(throttle.accepts(|| true));
        }
    }

    #[test]
    fn a_full_channel_closes_the_gate_until_a_pause() {
        let mut throttle = Throttle::new();
        assert!(!throttle.accepts(|| false));
        // The channel has room again, but the chunks keep coming: still no signal.
        just_now(&mut throttle);
        assert!(!throttle.accepts(|| true));
    }

    #[test]
    fn a_pause_reopens_the_gate() {
        let mut throttle = Throttle::new();
        assert!(!throttle.accepts(|| false));
        throttle.last_event = Instant::now() - Throttle::GAP;
        assert!(throttle.accepts(|| true));
    }

    #[test]
    fn every_chunk_is_timed_even_when_the_gate_is_shut() {
        let mut throttle = Throttle::new();
        assert!(!throttle.accepts(|| false));
        // Rewind so the next chunk arrives inside the gap, where it is suppressed. A
        // suppressed chunk still has to time itself: the gate measures the next gap from
        // it, so leaving the stamp behind would let a burst reopen the gate mid-stream.
        throttle.last_event = Instant::now() - Throttle::GAP / 2;
        assert!(!throttle.accepts(|| true));
        assert!(throttle.last_event.elapsed() < Throttle::GAP / 4);
    }
}
