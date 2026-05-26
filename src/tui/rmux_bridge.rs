use r3bl_tui::core::pty::{
    DefaultPtySessionConfig, PtyInputEvent, PtyOutputEvent, PtySessionBuilder,
    PtySessionConfigOption,
};
use r3bl_tui::{Size, height, width};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;

/// Commands from the TUI main thread to the rmux bridge background thread.
pub enum RmuxCommand {
    CreatePane {
        response_tx: std::sync::mpsc::Sender<u64>,
        size: Size,
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

/// Events from the rmux bridge background thread back to the TUI main thread.
pub enum RmuxEvent {
    Output { pane_id: u64, data: Vec<u8> },
    Exited { pane_id: u64 },
}

/// Handle to the rmux bridge background thread and its communication channels.
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
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
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

    /// Set a callback that the bridge thread calls after producing an [`RmuxEvent::Output`].
    /// Used by the main thread to request a re-render when new terminal output arrives.
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

async fn run_rmux_bridge(
    mut cmd_rx: UnboundedReceiver<RmuxCommand>,
    event_tx: UnboundedSender<RmuxEvent>,
    notify: Arc<std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>>,
) {
    let mut sessions: HashMap<u64, tokio::sync::mpsc::Sender<PtyInputEvent>> = HashMap::new();
    let mut next_pane_id: u64 = 1;

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

                        let create_result = tokio::task::spawn_blocking(move || {
                            PtySessionBuilder::new(shell_cmd())
                                .env_var("TERM", "xterm-256color")
                                .with_config(
                                    DefaultPtySessionConfig
                                        + PtySessionConfigOption::Size(size),
                                )
                                .start()
                        })
                        .await;

                        match create_result {
                            Ok(Ok(session)) => {
                                let input_tx = session.tx_input_event.clone();
                                let pid = pane_id;
                                let et = event_tx.clone();

                                let fwd_notify = notify.clone();
                                tokio::spawn(async move {
                                    let mut rx = session.rx_output_event;
                                    while let Some(event) = rx.recv().await {
                                        match event {
                                            PtyOutputEvent::Output(bytes) => {
                                                let _ =
                                                    et.send(RmuxEvent::Output {
                                                        pane_id: pid,
                                                        data: bytes,
                                                    });
                                                if let Some(f) = fwd_notify.get() {
                                                    f();
                                                }
                                            }
                                            PtyOutputEvent::Exit(_) => {
                                                let _ =
                                                    et.send(RmuxEvent::Exited {
                                                        pane_id: pid,
                                                    });
                                                break;
                                            }
                                            _ => {}
                                        }
                                    }
                                });

                                sessions.insert(pane_id, input_tx);
                                let _ = response_tx.send(pane_id);
                            }
                            Ok(Err(e)) => {
                                tracing::warn!("Failed to start PTY: {e}");
                                let _ = response_tx.send(0);
                            }
                            Err(e) => {
                                tracing::warn!("PTY spawn task failed: {e}");
                                let _ = response_tx.send(0);
                            }
                        }
                    }
                    RmuxCommand::SendInput { pane_id, data } => {
                        if let Some(tx) = sessions.get(&pane_id) {
                            let _ = tx.try_send(PtyInputEvent::Write(data));
                        }
                    }
                    RmuxCommand::ResizePane {
                        pane_id,
                        cols,
                        rows,
                    } => {
                        if let Some(tx) = sessions.get(&pane_id) {
                            let _ = tx.try_send(PtyInputEvent::Resize(Size {
                                col_width: width(cols),
                                row_height: height(rows),
                            }));
                        }
                    }

                }
            }
        }
    }

    drop(sessions);
    tracing::debug!("rmux bridge thread shut down");
}

fn shell_cmd() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "bash".into())
}
