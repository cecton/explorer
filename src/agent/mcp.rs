use super::llm::ToolSpec;
use super::tools::truncate_middle;
use camino::{Utf8Path, Utf8PathBuf};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

const PROTOCOL_VERSION: &str = "2025-06-18";
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
const START_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a turn waits for the servers still starting when it begins.
const START_WAIT: Duration = Duration::from_secs(10);
const MAX_OUTPUT_BYTES: usize = 30_000;
/// Opens the name of every MCP tool: `mcp__<server>__<tool>`.
const TOOL_PREFIX: &str = "mcp__";
const SEPARATOR: &str = "__";
/// The longest tool name the endpoints accept.
const MAX_TOOL_NAME: usize = 64;

/// An MCP server of the repository, as saved in its session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McpServerDecl {
    pub name: String,
    pub program: String,
}

/// A tool of an MCP server, as its `tools/list` describes it.
#[derive(Debug, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

impl McpTool {
    fn parse(tool: &Value) -> Option<Self> {
        Some(Self {
            name: tool.get("name")?.as_str()?.to_string(),
            description: tool
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            schema: tool
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object"})),
        })
    }
}

/// Where a server stands, as the Conversation Tree reports it.
#[derive(Clone, Debug, PartialEq)]
pub enum McpStatus {
    Starting,
    /// Running, with this many tools.
    Ready(usize),
    Failed(String),
}

impl McpStatus {
    /// `15 tools`, `starting`, `failed`.
    pub fn label(&self) -> String {
        match self {
            McpStatus::Starting => "starting".to_string(),
            McpStatus::Ready(1) => "1 tool".to_string(),
            McpStatus::Ready(tools) => format!("{tools} tools"),
            McpStatus::Failed(_) => "failed".to_string(),
        }
    }
}

/// Called with a server's name whenever its status changes.
pub type Notify = Arc<dyn Fn(&str) + Send + Sync>;

/// The MCP servers of the repository, running for as long as Explorer does.
#[derive(Default)]
pub struct McpHost {
    root: Utf8PathBuf,
    servers: Mutex<BTreeMap<String, Arc<Server>>>,
}

impl McpHost {
    pub fn new(root: Utf8PathBuf) -> Self {
        Self {
            root,
            servers: Mutex::default(),
        }
    }

    /// Starts `decl` in the background, replacing a server of the same name.
    pub fn start(&self, decl: &McpServerDecl, notify: Notify) {
        let server = Arc::new(Server {
            decl: decl.clone(),
            root: self.root.clone(),
            phase: watch::Sender::new(Phase::Starting),
            notify,
            restarting: tokio::sync::Mutex::new(()),
        });
        let replaced = self
            .servers
            .lock()
            .insert(decl.name.clone(), Arc::clone(&server));
        if let Some(replaced) = replaced {
            replaced.phase.send_replace(Phase::Removed);
        }
        tokio::spawn(async move { server.launch().await });
    }

    /// Stops the server `name`; a call in progress keeps it until it returns.
    pub fn remove(&self, name: &str) {
        let removed = self.servers.lock().remove(name);
        if let Some(server) = removed {
            server.phase.send_replace(Phase::Removed);
        }
    }

    pub fn statuses(&self) -> Vec<(String, McpStatus)> {
        self.servers
            .lock()
            .iter()
            .map(|(name, server)| (name.clone(), server.status()))
            .collect()
    }

    pub fn status(&self, name: &str) -> Option<McpStatus> {
        self.servers.lock().get(name).map(|server| server.status())
    }

    /// The tools a turn starting now can call: those of the servers running, after
    /// waiting a little for the ones still starting.
    pub async fn snapshot(self: &Arc<Self>) -> McpTools {
        let servers: Vec<Arc<Server>> = self.servers.lock().values().cloned().collect();
        let deadline = tokio::time::Instant::now() + START_WAIT;
        let mut ready = Vec::new();
        for server in servers {
            let mut phase = server.phase.subscribe();
            let started = phase.wait_for(|phase| !matches!(phase, Phase::Starting));
            let _ = tokio::time::timeout_at(deadline, started).await;
            if let Phase::Ready { tools, .. } = &*server.phase.borrow() {
                ready.push(ServerTools {
                    decl: server.decl.clone(),
                    tools: Arc::clone(tools),
                });
            }
        }
        McpTools {
            host: Arc::clone(self),
            servers: ready,
        }
    }

