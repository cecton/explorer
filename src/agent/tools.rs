use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

const READ_DEFAULT_LINES: usize = 2000;
const READ_MAX_BYTES: usize = 30_000;
const READ_MAX_LINE_CHARS: usize = 2000;
const GLOB_MAX_PATHS: usize = 500;
const GREP_MAX_MATCHES: usize = 200;
const GREP_MAX_LINE_CHARS: usize = 300;
const SHELL_DEFAULT_TIMEOUT_SECS: u64 = 120;
const SHELL_MAX_TIMEOUT_SECS: u64 = 600;
const SHELL_MAX_OUTPUT_BYTES: usize = 30_000;
const DESCRIBE_MAX_CHARS: usize = 200;

/// What the tools of one turn act on.
pub struct ToolContext {
    pub root: camino::Utf8PathBuf,
    pub tree: Arc<parking_lot::RwLock<crate::conversation::ConversationTree>>,
    /// The node whose turn is running.
    pub node: crate::conversation::NodeId,
    /// What the turn lends to its sub-agents; `None` for a sub-agent, which cannot
    /// call one in turn.
    pub subagents: Option<crate::agent::subagent::Subagents>,
    /// Background commands of the turn, shared with its sub-agents.
    pub background: Arc<Background>,
    /// Tools of the MCP servers running when the turn started.
    pub mcp: crate::agent::mcp::McpTools,
}

/// The process groups of a turn's background commands, killed when the turn ends.
#[derive(Default)]
pub struct Background {
    // Never waited for: a zombie leader keeps the group id from being reused.
    leaders: parking_lot::Mutex<Vec<tokio::process::Child>>,
}

impl Drop for Background {
    fn drop(&mut self) {
        for leader in self.leaders.get_mut().iter() {
            if let Some(pid) = leader.id() {
                // SAFETY: plain syscall on a process group this process created.
                unsafe {
                    libc::killpg(pid as libc::pid_t, libc::SIGKILL);
                }
            }
        }
    }
}

/// The tools advertised to the model: the `agent` tool only when sub-agents can run,
/// then the tools of the MCP servers.
pub fn specs(context: &ToolContext) -> Vec<crate::agent::llm::ToolSpec> {
    let mut specs = base_specs();
    if let Some(subagents) = &context.subagents {
        specs.push(crate::agent::subagent::spec(&subagents.config.roles));
    }
    specs.extend(context.mcp.specs());
    specs
}

fn base_specs() -> Vec<crate::agent::llm::ToolSpec> {
    use crate::agent::llm::ToolSpec;
    vec![
        ToolSpec {
            name: "conversation".into(),
            description: "Consult the conversation tree of this repository: every \
                          message the user ever sent here, with the agent's replies. \
                          `toc` lists conversations, or with `node` shows one page of \
                          the tree around that node; `read` returns a node's full \
                          prompt and reply; `commit` finds the node that produced a \
                          commit."
                .into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["toc", "read", "commit"]},
                    "node": {"type": "string", "description": "Node id, for `toc` and `read`"},
                    "page": {"type": "integer", "description": "Page of the list of conversations, for `toc` without a node"},
                    "hash": {"type": "string", "description": "Commit hash, for `commit`"}
                },
                "required": ["action"]
            }),
        },
        ToolSpec {
            name: "read".into(),
            description: "Read a text file of the repository. Lines are returned prefixed \
                          with their number. A file too long to fit is cut at a line \
                          boundary, and the last line says where to resume. A line longer \
                          than 2000 characters is truncated; use grep to find content \
                          inside such a line."
                .into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the repository root"},
                    "offset": {"type": "integer", "description": "First line to return, starting at 1"},
                    "limit": {"type": "integer", "description": "Maximum number of lines (default and maximum 2000)"}
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "format_markdown".into(),
            description: "Format markdown files in place with Explorer's formatter: \
                          tables aligned, lists and fences normalized, the line breaks \
                          of prose kept. Never format markdown by hand. Without `paths`, \
                          formats the markdown files changed in the working tree; a directory is \
                          formatted recursively. \
                          Returns the files it rewrote."
                .into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Markdown files or directories (formatted recursively), relative to the repository root"
                    }
                }
            }),
        },
        ToolSpec {
            name: "glob".into(),
            description: "List the files of the repository whose path matches a glob \
                          pattern such as `src/**/*.rs`."
                .into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Glob with *, ** and ?"}
                },
                "required": ["pattern"]
            }),
        },
        ToolSpec {
            name: "grep".into(),
            description: "Search file contents with a regular expression. Returns \
                          `path:line: text` for each match."
                .into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regular expression"},
                    "path": {"type": "string", "description": "File or directory to search in (default: whole repository)"},
                    "glob": {"type": "string", "description": "Only search files matching this glob"}
                },
                "required": ["pattern"]
            }),
        },
        ToolSpec {
            name: "shell".into(),
            description: "Run a shell command with `sh -c` in the repository root and \
                          return its output and exit code. With `background`, return at \
                          once with the pid and the file receiving the output, for a \
                          command that keeps running, such as an app to test; it is \
                          killed when the turn ends."
                .into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout": {"type": "integer", "description": "Seconds before the command is killed (default 120, maximum 600); ignored in the background"},
                    "background": {"type": "boolean", "description": "Run without waiting for the command to end (default false)"}
                },
                "required": ["command"]
            }),
        },
        ToolSpec {
            name: "write".into(),
            description: "Create a file or replace its whole content. Missing parent \
                          directories are created."
                .into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the repository root"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        },
        ToolSpec {
            name: "edit".into(),
            description: "Replace one exact occurrence of `old_string` in a file with \
                          `new_string`. Fails when the text is absent or appears more \
                          than once; include enough surrounding lines to make it unique."
                .into(),
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the repository root"},
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"}
                },
                "required": ["path", "old_string", "new_string"]
            }),
        },
    ]
}

