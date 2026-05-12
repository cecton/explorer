use crate::LoadedFile;
use crate::tui::state::AppSignal;
use camino::Utf8PathBuf;
use r3bl_tui::TerminalWindowMainThreadSignal;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;

macro_rules! log {
    ($($arg:tt)*) => {{
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true).append(true).open("/tmp/explorer.log")
        {
            let _ = writeln!(f, $($arg)*);
        }
    }};
}

pub type ColoredSpan = (String, Option<[u8; 3]>);
pub type ColoredLine = Vec<ColoredSpan>;

const RANGE_LINES: usize = 200;
const RANGE_THRESHOLD: usize = 100;

pub async fn run(
    root: Utf8PathBuf,
    files: Arc<Vec<LoadedFile>>,
    mut requests: mpsc::Receiver<usize>,
    colors: Arc<Mutex<HashMap<usize, Vec<ColoredLine>>>>,
    notify_tx: Arc<Mutex<Option<mpsc::Sender<TerminalWindowMainThreadSignal<AppSignal>>>>>,
    warmup_ms: Arc<Mutex<Option<u128>>>,
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

    let root_uri = format!("file://{root}");
    let pid = std::process::id();

    let init_req = json!({
        "jsonrpc": "2.0",
        "id": 0,
        "method": "initialize",
        "params": {
            "processId": pid,
            "rootUri": root_uri,
            "capabilities": {
                "textDocument": {
                    "semanticTokens": {
                        "requests": { "full": true, "range": true },
                        "tokenTypes": [],
                        "tokenModifiers": [],
                        "formats": ["relative"],
                        "multilineTokenSupport": false,
                        "overlappingTokenSupport": false
                    }
                }
            },
            "workspaceFolders": [{"uri": root_uri, "name": "root"}]
        }
    });

    if send_msg(&mut stdin, &init_req).await.is_err() {
        let _ = child.kill().await;
        return;
    }

    let (token_types, supports_range) = loop {
        let Ok(msg) = recv_msg(&mut reader).await else {
            let _ = child.kill().await;
            return;
        };
        if msg.get("id") == Some(&json!(0)) {
            let provider = &msg["result"]["capabilities"]["semanticTokensProvider"];
            let types = provider["legend"]["tokenTypes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let range = !provider["range"].is_null() && provider["range"] != json!(false);
            break (types, range);
        }
    };

    let notify_init = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}});
    if send_msg(&mut stdin, &notify_init).await.is_err() {
        let _ = child.kill().await;
        return;
    }

    let mut next_id = 1u64;
    // (file_idx, is_range, is_warmup)
    let mut pending: HashMap<u64, (usize, bool, bool)> = HashMap::new();
    let mut opened: HashSet<usize> = HashSet::new();

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
                let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");
                let has_id = msg.get("id").is_some();
                log!("recv: method={:?} has_id={} warmup_remaining={} notify_pending={}",
                    method, has_id, warmup_remaining, notify_pending);

                // Reply to any server-initiated request (e.g. window/workDoneProgress/create).
                if msg.get("method").is_some()
                    && let Some(id) = msg.get("id")
                {
                    log!("replying to server request: method={:?} id={}", method, id);
                    let reply = json!({"jsonrpc": "2.0", "id": id, "result": null});
                    if send_msg(&mut stdin, &reply).await.is_err() {
                        break;
                    }
                }

                if let Some(id) = msg.get("id").and_then(|v| v.as_u64())
                    && let Some((file_idx, is_range, is_warmup)) = pending.remove(&id)
                {
                    let has_data = msg["result"]["data"].is_array();
                    log!("token response: id={} file_idx={} is_range={} is_warmup={} has_data={} warmup_remaining={}",
                        id, file_idx, is_range, is_warmup, has_data, warmup_remaining);

                    if let Some(arr) = msg["result"]["data"].as_array() {
                        let data: Vec<u32> = arr
                            .iter()
                            .filter_map(|v| v.as_u64().map(|n| n as u32))
                            .collect();
                        let lines = decode_tokens(&data, &token_types, &files[file_idx].content);
                        let mut guard = colors.lock().unwrap();
                        if !is_range || !guard.contains_key(&file_idx) {
                            guard.insert(file_idx, lines);
                            drop(guard);
                            notify_pending = true;
                        }
                        if is_warmup && warmup_remaining > 0 {
                            warmup_remaining -= 1;
                            if warmup_remaining == 0 {
                                let elapsed = warmup_start.elapsed().as_millis();
                                *warmup_ms.lock().unwrap() = Some(elapsed);
                                notify_pending = true;
                                log!("warmup complete: elapsed={}ms", elapsed);
                            }
                        }
                    } else if is_warmup {
                        // Null response: rust-analyzer not ready yet. Retry up to 3 times.
                        let retries = warmup_retries.entry(file_idx).or_insert(0);
                        *retries += 1;
                        log!("warmup null: file_idx={} retry={}", file_idx, retries);
                        if *retries < 3 {
                            warmup_queue.push_back(file_idx);
                        } else {
                            // Give up on this file.
                            log!("warmup give up: file_idx={}", file_idx);
                            if warmup_remaining > 0 {
                                warmup_remaining -= 1;
                                if warmup_remaining == 0 {
                                    let elapsed = warmup_start.elapsed().as_millis();
                                    *warmup_ms.lock().unwrap() = Some(elapsed);
                                    notify_pending = true;
                                    log!("warmup complete (with gave-up files): elapsed={}ms", elapsed);
                                }
                            }
                        }
                    }
                }

                if notify_pending
                    && let Some(tx) = notify_tx.lock().unwrap().as_ref()
                {
                    let _ = tx.try_send(
                        TerminalWindowMainThreadSignal::ApplyAppSignal(AppSignal::Noop),
                    );
                    notify_pending = false;
                }
            }

            file_idx = requests.recv() => {
                let Some(file_idx) = file_idx else { break };
                log!("user request: file_idx={}", file_idx);
                let file = &files[file_idx];
                if file.path.extension() != Some("rs") {
                    continue;
                }
                let uri = format!("file://{}", file.path);

                if !opened.contains(&file_idx) {
                    let did_open = json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/didOpen",
                        "params": {
                            "textDocument": {
                                "uri": uri,
                                "languageId": "rust",
                                "version": 1,
                                "text": file.content
                            }
                        }
                    });
                    if send_msg(&mut stdin, &did_open).await.is_err() {
                        break;
                    }
                    opened.insert(file_idx);
                }

                let total_lines = file.line_starts.len();
                if supports_range && total_lines > RANGE_THRESHOLD {
                    let end_line = RANGE_LINES.min(total_lines);
                    let range_id = next_id;
                    next_id += 1;
                    let range_req = json!({
                        "jsonrpc": "2.0",
                        "id": range_id,
                        "method": "textDocument/semanticTokens/range",
                        "params": {
                            "textDocument": { "uri": uri },
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": end_line, "character": 0 }
                            }
                        }
                    });
                    if send_msg(&mut stdin, &range_req).await.is_err() {
                        break;
                    }
                    pending.insert(range_id, (file_idx, true, false));
                }

                let full_id = next_id;
                next_id += 1;
                let full_req = json!({
                    "jsonrpc": "2.0",
                    "id": full_id,
                    "method": "textDocument/semanticTokens/full",
                    "params": { "textDocument": { "uri": uri } }
                });
                if send_msg(&mut stdin, &full_req).await.is_err() {
                    break;
                }
                pending.insert(full_id, (file_idx, false, false));
            }

            // Send the next warmup file; only polled when ready and nothing to read.
            Some(file_idx) = async { warmup_queue.pop_front() },
                if !warmup_queue.is_empty() =>
            {
                let file = &files[file_idx];
                let uri = format!("file://{}", file.path);
                log!("warmup send: file_idx={} path={} queue_remaining={}", file_idx, file.path, warmup_queue.len());

                if !opened.contains(&file_idx) {
                    let did_open = json!({
                        "jsonrpc": "2.0",
                        "method": "textDocument/didOpen",
                        "params": {
                            "textDocument": {
                                "uri": uri,
                                "languageId": "rust",
                                "version": 1,
                                "text": file.content
                            }
                        }
                    });
                    if send_msg(&mut stdin, &did_open).await.is_err() {
                        break;
                    }
                    opened.insert(file_idx);
                }

                let total_lines = file.line_starts.len();
                if supports_range && total_lines > RANGE_THRESHOLD {
                    let end_line = RANGE_LINES.min(total_lines);
                    let range_id = next_id;
                    next_id += 1;
                    let range_req = json!({
                        "jsonrpc": "2.0",
                        "id": range_id,
                        "method": "textDocument/semanticTokens/range",
                        "params": {
                            "textDocument": { "uri": uri },
                            "range": {
                                "start": { "line": 0, "character": 0 },
                                "end": { "line": end_line, "character": 0 }
                            }
                        }
                    });
                    if send_msg(&mut stdin, &range_req).await.is_err() {
                        break;
                    }
                    pending.insert(range_id, (file_idx, true, true));
                } else {
                    let full_id = next_id;
                    next_id += 1;
                    let full_req = json!({
                        "jsonrpc": "2.0",
                        "id": full_id,
                        "method": "textDocument/semanticTokens/full",
                        "params": { "textDocument": { "uri": uri } }
                    });
                    if send_msg(&mut stdin, &full_req).await.is_err() {
                        break;
                    }
                    pending.insert(full_id, (file_idx, false, true));
                }
            }
        }
    }

    let _ = child.kill().await;
}