    async fn call(&self, server: &str, tool: &str, arguments: Value) -> Result<String, String> {
        let found = self.servers.lock().get(server).cloned();
        let server = found.ok_or_else(|| format!("the MCP server {server} was removed"))?;
        server.call(tool, arguments).await
    }
}

/// The MCP tools of one turn, shared with its sub-agents: what the servers offered when
/// the turn started.
#[derive(Clone, Default)]
pub struct McpTools {
    host: Arc<McpHost>,
    /// Sorted by name, so the tools come in the same order from turn to turn.
    servers: Vec<ServerTools>,
}

#[derive(Clone)]
struct ServerTools {
    decl: McpServerDecl,
    tools: Arc<Vec<McpTool>>,
}

impl McpTools {
    pub fn is_tool(name: &str) -> bool {
        name.starts_with(TOOL_PREFIX)
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.servers
            .iter()
            .flat_map(|server| {
                server.tools.iter().map(|tool| ToolSpec {
                    name: format!("{TOOL_PREFIX}{}{SEPARATOR}{}", server.decl.name, tool.name),
                    description: tool.description.clone(),
                    schema: tool.schema.clone(),
                })
            })
            .collect()
    }

    /// Runs the tool `name`, `mcp__<server>__<tool>`. A failure is returned as `Err`.
    pub async fn call(&self, name: &str, arguments: Value) -> Result<String, String> {
        // Matched against each server's name: one ending with `_` defeats a split on `__`.
        let offered = self.servers.iter().find_map(|offered| {
            let tool = name
                .strip_prefix(TOOL_PREFIX)?
                .strip_prefix(offered.decl.name.as_str())?
                .strip_prefix(SEPARATOR)?;
            let known = offered.tools.iter().any(|known| known.name == tool);
            known.then_some((offered.decl.name.as_str(), tool))
        });
        let Some((server, tool)) = offered else {
            return Err(format!("unknown tool: {name}"));
        };
        if !arguments.is_object() {
            return Err(format!("the arguments must be a JSON object: {arguments}"));
        }
        self.host.call(server, tool, arguments).await
    }

    /// The `--mcp-config` of a Claude Code run, which starts the servers itself.
    pub fn claude_config(&self) -> Option<String> {
        if self.servers.is_empty() {
            return None;
        }
        let servers: Map<String, Value> = self
            .servers
            .iter()
            .map(|server| {
                let config = json!({"command": server.decl.program, "args": []});
                (server.decl.name.clone(), config)
            })
            .collect();
        Some(json!({ "mcpServers": servers }).to_string())
    }
}

enum Phase {
    Starting,
    Ready {
        client: Arc<Client>,
        tools: Arc<Vec<McpTool>>,
    },
    Failed(String),
    /// Taken out of the host: never started again, even by a call still holding it.
    Removed,
}

struct Server {
    decl: McpServerDecl,
    root: Utf8PathBuf,
    phase: watch::Sender<Phase>,
    notify: Notify,
    restarting: tokio::sync::Mutex<()>,
}

impl Server {
    fn status(&self) -> McpStatus {
        match &*self.phase.borrow() {
            Phase::Starting => McpStatus::Starting,
            Phase::Ready { client, .. } if !client.alive() => {
                McpStatus::Failed("exited".to_string())
            }
            Phase::Ready { tools, .. } => McpStatus::Ready(tools.len()),
            Phase::Failed(reason) => McpStatus::Failed(reason.clone()),
            Phase::Removed => McpStatus::Failed("removed".to_string()),
        }
    }

