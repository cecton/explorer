use crate::loader::LoadedFile;
use crate::tui::AppSignal;
use crate::tui::SymbolRefLocation;
use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use lsp_types::notification::{DidOpenTextDocument, Initialized, Notification};
use lsp_types::request::{
    Initialize, Request, SemanticTokensFullRequest, SemanticTokensRangeRequest,
};
use lsp_types::{
    ClientCapabilities, DidOpenTextDocumentParams, HoverContents, HoverParams, InitializeParams,
    InitializeResult, InitializedParams, Location, PartialResultParams, Position, Range,
    ReferenceContext, ReferenceParams, SemanticToken, SemanticTokens,
    SemanticTokensClientCapabilities, SemanticTokensClientCapabilitiesRequests,
    SemanticTokensFullOptions, SemanticTokensParams, SemanticTokensRangeParams,
    TextDocumentClientCapabilities, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, TokenFormat, Uri, WorkDoneProgressParams, WorkspaceFolder,
};
use r3bl_tui::core::ThreadState;
use r3bl_tui::{
    Continuation, RRT, RRTEvent, RRTSoftwareInterrupt, RRTWorker, RestartPolicy,
    TerminalWindowMainThreadSignal,
};
use rustix::event::{PollFd, PollFlags, poll as rpoll};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Debug;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::io::AsFd;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::sync::mpsc as tokio_mpsc;

#[derive(Clone, Debug)]
pub enum LspInput {
    TokenRequest(usize),
    HoverThenReferences {
        file_idx: usize,
        line: u32,
        character: u32,
        word: Option<String>,
        group_id: Option<usize>,
    },
}

pub type ColoredSpan = (usize, usize, &'static str);
pub type ColoredLine = Vec<ColoredSpan>;

static TOKEN_TYPES: OnceLock<Vec<&'static str>> = OnceLock::new();

const RANGE_LINES: usize = 200;
const RANGE_THRESHOLD: usize = 100;

/// Timeout for blocking stdout poll. Keeps the loop responsive to request_rx.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(50);

#[derive(Serialize)]
struct RpcRequest<P: Serialize> {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    params: P,
}

#[derive(Serialize)]
struct RpcNotification<P: Serialize> {
    jsonrpc: &'static str,
    method: &'static str,
    params: P,
}

#[derive(Deserialize)]
struct RpcResponse {
    id: Option<Value>,
    method: Option<String>,
    result: Option<Value>,
}

// ── Interrupt ─────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct LspInterrupt;

impl RRTSoftwareInterrupt for LspInterrupt {
    fn trigger_software_interrupt(&self) {}
}

// ── RRT singleton ─────────────────────────────────────────────────────────────

pub static LSP_RRT: RRT<LspWorker> = RRT::new();

// ── Shared config set before first subscribe ──────────────────────────────────

#[derive(Clone)]
pub struct LspConfig {
    pub root: Utf8PathBuf,
    pub files: Arc<ArcSwap<Vec<Arc<LoadedFile>>>>,
    pub app_tx: tokio_mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
}

impl Debug for LspConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspConfig")
            .field("root", &self.root)
            .finish()
    }
}

// ── Worker ────────────────────────────────────────────────────────────────────

struct TokenRequestState {
    next_id: u64,
    pending: HashMap<u64, (usize, bool, bool)>,
    opened: HashSet<usize>,
}

#[derive(Clone, Debug)]
enum RefQueryPhase {
    Hover,
    References,
}

struct PendingRefQuery {
    file_idx: usize,
    line: u32,
    character: u32,
    word: Option<String>,
    phase: RefQueryPhase,
    qualified_name: Option<String>,
    group_id: Option<usize>,
}

pub struct LspWorker {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    input_receiver: tokio::sync::broadcast::Receiver<LspInput>,
    app_tx: tokio_mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
    files: Arc<ArcSwap<Vec<Arc<LoadedFile>>>>,
    req_state: TokenRequestState,
    warmup_queue: VecDeque<usize>,
    /// Timestamp of the last warmup file actually sent. Warmup yields to
    /// in-flight user requests, so wall-clock time since startup is not a
    /// reliable staleness signal; we force-complete only after a stretch with
    /// no warmup *progress* (see the timeout in the dispatch loop).
    last_warmup_progress: Instant,
    warmup_retries: HashMap<usize, u8>,
    supports_range: bool,
    dispatch_count: u64,
    ref_queries: HashMap<u64, PendingRefQuery>,
    /// Hover requests received during warmup, replayed once warmup completes:
    /// (file_idx, line, character, word, group_id).
    deferred_hovers: VecDeque<(usize, u32, u32, Option<String>, Option<usize>)>,
    /// Retry counts for group-rebuild hovers (group_id: Some) keyed by
    /// (file_idx, line, character). Our own tiny per-file warmup queue draining
    /// doesn't mean rust-analyzer has finished indexing the workspace — a hover
    /// sent right as warmup drains (e.g. right after session-restore re-queries
    /// fire at startup) commonly returns null just because the server isn't
    /// ready yet, not because the symbol is gone. Only rebuild/restore hovers
    /// retry; a user-initiated double-click (group_id: None) fails immediately,
    /// since by the time a human clicks, the server has had time to settle.
    hover_retries: HashMap<(usize, u32, u32), u8>,
}

