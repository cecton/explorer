use std::collections::HashMap;
use std::sync::Arc;

pub(crate) mod agents;
pub mod files;
pub mod metadata;
pub(crate) mod toc;

#[cfg(test)]
pub(crate) mod test_support;

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
        crate::util::time::parse_utc_stamp(&self.0)
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

/// What a turn cancelled before it said anything records as its error.
pub const CANCELLED_ERROR: &str = "cancelled by the user";

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

#[derive(Clone, Debug)]
pub struct Node {
    pub parent: Option<NodeId>,
    /// Oldest first.
    pub children: Vec<NodeId>,
    pub meta: metadata::Metadata,
    pub title: Option<String>,
    pub summary: Option<String>,
    /// Sub-agent runs of the node's turn, oldest first.
    pub agents: Vec<agents::AgentRecord>,
    /// First line of the prompt as typed, kept only when the node has no title.
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

    fn read(
        dir: &camino::Utf8Path,
        id: &NodeId,
        parent: Option<NodeId>,
        children: Vec<NodeId>,
    ) -> Self {
        let title = files::read_title(dir);
        let prompt_first_line = match title {
            Some(_) => None,
            None => files::read_input_file(dir)
                .or_else(|| files::read_prompt_file(dir))
                .and_then(|text| text.lines().next().map(str::to_string))
                .filter(|line| !line.is_empty()),
        };
        let created_display = id
            .created_secs()
            .map(|secs| {
                crate::util::time::format_log_time(secs, crate::util::time::local_offset_secs(secs))
            })
            .unwrap_or_default();
        Self {
            parent,
            children,
            meta: metadata::read(dir),
            title,
            summary: files::read_summary(dir),
            agents: agents::read(dir),
            prompt_first_line,
            created_display,
        }
    }
}

/// Where a node's turn finds its context: what to replay and what its parent left.
#[derive(Debug, PartialEq, Eq)]
pub struct TurnContext {
    pub node_dir: camino::Utf8PathBuf,
    /// Directories of the ancestors to replay, oldest first. Starts after the nearest
    /// compact ancestor, whose summary stands in for everything before it.
    pub ancestor_dirs: Vec<camino::Utf8PathBuf>,
    /// Directory of the parent when it is a compact node.
    pub compacted_parent: Option<camino::Utf8PathBuf>,
    /// HEAD when the parent's turn ended.
    pub parent_head: Option<String>,
    /// Claude Code session of the parent, when it ran there and is not a compact node.
    pub parent_session: Option<String>,
}

/// The conversation tree of one repository. Holds metadata, titles and summaries
/// only; prompts, replies and transcripts stay on disk.
#[derive(Clone, Debug)]
pub struct ConversationTree {
    dir: camino::Utf8PathBuf,
    nodes: HashMap<NodeId, Node>,
    /// Oldest first.
    roots: Vec<NodeId>,
    revision: u64,
}