    async fn launch(&self) {
        let mut phase = self.phase.subscribe();
        // A server removed while it starts is killed then, not once its handshake times out.
        let removed = phase.wait_for(|phase| matches!(phase, Phase::Removed));
        let started = tokio::select! {
            started = Client::start(&self.decl.program, &self.root) => started,
            _ = removed => return,
        };
        let launched = match started {
            Ok((client, mut tools)) => {
                // An endpoint refuses the whole request over one tool name it rejects.
                tools.retain(|tool| {
                    let kept = advertisable(&self.decl.name, &tool.name);
                    if !kept {
                        tracing::warn!("MCP server {}: tool {} skipped", self.decl.name, tool.name);
                    }
                    kept
                });
                Phase::Ready {
                    client: Arc::new(client),
                    tools: Arc::new(tools),
                }
            }
            // A server's error message may span lines; the notice reports it on the status bar.
            Err(reason) => Phase::Failed(reason.split_whitespace().collect::<Vec<_>>().join(" ")),
        };
        let kept = self.phase.send_if_modified(|phase| match phase {
            Phase::Removed => false,
            _ => {
                *phase = launched;
                true
            }
        });
        if kept {
            (self.notify)(&self.decl.name);
        }
    }

    async fn call(self: &Arc<Self>, tool: &str, arguments: Value) -> Result<String, String> {
        let client = match &*self.phase.borrow() {
            Phase::Ready { client, .. } if client.alive() => Some(Arc::clone(client)),
            _ => None,
        };
        let Some(client) = client else {
            return Err(self.restart(None).await);
        };
        let params = json!({"name": tool, "arguments": arguments});
        match client.request("tools/call", params, CALL_TIMEOUT).await {
            Ok(result) => format_result(&result),
            Err(Failure::Exited) => Err(self.restart(Some(client)).await),
            Err(Failure::Timeout) => Err(format!(
                "no answer from the MCP server {} after {}s",
                self.decl.name,
                CALL_TIMEOUT.as_secs()
            )),
            Err(Failure::Rpc(message)) => Err(message),
        }
    }

    /// Starts the server again after it stopped, or after `failed`, its client, lost its
    /// pipes, and tells the model what it lost.
    async fn restart(self: &Arc<Self>, failed: Option<Arc<Client>>) -> String {
        let name = &self.decl.name;
        let _restarting = self.restarting.lock().await;
        // A server still starting, like one added again under its name, is not started twice.
        let _ = self
            .phase
            .subscribe()
            .wait_for(|phase| !matches!(phase, Phase::Starting))
            .await;
        // The reader may not have seen the output close yet when a write failed.
        let lost = |client: &Arc<Client>| {
            !client.alive()
                || failed
                    .as_ref()
                    .is_some_and(|failed| Arc::ptr_eq(failed, client))
        };
        let stopped = self.phase.send_if_modified(|phase| match phase {
            Phase::Ready { client, .. } if !lost(client) => false,
            Phase::Removed => false,
            _ => {
                *phase = Phase::Starting;
                true
            }
        });
        drop(failed);
        if stopped {
            (self.notify)(name);
            let server = Arc::clone(self);
            // Detached: a cancelled turn must not leave the server starting forever.
            let _ = tokio::spawn(async move { server.launch().await }).await;
        }
        match &*self.phase.borrow() {
            Phase::Removed => format!("the MCP server {name} was removed"),
            Phase::Failed(reason) => {
                format!("the MCP server {name} had stopped and could not be restarted: {reason}")
            }
            _ => format!(
                "the MCP server {name} had stopped and was restarted: whatever it held is lost \
                 (for example, attach again), then retry the call"
            ),
        }
    }
}

/// Whether the endpoints accept `mcp__<server>__<tool>` as a tool name.
fn advertisable(server: &str, tool: &str) -> bool {
    let allowed = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    let length = TOOL_PREFIX.len() + server.len() + SEPARATOR.len() + tool.len();
    !tool.is_empty() && tool.chars().all(allowed) && length <= MAX_TOOL_NAME
}

