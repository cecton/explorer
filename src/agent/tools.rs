use super::llm::ToolSpec;
use super::{Mode, ToolCall};
use crate::conversation::{ConversationTree, NodeId};
use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use parking_lot::RwLock;
use serde_json::{Value, json};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

const READ_DEFAULT_LINES: usize = 2000;
const GLOB_MAX_PATHS: usize = 500;
const GREP_MAX_MATCHES: usize = 200;
const GREP_MAX_LINE_CHARS: usize = 300;
const SHELL_DEFAULT_TIMEOUT_SECS: u64 = 120;
const SHELL_MAX_TIMEOUT_SECS: u64 = 600;
const SHELL_MAX_OUTPUT_BYTES: usize = 30_000;

/// What the tools of one turn act on.
pub struct ToolContext {
    pub mode: Mode,
    pub root: Utf8PathBuf,
    pub tree: Arc<RwLock<ConversationTree>>,
    /// The node whose turn is running.
    pub node: NodeId,
}

/// The tools advertised to the model. `write` and `edit` exist in commit mode only.
pub fn specs(mode: Mode) -> Vec<ToolSpec> {
    let mut specs = read_only_specs();
    if mode == Mode::Commit {
        specs.push(ToolSpec {
            name: "write",
            description: "Create a file or replace its whole content. Missing parent \
                          directories are created.",
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the repository root"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        });
        specs.push(ToolSpec {
            name: "edit",
            description: "Replace one exact occurrence of `old_string` in a file with \
                          `new_string`. Fails when the text is absent or appears more \
                          than once; include enough surrounding lines to make it unique.",
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the repository root"},
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"}
                },
                "required": ["path", "old_string", "new_string"]
            }),
        });
    }
    specs
}

fn read_only_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "conversation",
            description: "Consult the conversation tree of this repository: every \
                          message the user ever sent here, with the agent's replies. \
                          `toc` lists conversations, or with `node` shows one page of \
                          the tree around that node; `read` returns a node's full \
                          prompt and reply; `commit` finds the node that produced a \
                          commit.",
            schema: json!({
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
            name: "read",
            description: "Read a text file of the repository. Lines are returned prefixed \
                          with their number.",
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the repository root"},
                    "offset": {"type": "integer", "description": "First line to return, starting at 1"},
                    "limit": {"type": "integer", "description": "Maximum number of lines (default 2000)"}
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "glob",
            description: "List the files of the repository whose path matches a glob \
                          pattern such as `src/**/*.rs`.",
            schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Glob with *, ** and ?"}
                },
                "required": ["pattern"]
            }),
        },
        ToolSpec {
            name: "grep",
            description: "Search file contents with a regular expression. Returns \
                          `path:line: text` for each match.",
            schema: json!({
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
            name: "shell",
            description: "Run a shell command with `sh -c` in the repository root and \
                          return its output and exit code.",
            schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout": {"type": "integer", "description": "Seconds before the command is killed (default 120, maximum 600)"}
                },
                "required": ["command"]
            }),
        },
    ]
}

/// Short description of a call for the status line, e.g. `shell cargo check`.
pub fn describe(call: &ToolCall) -> String {
    let detail = ["command", "path", "pattern"]
        .iter()
        .find_map(|key| call.arguments.get(key).and_then(Value::as_str))
        .unwrap_or("");
    let detail = detail.lines().next().unwrap_or("");
    format!("{} {detail}", call.name).trim_end().to_string()
}

