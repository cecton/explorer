//! What travels over the pipe to rust-analyzer: JSON-RPC messages framed by
//! `Content-Length` headers, and the URIs the messages name files by.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::io::AsFd;
use std::process::{ChildStdin, ChildStdout};

#[derive(serde::Serialize)]
pub(super) struct RpcRequest<P: serde::Serialize> {
    pub(super) jsonrpc: &'static str,
    pub(super) id: u64,
    pub(super) method: &'static str,
    pub(super) params: P,
}

#[derive(serde::Serialize)]
pub(super) struct RpcNotification<P: serde::Serialize> {
    pub(super) jsonrpc: &'static str,
    pub(super) method: &'static str,
    pub(super) params: P,
}

#[derive(serde::Deserialize)]
pub(super) struct RpcResponse {
    pub(super) id: Option<serde_json::Value>,
    pub(super) method: Option<String>,
    pub(super) result: Option<serde_json::Value>,
    /// Payload of server-initiated requests and notifications (e.g.
    /// `experimental/serverStatus`). Only `result` is set for responses to our own
    /// requests, so this defaults to `None` there.
    #[serde(default)]
    pub(super) params: Option<serde_json::Value>,
}

/// The `file://` URI rust-analyzer names `path` by.
pub(super) fn file_uri(path: &camino::Utf8Path) -> lsp_types::Uri {
    format!("file://{path}")
        .parse()
        .expect("a path makes a valid file URI")
}

pub(super) fn send_msg<T: serde::Serialize>(
    stdin: &mut ChildStdin,
    msg: &T,
) -> std::io::Result<()> {
    let body = serde_json::to_string(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    stdin.write_all(framed.as_bytes())?;
    stdin.flush()
}

pub(super) fn recv_msg(reader: &mut BufReader<ChildStdout>) -> std::io::Result<RpcResponse> {
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
    // Read body in chunks. BufReader may already have the body buffered (from the
    // read_line calls above), so check buffer before polling fd.
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

/// Returns true if the fd has data available within the timeout.
/// Retries on EINTR (signal interruption).
pub(super) fn poll_readable(fd: &impl AsFd, timeout: std::time::Duration) -> bool {
    use rustix::io::Errno;
    let mut pollfds = [rustix::event::PollFd::new(fd, rustix::event::PollFlags::IN)];
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    loop {
        match rustix::event::poll(&mut pollfds, ms) {
            Ok(_) => {
                return pollfds[0].revents().intersects(
                    rustix::event::PollFlags::IN
                        | rustix::event::PollFlags::HUP
                        | rustix::event::PollFlags::ERR,
                );
            }
            Err(Errno::INTR) => continue,
            Err(_) => return false,
        }
    }
}
