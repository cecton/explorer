use camino::{Utf8Path, Utf8PathBuf};
use std::collections::HashMap;
use std::sync::Arc;

/// A node's directory name, unique in the whole tree: `2026-10-05T14-32-07Z`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(Arc<str>);

impl NodeId {
    pub fn new(name: &str) -> Self {
        Self(Arc::from(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Creation time encoded in the id, as seconds since the Unix epoch (UTC).
    fn created_secs(&self) -> Option<i64> {
        let stamp = self.0.get(..20)?;
        let bytes = stamp.as_bytes();
        if bytes[4] != b'-'
            || bytes[7] != b'-'
            || bytes[10] != b'T'
            || bytes[13] != b'-'
            || bytes[16] != b'-'
            || bytes[19] != b'Z'
        {
            return None;
        }
        let year: i64 = stamp.get(0..4)?.parse().ok()?;
        let month: u32 = stamp.get(5..7)?.parse().ok()?;
        let day: u32 = stamp.get(8..10)?.parse().ok()?;
        let hour: i64 = stamp.get(11..13)?.parse().ok()?;
        let minute: i64 = stamp.get(14..16)?.parse().ok()?;
        let second: i64 = stamp.get(17..19)?.parse().ok()?;
        let days = crate::git::days_from_civil(year, month, day);
        Some(days * 86_400 + hour * 3600 + minute * 60 + second)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeType {
    Feedback,
    Commit,
    Compact,
}

impl NodeType {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "feedback" => Some(Self::Feedback),
            "commit" => Some(Self::Commit),
            "compact" => Some(Self::Compact),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Feedback => "feedback",
            Self::Commit => "commit",
            Self::Compact => "compact",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeStatus {
    Running,
    Done,
    Failed,
    Cancelled,
}

impl NodeStatus {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Content of a node's `metadata.md`: flat `key: value` lines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub node_type: Option<NodeType>,
    pub endpoint: Option<String>,
    pub kind: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub created: Option<String>,
    pub status: Option<NodeStatus>,
    pub head_before: Option<String>,
    pub head_after: Option<String>,
    pub commits: Vec<String>,
    pub stash: Option<String>,
    pub favorite: bool,
    pub command: Option<String>,
    pub session_id: Option<String>,
    pub tokens_in: Option<u64>,
    pub error: Option<String>,
}

impl Metadata {
    pub fn parse(text: &str) -> Self {
        let mut meta = Self::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            match key.trim() {
                "type" => meta.node_type = NodeType::parse(value),
                "endpoint" => meta.endpoint = Some(value.to_string()),
                "kind" => meta.kind = Some(value.to_string()),
                "model" => meta.model = Some(value.to_string()),
                "effort" => meta.effort = Some(value.to_string()),
                "created" => meta.created = Some(value.to_string()),
                "status" => meta.status = NodeStatus::parse(value),
                "head-before" => meta.head_before = Some(value.to_string()),
                "head-after" => meta.head_after = Some(value.to_string()),
                "commits" => {
                    meta.commits = value.split_whitespace().map(str::to_string).collect();
                }
                "stash" => meta.stash = Some(value.to_string()),
                "favorite" => meta.favorite = value == "true",
                "command" => meta.command = Some(value.to_string()),
                "session-id" => meta.session_id = Some(value.to_string()),
                "tokens-in" => meta.tokens_in = value.parse().ok(),
                "error" => meta.error = Some(value.to_string()),
                _ => {}
            }
        }
        meta
    }

    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let mut push = |key: &str, value: Option<&str>| {
            if let Some(value) = value {
                out.push_str(key);
                out.push_str(": ");
                out.push_str(&value.replace('\n', " "));
                out.push('\n');
            }
        };
        let commits = self.commits.join(" ");
        let tokens_in = self.tokens_in.map(|n| n.to_string());
        push("type", self.node_type.map(NodeType::as_str));
        push("endpoint", self.endpoint.as_deref());
        push("kind", self.kind.as_deref());
        push("model", self.model.as_deref());
        push("effort", self.effort.as_deref());
        push("created", self.created.as_deref());
        push("status", self.status.map(NodeStatus::as_str));
        push("head-before", self.head_before.as_deref());
        push("head-after", self.head_after.as_deref());
        push("commits", (!commits.is_empty()).then_some(commits.as_str()));
        push("stash", self.stash.as_deref());
        push("favorite", self.favorite.then_some("true"));
        push("command", self.command.as_deref());
        push("session-id", self.session_id.as_deref());
        push("tokens-in", tokens_in.as_deref());
        push("error", self.error.as_deref());
        out
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub parent: Option<NodeId>,
    /// Oldest first.
    pub children: Vec<NodeId>,
    pub meta: Metadata,
    pub title: Option<String>,
    pub summary: Option<String>,
    /// First line of `prompt.md`, kept only when the node has no title.
    prompt_first_line: Option<String>,
    /// Creation time in local time, `YYYY-MM-DD HH:MM`, derived from the id.
    pub created_display: String,
}

impl Node {
    pub fn display_title(&self) -> &str {
        self.title
            .as_deref()
            .or(self.prompt_first_line.as_deref())
            .unwrap_or("(no prompt)")
    }

    fn read(dir: &Utf8Path, id: &NodeId, parent: Option<NodeId>, children: Vec<NodeId>) -> Self {
        let read_trimmed = |name: &str| {
            std::fs::read_to_string(dir.join(name))
                .ok()
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
        };
        let mut meta = read_trimmed("metadata.md")
            .map(|text| Metadata::parse(&text))
            .unwrap_or_default();
        if meta.status == Some(NodeStatus::Running) {
            meta.status = Some(NodeStatus::Failed);
            meta.error = Some("interrupted".to_string());
            if let Err(e) = write_atomic(&dir.join("metadata.md"), &meta.to_text()) {
                tracing::warn!("failed to rewrite interrupted node {}: {e}", id.as_str());
            }
        }
        let title = read_trimmed("title.md").map(|text| single_line(&text));
        let prompt_first_line = match title {
            Some(_) => None,
            None => read_trimmed("prompt.md")
                .and_then(|text| text.lines().next().map(str::to_string))
                .filter(|line| !line.is_empty()),
        };
        let created_display = id
            .created_secs()
            .map(|secs| crate::git::format_log_time(secs, local_offset_secs(secs)))
            .unwrap_or_default();
        Self {
            parent,
            children,
            meta,
            title,
            summary: read_trimmed("summary.md"),
            prompt_first_line,
            created_display,
        }
    }
}

/// Where a node's turn finds its context: what to replay and what its parent left.
#[derive(Debug, PartialEq, Eq)]
pub struct TurnContext {
    pub node_dir: Utf8PathBuf,
    /// Directories of the ancestors to replay, oldest first. Starts after the nearest
    /// compact ancestor, whose summary stands in for everything before it.
    pub ancestor_dirs: Vec<Utf8PathBuf>,
    /// Directory of the parent when it is a compact node.
    pub compacted_parent: Option<Utf8PathBuf>,
    /// HEAD when the parent's turn ended.
    pub parent_head: Option<String>,
    /// Claude Code session of the parent, when it ran there and is not a compact node.
    pub parent_session: Option<String>,
}

/// The conversation tree of one repository. Holds metadata, titles and summaries
/// only; prompts, replies and transcripts stay on disk.
#[derive(Clone, Debug, Default)]
pub struct ConversationTree {
    dir: Utf8PathBuf,
    nodes: HashMap<NodeId, Node>,
    /// Oldest first.
    roots: Vec<NodeId>,
}

impl ConversationTree {
    pub fn load(dir: &Utf8Path) -> Self {
        let mut tree = Self {
            dir: dir.to_owned(),
            nodes: HashMap::new(),
            roots: Vec::new(),
        };
        tree.roots = tree.load_children(dir, None);
        tree
    }

    fn load_children(&mut self, dir: &Utf8Path, parent: Option<&NodeId>) -> Vec<NodeId> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut ids = Vec::new();
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let id = NodeId::new(&name);
            let node_dir = dir.join(&name);
            let children = self.load_children(&node_dir, Some(&id));
            let node = Node::read(&node_dir, &id, parent.cloned(), children);
            self.nodes.insert(id.clone(), node);
            ids.push(id);
        }
        ids.sort();
        ids
    }

    pub fn node(&self, id: &NodeId) -> Option<&Node> {
        self.nodes.get(id)
    }

    pub fn roots(&self) -> &[NodeId] {
        &self.roots
    }

    /// Directory of `id`: the tree root joined with every ancestor's name.
    pub fn node_dir(&self, id: &NodeId) -> Option<Utf8PathBuf> {
        let mut chain = vec![id];
        let mut current = self.nodes.get(id)?;
        while let Some(parent) = current.parent.as_ref() {
            chain.push(parent);
            current = self.nodes.get(parent)?;
        }
        let mut dir = self.dir.clone();
        for ancestor in chain.into_iter().rev() {
            dir.push(ancestor.as_str());
        }
        Some(dir)
    }

    /// `id` preceded by all its ancestors, root first.
    pub fn lineage(&self, id: &NodeId) -> Vec<NodeId> {
        let mut chain = vec![id.clone()];
        let mut current = self.nodes.get(id);
        while let Some(parent) = current.and_then(|node| node.parent.as_ref()) {
            chain.push(parent.clone());
            current = self.nodes.get(parent);
        }
        chain.reverse();
        chain
    }

    pub fn turn_context(&self, id: &NodeId) -> Option<TurnContext> {
        let lineage = self.lineage(id);
        let (_, ancestors) = lineage.split_last()?;
        let is_compact = |ancestor: &NodeId| {
            self.nodes
                .get(ancestor)
                .is_some_and(|node| node.meta.node_type == Some(NodeType::Compact))
        };
        let cut = ancestors.iter().rposition(is_compact);
        let replayed = &ancestors[cut.map_or(0, |index| index + 1)..];
        let parent = ancestors.last();
        let parent_is_compact = parent.is_some_and(is_compact);
        let parent_meta = parent
            .and_then(|parent| self.nodes.get(parent))
            .map(|node| &node.meta);
        Some(TurnContext {
            node_dir: self.node_dir(id)?,
            ancestor_dirs: replayed
                .iter()
                .filter_map(|ancestor| self.node_dir(ancestor))
                .collect(),
            compacted_parent: parent
                .filter(|_| parent_is_compact)
                .and_then(|parent| self.node_dir(parent)),
            parent_head: parent_meta.and_then(|meta| meta.head_after.clone()),
            parent_session: parent_meta
                .filter(|meta| {
                    !parent_is_compact
                        && meta.kind.as_deref() == Some(crate::agent::CLAUDE_CODE_KIND)
                })
                .and_then(|meta| meta.session_id.clone()),
        })
    }

    /// Creates a node under `parent` (or a new root): its directory, `prompt.md` and
    /// `metadata.md` are on disk when this returns.
    pub fn create_node(
        &mut self,
        parent: Option<&NodeId>,
        now_secs: i64,
        mut meta: Metadata,
        prompt: &str,
    ) -> std::io::Result<NodeId> {
        let stamp = utc_stamp(now_secs);
        let mut id = NodeId::new(&stamp.replace(':', "-"));
        let mut suffix = 2;
        while self.nodes.contains_key(&id) {
            id = NodeId::new(&format!("{}-{suffix}", stamp.replace(':', "-")));
            suffix += 1;
        }
        let dir = match parent {
            Some(parent) => self.node_dir(parent).ok_or_else(unknown_node)?,
            None => self.dir.clone(),
        }
        .join(id.as_str());
        meta.created = Some(stamp);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("prompt.md"), prompt)?;
        write_atomic(&dir.join("metadata.md"), &meta.to_text())?;

        let node = Node {
            parent: parent.cloned(),
            children: Vec::new(),
            meta,
            title: None,
            summary: None,
            prompt_first_line: prompt
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_string),
            created_display: crate::git::format_log_time(now_secs, local_offset_secs(now_secs)),
        };
        self.nodes.insert(id.clone(), node);
        match parent.and_then(|parent| self.nodes.get_mut(parent)) {
            Some(parent) => parent.children.push(id.clone()),
            None => self.roots.push(id.clone()),
        }
        Ok(id)
    }

    /// Prepares `id` for running its turn again in place: the previous attempt's reply
    /// and transcript are removed, and `change` rewrites the metadata.
    pub fn restart_node(
        &mut self,
        id: &NodeId,
        change: impl FnOnce(&mut Metadata),
    ) -> std::io::Result<()> {
        let dir = self.node_dir(id).ok_or_else(unknown_node)?;
        for name in ["reply.md", "transcript.jsonl"] {
            match std::fs::remove_file(dir.join(name)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        self.update_meta(id, change)
    }

    /// Changes the metadata of `id` in memory and writes it through.
    pub fn update_meta(
        &mut self,
        id: &NodeId,
        change: impl FnOnce(&mut Metadata),
    ) -> std::io::Result<()> {
        let dir = self.node_dir(id).ok_or_else(unknown_node)?;
        let node = self.nodes.get_mut(id).ok_or_else(unknown_node)?;
        change(&mut node.meta);
        write_atomic(&dir.join("metadata.md"), &node.meta.to_text())
    }

    pub fn set_title(&mut self, id: &NodeId, title: String) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.title = Some(title);
        }
    }

    pub fn set_summary(&mut self, id: &NodeId, summary: String) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.summary = Some(summary);
        }
    }

    /// Number of nodes in the subtree rooted at `id`, itself included.
    pub fn subtree_len(&self, id: &NodeId) -> usize {
        self.nodes.get(id).map_or(0, |node| {
            1 + node
                .children
                .iter()
                .map(|child| self.subtree_len(child))
                .sum::<usize>()
        })
    }

    pub fn toggle_favorite(&mut self, id: &NodeId) -> std::io::Result<()> {
        self.update_meta(id, |meta| meta.favorite = !meta.favorite)
    }

    /// Removes `id` and its descendants from disk and memory; returns every removed id.
    pub fn delete(&mut self, id: &NodeId) -> std::io::Result<Vec<NodeId>> {
        let dir = self.node_dir(id).ok_or_else(unknown_node)?;
        std::fs::remove_dir_all(&dir)?;
        let mut removed = Vec::new();
        let mut pending = vec![id.clone()];
        while let Some(next) = pending.pop() {
            if let Some(node) = self.nodes.remove(&next) {
                pending.extend(node.children.iter().cloned());
                if &next == id {
                    match node.parent.as_ref().and_then(|p| self.nodes.get_mut(p)) {
                        Some(parent) => parent.children.retain(|child| child != id),
                        None => self.roots.retain(|root| root != id),
                    }
                }
                removed.push(next);
            }
        }
        Ok(removed)
    }
}

