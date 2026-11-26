//! Explorer's own agent tools, offered to other MCP clients over standard input and
//! output: the `conversation`, `format_markdown` and `knowledge` tools, plus the MCP
//! servers the repository's session lists.
//!
//! The tools here are the ones a client has no way to do itself: the conversation tree is
//! Explorer's own data, markdown goes through Explorer's formatter, and the knowledge base
//! is a store only Explorer lays out. Reading, globbing, grepping, editing and running a
//! shell are the client's own business, and serving them would put a shell behind anything
//! able to start this process.

/// The protocol every request and reply here speaks; the same version Explorer's own
/// MCP client asks for.
const PROTOCOL_VERSION: &str = "2025-06-18";
/// Names the client in Explorer's logs and in the server's own answer to `initialize`.
const SERVER_NAME: &str = "explorer";

/// The argument that stops a server from starting the repository's own MCP servers: the
/// claude run it serves gets those from its own `--mcp-config`, and offering them again
/// under `mcp__explorer__` names would only duplicate them.
pub const OWN_TOOLS_ONLY: &str = "--own-tools-only";

/// A client outside Explorer has no node of its own, so the pages it reads mark nothing
/// as "this turn" or "your branch".
const EXTERNAL_NODE: &str = "external";

/// The tools this server advertises, by the names the agent tools use.
const SERVED: [&str; 3] = ["conversation", "format_markdown", "knowledge"];

/// What one client's requests act on.
struct Server {
    root: camino::Utf8PathBuf,
    /// The repository's conversation tree, read again for every call so a client that
    /// outlives a turn sees what it wrote.
    tree: std::sync::Arc<parking_lot::RwLock<crate::conversation::ConversationTree>>,
    /// The MCP servers the repository's session lists, started for as long as this
    /// server runs; their tools are forwarded under their own `mcp__<server>__<tool>`
    /// names. Empty when the client already has them.
    mcp: std::sync::Arc<crate::agent::mcp::McpHost>,
    /// The turn's socket, which answers the privileged tools; without one they read the disk.
    privileged_socket: Option<camino::Utf8PathBuf>,
}

impl Server {
    /// Starts `servers`, the MCP servers of the repository a client can call through this
    /// one. Neither the repository walk nor the TUI is touched: serving a tool must stay
    /// cheap.
    fn start(
        root: camino::Utf8PathBuf,
        servers: Vec<crate::agent::mcp::McpServerDecl>,
        privileged_socket: Option<camino::Utf8PathBuf>,
    ) -> Self {
        let mcp = std::sync::Arc::new(crate::agent::mcp::McpHost::new(root.clone()));
        let notify: crate::agent::mcp::Notify = std::sync::Arc::new(|_| {});
        for decl in &servers {
            mcp.start(decl, std::sync::Arc::clone(&notify));
        }
        Self {
            root,
            tree: std::sync::Arc::default(),
            mcp,
            privileged_socket,
        }
    }

    fn context(&self, mcp: crate::agent::mcp::McpTools) -> crate::agent::tools::ToolContext {
        crate::agent::tools::ToolContext {
            root: self.root.clone(),
            tree: std::sync::Arc::clone(&self.tree),
            node: crate::conversation::NodeId::new(EXTERNAL_NODE),
            node_dir: None,
            background: std::sync::Arc::default(),
            mcp,
            privileged: None,
        }
    }

    /// The tools a client starting now can call: this server's own, then those of the
    /// repository's servers.
    async fn specs(&self) -> Vec<crate::agent::llm::ToolSpec> {
        use crate::agent::mcp::McpTools;
        let context = self.context(self.mcp.snapshot().await);
        crate::agent::tools::specs(&context, None)
            .into_iter()
            .filter(|spec| SERVED.contains(&spec.name.as_str()) || McpTools::is_tool(&spec.name))
            .collect()
    }

