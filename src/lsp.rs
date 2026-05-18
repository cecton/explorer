use crate::loader::LoadedFile;
use arc_swap::ArcSwap;
use camino::Utf8PathBuf;
use lsp_types::notification::{DidOpenTextDocument, Initialized, Notification};
use lsp_types::request::{
    Initialize, Request, SemanticTokensFullRequest, SemanticTokensRangeRequest,
};
use lsp_types::{
    ClientCapabilities, DidOpenTextDocumentParams, InitializeParams, InitializeResult,
    InitializedParams, PartialResultParams, Position, Range, SemanticToken, SemanticTokens,
    SemanticTokensClientCapabilities, SemanticTokensClientCapabilitiesRequests,
    SemanticTokensFullOptions, SemanticTokensParams, SemanticTokensRangeParams,
    TextDocumentClientCapabilities, TextDocumentIdentifier, TextDocumentItem, TokenFormat, Uri,
    WorkDoneProgressParams, WorkspaceFolder,
};
use r3bl_tui::{Continuation, RRT, RRTEvent, RRTSoftwareInterrupt, RRTWorker, RestartPolicy};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Debug;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::sync::broadcast::Sender;

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

// ── Event broadcast by LspWorker to async consumers ──────────────────────────

#[derive(Clone, Debug)]
pub enum LspEvent {
    /// Semantic tokens for one or more files were updated. Consumers should re-render.
    TokensUpdated,
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

struct LspConfig {
    root: Utf8PathBuf,
    files: Arc<ArcSwap<Vec<LoadedFile>>>,
}

static LSP_CONFIG: OnceLock<LspConfig> = OnceLock::new();

pub fn set_lsp_config(root: Utf8PathBuf, files: Arc<ArcSwap<Vec<LoadedFile>>>) {
    let _ = LSP_CONFIG.set(LspConfig { root, files });
}

// ── Worker ────────────────────────────────────────────────────────────────────

struct TokenRequestState {
    next_id: u64,
    pending: HashMap<u64, (usize, bool, bool)>,
    opened: HashSet<usize>,
}

pub struct LspWorker {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    stdout_fd: std::os::unix::io::RawFd,
    request_rx: mpsc::Receiver<usize>,
    files: Arc<ArcSwap<Vec<LoadedFile>>>,
    req_state: TokenRequestState,
    warmup_queue: VecDeque<usize>,
    warmup_remaining: usize,
    warmup_start: Instant,
    warmup_retries: HashMap<usize, u8>,
    supports_range: bool,
}

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
    type Event = LspEvent;
    type Interrupt = LspInterrupt;

    fn create_and_register_os_sources() -> miette::Result<(Self, Self::Interrupt)> {
        let config = LSP_CONFIG
            .get()
            .expect("set_lsp_config must be called before subscribing");

        let _ = REQUEST_SLOT.get_or_init(|| std::sync::Mutex::new(None));
        let (tx, request_rx) = mpsc::sync_channel(64);
        *REQUEST_SLOT.get().unwrap().lock().unwrap() = Some(tx);

        let mut child = Command::new("rust-analyzer")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| miette::miette!("failed to spawn rust-analyzer: {e}"))?;

        let mut stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

        use std::os::unix::io::AsRawFd;
        let stdout_fd = stdout.as_raw_fd();
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
        let warmup_remaining = warmup_queue.len();
        let warmup_start = Instant::now();

