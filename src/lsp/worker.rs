//! The rust-analyzer thread: spawning the server, draining the requests it was given,
//! feeding it warmup files once it is idle and answering what it sends back.

use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;
use std::io::BufReader;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

/// How long the dispatch loop polls stdout before returning to the engine, which keeps it
/// responsive to the requests that arrive meanwhile.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(50);

/// If rust-analyzer never reports `quiescent` (e.g. a broken project), proceed in degraded
/// mode after this long rather than wedging the UI forever.
const READINESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// How long warmup may go without progress before the queue is given up as stuck.
const WARMUP_STALL: std::time::Duration = std::time::Duration::from_secs(60);

/// How many times a warmup file that answered `null` is asked for again.
const WARMUP_RETRIES: u8 = 3;

pub struct LspWorker {
    root: camino::Utf8PathBuf,
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    input_receiver: tokio::sync::broadcast::Receiver<crate::lsp::LspInput>,
    app_tx: crate::signal::MainTx,
    files: Arc<arc_swap::ArcSwap<Vec<Arc<crate::loader::LoadedFile>>>>,
    req_state: crate::lsp::tokens::TokenRequestState,
    warmup_queue: VecDeque<usize>,
    /// Timestamp of the last warmup file actually sent. Warmup yields to in-flight user
    /// requests, so wall-clock time since startup is not a reliable staleness signal; we
    /// force-complete only after a stretch with no warmup *progress*.
    last_warmup_progress: Instant,
    warmup_retries: HashMap<usize, u8>,
    supports_range: bool,
    /// The server's token legend, in the order its answers index it.
    legend: Vec<crate::highlight::TokenKind>,
    dispatch_count: u64,
    ref_queries: HashMap<u64, crate::lsp::symbols::PendingRefQuery>,
    /// Whether rust-analyzer has finished indexing and can resolve cross-file symbols.
    /// Driven by its `experimental/serverStatus` notification (`quiescent`), with a timeout
    /// fallback. Hover→references requests are held until this is true; it flips back to
    /// false when the server re-indexes after edits. Semantic-token requests are *not*
    /// gated on it — they answer early and are sent immediately for fast colors.
    server_ready: bool,
    /// Hover→references requests received before the server was ready, replayed (highest
    /// priority, one per iteration) once it is.
    held_requests: VecDeque<crate::lsp::LspInput>,
    /// Worker start time, for the readiness timeout fallback.
    worker_start: Instant,
}

impl Drop for LspWorker {
    fn drop(&mut self) {
        self.kill_server();
    }
}

impl Debug for LspWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspWorker").finish()
    }
}

impl r3bl_tui::RRTWorker for LspWorker {
    type Config = crate::lsp::LspConfig;
    type Input = crate::lsp::LspInput;
    type Output = ();
    type Interrupt = crate::lsp::LspInterrupt;

    fn create_and_register_os_sources(
        config: Self::Config,
        input_receiver: tokio::sync::broadcast::Receiver<Self::Input>,
    ) -> miette::Result<(Self, Self::Interrupt)> {
        let server = spawn_server(&config.root, &config.cargo_features)?;
        let files = Arc::clone(&config.files);
        let warmup_queue = warmup_queue(&files);
        let now = Instant::now();
        Ok((
            LspWorker {
                root: config.root,
                child: server.child,
                stdin: server.stdin,
                reader: server.reader,
                input_receiver,
                app_tx: config.app_tx,
                files,
                req_state: crate::lsp::tokens::TokenRequestState::new(),
                warmup_queue,
                last_warmup_progress: now,
                warmup_retries: HashMap::new(),
                supports_range: server.supports_range,
                legend: server.legend,
                dispatch_count: 0,
                ref_queries: HashMap::new(),
                server_ready: false,
                held_requests: VecDeque::new(),
                worker_start: now,
            },
            crate::lsp::LspInterrupt,
        ))
    }

    fn restart_policy() -> r3bl_tui::RestartPolicy {
        r3bl_tui::RestartPolicy::default()
    }