/// Handed out process-wide rather than per tree: `ConversationTree::load` replaces a tree
/// as a whole, so a counter kept in the tree would give its successor the number of the
/// tree it replaces and a cache of that tree would outlive it.
static NEXT_REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_revision() -> u64 {
    NEXT_REVISION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Default for ConversationTree {
    fn default() -> Self {
        Self {
            dir: camino::Utf8PathBuf::default(),
            nodes: HashMap::new(),
            roots: Vec::new(),
            revision: next_revision(),
        }
    }
}

impl ConversationTree {
    pub fn load(dir: &camino::Utf8Path) -> Self {
        let mut tree = Self {
            dir: dir.to_owned(),
            ..Self::default()
        };
        tree.roots = tree.load_children(dir, None);
        tree
    }

    /// Moves with every change to the tree, so what a reader cached of it can tell when
    /// it is stale.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn touch(&mut self) {
        self.revision = next_revision();
    }

    fn load_children(&mut self, dir: &camino::Utf8Path, parent: Option<&NodeId>) -> Vec<NodeId> {
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
            if name == files::AGENTS_DIR || name == files::PROCESSES_DIR {
                continue;
            }
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

    pub fn nodes(&self) -> impl Iterator<Item = (&NodeId, &Node)> {
        self.nodes.iter()
    }

    /// Directory of `id`: the tree root joined with every ancestor's name.
    pub fn node_dir(&self, id: &NodeId) -> Option<camino::Utf8PathBuf> {
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
                        && meta.kind.as_deref() == Some(crate::endpoint::CLAUDE_CODE_KIND)
                })
                .and_then(|meta| meta.session_id.clone()),
        })
    }

    /// Creates a node under `parent` (or a new root): its directory, `prompt.md` and
    /// `metadata.md` are on disk when this returns. `input` is the prompt as typed, when
    /// a slash command expanded it into `prompt`; it is stored as `input.md`.
    pub fn create_node(
        &mut self,
        parent: Option<&NodeId>,
        now_secs: i64,
        mut meta: metadata::Metadata,
        prompt: &str,
        input: Option<&str>,
    ) -> std::io::Result<NodeId> {
        self.touch();
        let stamp = crate::util::time::utc_stamp(now_secs);
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
        std::fs::write(dir.join(files::PROMPT), prompt)?;
        if let Some(input) = input {
            std::fs::write(dir.join(files::INPUT), input)?;
        }
        metadata::write(&dir, &meta)?;

        let node = Node {
            parent: parent.cloned(),
            children: Vec::new(),
            meta,
            title: None,
            summary: None,
            agents: Vec::new(),
            prompt_first_line: input
                .unwrap_or(prompt)
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_string),
            created_display: crate::util::time::format_log_time(
                now_secs,
                crate::util::time::local_offset_secs(now_secs),
            ),
        };
        self.nodes.insert(id.clone(), node);
        match parent.and_then(|parent| self.nodes.get_mut(parent)) {
            Some(parent) => parent.children.push(id.clone()),
            None => self.roots.push(id.clone()),
        }
        Ok(id)
    }

    /// Changes the metadata of `id` in memory and writes it through.
    pub fn update_meta(
        &mut self,
        id: &NodeId,
        change: impl FnOnce(&mut metadata::Metadata),
    ) -> std::io::Result<()> {
        let dir = self.node_dir(id).ok_or_else(unknown_node)?;
        let node = self.nodes.get_mut(id).ok_or_else(unknown_node)?;
        change(&mut node.meta);
        let written = metadata::write(&dir, &node.meta);
        self.touch();
        written
    }

    /// Records a turn that ended early, quitting or a signal: writes its reply and
    /// records its commits and the HEAD it ended on through the one rule for those
    /// fields, so a later turn derives its base from the commit the turn stopped at.
    /// The reply is kept even when its write fails: what the turn left is worth more
    /// than the file it could not be put in.
    pub fn mark_cut_short(
        &mut self,
        id: &NodeId,
        reply: &str,
        commits: &[String],
        head_after: Option<&str>,
    ) -> std::io::Result<()> {
        let dir = self.node_dir(id).ok_or_else(unknown_node)?;
        if let Err(e) = files::write_reply(&dir, reply) {
            tracing::warn!("failed to write the reply of {}: {e}", id.as_str());
        }
        self.update_meta(id, |meta| meta.mark_cut_short(reply, commits, head_after))
    }

    pub fn set_title(&mut self, id: &NodeId, title: String) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.title = Some(title);
            self.touch();
        }
    }

    pub fn set_summary(&mut self, id: &NodeId, summary: String) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.summary = Some(summary);
            self.touch();
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
        self.touch();
        Ok(removed)
    }

    /// The straight run below `head`: the single replies that fold into it, then the fork
    /// ending the run, if any. Both are empty when `head` is itself a fork.
    pub fn thread_below<'a>(&'a self, head: &NodeId) -> (Vec<&'a NodeId>, Option<&'a NodeId>) {
        let mut hidden = Vec::new();
        let mut current = self.node(head);
        while let Some([only]) = current.map(|node| node.children.as_slice()) {
            let reply = self.node(only);
            if reply.is_some_and(|reply| reply.children.len() > 1) {
                return (hidden, Some(only));
            }
            hidden.push(only);
            current = reply;
        }
        (hidden, None)
    }

    /// Last of the replies folded into the thread head `head`: where `Right` lands.
    pub fn thread_end(&self, head: &NodeId) -> Option<NodeId> {
        self.thread_below(head).0.last().map(|id| (*id).clone())
    }

    /// The nodes on the way to `node`, itself included, that can hold a fold: forks and
    /// thread heads.
    pub fn folds_on_the_way(&self, node: &NodeId) -> Vec<NodeId> {
        let is_fork = |id: &NodeId| self.node(id).is_some_and(|node| node.children.len() > 1);
        self.lineage(node)
            .into_iter()
            .filter(|id| {
                let parent = self.node(id).and_then(|node| node.parent.as_ref());
                is_fork(id) || parent.is_none_or(is_fork)
            })
            .collect()
    }
}

pub(crate) fn unknown_node() -> std::io::Error {
    std::io::Error::other("unknown conversation node")
}