/// A node's `prompt.md` and `reply.md`; a missing file reads as empty text.
pub fn read_prompt_and_reply(dir: &Utf8Path) -> (String, String) {
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap_or_default();
    (read("prompt.md"), read("reply.md"))
}

fn unknown_node() -> std::io::Error {
    std::io::Error::other("unknown conversation node")
}

fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix time.
fn utc_stamp(secs: i64) -> String {
    let (year, month, day) = crate::git::civil_from_days(secs.div_euclid(86_400));
    let of_day = secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        of_day / 3600,
        (of_day % 3600) / 60,
        of_day % 60
    )
}

pub fn write_atomic(path: &Utf8Path, content: &str) -> std::io::Result<()> {
    let tmp: Utf8PathBuf = format!("{path}.tmp").into();
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

fn local_offset_secs(time_secs: i64) -> i32 {
    let time = time_secs as libc::time_t;
    // SAFETY: `tm` is plain data that `localtime_r` fully initializes on success, and
    // both pointers outlive the call.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&time, &mut tm).is_null() {
            return 0;
        }
        tm
    };
    tm.tm_gmtoff as i32
}

/// `<data_dir>/conversations/<repo-hash>/` for the repository at `repo_root`.
pub fn conversations_dir(repo_root: &Utf8Path) -> Option<Utf8PathBuf> {
    let hash = crate::session::repo_hash(repo_root)?;
    directories::ProjectDirs::from("", "", "explorer").and_then(|dirs| {
        Utf8PathBuf::from_path_buf(dirs.data_dir().join("conversations").join(hash)).ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_node(dir: &Utf8Path, files: &[(&str, &str)]) {
        std::fs::create_dir_all(dir).unwrap();
        for (name, content) in files {
            std::fs::write(dir.join(name), content).unwrap();
        }
    }

    fn temp_root() -> (tempfile::TempDir, Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        (tmp, root)
    }

    #[test]
    fn metadata_round_trips_every_key() {
        let meta = Metadata {
            node_type: Some(NodeType::Commit),
            endpoint: Some("kimi".into()),
            kind: Some("openai".into()),
            model: Some("org/kimi-k2".into()),
            effort: Some("high".into()),
            created: Some("2026-10-05T14:32:07Z".into()),
            status: Some(NodeStatus::Done),
            head_before: Some("aaaa".into()),
            head_after: Some("bbbb".into()),
            commits: vec!["bbbb".into(), "cccc".into()],
            stash: Some("dddd".into()),
            favorite: true,
            command: Some("review".into()),
            session_id: Some("1234-uuid".into()),
            tokens_in: Some(84_210),
            error: Some("rate limited: try later".into()),
        };
        assert_eq!(Metadata::parse(&meta.to_text()), meta);
    }

    #[test]
    fn metadata_omits_unset_keys_and_ignores_unknown_ones() {
        assert_eq!(Metadata::default().to_text(), "");
        let meta = Metadata::parse("status: done\nwat: 1\nnot a pair\n");
        assert_eq!(meta.status, Some(NodeStatus::Done));
        assert!(!meta.favorite);
    }

    #[test]
    fn load_builds_tree_and_tolerates_missing_files() {
        let (_tmp, root) = temp_root();
        let a = root.join("2026-10-05T14-32-07Z");
        write_node(
            &a,
            &[
                ("metadata.md", "type: commit\nstatus: done\ncommits: abc\n"),
                ("title.md", "Add  the\nthing\n"),
                ("summary.md", "Adds the thing.\n"),
                ("prompt.md", "please add the thing"),
            ],
        );
        write_node(
            &a.join("2026-10-06T09-00-00Z"),
            &[("prompt.md", "second fork\nmore")],
        );
        write_node(&a.join("2026-10-05T15-00-00Z"), &[]);
        write_node(&root.join("2026-10-01T08-00-00Z"), &[("prompt.md", "old")]);

        let tree = ConversationTree::load(&root);
        let ids: Vec<&str> = tree.roots().iter().map(NodeId::as_str).collect();
        assert_eq!(ids, ["2026-10-01T08-00-00Z", "2026-10-05T14-32-07Z"]);

        let node_id = NodeId::new("2026-10-05T14-32-07Z");
        let node = tree.node(&node_id).unwrap();
        assert_eq!(node.display_title(), "Add the thing");
        assert_eq!(node.summary.as_deref(), Some("Adds the thing."));
        assert_eq!(node.meta.commits, ["abc"]);
        let children: Vec<&str> = node.children.iter().map(NodeId::as_str).collect();
        assert_eq!(children, ["2026-10-05T15-00-00Z", "2026-10-06T09-00-00Z"]);

        let fork_id = NodeId::new("2026-10-06T09-00-00Z");
        let fork = tree.node(&fork_id).unwrap();
        assert_eq!(fork.display_title(), "second fork");
        assert_eq!(fork.parent.as_ref(), Some(&node_id));
        let bare = tree.node(&NodeId::new("2026-10-05T15-00-00Z")).unwrap();
        assert_eq!(bare.display_title(), "(no prompt)");
        assert_eq!(tree.subtree_len(&node_id), 3);
        assert_eq!(
            tree.node_dir(&fork_id).unwrap(),
            a.join("2026-10-06T09-00-00Z")
        );
    }

    #[test]
    fn load_marks_running_nodes_as_interrupted() {
        let (_tmp, root) = temp_root();
        let dir = root.join("2026-10-05T14-32-07Z");
        write_node(&dir, &[("metadata.md", "status: running\n")]);

        let tree = ConversationTree::load(&root);
        let node = tree.node(&NodeId::new("2026-10-05T14-32-07Z")).unwrap();
        assert_eq!(node.meta.status, Some(NodeStatus::Failed));
        assert_eq!(node.meta.error.as_deref(), Some("interrupted"));
        let on_disk = std::fs::read_to_string(dir.join("metadata.md")).unwrap();
        assert_eq!(on_disk, "status: failed\nerror: interrupted\n");
    }

    #[test]
    fn toggle_favorite_writes_through() {
        let (_tmp, root) = temp_root();
        let dir = root.join("2026-10-05T14-32-07Z");
        write_node(&dir, &[("metadata.md", "status: done\n")]);
        let mut tree = ConversationTree::load(&root);
        let id = NodeId::new("2026-10-05T14-32-07Z");

        tree.toggle_favorite(&id).unwrap();
        assert!(tree.node(&id).unwrap().meta.favorite);
        let on_disk = std::fs::read_to_string(dir.join("metadata.md")).unwrap();
        assert_eq!(on_disk, "status: done\nfavorite: true\n");

        tree.toggle_favorite(&id).unwrap();
        let on_disk = std::fs::read_to_string(dir.join("metadata.md")).unwrap();
        assert_eq!(on_disk, "status: done\n");
    }

    #[test]
    fn delete_removes_subtree_from_disk_and_memory() {
        let (_tmp, root) = temp_root();
        let a = root.join("2026-10-05T14-32-07Z");
        let b = a.join("2026-10-05T15-00-00Z");
        write_node(&b.join("2026-10-05T16-00-00Z"), &[]);
        write_node(&a.join("2026-10-06T09-00-00Z"), &[]);
        let mut tree = ConversationTree::load(&root);

        let removed = tree.delete(&NodeId::new("2026-10-05T15-00-00Z")).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(!b.exists());
        let parent = tree.node(&NodeId::new("2026-10-05T14-32-07Z")).unwrap();
        assert_eq!(parent.children, [NodeId::new("2026-10-06T09-00-00Z")]);

        tree.delete(&NodeId::new("2026-10-05T14-32-07Z")).unwrap();
        assert!(tree.roots().is_empty());
        assert!(tree.node(&NodeId::new("2026-10-06T09-00-00Z")).is_none());
    }

    #[test]
    fn create_node_persists_before_returning_and_keeps_ids_unique() {
        let (_tmp, root) = temp_root();
        let mut tree = ConversationTree::load(&root);
        let meta = Metadata {
            node_type: Some(NodeType::Feedback),
            status: Some(NodeStatus::Running),
            ..Default::default()
        };

        let first = tree
            .create_node(None, 86_401, meta.clone(), "\n  hello there\nmore")
            .unwrap();
        assert_eq!(first.as_str(), "1970-01-02T00-00-01Z");
        let dir = root.join(first.as_str());
        assert_eq!(
            std::fs::read_to_string(dir.join("prompt.md")).unwrap(),
            "\n  hello there\nmore"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("metadata.md")).unwrap(),
            "type: feedback\ncreated: 1970-01-02T00:00:01Z\nstatus: running\n"
        );
        assert_eq!(tree.node(&first).unwrap().display_title(), "hello there");

        let twin = tree.create_node(None, 86_401, meta.clone(), "x").unwrap();
        assert_eq!(twin.as_str(), "1970-01-02T00-00-01Z-2");
        let child = tree.create_node(Some(&first), 86_401, meta, "y").unwrap();
        assert_eq!(child.as_str(), "1970-01-02T00-00-01Z-3");
        assert!(dir.join(child.as_str()).join("prompt.md").exists());
        assert_eq!(tree.lineage(&child), [first.clone(), child.clone()]);
        assert_eq!(tree.roots(), [first, twin]);
    }

    #[test]
    fn turn_context_replays_ancestors_after_the_nearest_compact_node() {
        let (_tmp, root) = temp_root();
        let a = root.join("a1");
        let b = a.join("a2");
        let c = b.join("a3");
        let d = c.join("a4");
        write_node(&a, &[("metadata.md", "head-after: h1\n")]);
        write_node(
            &b,
            &[(
                "metadata.md",
                "type: compact\nkind: claude-code\nsession-id: s2\n",
            )],
        );
        write_node(
            &c,
            &[(
                "metadata.md",
                "kind: claude-code\nsession-id: s3\nhead-after: h3\n",
            )],
        );
        write_node(&d, &[]);
        let tree = ConversationTree::load(&root);

        let after_compact = tree.turn_context(&NodeId::new("a3")).unwrap();
        assert!(after_compact.ancestor_dirs.is_empty());
        assert_eq!(after_compact.compacted_parent.as_ref(), Some(&b));
        assert_eq!(after_compact.parent_session, None);

        let later = tree.turn_context(&NodeId::new("a4")).unwrap();
        assert_eq!(later.node_dir, d);
        assert_eq!(later.ancestor_dirs, [c]);
        assert_eq!(later.compacted_parent, None);
        assert_eq!(later.parent_session.as_deref(), Some("s3"));
        assert_eq!(later.parent_head.as_deref(), Some("h3"));

        let compacting = tree.turn_context(&NodeId::new("a2")).unwrap();
        assert_eq!(compacting.ancestor_dirs, [a]);
        assert_eq!(compacting.parent_head.as_deref(), Some("h1"));
        let first = tree.turn_context(&NodeId::new("a1")).unwrap();
        assert!(first.ancestor_dirs.is_empty());
        assert_eq!(first.parent_head, None);
    }

    #[test]
    fn restart_node_drops_the_previous_attempt_but_keeps_the_prompt() {
        let (_tmp, root) = temp_root();
        let dir = root.join("a1");
        write_node(
            &dir,
            &[
                (
                    "metadata.md",
                    "status: failed\nerror: boom\nfavorite: true\n",
                ),
                ("prompt.md", "do it"),
                ("reply.md", "partial"),
                ("transcript.jsonl", "{}\n"),
            ],
        );
        let mut tree = ConversationTree::load(&root);
        let id = NodeId::new("a1");
        tree.restart_node(&id, |meta| {
            meta.status = Some(NodeStatus::Running);
            meta.error = None;
        })
        .unwrap();

        assert!(!dir.join("reply.md").exists());
        assert!(!dir.join("transcript.jsonl").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("prompt.md")).unwrap(),
            "do it"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("metadata.md")).unwrap(),
            "status: running\nfavorite: true\n"
        );
    }

    #[test]
    fn update_meta_writes_through() {
        let (_tmp, root) = temp_root();
        let mut tree = ConversationTree::load(&root);
        let id = tree
            .create_node(None, 0, Metadata::default(), "prompt")
            .unwrap();
        tree.update_meta(&id, |meta| meta.status = Some(NodeStatus::Done))
            .unwrap();
        let reloaded = ConversationTree::load(&root);
        assert_eq!(
            reloaded.node(&id).unwrap().meta.status,
            Some(NodeStatus::Done)
        );
    }

    #[test]
    fn created_secs_parses_the_id_timestamp() {
        let id = NodeId::new("1970-01-02T00-00-01Z-2");
        assert_eq!(id.created_secs(), Some(86_401));
        assert_eq!(NodeId::new("nonsense").created_secs(), None);
        assert_eq!(NodeId::new("2026x10-05T14-32-07Z").created_secs(), None);
    }
}