/// Short description of a call for the status line, e.g. `shell cargo check`.
pub fn describe(call: &crate::agent::ToolCall) -> String {
    if call.name == crate::agent::subagent::TOOL {
        return crate::agent::subagent::describe(&call.arguments);
    }
    let arg = |key: &str| call.arguments.get(key).and_then(serde_json::Value::as_str);
    let detail = match (arg("pattern"), arg("command"), arg("path")) {
        (Some(pattern), _, Some(path)) => format!("{pattern} in {path}"),
        (Some(pattern), _, None) => pattern.to_string(),
        (None, Some(command), _) => command.to_string(),
        (None, None, Some(path)) => path.to_string(),
        (None, None, None) => arguments_detail(&call.arguments),
    };
    let detail = detail.lines().next().unwrap_or("");
    format!("{} {detail}", call.name).trim_end().to_string()
}

/// The arguments of a call as compact JSON, cut short; empty when there are none.
pub(super) fn arguments_detail(arguments: &serde_json::Value) -> String {
    if arguments.is_null() || arguments.as_object().is_some_and(|map| map.is_empty()) {
        return String::new();
    }
    let text = arguments.to_string();
    match text.char_indices().nth(DESCRIBE_MAX_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text,
    }
}

/// Runs one tool call. A failure is returned as text for the model, never raised.
pub async fn run(context: &ToolContext, call: &crate::agent::ToolCall) -> String {
    use crate::agent::mcp::McpTools;
    let args = call.arguments.clone();
    let root = context.root.as_path();
    let result = match call.name.as_str() {
        "conversation" => {
            let tree = Arc::clone(&context.tree);
            let node = context.node.clone();
            blocking(move || conversation(&tree, &node, &args)).await
        }
        "read" => read(root, &args),
        "write" => write(root, &args),
        "edit" => edit(root, &args),
        "format_markdown" => {
            let root = root.to_owned();
            blocking(move || format_markdown(&root, &args)).await
        }
        "glob" => {
            let root = root.to_owned();
            blocking(move || glob(&root, &args)).await
        }
        "grep" => {
            let root = root.to_owned();
            blocking(move || grep(&root, &args)).await
        }
        "shell" if args.get("background").and_then(serde_json::Value::as_bool) == Some(true) => {
            let dir = context.tree.read().node_dir(&context.node);
            match dir {
                Some(dir) => shell_background(root, &dir, &context.background, &args),
                None => Err("this turn has no node directory for the output".to_string()),
            }
        }
        "shell" => shell(root, &args).await,
        name if McpTools::is_tool(name) => context.mcp.call(name, args).await,
        other => Err(format!("unknown tool: {other}")),
    };
    result.unwrap_or_else(|error| format!("error: {error}"))
}

async fn blocking(
    work: impl FnOnce() -> Result<String, String> + Send + 'static,
) -> Result<String, String> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())?
}

fn str_arg<'a>(args: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("missing string argument `{key}`"))
}

