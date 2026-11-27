//! The knowledge base of a repository: short entries about its code, one markdown file
//! each, under `docs/knowledge/`. An entry `x.md` holds its children in a directory `x/`,
//! so the store is a tree, and the `knowledge` tool serves it in bounded pages the way the
//! `conversation` tool serves the conversation tree.
//!
//! The files are the store: nothing is cached between calls, so an entry written by a turn
//! is read by the next one. An entry Explorer did not write is reported as malformed rather
//! than read, so the layout and the formatter stay one fixed point.
//!
//! What a turn may do here follows its mode: a commit turn records, changes and removes
//! entries, and each change is staged so that the turn's own commit carries it, while a
//! feedback turn only reads, since its work is discarded when it ends.

use std::collections::BTreeMap;

/// Where the store lives, relative to the repository root.
pub(crate) const DIR: &str = "docs/knowledge";
/// Entries listed on one page of `toc`.
const TOC_PAGE: usize = 30;
/// Entries `read` returns at once.
const READ_MAX_IDS: usize = 10;
/// Entries `search` returns at once.
const SEARCH_MAX_HITS: usize = 20;
/// Characters a title, a summary and a body hold.
const TITLE_MAX: usize = 60;
const SUMMARY_MAX: usize = 200;
const BODY_MAX: usize = 2000;
/// Characters one segment of an id holds.
const NAME_MAX: usize = 32;
/// Faults `check` lists at once.
const CHECK_MAX_PROBLEMS: usize = 40;
/// Times staging waits for another git process to release the index.
const INDEX_LOCK_TRIES: u32 = 20;
const INDEX_LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(50);

/// What a turn may do with the store; a feedback turn only reads it, its copy is not the repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Access {
    Read,
    Write,
}

/// One entry of the knowledge base.
#[derive(Debug)]
struct Entry {
    title: String,
    summary: String,
    refs: Vec<String>,
    body: String,
}

impl Entry {
    /// The entry `text` holds: a title and a summary in its metadata block, the rest is the body.
    fn parse(text: &str) -> Result<Self, String> {
        let (metadata, body) = crate::util::text::split_metadata(text);
        if metadata.is_empty() {
            return Err("it has no metadata block".to_string());
        }
        let mut title = None;
        let mut summary = None;
        let mut refs = Vec::new();
        for (key, value) in crate::util::text::key_values(metadata) {
            match key {
                "title" => title = Some(value.to_string()),
                "summary" => summary = Some(value.to_string()),
                "refs" => {
                    refs = value
                        .split(',')
                        .map(str::trim)
                        .filter(|reference| !reference.is_empty())
                        .map(str::to_string)
                        .collect();
                }
                _ => {}
            }
        }
        Ok(Self {
            title: title.ok_or("it has no title")?,
            summary: summary.ok_or("it has no summary")?,
            refs,
            body: body.trim().to_string(),
        })
    }

    /// The file this entry is written as, in the formatter's layout.
    fn render(&self) -> Result<String, String> {
        let mut out = String::from("---\n");
        out.push_str(&format!("title: {}\n", self.title));
        out.push_str(&format!("summary: {}\n", self.summary));
        if !self.refs.is_empty() {
            out.push_str(&format!("refs: {}\n", self.refs.join(", ")));
        }
        out.push_str("---\n");
        if !self.body.is_empty() {
            out.push_str(&format!("\n{}\n", self.body));
        }
        crate::markdown_format::format_markdown(&out)
    }

    /// Whether `text` is the file this entry is written as; any other layout is not read.
    fn is_written_as(&self, text: &str) -> Result<(), String> {
        if self.render()?.trim_end() != text.trim_end() {
            return Err("its layout is not the one Explorer writes".to_string());
        }
        Ok(())
    }
}

/// One id of the store: an entry file, or a directory that holds no entry of its own.
enum Node {
    Bare,
    File {
        text: String,
        entry: Result<Entry, String>,
    },
}

/// Everything under `docs/knowledge/`, by id, read when the tool is called.
struct Store {
    root: camino::Utf8PathBuf,
    nodes: BTreeMap<String, Node>,
}

impl Store {
    /// Reads the store of `root`, empty when it is missing or reached through a link.
    fn load(root: &camino::Utf8Path) -> Self {
        let dir = root.join(DIR);
        let mut store = Self {
            root: root.to_owned(),
            nodes: BTreeMap::new(),
        };
        if is_real_dir(&root.join("docs")) && is_real_dir(&dir) {
            store.walk(&dir, "");
        }
        store
    }

    /// Adds the entries under `dir` with id prefix `prefix`, skipping links and non-segment names.
    fn walk(&mut self, dir: &camino::Utf8Path, prefix: &str) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if kind.is_dir() && is_segment(&name) {
                let id = joined(prefix, &name);
                self.insert_dir(&id);
                let path = camino::Utf8PathBuf::from_path_buf(entry.path());
                if let Ok(path) = path {
                    self.walk(&path, &id);
                }
            } else if kind.is_file()
                && let Some(stem) = name.strip_suffix(".md")
                && is_segment(stem)
            {
                let (text, entry) = match std::fs::read_to_string(entry.path()) {
                    Ok(text) => {
                        let entry = Entry::parse(&text);
                        (text, entry)
                    }
                    Err(error) => (String::new(), Err(format!("it cannot be read: {error}"))),
                };
                self.nodes
                    .insert(joined(prefix, stem), Node::File { text, entry });
            }
        }
    }

    /// Records the directory `id`, never replacing an entry file of the same id.
    fn insert_dir(&mut self, id: &str) {
        self.nodes.entry(id.to_string()).or_insert(Node::Bare);
    }

    /// The ids whose parent is `parent`, in id order.
    fn children(&self, parent: Option<&str>) -> Vec<&str> {
        self.nodes
            .keys()
            .filter(|id| parent_of(id) == parent)
            .map(String::as_str)
            .collect()
    }

    /// The parsed entries in id order, including those not laid out by Explorer (search reads them).
    fn entries(&self) -> impl Iterator<Item = (&str, &Entry)> {
        self.nodes.iter().filter_map(|(id, node)| match node {
            Node::File {
                entry: Ok(entry), ..
            } => Some((id.as_str(), entry)),
            Node::Bare | Node::File { .. } => None,
        })
    }

    /// The directory that holds the children of `id`, where the store keeps them.
    fn dir_of(&self, id: &str) -> camino::Utf8PathBuf {
        self.root.join(DIR).join(id)
    }

    /// The entry `id` names, or why the file it names is not one.
    fn entry(&self, id: &str) -> Result<&Entry, String> {
        match self.nodes.get(id) {
            Some(Node::File { text, entry }) => {
                let entry = entry.as_ref().map_err(|error| error.clone())?;
                entry.is_written_as(text)?;
                Ok(entry)
            }
            Some(Node::Bare) => Err("it has no entry file".to_string()),
            None => Err("no such entry".to_string()),
        }
    }

    /// One line about `id`, as a `toc` page lists it.
    fn describe(&self, id: &str) -> String {
        let mut line = match self.entry(id) {
            Ok(entry) => format!("{id}: {} — {}", entry.title, entry.summary),
            Err(error) => format!("{id}: (malformed: {error})"),
        };
        let children = self.children(Some(id)).len();
        match children {
            0 => {}
            1 => line.push_str(" (1 child)"),
            count => line.push_str(&format!(" ({count} children)")),
        }
        line
    }

    /// The entries under `parent`, one page of them, and the paging that surrounds it.
    fn level_page(&self, parent: Option<&str>, page: usize) -> (Vec<String>, usize, usize) {
        let children = self.children(parent);
        let pages = children.len().div_ceil(TOC_PAGE).max(1);
        let page = page.clamp(1, pages);
        let lines = children
            .iter()
            .skip((page - 1) * TOC_PAGE)
            .take(TOC_PAGE)
            .map(|id| format!("- {}\n", self.describe(id)))
            .collect();
        (lines, page, pages)
    }

    fn toc(&self, id: Option<&str>, page: usize) -> Result<String, String> {
        let Some(id) = id else {
            if self.nodes.is_empty() {
                return Ok("No knowledge recorded yet.\n".to_string());
            }
            let (lines, page, pages) = self.level_page(None, page);
            let total = self.children(None).len();
            let mut out = format!(
                "Knowledge base: {total} entries at the top level (page {page} of {pages}). \
                 Use `toc` with an id to look inside one, or `search` to find an entry.\n\n"
            );
            out.extend(lines);
            if page < pages {
                out.push_str(&format!("\nMore entries: toc page={}\n", page + 1));
            }
            return Ok(out);
        };
        if !self.nodes.contains_key(id) {
            return Err(format!("unknown knowledge id: {id}"));
        }
        let mut out = match self.entry(id) {
            Ok(entry) => {
                let mut block = format!("{id}: {} — {}\n", entry.title, entry.summary);
                if !entry.refs.is_empty() {
                    block.push_str(&format!("refs: {}\n", entry.refs.join(", ")));
                }
                block
            }
            Err(error) => format!("{id}: (malformed: {error})\n"),
        };
        let (lines, page, pages) = self.level_page(Some(id), page);
        match lines.len() {
            0 => out.push_str("\nNo children.\n"),
            _ => {
                out.push_str(&format!("\nChildren (page {page} of {pages}):\n"));
                out.extend(lines);
                if page < pages {
                    out.push_str(&format!("\nMore entries: toc id={id} page={}\n", page + 1));
                }
            }
        }
        Ok(out)
    }

    fn read(&self, ids: &[&str]) -> Result<String, String> {
        if ids.is_empty() {
            return Err("name at least one id".to_string());
        }
        if ids.len() > READ_MAX_IDS {
            return Err(format!("read at most {READ_MAX_IDS} ids at once"));
        }
        // An id is a key of the store, so nothing outside it is ever opened.
        let mut out = String::new();
        for id in ids {
            if !self.nodes.contains_key(*id) {
                out.push_str(&format!("# {id}\n\n(no such entry)\n"));
                continue;
            }
            let entry = match self.entry(id) {
                Ok(entry) => entry,
                Err(error) => {
                    out.push_str(&format!("# {id}\n\n(malformed: {error})\n"));
                    continue;
                }
            };
            out.push_str(&format!("# {id}: {}\n\n{}\n", entry.title, entry.summary));
            if !entry.refs.is_empty() {
                out.push_str(&format!("\nrefs: {}\n", entry.refs.join(", ")));
            }
            if !entry.body.is_empty() {
                out.push_str(&format!("\n{}\n", entry.body));
            }
            let children = self.children(Some(id));
            if !children.is_empty() {
                out.push_str(&format!("\nChildren: {}\n", children.join(", ")));
            }
        }
        Ok(out)
    }

    fn search(&self, query: &str) -> Result<String, String> {
        use crate::util::fuzzy::Kind;
        let query = query.trim();
        if query.is_empty() {
            return Err("the query is empty".to_string());
        }
        let mut hits: Vec<&str> = Vec::new();
        let ranked = crate::util::fuzzy::ranked(
            Kind::Plain,
            query,
            self.entries()
                .map(|(id, entry)| (id, format!("{id} {} {}", entry.title, entry.summary))),
        );
        hits.extend(ranked.unwrap_or_default());
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        for (id, entry) in self.entries() {
            if hits.contains(&id) {
                continue;
            }
            let body = entry.body.to_lowercase();
            if words.iter().all(|word| body.contains(word.as_str())) {
                hits.push(id);
            }
        }
        hits.truncate(SEARCH_MAX_HITS);
        if hits.is_empty() {
            return Ok(format!(
                "Nothing in the knowledge base matches \"{query}\".\n"
            ));
        }
        // A page that came out full may have cut the rest of the matches.
        let more = hits.len() == SEARCH_MAX_HITS;
        let noun = if hits.len() == 1 { "entry" } else { "entries" };
        let mut out = format!("{query}: {} {noun}\n\n", hits.len());
        for id in hits {
            out.push_str(&format!("- {}\n", self.describe(id)));
        }
        if more {
            out.push_str(&format!(
                "\nAt least {SEARCH_MAX_HITS} matches: narrow the query to see the rest.\n"
            ));
        }
        Ok(out)
    }

    /// What is wrong with the store, one page of the problems at a time.
    fn check(&self, page: usize) -> Result<String, String> {
        let mut problems: Vec<String> = Vec::new();
        let mut symbols: Vec<(String, String)> = Vec::new();
        for (id, node) in &self.nodes {
            if let Err(error) = self.entry(id) {
                let children = self.children(Some(id)).len();
                let more = match (matches!(node, Node::Bare), children) {
                    (true, 1) => " (1 child)".to_string(),
                    (true, count) if count > 1 => format!(" ({count} children)"),
                    _ => String::new(),
                };
                problems.push(format!("- {id}: (malformed: {error}){more}"));
            }
        }
        for (id, entry) in self.entries() {
            if entry.refs.is_empty() {
                problems.push(format!(
                    "- {id}: no refs, so nothing names the code it describes"
                ));
                continue;
            }
            for reference in &entry.refs {
                match is_path(reference) {
                    true if !self.root.join(reference).exists() => {
                        problems.push(missing_path(id, reference))
                    }
                    true => {}
                    false => symbols.push((id.to_string(), reference.clone())),
                }
            }
        }
        for (id, missing) in self.missing_symbols(&symbols)? {
            for name in missing {
                problems.push(format!("- {id}: refs {name}, which no file holds"));
            }
        }
        if self.nodes.is_empty() {
            return Ok("No knowledge recorded yet.\n".to_string());
        }
        if problems.is_empty() {
            let noun = if self.nodes.len() == 1 {
                "entry"
            } else {
                "entries"
            };
            return Ok(format!(
                "The knowledge base checks out: {} {noun}, none with a problem.\n",
                self.nodes.len()
            ));
        }
        let pages = problems.len().div_ceil(CHECK_MAX_PROBLEMS).max(1);
        let page = page.clamp(1, pages);
        let mut out = format!(
            "Knowledge check: {} problem{} (page {page} of {pages}).\n\n",
            problems.len(),
            if problems.len() == 1 { "" } else { "s" }
        );
        for problem in problems
            .iter()
            .skip((page - 1) * CHECK_MAX_PROBLEMS)
            .take(CHECK_MAX_PROBLEMS)
        {
            out.push_str(problem);
            out.push('\n');
        }
        if page < pages {
            out.push_str(&format!("\nMore problems: check page={}\n", page + 1));
        }
        Ok(out)
    }

    /// The symbol refs of `symbols` that no file of the tree holds, grouped by id, in one `git grep`.
    fn missing_symbols(
        &self,
        symbols: &[(String, String)],
    ) -> Result<Vec<(String, Vec<String>)>, String> {
        if symbols.is_empty() {
            return Ok(Vec::new());
        }
        let mut names: Vec<&str> = symbols
            .iter()
            .map(|(_, reference)| symbol_name(reference))
            .collect();
        names.sort();
        names.dedup();
        let held = grep_symbols(&self.root, &names)?;
        let mut missing: Vec<(String, Vec<String>)> = Vec::new();
        for (id, reference) in symbols {
            if held.iter().any(|name| name == symbol_name(reference)) {
                continue;
            }
            match missing.iter_mut().find(|(group, _)| group == id) {
                Some((_, group)) => group.push(reference.clone()),
                None => missing.push((id.clone(), vec![reference.clone()])),
            }
        }
        Ok(missing)
    }
}

