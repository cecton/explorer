use super::ToolCall;
use super::llm::ToolSpec;
use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::Duration;

const READ_DEFAULT_LINES: usize = 2000;
const GLOB_MAX_PATHS: usize = 500;
const GREP_MAX_MATCHES: usize = 200;
const GREP_MAX_LINE_CHARS: usize = 300;
const SHELL_DEFAULT_TIMEOUT_SECS: u64 = 120;
const SHELL_MAX_TIMEOUT_SECS: u64 = 600;
const SHELL_MAX_OUTPUT_BYTES: usize = 30_000;

/// The tools advertised to the model.
pub fn specs() -> Vec<ToolSpec> {
    vec![
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
pub async fn run(root: &Utf8Path, call: &ToolCall) -> String {
    let args = call.arguments.clone();
    let result = match call.name.as_str() {
        "read" => read(root, &args),
        "glob" => {
            let root = root.to_owned();
            blocking(move || glob(&root, &args)).await
        }
        "grep" => {
            let root = root.to_owned();
            blocking(move || grep(&root, &args)).await
        }
        "shell" => shell(root, &args).await,
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
        let all = run(&root, &call("read", json!({"path": "src/main.rs"}))).await;
        assert_eq!(all, "1\tfn main() {\n2\t    run();\n3\t}\n");
        let part = run(
            &root,
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
        let outside = run(&root, &call("read", json!({"path": "../x"}))).await;
        assert!(outside.starts_with("error: "), "{outside}");
    }

    #[tokio::test]
    async fn glob_and_grep_skip_pruned_directories() {
        let (_tmp, root) = repo();
        let listed = run(&root, &call("glob", json!({"pattern": "**/*.rs"}))).await;
        assert_eq!(listed, "src/main.rs\nsrc/tui/app.rs\n");

        let found = run(&root, &call("grep", json!({"pattern": "run"}))).await;
        assert_eq!(
            found,
            "README.md:2: run it\nsrc/main.rs:2:     run();\nsrc/tui/app.rs:1: pub fn run() {}\n"
        );
        let scoped = run(
            &root,
            &call(
                "grep",
                json!({"pattern": "run", "path": "src", "glob": "**/app.rs"}),
            ),
        )
        .await;
        assert_eq!(scoped, "src/tui/app.rs:1: pub fn run() {}\n");
        let none = run(&root, &call("grep", json!({"pattern": "zzz"}))).await;
        assert_eq!(none, "no matches\n");
    }

    #[tokio::test]
    async fn shell_reports_output_exit_code_and_timeout() {
        let (_tmp, root) = repo();
        let ok = run(&root, &call("shell", json!({"command": "echo hi; pwd"}))).await;
        assert_eq!(ok, format!("hi\n{root}\n"));
        let failed = run(
            &root,
            &call("shell", json!({"command": "echo oops >&2; exit 3"})),
        )
        .await;
        assert_eq!(failed, "oops\nexit code: 3\n");
        let slow = run(
            &root,
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
}