fn usize_arg(args: &serde_json::Value, key: &str) -> Option<usize> {
    args.get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

fn format_markdown(root: &camino::Utf8Path, args: &serde_json::Value) -> Result<String, String> {
    let relatives = match args.get("paths").and_then(serde_json::Value::as_array) {
        Some(paths) if !paths.is_empty() => paths
            .iter()
            .map(|path| {
                let path = path.as_str().ok_or("`paths` holds a non-string")?;
                let resolved = crate::util::fs::resolve_in(root, path)?;
                resolved
                    .strip_prefix(root)
                    .map(camino::Utf8Path::to_path_buf)
                    .map_err(|_| format!("{path} is outside the repository"))
            })
            .collect::<Result<Vec<camino::Utf8PathBuf>, String>>()?,
        _ => crate::git::working_tree_changes(root)?,
    };
    let relatives = crate::markdown_format::expand_directories(root, relatives)?;
    let changed = crate::markdown_format::format_files(root, &relatives)?;
    if changed.is_empty() {
        Ok("nothing to format".to_string())
    } else {
        let names: Vec<&str> = changed.iter().map(|path| path.as_str()).collect();
        Ok(format!("formatted:\n{}", names.join("\n")))
    }
}

fn read(root: &camino::Utf8Path, args: &serde_json::Value) -> Result<String, String> {
    let path = crate::util::fs::resolve_in(root, str_arg(args, "path")?)?;
    let content = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let offset = usize_arg(args, "offset").unwrap_or(1).max(1);
    let limit = usize_arg(args, "limit")
        .unwrap_or(READ_DEFAULT_LINES)
        .clamp(1, READ_DEFAULT_LINES);
    let total = content.lines().count();
    if offset > total.max(1) {
        return Ok(format!(
            "[{total} lines; offset {offset} is past the end]\n"
        ));
    }
    let mut out = String::new();
    let mut shown_until = offset - 1;
    for (number, line) in content.lines().enumerate().skip(offset - 1).take(limit) {
        let rendered = render_line(number + 1, line);
        if !out.is_empty() && out.len() + rendered.len() > READ_MAX_BYTES {
            break;
        }
        out.push_str(&rendered);
        shown_until = number + 1;
    }
    if shown_until < total {
        out.push_str(&format!(
            "[lines {offset}-{shown_until} of {total}; use offset {} to read more]\n",
            shown_until + 1
        ));
    }
    Ok(out)
}

fn render_line(number: usize, line: &str) -> String {
    match line
        .char_indices()
        .nth(READ_MAX_LINE_CHARS)
        .map(|(cut, _)| cut)
    {
        Some(cut) => format!(
            "{number}\t{}... (line truncated to {READ_MAX_LINE_CHARS} chars)\n",
            &line[..cut]
        ),
        None => format!("{number}\t{line}\n"),
    }
}

fn conversation(
    tree: &parking_lot::RwLock<crate::conversation::ConversationTree>,
    current: &crate::conversation::NodeId,
    args: &serde_json::Value,
) -> Result<String, String> {
    use crate::conversation::NodeId;
    let node_arg = || str_arg(args, "node").map(crate::conversation::NodeId::new);
    let unknown = |node: &NodeId| format!("unknown node: {}", node.as_str());
    match str_arg(args, "action")? {
        "toc" => match args.get("node").and_then(serde_json::Value::as_str) {
            Some(node) => {
                let node = crate::conversation::NodeId::new(node);
                tree.read()
                    .toc_node(&node, current)
                    .ok_or_else(|| unknown(&node))
            }
            None => Ok(tree
                .read()
                .toc_roots(usize_arg(args, "page").unwrap_or(1), current)),
        },
        "read" => {
            let node = node_arg()?;
            let dir = tree.read().node_dir(&node).ok_or_else(|| unknown(&node))?;
            let (prompt, reply) = crate::conversation::read_prompt_and_reply(&dir);
            Ok(format!(
                "# Prompt\n\n{}\n\n# Reply\n\n{}\n",
                prompt.trim(),
                reply.trim()
            ))
        }
        "commit" => {
            let hash = str_arg(args, "hash")?;
            let tree = tree.read();
            let owner = tree
                .node_of_commit(hash)
                .ok_or_else(|| format!("no node recorded commit {hash}"))?;
            tree.toc_node(owner, current).ok_or_else(|| unknown(owner))
        }
        other => Err(format!("unknown action: {other}")),
    }
}

fn write(root: &camino::Utf8Path, args: &serde_json::Value) -> Result<String, String> {
    let path = crate::util::fs::resolve_in(root, str_arg(args, "path")?)?;
    let content = str_arg(args, "content")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{parent}: {e}"))?;
    }
    std::fs::write(&path, content).map_err(|e| format!("{path}: {e}"))?;
    Ok(format!("wrote {} bytes\n", content.len()))
}

