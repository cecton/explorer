/// One sub-agent run of a node's turn, kept in `<node dir>/agents/<index>/` with the
/// files of a node: `metadata.md`, `prompt.md` (the task), `reply.md`, `activity.md`
/// and `transcript.jsonl`.
#[derive(Clone, Debug)]
pub struct AgentRecord {
    /// Starts at 1, in the order the runs started.
    pub index: u32,
    pub meta: crate::conversation::metadata::Metadata,
    /// What the summary model made of the run once it finished; `None` until then.
    pub title: Option<String>,
}

impl AgentRecord {
    /// What the summary model called the finished run; without one,
    /// `agent reviewer: description`, or `agent: description` for the default role —
    /// the run as the work log of its node names it.
    pub fn title(&self) -> String {
        if let Some(title) = &self.title {
            return title.clone();
        }
        let description = self
            .meta
            .description
            .as_deref()
            .unwrap_or("(no description)");
        match &self.meta.role {
            Some(role) => format!("agent {role}: {description}"),
            None => format!("agent: {description}"),
        }
    }
}

/// How the agent tool names a sub-agent run: `<node>/<index>`.
pub fn agent_id(node: &crate::conversation::NodeId, index: u32) -> String {
    format!("{}/{index}", node.as_str())
}

/// The sub-agent runs recorded under a node's directory, oldest first.
pub(crate) fn read(node_dir: &camino::Utf8Path) -> Vec<AgentRecord> {
    use crate::conversation::files;
    let Ok(entries) = std::fs::read_dir(node_dir.join(files::AGENTS_DIR)) else {
        return Vec::new();
    };
    let mut agents: Vec<AgentRecord> = entries
        .flatten()
        .filter_map(|entry| {
            let index: u32 = entry.file_name().to_str()?.parse().ok()?;
            let dir = camino::Utf8PathBuf::from_path_buf(entry.path()).ok()?;
            Some(AgentRecord {
                index,
                meta: crate::conversation::metadata::read(&dir),
                title: files::read_title(&dir),
            })
        })
        .collect();
    agents.sort_by_key(|agent| agent.index);
    agents
}

impl crate::conversation::ConversationTree {
    pub fn agent(&self, node: &crate::conversation::NodeId, index: u32) -> Option<&AgentRecord> {
        self.nodes
            .get(node)?
            .agents
            .iter()
            .find(|run| run.index == index)
    }

    /// Directory of sub-agent run `index` of `node`.
    pub fn agent_dir(
        &self,
        node: &crate::conversation::NodeId,
        index: u32,
    ) -> Option<camino::Utf8PathBuf> {
        use crate::conversation::files;
        self.agent(node, index)?;
        Some(
            self.node_dir(node)?
                .join(files::AGENTS_DIR)
                .join(index.to_string()),
        )
    }

