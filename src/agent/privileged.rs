//! The socket of one turn, through which a process that runs the turn's tools reaches the
//! tools only Explorer can serve: those that read its own state (the `conversation` tool
//! needs the tree held in memory, current down to nodes not yet flushed).
//!
//! A turn opens one socket and the names it answers are fixed when it opens, so whoever
//! finds the path can ask for no more than the turn's own tool list offers. The socket
//! lives in a private directory removed with the guard, and nothing outlives the turn.
//! The wire is one JSON line per request, `{"name", "arguments"}`, and one per reply,
//! `{"ok"}` or `{"error"}`.

use std::sync::Arc;

/// The tools served through the socket instead of where the agent runs.
pub const PRIVILEGED: [&str; 1] = ["conversation"];

/// A turn's socket; dropping it stops the answers and removes the socket.
pub struct PrivilegedSocket {
    // Removed on drop, with the socket inside.
    _dir: tempfile::TempDir,
    path: camino::Utf8PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl PrivilegedSocket {
    /// Opens `node`'s turn socket, answering the `allowed` tools of `tree`; needs a tokio runtime.
    pub fn start(
        root: camino::Utf8PathBuf,
        tree: Arc<parking_lot::RwLock<crate::conversation::ConversationTree>>,
        node: crate::conversation::NodeId,
        allowed: &'static [&'static str],
    ) -> Result<Self, String> {
        use crate::agent::mcp::McpTools;
        use crate::agent::tools::ToolContext;
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .filter(|dir| dir.is_dir())
            .unwrap_or_else(std::env::temp_dir);
        let dir = tempfile::Builder::new()
            .prefix("explorer-turn-")
            .tempdir_in(base)
            .map_err(|e| format!("could not make the tool socket's directory: {e}"))?;
        let path = camino::Utf8PathBuf::from_path_buf(dir.path().join("tools.sock"))
            .map_err(|path| format!("the tool socket's path is not UTF-8: {}", path.display()))?;
        let listener = tokio::net::UnixListener::bind(&path)
            .map_err(|e| format!("could not open the tool socket: {e}"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("could not restrict the tool socket: {e}"))?;
        let context = Arc::new(ToolContext {
            root,
            tree,
            node,
            node_dir: None,
            background: Arc::default(),
            mcp: McpTools::default(),
        });
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(answer(stream, Arc::clone(&context), allowed));
            }
        });
        Ok(Self {
            _dir: dir,
            path,
            task,
        })
    }

    pub fn path(&self) -> camino::Utf8PathBuf {
        self.path.clone()
    }
}

impl Drop for PrivilegedSocket {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Answers the requests of one connection until the client closes it.
async fn answer(
    stream: tokio::net::UnixStream,
    context: Arc<crate::agent::tools::ToolContext>,
    allowed: &'static [&'static str],
) {
    use crate::agent::ToolCall;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let (reader, mut writer) = stream.into_split();
    let mut lines = tokio::io::BufReader::new(reader).split(b'\n');
    while let Ok(Some(line)) = lines.next_segment().await {
        let reply = match serde_json::from_slice::<Request>(&line) {
            Ok(request) if allowed.contains(&request.name.as_str()) => {
                let call = ToolCall {
                    id: String::new(),
                    name: request.name,
                    arguments: request.arguments,
                };
                crate::agent::tools::try_run(&context, &call).await
            }
            Ok(request) => Err(format!("unknown tool: {}", request.name)),
            Err(error) => Err(format!("bad request: {error}")),
        };
        let mut written = match reply {
            Ok(ok) => serde_json::json!({"ok": ok}),
            Err(error) => serde_json::json!({"error": error}),
        }
        .to_string();
        written.push('\n');
        if writer.write_all(written.as_bytes()).await.is_err() {
            break;
        }
    }
}

#[derive(serde::Deserialize)]
struct Request {
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

/// Asks the turn behind `socket` to run one tool.
pub async fn call(
    socket: &camino::Utf8Path,
    name: &str,
    arguments: serde_json::Value,
) -> Result<String, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .map_err(|e| format!("the turn's tool socket is not reachable: {e}"))?;
    let (reader, mut writer) = stream.into_split();
    let mut request = serde_json::json!({"name": name, "arguments": arguments}).to_string();
    request.push('\n');
    writer
        .write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut lines = tokio::io::BufReader::new(reader).split(b'\n');
    let line = lines
        .next_segment()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("the turn's tool socket closed without an answer")?;
    let reply: serde_json::Value = serde_json::from_slice(&line).map_err(|e| e.to_string())?;
    match (reply.get("ok"), reply.get("error")) {
        (Some(serde_json::Value::String(ok)), _) => Ok(ok.clone()),
        (_, Some(serde_json::Value::String(error))) => Err(error.clone()),
        _ => Err("the turn's tool socket sent a reply this client cannot read".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn socket() -> (PrivilegedSocket, camino::Utf8PathBuf) {
        let socket = PrivilegedSocket::start(
            camino::Utf8PathBuf::from("/repo"),
            Arc::default(),
            crate::conversation::NodeId::new("n1"),
            &PRIVILEGED,
        )
        .unwrap();
        let path = socket.path();
        (socket, path)
    }

    #[tokio::test]
    async fn the_socket_answers_a_privileged_tool_from_the_turns_tree() {
        let (_socket, path) = socket();
        let text = call(&path, "conversation", serde_json::json!({"action": "toc"}))
            .await
            .unwrap();
        assert!(!text.is_empty());
        let failed = call(&path, "conversation", serde_json::json!({"action": "nope"})).await;
        assert!(failed.is_err(), "a tool's failure comes back as an error");
    }

    #[tokio::test]
    async fn the_socket_refuses_a_name_the_turn_did_not_allow() {
        let (_socket, path) = socket();
        for name in ["shell", "write", "format_markdown"] {
            let refused = call(&path, name, serde_json::json!({"command": "true"})).await;
            assert_eq!(refused, Err(format!("unknown tool: {name}")));
        }
    }

    #[tokio::test]
    async fn the_socket_is_private_and_disappears_with_the_turn() {
        use std::os::unix::fs::PermissionsExt;
        let (socket, path) = socket();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        drop(socket);
        assert!(!path.exists());
        assert!(
            call(&path, "conversation", serde_json::json!({}))
                .await
                .is_err()
        );
    }
}