/// `<data_dir>/conversations/<repo-hash>/` for the repository at `repo_root`.
pub fn conversations_dir(repo_root: &camino::Utf8Path) -> Option<camino::Utf8PathBuf> {
    let hash = crate::util::fs::repo_hash(repo_root)?;
    Some(
        crate::util::fs::data_dir()?
            .join("conversations")
            .join(hash),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::test_support::temp_root;
    use crate::conversation::test_support::write_node;

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
        use crate::conversation::metadata::Metadata;
        let (_tmp, root) = temp_root();
        let mut tree = ConversationTree::load(&root);
        let meta = Metadata {
            node_type: Some(NodeType::Feedback),
            status: Some(NodeStatus::Running),
            ..Default::default()
        };

        let first = tree
            .create_node(None, 86_401, meta.clone(), "\n  hello there\nmore", None)
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

        let twin = tree
            .create_node(None, 86_401, meta.clone(), "x", None)
            .unwrap();
        assert_eq!(twin.as_str(), "1970-01-02T00-00-01Z-2");
        let child = tree
            .create_node(Some(&first), 86_401, meta, "y", None)
            .unwrap();
        assert_eq!(child.as_str(), "1970-01-02T00-00-01Z-3");
        assert!(dir.join(child.as_str()).join("prompt.md").exists());
        assert_eq!(tree.lineage(&child), [first.clone(), child.clone()]);
        assert_eq!(tree.roots(), [first, twin]);
    }

    #[test]
    fn create_node_keeps_the_typed_input_of_an_expanded_prompt() {
        use crate::conversation::metadata::Metadata;
        let (_tmp, root) = temp_root();
        let mut tree = ConversationTree::load(&root);

        let expanded = tree
            .create_node(
                None,
                0,
                Metadata::default(),
                "Review the change.\n\nfoo",
                Some("/review foo"),
            )
            .unwrap();
        let dir = root.join(expanded.as_str());
        assert_eq!(files::read_prompt(&dir), "Review the change.\n\nfoo");
        assert_eq!(files::read_input(&dir), "/review foo");
        assert_eq!(tree.node(&expanded).unwrap().display_title(), "/review foo");

        let typed = tree
            .create_node(None, 1, Metadata::default(), "as typed", None)
            .unwrap();
        let dir = root.join(typed.as_str());
        assert!(files::read_typed_input(&dir).is_none());
        assert_eq!(files::read_input(&dir), "as typed");

        let reloaded = ConversationTree::load(&root);
        assert_eq!(
            reloaded.node(&expanded).unwrap().display_title(),
            "/review foo"
        );
    }

    #[test]
    fn every_change_moves_the_revision_and_a_reloaded_tree_never_reuses_one() {
        use crate::conversation::metadata::Metadata;
        let (_tmp, root) = temp_root();
        let mut tree = ConversationTree::load(&root);
        let fresh = tree.revision();
        assert_eq!(fresh, tree.revision(), "reading it does not move it");

        // The tree is replaced as a whole (`*tree.write() = ConversationTree::load(..)`)
        // instead of changed in place, so a counter kept in the tree would hand the new
        // one the number of the old and what was cached of that one would pass for the
        // new one's.
        let reloaded = ConversationTree::load(&root);
        assert_ne!(fresh, reloaded.revision());
        assert_ne!(fresh, ConversationTree::default().revision());

        let id = tree
            .create_node(None, 0, Metadata::default(), "p", None)
            .unwrap();
        let created = tree.revision();
        tree.set_title(&id, "Titled".to_string());
        let titled = tree.revision();
        tree.set_summary(&id, "Summarized".to_string());
        let summarized = tree.revision();
        tree.toggle_favorite(&id).unwrap();
        let favorited = tree.revision();
        let agent = tree
            .create_agent(&id, 0, Metadata::default(), "task")
            .unwrap();
        let agent_created = tree.revision();
        tree.set_agent_title(&id, agent, "Ran".to_string());
        let agent_titled = tree.revision();
        tree.delete(&id).unwrap();
        let deleted = tree.revision();

        let moved = [
            created,
            titled,
            summarized,
            favorited,
            agent_created,
            agent_titled,
            deleted,
        ];
        for (i, before) in moved.iter().enumerate() {
            assert_ne!(*before, fresh);
            assert!(
                moved[i + 1..].iter().all(|after| after != before),
                "revision {before} was handed out twice"
            );
        }
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
    fn update_meta_writes_through() {
        use crate::conversation::metadata::Metadata;
        let (_tmp, root) = temp_root();
        let mut tree = ConversationTree::load(&root);
        let id = tree
            .create_node(None, 0, Metadata::default(), "prompt", None)
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
    fn marks_a_turn_cut_short_with_the_commits_it_made_and_the_head_it_stopped_at() {
        use crate::conversation::metadata::Metadata;
        let (_tmp, root) = temp_root();
        let mut tree = ConversationTree::load(&root);
        let id = tree
            .create_node(None, 0, Metadata::default(), "prompt", None)
            .unwrap();
        tree.mark_cut_short(
            &id,
            "half an answer",
            &["abc1234".to_string()],
            Some("abc1234"),
        )
        .unwrap();

        let dir = tree.node_dir(&id).unwrap();
        assert_eq!(files::read_reply(&dir), "half an answer");
        let reloaded = ConversationTree::load(&root);
        let meta = &reloaded.node(&id).unwrap().meta;
        assert_eq!(meta.status, Some(NodeStatus::Cancelled));
        assert_eq!(meta.commits, ["abc1234"]);
        assert_eq!(meta.head_after.as_deref(), Some("abc1234"));
    }

    #[test]
    fn created_secs_parses_the_id_timestamp() {
        let id = NodeId::new("1970-01-02T00-00-01Z-2");
        assert_eq!(id.created_secs(), Some(86_401));
        assert_eq!(NodeId::new("nonsense").created_secs(), None);
        assert_eq!(NodeId::new("2026x10-05T14-32-07Z").created_secs(), None);
    }
}