/// Why a request got no result.
enum Failure {
    /// The server closed its output or its input.
    Exited,
    Timeout,
    /// The server answered with an error.
    Rpc(String),
}

impl Failure {
    fn text(self, what: &str) -> String {
        match self {
            Failure::Exited => format!("exited during {what}"),
            Failure::Timeout => format!("no answer to {what}"),
            Failure::Rpc(message) => format!("{what} failed: {message}"),
        }
    }
}

type Waiters = Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>;

/// A running MCP server process, spoken to over its standard input and output.
struct Client {
    pid: Option<u32>,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    waiters: Arc<Waiters>,
    alive: Arc<AtomicBool>,
    next_id: AtomicU64,
    reader: JoinHandle<()>,
    _child: Child,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.reader.abort();
        if let Some(pid) = self.pid {
            // SAFETY: plain syscall on a process group this process created.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

/// Removes a request's waiter when the request ends, however it ends: a reply coming
/// after a cancelled call then finds no one and is dropped.
struct Waiting<'a> {
    waiters: &'a Waiters,
    id: u64,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.waiters.lock().remove(&self.id);
    }
}

impl Client {
    /// Spawns `program` in `root`, goes through the handshake and lists its tools.
    async fn start(program: &str, root: &Utf8Path) -> Result<(Self, Vec<McpTool>), String> {
        let mut child = tokio::process::Command::new(program)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdin = Arc::new(tokio::sync::Mutex::new(
            child.stdin.take().expect("stdin is piped"),
        ));
        let stdout = child.stdout.take().expect("stdout is piped");
        let waiters = Arc::new(Waiters::default());
        let alive = Arc::new(AtomicBool::new(true));
        let reader = tokio::spawn(read_messages(
            stdout,
            Arc::clone(&stdin),
            Arc::clone(&waiters),
            Arc::clone(&alive),
        ));
        let client = Self {
            pid: child.id(),
            stdin,
            waiters,
            alive,
            next_id: AtomicU64::new(1),
            reader,
            _child: child,
        };
        let tools = client.handshake().await?;
        Ok((client, tools))
    }

    async fn handshake(&self) -> Result<Vec<McpTool>, String> {
        let initialize = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "explorer", "version": env!("CARGO_PKG_VERSION")}
        });
        self.request("initialize", initialize, START_TIMEOUT)
            .await
            .map_err(|failure| failure.text("initialize"))?;
        let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        if !matches!(write_message(&self.stdin, initialized).await, Ok(Ok(()))) {
            return Err(Failure::Exited.text("initialize"));
        }
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let page = self
                .request("tools/list", params, START_TIMEOUT)
                .await
                .map_err(|failure| failure.text("tools/list"))?;
            let listed = page.get("tools").and_then(Value::as_array);
            tools.extend(listed.into_iter().flatten().filter_map(McpTool::parse));
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
    }

    fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, Failure> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply_tx, reply_rx) = oneshot::channel();
        self.waiters.lock().insert(id, reply_tx);
        let _waiting = Waiting {
            waiters: &self.waiters,
            id,
        };
        // The reader clears the waiters after marking the client dead.
        if !self.alive() {
            return Err(Failure::Exited);
        }
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        // A server that stops reading its input must not hang the call past its timeout.
        let exchange = async {
            if !matches!(write_message(&self.stdin, message).await, Ok(Ok(()))) {
                return Err(Failure::Exited);
            }
            match reply_rx.await {
                Err(_) => Err(Failure::Exited),
                Ok(reply) => reply.map_err(Failure::Rpc),
            }
        };
        tokio::time::timeout(timeout, exchange)
            .await
            .unwrap_or(Err(Failure::Timeout))
    }
}

/// Writes `message` as one line in a task of its own: a call dropped midway would leave
/// half a line on the pipe, and the next message glued to it.
fn write_message(
    stdin: &Arc<tokio::sync::Mutex<ChildStdin>>,
    message: Value,
) -> JoinHandle<std::io::Result<()>> {
    let mut line = message.to_string();
    line.push('\n');
    let stdin = Arc::clone(stdin);
    tokio::spawn(async move {
        let mut stdin = stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await
    })
}