fn edit(root: &camino::Utf8Path, args: &serde_json::Value) -> Result<String, String> {
    let path = crate::util::fs::resolve_in(root, str_arg(args, "path")?)?;
    let old = str_arg(args, "old_string")?;
    let new = str_arg(args, "new_string")?;
    if old.is_empty() {
        return Err("old_string must not be empty".to_string());
    }
    let content = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    match content.matches(old).count() {
        0 => Err("old_string was not found in the file".to_string()),
        1 => {
            std::fs::write(&path, content.replacen(old, new, 1))
                .map_err(|e| format!("{path}: {e}"))?;
            Ok("edited\n".to_string())
        }
        count => Err(format!(
            "old_string appears {count} times; add surrounding lines to make it unique"
        )),
    }
}

/// Translates a glob into an anchored regex: `**` crosses directories, `*` and `?`
/// do not.
fn glob_regex(pattern: &str) -> Result<regex::Regex, String> {
    let mut out = String::from("^");
    let mut rest = pattern;
    while let Some(ch) = rest.chars().next() {
        if let Some(tail) = rest.strip_prefix("**/") {
            out.push_str("(?:.*/)?");
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("**") {
            out.push_str(".*");
            rest = tail;
        } else {
            match ch {
                '*' => out.push_str("[^/]*"),
                '?' => out.push_str("[^/]"),
                other => out.push_str(&regex::escape(other.encode_utf8(&mut [0; 4]))),
            }
            rest = &rest[ch.len_utf8()..];
        }
    }
    out.push('$');
    regex::Regex::new(&out).map_err(|e| e.to_string())
}

/// Repository files as paths relative to the root, sorted.
fn repo_files(root: &camino::Utf8Path) -> Vec<camino::Utf8PathBuf> {
    let mut files: Vec<camino::Utf8PathBuf> = crate::loader::walk_repo_files(root)
        .into_iter()
        .filter_map(|path| camino::Utf8PathBuf::from_path_buf(path).ok())
        .filter_map(|path| path.strip_prefix(root).ok().map(camino::Utf8Path::to_owned))
        .collect();
    files.sort();
    files
}

fn glob(root: &camino::Utf8Path, args: &serde_json::Value) -> Result<String, String> {
    let matcher = glob_regex(str_arg(args, "pattern")?)?;
    let matches: Vec<camino::Utf8PathBuf> = repo_files(root)
        .into_iter()
        .filter(|path| matcher.is_match(path.as_str()))
        .collect();
    if matches.is_empty() {
        return Ok("no files match\n".to_string());
    }
    let mut out = String::new();
    for path in matches.iter().take(GLOB_MAX_PATHS) {
        out.push_str(path.as_str());
        out.push('\n');
    }
    if matches.len() > GLOB_MAX_PATHS {
        out.push_str(&format!(
            "[{} more files not shown]\n",
            matches.len() - GLOB_MAX_PATHS
        ));
    }
    Ok(out)
}

fn grep(root: &camino::Utf8Path, args: &serde_json::Value) -> Result<String, String> {
    let pattern = regex::Regex::new(str_arg(args, "pattern")?).map_err(|e| e.to_string())?;
    let scope = match args.get("path").and_then(serde_json::Value::as_str) {
        Some(path) => crate::util::fs::resolve_in(root, path)?
            .strip_prefix(root)
            .map(camino::Utf8Path::to_owned)
            .unwrap_or_default(),
        None => camino::Utf8PathBuf::new(),
    };
    let filter = match args.get("glob").and_then(serde_json::Value::as_str) {
        Some(glob) => Some(glob_regex(glob)?),
        None => None,
    };
    let mut out = String::new();
    let mut found = 0;
    'files: for path in repo_files(root) {
        if !path.starts_with(&scope) {
            continue;
        }
        if filter
            .as_ref()
            .is_some_and(|filter| !filter.is_match(path.as_str()))
        {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(root.join(&path)) else {
            continue;
        };
        for (number, line) in content.lines().enumerate() {
            if !pattern.is_match(line) {
                continue;
            }
            if found == GREP_MAX_MATCHES {
                out.push_str("[more matches not shown; narrow the search]\n");
                break 'files;
            }
            found += 1;
            let shown: String = line.chars().take(GREP_MAX_LINE_CHARS).collect();
            out.push_str(&format!("{path}:{}: {shown}\n", number + 1));
        }
    }
    if found == 0 {
        return Ok("no matches\n".to_string());
    }
    Ok(out)
}

/// Kills a command's whole process group unless disarmed: covers timeouts and a
/// cancelled turn dropping the future mid-command.
struct GroupKiller(Option<u32>);

impl Drop for GroupKiller {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            // SAFETY: plain syscall on a process group this process created.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

/// Keeps the head and the tail of an output that is too long.
pub(super) fn truncate_middle(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let half = max_bytes / 2;
    let mut head_end = half;
    while !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len() - half;
    while !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!(
        "{}\n[... {} bytes omitted ...]\n{}",
        &text[..head_end],
        tail_start - head_end,
        &text[tail_start..]
    )
}

async fn shell(root: &camino::Utf8Path, args: &serde_json::Value) -> Result<String, String> {
    let command = str_arg(args, "command")?;
    let timeout = args
        .get("timeout")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(SHELL_DEFAULT_TIMEOUT_SECS)
        .clamp(1, SHELL_MAX_TIMEOUT_SECS);

    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("failed to start sh: {e}"))?;
    let mut killer = GroupKiller(child.id());

    let waited = tokio::time::timeout(Duration::from_secs(timeout), child.wait_with_output()).await;
    let Ok(output) = waited else {
        return Ok(format!("command killed after {timeout}s timeout\n"));
    };
    killer.0 = None;
    let output = output.map_err(|e| e.to_string())?;

    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&stderr);
    }
    let mut text = truncate_middle(&text, SHELL_MAX_OUTPUT_BYTES);
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    match output.status.code() {
        Some(0) => {}
        Some(code) => text.push_str(&format!("exit code: {code}\n")),
        None => text.push_str("terminated by a signal\n"),
    }
    if text.is_empty() {
        text.push_str("(no output)\n");
    }
    Ok(text)
}