/// Runs one tool call. A failure is returned as text for the model, never raised.
pub async fn run(context: &ToolContext, call: &ToolCall) -> String {
    let args = call.arguments.clone();
    let root = context.root.as_path();
    let result = match (call.name.as_str(), context.mode) {
        ("conversation", _) => {
            let tree = Arc::clone(&context.tree);
            let node = context.node.clone();
            blocking(move || conversation(&tree, &node, &args)).await
        }
        ("read", _) => read(root, &args),
        ("write", Mode::Commit) => write(root, &args),
        ("edit", Mode::Commit) => edit(root, &args),
        ("write" | "edit", Mode::Feedback) => {
            Err("files cannot be changed in feedback mode".to_string())
        }
        ("glob", _) => {
            let root = root.to_owned();
            blocking(move || glob(&root, &args)).await
        }
        ("grep", _) => {
            let root = root.to_owned();
            blocking(move || grep(&root, &args)).await
        }
        ("shell", _) => shell(root, &args).await,
        (other, _) => Err(format!("unknown tool: {other}")),
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

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string argument `{key}`"))
}

fn usize_arg(args: &Value, key: &str) -> Option<usize> {
    args.get(key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
}

/// Resolves `path` against the repository root and refuses anything outside it.
fn resolve(root: &Utf8Path, path: &str) -> Result<Utf8PathBuf, String> {
    let joined = if Utf8Path::new(path).is_absolute() {
        Utf8PathBuf::from(path)
    } else {
        root.join(path)
    };
    let mut resolved = Utf8PathBuf::new();
    for component in joined.components() {
        match component {
            Utf8Component::CurDir => {}
            Utf8Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other.as_str()),
        }
    }
    if resolved.starts_with(root) {
        Ok(resolved)
    } else {
        Err(format!("{path} is outside the repository"))
    }
}

fn read(root: &Utf8Path, args: &Value) -> Result<String, String> {
    let path = resolve(root, str_arg(args, "path")?)?;
    let content = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let offset = usize_arg(args, "offset").unwrap_or(1).max(1);
    let limit = usize_arg(args, "limit").unwrap_or(READ_DEFAULT_LINES);
    let total = content.lines().count();
    let mut out = String::new();
    for (number, line) in content.lines().enumerate().skip(offset - 1).take(limit) {
        out.push_str(&format!("{}\t{line}\n", number + 1));
    }
    let shown_until = (offset - 1 + limit).min(total);
    if shown_until < total {
        out.push_str(&format!(
            "[lines {offset}-{shown_until} of {total}; use offset to read more]\n"
        ));
    }
    Ok(out)
}

fn conversation(
    tree: &RwLock<ConversationTree>,
    current: &NodeId,
    args: &Value,
) -> Result<String, String> {
    let node_arg = || str_arg(args, "node").map(NodeId::new);
    let unknown = |node: &NodeId| format!("unknown node: {}", node.as_str());
    match str_arg(args, "action")? {
        "toc" => match args.get("node").and_then(Value::as_str) {
            Some(node) => {
                let node = NodeId::new(node);
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

fn write(root: &Utf8Path, args: &Value) -> Result<String, String> {
    let path = resolve(root, str_arg(args, "path")?)?;
    let content = str_arg(args, "content")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{parent}: {e}"))?;
    }
    std::fs::write(&path, content).map_err(|e| format!("{path}: {e}"))?;
    Ok(format!("wrote {} bytes\n", content.len()))
}

fn edit(root: &Utf8Path, args: &Value) -> Result<String, String> {
    let path = resolve(root, str_arg(args, "path")?)?;
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
fn repo_files(root: &Utf8Path) -> Vec<Utf8PathBuf> {
    let mut files: Vec<Utf8PathBuf> = crate::loader::walk_repo_files(root)
        .into_iter()
        .filter_map(|path| Utf8PathBuf::from_path_buf(path).ok())
        .filter_map(|path| path.strip_prefix(root).ok().map(Utf8Path::to_owned))
        .collect();
    files.sort();
    files
}

fn glob(root: &Utf8Path, args: &Value) -> Result<String, String> {
    let matcher = glob_regex(str_arg(args, "pattern")?)?;
    let matches: Vec<Utf8PathBuf> = repo_files(root)
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

fn grep(root: &Utf8Path, args: &Value) -> Result<String, String> {
    let pattern = regex::Regex::new(str_arg(args, "pattern")?).map_err(|e| e.to_string())?;
    let scope = match args.get("path").and_then(Value::as_str) {
        Some(path) => resolve(root, path)?
            .strip_prefix(root)
            .map(Utf8Path::to_owned)
            .unwrap_or_default(),
        None => Utf8PathBuf::new(),
    };
    let filter = match args.get("glob").and_then(Value::as_str) {
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
fn truncate_middle(text: &str, max_bytes: usize) -> String {
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

async fn shell(root: &Utf8Path, args: &Value) -> Result<String, String> {
    let command = str_arg(args, "command")?;
    let timeout = args
        .get("timeout")
        .and_then(Value::as_u64)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> (tempfile::TempDir, Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().canonicalize().unwrap()).unwrap();
        std::fs::create_dir_all(root.join("src/tui")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {\n    run();\n}\n").unwrap();
        std::fs::write(root.join("src/tui/app.rs"), "pub fn run() {}\n").unwrap();
        std::fs::write(root.join("README.md"), "# Demo\nrun it\n").unwrap();
        std::fs::write(root.join("target/out.rs"), "fn run() {}\n").unwrap();
        (tmp, root)
    }

    fn context(mode: Mode, root: &Utf8Path) -> ToolContext {
        ToolContext {
            mode,
            root: root.to_owned(),
            tree: Arc::default(),
            node: NodeId::new("current"),
        }
    }

    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments,
        }
    }

    #[test]
    fn resolve_refuses_paths_outside_the_root() {
        let root = Utf8Path::new("/repo");
        assert_eq!(resolve(root, "src/../a.rs").unwrap(), "/repo/a.rs");
        assert_eq!(resolve(root, "/repo/b.rs").unwrap(), "/repo/b.rs");
        assert!(resolve(root, "../etc/passwd").is_err());
        assert!(resolve(root, "/etc/passwd").is_err());
        assert!(resolve(root, "src/../../x").is_err());
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
    async fn read_numbers_lines_and_honors_offset_and_limit() {
        let (_tmp, root) = repo();
        let all = run(
            &context(Mode::Feedback, &root),
            &call("read", json!({"path": "src/main.rs"})),
        )
        .await;
        assert_eq!(all, "1\tfn main() {\n2\t    run();\n3\t}\n");
        let part = run(
            &context(Mode::Feedback, &root),
            &call(
                "read",
                json!({"path": "src/main.rs", "offset": 2, "limit": 1}),
            ),
        )
        .await;
        assert_eq!(
            part,
            "2\t    run();\n[lines 2-2 of 3; use offset to read more]\n"
        );
        let outside = run(
            &context(Mode::Feedback, &root),
            &call("read", json!({"path": "../x"})),
        )
        .await;
        assert!(outside.starts_with("error: "), "{outside}");
    }

    #[tokio::test]
    async fn glob_and_grep_skip_pruned_directories() {
        let (_tmp, root) = repo();
        let listed = run(
            &context(Mode::Feedback, &root),
            &call("glob", json!({"pattern": "**/*.rs"})),
        )
        .await;
        assert_eq!(listed, "src/main.rs\nsrc/tui/app.rs\n");

        let found = run(
            &context(Mode::Feedback, &root),
            &call("grep", json!({"pattern": "run"})),
        )
        .await;
        assert_eq!(
            found,
            "README.md:2: run it\nsrc/main.rs:2:     run();\nsrc/tui/app.rs:1: pub fn run() {}\n"
        );
        let scoped = run(
            &context(Mode::Feedback, &root),
            &call(
                "grep",
                json!({"pattern": "run", "path": "src", "glob": "**/app.rs"}),
            ),
        )
        .await;
        assert_eq!(scoped, "src/tui/app.rs:1: pub fn run() {}\n");
        let none = run(
            &context(Mode::Feedback, &root),
            &call("grep", json!({"pattern": "zzz"})),
        )
        .await;
        assert_eq!(none, "no matches\n");
    }

    #[tokio::test]
    async fn shell_reports_output_exit_code_and_timeout() {
        let (_tmp, root) = repo();
        let ok = run(
            &context(Mode::Feedback, &root),
            &call("shell", json!({"command": "echo hi; pwd"})),
        )
        .await;
        assert_eq!(ok, format!("hi\n{root}\n"));
        let failed = run(
            &context(Mode::Feedback, &root),
            &call("shell", json!({"command": "echo oops >&2; exit 3"})),
        )
        .await;
        assert_eq!(failed, "oops\nexit code: 3\n");
        let slow = run(
            &context(Mode::Feedback, &root),
            &call("shell", json!({"command": "sleep 5", "timeout": 1})),
        )
        .await;
        assert_eq!(slow, "command killed after 1s timeout\n");
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
        let shell = call("shell", json!({"command": "cargo check\necho done"}));
        assert_eq!(describe(&shell), "shell cargo check");
        assert_eq!(
            describe(&call("glob", json!({"pattern": "*.rs"}))),
            "glob *.rs"
        );
        assert_eq!(describe(&call("odd", json!({}))), "odd");
    }

    #[tokio::test]
    async fn write_and_edit_exist_in_commit_mode_only() {
        let (_tmp, root) = repo();
        let names = |mode| -> Vec<&str> { specs(mode).iter().map(|spec| spec.name).collect() };
        assert_eq!(
            names(Mode::Feedback),
            ["conversation", "read", "glob", "grep", "shell"]
        );
        assert_eq!(
            names(Mode::Commit),
            [
                "conversation",
                "read",
                "glob",
                "grep",
                "shell",
                "write",
                "edit"
            ]
        );

        let write_call = call(
            "write",
            json!({"path": "docs/new.md", "content": "one\ntwo\n"}),
        );
        let refused = run(&context(Mode::Feedback, &root), &write_call).await;
        assert_eq!(refused, "error: files cannot be changed in feedback mode");
        assert!(!root.join("docs/new.md").exists());

        assert_eq!(
            run(&context(Mode::Commit, &root), &write_call).await,
            "wrote 8 bytes\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("docs/new.md")).unwrap(),
            "one\ntwo\n"
        );
        let outside = call("write", json!({"path": "../evil", "content": "x"}));
        assert!(
            run(&context(Mode::Commit, &root), &outside)
                .await
                .starts_with("error: ")
        );
    }

    #[tokio::test]
    async fn edit_replaces_exactly_one_occurrence() {
        let (_tmp, root) = repo();
        std::fs::write(root.join("a.txt"), "alpha beta alpha\n").unwrap();
        let edit_call = |old: &str, new: &str| {
            call(
                "edit",
                json!({"path": "a.txt", "old_string": old, "new_string": new}),
            )
        };
        let ambiguous = run(&context(Mode::Commit, &root), &edit_call("alpha", "x")).await;
        assert!(ambiguous.contains("appears 2 times"), "{ambiguous}");
        let missing = run(&context(Mode::Commit, &root), &edit_call("gamma", "x")).await;
        assert_eq!(missing, "error: old_string was not found in the file");
        assert_eq!(
            run(&context(Mode::Commit, &root), &edit_call("beta", "gamma")).await,
            "edited\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("a.txt")).unwrap(),
            "alpha gamma alpha\n"
        );
    }

    #[tokio::test]
    async fn conversation_tool_serves_pages_nodes_and_commit_lookups() {
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
            mode: Mode::Feedback,
            root: root.clone(),
            tree: Arc::new(RwLock::new(ConversationTree::load(&store))),
            node: NodeId::new("n2"),
        };
        let ask = |arguments| call("conversation", arguments);

        let roots = run(&context, &ask(json!({"action": "toc"}))).await;
        assert!(
            roots.contains("- n1 [commit] abcdef0 First (2 nodes) <- your branch"),
            "{roots}"
        );
        let page = run(&context, &ask(json!({"action": "toc", "node": "n1"}))).await;
        assert!(
            page.contains("Continues with:\n- n2 [?] (no prompt) <- this turn"),
            "{page}"
        );
        let read = run(&context, &ask(json!({"action": "read", "node": "n1"}))).await;
        assert_eq!(read, "# Prompt\n\ndo it\n\n# Reply\n\ndone\n");
        let owner = run(
            &context,
            &ask(json!({"action": "commit", "hash": "abcdef0"})),
        )
        .await;
        assert!(owner.starts_with("Node:\n- n1 [commit]"), "{owner}");

        let missing = run(&context, &ask(json!({"action": "read", "node": "zz"}))).await;
        assert_eq!(missing, "error: unknown node: zz");
        let unknown = run(
            &context,
            &ask(json!({"action": "commit", "hash": "1234567"})),
        )
        .await;
        assert_eq!(unknown, "error: no node recorded commit 1234567");
    }
}
