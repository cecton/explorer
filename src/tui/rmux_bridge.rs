use r3bl_tui::OffscreenBuffer;
use rmux_sdk::{EnsureSession, Pane, Rmux, Session, SessionName, SplitDirection, TerminalSizeSpec};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

pub enum RmuxCommand {
    CreatePane {
        response_tx: std::sync::mpsc::Sender<u64>,
        size: r3bl_tui::Size,
    },
    SendInput {
        pane_id: u64,
        data: Vec<u8>,
    },
    ResizePane {
        pane_id: u64,
        cols: u16,
        rows: u16,
    },
    Shutdown,
}

pub enum RmuxEvent {
    Render {
        pane_id: u64,
        ofs_buf: Box<OffscreenBuffer>,
        mode: u32,
    },
    Exited {
        pane_id: u64,
    },
}

pub struct RmuxBridge {
    pub cmd_tx: UnboundedSender<RmuxCommand>,
    pub event_rx: UnboundedReceiver<RmuxEvent>,
    notify: Arc<std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>>,
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl RmuxBridge {
    pub fn spawn() -> Self {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let notify = Arc::new(std::sync::OnceLock::new());

        let thread_notify = notify.clone();
        let thread_handle = std::thread::Builder::new()
            .name("rmux-bridge".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .worker_threads(2)
                    .build()
                    .expect("failed to build tokio runtime for rmux bridge");
                rt.block_on(run_rmux_bridge(cmd_rx, event_tx, thread_notify));
            })
            .expect("failed to spawn rmux bridge thread");

        Self {
            cmd_tx,
            event_rx,
            notify,
            thread_handle: Some(thread_handle),
        }
    }

    pub fn set_notify(&self, f: Box<dyn Fn() + Send + Sync>) {
        let _ = self.notify.set(f);
    }
}

impl Drop for RmuxBridge {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(RmuxCommand::Shutdown);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

async fn connect_daemon() -> Rmux {
    match Rmux::connect_or_start().await {
        Ok(rmux) => rmux,
        Err(e) => {
            tracing::warn!("Failed to connect to rmux daemon: {e}");
            Rmux::builder().build()
        }
    }
}

async fn ensure_session(rmux: &Rmux) -> Option<Session> {
    let name = match SessionName::new("explorer") {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("Invalid session name: {e}");
            return None;
        }
    };

    match rmux
        .ensure_session(
            EnsureSession::named(name)
                .shell(shell_cmd())
                .size(TerminalSizeSpec::new(120, 65))
                .create_or_reuse(),
        )
        .await
    {
        Ok(session) => Some(session),
        Err(e) => {
            tracing::warn!("Failed to create rmux session: {e}");
            None
        }
    }
}

async fn create_pane(session: &Session, panes: &HashMap<u64, Pane>, pane_id: u64) -> Option<Pane> {
    if pane_id == 1 {
        return Some(session.pane(0, 0));
    }

    let existing = panes.values().next()?;
    let shell = shell_cmd();

    match existing
        .split_with(SplitDirection::Right)
        .shell(shell)
        .await
    {
        Ok(pane) => Some(pane),
        Err(e) => {
            tracing::warn!("Failed to split pane: {e}");
            None
        }
    }
}

fn spawn_forwarder(
    pane_id: u64,
    pane: Pane,
    event_tx: UnboundedSender<RmuxEvent>,
    notify: Arc<std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>>,
) {
    tokio::spawn(async move {
        let snapshot = match pane.snapshot().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Initial snapshot failed for pane {pane_id}: {e}");
                let _ = event_tx.send(RmuxEvent::Exited { pane_id });
                return;
            }
        };

        let mode = snapshot.mode;
        let ofs_buf = r3bl_rmux::to_offscreen_buffer(&snapshot);
        let _ = event_tx.send(RmuxEvent::Render {
            pane_id,
            ofs_buf: Box::new(ofs_buf),
            mode,
        });
        if let Some(f) = notify.get() {
            f();
        }

        let mut stream = match pane.render_stream().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Render stream failed for pane {pane_id}: {e}");
                let _ = event_tx.send(RmuxEvent::Exited { pane_id });
                return;
            }
        };

        loop {
            match stream.next().await {
                Ok(Some(update)) => {
                    let snapshot = update.into_snapshot();
                    let mode = snapshot.mode;
                    let ofs_buf = r3bl_rmux::to_offscreen_buffer(&snapshot);
                    let _ = event_tx.send(RmuxEvent::Render {
                        pane_id,
                        ofs_buf: Box::new(ofs_buf),
                        mode,
                    });
                    if let Some(f) = notify.get() {
                        f();
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("Render stream error for pane {pane_id}: {e}");
                    break;
                }
            }
        }

        let _ = event_tx.send(RmuxEvent::Exited { pane_id });
    });
}

