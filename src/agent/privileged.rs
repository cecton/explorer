//! The socket of one turn, through which a process that runs the turn's tools reaches the
//! tools only Explorer can serve: those that read its own state (the `conversation` tool
//! needs the tree held in memory, current down to nodes not yet flushed).
//!
//! A turn opens one socket, whose answers are no more than the turn's own tool list offers.
//! It lives in a private directory removed with the guard. The wire is one JSON line per
//! request, `{"name", "arguments"}`, and one per reply, `{"ok"}` or `{"error"}`.

use std::sync::Arc;

/// The tools served through the socket instead of where the agent runs.
pub const PRIVILEGED: [&str; 1] = ["conversation"];

/// The longest path a unix socket address takes, less the terminating NUL.
const SOCKET_PATH_MAX: usize = 107;

/// `/explorer-turn-` and six random characters, then `/tools.sock`.
const SOCKET_PATH_SUFFIX: usize = 32;

/// A turn's socket; dropping it stops the calls it is answering and removes the socket.
pub struct PrivilegedSocket {
    // Removed on drop, with the socket inside.
    _dir: tempfile::TempDir,
    path: camino::Utf8PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl PrivilegedSocket {
    /// Opens the turn's socket for `context`; needs a tokio runtime.
    pub fn start(context: crate::agent::tools::ToolContext) -> Result<Self, String> {
        use std::os::unix::fs::PermissionsExt;
        let base = socket_base(
            std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from),
            std::env::temp_dir(),
        );
        let dir = tempfile::Builder::new()
            .prefix("explorer-turn-")
            .tempdir_in(base)
            .map_err(|e| format!("could not make the tool socket's directory: {e}"))?;
        let path = camino::Utf8PathBuf::from_path_buf(dir.path().join("tools.sock"))
            .map_err(|path| format!("the tool socket's path is not UTF-8: {}", path.display()))?;
        if path.as_str().len() > SOCKET_PATH_MAX {
            return Err(format!("the tool socket's path is too long: {path}"));
        }
        let listener = tokio::net::UnixListener::bind(&path)
            .map_err(|e| format!("could not open the tool socket: {e}"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("could not restrict the tool socket: {e}"))?;
        let context = Arc::new(context);
        let task = tokio::spawn(async move {
            // Dropped with this task, which stops every call still running.
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        connections.spawn(answer(stream, Arc::clone(&context)));
                    }
                    Some(_) = connections.join_next() => {}
                }
            }
            while connections.join_next().await.is_some() {}
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

/// `$XDG_RUNTIME_DIR` when it is a directory with room for the path, else the temp dir.
fn socket_base(
    runtime_dir: Option<std::path::PathBuf>,
    temp_dir: std::path::PathBuf,
) -> std::path::PathBuf {
    runtime_dir
        .filter(|dir| dir.is_dir() && dir.as_os_str().len() + SOCKET_PATH_SUFFIX <= SOCKET_PATH_MAX)
        .unwrap_or(temp_dir)
}

/// Answers the requests of one connection until the client closes it.
async fn answer(stream: tokio::net::UnixStream, context: Arc<crate::agent::tools::ToolContext>) {
    use crate::agent::ToolCall;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let (reader, mut writer) = stream.into_split();
    let mut lines = tokio::io::BufReader::new(reader).split(b'\n');
    while let Ok(Some(line)) = lines.next_segment().await {
        let reply = match serde_json::from_slice::<Request>(&line) {
            Ok(request) if PRIVILEGED.contains(&request.name.as_str()) => {
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

    fn context() -> crate::agent::tools::ToolContext {
        use crate::agent::mcp::McpTools;
        use crate::agent::tools::ToolContext;
        use crate::conversation::NodeId;
        ToolContext {
            root: camino::Utf8PathBuf::from("/repo"),
            tree: Arc::default(),
            node: NodeId::new("n1"),
            node_dir: None,
            background: Arc::default(),
            mcp: McpTools::default(),
            privileged: None,
        }
    }

    fn socket() -> (PrivilegedSocket, camino::Utf8PathBuf) {
        let socket = PrivilegedSocket::start(context()).unwrap();
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
        for name in ["shell", "write", "format_markdown", "agent"] {
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

    #[test]
    fn the_socket_directory_falls_back_when_the_runtime_dir_has_no_room() {
        let tmp = tempfile::tempdir().unwrap();
        let temp = std::path::PathBuf::from("/fallback");
        let short = tmp.path().to_path_buf();
        assert_eq!(socket_base(Some(short.clone()), temp.clone()), short);
        assert_eq!(socket_base(None, temp.clone()), temp);
        assert_eq!(socket_base(Some(short.join("missing")), temp.clone()), temp);
        let long = short.join("a".repeat(80));
        std::fs::create_dir(&long).unwrap();
        assert_eq!(socket_base(Some(long), temp.clone()), temp);
    }
}