/// Whether a reference names a file rather than a symbol: it holds a separator or an extension.
fn is_path(reference: &str) -> bool {
    reference.contains('/') || reference.contains('.')
}

/// The problem a path ref that names no file is: one that also holds `::` joins a path and
/// a symbol, which the agent may mean as one ref where the store wants two.
fn missing_path(id: &str, reference: &str) -> String {
    match reference.contains("::") {
        true => format!(
            "- {id}: refs {reference}, which is a path and a symbol joined; write them as two refs"
        ),
        false => format!("- {id}: refs {reference}, which does not exist"),
    }
}

/// The name a reference is searched for: its last `::` segment, since a method is rarely a literal.
fn symbol_name(reference: &str) -> &str {
    reference.rsplit("::").next().unwrap_or(reference)
}

/// The symbols of `names` that a file of `root` outside the store holds.
fn grep_symbols(root: &camino::Utf8Path, names: &[&str]) -> Result<Vec<String>, String> {
    use std::io::Write;
    let mut patterns = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    for name in names {
        writeln!(patterns, "{name}").map_err(|e| e.to_string())?;
    }
    let output = std::process::Command::new("git")
        .current_dir(root)
        // `--untracked`: a turn that writes code and its entry in one commit would otherwise
        // find its own new symbols missing.
        .args(["grep", "--untracked", "-F", "-w", "-o", "-h", "-f"])
        .arg(patterns.path())
        // The store's own refs name every symbol it lists, so it is not searched.
        .args(["--", "."])
        .arg(format!(":(exclude){DIR}"))
        .output()
        .map_err(|e| format!("git grep: {e}"))?;
    // Exit status 1 is no match: no file holds any of them, which is no failure.
    if !matches!(output.status.code(), Some(0 | 1)) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git grep: {}", stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| line.trim().to_string())
        .collect())
}