const MAX_HOVER_RETRIES: u8 = 10;
/// Delay between rebuild-hover retries (see `hover_retries`). 10 retries * 300ms
/// gives rust-analyzer up to 3s to finish indexing after a cold start.
const HOVER_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(300);

impl Drop for LspWorker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Debug for LspWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspWorker").finish()
    }
}

impl RRTWorker for LspWorker {
    type Config = LspConfig;
    type Input = LspInput;
    type Output = ();
    type Interrupt = LspInterrupt;

    fn create_and_register_os_sources(
        config: Self::Config,
        input_receiver: tokio::sync::broadcast::Receiver<Self::Input>,
    ) -> miette::Result<(Self, Self::Interrupt)> {
        let app_tx = config.app_tx.clone();

        let mut child = Command::new("rust-analyzer")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| miette::miette!("failed to spawn rust-analyzer: {e}"))?;

        let mut stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let mut reader = BufReader::new(stdout);

        let root = &config.root;
        let root_uri: Uri = format!("file://{root}").parse().expect("valid root URI");
        let pid = std::process::id();

        let init_req = RpcRequest {
            jsonrpc: "2.0",
            id: 0,
            method: Initialize::METHOD,
            params: InitializeParams {
                process_id: Some(pid),
                capabilities: ClientCapabilities {
                    text_document: Some(TextDocumentClientCapabilities {
                        semantic_tokens: Some(SemanticTokensClientCapabilities {
                            requests: SemanticTokensClientCapabilitiesRequests {
                                full: Some(SemanticTokensFullOptions::Bool(true)),
                                range: Some(true),
                            },
                            token_types: vec![],
                            token_modifiers: vec![],
                            formats: vec![TokenFormat::RELATIVE],
                            multiline_token_support: Some(false),
                            overlapping_token_support: Some(false),
                            ..Default::default()
                        }),
                        hover: Some(lsp_types::HoverClientCapabilities::default()),
                        references: Some(lsp_types::ReferenceClientCapabilities::default()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                workspace_folders: Some(vec![WorkspaceFolder {
                    uri: root_uri.clone(),
                    name: "root".to_string(),
                }]),
                ..Default::default()
            },
        };

        if send_msg(&mut stdin, &init_req).is_err() {
            let _ = child.kill();
            return Err(miette::miette!("failed to send initialize request"));
        }

        let supports_range = loop {
            let Ok(msg) = recv_msg(&mut reader) else {
                let _ = child.kill();
                return Err(miette::miette!("LSP init handshake failed"));
            };
            if msg.id.as_ref().and_then(|v| v.as_u64()) == Some(0) {
                let result: InitializeResult =
                    match msg.result.and_then(|v| serde_json::from_value(v).ok()) {
                        Some(r) => r,
                        None => {
                            let _ = child.kill();
                            return Err(miette::miette!("LSP InitializeResult parse failed"));
                        }
                    };
                TOKEN_TYPES.get_or_init(|| {
                    result
                        .capabilities
                        .semantic_tokens_provider
                        .as_ref()
                        .map(|p| match p {
                            lsp_types::SemanticTokensServerCapabilities::SemanticTokensOptions(
                                opts,
                            ) => &opts.legend.token_types,
                            lsp_types::SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(
                                opts,
                            ) => &opts.semantic_tokens_options.legend.token_types,
                        })
                        .map(|types| {
                            types
                                .iter()
                                .map(|t| {
                                    Box::leak(t.as_str().to_string().into_boxed_str()) as &'static str
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                });
                let range = result
                    .capabilities
                    .semantic_tokens_provider
                    .as_ref()
                    .map(|p| match p {
                        lsp_types::SemanticTokensServerCapabilities::SemanticTokensOptions(
                            opts,
                        ) => opts.range.unwrap_or(false),
                        lsp_types::SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(
                            opts,
                        ) => opts.semantic_tokens_options.range.unwrap_or(false),
                    })
                    .unwrap_or(false);
                break range;
            }
        };

        let notify_init = RpcNotification {
            jsonrpc: "2.0",
            method: Initialized::METHOD,
            params: InitializedParams {},
        };
        if send_msg(&mut stdin, &notify_init).is_err() {
            let _ = child.kill();
            return Err(miette::miette!("failed to send initialized notification"));
        }

        let files = Arc::clone(&config.files);
        let warmup_queue: VecDeque<usize> = {
            let snapshot = files.load();
            snapshot
                .iter()
                .enumerate()
                .filter(|(_, f)| f.path.extension() == Some("rs"))
                .map(|(i, _)| i)
                .collect()
        };
        let last_warmup_progress = Instant::now();

        Ok((
            LspWorker {
                child,
                stdin,
                reader,
                input_receiver,
                app_tx,
                files,
                req_state: TokenRequestState {
                    next_id: 1,
                    pending: HashMap::new(),
                    opened: HashSet::new(),
                },
                warmup_queue,
                last_warmup_progress,
                warmup_retries: HashMap::new(),
                supports_range,
                dispatch_count: 0,
                ref_queries: HashMap::new(),
                deferred_hovers: VecDeque::new(),
                hover_retries: HashMap::new(),
            },
            LspInterrupt,
        ))
    }

    fn restart_policy() -> RestartPolicy {
        RestartPolicy::default()
    }

    fn block_until_ready_then_dispatch(
        &mut self,
        _sender: &tokio::sync::broadcast::Sender<RRTEvent<Self::Output>>,
    ) -> Continuation {
        self.dispatch_count += 1;
        tracing::trace!("LSP: dispatch enter #{}", self.dispatch_count);
        // Drain any user-requested file indices first (non-blocking).
        loop {
            match self.input_receiver.try_recv() {
                Ok(input) => match input {
                    LspInput::TokenRequest(file_idx) => {
                        let snapshot = self.files.load();
                        let file = &snapshot[file_idx];
                        tracing::debug!(
                            "LSP: user request file_idx={} path={} ext={:?}",
                            file_idx,
                            file.path,
                            file.path.extension()
                        );
                        if file.removed.load(Ordering::Relaxed) {
                            if close_file(&mut self.stdin, file, file_idx, &mut self.req_state)
                                .is_err()
                            {
                                tracing::info!(
                                    "LSP: dispatch -> Restart (close_file failed for file_idx={})",
                                    file_idx
                                );
                                return Continuation::Restart;
                            }
                        } else if file.path.extension() == Some("rs")
                            && request_tokens(
                                &mut self.stdin,
                                file,
                                file_idx,
                                self.supports_range,
                                &mut self.req_state,
                                false,
                            )
                            .is_err()
                        {
                            tracing::info!(
                                "LSP: dispatch -> Restart (request_tokens failed for file_idx={})",
                                file_idx
                            );
                            return Continuation::Restart;
                        }
                    }
                    LspInput::HoverThenReferences {
                        file_idx,
                        line,
                        character,
                        word,
                        group_id,
                    } => {
                        // User double-clicks (group_id: None) are sent
                        // immediately, even mid-warmup: warmup yields to
                        // in-flight user requests (see the `user_busy` gate on
                        // the warmup send below), so there is no didOpen flood
                        // to stall the pipe against. Programmatic hovers
                        // (group_id: Some, from session-restore/rebuild) stay
                        // deferred behind warmup — they are not latency-critical
                        // and rely on rust-analyzer having settled first; they
                        // retry via `hover_retries`.
                        if group_id.is_none() || self.warmup_queue.is_empty() {
                            if self
                                .send_hover_request(file_idx, line, character, word, group_id)
                                .is_err()
                            {
                                tracing::info!("LSP: dispatch -> Restart (hover send failed)");
                                return Continuation::Restart;
                            }
                        } else {
                            tracing::debug!(
                                "LSP: deferring hover for file_idx={} until warmup done",
                                file_idx
                            );
                            self.deferred_hovers
                                .retain(|d| !(d.0 == file_idx && d.1 == line && d.2 == character));
                            self.deferred_hovers
                                .push_back((file_idx, line, character, word, group_id));
                        }
                    }
                },
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(n)) => {
                    tracing::warn!("LSP: input_receiver lagged by {} messages", n);
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    tracing::info!("LSP: dispatch -> Stop (input_receiver closed)");
                    return Continuation::Stop;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            }
        }

        // Force-complete warmup after 60s *without progress* to prevent a stuck
        // queue if the server fails to respond to a warmup request. Measured
        // from the last warmup send (not startup) because warmup yields to user
        // requests and may legitimately pause for long stretches under activity.
        if !self.warmup_queue.is_empty()
            && self.last_warmup_progress.elapsed() > std::time::Duration::from_secs(60)
        {
            tracing::warn!("warmup stalled (no progress for 60s), forcing completion");
            self.warmup_queue.clear();
            self.warmup_retries.clear();
        }

        // Warmup yields to in-flight user work: while any user-initiated token
        // request, hover, or reference query is awaiting a response, don't feed
        // rust-analyzer more warmup files — let it answer the user first. User
        // token requests carry is_warmup=false in req_state.pending; any
        // ref_queries entry is a hover/reference round-trip in progress.
        let user_busy = self
            .req_state
            .pending
            .values()
            .any(|(_, _, is_warmup)| !*is_warmup)
            || !self.ref_queries.is_empty();

        // Send one warmup file if the queue is non-empty and no user request is
        // outstanding. Before sending, drain one pending response if available
        // to prevent pipe deadlock (filling stdout pipe → ra blocks → stdin
        // fills → we block).
        if !self.warmup_queue.is_empty() && !user_busy {
            if !self.reader.buffer().is_empty()
                || poll_readable(self.reader.get_ref(), std::time::Duration::from_millis(10))
            {
                let msg = match recv_msg(&mut self.reader) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::info!(
                            "LSP: dispatch -> Restart (warmup drain recv_msg error: {:?})",
                            e.kind(),
                        );
                        return Continuation::Restart;
                    }
                };
                return self.handle_recv_msg(msg);
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
            if request_tokens(
                &mut self.stdin,
                file,
                file_idx,
                self.supports_range,
                &mut self.req_state,
                is_warmup,
            )
            .is_err()
            {
                let _ = self.child.kill();
                tracing::info!(
                    "LSP: dispatch -> Restart (warmup request_tokens failed for file_idx={})",
                    file_idx
                );
                return Continuation::Restart;
            }
            self.last_warmup_progress = Instant::now();
            tracing::debug!(
                "LSP: warmup send OK file_idx={} queue_remaining={}",
                file_idx,
                self.warmup_queue.len()
            );
        }

        // Replay hovers that were deferred while warmup was in progress.
        if self.warmup_queue.is_empty()
            && let Some((file_idx, line, character, word, group_id)) =
                self.deferred_hovers.pop_front()
        {
            if self
                .send_hover_request(file_idx, line, character, word, group_id)
                .is_err()
            {
                tracing::info!("LSP: dispatch -> Restart (deferred hover send failed)");
                return Continuation::Restart;
            }
        }

        // Poll stdout for up to READ_TIMEOUT before attempting a blocking read.
        let poll_ret = poll_readable(self.reader.get_ref(), READ_TIMEOUT);
        tracing::trace!(
            "LSP: poll ret={} pending={} warmup_queue={}",
            poll_ret,
            self.req_state.pending.len(),
            self.warmup_queue.len(),
        );
        if !poll_ret {
            return Continuation::Continue;
        }

        // Block on stdout for up to READ_TIMEOUT, then return.
        let msg = match recv_msg(&mut self.reader) {
            Ok(m) => m,
            Err(e) => {
                tracing::info!(
                    "LSP: dispatch -> Restart (recv_msg error: {:?}, pending={})",
                    e.kind(),
                    self.req_state.pending.len()
                );
                return Continuation::Restart;
            }
        };

        self.handle_recv_msg(msg)
    }
}

impl LspWorker {
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
        let uri: Uri = format!("file://{}", file.path).parse().expect("valid URI");
        let id = self.req_state.next_id;
        self.req_state.next_id += 1;
        let hover_req = RpcRequest {
            jsonrpc: "2.0",
            id,
            method: "textDocument/hover",
            params: HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri },
                    position: Position { line, character },
                },
                work_done_progress_params: WorkDoneProgressParams::default(),
            },
        };
        tracing::debug!("LSP: sending hover id={} file_idx={}", id, file_idx);
        send_msg(&mut self.stdin, &hover_req)?;
        self.ref_queries.insert(
            id,
            PendingRefQuery {
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

    fn handle_recv_msg(&mut self, msg: RpcResponse) -> Continuation {
        let method = msg.method.as_deref().unwrap_or("");
        let has_id = msg.id.is_some();
        tracing::debug!(
            "recv: method={:?} has_id={} warmup_queue={}",
            method,
            has_id,
            self.warmup_queue.len()
        );

        // Reply to server-initiated requests (e.g. window/workDoneProgress/create).
        // Per JSON-RPC 2.0 every request (a message with an id) gets a response,
        // even if the server just wants a null acknowledgement.
        if msg.method.is_some()
            && let Some(ref id) = msg.id
        {
            match method {
                "window/workDoneProgress/create" => {
                    tracing::debug!("replying to server request: method={:?} id={}", method, id);
                    let reply = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": null});
                    if send_msg(&mut self.stdin, &reply).is_err() {
                        tracing::info!(
                            "LSP: handle_recv_msg -> Restart (reply to server request failed)"
                        );
                        return Continuation::Restart;
                    }
                }
                // rust-analyzer asks us to re-pull diagnostics after workspace changes.
                // Explorer doesn't render diagnostics, so we ack with null and move on
                // instead of letting it fall through to the catch-all — that would
                // spam WARN logs and may cause the server to retry.
                "workspace/diagnostic/refresh" => {
                    tracing::debug!("replying to server request: method={:?} id={}", method, id);
                    let reply = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": null});
                    if send_msg(&mut self.stdin, &reply).is_err() {
                        tracing::info!(
                            "LSP: handle_recv_msg -> Restart (reply to server request failed)"
                        );
                        return Continuation::Restart;
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
            let tokens: Option<SemanticTokens> = msg
                .result
                .as_ref()
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            let has_data = tokens.is_some();
            tracing::debug!(
                "LSP: token response id={} file_idx={} is_range={} is_warmup={} has_data={}",
                id,
                file_idx,
                is_range,
                is_warmup,
                has_data,
            );

            if let Some(SemanticTokens { data, .. }) = tokens {
                let snapshot = self.files.load();
                let file = &snapshot[file_idx];
                let lines = {
                    let d = file
                        .data
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner());
                    let mut lines = decode_tokens(&data, &d.content);
                    if is_range {
                        lines.truncate(RANGE_LINES);
                    }
                    lines
                };
                let mut guard = file
                    .colored_lines
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                let should_write = if is_range {
                    guard.is_empty()
                } else {
                    guard.len() != lines.len()
                };
                if should_write {
                    let line_count = lines.len();
                    *guard = lines;
                    drop(guard);
                    notify = true;
                    tracing::debug!(
                        "LSP: wrote colored_lines for file_idx={} ({} lines)",
                        file_idx,
                        line_count
                    );
                } else {
                    tracing::debug!(
                        "LSP: skip write for file_idx={} (should_write=false)",
                        file_idx
                    );
                }
            } else if is_warmup {
                let retries = self.warmup_retries.entry(file_idx).or_insert(0);
                *retries += 1;
                tracing::debug!("warmup null: file_idx={} retry={}", file_idx, retries);
                if *retries < 3 {
                    self.warmup_queue.push_back(file_idx);
                } else {
                    tracing::debug!("warmup give up: file_idx={}", file_idx);
                }
            }
        }

        if let Some(id) = msg.id.as_ref().and_then(|v| v.as_u64())
            && let Some(pending) = self.ref_queries.remove(&id)
        {
            match pending.phase {
                RefQueryPhase::Hover => {
                    let hover_raw = msg
                        .result
                        .as_ref()
                        .and_then(|v| serde_json::from_value::<lsp_types::Hover>(v.clone()).ok());
                    let name: Option<String> =
                        hover_raw.as_ref().and_then(|hover| match &hover.contents {
                            HoverContents::Markup(content) => {
                                let name = match content.kind {
                                    lsp_types::MarkupKind::Markdown => {
                                        let mut parts = content.value.split('`');
                                        parts.nth(1)?.to_string()
                                    }
                                    lsp_types::MarkupKind::PlainText => {
                                        let parent = content.value.lines().next()?;
                                        // If hover returns just the parent type (e.g. for a
                                        // struct field), append the clicked word to form a
                                        // unique qualified name for the field.
                                        if let Some(w) = &pending.word {
                                            let last_seg = parent.rsplit("::").next().unwrap_or("");
                                            if last_seg != w.as_str()
                                                && !parent.contains(&format!("::{}", w))
                                            {
                                                format!("{}::{}", parent, w)
                                            } else {
                                                parent.to_string()
                                            }
                                        } else {
                                            parent.to_string()
                                        }
                                    }
                                };
                                if name.contains("::") {
                                    Some(name)
                                } else {
                                    tracing::debug!(
                                        "LSP: hover name doesn't contain '::': {:?}",
                                        name,
                                    );
                                    None
                                }
                            }
                            other => {
                                tracing::debug!(
                                    "LSP: hover contents not Markup: {:?}",
                                    std::mem::discriminant(other),
                                );
                                None
                            }
                        });
                    if name.is_none() {
                        tracing::debug!(
                            "LSP: hover name extraction failed for id={}. hover_raw={:?}",
                            id,
                            hover_raw.as_ref().map(|h| &h.contents),
                        );
                    }
                    let Some(name) = name else {
                        // A null result (hover_raw = None) commonly just means
                        // rust-analyzer hasn't finished indexing yet, not that the
                        // symbol is gone — most visible right at startup, when
                        // session-restore fires rebuild hovers the instant our own
                        // (much smaller) local warmup queue drains. Retry those;
                        // a user-initiated double-click (group_id: None) or a
                        // hover that came back with real-but-unusable contents
                        // (e.g. no `::` — a keyword or builtin) fails immediately.
                        let retry_key = (pending.file_idx, pending.line, pending.character);
                        if hover_raw.is_none() && pending.group_id.is_some() {
                            let retries = self.hover_retries.entry(retry_key).or_insert(0);
                            if *retries < MAX_HOVER_RETRIES {
                                *retries += 1;
                                tracing::debug!(
                                    "LSP: hover null, retry {}/{} for file_idx={} line={} char={}",
                                    retries,
                                    MAX_HOVER_RETRIES,
                                    pending.file_idx,
                                    pending.line,
                                    pending.character,
                                );
                                // rust-analyzer answers a "not ready" hover with a null
                                // result essentially instantly (it's a fast rejection,
                                // not a slow one) — resending immediately just burns
                                // through all retries in the same millisecond, giving
                                // indexing no time to actually progress. Block this
                                // dedicated worker thread briefly instead, matching
                                // the blocking-I/O design already used elsewhere here
                                // (e.g. the 500ms body-read deadline in recv_msg).
                                std::thread::sleep(HOVER_RETRY_DELAY);
                                self.deferred_hovers.push_back((
                                    pending.file_idx,
                                    pending.line,
                                    pending.character,
                                    pending.word,
                                    pending.group_id,
                                ));
                                return Continuation::Continue;
                            }
                            tracing::debug!(
                                "LSP: hover retries exhausted for file_idx={} line={} char={}",
                                pending.file_idx,
                                pending.line,
                                pending.character,
                            );
                        }
                        self.hover_retries.remove(&retry_key);
                        // blocking_send: this runs on the RRT std::thread, and a
                        // dropped result would strand the group in limbo forever.
                        let _ = self.app_tx.blocking_send(
                            r3bl_tui::TerminalWindowMainThreadSignal::ApplyAppSignal(
                                AppSignal::SymbolHighlightResult {
                                    qualified_name: String::new(),
                                    group_id: pending.group_id,
                                    origin_file_idx: pending.file_idx,
                                    origin_line: pending.line,
                                    origin_char: pending.character,
                                    origin_word: pending.word,
                                    origin_locations: Vec::new(),
                                    reference_locations: Vec::new(),
                                },
                            ),
                        );
                        return Continuation::Continue;
                    };
                    self.hover_retries
                        .remove(&(pending.file_idx, pending.line, pending.character));
                    let snapshot = self.files.load();
                    let file = &snapshot[pending.file_idx];
                    let uri: Uri = format!("file://{}", file.path).parse().expect("valid URI");
                    let refs_id = self.req_state.next_id;
                    self.req_state.next_id += 1;
                    let refs_req = RpcRequest {
                        jsonrpc: "2.0",
                        id: refs_id,
                        method: "textDocument/references",
                        params: ReferenceParams {
                            text_document_position: TextDocumentPositionParams {
                                text_document: TextDocumentIdentifier { uri },
                                position: Position {
                                    line: pending.line,
                                    character: pending.character,
                                },
                            },
                            context: ReferenceContext {
                                include_declaration: true,
                            },
                            work_done_progress_params: WorkDoneProgressParams::default(),
                            partial_result_params: PartialResultParams::default(),
                        },
                    };
                    tracing::debug!(
                        "LSP: hover succeeded name={:?}, sending references id={}",
                        name,
                        refs_id,
                    );
                    if send_msg(&mut self.stdin, &refs_req).is_err() {
                        tracing::info!("LSP: refs send failed");
                        return Continuation::Restart;
                    }
                    self.ref_queries.insert(
                        refs_id,
                        PendingRefQuery {
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
                }
                RefQueryPhase::References => {
                    let Some(name) = pending.qualified_name else {
                        return Continuation::Continue;
                    };
                    let locations: Vec<Location> = msg
                        .result
                        .and_then(|v| serde_json::from_value(v).ok())
                        .unwrap_or_default();
                    let snapshot = self.files.load();
                    let origin_path: String = snapshot[pending.file_idx].path.as_str().to_string();
                    let mut origin_locations: Vec<SymbolRefLocation> = Vec::new();
                    let mut reference_locations: Vec<SymbolRefLocation> = Vec::new();
                    for loc in locations {
                        let Some(path_str) = loc.uri.as_str().strip_prefix("file://") else {
                            continue;
                        };
                        let path = camino::Utf8PathBuf::from(path_str);
                        let Some(file_key) = crate::loader::file_key_for_path(&snapshot, &path)
                        else {
                            continue;
                        };
                        let file = &snapshot[file_key.0];
                        let Ok(guard) = file.data.lock() else {
                            continue;
                        };
                        let line_starts = &guard.line_starts;
                        let content = &guard.content;
                        let line = loc.range.start.line as usize;
                        if line >= line_starts.len() {
                            continue;
                        }
                        let line_start = line_starts[line];
                        let line_str = &content[line_start..];
                        let start_byte = line_start
                            + utf16_to_byte(line_str, loc.range.start.character as usize);
                        let end_byte =
                            line_start + utf16_to_byte(line_str, loc.range.end.character as usize);
                        drop(guard);
                        let sl = SymbolRefLocation {
                            file_key,
                            start_byte,
                            end_byte,
                        };
                        if loc.uri.as_str().strip_prefix("file://") == Some(&origin_path) {
                            origin_locations.push(sl);
                        } else {
                            reference_locations.push(sl);
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
                            AppSignal::SymbolHighlightResult {
                                qualified_name: name,
                                group_id: pending.group_id,
                                origin_file_idx: pending.file_idx,
                                origin_line: pending.line,
                                origin_char: pending.character,
                                origin_word: pending.word,
                                origin_locations,
                                reference_locations,
                            },
                        ),
                    );
                }
            }
            return Continuation::Continue;
        }

        if notify {
            let _ = self
                .app_tx
                .try_send(TerminalWindowMainThreadSignal::ApplyAppSignal(
                    AppSignal::Noop,
                ));
        }

        Continuation::Continue
    }
}

// ── Send file requests via the RRT input channel ─────────────────────────────

/// Buffer for file indices requested before the LSP worker is in Running state.
static FILE_REQUEST_BUFFER: std::sync::Mutex<Vec<LspInput>> = std::sync::Mutex::new(Vec::new());

fn drain_request_buffer(tx: &tokio::sync::broadcast::Sender<LspInput>) {
    if let Ok(mut buf) = FILE_REQUEST_BUFFER.lock() {
        buf.retain(|item| tx.send(item.clone()).is_err());
    }
}

fn buffer_file_request(input: LspInput) {
    const MAX: usize = 10_000;
    let _ = FILE_REQUEST_BUFFER.lock().map(|mut buf| {
        if buf.len() >= MAX {
            tracing::warn!(
                "LSP: request buffer full ({} entries), dropping request",
                MAX
            );
        } else {
            buf.push(input);
        }
    });
}

fn send_lsp_input(input: LspInput) {
    let generation = LSP_RRT.get_thread_generation();
    let output_receivers = LSP_RRT.get_receiver_count();
    let guard = LSP_RRT.shared_state.lock();
    match &*guard {
        ThreadState::Running(_, tx) => {
            let mut buf = FILE_REQUEST_BUFFER
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for item in buf.drain(..) {
                let _ = tx.send(item);
            }
            drop(buf);
            let input_receivers = tx.receiver_count();
            let input_for_buffer = input.clone();
            match tx.send(input) {
                Ok(0) => {
                    tracing::warn!(
                        "LSP: send to 0 receivers (input={} output={} gen={})",
                        input_receivers,
                        output_receivers,
                        generation
                    );
                    buffer_file_request(input_for_buffer);
                }
                Ok(n) => {
                    tracing::debug!("LSP: sent to {} receivers (gen={})", n, generation);
                }
                Err(e) => {
                    tracing::error!("LSP: send error: {:?} gen={}", e, generation);
                    buffer_file_request(input_for_buffer);
                }
            }
        }
        state => {
            tracing::warn!(
                "LSP: send -> Buffered state={:?} gen={} output_receivers={}",
                state,
                generation,
                output_receivers
            );
            buffer_file_request(input);
        }
    }
}

pub fn send_file_request(file_idx: usize) {
    send_lsp_input(LspInput::TokenRequest(file_idx));
}

pub fn send_symbol_request(
    file_idx: usize,
    line: u32,
    character: u32,
    word: Option<String>,
    group_id: Option<usize>,
) {
    tracing::debug!(
        "LSP: send_symbol_request file_idx={} line={} char={} word={:?} group_id={:?}",
        file_idx,
        line,
        character,
        word,
        group_id,
    );
    send_lsp_input(LspInput::HoverThenReferences {
        file_idx,
        line,
        character,
        word,
        group_id,
    });
}

/// Health of the LSP worker, returned by [`health_check`].
#[derive(Debug)]
pub enum LspHealth {
    Running {
        input_receivers: usize,
        generation: u8,
    },
    NotRunning,
}

/// Returns the current health of the LSP worker.
/// Call this periodically to detect silent worker exit.
pub fn health_check() -> LspHealth {
    let generation = LSP_RRT.get_thread_generation();
    let guard = LSP_RRT.shared_state.lock();
    match &*guard {
        ThreadState::Running(_, tx) => LspHealth::Running {
            input_receivers: tx.receiver_count(),
            generation,
        },
        _ => LspHealth::NotRunning,
    }
}

/// Drain the file request buffer if the worker is running.
/// Safe to call from the main thread on any tick.
pub fn try_drain_pending_requests() {
    let guard = LSP_RRT.shared_state.lock();
    if let ThreadState::Running(_, tx) = &*guard {
        drain_request_buffer(tx);
    }
}

// ── I/O helpers ───────────────────────────────────────────────────────────────

/// Returns true if the fd has data available within the timeout.
/// Retries on EINTR (signal interruption).
fn poll_readable(fd: &impl AsFd, timeout: std::time::Duration) -> bool {
    use rustix::io::Errno;
    let mut pollfds = [PollFd::new(fd, PollFlags::IN)];
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    loop {
        match rpoll(&mut pollfds, ms) {
            Ok(_) => {
                return pollfds[0]
                    .revents()
                    .intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR);
            }
            Err(Errno::INTR) => continue,
            Err(_) => return false,
        }
    }
}

fn send_msg<T: Serialize>(stdin: &mut ChildStdin, msg: &T) -> std::io::Result<()> {
    let body = serde_json::to_string(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    stdin.write_all(framed.as_bytes())?;
    stdin.flush()
}

fn recv_msg(reader: &mut BufReader<ChildStdout>) -> std::io::Result<RpcResponse> {
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(val) = trimmed
            .strip_prefix("Content-Length: ")
            .or_else(|| trimmed.strip_prefix("content-length: "))
        {
            content_length = val.trim().parse().unwrap_or(0);
        }
    }
    if content_length == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "zero content length",
        ));
    }
    // Read body in chunks. BufReader may already have the body buffered
    // (from the read_line calls above), so check buffer before polling fd.
    let mut buf = vec![0u8; content_length];
    let mut read = 0usize;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while read < content_length {
        if std::time::Instant::now() > deadline {
            tracing::info!(
                "LSP: recv_msg body read timeout after {} of {} bytes",
                read,
                content_length,
            );
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "body read total timeout",
            ));
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if reader.buffer().is_empty() && !poll_readable(reader.get_ref(), remaining) {
            tracing::info!(
                "LSP: recv_msg body chunk timeout after {} of {} bytes",
                read,
                content_length,
            );
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "body read chunk timeout",
            ));
        }
        let n = reader.read(&mut buf[read..])?;
        if n == 0 {
            tracing::info!(
                "LSP: recv_msg body EOF after {} of {} bytes",
                read,
                content_length,
            );
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "body EOF",
            ));
        }
        read += n;
    }
    serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

// ── Token helpers ─────────────────────────────────────────────────────────────

/// Send didClose to the LSP server for a file that was removed.
/// Safe to call even if the file was never opened with the server.
fn close_file(
    stdin: &mut ChildStdin,
    file: &LoadedFile,
    file_idx: usize,
    state: &mut TokenRequestState,
) -> std::io::Result<()> {
    if !state.opened.contains(&file_idx) {
        return Ok(());
    }
    let uri: Uri = format!("file://{}", file.path)
        .parse()
        .expect("valid file URI");
    tracing::debug!("LSP: didClose for file_idx={} path={}", file_idx, file.path);
    let close = RpcNotification {
        jsonrpc: "2.0",
        method: "textDocument/didClose",
        params: serde_json::json!({
            "textDocument": { "uri": uri }
        }),
    };
    send_msg(stdin, &close)?;
    state.opened.remove(&file_idx);
    Ok(())
}

fn request_tokens(
    stdin: &mut ChildStdin,
    file: &LoadedFile,
    file_idx: usize,
    supports_range: bool,
    state: &mut TokenRequestState,
    is_warmup: bool,
) -> std::io::Result<()> {
    let uri: Uri = format!("file://{}", file.path)
        .parse()
        .expect("valid file URI");

    let (total_lines, content) = {
        let guard = file
            .data
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        (guard.content.lines().count(), guard.content.clone())
    };
    let colored_len = file
        .colored_lines
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .len();
    if colored_len == total_lines {
        return Ok(());
    }

    if !is_warmup && state.opened.contains(&file_idx) && colored_len == 0 {
        tracing::debug!("LSP: didClose for file_idx={}", file_idx);
        let close = RpcNotification {
            jsonrpc: "2.0",
            method: "textDocument/didClose",
            params: serde_json::json!({
                "textDocument": { "uri": uri }
            }),
        };
        send_msg(stdin, &close)?;
        state.opened.remove(&file_idx);
    }

    if !state.opened.contains(&file_idx) {
        tracing::debug!(
            "LSP: didOpen for file_idx={} ({} bytes)",
            file_idx,
            content.len()
        );
        let did_open = RpcNotification {
            jsonrpc: "2.0",
            method: DidOpenTextDocument::METHOD,
            params: DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "rust".to_string(),
                    version: 1,
                    text: content,
                },
            },
        };
        send_msg(stdin, &did_open)?;
        state.opened.insert(file_idx);
    }

    if is_warmup && supports_range && total_lines > RANGE_THRESHOLD && colored_len == 0 {
        let end_line = RANGE_LINES.min(total_lines) as u32;
        let range_id = state.next_id;
        state.next_id += 1;
        tracing::debug!(
            "LSP: range request id={} file_idx={} lines=0..{}",
            range_id,
            file_idx,
            end_line
        );
        let range_req = RpcRequest {
            jsonrpc: "2.0",
            id: range_id,
            method: SemanticTokensRangeRequest::METHOD,
            params: SemanticTokensRangeParams {
                work_done_progress_params: WorkDoneProgressParams::default(),
                partial_result_params: PartialResultParams::default(),
                text_document: TextDocumentIdentifier { uri: uri.clone() },
                range: Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: end_line,
                        character: 0,
                    },
                },
            },
        };
        send_msg(stdin, &range_req)?;
        state.pending.insert(range_id, (file_idx, true, is_warmup));
    }

    if is_warmup && supports_range && total_lines > RANGE_THRESHOLD {
        return Ok(());
    }

    let full_id = state.next_id;
    state.next_id += 1;
    tracing::debug!("LSP: full request id={} file_idx={}", full_id, file_idx);
    let full_req = RpcRequest {
        jsonrpc: "2.0",
        id: full_id,
        method: SemanticTokensFullRequest::METHOD,
        params: SemanticTokensParams {
            work_done_progress_params: WorkDoneProgressParams::default(),
            partial_result_params: PartialResultParams::default(),
            text_document: TextDocumentIdentifier { uri },
        },
    };
    send_msg(stdin, &full_req)?;
    state.pending.insert(full_id, (file_idx, false, is_warmup));

    Ok(())
}