/// Hands each reply to the request waiting for it and answers the server's own
/// requests; marks the client dead once the server's output closes.
async fn read_messages(
    stdout: ChildStdout,
    stdin: Arc<tokio::sync::Mutex<ChildStdin>>,
    waiters: Arc<Waiters>,
    alive: Arc<AtomicBool>,
) {
    // Split on bytes: a stray line that is not UTF-8 must not end the reader as `lines` would.
    let mut lines = BufReader::new(stdout).split(b'\n');
    while let Ok(Some(line)) = lines.next_segment().await {
        let Ok(message) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let id = message.get("id").cloned();
        match (message.get("method").and_then(Value::as_str), id) {
            (Some(method), Some(id)) => {
                let answer = match method {
                    "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
                    _ => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32601, "message": format!("{method} is not supported")}
                    }),
                };
                // Not awaited: a server that stops reading must not stop the reader too.
                drop(write_message(&stdin, answer));
            }
            (Some(_), None) => {}
            (None, id) => {
                let waiter = id
                    .as_ref()
                    .and_then(Value::as_u64)
                    .and_then(|id| waiters.lock().remove(&id));
                if let Some(waiter) = waiter {
                    let reply = match message.get("error") {
                        Some(error) => Err(error
                            .get("message")
                            .and_then(Value::as_str)
                            .map_or_else(|| error.to_string(), str::to_string)),
                        None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = waiter.send(reply);
                }
            }
        }
    }
    alive.store(false, Ordering::SeqCst);
    waiters.lock().clear();
}

/// The text the model gets from a `tools/call` result: its text blocks, or else its
/// structured content; an `isError` result is an `Err`.
fn format_result(result: &Value) -> Result<String, String> {
    let blocks = result.get("content").and_then(Value::as_array);
    let mut parts: Vec<String> = blocks
        .into_iter()
        .flatten()
        .map(|block| match block.get("type").and_then(Value::as_str) {
            Some("text") => block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            kind => {
                let kind = block.get("mimeType").and_then(Value::as_str).or(kind);
                format!("[{} omitted]", kind.unwrap_or("content"))
            }
        })
        .collect();
    if parts.is_empty()
        && let Some(structured) = result.get("structuredContent")
    {
        parts.push(structured.to_string());
    }
    let mut text = truncate_middle(&parts.join("\n"), MAX_OUTPUT_BYTES);
    if text.is_empty() {
        text.push_str("(no output)");
    }
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        Err(text)
    } else {
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Answers the handshake, slowly once `slow-start` exists in its directory (writing its
    /// pid to `started`), lists four tools plus one whose name no endpoint accepts, and
    /// acts on calls by tool name, `echo` only with an object for arguments and after a line
    /// that is not UTF-8.
    const FAKE_SERVER: &str = r#"#!/bin/sh
reply() { printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$1"; }
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/^{[^{]*"id":\([0-9]*\).*/\1/p')
  case "$line" in
  *'"method":"initialize"'*) [ -e slow-start ] && { echo $$ > started; sleep 1; }; reply '{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"0"}}' ;;
  *'"method":"tools/list"'*) reply '{"tools":[{"name":"echo","description":"Says hi","inputSchema":{"type":"object","properties":{}}},{"name":"slow"},{"name":"die"},{"name":"broken"},{"name":"dotted.name"}]}' ;;
  *'"arguments":{'*'"name":"echo"'*) printf '\377\n'; reply '{"content":[{"type":"text","text":"hi"}]}' ;;
  *'"name":"slow"'*) sleep 1; reply '{"content":[{"type":"text","text":"late"}]}' ;;
  *'"name":"broken"'*) reply '{"content":[{"type":"text","text":"not attached"}],"isError":true}' ;;
  *'"name":"die"'*) exit 0 ;;
  esac