/// Whether `name` is one segment of a knowledge id.
fn is_segment(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn joined(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

/// Whether `path` is a directory itself, not a link to one (`is_dir` would follow the link).
fn is_real_dir(path: &camino::Utf8Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

fn parent_of(id: &str) -> Option<&str> {
    id.rsplit_once('/').map(|(parent, _)| parent)
}

/// Whether one call of the tool changes the store rather than reading it.
pub(crate) fn is_mutation(args: &serde_json::Value) -> bool {
    matches!(
        args.get("action").and_then(serde_json::Value::as_str),
        Some("write" | "delete" | "move" | "rename")
    )
}

/// The description and schema of the tool for `access`, offering only what it may do.
pub(crate) struct Spec {
    pub(crate) description: String,
    pub(crate) schema: serde_json::Value,
}

pub(crate) fn spec(access: Access) -> Spec {
    let mut actions = vec!["toc", "read", "search", "check"];
    let mut description = "Consult the knowledge base of this repository: short entries, \
                           under docs/knowledge/, about how its code works. `toc` lists \
                           entries, or with `id` one entry and its children; `read` \
                           returns the given entries; `search` finds entries by title, \
                           summary and body; `check` lists what is wrong with the base: \
                           entries that cannot be read, entries without `refs` or whose \
                           `refs` name no code, and directories that hold children but no \
                           entry of their own."
        .to_string();
    if access == Access::Write {
        actions.extend(["write", "delete", "move", "rename"]);
        description.push_str(
            " `write` records an entry (without an `id`) or changes the fields it is given \
             (with one); `delete` removes an entry and everything under it; `move` puts an \
             entry under another one; `rename` gives it another name. Each change is staged, \
             so the turn's commit takes it.",
        );
    }
    let id_for = match access {
        Access::Read => "`toc`",
        Access::Write => "`toc`, `write`, `delete`, `move` and `rename`",
    };
    let mut properties = serde_json::json!({
        "action": {"type": "string", "enum": actions},
        "id": {"type": "string", "description": format!("Knowledge id: the entry, for {id_for}")},
        "page": {"type": "integer", "description": "Page of the list, for `toc` and `check`"},
        "ids": {"type": "array", "items": {"type": "string"}, "description": "Knowledge ids, for `read`"},
        "query": {"type": "string", "description": "Words to look for, for `search`"}
    });
    if access == Access::Write {
        let properties = properties.as_object_mut().expect("an object");
        for (key, value) in [
            (
                "parent",
                serde_json::json!({
                    "type": "string",
                    "description": "Id of the entry the entry goes under, for `write` and `move`; the top level when absent"
                }),
            ),
            (
                "name",
                serde_json::json!({
                    "type": "string",
                    "description": "Name of the entry's id, for `write` without an `id` and for `rename`: lowercase letters, digits and dashes, at most 32 characters"
                }),
            ),
            (
                "title",
                serde_json::json!({"type": "string", "description": "One line, at most 60 characters"}),
            ),
            (
                "summary",
                serde_json::json!({
                    "type": "string",
                    "description": "One line, at most 200 characters, saying what the entry is about"
                }),
            ),
            (
                "refs",
                serde_json::json!({
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "The code the entry describes: a path from the repository root or a bare symbol name, one per item, with no comma or line break"
                }),
            ),
            (
                "body",
                serde_json::json!({
                    "type": "string",
                    "description": "At most 2000 characters of markdown; it is stored as Explorer's formatter lays it out"
                }),
            ),
        ] {
            properties.insert(key.to_string(), value);
        }
    }
    Spec {
        description,
        schema: serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": ["action"]
        }),
    }
}

/// The message every change ends with, so a turn knows its work is staged.
fn staged(paths: &[String]) -> String {
    format!(
        "Staged {}. Knowledge changes are staged by the knowledge tool so they can be \
         committed by the turn's commit. Do not unstage them.\n",
        paths.join(", ")
    )
}

/// Serializes changes to the store: two calls writing one file must not interleave.
static CHANGES: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// Returns once any change in flight has finished, so a turn's close step does not race it.
pub(crate) fn settle() {
    drop(CHANGES.lock());
}

/// Records a new entry under `args`'s `parent`, and stages its file.
fn create(
    store: &Store,
    root: &camino::Utf8Path,
    args: &serde_json::Value,
) -> Result<String, String> {
    let parent = parent_arg(store, args)?;
    let name = name_arg(args)?;
    if let Some(taken) = sibling(store, parent.as_deref(), &name) {
        return Err(format!("{taken} already has that name; pick another"));
    }
    let entry = Entry {
        title: line_arg(args, "title", TITLE_MAX)?,
        summary: line_arg(args, "summary", SUMMARY_MAX)?,
        refs: refs_arg(args)?,
        body: body_arg(args)?,
    };
    let id = joined(parent.as_deref().unwrap_or_default(), &name);
    write_entry(root, &id, &entry)?;
    Ok(format!("Recorded {id}.\n{}", staged(&[file(id.as_str())])))
}

/// Changes the fields `args` gives of the entry `args` names.
fn update(
    store: &Store,
    root: &camino::Utf8Path,
    args: &serde_json::Value,
) -> Result<String, String> {
    if has(args, "name") {
        return Err("`rename` gives an entry another name; `write` changes its text".to_string());
    }
    if has(args, "parent") {
        return Err("`move` puts an entry under another one; `write` changes its text".to_string());
    }
    let id = str_arg(args, "id")?;
    let known = store.entry(id).is_ok();
    let existing = known.then(|| store.entry(id)).transpose()?;
    let needs_both = |error: String| match known {
        true => error,
        false => {
            format!("{error} ({id} has no readable entry, so `title` and `summary` are needed)")
        }
    };
    let title = match existing {
        Some(entry) if !has(args, "title") => entry.title.clone(),
        _ => line_arg(args, "title", TITLE_MAX).map_err(needs_both)?,
    };
    let summary = match existing {
        Some(entry) if !has(args, "summary") => entry.summary.clone(),
        _ => line_arg(args, "summary", SUMMARY_MAX).map_err(needs_both)?,
    };
    let entry = Entry {
        title,
        summary,
        refs: match has(args, "refs") {
            true => refs_arg(args)?,
            false => existing.map(|entry| entry.refs.clone()).unwrap_or_default(),
        },
        body: match has(args, "body") {
            true => body_arg(args)?,
            false => existing.map(|entry| entry.body.clone()).unwrap_or_default(),
        },
    };
    if !store.nodes.contains_key(id) {
        return Err(format!("unknown knowledge id: {id}"));
    }
    write_entry(root, id, &entry)?;
    let done = match known {
        true => format!("Changed {id}."),
        false => format!("Rewrote {id} from scratch (it was malformed or had no entry file)."),
    };
    Ok(format!("{done}\n{}", staged(&[file(id)])))
}

/// Removes the entry `args` names with everything under it, and stages every file that goes.
fn delete(
    store: &Store,
    root: &camino::Utf8Path,
    args: &serde_json::Value,
) -> Result<String, String> {
    let id = existing_arg(store, args)?;
    let ids = ids_under(store, id);
    let paths = paths_under(store, id);
    if paths.is_empty() {
        return Err(format!("{id} holds no file to remove"));
    }
    stage(root, &paths, || remove_paths(root, &paths))?;
    Ok(format!("Removed {}.\n{}", ids.join(", "), staged(&paths)))
}

/// Puts the entry `args` names under `args`'s `parent`, or at the top level.
fn move_entry(
    store: &Store,
    root: &camino::Utf8Path,
    args: &serde_json::Value,
) -> Result<String, String> {
    let id = existing_arg(store, args)?;
    let parent = parent_arg(store, args)?;
    if let Some(parent) = &parent
        && (parent == id || parent.starts_with(&format!("{id}/")))
    {
        return Err(format!("{id} cannot go under itself"));
    }
    let name = id.rsplit('/').next().unwrap_or(id);
    if let Some(taken) = sibling(store, parent.as_deref(), name) {
        return Err(format!("{taken} already has that name"));
    }
    let moved = joined(parent.as_deref().unwrap_or_default(), name);
    let paths = relocate(store, root, id, &moved)?;
    Ok(format!("Moved {id} to {moved}.\n{}", staged(&paths)))
}

/// Gives the entry `args` names the name `args` gives, everything under it moving with it.
fn rename(
    store: &Store,
    root: &camino::Utf8Path,
    args: &serde_json::Value,
) -> Result<String, String> {
    let id = existing_arg(store, args)?;
    let name = name_arg(args)?;
    let parent = parent_of(id);
    if let Some(taken) = sibling(store, parent, &name) {
        return Err(format!("{taken} already has that name; pick another"));
    }
    let renamed = joined(parent.unwrap_or_default(), &name);
    let paths = relocate(store, root, id, &renamed)?;
    Ok(format!("Renamed {id} to {renamed}.\n{}", staged(&paths)))
}

/// Moves `id` and everything under it into the id `to`, and stages both ends; returns what is staged.
fn relocate(
    store: &Store,
    root: &camino::Utf8Path,
    id: &str,
    to: &str,
) -> Result<Vec<String>, String> {
    let paths = paths_under(store, id);
    if paths.is_empty() {
        return Err(format!("{id} holds no file to move"));
    }
    let mut staged_paths = paths.clone();
    // Where each file goes, which is what the index has to be told about as well.
    for path in &paths {
        let rest = path.strip_prefix(&format!("{DIR}/{id}")).unwrap_or(path);
        staged_paths.push(format!("{DIR}/{to}{rest}"));
    }
    staged_paths.sort();
    staged_paths.dedup();
    stage(root, &staged_paths, || move_files(root, &paths, id, to))?;
    Ok(staged_paths)
}

/// Moves the files the store holds for `id` into `to`.
fn move_files(root: &camino::Utf8Path, paths: &[String], id: &str, to: &str) -> Result<(), String> {
    let moves: Vec<(camino::Utf8PathBuf, camino::Utf8PathBuf)> = paths
        .iter()
        .map(|path| {
            let rest = path.strip_prefix(&format!("{DIR}/{id}")).unwrap_or(path);
            (root.join(path), root.join(format!("{DIR}/{to}{rest}")))
        })
        .collect();
    // Every target is checked before the first file moves, so a refusal leaves the store as it was.
    for (_, target) in &moves {
        guard_links(root, target)?;
    }
    for (source, target) in &moves {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{parent}: {e}"))?;
        }
        std::fs::rename(source, target).map_err(|e| format!("{source}: {e}"))?;
    }
    // The directories that held the entry's children go once nothing is left in them.
    remove_empty_dirs(&root.join(DIR).join(id));
    let store = root.join(DIR);
    for (source, _) in &moves {
        for dir in source
            .ancestors()
            .skip(1)
            .take_while(|dir| dir.starts_with(&store) && dir.as_str() != store.as_str())
        {
            // A directory that is gone is passed; one still holding something stops the climb.
            if std::fs::remove_dir(dir).is_err() && dir.exists() {
                break;
            }
        }
    }
    Ok(())
}

/// Removes `dir` and its empty subdirectories, leaving anything that still holds a file.
fn remove_empty_dirs(dir: &camino::Utf8Path) {
    if !is_real_dir(dir) {
        return;
    }
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir())
            && let Ok(path) = camino::Utf8PathBuf::from_path_buf(entry.path())
        {
            remove_empty_dirs(&path);
        }
    }
    let _ = std::fs::remove_dir(dir);
}

/// Writes the file of `id` as `entry` lays it out, and stages it.
fn write_entry(root: &camino::Utf8Path, id: &str, entry: &Entry) -> Result<(), String> {
    let path = root.join(file(id));
    let text = entry.render()?;
    // A body the formatter lays out differently is refused, so no file shows as malformed.
    if entry_of(&text)?.render()? != text {
        return Err(
            "the entry does not survive the formatter; shorten or simplify its body".to_string(),
        );
    }
    let staged_path = file(id);
    stage(root, &[staged_path], || {
        // Before a directory is made: a linked store is not one to write into.
        guard_links(root, &path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{parent}: {e}"))?;
        }
        crate::util::fs::write_atomic(&path, &text).map_err(|e| format!("{path}: {e}"))
    })
}

/// The entry a rendered file holds, as `Store` would read it back.
fn entry_of(text: &str) -> Result<Entry, String> {
    let entry = Entry::parse(text)?;
    entry.is_written_as(text)?;
    Ok(entry)
}

/// Runs `change`, then stages `paths`; a failed staging is reported as a change left unstaged.
fn stage(
    root: &camino::Utf8Path,
    paths: &[String],
    change: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    change()?;
    match stage_paths(root, paths) {
        Ok(()) => Ok(()),
        Err(error) => Err(format!(
            "the change was made but not staged: {error}. Stage it yourself before you finish"
        )),
    }
}

fn stage_paths(root: &camino::Utf8Path, paths: &[String]) -> Result<(), String> {
    paths.iter().try_for_each(|path| stage_path(root, path))
}

/// `git add` of one path, waiting out another git process holding the index.
fn stage_path(root: &camino::Utf8Path, path: &str) -> Result<(), String> {
    // One path per call: git fails a call when one matches nothing. `:(literal)`: no globs.
    let pathspec = format!(":(literal){path}");
    for _ in 0..INDEX_LOCK_TRIES {
        match crate::git::git_output(root, &["add", "-A", "--", pathspec.as_str()]) {
            Ok(_) => return Ok(()),
            Err(error) if error.contains("index.lock") => {
                std::thread::sleep(INDEX_LOCK_WAIT);
            }
            // A path nothing holds any more is already where the index wants it.
            Err(error) if error.contains("did not match any files") => return Ok(()),
            Err(error) => return Err(error),
        }
    }
    Err("git could not take its index: another git process is holding index.lock".to_string())
}