    /// Runs one call. A name this server does not offer is an error the client sees.
    async fn call(&self, name: &str, arguments: serde_json::Value) -> Result<String, String> {
        use crate::agent::mcp::McpTools;
        if !SERVED.contains(&name) && !McpTools::is_tool(name) {
            return Err(format!("unknown tool: {name}"));
        }
        if let Some(socket) = &self.privileged_socket
            && crate::agent::privileged::PRIVILEGED.contains(&name)
        {
            return crate::agent::privileged::call(socket, name, arguments).await;
        }
        // The tree is read afresh: a client outlives the turns of the window that started
        // it, and a node written since this process began must be readable.
        let tree = match crate::conversation::conversations_dir(&self.root) {
            Some(dir) => crate::conversation::ConversationTree::load(&dir),
            None => crate::conversation::ConversationTree::default(),
        };
        *self.tree.write() = tree;
        let call = crate::agent::ToolCall {
            id: String::new(),
            name: name.to_string(),
            arguments,
        };
        let context = self.context(self.mcp.snapshot().await);
        crate::agent::tools::try_run(&context, &call).await
    }

    /// Answers one message, or `None` for a notification, which takes no reply.
    async fn answer(&self, message: &serde_json::Value) -> Option<serde_json::Value> {
        let id = message.get("id")?.clone();
        let method = message
            .get("method")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let params = message
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let reply = |result: serde_json::Value| serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
        let error = |code: i64, text: String| {
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": code, "message": text},
            })
        };
        Some(match method {
            "initialize" => reply(serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
            })),
            // Answered so a client that waits for a reply before listing tools goes on.
            "ping" => reply(serde_json::json!({})),
            "tools/list" => {
                let tools: Vec<serde_json::Value> = self
                    .specs()
                    .await
                    .into_iter()
                    .map(|spec| {
                        serde_json::json!({
                            "name": spec.name,
                            "description": spec.description,
                            "inputSchema": spec.schema,
                        })
                    })
                    .collect();
                reply(serde_json::json!({"tools": tools}))
            }
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                match self.call(name, arguments).await {
                    Ok(text) => reply(serde_json::json!({
                        "content": [{"type": "text", "text": text}],
                    })),
                    Err(text) => reply(serde_json::json!({
                        "content": [{"type": "text", "text": text}],
                        "isError": true,
                    })),
                }
            }
            other => error(-32601, format!("{other} is not supported")),
        })
    }
}

/// Serves the tools of the repository at `root` until the client closes its input. `servers`
/// are the MCP servers of the repository's session, started so the client sees them too; the
/// caller reads them, since a session is the UI's business and not the agent's.
pub async fn serve(
    root: camino::Utf8PathBuf,
    servers: Vec<crate::agent::mcp::McpServerDecl>,
    privileged_socket: Option<camino::Utf8PathBuf>,
) -> i32 {
    let server = std::sync::Arc::new(Server::start(root, servers, privileged_socket));
    converse(
        server,
        tokio::io::BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    )
    .await
}

/// Answers every line of `input` on `output` until either end closes. One reply per
/// request, each on a line of its own: that is the whole wire between a client and this
/// server, and the one place a stray write would break the protocol.
async fn converse(
    server: std::sync::Arc<Server>,
    input: impl tokio::io::AsyncRead + Unpin,
    mut output: impl tokio::io::AsyncWrite + Unpin,
) -> i32 {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    // Split on bytes: a stray line that is not UTF-8 must not end the server as `lines`
    // would. Explorer's own client reads its servers the same way.
    let mut lines = tokio::io::BufReader::new(input).split(b'\n');
    while let Ok(Some(line)) = lines.next_segment().await {
        let Ok(message) = serde_json::from_slice::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(answer) = server.answer(&message).await else {
            continue;
        };
        let mut written = answer.to_string();
        written.push('\n');
        if output.write_all(written.as_bytes()).await.is_err() || output.flush().await.is_err() {
            break;
        }
    }
    0
}

/// The `mcpServers` entry that hands a Claude Code run Explorer's own tools, as the
/// `--mcp-config` value its arguments carry. `own_tools_only` names the run's own
/// `--mcp-config` as the source of the repository's servers, so this process does not
/// offer them a second time under longer names.
pub fn claude_server(
    root: &camino::Utf8Path,
    own_tools_only: bool,
    privileged_socket: Option<&camino::Utf8Path>,
) -> (String, serde_json::Value) {
    let program = std::env::current_exe()
        .ok()
        .and_then(|path| camino::Utf8PathBuf::from_path_buf(path).ok())
        .unwrap_or_else(|| camino::Utf8PathBuf::from(SERVER_NAME));
    let mut args = vec!["mcp-serve".to_string(), root.to_string()];
    if own_tools_only {
        args.push(OWN_TOOLS_ONLY.to_string());
    }
    if let Some(socket) = privileged_socket {
        args.extend(["--privileged-socket".to_string(), socket.to_string()]);
    }
    (
        SERVER_NAME.to_string(),
        serde_json::json!({"command": program, "args": args}),
    )
}

