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

/// The `file://` URI rust-analyzer names `path` by, with every byte that is not
/// valid in a URI percent-encoded.
pub(super) fn file_uri(path: &camino::Utf8Path) -> lsp_types::Uri {
    let url = url::Url::from_file_path(path.as_std_path())
        .expect("rust-analyzer is only given absolute paths");
    // `url` leaves these four alone in a path, but `lsp_types::Uri` refuses them.
    let encoded = url
        .as_str()
        .replace('[', "%5B")
        .replace(']', "%5D")
        .replace('^', "%5E")
        .replace('|', "%7C");
    encoded.parse().expect("an encoded file URL is a valid URI")
}

/// The path a `file://` URI names, or `None` for any other kind of URI.
pub(super) fn uri_path(uri: &lsp_types::Uri) -> Option<camino::Utf8PathBuf> {
    let path = url::Url::parse(uri.as_str()).ok()?.to_file_path().ok()?;
    camino::Utf8PathBuf::from_path_buf(path).ok()
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

#[cfg(test)]
mod tests {
    use super::*;

    const PATHS: [&str; 6] = [
        "/repo/my code/src/caf\u{e9}.rs",
        "/a/b[1]/c.rs",
        "/a/100%/c.rs",
        "/a/x#y.rs",
        "/a/what?.rs",
        "/a/x^y|z.rs",
    ];

    #[test]
    fn file_uri_round_trips_awkward_paths() {
        for path in PATHS {
            let path = camino::Utf8Path::new(path);
            let uri = file_uri(path);
            assert_eq!(uri_path(&uri).as_deref(), Some(path), "{path}");
        }
    }

    #[test]
    fn file_uri_of_a_plain_path_is_unchanged() {
        let uri = file_uri(camino::Utf8Path::new("/repo/src/main.rs"));
        assert_eq!(uri.as_str(), "file:///repo/src/main.rs");
    }

    #[test]
    fn uri_path_reads_a_percent_encoded_uri() {
        let uri: lsp_types::Uri = "file:///repo/my%20code/caf%C3%A9.rs".parse().unwrap();
        assert_eq!(
            uri_path(&uri).as_deref(),
            Some(camino::Utf8Path::new("/repo/my code/caf\u{e9}.rs"))
        );
    }

    #[test]
    fn uri_path_refuses_other_schemes() {
        let uri: lsp_types::Uri = "https://example.com/a.rs".parse().unwrap();
        assert_eq!(uri_path(&uri), None);
    }
}