    fn block_until_ready_then_dispatch(
        &mut self,
        _sender: &tokio::sync::broadcast::Sender<r3bl_tui::RRTEvent<Self::Output>>,
    ) -> r3bl_tui::Continuation {
        self.dispatch_count += 1;
        tracing::trace!("LSP: dispatch enter #{}", self.dispatch_count);
        if let Some(continuation) = self.drain_requests() {
            return continuation;
        }
        self.check_readiness();
        self.check_warmup();
        if let Some(continuation) = self.drain_one_held() {
            return continuation;
        }
        if let Some(continuation) = self.send_warmup() {
            return continuation;
        }
        if !crate::lsp::rpc::poll_readable(self.reader.get_ref(), READ_TIMEOUT) {
            return r3bl_tui::Continuation::Continue;
        }
        match crate::lsp::rpc::recv_msg(&mut self.reader) {
            Ok(msg) => self.handle_recv_msg(msg),
            Err(e) => {
                tracing::info!(
                    "LSP: dispatch -> Restart (recv_msg error: {:?}, pending={})",
                    e.kind(),
                    self.req_state.pending.len()
                );
                r3bl_tui::Continuation::Restart
            }
        }
    }
}

impl LspWorker {
    /// Takes every request waiting for the worker: the buffered ones first, then the
    /// channel's.
    fn drain_requests(&mut self) -> Option<r3bl_tui::Continuation> {
        for input in crate::lsp::take_buffered_requests() {
            if self.accept(input).is_err() {
                return Some(r3bl_tui::Continuation::Restart);
            }
        }
        loop {
            match self.input_receiver.try_recv() {
                Ok(input) => {
                    if self.accept(input).is_err() {
                        return Some(r3bl_tui::Continuation::Restart);
                    }
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                    tracing::warn!("LSP: input_receiver lagged by {} messages", n);
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    tracing::info!("LSP: dispatch -> Stop (input_receiver closed)");
                    return Some(r3bl_tui::Continuation::Stop);
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            }
        }
        None
    }

    /// Sends `input` to the server, or holds a hover that needs an indexed workspace.
    /// `Err(())` when the worker must restart.
    fn accept(&mut self, input: crate::lsp::LspInput) -> Result<(), ()> {
        use crate::lsp::LspInput;
        match input {
            // Semantic tokens (syntax highlighting) don't need a fully indexed workspace —
            // rust-analyzer answers them early from the file's own syntax — so they go out
            // at once for fast colors, even before quiescent.
            LspInput::TokenRequest(file_idx) => self.send_tokens(file_idx),
            // A qualified name is resolved across files, which genuinely needs the index.
            LspInput::HoverThenReferences {
                file_idx,
                line,
                character,
                word,
                group_id,
            } if !self.server_ready => {
                tracing::debug!(
                    "LSP: holding hover until ready: file_idx={file_idx} line={line} char={character}",
                );
                self.hold_hover(file_idx, line, character, word, group_id);
                Ok(())
            }
            LspInput::HoverThenReferences {
                file_idx,
                line,
                character,
                word,
                group_id,
            } => self
                .send_hover_request(file_idx, line, character, word, group_id)
                .map_err(|_| {
                    tracing::info!("LSP: -> Restart (hover send failed)");
                }),
            LspInput::Restart(selection) => self.restart_server(selection),
        }
    }

    /// Asks for the colors of one file, or closes it on the server when it went away.
    fn send_tokens(&mut self, file_idx: usize) -> Result<(), ()> {
        let snapshot = self.files.load();
        let file = &snapshot[file_idx];
        tracing::debug!(
            "LSP: token request file_idx={} path={} ext={:?}",
            file_idx,
            file.path,
            file.path.extension()
        );
        if file.removed.load(Ordering::Relaxed) {
            crate::lsp::tokens::close_file(&mut self.stdin, file, file_idx, &mut self.req_state)
                .map_err(|_| {
                    tracing::info!(
                        "LSP: -> Restart (close_file failed for file_idx={})",
                        file_idx
                    );
                })
        } else if file.path.extension() != Some("rs") {
            Ok(())
        } else {
            crate::lsp::tokens::request_tokens(
                &mut self.stdin,
                file,
                file_idx,
                self.supports_range,
                &mut self.req_state,
                false,
            )
            .map_err(|_| {
                tracing::info!(
                    "LSP: -> Restart (request_tokens failed for file_idx={})",
                    file_idx
                );
            })
        }
    }

    /// Restarts rust-analyzer on `selection`, the new process replacing the old one only
    /// once its handshake succeeded: a selection it cannot start under leaves the colors
    /// it already gave us in place.
    fn restart_server(
        &mut self,
        selection: crate::config::CargoFeatureSelection,
    ) -> Result<(), ()> {
        tracing::info!("LSP: restarting rust-analyzer for {selection:?}");
        let server = match spawn_server(&self.root, &selection) {
            Ok(server) => server,
            // Restarting here would have the engine drop this worker and kill the server
            // still answering, which is what spawn-before-kill exists to avoid.
            Err(e) => {
                tracing::warn!("LSP: rust-analyzer did not restart, keeping the old one: {e}");
                return Ok(());
            }
        };
        let reopened: Vec<usize> = {
            let mut opened: Vec<usize> = self.req_state.opened.iter().copied().collect();
            opened.sort_unstable();
            opened
        };
        self.kill_server();
        self.child = server.child;
        self.stdin = server.stdin;
        self.reader = server.reader;
        self.supports_range = server.supports_range;
        self.legend = server.legend;
        self.req_state = crate::lsp::tokens::TokenRequestState::new();
        self.ref_queries.clear();
        self.warmup_retries.clear();
        self.warmup_queue = warmup_queue(&self.files);
        self.last_warmup_progress = Instant::now();
        self.worker_start = Instant::now();
        self.server_ready = false;
        for file in self.files.load().iter() {
            file.colored_lines.lock().clear();
        }
        {
            use crate::signal::AppSignal;
            crate::signal::try_send(&self.app_tx, AppSignal::Redraw);
        }
        for file_idx in reopened {
            // A write to the new server may fail here where the old one was healthy;
            // turning that into a restart would have the engine drop this process and
            // spawn the next one on the config's startup feature selection.
            if self.send_tokens(file_idx).is_err() {
                tracing::warn!("LSP: did not reopen file_idx={file_idx} on the new server");
            }
        }
        Ok(())
    }

    fn kill_server(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Proceeds in degraded mode when the server never reports `quiescent`.
    fn check_readiness(&mut self) {
        if !self.server_ready && self.worker_start.elapsed() > READINESS_TIMEOUT {
            tracing::warn!("LSP: readiness timeout, proceeding in degraded mode");
            self.server_ready = true;
        }
    }

    /// Force-completes warmup after a stretch without progress, so a server that stopped
    /// answering cannot pin the queue forever.
    fn check_warmup(&mut self) {
        if !self.warmup_queue.is_empty() && self.last_warmup_progress.elapsed() > WARMUP_STALL {
            tracing::warn!("warmup stalled (no progress for 60s), forcing completion");
            self.warmup_queue.clear();
            self.warmup_retries.clear();
        }
    }

    /// Whether a user-initiated token request, hover or reference query is awaiting an
    /// answer. Warmup yields to those: rust-analyzer is not fed more files while it owes
    /// the user one.
    fn user_busy(&self) -> bool {
        self.req_state
            .pending
            .values()
            .any(|(_, _, is_warmup)| !*is_warmup)
            || !self.ref_queries.is_empty()
    }

    /// Sends one held request, the oldest: a restored session replays many highlight
    /// groups, and one per iteration keeps rust-analyzer from being flooded.
    fn drain_one_held(&mut self) -> Option<r3bl_tui::Continuation> {
        if !self.server_ready || self.user_busy() || self.held_requests.is_empty() {
            return None;
        }
        let input = self
            .held_requests
            .pop_front()
            .expect("just checked non-empty");
        tracing::debug!("LSP: draining held request: {:?}", input);
        self.accept(input)
            .err()
            .map(|()| r3bl_tui::Continuation::Restart)
    }

    /// Sends one warmup file while the server is ready and idle.
    fn send_warmup(&mut self) -> Option<r3bl_tui::Continuation> {
        if self.warmup_queue.is_empty() || !self.server_ready || self.user_busy() {
            return None;
        }
        // Drain one pending response first if available, so a full stdout pipe cannot
        // deadlock the server against its own stdin.
        if !self.reader.buffer().is_empty()
            || crate::lsp::rpc::poll_readable(
                self.reader.get_ref(),
                std::time::Duration::from_millis(10),
            )
        {
            return match crate::lsp::rpc::recv_msg(&mut self.reader) {
                Ok(msg) => Some(self.handle_recv_msg(msg)),
                Err(e) => {
                    tracing::info!(
                        "LSP: dispatch -> Restart (warmup drain recv_msg error: {:?})",
                        e.kind(),
                    );
                    Some(r3bl_tui::Continuation::Restart)
                }
            };
        }

        let file_idx = self
            .warmup_queue
            .pop_front()
            .expect("just checked non-empty");
        let snapshot = self.files.load();
        let file = &snapshot[file_idx];
        tracing::debug!(
            "LSP: warmup send file_idx={} path={} queue_remaining={}",
            file_idx,
            file.path,
            self.warmup_queue.len()
        );
        let is_warmup = !file.needs_full_load.load(Ordering::Relaxed);
        if crate::lsp::tokens::request_tokens(
            &mut self.stdin,
            file,
            file_idx,
            self.supports_range,
            &mut self.req_state,
            is_warmup,
        )
        .is_err()
        {
            self.kill_server();
            tracing::info!(
                "LSP: dispatch -> Restart (warmup request_tokens failed for file_idx={})",
                file_idx
            );
            return Some(r3bl_tui::Continuation::Restart);
        }
        self.last_warmup_progress = Instant::now();
        tracing::debug!(
            "LSP: warmup send OK file_idx={} queue_remaining={}",
            file_idx,
            self.warmup_queue.len()
        );
        None
    }

    /// Queues a hover received before the server was ready, collapsing repeated requests
    /// for the same position so a burst replays only once.
    fn hold_hover(
        &mut self,
        file_idx: usize,
        line: u32,
        character: u32,
        word: Option<String>,
        group_id: Option<usize>,
    ) {
        use crate::lsp::LspInput;
        self.held_requests.retain(|h| {
            !matches!(
                h,
                LspInput::HoverThenReferences { file_idx: f, line: l, character: c, .. }
                if *f == file_idx && *l == line && *c == character
            )
        });
        self.held_requests.push_back(LspInput::HoverThenReferences {
            file_idx,
            line,
            character,
            word,
            group_id,
        });
    }

    fn send_hover_request(
        &mut self,
        file_idx: usize,
        line: u32,
        character: u32,
        word: Option<String>,
        group_id: Option<usize>,
    ) -> std::io::Result<()> {
        let snapshot = self.files.load();
        let file = &snapshot[file_idx];
        let id = self.req_state.take_id();
        let hover_req = crate::lsp::rpc::RpcRequest {
            jsonrpc: "2.0",
            id,
            method: "textDocument/hover",
            params: lsp_types::HoverParams {
                text_document_position_params: lsp_types::TextDocumentPositionParams {
                    text_document: lsp_types::TextDocumentIdentifier {
                        uri: crate::lsp::rpc::file_uri(&file.path),
                    },
                    position: lsp_types::Position { line, character },
                },
                work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
            },
        };
        tracing::debug!("LSP: sending hover id={} file_idx={}", id, file_idx);
        crate::lsp::rpc::send_msg(&mut self.stdin, &hover_req)?;
        use crate::lsp::symbols::RefQueryPhase;
        self.ref_queries.insert(
            id,
            crate::lsp::symbols::PendingRefQuery {
                file_idx,
                line,
                character,
                word,
                phase: RefQueryPhase::Hover,
                qualified_name: None,
                group_id,
            },
        );
        Ok(())
    }

    fn handle_recv_msg(&mut self, msg: crate::lsp::rpc::RpcResponse) -> r3bl_tui::Continuation {
        use crate::signal::AppSignal;
        let method = msg.method.as_deref().unwrap_or("");
        let has_id = msg.id.is_some();
        tracing::debug!(
            "recv: method={:?} has_id={} warmup_queue={}",
            method,
            has_id,
            self.warmup_queue.len()
        );

        // rust-analyzer readiness. `experimental/serverStatus` (enabled through the
        // capability we advertise) reports `quiescent`: false while it indexes, true once
        // it can answer feature requests. `health` (ok/warning/error) does not gate
        // readiness — an errored project can still answer what it has.
        if method == "experimental/serverStatus" {
            let quiescent = msg
                .params
                .as_ref()
                .and_then(|p| p.get("quiescent"))
                .and_then(|q| q.as_bool())
                .unwrap_or(false);
            if quiescent != self.server_ready {
                tracing::info!("LSP: server_ready {} -> {}", self.server_ready, quiescent);
            }
            self.server_ready = quiescent;
            return r3bl_tui::Continuation::Continue;
        }

        // Acknowledge the server's own requests. Per JSON-RPC 2.0 every request (a message
        // with an id) gets a response, even if the server only wants a null; the
        // diagnostics refresh is one Explorer ignores otherwise, and answering it keeps
        // rust-analyzer from retrying.
        if msg.method.is_some()
            && let Some(ref id) = msg.id
        {
            match method {
                "window/workDoneProgress/create" | "workspace/diagnostic/refresh" => {
                    tracing::debug!("replying to server request: method={:?} id={}", method, id);
                    let reply = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": null});
                    if crate::lsp::rpc::send_msg(&mut self.stdin, &reply).is_err() {
                        tracing::info!(
                            "LSP: handle_recv_msg -> Restart (reply to server request failed)"
                        );
                        return r3bl_tui::Continuation::Restart;
                    }
                }
                _ => {
                    tracing::warn!("unhandled server request: method={:?} id={:?}", method, id);
                }
            }
        }

        let mut notify = false;

        if let Some(id) = msg.id.as_ref().and_then(|v| v.as_u64())
            && let Some((file_idx, is_range, is_warmup)) = self.req_state.pending.remove(&id)
        {
            let tokens: Option<lsp_types::SemanticTokens> = msg
                .result
                .as_ref()
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            tracing::debug!(
                "LSP: token response id={} file_idx={} is_range={} is_warmup={} has_data={}",
                id,
                file_idx,
                is_range,
                is_warmup,
                tokens.is_some(),
            );
            match tokens {
                Some(tokens) => {
                    let snapshot = self.files.load();
                    notify = crate::lsp::tokens::apply_tokens(
                        &snapshot[file_idx],
                        &tokens,
                        is_range,
                        &self.legend,
                    );
                }
                None if is_warmup => {
                    let retries = self.warmup_retries.entry(file_idx).or_insert(0);
                    *retries += 1;
                    tracing::debug!("warmup null: file_idx={} retry={}", file_idx, retries);
                    if *retries < WARMUP_RETRIES {
                        self.warmup_queue.push_back(file_idx);
                    } else {
                        tracing::debug!("warmup give up: file_idx={}", file_idx);
                    }
                }
                None => {}
            }
        }

        if let Some(id) = msg.id.as_ref().and_then(|v| v.as_u64())
            && let Some(pending) = self.ref_queries.remove(&id)
            && let Some(continuation) = self.handle_ref_reply(pending, msg.result)
        {
            return continuation;
        }

        if notify {
            crate::signal::try_send(&self.app_tx, AppSignal::Redraw);
        }

        r3bl_tui::Continuation::Continue
    }

    /// Answers a hover with the references request it enables, and a references answer with
    /// the highlight group the UI draws.
    fn handle_ref_reply(
        &mut self,
        pending: crate::lsp::symbols::PendingRefQuery,
        result: Option<serde_json::Value>,
    ) -> Option<r3bl_tui::Continuation> {
        use crate::lsp::symbols::RefQueryPhase;
        use crate::signal::AppSignal;
        use crate::signal::SymbolHighlight;
        use crate::signal::SymbolRefLocation;
        match pending.phase {
            RefQueryPhase::Hover => {
                let hover: Option<lsp_types::Hover> =
                    result.and_then(|v| serde_json::from_value(v).ok());
                let name = hover.as_ref().and_then(|hover| {
                    crate::lsp::symbols::qualified_name(&hover.contents, pending.word.as_deref())
                });
                let Some(name) = name else {
                    // A hover the server did not answer at all while it went back to
                    // indexing: re-hold and retry once it is quiescent. A hover it answered
                    // without a usable qualified name (a keyword, a builtin, no `::`, or a
                    // symbol that is gone) resolves to empty, so the group settles instead of
                    // hanging.
                    if hover.is_none() && !self.server_ready {
                        tracing::debug!(
                            "LSP: hover null while not ready, re-holding file_idx={} line={} char={}",
                            pending.file_idx,
                            pending.line,
                            pending.character,
                        );
                        self.hold_hover(
                            pending.file_idx,
                            pending.line,
                            pending.character,
                            pending.word,
                            pending.group_id,
                        );
                        return Some(r3bl_tui::Continuation::Continue);
                    }
                    // blocking_send: this runs on the RRT thread, and a dropped result would
                    // strand the group in limbo forever.
                    let _ = self.app_tx.blocking_send(
                        r3bl_tui::TerminalWindowMainThreadSignal::ApplyAppSignal(
                            AppSignal::SymbolHighlight(SymbolHighlight {
                                qualified_name: String::new(),
                                group_id: pending.group_id,
                                origin: crate::loader::FileKey(pending.file_idx),
                                origin_line: pending.line,
                                origin_char: pending.character,
                                origin_word: pending.word,
                                origin_locations: Vec::new(),
                                reference_locations: Vec::new(),
                            }),
                        ),
                    );
                    return Some(r3bl_tui::Continuation::Continue);
                };
                let snapshot = self.files.load();
                let file = &snapshot[pending.file_idx];
                let refs_id = self.req_state.take_id();
                let refs_req = crate::lsp::rpc::RpcRequest {
                    jsonrpc: "2.0",
                    id: refs_id,
                    method: "textDocument/references",
                    params: lsp_types::ReferenceParams {
                        text_document_position: lsp_types::TextDocumentPositionParams {
                            text_document: lsp_types::TextDocumentIdentifier {
                                uri: crate::lsp::rpc::file_uri(&file.path),
                            },
                            position: lsp_types::Position {
                                line: pending.line,
                                character: pending.character,
                            },
                        },
                        context: lsp_types::ReferenceContext {
                            include_declaration: true,
                        },
                        work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
                        partial_result_params: lsp_types::PartialResultParams::default(),
                    },
                };
                tracing::debug!(
                    "LSP: hover succeeded name={:?}, sending references id={}",
                    name,
                    refs_id,
                );
                if crate::lsp::rpc::send_msg(&mut self.stdin, &refs_req).is_err() {
                    tracing::info!("LSP: refs send failed");
                    return Some(r3bl_tui::Continuation::Restart);
                }
                self.ref_queries.insert(
                    refs_id,
                    crate::lsp::symbols::PendingRefQuery {
                        file_idx: pending.file_idx,
                        line: pending.line,
                        character: pending.character,
                        word: pending.word.clone(),
                        phase: RefQueryPhase::References,
                        qualified_name: Some(name),
                        group_id: pending.group_id,
                    },
                );
                tracing::debug!(
                    "LSP: references request sent, pending refs count={}",
                    self.ref_queries.len()
                );
                Some(r3bl_tui::Continuation::Continue)
            }
            RefQueryPhase::References => {
                let Some(name) = pending.qualified_name else {
                    return Some(r3bl_tui::Continuation::Continue);
                };
                let locations: Vec<lsp_types::Location> = result
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default();
                let snapshot = self.files.load();
                let origin_path = &snapshot[pending.file_idx].path;
                let mut origin_locations: Vec<SymbolRefLocation> = Vec::new();
                let mut reference_locations: Vec<SymbolRefLocation> = Vec::new();
                for loc in locations {
                    let Some(path) = loc.uri.as_str().strip_prefix("file://") else {
                        continue;
                    };
                    let path = camino::Utf8PathBuf::from(path);
                    let Some(file_key) = crate::loader::file_key_for_path(&snapshot, &path) else {
                        continue;
                    };
                    let file = &snapshot[file_key.0];
                    let guard = file.data.read();
                    let line_starts = &guard.line_starts;
                    let content = &guard.content;
                    let line = loc.range.start.line as usize;
                    if line >= line_starts.len() {
                        continue;
                    }
                    let line_start = line_starts[line];
                    let line_str = &content[line_start..];
                    let start_byte = line_start
                        + crate::lsp::utf16_to_byte(line_str, loc.range.start.character as usize);
                    let end_byte = line_start
                        + crate::lsp::utf16_to_byte(line_str, loc.range.end.character as usize);
                    drop(guard);
                    let location = SymbolRefLocation {
                        file_key,
                        start_byte,
                        end_byte,
                    };
                    if path == *origin_path {
                        origin_locations.push(location);
                    } else {
                        reference_locations.push(location);
                    }
                }
                tracing::debug!(
                    "LSP: references response name={} origin_count={} ref_count={} group_id={:?}",
                    name,
                    origin_locations.len(),
                    reference_locations.len(),
                    pending.group_id,
                );
                let _ = self.app_tx.blocking_send(
                    r3bl_tui::TerminalWindowMainThreadSignal::ApplyAppSignal(
                        AppSignal::SymbolHighlight(SymbolHighlight {
                            qualified_name: name,
                            group_id: pending.group_id,
                            origin: crate::loader::FileKey(pending.file_idx),
                            origin_line: pending.line,
                            origin_char: pending.character,
                            origin_word: pending.word,
                            origin_locations,
                            reference_locations,
                        }),
                    ),
                );
                Some(r3bl_tui::Continuation::Continue)
            }
        }
    }
}

/// The indices of the files warmup colors, in the order the walk found them.
fn warmup_queue(
    files: &Arc<arc_swap::ArcSwap<Vec<Arc<crate::loader::LoadedFile>>>>,
) -> VecDeque<usize> {
    files
        .load()
        .iter()
        .enumerate()
        .filter(|(_, f)| f.path.extension() == Some("rs"))
        .map(|(i, _)| i)
        .collect()
}

/// A running rust-analyzer, past its handshake.
struct Server {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    /// Whether it can answer a range of lines instead of the whole file.
    supports_range: bool,
    legend: Vec<crate::highlight::TokenKind>,
}

/// Starts rust-analyzer on `root` and completes the handshake.
fn spawn_server(
    root: &camino::Utf8Path,
    selection: &crate::config::CargoFeatureSelection,
) -> miette::Result<Server> {
    use lsp_types::notification::Notification;
    use lsp_types::request::Request;

    let mut child = Command::new("rust-analyzer")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| miette::miette!("failed to spawn rust-analyzer: {e}"))?;
    let mut stdin = child.stdin.take().expect("stdin piped");
    let stdout = child.stdout.take().expect("stdout piped");
    let mut reader = BufReader::new(stdout);

    let root_uri = crate::lsp::rpc::file_uri(root);
    let init_req = crate::lsp::rpc::RpcRequest {
        jsonrpc: "2.0",
        id: 0,
        method: lsp_types::request::Initialize::METHOD,
        params: lsp_types::InitializeParams {
            process_id: Some(std::process::id()),
            capabilities: lsp_types::ClientCapabilities {
                text_document: Some(lsp_types::TextDocumentClientCapabilities {
                    semantic_tokens: Some(lsp_types::SemanticTokensClientCapabilities {
                        requests: lsp_types::SemanticTokensClientCapabilitiesRequests {
                            full: Some(lsp_types::SemanticTokensFullOptions::Bool(true)),
                            range: Some(true),
                        },
                        token_types: vec![],
                        token_modifiers: vec![],
                        formats: vec![lsp_types::TokenFormat::RELATIVE],
                        multiline_token_support: Some(false),
                        overlapping_token_support: Some(false),
                        ..Default::default()
                    }),
                    hover: Some(lsp_types::HoverClientCapabilities::default()),
                    references: Some(lsp_types::ReferenceClientCapabilities::default()),
                    ..Default::default()
                }),
                // Ask for `experimental/serverStatus`, so we know when rust-analyzer has
                // finished indexing (quiescent) and can answer feature requests.
                experimental: Some(serde_json::json!({
                    "serverStatusNotification": true
                })),
                ..Default::default()
            },
            workspace_folders: Some(vec![lsp_types::WorkspaceFolder {
                uri: root_uri.clone(),
                name: "root".to_string(),
            }]),
            initialization_options: crate::lsp::cargo_initialization_options(selection),
            ..Default::default()
        },
    };
    if crate::lsp::rpc::send_msg(&mut stdin, &init_req).is_err() {
        let _ = child.kill();
        return Err(miette::miette!("failed to send initialize request"));
    }

    let (legend, supports_range) = loop {
        let Ok(msg) = crate::lsp::rpc::recv_msg(&mut reader) else {
            let _ = child.kill();
            return Err(miette::miette!("LSP init handshake failed"));
        };
        if msg.id.as_ref().and_then(|v| v.as_u64()) != Some(0) {
            continue;
        }
        let Some(result) = msg
            .result
            .and_then(|v| serde_json::from_value::<lsp_types::InitializeResult>(v).ok())
        else {
            let _ = child.kill();
            return Err(miette::miette!("LSP InitializeResult parse failed"));
        };
        break legend_and_range(&result);
    };

    let notify_init = crate::lsp::rpc::RpcNotification {
        jsonrpc: "2.0",
        method: lsp_types::notification::Initialized::METHOD,
        params: lsp_types::InitializedParams {},
    };
    if crate::lsp::rpc::send_msg(&mut stdin, &notify_init).is_err() {
        let _ = child.kill();
        return Err(miette::miette!("failed to send initialized notification"));
    }

    Ok(Server {
        child,
        stdin,
        reader,
        supports_range,
        legend,
    })
}

/// The server's token legend, mapped once to the kinds the previews paint, and whether it
/// answers a range of lines.
fn legend_and_range(
    result: &lsp_types::InitializeResult,
) -> (Vec<crate::highlight::TokenKind>, bool) {
    use crate::highlight::TokenKind;
    use lsp_types::SemanticTokensServerCapabilities;
    let Some(provider) = result.capabilities.semantic_tokens_provider.as_ref() else {
        return (Vec::new(), false);
    };
    let (legend, range) = match provider {
        SemanticTokensServerCapabilities::SemanticTokensOptions(opts) => (&opts.legend, opts.range),
        SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(opts) => (
            &opts.semantic_tokens_options.legend,
            opts.semantic_tokens_options.range,
        ),
    };
    let types = legend
        .token_types
        .iter()
        .map(|t| TokenKind::from_name(t.as_str()))
        .collect();
    (types, range.unwrap_or(false))
}