async fn send_msg(stdin: &mut ChildStdin, msg: &Value) -> std::io::Result<()> {
    let body = msg.to_string();
    let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    stdin.write_all(framed.as_bytes()).await?;
    stdin.flush().await
}

async fn recv_msg(reader: &mut BufReader<ChildStdout>) -> std::io::Result<Value> {
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

type LineTokens = Vec<(usize, usize, Option<[u8; 3]>)>;

fn decode_tokens(data: &[u32], token_types: &[String], content: &str) -> Vec<ColoredLine> {
    let lines: Vec<&str> = content.lines().collect();
    let mut line_tokens: Vec<LineTokens> = vec![Vec::new(); lines.len()];

    let mut abs_line = 0usize;
    let mut abs_char = 0usize;

    for chunk in data.chunks_exact(5) {
        let delta_line = chunk[0] as usize;
        let delta_start = chunk[1] as usize;
        let length = chunk[2] as usize;
        let type_idx = chunk[3] as usize;

        if delta_line > 0 {
            abs_line += delta_line;
            abs_char = delta_start;
        } else {
            abs_char += delta_start;
        }

        if abs_line >= lines.len() {
            continue;
        }

        let type_name = token_types.get(type_idx).map(String::as_str).unwrap_or("");
        let color = token_color(type_name);
        let line = lines[abs_line];
        let start = utf16_to_byte(line, abs_char);
        let end = utf16_to_byte(line, abs_char + length).min(line.len());
        if start < end {
            line_tokens[abs_line].push((start, end, color));
        }
    }

    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let tokens = &line_tokens[i];
            if tokens.is_empty() {
                return vec![(line.to_string(), None)];
            }
            let mut spans: ColoredLine = Vec::new();
            let mut pos = 0usize;
            for &(start, end, color) in tokens {
                if pos < start {
                    spans.push((line[pos..start].to_owned(), None));
                }
                spans.push((line[start..end].to_owned(), color));
                pos = end;
            }
            if pos < line.len() {
                spans.push((line[pos..].to_owned(), None));
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

fn token_color(token_type: &str) -> Option<[u8; 3]> {
    match token_type {
        "keyword" | "modifier" => Some([204, 120, 50]),
        "string" | "comment" => Some([106, 153, 85]),
        "number" => Some([181, 206, 168]),
        "type" | "class" | "struct" | "enum" | "interface" | "namespace" => Some([78, 201, 176]),
        "function" | "method" => Some([220, 220, 170]),
        "macro" => Some([189, 99, 197]),
        "variable" | "parameter" => Some([156, 220, 254]),
        "property" | "enumMember" => Some([206, 145, 120]),
        "operator" => Some([212, 212, 212]),
        _ => None,
    }
}
