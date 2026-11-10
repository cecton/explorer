//! Semantic tokens: opening a file on the server, asking for its colors and decoding
//! the answer into the per-line spans the preview paints.

use std::collections::HashSet;
use std::process::ChildStdin;

/// How many lines of a large file a warmup request asks to color.
const RANGE_LINES: usize = 200;

/// Above this many lines a warmup asks for the first range only.
const RANGE_THRESHOLD: usize = 100;

/// The token requests in flight and the files the server has open.
pub(super) struct TokenRequestState {
    pub(super) next_id: u64,
    /// By request id: the file, whether the answer is a range of lines, and whether the
    /// request was warmup.
    pub(super) pending: std::collections::HashMap<u64, (usize, bool, bool)>,
    pub(super) opened: HashSet<usize>,
}

impl TokenRequestState {
    pub(super) fn new() -> Self {
        Self {
            next_id: 1,
            pending: std::collections::HashMap::new(),
            opened: HashSet::new(),
        }
    }

    pub(super) fn take_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// Sends didClose to the server for a file that is no longer needed. Safe to call even
/// if the file was never opened.
pub(super) fn close_file(
    stdin: &mut ChildStdin,
    file: &crate::loader::LoadedFile,
    file_idx: usize,
    state: &mut TokenRequestState,
) -> std::io::Result<()> {
    if !state.opened.contains(&file_idx) {
        return Ok(());
    }
    tracing::debug!("LSP: didClose for file_idx={} path={}", file_idx, file.path);
    let close = crate::lsp::rpc::RpcNotification {
        jsonrpc: "2.0",
        method: "textDocument/didClose",
        params: serde_json::json!({
            "textDocument": { "uri": crate::lsp::rpc::file_uri(&file.path) }
        }),
    };
    crate::lsp::rpc::send_msg(stdin, &close)?;
    state.opened.remove(&file_idx);
    Ok(())
}

/// Asks for the file's tokens: a warmup request settles for the first range of a large
/// file, a user request asks for all of it.
pub(super) fn request_tokens(
    stdin: &mut ChildStdin,
    file: &crate::loader::LoadedFile,
    file_idx: usize,
    supports_range: bool,
    state: &mut TokenRequestState,
    is_warmup: bool,
) -> std::io::Result<()> {
    let total_lines = file.data.read().content.lines().count();
    let colored_len = file.colored_lines.lock().len();
    if colored_len == total_lines {
        return Ok(());
    }
    let uri = crate::lsp::rpc::file_uri(&file.path);

    if !is_warmup && colored_len == 0 {
        // A file whose colors were cleared asks again from the start.
        close_file(stdin, file, file_idx, state)?;
    }

    if !state.opened.contains(&file_idx) {
        let content = file.data.read().content.clone();
        tracing::debug!(
            "LSP: didOpen for file_idx={} ({} bytes)",
            file_idx,
            content.len()
        );
        use lsp_types::notification::Notification;
        let did_open = crate::lsp::rpc::RpcNotification {
            jsonrpc: "2.0",
            method: lsp_types::notification::DidOpenTextDocument::METHOD,
            params: lsp_types::DidOpenTextDocumentParams {
                text_document: lsp_types::TextDocumentItem {
                    uri: uri.clone(),
                    language_id: "rust".to_string(),
                    version: 1,
                    text: content,
                },
            },
        };
        crate::lsp::rpc::send_msg(stdin, &did_open)?;
        state.opened.insert(file_idx);
    }

    if is_warmup && supports_range && total_lines > RANGE_THRESHOLD && colored_len == 0 {
        let end_line = RANGE_LINES.min(total_lines) as u32;
        let range_id = state.take_id();
        tracing::debug!(
            "LSP: range request id={} file_idx={} lines=0..{}",
            range_id,
            file_idx,
            end_line
        );
        use lsp_types::request::Request;
        let range_req = crate::lsp::rpc::RpcRequest {
            jsonrpc: "2.0",
            id: range_id,
            method: lsp_types::request::SemanticTokensRangeRequest::METHOD,
            params: lsp_types::SemanticTokensRangeParams {
                work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
                partial_result_params: lsp_types::PartialResultParams::default(),
                text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
                range: lsp_types::Range {
                    start: lsp_types::Position {
                        line: 0,
                        character: 0,
                    },
                    end: lsp_types::Position {
                        line: end_line,
                        character: 0,
                    },
                },
            },
        };
        crate::lsp::rpc::send_msg(stdin, &range_req)?;
        state.pending.insert(range_id, (file_idx, true, is_warmup));
    }

    if is_warmup && supports_range && total_lines > RANGE_THRESHOLD {
        return Ok(());
    }

    let full_id = state.take_id();
    tracing::debug!("LSP: full request id={} file_idx={}", full_id, file_idx);
    use lsp_types::request::Request;
    let full_req = crate::lsp::rpc::RpcRequest {
        jsonrpc: "2.0",
        id: full_id,
        method: lsp_types::request::SemanticTokensFullRequest::METHOD,
        params: lsp_types::SemanticTokensParams {
            work_done_progress_params: lsp_types::WorkDoneProgressParams::default(),
            partial_result_params: lsp_types::PartialResultParams::default(),
            text_document: lsp_types::TextDocumentIdentifier { uri },
        },
    };
    crate::lsp::rpc::send_msg(stdin, &full_req)?;
    state.pending.insert(full_id, (file_idx, false, is_warmup));

    Ok(())
}

/// Writes the decoded colors of `file`, unless it already holds as many lines as the
/// answer would give. Returns whether they changed, which needs a repaint.
pub(super) fn apply_tokens(
    file: &crate::loader::LoadedFile,
    tokens: &lsp_types::SemanticTokens,
    is_range: bool,
    legend: &[crate::highlight::TokenKind],
) -> bool {
    let lines = {
        let data = file.data.read();
        let mut lines = decode_tokens(&tokens.data, &data.content, legend);
        if is_range {
            lines.truncate(RANGE_LINES);
        }
        lines
    };
    let mut guard = file.colored_lines.lock();
    let should_write = if is_range {
        guard.is_empty()
    } else {
        guard.len() != lines.len()
    };
    if !should_write {
        return false;
    }
    *guard = lines;
    true
}

/// The answer's tokens as one colored line per line of `content`, `legend` naming the
/// kind of each token type. Every line is covered end to end, a line no token reaches as
/// one `Other` span.
fn decode_tokens(
    data: &[lsp_types::SemanticToken],
    content: &str,
    legend: &[crate::highlight::TokenKind],
) -> Vec<crate::highlight::ColoredLine> {
    use crate::highlight::{ColoredLine, ColoredSpan, TokenKind};

    let lines: Vec<&str> = content.lines().collect();
    let mut line_tokens: Vec<ColoredLine> = vec![Vec::new(); lines.len()];

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

        let kind = legend.get(type_idx).copied().unwrap_or(TokenKind::Other);
        let line = lines[abs_line];
        let start = crate::lsp::utf16_to_byte(line, abs_char);
        let end = crate::lsp::utf16_to_byte(line, abs_char + length).min(line.len());
        if start < end {
            line_tokens[abs_line].push(ColoredSpan { start, end, kind });
        }
    }

    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let tokens = &line_tokens[i];
            if tokens.is_empty() {
                return vec![ColoredSpan {
                    start: 0,
                    end: line.len(),
                    kind: TokenKind::Other,
                }];
            }
            let mut spans: ColoredLine = Vec::new();
            let mut pos = 0usize;
            for &span in tokens {
                if pos < span.start {
                    spans.push(ColoredSpan {
                        start: pos,
                        end: span.start,
                        kind: TokenKind::Other,
                    });
                }
                spans.push(span);
                pos = span.end;
            }
            if pos < line.len() {
                spans.push(ColoredSpan {
                    start: pos,
                    end: line.len(),
                    kind: TokenKind::Other,
                });
            }
            spans
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::highlight::TokenKind;
    use lsp_types::SemanticToken;