type LineTokens = Vec<(usize, usize, &'static str)>;

fn decode_tokens(data: &[SemanticToken], content: &str) -> Vec<ColoredLine> {
    let token_types = TOKEN_TYPES.get().map(Vec::as_slice).unwrap_or(&[]);
    let static_types = token_types;

    let lines: Vec<&str> = content.lines().collect();
    let mut line_tokens: Vec<LineTokens> = vec![Vec::new(); lines.len()];

    let mut abs_line = 0usize;
    let mut abs_char = 0usize;

    for token in data {
        let delta_line = token.delta_line as usize;
        let delta_start = token.delta_start as usize;
        let length = token.length as usize;
        let type_idx = token.token_type as usize;

        if delta_line > 0 {
            abs_line += delta_line;
            abs_char = delta_start;
        } else {
            abs_char += delta_start;
        }

        if abs_line >= lines.len() {
            continue;
        }

        let type_name = static_types.get(type_idx).copied().unwrap_or("");
        let line = lines[abs_line];
        let start = utf16_to_byte(line, abs_char);
        let end = utf16_to_byte(line, abs_char + length).min(line.len());
        if start < end {
            line_tokens[abs_line].push((start, end, type_name));
        }
    }

    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let tokens = &line_tokens[i];
            if tokens.is_empty() {
                return vec![(0, line.len(), "")];
            }
            let mut spans: ColoredLine = Vec::new();
            let mut pos = 0usize;
            for &(start, end, type_name) in tokens {
                if pos < start {
                    spans.push((pos, start, ""));
                }
                spans.push((start, end, type_name));
                pos = end;
            }
            if pos < line.len() {
                spans.push((pos, line.len(), ""));
            }
            spans
        })
        .collect()
}