/// Starts a command that outlives the call, its output going to
/// `<node dir>/processes/<n>.log`.
fn shell_background(
    root: &camino::Utf8Path,
    node_dir: &camino::Utf8Path,
    background: &Background,
    args: &serde_json::Value,
) -> Result<String, String> {
    let command = str_arg(args, "command")?;
    let dir = node_dir.join(crate::conversation::PROCESSES_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{dir}: {e}"))?;
    let mut index = 1;
    let (log, file) = loop {
        let log = dir.join(format!("{index}.log"));
        match std::fs::File::create_new(&log) {
            Ok(file) => break (log, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => index += 1,
            Err(e) => return Err(format!("{log}: {e}")),
        }
    };
    let stderr = file.try_clone().map_err(|e| format!("{log}: {e}"))?;
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(stderr)
        .process_group(0)
        .spawn()
        .map_err(|e| format!("failed to start sh: {e}"))?;
    let pid = child.id().expect("a child never waited for has a pid");
    background.leaders.lock().push(child);
    Ok(format!(
        "started in the background: pid {pid}, output in {log}\n\
         Read the output with the shell tool, e.g. `tail -n 50 {log}`. Stop the command \
         with `kill -- -{pid}` (its process group); it is killed when the turn ends.\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(tmp.path().canonicalize().unwrap()).unwrap();
        std::fs::create_dir_all(root.join("src/tui")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {\n    run();\n}\n").unwrap();
        std::fs::write(root.join("src/tui/app.rs"), "pub fn run() {}\n").unwrap();
        std::fs::write(root.join("README.md"), "# Demo\nrun it\n").unwrap();
        std::fs::write(root.join("target/out.rs"), "fn run() {}\n").unwrap();
        (tmp, root)
    }

    fn context(root: &camino::Utf8Path) -> ToolContext {
        use crate::agent::mcp::McpTools;
        use crate::conversation::NodeId;
        ToolContext {
            root: root.to_owned(),
            tree: Arc::default(),
            node: NodeId::new("current"),
            subagents: None,
            background: Arc::default(),
            mcp: McpTools::default(),
        }
    }

    fn call(name: &str, arguments: serde_json::Value) -> crate::agent::ToolCall {
        use crate::agent::ToolCall;
        ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments,
        }
    }

    #[test]
    fn glob_regex_distinguishes_single_and_double_star() {
        let single = glob_regex("src/*.rs").unwrap();
        assert!(single.is_match("src/main.rs"));
        assert!(!single.is_match("src/tui/app.rs"));
        let double = glob_regex("src/**/*.rs").unwrap();
        assert!(double.is_match("src/main.rs"));
        assert!(double.is_match("src/tui/app.rs"));
        assert!(!double.is_match("README.md"));
        assert!(glob_regex("a?c.txt").unwrap().is_match("abc.txt"));
        assert!(!glob_regex("a.c").unwrap().is_match("abc"));
    }

    #[tokio::test]
    async fn read_caps_output_bytes_at_a_line_boundary() {
        let (_tmp, root) = repo();
        let mut many = String::new();
        for number in 1..=200 {
            many.push_str(&format!("{number}\t{}\n", "y".repeat(1000)));
        }
        std::fs::write(root.join("many.txt"), many).unwrap();
        let out = run(
            &context(&root),
            &call("read", serde_json::json!({"path": "many.txt"})),
        )
        .await;
        assert!(out.len() <= READ_MAX_BYTES + 100, "{}", out.len());
        let footer = out.lines().last().unwrap();
        assert!(footer.starts_with("[lines 1-"), "{footer}");
        assert!(
            footer.ends_with("of 200; use offset 30 to read more]"),
            "{footer}"
        );
        assert!(
            !out.contains("bytes omitted"),
            "the middle is never spliced out"
        );
    }

    #[tokio::test]
    async fn read_cuts_a_long_line_and_says_so() {
        let (_tmp, root) = repo();
        let payload = "x".repeat(100_000);
        std::fs::write(root.join("min.json"), format!("{{\"k\":\"{payload}\"}}")).unwrap();
        let out = run(
            &context(&root),
            &call("read", serde_json::json!({"path": "min.json"})),
        )
        .await;
        assert!(out.len() < 3000, "{}", out.len());
        assert!(out.starts_with("1\t{\"k\":\"xxx"), "{out}");
        assert!(
            out.ends_with("... (line truncated to 2000 chars)\n"),
            "{out}"
        );
        assert!(!out.contains("bytes omitted"));
        assert!(!out.contains("use offset"));
    }

    #[tokio::test]
    async fn read_says_when_the_offset_is_past_the_end() {
        let (_tmp, root) = repo();
        let out = run(
            &context(&root),
            &call(
                "read",
                serde_json::json!({"path": "src/main.rs", "offset": 500}),
            ),
        )
        .await;
        assert_eq!(out, "[3 lines; offset 500 is past the end]\n");
    }

    #[tokio::test]
    async fn read_treats_a_zero_limit_as_one_line() {
        let (_tmp, root) = repo();
        let out = run(
            &context(&root),
            &call(
                "read",
                serde_json::json!({"path": "src/main.rs", "limit": 0}),
            ),
        )
        .await;
        assert_eq!(
            out,
            "1\tfn main() {\n[lines 1-1 of 3; use offset 2 to read more]\n"
        );
    }

    #[tokio::test]
    async fn read_of_an_ordinary_file_is_never_truncated() {
        let (_tmp, root) = repo();
        let out = run(
            &context(&root),
            &call("read", serde_json::json!({"path": "src/main.rs"})),
        )
        .await;
        assert_eq!(out, "1\tfn main() {\n2\t    run();\n3\t}\n");
    }

    #[tokio::test]
    async fn read_numbers_lines_and_honors_offset_and_limit() {
        let (_tmp, root) = repo();
        let all = run(
            &context(&root),
            &call("read", serde_json::json!({"path": "src/main.rs"})),
        )
        .await;
        assert_eq!(all, "1\tfn main() {\n2\t    run();\n3\t}\n");
        let part = run(
            &context(&root),
            &call(
                "read",
                serde_json::json!({"path": "src/main.rs", "offset": 2, "limit": 1}),
            ),
        )
        .await;
        assert_eq!(
            part,
            "2\t    run();\n[lines 2-2 of 3; use offset 3 to read more]\n"
        );
        let outside = run(
            &context(&root),
            &call("read", serde_json::json!({"path": "../x"})),
        )
        .await;
        assert!(outside.starts_with("error: "), "{outside}");
    }

    #[tokio::test]
    async fn glob_and_grep_skip_pruned_directories() {
        let (_tmp, root) = repo();
        let listed = run(
            &context(&root),
            &call("glob", serde_json::json!({"pattern": "**/*.rs"})),
        )
        .await;
        assert_eq!(listed, "src/main.rs\nsrc/tui/app.rs\n");

        let found = run(
            &context(&root),
            &call("grep", serde_json::json!({"pattern": "run"})),
        )
        .await;
        assert_eq!(
            found,
            "README.md:2: run it\nsrc/main.rs:2:     run();\nsrc/tui/app.rs:1: pub fn run() {}\n"
        );
        let scoped = run(
            &context(&root),
            &call(
                "grep",
                serde_json::json!({"pattern": "run", "path": "src", "glob": "**/app.rs"}),
            ),
        )
        .await;
        assert_eq!(scoped, "src/tui/app.rs:1: pub fn run() {}\n");
        let none = run(
            &context(&root),
            &call("grep", serde_json::json!({"pattern": "zzz"})),
        )
        .await;
        assert_eq!(none, "no matches\n");
    }

    #[tokio::test]
    async fn shell_reports_output_exit_code_and_timeout() {
        let (_tmp, root) = repo();
        let ok = run(
            &context(&root),
            &call("shell", serde_json::json!({"command": "echo hi; pwd"})),
        )
        .await;
        assert_eq!(ok, format!("hi\n{root}\n"));
        let failed = run(
            &context(&root),
            &call(
                "shell",
                serde_json::json!({"command": "echo oops >&2; exit 3"}),
            ),
        )
        .await;
        assert_eq!(failed, "oops\nexit code: 3\n");
        let slow = run(
            &context(&root),
            &call(
                "shell",
                serde_json::json!({"command": "sleep 5", "timeout": 1}),
            ),
        )
        .await;
        assert_eq!(slow, "command killed after 1s timeout\n");
    }

    #[tokio::test]
    async fn background_shell_returns_at_once_and_dies_with_the_turn() {
        use crate::agent::mcp::McpTools;
        use crate::conversation::ConversationTree;
        use crate::conversation::NodeId;
        let (_tmp, root) = repo();
        let store = root.join("store");
        std::fs::create_dir_all(store.join("n1")).unwrap();
        let context = ToolContext {
            root: root.clone(),
            tree: Arc::new(parking_lot::RwLock::new(ConversationTree::load(&store))),
            node: NodeId::new("n1"),
            subagents: None,
            background: Arc::default(),
            mcp: McpTools::default(),
        };
        let start = std::time::Instant::now();
        let started = run(
            &context,
            &call(
                "shell",
                serde_json::json!({"command": "echo up; exec sleep 30", "background": true}),
            ),
        )
        .await;
        assert!(start.elapsed() < Duration::from_secs(5), "{started}");
        let log = store.join("n1/processes/1.log");
        assert!(started.contains(&format!("output in {log}")), "{started}");
        let pid: u32 = started
            .split("pid ")
            .nth(1)
            .and_then(|rest| rest.split(',').next())
            .and_then(|pid| pid.parse().ok())
            .unwrap();
        // A killed child stays a zombie until tokio reaps it.
        let alive = || {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .is_ok_and(|stat| !stat.contains(") Z "))
        };
        for _ in 0..250 {
            if std::fs::read_to_string(&log).unwrap_or_default() == "up\n" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "up\n");
        assert!(alive());
        let second = call(
            "shell",
            serde_json::json!({"command": "true", "background": true}),
        );
        let second = run(&context, &second).await;
        assert!(second.contains("n1/processes/2.log"), "{second}");

        drop(context);
        for _ in 0..250 {
            if !alive() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!alive(), "the background command outlived its turn");
    }

    #[test]
    fn truncate_middle_keeps_head_and_tail() {
        let text = "a".repeat(50) + &"b".repeat(50);
        let cut = truncate_middle(&text, 20);
        assert!(cut.starts_with("aaaaaaaaaa\n[... 80 bytes omitted ...]\n"));
        assert!(cut.ends_with("bbbbbbbbbb"));
    }

    #[test]
    fn describe_shows_the_main_argument() {
        let shell = call(
            "shell",
            serde_json::json!({"command": "cargo check\necho done"}),
        );
        assert_eq!(describe(&shell), "shell cargo check");
        assert_eq!(
            describe(&call("glob", serde_json::json!({"pattern": "*.rs"}))),
            "glob *.rs"
        );
        assert_eq!(describe(&call("odd", serde_json::json!({}))), "odd");
        assert_eq!(
            describe(&call("mcp__egui__click", serde_json::json!({"id": 4}))),
            r#"mcp__egui__click {"id":4}"#
        );
        let grep = call(
            "grep",
            serde_json::json!({"pattern": "struct Recorder", "path": "src"}),
        );
        assert_eq!(describe(&grep), "grep struct Recorder in src");
        let grep = call(
            "grep",
            serde_json::json!({"pattern": "struct Recorder", "glob": "*.rs"}),
        );
        assert_eq!(describe(&grep), "grep struct Recorder");
        assert_eq!(
            describe(&call("read", serde_json::json!({"path": "src/main.rs"}))),
            "read src/main.rs"
        );
    }

    #[tokio::test]
    async fn write_creates_files_inside_the_repository_only() {
        let (_tmp, root) = repo();
        let names: Vec<String> = specs(&context(&root))
            .into_iter()
            .map(|spec| spec.name)
            .collect();
        assert_eq!(
            names,
            [
                "conversation",
                "read",
                "format_markdown",
                "glob",
                "grep",
                "shell",
                "write",
                "edit"
            ]
        );

        let write_call = call(
            "write",
            serde_json::json!({"path": "docs/new.md", "content": "one\ntwo\n"}),
        );
        assert_eq!(run(&context(&root), &write_call).await, "wrote 8 bytes\n");
        assert_eq!(
            std::fs::read_to_string(root.join("docs/new.md")).unwrap(),
            "one\ntwo\n"
        );
        let outside = call(
            "write",
            serde_json::json!({"path": "../evil", "content": "x"}),
        );
        assert!(run(&context(&root), &outside).await.starts_with("error: "));
    }

    #[tokio::test]
    async fn format_markdown_formats_the_given_files_and_refuses_outside_paths() {
        let (_tmp, root) = repo();
        std::fs::write(root.join("t.md"), "|a|b|\n|-|-|\n|1|2|\n").unwrap();
        let format_call = call(
            "format_markdown",
            serde_json::json!({"paths": ["t.md", "src/main.rs"]}),
        );
        assert_eq!(run(&context(&root), &format_call).await, "formatted:\nt.md");
        assert_eq!(
            std::fs::read_to_string(root.join("t.md")).unwrap(),
            "| a | b |\n| - | - |\n| 1 | 2 |\n"
        );
        assert_eq!(
            run(&context(&root), &format_call).await,
            "nothing to format"
        );
        let outside = call("format_markdown", serde_json::json!({"paths": ["../x.md"]}));
        assert!(run(&context(&root), &outside).await.starts_with("error: "));
    }

    #[tokio::test]
    async fn edit_replaces_exactly_one_occurrence() {
        let (_tmp, root) = repo();
        std::fs::write(root.join("a.txt"), "alpha beta alpha\n").unwrap();
        let edit_call = |old: &str, new: &str| {
            call(
                "edit",
                serde_json::json!({"path": "a.txt", "old_string": old, "new_string": new}),
            )
        };
        let ambiguous = run(&context(&root), &edit_call("alpha", "x")).await;
        assert!(ambiguous.contains("appears 2 times"), "{ambiguous}");
        let missing = run(&context(&root), &edit_call("gamma", "x")).await;
        assert_eq!(missing, "error: old_string was not found in the file");
        assert_eq!(
            run(&context(&root), &edit_call("beta", "gamma")).await,
            "edited\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "alpha gamma alpha\n"
        );
    }

    #[tokio::test]
    async fn conversation_tool_serves_pages_nodes_and_commit_lookups() {
        use crate::agent::mcp::McpTools;
        use crate::conversation::ConversationTree;
        use crate::conversation::NodeId;
        let (_tmp, root) = repo();
        let store = root.join("store");
        let node_dir = store.join("n1");
        std::fs::create_dir_all(node_dir.join("n2")).unwrap();
        std::fs::write(
            node_dir.join("metadata.md"),
            "type: commit\ncommits: abcdef0123\n",
        )
        .unwrap();
        std::fs::write(node_dir.join("title.md"), "First").unwrap();
        std::fs::write(node_dir.join("prompt.md"), "do it\n").unwrap();
        std::fs::write(node_dir.join("reply.md"), "done\n").unwrap();
        let context = ToolContext {
            root: root.clone(),
            tree: Arc::new(parking_lot::RwLock::new(ConversationTree::load(&store))),
            node: NodeId::new("n2"),
            subagents: None,
            background: Arc::default(),
            mcp: McpTools::default(),
        };
        let ask = |arguments| call("conversation", arguments);

        let roots = run(&context, &ask(serde_json::json!({"action": "toc"}))).await;
        assert!(
            roots.contains("- n1 [commit] abcdef0 First (2 nodes) <- your branch"),
            "{roots}"
        );
        let page = run(
            &context,
            &ask(serde_json::json!({"action": "toc", "node": "n1"})),
        )
        .await;
        assert!(
            page.contains("Continues with:\n- n2 [?] (no prompt) <- this turn"),
            "{page}"
        );
        let read = run(
            &context,
            &ask(serde_json::json!({"action": "read", "node": "n1"})),
        )
        .await;
        assert_eq!(read, "# Prompt\n\ndo it\n\n# Reply\n\ndone\n");
        let owner = run(
            &context,
            &ask(serde_json::json!({"action": "commit", "hash": "abcdef0"})),
        )
        .await;
        assert!(owner.starts_with("Node:\n- n1 [commit]"), "{owner}");

        let missing = run(
            &context,
            &ask(serde_json::json!({"action": "read", "node": "zz"})),
        )
        .await;
        assert_eq!(missing, "error: unknown node: zz");
        let unknown = run(
            &context,
            &ask(serde_json::json!({"action": "commit", "hash": "1234567"})),
        )
        .await;
        assert_eq!(unknown, "error: no node recorded commit 1234567");
    }
}