done
"#;

    fn fake_server() -> (tempfile::TempDir, McpServerDecl) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("fake-mcp");
        std::fs::write(&path, FAKE_SERVER).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A child another test forks while the script is open for writing causes ETXTBSY.
        while let Err(error) = std::process::Command::new(&path)
            .stdin(Stdio::null())
            .status()
        {
            assert_eq!(error.raw_os_error(), Some(libc::ETXTBSY), "{error}");
            std::thread::sleep(Duration::from_millis(10));
        }
        let decl = McpServerDecl {
            name: "fake".to_string(),
            program: path.to_str().unwrap().to_string(),
        };
        (tmp, decl)
    }

    fn host(tmp: &tempfile::TempDir) -> Arc<McpHost> {
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        Arc::new(McpHost::new(root))
    }

    fn no_notify() -> Notify {
        Arc::new(|_| {})
    }

    #[tokio::test]
    async fn a_started_server_lists_its_tools_under_prefixed_names() {
        let (tmp, decl) = fake_server();
        let host = host(&tmp);
        let missing = McpServerDecl {
            name: "missing".to_string(),
            program: "/nonexistent/mcp".to_string(),
        };
        host.start(&decl, no_notify());
        host.start(&missing, no_notify());
        let tools = host.snapshot().await;

        let names: Vec<String> = tools.specs().into_iter().map(|spec| spec.name).collect();
        assert_eq!(
            names,
            [
                "mcp__fake__echo",
                "mcp__fake__slow",
                "mcp__fake__die",
                "mcp__fake__broken"
            ]
        );
        assert_eq!(tools.specs()[0].description, "Says hi");
        assert_eq!(host.status("fake"), Some(McpStatus::Ready(4)));
        assert!(matches!(host.status("missing"), Some(McpStatus::Failed(_))));
        assert_eq!(
            tools.call("mcp__fake__echo", json!({})).await,
            Ok("hi".into())
        );
        assert_eq!(
            tools.call("mcp__fake__broken", json!({})).await,
            Err("not attached".into())
        );
        assert_eq!(
            tools.call("mcp__fake__nope", json!({})).await,
            Err("unknown tool: mcp__fake__nope".into())
        );
        assert!(tools.call("mcp__fake__echo", json!("{oops")).await.is_err());
        let config: Value = serde_json::from_str(&tools.claude_config().unwrap()).unwrap();
        assert_eq!(
            config,
            json!({"mcpServers": {"fake": {"command": decl.program, "args": []}}})
        );
    }

    #[tokio::test]
    async fn a_server_name_ending_with_an_underscore_routes_its_calls() {
        let (tmp, mut decl) = fake_server();
        decl.name = "fake_".to_string();
        let host = host(&tmp);
        host.start(&decl, no_notify());
        let tools = host.snapshot().await;
        assert_eq!(
            tools.call("mcp__fake___echo", json!({})).await,
            Ok("hi".into())
        );
    }

    #[tokio::test]
    async fn a_reply_to_a_cancelled_call_is_dropped() {
        let (tmp, decl) = fake_server();
        let host = host(&tmp);
        host.start(&decl, no_notify());
        let tools = host.snapshot().await;
        let cancelled = tokio::time::timeout(
            Duration::from_millis(100),
            tools.call("mcp__fake__slow", json!({})),
        )
        .await;
        assert!(cancelled.is_err());
        assert_eq!(
            tools.call("mcp__fake__echo", json!({})).await,
            Ok("hi".into())
        );
    }

    #[tokio::test]
    async fn a_server_that_died_is_restarted_and_the_model_told() {
        let (tmp, decl) = fake_server();
        let host = host(&tmp);
        let changes = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&changes);
        host.start(
            &decl,
            Arc::new(move |_| {
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let tools = host.snapshot().await;
        let died = tools.call("mcp__fake__die", json!({})).await.unwrap_err();
        assert!(died.contains("was restarted"), "{died}");
        assert_eq!(
            tools.call("mcp__fake__echo", json!({})).await,
            Ok("hi".into())
        );
        // Started, then restarting and restarted.
        assert_eq!(changes.load(Ordering::SeqCst), 3);
        let held = host.servers.lock().get("fake").cloned().unwrap();
        host.remove("fake");
        assert_eq!(
            tools.call("mcp__fake__echo", json!({})).await,
            Err("the MCP server fake was removed".into())
        );
        // A call that still holds the server never starts it again.
        assert_eq!(
            held.call("echo", json!({})).await,
            Err("the MCP server fake was removed".into())
        );
        assert_eq!(changes.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_restart_outlives_the_call_that_started_it() {
        let (tmp, decl) = fake_server();
        let host = host(&tmp);
        host.start(&decl, no_notify());
        let tools = host.snapshot().await;
        std::fs::write(tmp.path().join("slow-start"), "").unwrap();
        let call = tokio::spawn(async move { tools.call("mcp__fake__die", json!({})).await });
        let restarting = async {
            while host.status("fake") != Some(McpStatus::Starting) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), restarting)
            .await
            .expect("the dead server is restarted");
        call.abort();
        assert!(call.await.unwrap_err().is_cancelled());
        host.snapshot().await;
        assert_eq!(host.status("fake"), Some(McpStatus::Ready(4)));
    }

    #[tokio::test]
    async fn a_server_removed_while_it_starts_is_killed_at_once() {
        let (tmp, decl) = fake_server();
        let host = host(&tmp);
        std::fs::write(tmp.path().join("slow-start"), "").unwrap();
        host.start(&decl, no_notify());
        let started = tmp.path().join("started");
        let pid = async {
            loop {
                if let Ok(pid) = std::fs::read_to_string(&started)
                    && let Ok(pid) = pid.trim().parse::<u32>()
                {
                    return pid;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        let pid = tokio::time::timeout(Duration::from_secs(5), pid)
            .await
            .expect("the server starts");
        host.remove("fake");
        // Gone or a zombie: the handshake would have ended a second later.
        let dead = || {
            std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
                stat.rsplit(") ")
                    .next()
                    .is_some_and(|rest| rest.starts_with('Z'))
            })
        };
        let killed = async {
            while !dead() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        tokio::time::timeout(Duration::from_millis(500), killed)
            .await
            .expect("the removed server is killed");
    }

    #[test]
    fn results_keep_text_and_stand_in_for_the_rest() {
        let result = json!({"content": [
            {"type": "text", "text": "tree"},
            {"type": "image", "data": "iVBOR", "mimeType": "image/png"},
            {"type": "audio", "data": "x"}
        ]});
        assert_eq!(
            format_result(&result),
            Ok("tree\n[image/png omitted]\n[audio omitted]".into())
        );
        let structured = json!({"content": [], "structuredContent": {"state": "idle"}});
        assert_eq!(format_result(&structured), Ok(r#"{"state":"idle"}"#.into()));
        assert_eq!(format_result(&json!({})), Ok("(no output)".into()));
        let failed = json!({"content": [{"type": "text", "text": "no app"}], "isError": true});
        assert_eq!(format_result(&failed), Err("no app".into()));
    }

    #[test]
    fn tool_names_the_endpoints_refuse_are_not_advertised() {
        assert!(advertisable("egui", "click"));
        assert!(!advertisable("egui", "dotted.name"));
        assert!(!advertisable("egui", &"a".repeat(MAX_TOOL_NAME)));
    }

    #[test]
    fn long_results_are_cut_in_the_middle() {
        let text = "a".repeat(MAX_OUTPUT_BYTES) + &"b".repeat(MAX_OUTPUT_BYTES);
        let result = json!({"content": [{"type": "text", "text": text}]});
        let cut = format_result(&result).unwrap();
        assert!(cut.len() < MAX_OUTPUT_BYTES + 100);
        assert!(cut.starts_with('a') && cut.ends_with('b'));
        assert!(cut.contains("bytes omitted"));
    }
}