fn utf16_to_byte(s: &str, utf16_offset: usize) -> usize {
    let mut u16_count = 0usize;
    for (byte_idx, ch) in s.char_indices() {
        if u16_count >= utf16_offset {
            return byte_idx;
        }
        u16_count += ch.len_utf16();
    }
    s.len()
}

pub(crate) fn byte_to_utf16(s: &str, byte_offset: usize) -> u32 {
    let byte_offset = byte_offset.min(s.len());
    let mut u16_count = 0u32;
    for (i, c) in s.char_indices() {
        if byte_offset < i + c.len_utf8() {
            return u16_count;
        }
        u16_count += c.len_utf16() as u32;
    }
    u16_count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_byte_to_utf16_ascii() {
        assert_eq!(byte_to_utf16("hello", 0), 0);
        assert_eq!(byte_to_utf16("hello", 3), 3);
        assert_eq!(byte_to_utf16("hello", 5), 5);
    }

    #[test]
    fn test_byte_to_utf16_multi_byte() {
        assert_eq!(byte_to_utf16("héllo", 0), 0);
        assert_eq!(byte_to_utf16("héllo", 2), 1);
        assert_eq!(byte_to_utf16("héllo", 3), 2);

        assert_eq!(byte_to_utf16("𐍈test", 0), 0);
        assert_eq!(byte_to_utf16("𐍈test", 4), 2);
        assert_eq!(byte_to_utf16("𐍈test", 5), 3);
    }

    #[test]
    fn test_byte_to_utf16_bounds() {
        assert_eq!(byte_to_utf16("hi", 100), 2);
    }
}