#[cfg(test)]
mod tests {
    use super::{PROTOCOL_VERSION, SERVER_NAME, Server, claude_server, converse};

    fn server() -> (
        tempfile::TempDir,
        camino::Utf8PathBuf,
        std::sync::Arc<Server>,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(tmp.path().canonicalize().unwrap()).unwrap();
        let server = std::sync::Arc::new(Server::start(root.clone(), Vec::new(), None));
        (tmp, root, server)
    }

    async fn ask(server: &Server, method: &str, params: serde_json::Value) -> serde_json::Value {
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        server
            .answer(&message)
            .await
            .expect("a request is answered")
    }

    #[tokio::test]
    async fn the_server_speaks_the_handshake_and_ignores_notifications() {
        let (_tmp, _root, server) = server();
        let reply = ask(&server, "initialize", serde_json::json!({})).await;
        assert_eq!(reply["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(reply["result"]["serverInfo"]["name"], SERVER_NAME);
        assert!(reply["result"]["capabilities"]["tools"].is_object());
        let notification =
            serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert!(server.answer(&notification).await.is_none());
    }

    #[tokio::test]
    async fn only_explorers_own_tools_and_the_repositorys_are_advertised() {
        let (_tmp, _root, server) = server();
        let reply = ask(&server, "tools/list", serde_json::json!({})).await;
        let names: Vec<&str> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["conversation", "knowledge", "format_markdown"]);
        for absent in ["read", "glob", "grep", "shell", "write", "edit"] {
            assert!(!names.contains(&absent), "{absent} must not be served");
        }
    }

    #[tokio::test]
    async fn a_call_answers_with_text_and_an_unknown_tool_is_an_error() {
        let (_tmp, root, server) = server();
        std::fs::write(root.join("a.md"), "# T\n").unwrap();
        let reply = ask(
            &server,
            "tools/call",
            serde_json::json!({"name": "conversation", "arguments": {"action": "toc"}}),
        )
        .await;
        assert!(reply["result"]["content"][0]["text"].as_str().is_some());

        let reply = ask(
            &server,
            "tools/call",
            serde_json::json!({"name": "shell", "arguments": {"command": "true"}}),
        )
        .await;
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(reply["result"]["content"][0]["text"], "unknown tool: shell");
    }

    /// Drives the real wire: the bytes a client sends and the replies it gets back.
    async fn converse_bytes_with(
        server: std::sync::Arc<Server>,
        input: &[u8],
    ) -> Vec<serde_json::Value> {
        let mut sink: Vec<u8> = Vec::new();
        converse(server, input, &mut sink).await;
        String::from_utf8(sink)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    async fn converse_with(server: std::sync::Arc<Server>, input: &str) -> Vec<serde_json::Value> {
        converse_bytes_with(server, input.as_bytes()).await
    }

    #[tokio::test]
    async fn a_line_that_is_not_utf8_does_not_end_the_server() {
        let (_tmp, _root, server) = server();
        let mut input: Vec<u8> = Vec::new();
        input.extend_from_slice(b"\xff\xfe\n");
        input.extend_from_slice(
            serde_json::json!({"jsonrpc": "2.0", "id": 7, "method": "initialize"})
                .to_string()
                .as_bytes(),
        );
        input.push(b'\n');
        let replies = converse_bytes_with(server, &input).await;
        assert_eq!(replies.len(), 1, "the stray line is skipped, not fatal");
        assert_eq!(replies[0]["id"], 7);
    }

    #[tokio::test]
    async fn the_wire_carries_one_json_reply_per_request_and_nothing_else() {
        let (_tmp, _root, server) = server();
        let input = format!(
            "{}\n{}\nnot json at all\n{}\n",
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
            serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        );
        let replies = converse_with(server, &input).await;
        // The notification and the line that is not JSON take no reply: two lines, two ids.
        assert_eq!(replies.len(), 2, "{replies:?}");
        assert_eq!(replies[0]["id"], 1);
        assert_eq!(replies[1]["id"], 2);
        assert_eq!(replies[1]["result"]["tools"][0]["name"], "conversation");
    }

    #[tokio::test]
    async fn a_tool_the_client_never_asked_about_is_refused_and_the_server_goes_on() {
        let (_tmp, _root, server) = server();
        let input = format!(
            "{}\n{}\n",
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": "edit", "arguments": {"path": "a.md"}}}),
            serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        );
        let replies = converse_with(server, &input).await;
        assert_eq!(replies[0]["result"]["isError"], true);
        assert_eq!(replies[1]["id"], 2, "the server answered the next request");
    }

    #[tokio::test]
    async fn the_servers_a_client_is_given_are_listed_beside_its_own() {
        // A fake server script, so the forwarding is exercised without a real one.
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(tmp.path().canonicalize().unwrap()).unwrap();
        let script = tmp.path().join("fake-mcp");
        crate::agent::mcp::write_script(
            &script,
            r#"#!/bin/sh
reply() { printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$1"; }
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/^{[^{]*"id":\([0-9]*\).*/\1/p')
  case "$line" in
  *'"method":"initialize"'*) reply '{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"0"}}' ;;
  *'"method":"tools/list"'*) reply '{"tools":[{"name":"echo","description":"Says hi","inputSchema":{"type":"object"}}]}' ;;
  esac
done
"#,
        );
        let decl = crate::agent::mcp::McpServerDecl {
            name: "fake".to_string(),
            program: script.to_str().unwrap().to_string(),
        };
        let server = std::sync::Arc::new(Server::start(root, vec![decl], None));
        let reply = ask(&server, "tools/list", serde_json::json!({})).await;
        let names: Vec<&str> = reply["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "conversation",
                "knowledge",
                "format_markdown",
                "mcp__fake__echo"
            ]
        );
    }

    #[tokio::test]
    async fn conversation_goes_to_the_turns_socket_when_there_is_one() {
        use crate::agent::mcp::McpTools;
        use crate::agent::privileged::PrivilegedSocket;
        use crate::agent::tools::ToolContext;
        use crate::conversation::NodeId;
        let (_tmp, root, _) = server();
        // The socket's tree is the one that knows the node; a read from disk would not.
        let tree = std::sync::Arc::default();
        let socket = PrivilegedSocket::start(ToolContext {
            root: root.clone(),
            tree,
            node: NodeId::new("n7"),
            node_dir: None,
            background: std::sync::Arc::default(),
            mcp: McpTools::default(),
            privileged: None,
        })
        .unwrap();
        let server = Server::start(root, Vec::new(), Some(socket.path()));
        let reply = ask(
            &server,
            "tools/call",
            serde_json::json!({"name": "conversation", "arguments": {"action": "toc", "node": "n7"}}),
        )
        .await;
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(reply["result"]["content"][0]["text"], "unknown node: n7");

        drop(socket);
        let reply = ask(
            &server,
            "tools/call",
            serde_json::json!({"name": "conversation", "arguments": {"action": "toc"}}),
        )
        .await;
        assert_eq!(
            reply["result"]["isError"], true,
            "a finished turn answers nothing"
        );
    }

    #[test]
    fn a_claude_entry_carries_the_turns_socket() {
        let root = camino::Utf8Path::new("/repo");
        let socket = camino::Utf8Path::new("/run/turn/tools.sock");
        let (_, config) = claude_server(root, true, Some(socket));
        assert_eq!(
            config["args"],
            serde_json::json!([
                "mcp-serve",
                "/repo",
                super::OWN_TOOLS_ONLY,
                "--privileged-socket",
                "/run/turn/tools.sock"
            ])
        );
    }

    #[test]
    fn a_claude_entry_always_names_explorer_and_its_own_tools_only_flag() {
        let root = camino::Utf8Path::new("/repo");
        let (plain, plain_config) = claude_server(root, false, None);
        assert_eq!(plain, SERVER_NAME);
        assert_eq!(
            plain_config["args"],
            serde_json::json!(["mcp-serve", "/repo"])
        );
        let (_, own_config) = claude_server(root, true, None);
        assert_eq!(
            own_config["args"],
            serde_json::json!(["mcp-serve", "/repo", super::OWN_TOOLS_ONLY])
        );
        assert!(
            own_config["command"]
                .as_str()
                .is_some_and(|p| !p.is_empty())
        );
    }
}