/// The store's file for `id`, relative to the repository root.
fn file(id: &str) -> String {
    format!("{DIR}/{id}.md")
}

/// Deletes the files of `paths` and the directories they leave empty.
fn remove_paths(root: &camino::Utf8Path, paths: &[String]) -> Result<(), String> {
    for path in paths {
        let full = root.join(path);
        match std::fs::remove_file(&full) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{full}: {error}")),
        }
    }
    // The directories that held an entry's children go once nothing is left in them.
    let mut dirs: Vec<camino::Utf8PathBuf> = paths
        .iter()
        .filter_map(|path| root.join(path).parent().map(camino::Utf8Path::to_owned))
        .collect();
    dirs.sort();
    dirs.dedup();
    let mut deepest_first = dirs;
    deepest_first.sort_by_key(|dir| std::cmp::Reverse(dir.as_str().len()));
    let store = root.join(DIR);
    for dir in deepest_first {
        let mut dir = dir;
        while dir.starts_with(&store) && dir != store {
            let gone = std::fs::remove_dir(&dir).is_ok();
            let Some(parent) = dir.parent().map(camino::Utf8Path::to_owned) else {
                break;
            };
            if !gone {
                break;
            }
            dir = parent;
        }
    }
    Ok(())
}

/// Refuses a file that would be written through a link, or into one.
fn guard_links(root: &camino::Utf8Path, path: &camino::Utf8Path) -> Result<(), String> {
    for dir in [&root.join("docs"), &root.join(DIR)] {
        if std::fs::symlink_metadata(dir).is_ok_and(|meta| !meta.is_dir()) {
            return Err(format!("{dir} is not a directory of its own"));
        }
    }
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_symlink()) {
        return Err(format!("{path} is a link, not an entry file"));
    }
    let store = root.join(DIR);
    for dir in path
        .ancestors()
        .skip(1)
        .take_while(|dir| dir.starts_with(&store))
    {
        if std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_symlink()) {
            return Err(format!("{dir} is a link, not a directory of the store"));
        }
    }
    Ok(())
}

/// The `parent` argument, which must be an entry that holds children.
fn parent_arg(store: &Store, args: &serde_json::Value) -> Result<Option<String>, String> {
    let Some(parent) = given(args, "parent") else {
        return Ok(None);
    };
    let parent = parent.as_str().ok_or("`parent` is not a string")?;
    if !store.nodes.contains_key(parent) {
        return Err(format!("unknown knowledge id: {parent}"));
    }
    store
        .entry(parent)
        .map_err(|error| format!("{parent} cannot hold entries: {error}"))?;
    Ok(Some(parent.to_string()))
}

/// The `id` argument, which must name an id of the store.
fn existing_arg<'a>(store: &Store, args: &'a serde_json::Value) -> Result<&'a str, String> {
    let id = str_arg(args, "id")?;
    match store.nodes.contains_key(id) {
        true => Ok(id),
        false => Err(format!("unknown knowledge id: {id}")),
    }
}

/// A `name` argument, normalized into one segment of an id.
fn name_arg(args: &serde_json::Value) -> Result<String, String> {
    let raw = str_arg(args, "name")?;
    let name: String = raw
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let name: String = name
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<&str>>()
        .join("-");
    if name.is_empty() {
        return Err("`name` has no letters or digits to make a name of".to_string());
    }
    if name.len() > NAME_MAX {
        return Err(format!("`name` is longer than {NAME_MAX} characters"));
    }
    Ok(name)
}

/// A one-line `title` or `summary` argument.
fn line_arg(args: &serde_json::Value, key: &str, limit: usize) -> Result<String, String> {
    let value = str_arg(args, key)?.trim();
    if value.is_empty() {
        return Err(format!("`{key}` is empty"));
    }
    if value.lines().count() > 1 {
        return Err(format!("`{key}` holds more than one line"));
    }
    if value.chars().count() > limit {
        return Err(format!("`{key}` is longer than {limit} characters"));
    }
    Ok(value.to_string())
}

fn body_arg(args: &serde_json::Value) -> Result<String, String> {
    let body = match given(args, "body") {
        Some(body) => body.as_str().ok_or("`body` is not a string")?,
        None => "",
    };
    let body = body.trim();
    if body.chars().count() > BODY_MAX {
        return Err(format!("`body` is longer than {BODY_MAX} characters"));
    }
    Ok(body.to_string())
}

fn refs_arg(args: &serde_json::Value) -> Result<Vec<String>, String> {
    let Some(refs) = given(args, "refs") else {
        return Ok(Vec::new());
    };
    let refs = refs.as_array().ok_or("`refs` is not a list")?;
    refs.iter()
        .map(|reference| {
            let text = reference
                .as_str()
                .ok_or_else(|| "`refs` holds a non-string".to_string())?
                .trim();
            if text.is_empty() {
                return Err("`refs` holds an empty reference".to_string());
            }
            if text.contains(',') || text.contains('\n') {
                return Err(format!(
                    "`refs` holds `{text}`, which has a comma or a line break"
                ));
            }
            Ok(text.to_string())
        })
        .collect()
}

/// The id of `parent`'s child named `name`, if one is taken.
fn sibling(store: &Store, parent: Option<&str>, name: &str) -> Option<String> {
    let id = joined(parent.unwrap_or_default(), name);
    store.nodes.contains_key(&id).then_some(id)
}

/// `id` and every id under it, in id order.
fn ids_under(store: &Store, id: &str) -> Vec<String> {
    let below = format!("{id}/");
    store
        .nodes
        .keys()
        .filter(|node| node.as_str() == id || node.starts_with(&below))
        .cloned()
        .collect()
}

/// The store's files for `id` and everything under it, as repository paths.
fn paths_under(store: &Store, id: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    for node in ids_under(store, id) {
        match &store.nodes[&node] {
            Node::File { .. } => paths.push(file(&node)),
            // A directory stands for the files it holds, which are walked below.
            Node::Bare => {}
        }
    }
    for node in ids_under(store, id) {
        let dir = store.dir_of(&node);
        if is_real_dir(&dir) {
            walk_files(&store.root, &dir, &mut paths);
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

/// Every file under `dir`, as repository paths relative to the repository `root`.
fn walk_files(root: &camino::Utf8Path, dir: &camino::Utf8Path, paths: &mut Vec<String>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let Ok(path) = camino::Utf8PathBuf::from_path_buf(entry.path()) else {
            continue;
        };
        // A link is a name in the store, not a tree to walk.
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            walk_files(root, &path, paths);
        } else if kind.is_file()
            && let Ok(relative) = path.strip_prefix(root)
        {
            paths.push(relative.as_str().to_string());
        }
    }
}

/// Runs one call of the `knowledge` tool. A failure is returned as text for the model.
pub(crate) fn run(
    root: &camino::Utf8Path,
    args: &serde_json::Value,
    access: Access,
) -> Result<String, String> {
    if is_mutation(args) && access == Access::Read {
        return Err(
            "this is a feedback turn: the knowledge base is read-only here, and its work \
                    is discarded when the turn ends. Put what is worth recording in your \
                    closing instruction, so the turn that carries it out records it."
                .to_string(),
        );
    }
    // Held from the load on: the checks of a change read the store it then writes.
    let _change = is_mutation(args).then(|| CHANGES.lock());
    let store = Store::load(root);
    match str_arg(args, "action")? {
        "toc" => store.toc(
            args.get("id").and_then(serde_json::Value::as_str),
            usize_arg(args, "page").unwrap_or(1),
        ),
        "read" => store.read(&ids_arg(args)?),
        "search" => store.search(str_arg(args, "query")?),
        "check" => store.check(usize_arg(args, "page").unwrap_or(1)),
        "write" if has(args, "id") => update(&store, root, args),
        "write" => create(&store, root, args),
        "delete" => delete(&store, root, args),
        "move" => move_entry(&store, root, args),
        "rename" => rename(&store, root, args),
        other => Err(format!("unknown action: {other}")),
    }
}

/// The value `args` gives `key`; a JSON null is no value, so it gives none.
fn given<'a>(args: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    args.get(key).filter(|value| !value.is_null())
}

