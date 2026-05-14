use crate::LoadedFile;
use crate::tui::state::AppSignal;
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
use r3bl_tui::TerminalWindowMainThreadSignal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;

pub type ColoredSpan = (usize, usize, &'static str);
pub type ColoredLine = Vec<ColoredSpan>;

static TOKEN_TYPES: OnceLock<Vec<&'static str>> = OnceLock::new();

const RANGE_LINES: usize = 200;
const RANGE_THRESHOLD: usize = 100;

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

pub async fn run(
    root: Utf8PathBuf,
    files: Arc<Vec<LoadedFile>>,
    mut requests: mpsc::Receiver<usize>,
    notify_tx: mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>,
) {
    let Ok(mut child) = Command::new("rust-analyzer")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };

    let mut stdin = child.stdin.take().expect("stdin piped");
    let stdout = child.stdout.take().expect("stdout piped");
    let mut reader = BufReader::new(stdout);

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

    if send_msg(&mut stdin, &init_req).await.is_err() {
        let _ = child.kill().await;
        return;
    }

    let supports_range = loop {
        let Ok(msg) = recv_msg(&mut reader).await else {
            let _ = child.kill().await;
            return;
        };
        if msg.id.as_ref().and_then(|v| v.as_u64()) == Some(0) {
            let result: InitializeResult =
                match msg.result.and_then(|v| serde_json::from_value(v).ok()) {
                    Some(r) => r,
                    None => {
                        let _ = child.kill().await;
                        return;
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
                .map(|p| {
                    match p {
                    lsp_types::SemanticTokensServerCapabilities::SemanticTokensOptions(
                        opts,
                    ) => opts.range.unwrap_or(false),
                    lsp_types::SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(
                        opts,
                    ) => opts.semantic_tokens_options.range.unwrap_or(false),
                }
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
    if send_msg(&mut stdin, &notify_init).await.is_err() {
        let _ = child.kill().await;
        return;
    }

    let mut req_state = TokenRequestState {
        next_id: 1,
        pending: HashMap::new(),
        opened: HashSet::new(),
    };

    // Queue of file indices still needing warmup sends; drained inside the select loop.
    let mut warmup_queue: VecDeque<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, f)| f.path.extension() == Some("rs"))
        .map(|(i, _)| i)
        .collect();
    let mut warmup_remaining = warmup_queue.len();
    let warmup_start = Instant::now();
    // Retry counts for warmup files that returned null; give up after 3 attempts.
    let mut warmup_retries: HashMap<usize, u8> = HashMap::new();

    let mut notify_pending = false;

    loop {
        tokio::select! {
            // Drain stdout first to prevent pipe deadlock.
            biased;

            result = recv_msg(&mut reader) => {
                let Ok(msg) = result else { break };
                let method = msg.method.as_deref().unwrap_or("");
                let has_id = msg.id.is_some();
                tracing::debug!("recv: method={:?} has_id={} warmup_remaining={} notify_pending={}",
                    method, has_id, warmup_remaining, notify_pending);

                // Reply to any server-initiated request (e.g. window/workDoneProgress/create).
                if msg.method.is_some()
                    && let Some(ref id) = msg.id
                {
                    tracing::debug!("replying to server request: method={:?} id={}", method, id);
                    let reply = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": null});
                    if send_msg(&mut stdin, &reply).await.is_err() {
                        break;
                    }
                }

                if let Some(id) = msg.id.as_ref().and_then(|v| v.as_u64())
                    && let Some((file_idx, is_range, is_warmup)) = req_state.pending.remove(&id)
                {
                    let tokens: Option<SemanticTokens> = msg
                        .result
                        .and_then(|v| serde_json::from_value(v).ok());
                    let has_data = tokens.is_some();
                    tracing::debug!("token response: id={} file_idx={} is_range={} is_warmup={} has_data={} warmup_remaining={}",
                        id, file_idx, is_range, is_warmup, has_data, warmup_remaining);

                    if let Some(SemanticTokens { data, .. }) = tokens {
                        let lines = decode_tokens(&data, &files[file_idx].content);
                        let mut guard = files[file_idx].colored_lines.lock().unwrap();
                        if !is_range || guard.is_empty() {
                            *guard = lines;
                            drop(guard);
                            notify_pending = true;
                        }
                        if is_warmup && warmup_remaining > 0 {
                            warmup_remaining -= 1;
                            if warmup_remaining == 0 {
                                let elapsed = warmup_start.elapsed().as_millis();
                                notify_pending = true;
                                tracing::info!("warmup complete: elapsed={}ms", elapsed);
                            }
                        }
                    } else if is_warmup {
                        // Null response: rust-analyzer not ready yet. Retry up to 3 times.
                        let retries = warmup_retries.entry(file_idx).or_insert(0);
                        *retries += 1;
                        tracing::debug!("warmup null: file_idx={} retry={}", file_idx, retries);
                        if *retries < 3 {
                            warmup_queue.push_back(file_idx);
                        } else {
                            // Give up on this file.
                            tracing::debug!("warmup give up: file_idx={}", file_idx);
                            if warmup_remaining > 0 {
                                warmup_remaining -= 1;
                                if warmup_remaining == 0 {
                                    let elapsed = warmup_start.elapsed().as_millis();
                                    notify_pending = true;
                                    tracing::info!("warmup complete (with gave-up files): elapsed={}ms", elapsed);
                                }
                            }
                        }
                    }
                }

                if notify_pending {
                    let _ = notify_tx.try_send(
                        TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::Noop),
                    );
                    notify_pending = false;
                }
            }

            file_idx = requests.recv() => {
                let Some(file_idx) = file_idx else { break };
                tracing::debug!("user request: file_idx={}", file_idx);
                let file = &files[file_idx];
                if file.path.extension() != Some("rs") {
                    continue;
                }
                if request_tokens(
                    &mut stdin,
                    file,
                    file_idx,
                    supports_range,
                    &mut req_state,
                    false,
                )
                .await
                .is_err()
                {
                    break;
                }
            }

            // Send the next warmup file; only polled when ready and nothing to read.
            Some(file_idx) = async { warmup_queue.pop_front() },
                if !warmup_queue.is_empty() =>
            {
                let file = &files[file_idx];
                tracing::debug!("warmup send: file_idx={} path={} queue_remaining={}", file_idx, file.path, warmup_queue.len());
                if request_tokens(
                    &mut stdin,
                    file,
                    file_idx,
                    supports_range,
                    &mut req_state,
                    true,
                )
                .await
                .is_err()
                {
                    break;
                }
            }
        }
    }

    let _ = child.kill().await;
}

struct TokenRequestState {
    next_id: u64,
    pending: HashMap<u64, (usize, bool, bool)>,
    opened: HashSet<usize>,
}

async fn request_tokens(
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
        let did_open = RpcNotification {
            jsonrpc: "2.0",
            method: DidOpenTextDocument::METHOD,
            params: DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "rust".to_string(),
                    version: 1,
                    text: file.content.clone(),
                },
            },
        };
        send_msg(stdin, &did_open).await?;
        state.opened.insert(file_idx);
    }

    let total_lines = file.line_starts.len();
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
        send_msg(stdin, &range_req).await?;
        state.pending.insert(range_id, (file_idx, true, is_warmup));
    }

    // For warmup, skip full request when a range request was already sent.
    if is_warmup && supports_range && file.line_starts.len() > RANGE_THRESHOLD {
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
    send_msg(stdin, &full_req).await?;
    state.pending.insert(full_id, (file_idx, false, is_warmup));

    Ok(())
}

async fn send_msg<T: Serialize>(stdin: &mut ChildStdin, msg: &T) -> std::io::Result<()> {
    let body = serde_json::to_string(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    stdin.write_all(framed.as_bytes()).await?;
    stdin.flush().await
}

async fn recv_msg(reader: &mut BufReader<ChildStdout>) -> std::io::Result<RpcResponse> {
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(val) = trimmed.strip_prefix("Content-Length: ") {
            content_length = val.trim().parse().unwrap_or(0);
        }
    }
    let mut buf = vec![0u8; content_length];
    reader.read_exact(&mut buf).await?;
    serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
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