    /// Records a new sub-agent run of `node`'s turn: its directory, `prompt.md` and
    /// `metadata.md` are on disk when this returns. Returns its index.
    pub fn create_agent(
        &mut self,
        node: &crate::conversation::NodeId,
        now_secs: i64,
        mut meta: crate::conversation::metadata::Metadata,
        prompt: &str,
    ) -> std::io::Result<u32> {
        use crate::conversation::files;
        let node_dir = self
            .node_dir(node)
            .ok_or_else(crate::conversation::unknown_node)?;
        let owner = self
            .nodes
            .get_mut(node)
            .ok_or_else(crate::conversation::unknown_node)?;
        let index = owner.agents.last().map_or(1, |agent| agent.index + 1);
        let dir = node_dir.join(files::AGENTS_DIR).join(index.to_string());
        meta.created = Some(crate::util::time::utc_stamp(now_secs));
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(files::PROMPT), prompt)?;
        crate::conversation::metadata::write(&dir, &meta)?;
        owner.agents.push(AgentRecord {
            index,
            meta,
            title: None,
        });
        Ok(index)
    }

    /// Changes the metadata of sub-agent run `index` of `node` and writes it through.
    pub fn update_agent_meta(
        &mut self,
        node: &crate::conversation::NodeId,
        index: u32,
        change: impl FnOnce(&mut crate::conversation::metadata::Metadata),
    ) -> std::io::Result<()> {
        let dir = self
            .agent_dir(node, index)
            .ok_or_else(crate::conversation::unknown_node)?;
        let agent = self
            .agent_mut(node, index)
            .ok_or_else(crate::conversation::unknown_node)?;
        change(&mut agent.meta);
        crate::conversation::metadata::write(&dir, &agent.meta)
    }

    /// Sets the title of sub-agent run `index` of `node` in memory; `title.md` is
    /// written by the task that asked the summary model, like a node's.
    pub fn set_agent_title(
        &mut self,
        node: &crate::conversation::NodeId,
        index: u32,
        title: String,
    ) {
        if let Some(agent) = self.agent_mut(node, index) {
            agent.title = Some(title);
        }
    }

    /// Marks the runs of `node` still running as cancelled: its turn is over, and a
    /// sub-agent never outlives the turn that called it. A run with nothing to show
    /// records as failed instead; one with a reply stays cancelled. A run cut short
    /// never got to record its duration, so its `created` stamp stands in for one.
    pub fn cancel_running_agents(&mut self, node: &crate::conversation::NodeId, now_secs: i64) {
        use crate::conversation::NodeStatus;
        use crate::conversation::files;
        let running: Vec<(u32, camino::Utf8PathBuf)> =
            self.nodes.get(node).map_or_else(Vec::new, |owner| {
                owner
                    .agents
                    .iter()
                    .filter(|agent| agent.meta.status == Some(NodeStatus::Running))
                    .filter_map(|agent| Some((agent.index, self.agent_dir(node, agent.index)?)))
                    .collect()
            });
        for (index, dir) in running {
            // A sub-agent commits nothing and holds no HEAD: only its reply says
            // whether it has something to show.
            let reply = files::read_reply(&dir);
            let cancelled = self.update_agent_meta(node, index, |meta| {
                meta.mark_cut_short(&reply, &[], None);
                if let Some(created) = meta
                    .created
                    .as_deref()
                    .and_then(crate::util::time::parse_utc_stamp)
                {
                    meta.secs = Some((now_secs - created).max(0) as u64);
                }
            });
            if let Err(e) = cancelled {
                tracing::warn!(
                    "failed to mark sub-agent {} cancelled: {e}",
                    agent_id(node, index)
                );
            }
        }
    }

    /// The run an agent id names, when the turn of `current` may continue it: a run of
    /// `current` itself or of one of its ancestors, the only ones its agent has seen.
    pub fn resumable_agent(
        &self,
        current: &crate::conversation::NodeId,
        id: &str,
    ) -> Result<(crate::conversation::NodeId, u32), String> {
        use crate::conversation::NodeId;
        let unknown = || format!("unknown agent_id: {id}");
        let (node, index) = id.trim().rsplit_once('/').ok_or_else(unknown)?;
        let node = NodeId::new(node);
        let index: u32 = index.parse().map_err(|_| unknown())?;
        if !self.lineage(current).contains(&node) {
            return Err(format!(
                "agent_id {id} belongs to another branch of the conversation"
            ));
        }
        self.agent(&node, index).ok_or_else(unknown)?;
        Ok((node, index))
    }

    /// The runs that run `index` of `node` continues, oldest first, ending with that run
    /// itself.
    pub fn agent_chain(
        &self,
        node: &crate::conversation::NodeId,
        index: u32,
    ) -> Vec<(crate::conversation::NodeId, u32)> {
        use crate::conversation::NodeId;
        let mut chain = Vec::new();
        let mut next = Some((node.clone(), index));
        while let Some((node, index)) = next.take() {
            if self.agent(&node, index).is_none() {
                break;
            }
            chain.push((node.clone(), index));
            next = self
                .agent(&node, index)
                .and_then(|agent| agent.meta.resumed_from.as_deref())
                .and_then(|from| {
                    let (node, index) = from.rsplit_once('/')?;
                    Some((NodeId::new(node), index.parse().ok()?))
                });
        }
        chain.reverse();
        chain
    }

    fn agent_mut(
        &mut self,
        node: &crate::conversation::NodeId,
        index: u32,
    ) -> Option<&mut AgentRecord> {
        self.nodes
            .get_mut(node)?
            .agents
            .iter_mut()
            .find(|run| run.index == index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::NodeStatus;
    use crate::conversation::files;
    use crate::conversation::metadata::Metadata;
    use crate::conversation::test_support::temp_root;
    use crate::conversation::test_support::tree_with_node;

    #[test]
    fn sub_agent_runs_are_recorded_under_their_node_and_never_load_as_nodes() {
        let (_tmp, root) = temp_root();
        let (mut tree, parent) = tree_with_node(&root);
        let running = Metadata {
            status: Some(NodeStatus::Running),
            role: Some("reviewer".into()),
            description: Some("Review".into()),
            ..Default::default()
        };
        let first = tree
            .create_agent(&parent, 86_402, running.clone(), "task one")
            .unwrap();
        let second = tree
            .create_agent(&parent, 86_403, running, "task two")
            .unwrap();
        assert_eq!((first, second), (1, 2));
        let dir = tree.agent_dir(&parent, 2).unwrap();
        assert_eq!(dir, tree.node_dir(&parent).unwrap().join("agents/2"));
        assert_eq!(files::read_prompt(&dir), "task two");
        tree.update_agent_meta(&parent, 1, |meta| meta.status = Some(NodeStatus::Done))
            .unwrap();

        let reloaded = crate::conversation::ConversationTree::load(&root);
        assert_eq!(reloaded.nodes().count(), 1);
        let node = reloaded.node(&parent).unwrap();
        assert!(node.children.is_empty());
        let statuses: Vec<_> = node
            .agents
            .iter()
            .map(|a| (a.index, a.meta.status))
            .collect();
        assert_eq!(
            statuses,
            [(1, Some(NodeStatus::Done)), (2, Some(NodeStatus::Failed))]
        );
        assert_eq!(node.agents[0].title(), "agent reviewer: Review");
    }

    #[test]
    fn a_run_is_titled_by_its_own_title_md_and_falls_back_to_the_description() {
        let (_tmp, root) = temp_root();
        let (mut tree, node) = tree_with_node(&root);
        let meta = Metadata {
            status: Some(NodeStatus::Done),
            role: Some("reviewer-fixer".into()),
            description: Some("Review round six".into()),
            ..Default::default()
        };
        tree.create_agent(&node, 86_402, meta, "task").unwrap();
        assert_eq!(
            tree.agent(&node, 1).unwrap().title(),
            "agent reviewer-fixer: Review round six"
        );

        let dir = tree.agent_dir(&node, 1).unwrap();
        files::write_title(&dir, "One i18n fix left\nuncommitted").unwrap();
        let reloaded = crate::conversation::ConversationTree::load(&root);
        assert_eq!(
            reloaded.agent(&node, 1).unwrap().title(),
            "One i18n fix left uncommitted"
        );

        // An empty file is no title: the caller's description stands in.
        files::write_title(&dir, "  \n").unwrap();
        let reloaded = crate::conversation::ConversationTree::load(&root);
        assert_eq!(
            reloaded.agent(&node, 1).unwrap().title(),
            "agent reviewer-fixer: Review round six"
        );
    }

    #[test]
    fn setting_a_run_title_keeps_it_in_memory() {
        let (_tmp, root) = temp_root();
        let (mut tree, node) = tree_with_node(&root);
        tree.create_agent(&node, 86_402, Metadata::default(), "task")
            .unwrap();
        tree.set_agent_title(&node, 1, "Nothing to fix".to_string());
        assert_eq!(tree.agent(&node, 1).unwrap().title(), "Nothing to fix");
        // An unknown run is a no-op, not a panic.
        tree.set_agent_title(&node, 9, "Nowhere".to_string());
        assert_eq!(tree.agent(&node, 1).unwrap().title(), "Nothing to fix");
    }

    #[test]
    fn cancelling_a_turn_cancels_its_running_sub_agents() {
        let (_tmp, root) = temp_root();
        let (mut tree, node) = tree_with_node(&root);
        let running = Metadata {
            status: Some(NodeStatus::Running),
            ..Default::default()
        };
        tree.create_agent(&node, 86_402, running, "t").unwrap();
        // A run with a reply stays cancelled: it has something to show.
        let dir = tree.agent_dir(&node, 1).unwrap();
        files::write_reply(&dir, "a partial answer").unwrap();
        let cancelled = {
            tree.cancel_running_agents(&node, 86_652);
            tree.agent(&node, 1).unwrap()
        };
        assert_eq!(cancelled.meta.status, Some(NodeStatus::Cancelled));
        assert_eq!(cancelled.meta.secs, Some(250));
        let reloaded = crate::conversation::ConversationTree::load(&root);
        let reloaded = reloaded.agent(&node, 1).unwrap();
        assert_eq!(reloaded.meta.status, Some(NodeStatus::Cancelled));
        assert_eq!(reloaded.meta.secs, Some(250));
    }

    #[test]
    fn a_cancelled_run_without_a_reply_records_as_failed() {
        let (_tmp, root) = temp_root();
        let (mut tree, node) = tree_with_node(&root);
        let running = Metadata {
            status: Some(NodeStatus::Running),
            ..Default::default()
        };
        tree.create_agent(&node, 86_402, running, "t").unwrap();
        tree.cancel_running_agents(&node, 86_652);
        let agent = tree.agent(&node, 1).unwrap();
        assert_eq!(agent.meta.status, Some(NodeStatus::Failed));
        assert_eq!(
            agent.meta.error.as_deref(),
            Some(crate::conversation::CANCELLED_ERROR)
        );
        // A run starting at 86_402 cut at 86_652 keeps its duration.
        assert_eq!(agent.meta.secs, Some(250));
    }

    #[test]
    fn only_runs_of_the_lineage_can_be_resumed_and_their_chain_is_followed() {
        let (_tmp, root) = temp_root();
        let (mut tree, root_node) = tree_with_node(&root);
        let child = tree
            .create_node(Some(&root_node), 86_402, Metadata::default(), "c", None)
            .unwrap();
        let sibling = tree
            .create_node(Some(&root_node), 86_403, Metadata::default(), "s", None)
            .unwrap();
        tree.create_agent(&root_node, 86_404, Metadata::default(), "first")
            .unwrap();
        tree.create_agent(&sibling, 86_405, Metadata::default(), "elsewhere")
            .unwrap();
        let resumed = Metadata {
            resumed_from: Some(agent_id(&root_node, 1)),
            ..Default::default()
        };
        tree.create_agent(&child, 86_406, resumed, "again").unwrap();

        let from_child = |id: &str| tree.resumable_agent(&child, id);
        assert_eq!(
            from_child(&agent_id(&root_node, 1)),
            Ok((root_node.clone(), 1))
        );
        assert!(
            from_child(&agent_id(&sibling, 1))
                .unwrap_err()
                .contains("another branch")
        );
        assert!(from_child(&agent_id(&root_node, 9)).is_err());
        assert!(from_child("nonsense").is_err());

        assert_eq!(
            tree.agent_chain(&child, 1),
            [(root_node.clone(), 1), (child.clone(), 1)]
        );
    }
}