    const LEGEND: [TokenKind; 2] = [TokenKind::Keyword, TokenKind::Function];

    /// Type index 0 is `Keyword` and 1 is `Function`; a line no token covers is one whole
    /// `Other` span.
    fn tokens(data: &[SemanticToken], content: &str) -> Vec<Vec<crate::highlight::ColoredSpan>> {
        super::decode_tokens(data, content, &LEGEND)
    }

    fn token(delta_line: u32, delta_start: u32, length: u32, token_type: u32) -> SemanticToken {
        SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: 0,
        }
    }

    #[test]
    fn a_line_without_tokens_is_one_other_span() {
        let lines = tokens(&[], "fn main() {\n    run();\n}\n");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].len(), 1);
        assert_eq!((lines[0][0].start, lines[0][0].end), (0, 11));
        assert_eq!(lines[0][0].kind, TokenKind::Other);
    }

    #[test]
    fn a_token_splits_its_line_into_a_span_on_either_side() {
        let lines = tokens(&[token(0, 0, 2, 0)], "fn main() {\n");
        let first = &lines[0];
        assert_eq!(first.len(), 2, "{first:?}");
        assert_eq!((first[0].start, first[0].end), (0, 2));
        assert_eq!(first[0].kind, TokenKind::Keyword);
        assert_eq!((first[1].start, first[1].end), (2, 11));
        assert_eq!(first[1].kind, TokenKind::Other);
    }

    /// A token's line and column are deltas on the token before it: a later line's column
    /// counts from the start of that line, not from the one before.
    #[test]
    fn deltas_place_a_token_on_a_later_line() {
        let lines = tokens(
            &[token(0, 0, 2, 0), token(1, 4, 3, 1)],
            "fn main() {\n    run();\n}\n",
        );
        let second = &lines[1];
        assert_eq!((second[0].start, second[0].end), (0, 4));
        assert_eq!((second[1].start, second[1].end), (4, 7));
        assert_eq!(second[1].kind, TokenKind::Function);
    }

    /// On one line a token's column is a delta on the token before it too.
    #[test]
    fn a_delta_within_a_line_moves_the_column_on() {
        let lines = tokens(&[token(0, 0, 2, 0), token(0, 5, 4, 1)], "fn main() {\n");
        let first = &lines[0];
        assert_eq!((first[1].start, first[1].end), (2, 5));
        assert_eq!((first[2].start, first[2].end), (5, 9));
        assert_eq!(first[2].kind, TokenKind::Function);
    }

    /// rust-analyzer knows more token types than Explorer has kinds: one the legend does
    /// not name is painted as `Other` rather than dropped.
    #[test]
    fn a_type_index_past_the_legend_falls_back_to_other() {
        let lines = tokens(&[token(0, 0, 2, 9)], "fn main() {\n");
        assert_eq!(lines[0][0].kind, TokenKind::Other);
    }

    /// A column is counted in UTF-16 units by the protocol but cut in bytes here.
    #[test]
    fn a_token_after_a_multibyte_character_starts_at_its_byte() {
        let lines = tokens(&[token(0, 2, 1, 0)], "é x\n");
        assert_eq!((lines[0][0].start, lines[0][0].end), (0, 3));
        assert_eq!((lines[0][1].start, lines[0][1].end), (3, 4));
        assert_eq!(lines[0][1].kind, TokenKind::Keyword);
    }

    /// A token pointing past the end of its line is cut at the line's end rather than
    /// panicking on a byte offset that split a character.
    #[test]
    fn a_token_past_the_end_of_its_line_is_cut_at_the_end() {
        let lines = tokens(&[token(0, 1, 99, 0)], "fn\n");
        assert_eq!(lines[0].len(), 2);
        assert_eq!((lines[0][0].start, lines[0][0].end), (0, 1));
        assert_eq!((lines[0][1].start, lines[0][1].end), (1, 2));
    }
}