        Ok((
            LspWorker {
                child,
                stdin,
                reader,
                stdout_fd,
                request_rx,
                files,
                req_state: TokenRequestState {
                    next_id: 1,
                    pending: HashMap::new(),
                    opened: HashSet::new(),
                },
                warmup_queue,
                warmup_remaining,
                warmup_start,
                warmup_retries: HashMap::new(),
                supports_range,
            },
            LspInterrupt,
        ))
    }

    fn restart_policy() -> RestartPolicy {
        RestartPolicy::default()
    }

    fn block_until_ready_then_dispatch(
        &mut self,
        sender: &Sender<RRTEvent<Self::Event>>,
    ) -> Continuation {
        // Drain any user-requested file indices first (non-blocking).
        while let Ok(file_idx) = self.request_rx.try_recv() {
            tracing::debug!("user request: file_idx={}", file_idx);
            let snapshot = self.files.load();
            let file = &snapshot[file_idx];
            if file.path.extension() == Some("rs")
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
                return Continuation::Restart;
            }
        }

        // Send one warmup file if the queue is non-empty.
        if let Some(file_idx) = self.warmup_queue.pop_front() {
            let snapshot = self.files.load();
            let file = &snapshot[file_idx];
            tracing::debug!(
                "warmup send: file_idx={} path={} queue_remaining={}",
                file_idx,
                file.path,
                self.warmup_queue.len()
            );
            if request_tokens(
                &mut self.stdin,
                file,
                file_idx,
                self.supports_range,
                &mut self.req_state,
                true,
            )
            .is_err()
            {
                let _ = self.child.kill();
                return Continuation::Restart;
            }
        }

        // Poll stdout for up to READ_TIMEOUT before attempting a blocking read.
        if !poll_readable(self.stdout_fd, READ_TIMEOUT) {
            return Continuation::Continue;
        }

        // Block on stdout for up to READ_TIMEOUT, then return.
        let msg = match recv_msg(&mut self.reader) {
            Ok(m) => m,
            Err(_) => {
                return Continuation::Restart;
            }
        };

        let method = msg.method.as_deref().unwrap_or("");
        let has_id = msg.id.is_some();
        tracing::debug!(
            "recv: method={:?} has_id={} warmup_remaining={}",
            method,
            has_id,
            self.warmup_remaining
        );

        // Reply to server-initiated requests (e.g. window/workDoneProgress/create).
        if msg.method.is_some()
            && let Some(ref id) = msg.id
        {
            tracing::debug!("replying to server request: method={:?} id={}", method, id);
            let reply = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": null});
            if send_msg(&mut self.stdin, &reply).is_err() {
                return Continuation::Restart;
            }
        }

        let mut notify = false;

        if let Some(id) = msg.id.as_ref().and_then(|v| v.as_u64())
            && let Some((file_idx, is_range, is_warmup)) = self.req_state.pending.remove(&id)
        {
            let tokens: Option<SemanticTokens> =
                msg.result.and_then(|v| serde_json::from_value(v).ok());
            let has_data = tokens.is_some();
            tracing::debug!(
                "token response: id={} file_idx={} is_range={} is_warmup={} has_data={} warmup_remaining={}",
                id,
                file_idx,
                is_range,
                is_warmup,
                has_data,
                self.warmup_remaining
            );

            if let Some(SemanticTokens { data, .. }) = tokens {
                let snapshot = self.files.load();
                let file = &snapshot[file_idx];
                let lines = {
                    let d = file.data.lock().unwrap();
                    decode_tokens(&data, &d.content)
                };
                let mut guard = file.colored_lines.lock().unwrap();
                if !is_range || guard.is_empty() {
                    *guard = lines;
                    drop(guard);
                    notify = true;
                }
                if is_warmup && self.warmup_remaining > 0 {
                    self.warmup_remaining -= 1;
                    if self.warmup_remaining == 0 {
                        let elapsed = self.warmup_start.elapsed().as_millis();
                        notify = true;
                        tracing::info!("warmup complete: elapsed={}ms", elapsed);
                    }
                }
            } else if is_warmup {
                let retries = self.warmup_retries.entry(file_idx).or_insert(0);
                *retries += 1;
                tracing::debug!("warmup null: file_idx={} retry={}", file_idx, retries);
                if *retries < 3 {
                    self.warmup_queue.push_back(file_idx);
                } else {
                    tracing::debug!("warmup give up: file_idx={}", file_idx);
                    if self.warmup_remaining > 0 {
                        self.warmup_remaining -= 1;
                        if self.warmup_remaining == 0 {
                            let elapsed = self.warmup_start.elapsed().as_millis();
                            notify = true;
                            tracing::info!(
                                "warmup complete (with gave-up files): elapsed={}ms",
                                elapsed
                            );
                        }
                    }
                }
            }
        }

        if notify
            && sender
                .send(RRTEvent::Worker(LspEvent::TokensUpdated))
                .is_err()
        {
            return Continuation::Stop;
        }

        Continuation::Continue
    }
}

// ── Shared sender slot (survives restarts) ────────────────────────────────────

static REQUEST_SLOT: OnceLock<std::sync::Mutex<Option<mpsc::SyncSender<usize>>>> = OnceLock::new();

pub fn send_file_request(file_idx: usize) {
    if let Some(slot) = REQUEST_SLOT.get()
        && let Ok(guard) = slot.lock()
        && let Some(ref tx) = *guard
    {
        let _ = tx.try_send(file_idx);
    }
}

// ── I/O helpers ───────────────────────────────────────────────────────────────

/// Returns true if the fd has data available within the timeout.
fn poll_readable(fd: std::os::unix::io::RawFd, timeout: std::time::Duration) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    let ret = unsafe { libc::poll(&mut pfd, 1, ms) };
    ret > 0 && (pfd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)) != 0
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
        if let Some(val) = trimmed.strip_prefix("Content-Length: ") {
            content_length = val.trim().parse().unwrap_or(0);
        }
    }
    let mut buf = vec![0u8; content_length];
    reader.read_exact(&mut buf)?;
    serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

// ── Token helpers ─────────────────────────────────────────────────────────────

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

    if !state.opened.contains(&file_idx) {
        let content = file.data.lock().unwrap().content.clone();
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

    let total_lines = file.data.lock().unwrap().line_starts.len();
    if supports_range && total_lines > RANGE_THRESHOLD {
        let end_line = RANGE_LINES.min(total_lines) as u32;
        let range_id = state.next_id;
        state.next_id += 1;
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

    if is_warmup && supports_range && file.data.lock().unwrap().line_starts.len() > RANGE_THRESHOLD
    {
        return Ok(());
    }

    let full_id = state.next_id;
    state.next_id += 1;
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