/// Whether `args` was given `key`, which an empty value still does and a null does not.
fn has(args: &serde_json::Value, key: &str) -> bool {
    given(args, key).is_some()
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

fn ids_arg(args: &serde_json::Value) -> Result<Vec<&str>, String> {
    let ids = args
        .get("ids")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "missing array argument `ids`".to_string())?;
    ids.iter()
        .map(|id| {
            id.as_str()
                .ok_or_else(|| "`ids` holds a non-string".to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::test_support::temp_root;

    /// One call, as a commit turn makes it.
    fn run(root: &camino::Utf8Path, args: &serde_json::Value) -> Result<String, String> {
        crate::knowledge::run(root, args, Access::Write)
    }

    fn entry(title: &str, summary: &str) -> Entry {
        Entry {
            title: title.to_string(),
            summary: summary.to_string(),
            refs: Vec::new(),
            body: String::new(),
        }
    }

    /// A repository whose store holds `files`, each written as given.
    fn store_of(files: &[(&str, String)]) -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let (tmp, root) = temp_root();
        std::fs::create_dir_all(root.join(DIR)).unwrap();
        for (id, text) in files {
            let path = root.join(DIR).join(format!("{id}.md"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        (tmp, root)
    }

    /// A repository, its store and a first commit, so the index has a HEAD to compare with.
    fn staged_repo(files: &[(&str, String)]) -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let (tmp, root) = store_of(files);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        crate::util::test_support::init_repo(&root);
        crate::util::test_support::git(&root, &["add", "-A"]);
        crate::util::test_support::git(&root, &["commit", "-q", "-m", "init"]);
        (tmp, root)
    }

    /// What the index holds against HEAD, as `git status --short` shows it.
    fn staged(root: &camino::Utf8Path) -> String {
        crate::util::test_support::git(root, &["status", "--short"])
    }

    fn record(id: &str, name: &str, title: &str, summary: &str) -> serde_json::Value {
        let mut args = serde_json::json!({
            "action": "write",
            "name": name,
            "title": title,
            "summary": summary,
        });
        if id.is_empty() {
            return args;
        }
        args["id"] = serde_json::json!(id);
        args
    }

    #[test]
    fn a_directory_never_replaces_its_entry_file() {
        let file = || Node::File {
            text: String::new(),
            entry: Ok(entry("t", "s")),
        };
        let (_tmp, root) = temp_root();
        let mut file_first = Store {
            root: root.clone(),
            nodes: BTreeMap::new(),
        };
        file_first.nodes.insert("a".to_string(), file());
        file_first.insert_dir("a");
        assert!(matches!(file_first.nodes["a"], Node::File { .. }));

        let mut dir_first = Store {
            root,
            nodes: BTreeMap::new(),
        };
        dir_first.insert_dir("a");
        dir_first.nodes.insert("a".to_string(), file());
        assert!(matches!(dir_first.nodes["a"], Node::File { .. }));
    }

    /// The entry `text` holds, read the way the store reads a file.
    fn read_back(text: &str) -> Result<Entry, String> {
        let entry = Entry::parse(text)?;
        entry.is_written_as(text)?;
        Ok(entry)
    }

    fn rendered(entries: &[(&'static str, Entry)]) -> Vec<(&'static str, String)> {
        entries
            .iter()
            .map(|(id, entry)| (*id, entry.render().unwrap()))
            .collect()
    }

    #[test]
    fn an_entry_round_trips_through_its_file() {
        let mut written = entry("Closing a turn", "What a turn does when it ends.");
        written.refs = vec![
            "src/agent/turn/close.rs".to_string(),
            "discard_turn_work".to_string(),
        ];
        written.body = "A turn that works in the repository ends with a reset.".to_string();
        let text = written.render().unwrap();
        assert!(text.starts_with("---\ntitle: Closing a turn\nsummary: What a turn does when it ends.\nrefs: src/agent/turn/close.rs, discard_turn_work\n---\n\n"));
        assert!(text.ends_with("reset.\n"));
        let read = read_back(&text).unwrap();
        assert_eq!(read.title, written.title);
        assert_eq!(read.refs, written.refs);
        assert_eq!(read.body, written.body);
        assert_eq!(read.render().unwrap(), text);
    }

    #[test]
    fn an_entry_without_a_body_or_refs_round_trips() {
        let text = entry("A title", "A summary.").render().unwrap();
        assert_eq!(text, "---\ntitle: A title\nsummary: A summary.\n---\n");
        assert!(read_back(&text).unwrap().body.is_empty());
    }

    #[test]
    fn a_body_holding_a_rule_line_is_kept() {
        let mut written = entry("Rules", "A rule in the body.");
        written.body = "Above\n\n---\n\nBelow".to_string();
        let text = written.render().unwrap();
        assert_eq!(read_back(&text).unwrap().body, written.body);
    }

    #[test]
    fn a_file_without_its_metadata_block_is_refused() {
        let error = read_back("Just a body.\n").unwrap_err();
        assert!(error.contains("no metadata block"), "{error}");
        let missing = read_back("---\nsummary: s\n---\n").unwrap_err();
        assert_eq!(missing, "it has no title");
        let missing = read_back("---\ntitle: t\n---\n").unwrap_err();
        assert_eq!(missing, "it has no summary");
    }

    #[test]
    fn a_file_explorer_did_not_lay_out_is_refused() {
        let mut written = entry("Bullets", "A list.");
        written.body = "* one".to_string();
        let mut text = written.render().unwrap();
        assert!(text.ends_with("- one\n"), "{text}");
        assert!(read_back(&text).is_ok());
        text = text.replace("- one", "* one");
        let error = read_back(&text).unwrap_err();
        assert!(error.contains("not the one Explorer writes"), "{error}");
    }

    #[test]
    fn a_file_saved_without_its_last_newline_is_still_the_entry_it_holds() {
        let text = entry("A title", "A summary.").render().unwrap();
        assert!(read_back(text.trim_end()).is_ok());
    }

    #[test]
    fn the_store_reads_the_tree_of_entries_and_their_children() {
        let first = entry("First", "The first entry.");
        let second = entry("Second", "A child of the first.");
        let (_tmp, root) = store_of(&rendered(&[("first", first), ("first/second", second)]));
        let store = Store::load(&root);
        assert_eq!(store.children(None), ["first"]);
        assert_eq!(store.children(Some("first")), ["first/second"]);
        assert_eq!(
            store.describe("first"),
            "first: First — The first entry. (1 child)"
        );
        assert_eq!(store.entries().count(), 2);
    }

    #[test]
    fn the_store_skips_names_that_are_not_ids_and_never_follows_a_link() {
        let (_tmp, root) = store_of(&rendered(&[("good", entry("Good", "Kept."))]));
        let dir = root.join(DIR);
        std::fs::write(
            dir.join("Bad_Name.md"),
            entry("Bad", "Skipped.").render().unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "not an entry").unwrap();
        std::fs::create_dir_all(dir.join("Upper")).unwrap();
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("x.md"),
            entry("Out", "Outside.").render().unwrap(),
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("linked")).unwrap();

        let store = Store::load(&root);
        assert_eq!(store.nodes.keys().collect::<Vec<_>>(), ["good"]);
    }

    #[test]
    fn a_store_reached_through_a_link_is_not_read() {
        // An entry that exists only outside the repository's `docs/`, under `elsewhere/`.
        let (_tmp, root) = temp_root();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("knowledge")).unwrap();
        let text = entry("Outside", "Not in the store.").render().unwrap();
        std::fs::write(elsewhere.join("knowledge/out.md"), text).unwrap();

        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::os::unix::fs::symlink(elsewhere.join("knowledge"), root.join(DIR)).unwrap();
        assert_eq!(Store::load(&root).entries().count(), 0);

        std::fs::remove_file(root.join(DIR)).unwrap();
        std::fs::remove_dir(root.join("docs")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join("docs")).unwrap();
        assert_eq!(Store::load(&root).entries().count(), 0);
    }

    #[test]
    fn a_file_that_is_not_utf8_is_malformed_rather_than_read() {
        let (_tmp, root) = store_of(&rendered(&[("good", entry("Good", "Kept."))]));
        std::fs::write(root.join(DIR).join("binary.md"), [0xff, 0xfe, 0x00]).unwrap();
        let store = Store::load(&root);
        let page = store.toc(None, 1).unwrap();
        assert!(
            page.contains("- binary: (malformed: it cannot be read:"),
            "{page}"
        );
        assert!(page.contains("- good: Good — Kept."), "{page}");
        assert_eq!(store.entries().count(), 1);
    }

    #[test]
    fn a_directory_without_an_entry_file_is_malformed_but_its_children_are_reachable() {
        let (_tmp, root) = store_of(&rendered(&[("x/y", entry("A child", "Under a bare dir."))]));
        let store = Store::load(&root);
        assert_eq!(
            store.describe("x"),
            "x: (malformed: it has no entry file) (1 child)"
        );
        let page = store.toc(Some("x"), 1).unwrap();
        assert!(
            page.contains("x: (malformed: it has no entry file)"),
            "{page}"
        );
        assert!(
            page.contains("- x/y: A child — Under a bare dir."),
            "{page}"
        );
    }

    #[test]
    fn toc_lists_the_top_level_and_names_the_next_page() {
        let entries: Vec<(String, Entry)> = (0..35)
            .map(|i| {
                (
                    format!("e{i:02}"),
                    entry(&format!("Entry {i}"), "A summary."),
                )
            })
            .collect();
        let files: Vec<(&str, String)> = entries
            .iter()
            .map(|(id, entry)| (id.as_str(), entry.render().unwrap()))
            .collect();
        let (_tmp, root) = store_of(&files);
        let store = Store::load(&root);
        let first = store.toc(None, 1).unwrap();
        assert!(
            first.starts_with("Knowledge base: 35 entries at the top level (page 1 of 2)."),
            "{first}"
        );
        assert!(first.contains("- e00: Entry 0 — A summary."));
        assert!(!first.contains("- e30:"));
        assert!(first.ends_with("More entries: toc page=2\n"));
        let second = store.toc(None, 2).unwrap();
        assert!(second.contains("- e30: Entry 30 — A summary."));
        assert!(!second.contains("More entries"));
    }

    #[test]
    fn an_empty_or_missing_store_says_so() {
        let (tmp, root) = temp_root();
        let empty = Store::load(&root);
        assert_eq!(empty.toc(None, 1).unwrap(), "No knowledge recorded yet.\n");
        std::fs::create_dir_all(root.join(DIR)).unwrap();
        assert_eq!(
            Store::load(&root).toc(None, 1).unwrap(),
            "No knowledge recorded yet.\n"
        );
        drop(tmp);
    }

    #[test]
    fn toc_of_an_unknown_id_is_an_error() {
        let (_tmp, root) = store_of(&rendered(&[("a", entry("A", "An entry."))]));
        let error = Store::load(&root).toc(Some("nope"), 1).unwrap_err();
        assert_eq!(error, "unknown knowledge id: nope");
    }

    #[test]
    fn toc_of_an_entry_without_children_says_so() {
        let (_tmp, root) = store_of(&rendered(&[("a", entry("A", "An entry."))]));
        let page = Store::load(&root).toc(Some("a"), 1).unwrap();
        assert_eq!(page, "a: A — An entry.\n\nNo children.\n");
    }

    #[test]
    fn read_returns_entries_and_reports_the_ids_it_cannot() {
        let mut written = entry("A", "An entry.");
        written.refs = vec!["src/x.rs".to_string()];
        written.body = "Reads work.".to_string();
        let child = entry("B", "A child.");
        let (_tmp, root) = store_of(&rendered(&[("a", written), ("a/b", child)]));
        let store = Store::load(&root);
        let text = store.read(&["a", "nope"]).unwrap();
        assert!(text.starts_with("# a: A\n\nAn entry.\n"), "{text}");
        assert!(text.contains("\nrefs: src/x.rs\n"));
        assert!(text.contains("\nReads work.\n"));
        assert!(text.contains("\nChildren: a/b\n"));
        assert!(text.ends_with("# nope\n\n(no such entry)\n"));
        assert_eq!(store.read(&[]).unwrap_err(), "name at least one id");
        let many = vec!["a"; READ_MAX_IDS + 1];
        assert!(store.read(&many).is_err());
    }

    #[test]
    fn search_finds_an_entry_by_title_and_by_a_body_word_without_repeating_it() {
        let mut turn = entry("Closing a turn", "How a turn ends.");
        turn.body = "A turn ends with a hard reset of the tree.".to_string();
        let other = entry("Reftlink copies", "A copy of the repository.");
        let (_tmp, root) = store_of(&rendered(&[("turn", turn), ("copies", other)]));
        let store = Store::load(&root);

        let by_title = store.search("turn").unwrap();
        assert!(by_title.contains("- turn: Closing a turn — How a turn ends."));
        assert!(!by_title.contains("copies:"));

        let by_body = store.search("hard reset").unwrap();
        assert!(by_body.starts_with("hard reset: 1 entry\n"), "{by_body}");
        assert!(by_body.contains("- turn:"));

        // A fuzzy hit is not listed again for its body; a body match needs every word.
        let both = store.search("turn reset").unwrap();
        assert_eq!(both.matches("- turn:").count(), 1, "{both}");
        assert!(both.starts_with("turn reset: 1 entry\n"), "{both}");
        let title = store.search("turn").unwrap();
        assert_eq!(title.matches("- turn:").count(), 1, "{title}");
        assert_eq!(
            store.search("nothing").unwrap(),
            "Nothing in the knowledge base matches \"nothing\".\n"
        );
        assert_eq!(store.search("  ").unwrap_err(), "the query is empty");
    }

    #[test]
    fn search_says_when_the_page_of_matches_was_cut() {
        let entries: Vec<(String, Entry)> = (0..25)
            .map(|i| {
                (
                    format!("e{i:02}"),
                    entry(&format!("Entry {i}"), "A summary."),
                )
            })
            .collect();
        let files: Vec<(&str, String)> = entries
            .iter()
            .map(|(id, entry)| (id.as_str(), entry.render().unwrap()))
            .collect();
        let (_tmp, root) = store_of(&files);
        let store = Store::load(&root);

        let cut = store.search("summary").unwrap();
        assert!(
            cut.starts_with(&format!("summary: {SEARCH_MAX_HITS} entries\n")),
            "{cut}"
        );
        assert!(
            cut.ends_with(&format!(
                "\nAt least {SEARCH_MAX_HITS} matches: narrow the query to see the rest.\n"
            )),
            "{cut}"
        );

        // A page that came out short names nothing more.
        let (_tmp, root) = store_of(&rendered(&[("a", entry("A", "An entry."))]));
        let short = Store::load(&root).search("a").unwrap();
        assert!(!short.contains("narrow the query"), "{short}");
    }

    #[test]
    fn run_reads_its_arguments() {
        let (_tmp, root) = store_of(&rendered(&[("a", entry("A", "An entry."))]));
        assert!(run(&root, &serde_json::json!({"action": "toc"})).is_ok());
        assert!(run(&root, &serde_json::json!({"action": "read", "ids": ["a"]})).is_ok());
        assert!(
            run(
                &root,
                &serde_json::json!({"action": "search", "query": "entry"})
            )
            .is_ok()
        );
        assert_eq!(
            run(&root, &serde_json::json!({"action": "write"})).unwrap_err(),
            "missing string argument `name`"
        );
        assert_eq!(
            run(&root, &serde_json::json!({"action": "nope"})).unwrap_err(),
            "unknown action: nope"
        );
        assert!(
            run(&root, &serde_json::json!({}))
                .unwrap_err()
                .contains("action")
        );
        assert!(
            run(&root, &serde_json::json!({"action": "read"}))
                .unwrap_err()
                .contains("ids")
        );
    }

    #[test]
    fn segments_are_lowercase_words_of_letters_digits_and_dashes() {
        for good in ["a", "a-b", "turn-closing", "a1"] {
            assert!(is_segment(good), "{good}");
        }
        for bad in ["", "-a", "a-", "A", "a_b", "a.b", "a b", ".."] {
            assert!(!is_segment(bad), "{bad}");
        }
    }

    #[test]
    fn ids_nest_and_names_their_parent() {
        assert_eq!(parent_of("a"), None);
        assert_eq!(parent_of("a/b"), Some("a"));
        assert_eq!(parent_of("a/b/c"), Some("a/b"));
        assert_eq!(joined("", "a"), "a");
        assert_eq!(joined("a", "b"), "a/b");
    }

    #[test]
    fn a_name_becomes_one_segment_of_an_id() {
        let named = |raw: &str| name_arg(&serde_json::json!({"name": raw}));
        assert_eq!(named("Closing a turn").unwrap(), "closing-a-turn");
        assert_eq!(named("  Twin::Peaks  ").unwrap(), "twin-peaks");
        assert_eq!(named("a--b").unwrap(), "a-b");
        assert_eq!(named("__x__").unwrap(), "x");
        assert_eq!(
            named("  !!!  ").unwrap_err(),
            "`name` has no letters or digits to make a name of"
        );
        let long = "a".repeat(NAME_MAX + 1);
        assert!(named(&long).unwrap_err().contains("longer than 32"));
    }

    #[test]
    fn a_title_and_a_summary_are_one_line_within_their_limit() {
        let line = |value: &str, limit: usize| {
            line_arg(&serde_json::json!({"title": value}), "title", limit)
        };
        assert_eq!(line("A title", TITLE_MAX).unwrap(), "A title");
        assert_eq!(line("  A title  ", TITLE_MAX).unwrap(), "A title");
        assert_eq!(line("", TITLE_MAX).unwrap_err(), "`title` is empty");
        assert_eq!(
            line("one\ntwo", TITLE_MAX).unwrap_err(),
            "`title` holds more than one line"
        );
        let long = "x".repeat(TITLE_MAX + 1);
        assert_eq!(
            line(&long, TITLE_MAX).unwrap_err(),
            "`title` is longer than 60 characters"
        );
    }

    #[test]
    fn a_reference_holds_no_comma_and_no_line_break() {
        let refs = |value: serde_json::Value| refs_arg(&serde_json::json!({"refs": value}));
        assert_eq!(
            refs(serde_json::json!([" src/a.rs ", "Recorder"])).unwrap(),
            ["src/a.rs", "Recorder"]
        );
        assert!(
            refs(serde_json::json!(["a,b"]))
                .unwrap_err()
                .contains("comma")
        );
        assert!(
            refs(serde_json::json!(["a\nb"]))
                .unwrap_err()
                .contains("comma")
        );
        assert!(refs(serde_json::json!([""])).unwrap_err().contains("empty"));
        assert!(
            refs(serde_json::json!([7]))
                .unwrap_err()
                .contains("non-string")
        );
        assert!(refs(serde_json::Value::Null).unwrap().is_empty());
    }

    #[test]
    fn a_new_entry_is_written_in_the_layout_and_staged() {
        let (_tmp, root) = staged_repo(&[]);
        let written = run(
            &root,
            &record(
                "",
                "Closing a turn",
                "Closing a turn",
                "What a turn does when it ends.",
            ),
        )
        .unwrap();
        assert_eq!(
            written,
            "Recorded closing-a-turn.\nStaged docs/knowledge/closing-a-turn.md. Knowledge \
             changes are staged by the knowledge tool so they can be committed by the turn's \
             commit. Do not unstage them.\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join(DIR).join("closing-a-turn.md")).unwrap(),
            "---\ntitle: Closing a turn\nsummary: What a turn does when it ends.\n---\n"
        );
        assert_eq!(staged(&root), "A  docs/knowledge/closing-a-turn.md");
        // The entry reads back as the one written.
        assert_eq!(
            Store::load(&root).entry("closing-a-turn").unwrap().title,
            "Closing a turn"
        );
    }

    #[test]
    fn a_new_entry_goes_under_a_parent_that_holds_entries() {
        let (_tmp, root) = staged_repo(&rendered(&[("a", entry("A", "An entry."))]));
        let mut args = record("", "b", "B", "A child.");
        args["parent"] = serde_json::json!("a");
        run(&root, &args).unwrap();
        assert!(root.join(DIR).join("a/b.md").is_file());
        assert_eq!(staged(&root), "A  docs/knowledge/a/b.md");

        let mut nowhere = record("", "c", "C", "Orphan.");
        nowhere["parent"] = serde_json::json!("nope");
        assert_eq!(
            run(&root, &nowhere).unwrap_err(),
            "unknown knowledge id: nope"
        );
        // A bare directory has no entry file, so nothing may go under it either.
        std::fs::create_dir_all(root.join(DIR).join("bare")).unwrap();
        let mut bare = record("", "d", "D", "Under a bare dir.");
        bare["parent"] = serde_json::json!("bare");
        assert!(
            run(&root, &bare)
                .unwrap_err()
                .contains("bare cannot hold entries: it has no entry file")
        );
    }

    #[test]
    fn two_creates_of_one_name_at_once_record_one_and_refuse_the_other() {
        let (_tmp, root) = staged_repo(&[]);
        let results: Vec<Result<String, String>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|n| {
                    let root = &root;
                    scope.spawn(move || run(root, &record("", "x", &format!("T{n}"), "S.")))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(
            results.iter().filter(|r| r.is_ok()).count(),
            1,
            "{results:?}"
        );
        let refused = results.iter().find_map(|r| r.as_ref().err()).unwrap();
        assert_eq!(refused, "x already has that name; pick another");
    }

    #[test]
    fn rewriting_a_malformed_entry_says_it_started_from_scratch() {
        let (_tmp, root) = staged_repo(&[("x/y", entry("Y", "Why.").render().unwrap())]);
        let mut args = record("x", "", "X", "Ex.");
        args.as_object_mut().unwrap().remove("name");
        let result = run(&root, &args).unwrap();
        assert!(result.starts_with("Rewrote x from scratch"), "{result}");
    }

    #[test]
    fn a_name_a_sibling_already_has_is_refused_and_named() {
        let (_tmp, root) = staged_repo(&rendered(&[("a", entry("A", "An entry."))]));
        assert_eq!(
            run(&root, &record("", "a", "Other", "Other.")).unwrap_err(),
            "a already has that name; pick another"
        );
    }

    #[test]
    fn a_write_with_an_id_changes_only_the_fields_it_is_given() {
        let mut written = entry("A", "An entry.");
        written.refs = vec!["src/a.rs".to_string()];
        written.body = "A body.".to_string();
        let (_tmp, root) = staged_repo(&rendered(&[("a", written)]));
        let changed = run(
            &root,
            &serde_json::json!({"action": "write", "id": "a", "summary": "Another summary."}),
        )
        .unwrap();
        assert!(changed.starts_with("Changed a.\n"), "{changed}");
        assert!(
            run(
                &root,
                &serde_json::json!({"action": "write", "id": "a", "parent": "b"})
            )
            .unwrap_err()
            .contains("`move` puts an entry")
        );
        let store = Store::load(&root);
        let entry = store.entry("a").unwrap();
        assert_eq!(entry.title, "A");
        assert_eq!(entry.summary, "Another summary.");
        assert_eq!(entry.refs, ["src/a.rs"]);
        assert_eq!(entry.body, "A body.");
        assert_eq!(
            staged(&root),
            "M  docs/knowledge/a.md",
            "the entry is staged, not left in the tree"
        );
        assert_eq!(
            run(
                &root,
                &serde_json::json!({"action": "write", "id": "a", "name": "b"})
            )
            .unwrap_err(),
            "`rename` gives an entry another name; `write` changes its text"
        );
    }

    #[test]
    fn a_null_field_is_no_value_and_a_field_of_the_wrong_type_is_refused() {
        let mut written = entry("A", "An entry.");
        written.refs = vec!["src/a.rs".to_string()];
        written.body = "A body.".to_string();
        let (_tmp, root) = staged_repo(&rendered(&[("a", written)]));
        let changed = run(
            &root,
            &serde_json::json!({
                "action": "write", "id": "a", "summary": "Another summary.",
                "body": null, "refs": null, "parent": null, "title": null
            }),
        )
        .unwrap();
        assert!(changed.starts_with("Changed a.\n"), "{changed}");
        let store = Store::load(&root);
        let kept = store.entry("a").unwrap();
        assert_eq!(kept.summary, "Another summary.");
        assert_eq!(kept.refs, ["src/a.rs"]);
        assert_eq!(kept.body, "A body.");
        assert!(
            run(
                &root,
                &serde_json::json!({"action": "write", "id": "a", "body": 3})
            )
            .unwrap_err()
            .contains("`body` is not a string")
        );
        assert!(
            run(
                &root,
                &serde_json::json!({"action": "write", "id": "a", "refs": "src/a.rs"})
            )
            .unwrap_err()
            .contains("`refs` is not a list")
        );
        assert!(
            run(
                &root,
                &serde_json::json!({"action": "write", "name": "b", "title": "B", "summary": "s", "parent": 4})
            )
            .unwrap_err()
            .contains("`parent` is not a string")
        );
        assert!(!root.join(DIR).join("b.md").exists());
    }

    #[test]
    fn a_null_id_is_no_id_so_the_write_records_a_new_entry() {
        let (_tmp, root) = staged_repo(&rendered(&[("a", entry("A", "An entry."))]));
        let mut args = record("", "b", "B", "A new entry.");
        args["id"] = serde_json::Value::Null;
        let recorded = run(&root, &args).unwrap();
        assert!(recorded.starts_with("Recorded b."), "{recorded}");
        assert!(root.join(DIR).join("b.md").is_file());
    }

    #[test]
    fn a_bad_field_of_a_readable_entry_is_refused_without_the_malformed_note() {
        let (_tmp, root) = staged_repo(&rendered(&[("a", entry("A", "An entry."))]));
        assert_eq!(
            run(
                &root,
                &serde_json::json!({"action": "write", "id": "a", "title": ""})
            )
            .unwrap_err(),
            "`title` is empty"
        );
    }

    #[test]
    fn a_file_explorer_did_not_lay_out_is_repaired_with_a_whole_entry() {
        let (_tmp, root) = staged_repo(&[("a", "* not the layout".to_string())]);
        let error = run(
            &root,
            &serde_json::json!({"action": "write", "id": "a", "summary": "s"}),
        )
        .unwrap_err();
        assert!(error.contains("has no readable entry"), "{error}");
        run(
            &root,
            &serde_json::json!({"action": "write", "id": "a", "title": "A", "summary": "A summary."}),
        )
        .unwrap();
        assert!(Store::load(&root).entry("a").is_ok());
    }

    #[test]
    fn a_body_the_formatter_would_rewrite_is_stored_as_the_formatter_writes_it() {
        let (_tmp, root) = staged_repo(&[]);
        let mut args = record("", "lists", "Lists", "A list.");
        args["body"] = serde_json::json!("* one\n* two");
        run(&root, &args).unwrap();
        let text = std::fs::read_to_string(root.join(DIR).join("lists.md")).unwrap();
        assert!(text.ends_with("\n- one\n- two\n"), "{text}");
    }

    #[test]
    fn a_delete_stages_the_tracked_files_even_when_an_untracked_one_goes_too() {
        let (_tmp, root) = staged_repo(&rendered(&[("x", entry("X", "An entry."))]));
        let loose = root.join(DIR).join("x").join("loose.md");
        std::fs::create_dir_all(loose.parent().unwrap()).unwrap();
        std::fs::write(&loose, entry("Loose", "Untracked.").render().unwrap()).unwrap();
        run(&root, &serde_json::json!({"action": "delete", "id": "x"})).unwrap();
        assert_eq!(staged(&root), "D  docs/knowledge/x.md");
    }

    #[test]
    fn a_delete_removes_the_entry_and_its_children_and_stages_them_all() {
        let (_tmp, root) = staged_repo(&rendered(&[
            ("a", entry("A", "An entry.")),
            ("a/b", entry("B", "A child.")),
            ("a/b/c", entry("C", "A grandchild.")),
            ("keep", entry("K", "Kept.")),
        ]));
        let removed = run(&root, &serde_json::json!({"action": "delete", "id": "a"})).unwrap();
        assert!(removed.starts_with("Removed a, a/b, a/b/c.\n"), "{removed}");
        assert!(!root.join(DIR).join("a.md").exists());
        assert!(
            !root.join(DIR).join("a").exists(),
            "its directory went with it"
        );
        assert!(root.join(DIR).join("keep.md").is_file());
        assert_eq!(
            staged(&root),
            "D  docs/knowledge/a.md\nD  docs/knowledge/a/b.md\nD  docs/knowledge/a/b/c.md"
        );
        assert_eq!(
            run(&root, &serde_json::json!({"action": "delete", "id": "a"})).unwrap_err(),
            "unknown knowledge id: a"
        );
    }

    #[test]
    fn a_rename_moves_everything_and_stages_both_names() {
        let (_tmp, root) = staged_repo(&rendered(&[
            ("a", entry("A", "An entry.")),
            ("a/b", entry("B", "A child.")),
        ]));
        let renamed = run(
            &root,
            &serde_json::json!({"action": "rename", "id": "a", "name": "Z"}),
        )
        .unwrap();
        assert!(renamed.starts_with("Renamed a to z.\n"), "{renamed}");
        // Every path the change staged is named, the children's too.
        assert!(
            renamed.contains("docs/knowledge/z/b.md") && renamed.contains("docs/knowledge/a/b.md"),
            "{renamed}"
        );
        assert!(root.join(DIR).join("z.md").is_file());
        assert!(root.join(DIR).join("z/b.md").is_file());
        assert!(!root.join(DIR).join("a").exists());
        assert_eq!(
            staged(&root),
            "R  docs/knowledge/a.md -> docs/knowledge/z.md\nR  docs/knowledge/a/b.md -> docs/knowledge/z/b.md"
        );
        let store = Store::load(&root);
        assert_eq!(store.children(Some("z")), ["z/b"]);
    }

    #[test]
    fn a_move_puts_an_entry_under_another_and_refuses_a_cycle() {
        let (_tmp, root) = staged_repo(&rendered(&[
            ("a", entry("A", "An entry.")),
            ("a/b", entry("B", "A child.")),
            ("c", entry("C", "Another entry.")),
        ]));
        let moved = run(
            &root,
            &serde_json::json!({"action": "move", "id": "c", "parent": "a"}),
        )
        .unwrap();
        assert!(moved.starts_with("Moved c to a/c.\n"), "{moved}");
        assert!(root.join(DIR).join("a/c.md").is_file());
        assert!(!root.join(DIR).join("c.md").exists());

        assert_eq!(
            run(
                &root,
                &serde_json::json!({"action": "move", "id": "a", "parent": "a/b"})
            )
            .unwrap_err(),
            "a cannot go under itself"
        );
        assert_eq!(
            run(
                &root,
                &serde_json::json!({"action": "move", "id": "a/b", "parent": "a"})
            )
            .unwrap_err(),
            "a/b already has that name"
        );
        // No parent at all is the top level, where the name may be free again.
        run(&root, &serde_json::json!({"action": "move", "id": "a/b"})).unwrap();
        assert!(root.join(DIR).join("b.md").is_file());
    }

    #[test]
    fn a_move_leaves_no_empty_directory_behind() {
        let (_tmp, root) = staged_repo(&rendered(&[
            ("top", entry("T", "A top entry.")),
            ("top/mid/leaf", entry("L", "A leaf.")),
        ]));
        run(
            &root,
            &serde_json::json!({"action": "move", "id": "top/mid/leaf"}),
        )
        .unwrap();
        assert!(root.join(DIR).join("leaf.md").is_file());
        assert!(!root.join(DIR).join("top/mid").exists());
        assert!(!Store::load(&root).nodes.contains_key("top/mid"));
    }

    #[test]
    fn a_change_refuses_a_store_reached_through_a_link() {
        let (_tmp, root) = temp_root();
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join(DIR)).unwrap();
        crate::util::test_support::init_repo(&root);
        let error = run(&root, &record("", "a", "A", "An entry.")).unwrap_err();
        assert!(error.contains("is not a directory of its own"), "{error}");
        assert!(!elsewhere.join("a.md").exists(), "nothing was written");
    }

    #[test]
    fn a_move_into_a_linked_directory_is_refused_before_anything_moves() {
        let (_tmp, root) = staged_repo(&rendered(&[
            ("a", entry("A", "An entry.")),
            ("a/b", entry("B", "A child.")),
        ]));
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join(DIR).join("z")).unwrap();
        let error = run(
            &root,
            &serde_json::json!({"action": "rename", "id": "a", "name": "z"}),
        )
        .unwrap_err();
        assert!(error.contains("is a link"), "{error}");
        assert!(root.join(DIR).join("a.md").is_file());
        assert!(root.join(DIR).join("a/b.md").is_file());
        assert!(
            !elsewhere.join("b.md").exists(),
            "nothing was written through the link"
        );
    }

    #[test]
    fn settle_waits_for_a_change_in_flight() {
        let held = CHANGES.lock();
        let (sender, receiver) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            settle();
            sender.send(()).unwrap();
        });
        assert!(
            receiver
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err()
        );
        drop(held);
        receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        waiter.join().unwrap();
    }

    #[test]
    fn a_delete_never_follows_a_link_out_of_the_store() {
        let (_tmp, root) = staged_repo(&rendered(&[("a", entry("A", "An entry."))]));
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("kept.md"), "not an entry\n").unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join(DIR).join("a")).unwrap();
        let result = run(&root, &serde_json::json!({"action": "delete", "id": "a"}));
        assert!(result.is_ok(), "{result:?}");
        assert!(!root.join(DIR).join("a.md").exists());
        assert!(
            elsewhere.join("kept.md").is_file(),
            "a file outside the store was removed through the link"
        );
    }

    #[test]
    fn a_read_only_turn_changes_nothing_and_says_where_to_record_it() {
        let (_tmp, root) = staged_repo(&rendered(&[("a", entry("A", "An entry."))]));
        let error =
            crate::knowledge::run(&root, &record("", "b", "B", "Another entry."), Access::Read)
                .unwrap_err();
        assert!(error.contains("feedback turn"), "{error}");
        assert!(error.contains("closing instruction"), "{error}");
        assert!(!root.join(DIR).join("b.md").exists());
        assert_eq!(staged(&root), "");
        // Reading is what it may do.
        assert!(
            crate::knowledge::run(&root, &serde_json::json!({"action": "toc"}), Access::Read)
                .is_ok()
        );
    }

    #[test]
    fn the_schema_offers_a_read_only_turn_only_the_reading_actions() {
        let actions = |access: Access| {
            spec(access).schema["properties"]["action"]["enum"]
                .as_array()
                .unwrap()
                .iter()
                .map(|action| action.as_str().unwrap().to_string())
                .collect::<Vec<String>>()
        };
        assert_eq!(actions(Access::Read), ["toc", "read", "search", "check"]);
        assert_eq!(
            actions(Access::Write),
            [
                "toc", "read", "search", "check", "write", "delete", "move", "rename"
            ]
        );
        assert!(
            spec(Access::Read).schema["properties"]
                .get("body")
                .is_none()
        );
        assert!(
            spec(Access::Write).schema["properties"]
                .get("body")
                .is_some()
        );
        assert!(spec(Access::Write).description.contains("staged"));
        assert!(!spec(Access::Read).description.contains("staged"));
    }

    #[test]
    fn only_a_change_is_a_mutation() {
        for action in ["write", "delete", "move", "rename"] {
            assert!(
                is_mutation(&serde_json::json!({"action": action})),
                "{action}"
            );
        }
        for action in ["toc", "read", "search", "check"] {
            assert!(
                !is_mutation(&serde_json::json!({"action": action})),
                "{action}"
            );
        }
        assert!(!is_mutation(&serde_json::json!({})));
    }

    #[test]
    fn staging_waits_out_another_git_process_holding_the_index() {
        let (_tmp, root) = staged_repo(&[]);
        let lock = root.join(".git").join("index.lock");
        std::fs::write(&lock, "").unwrap();
        let held = std::thread::spawn({
            let lock = lock.clone();
            move || {
                std::thread::sleep(INDEX_LOCK_WAIT * 3);
                std::fs::remove_file(&lock).unwrap();
            }
        });
        run(&root, &record("", "a", "A", "An entry.")).unwrap();
        held.join().unwrap();
        assert_eq!(staged(&root), "A  docs/knowledge/a.md");
    }

    #[test]
    fn staging_a_name_with_glob_characters_stages_that_file_only() {
        let (_tmp, root) = staged_repo(&[]);
        std::fs::write(root.join(DIR).join("a1.md"), "").unwrap();
        std::fs::write(root.join(DIR).join("a[1].md"), "").unwrap();
        stage_path(&root, "docs/knowledge/a[1].md").unwrap();
        assert_eq!(
            crate::util::test_support::git(&root, &["diff", "--cached", "--name-only"]),
            "docs/knowledge/a[1].md"
        );
    }

    #[test]
    fn a_file_the_store_could_not_stage_says_the_change_stands() {
        let (_tmp, root) = staged_repo(&[]);
        let lock = root.join(".git").join("index.lock");
        std::fs::write(&lock, "").unwrap();
        let error = run(&root, &record("", "a", "A", "An entry.")).unwrap_err();
        std::fs::remove_file(&lock).unwrap();
        assert!(
            error.contains("the change was made but not staged"),
            "{error}"
        );
        assert!(root.join(DIR).join("a.md").is_file());
    }

    /// A store whose entries name `src/main.rs` and a symbol the code holds.
    fn checked_repo() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let mut good = entry("A good entry", "It names code that exists.");
        good.refs = vec!["src/main.rs".to_string(), "main".to_string()];
        let (tmp, root) = staged_repo(&rendered(&[("good", good)]));
        (tmp, root)
    }

    /// What `check` reports about `root`.
    fn check(root: &camino::Utf8Path) -> String {
        run(root, &serde_json::json!({"action": "check"})).unwrap()
    }

    #[test]
    fn check_passes_a_base_whose_refs_name_the_code() {
        let (_tmp, root) = checked_repo();
        assert_eq!(
            check(&root),
            "The knowledge base checks out: 1 entry, none with a problem.\n"
        );
        let (empty_tmp, empty) = temp_root();
        assert_eq!(check(&empty), "No knowledge recorded yet.\n");
        drop(empty_tmp);
    }

    #[test]
    fn check_reports_every_kind_of_problem() {
        let (_tmp, root) = checked_repo();
        let dir = root.join(DIR);
        // An entry whose path is gone and whose symbol nothing holds.
        let mut stale = entry("Stale", "It names code that is gone.");
        stale.refs = vec!["src/gone.rs".to_string(), "NoSuchSymbol".to_string()];
        // An entry that names nothing.
        let bare = entry("Unreferenced", "It names no code.");
        // One that cannot be read, and a directory that holds children but no entry.
        for (id, text) in [
            ("stale", stale.render().unwrap()),
            ("unreferenced", bare.render().unwrap()),
            ("malformed", "No metadata block.\n".to_string()),
            (
                "bare/child",
                entry("A child", "Under a directory without an entry.")
                    .render()
                    .unwrap(),
            ),
        ] {
            let path = dir.join(format!("{id}.md"));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        crate::util::test_support::git(&root, &["add", "-A"]);
        let reported = check(&root);
        for expected in [
            "- bare: (malformed: it has no entry file) (1 child)",
            "- malformed: (malformed: it has no metadata block)",
            "- stale: refs src/gone.rs, which does not exist",
            "- stale: refs NoSuchSymbol, which no file holds",
            "- unreferenced: no refs, so nothing names the code it describes",
            "- bare/child: no refs, so nothing names the code it describes",
        ] {
            assert!(reported.contains(expected), "{expected}\n{reported}");
        }
        // The entry that names code that exists is not reported.
        assert!(!reported.contains("- good:"), "{reported}");
        assert!(
            reported.starts_with("Knowledge check: 6 problems"),
            "{reported}"
        );
    }

    #[test]
    fn check_explains_a_path_and_a_symbol_joined_into_one_ref() {
        let (_tmp, root) = checked_repo();
        let mut joined = entry("Joined", "It joins a path and a symbol.");
        joined.refs = vec!["src/main.rs::ToolContext".to_string()];
        std::fs::write(root.join(DIR).join("joined.md"), joined.render().unwrap()).unwrap();
        crate::util::test_support::git(&root, &["add", "-A"]);
        let reported = check(&root);
        assert!(
            reported.contains(
                "- joined: refs src/main.rs::ToolContext, which is a path and a symbol joined; \
                 write them as two refs"
            ),
            "{reported}"
        );
    }

    #[test]
    fn check_finds_a_symbol_in_a_file_the_turn_has_not_committed_yet() {
        let mut good = entry("A good entry", "It names code written in the same turn.");
        good.refs = vec!["src/fresh.rs".to_string(), "FreshSymbol".to_string()];
        let (_tmp, root) = staged_repo(&rendered(&[("good", good)]));
        std::fs::write(root.join("src/fresh.rs"), "fn FreshSymbol() {}\n").unwrap();
        assert_eq!(
            check(&root),
            "The knowledge base checks out: 1 entry, none with a problem.\n"
        );
    }

    #[test]
    fn check_pages_a_long_list_of_problems() {
        let files: Vec<(String, String)> = (0..CHECK_MAX_PROBLEMS + 5)
            .map(|i| {
                (
                    format!("e{i:02}"),
                    entry("No refs", "It names no code.").render().unwrap(),
                )
            })
            .collect();
        let files: Vec<(&str, String)> = files
            .iter()
            .map(|(id, text)| (id.as_str(), text.clone()))
            .collect();
        let (_tmp, root) = store_of(&files);
        let first = check(&root);
        assert!(
            first.starts_with(&format!(
                "Knowledge check: {} problems (page 1 of 2).",
                CHECK_MAX_PROBLEMS + 5
            )),
            "{first}"
        );
        assert!(first.ends_with("\nMore problems: check page=2\n"));
        let second = run(&root, &serde_json::json!({"action": "check", "page": 2})).unwrap();
        assert_eq!(second.matches("no refs").count(), 5, "{second}");
    }

    #[test]
    fn a_symbol_ref_is_looked_up_by_its_last_segment() {
        assert_eq!(symbol_name("Store::entry"), "entry");
        assert_eq!(symbol_name("RefKind::Branch"), "Branch");
        assert_eq!(symbol_name("grep"), "grep");
        assert!(is_path("src/agent/tools.rs"));
        assert!(is_path("Cargo.toml"));
        assert!(!is_path("ToolContext"));
        assert!(!is_path("Store::entry"));
    }

    #[test]
    fn check_is_offered_to_a_feedback_turn() {
        let actions = spec(Access::Read).schema["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|action| action.as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert!(actions.contains(&"check".to_string()));
    }
}