async fn run_rmux_bridge(
    mut cmd_rx: UnboundedReceiver<RmuxCommand>,
    event_tx: UnboundedSender<RmuxEvent>,
    notify: Arc<std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>>,
) {
    let rmux = connect_daemon().await;
    let Some(session) = ensure_session(&rmux).await else {
        tracing::warn!("rmux bridge: no session, terminal panes will not work");
        loop {
            if cmd_rx.recv().await.is_none() {
                break;
            }
        }
        return;
    };

    let window = session.window(0);
    let mut panes: HashMap<u64, Pane> = HashMap::new();
    let mut next_pane_id: u64 = 1;
    let mut last_snapshot_sizes: HashMap<u64, (u16, u16)> = HashMap::new();

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    break;
                };
                match cmd {
                    RmuxCommand::Shutdown => break,
                    RmuxCommand::CreatePane {
                        response_tx,
                        size,
                    } => {
                        let pane_id = next_pane_id;
                        next_pane_id += 1;

                        let Some(pane) = create_pane(&session, &panes, pane_id).await else {
                            let _ = response_tx.send(0);
                            continue;
                        };

                        let cols = size.col_width.as_u16();
                        let rows = size.row_height.as_u16();
                        if let Err(e) = window.resize(Some(cols), Some(rows)).await {
                            tracing::error!(
                                "initial resize failed for pane {pane_id}: {e}"
                            );
                        }

                        let et = event_tx.clone();
                        let n = notify.clone();
                        spawn_forwarder(pane_id, pane.clone(), et, n);

                        panes.insert(pane_id, pane);
                        let _ = response_tx.send(pane_id);
                    }
                    RmuxCommand::SendInput { pane_id, data } => {
                        if let Some(pane) = panes.get(&pane_id) {
                            let text = String::from_utf8_lossy(&data);
                            let _ = pane.send_text(text.as_ref()).await;
                        }
                    }
                    RmuxCommand::ResizePane {
                        pane_id,
                        cols,
                        rows,
                    } => {
                        if let Err(e) = window.resize(Some(cols), Some(rows)).await {
                            tracing::error!(
                                "window resize failed for pane {pane_id}: {e}"
                            );
                        }
                        if let Some(pane) = panes.get(&pane_id) {
                            // Take a snapshot after resize. Only forward if the
                            // dimensions actually changed — otherwise we loop
                            // infinitely on every render cycle (since the render
                            // stream doesn't fire on SIGWINCH alone).
                            if let Ok(s) = pane.snapshot().await {
                                let new_size = (s.cols, s.rows);
                                if last_snapshot_sizes.get(&pane_id) != Some(&new_size) {
                                    let mode = s.mode;
                                    let ofs_buf =
                                        r3bl_rmux::to_offscreen_buffer(&s);
                                    let _ = event_tx.send(RmuxEvent::Render {
                                        pane_id,
                                        ofs_buf: Box::new(ofs_buf),
                                        mode,
                                    });
                                    if let Some(f) = notify.get() {
                                        f();
                                    }
                                    last_snapshot_sizes.insert(pane_id, new_size);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    drop(panes);
    tracing::debug!("rmux bridge thread shut down");
}

fn shell_cmd() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "bash".into())
}
