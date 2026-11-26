//! The knowledge base of a repository: short entries about its code, one markdown file
//! each, under `docs/knowledge/`. An entry `x.md` holds its children in a directory `x/`,
//! so the store is a tree, and the `knowledge` tool serves it in bounded pages the way the
//! `conversation` tool serves the conversation tree.
//!
//! The files are the store: nothing is cached between calls, so an entry written by a turn
//! is read by the next one. An entry Explorer did not write is reported as malformed rather
//! than read, so the layout and the formatter stay one fixed point.

use std::collections::BTreeMap;

/// Where the store lives, relative to the repository root.
pub(crate) const DIR: &str = "docs/knowledge";
/// Entries listed on one page of `toc`.
const TOC_PAGE: usize = 30;
/// Entries `read` returns at once.
const READ_MAX_IDS: usize = 10;
/// Entries `search` returns at once.
const SEARCH_MAX_HITS: usize = 20;

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

/// Every entry under `docs/knowledge/`, by id, read when the tool is called.
struct Store {
    nodes: BTreeMap<String, Node>,
}

impl Store {
    /// Reads the store of `root`, empty when it is missing or reached through a link.
    fn load(root: &camino::Utf8Path) -> Self {
        let dir = root.join(DIR);
        let mut store = Self {
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

/// Runs one call of the `knowledge` tool. A failure is returned as text for the model.
pub(crate) fn run(root: &camino::Utf8Path, args: &serde_json::Value) -> Result<String, String> {
    let store = Store::load(root);
    match str_arg(args, "action")? {
        "toc" => store.toc(
            args.get("id").and_then(serde_json::Value::as_str),
            usize_arg(args, "page").unwrap_or(1),
        ),
        "read" => store.read(&ids_arg(args)?),
        "search" => store.search(str_arg(args, "query")?),
        other => Err(format!("unknown action: {other}")),
    }
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

    #[test]
    fn a_directory_never_replaces_its_entry_file() {
        let file = || Node::File {
            text: String::new(),
            entry: Ok(entry("t", "s")),
        };
        let mut file_first = Store {
            nodes: BTreeMap::new(),
        };
        file_first.nodes.insert("a".to_string(), file());
        file_first.insert_dir("a");
        assert!(matches!(file_first.nodes["a"], Node::File { .. }));

        let mut dir_first = Store {
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
            "unknown action: write"
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
}
