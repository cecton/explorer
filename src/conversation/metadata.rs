/// Content of a node's `metadata.md`: flat `key: value` lines.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Metadata {
    pub node_type: Option<crate::conversation::NodeType>,
    pub endpoint: Option<String>,
    pub kind: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub created: Option<String>,
    pub status: Option<crate::conversation::NodeStatus>,
    pub head_before: Option<String>,
    pub head_after: Option<String>,
    pub commits: Vec<String>,
    pub scratch_reset: bool,
    pub favorite: bool,
    pub command: Option<String>,
    /// Set for a custom command sent without the mode's system prompt.
    pub no_system_prompt: bool,
    pub session_id: Option<String>,
    pub tokens_in: Option<u64>,
    /// Input tokens of the node's first request that the provider served from its cache.
    pub cached_in: Option<u64>,
    pub error: Option<String>,
    /// Wall-clock seconds the turn took.
    pub secs: Option<u64>,
    /// Role a sub-agent ran as; `None` for the default one and for nodes.
    pub role: Option<String>,
    /// What the delegating agent called a sub-agent's task.
    pub description: Option<String>,
    /// The sub-agent run this one continues, as an agent id.
    pub resumed_from: Option<String>,
}

/// How a turn ended, applied to the node's metadata on the main thread.
#[derive(Clone, Debug)]
pub struct TurnOutcome {
    pub status: crate::conversation::NodeStatus,
    pub error: Option<String>,
    /// Seconds the turn took, wall clock.
    pub secs: u64,
    pub head_after: Option<String>,
    pub tokens_in: Option<u64>,
    pub cached_in: Option<u64>,
    /// Commits the turn produced, oldest first.
    pub commits: Vec<String>,
    /// True when the turn's uncommitted work was discarded at its end.
    pub reset: bool,
    /// Claude Code session of the turn; `None` for our own loop.
    pub session_id: Option<String>,
}

impl Metadata {
    /// Records how a turn ended: the fields `TurnOutcome` carries, in one place so a
    /// new one cannot be forgotten at a call site.
    pub fn record_outcome(&mut self, outcome: &TurnOutcome) {
        self.status = Some(outcome.status);
        self.error.clone_from(&outcome.error);
        self.secs = Some(outcome.secs);
        self.head_after.clone_from(&outcome.head_after);
        self.tokens_in = outcome.tokens_in;
        self.cached_in = outcome.cached_in;
        self.commits.clone_from(&outcome.commits);
        self.scratch_reset = outcome.reset;
        self.session_id.clone_from(&outcome.session_id);
    }

    /// Records a turn cut short, with the commits it made and the HEAD it ended on. The
    /// four go together: a node that kept its commits but not its HEAD makes the next
    /// turn replay from the wrong base.
    pub fn mark_cut_short(&mut self, reply: &str, commits: &[String], head_after: Option<&str>) {
        let (status, error) = cut_short_outcome(reply, commits);
        self.status = Some(status);
        self.error = error;
        self.commits = commits.to_vec();
        self.head_after = head_after.map(str::to_string);
    }

    pub fn parse(text: &str) -> Self {
        use crate::conversation::NodeStatus;
        use crate::conversation::NodeType;
        let mut meta = Self::default();
        for (key, value) in crate::util::text::key_values(text) {
            match key {
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
                "scratch-reset" => meta.scratch_reset = value == "true",
                "favorite" => meta.favorite = value == "true",
                "command" => meta.command = Some(value.to_string()),
                "system-prompt" => meta.no_system_prompt = value == "off",
                "session-id" => meta.session_id = Some(value.to_string()),
                "tokens-in" => meta.tokens_in = value.parse().ok(),
                "cached-in" => meta.cached_in = value.parse().ok(),
                "secs" => meta.secs = value.parse().ok(),
                "error" => meta.error = Some(value.to_string()),
                "role" => meta.role = Some(value.to_string()),
                "description" => meta.description = Some(value.to_string()),
                "resumed-from" => meta.resumed_from = Some(value.to_string()),
                _ => {}
            }
        }
        meta
    }

    pub fn to_text(&self) -> String {
        use crate::conversation::NodeStatus;
        use crate::conversation::NodeType;
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
        let cached_in = self.cached_in.map(|n| n.to_string());
        let secs = self.secs.map(|n| n.to_string());
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
        push("scratch-reset", self.scratch_reset.then_some("true"));
        push("favorite", self.favorite.then_some("true"));
        push("command", self.command.as_deref());
        push("system-prompt", self.no_system_prompt.then_some("off"));
        push("session-id", self.session_id.as_deref());
        push("tokens-in", tokens_in.as_deref());
        push("cached-in", cached_in.as_deref());
        push("secs", secs.as_deref());
        push("error", self.error.as_deref());
        push("role", self.role.as_deref());
        push("description", self.description.as_deref());
        push("resumed-from", self.resumed_from.as_deref());
        out
    }

    /// The model the turn ran on, as `endpoint/model (effort)` with `default` effort
    /// shown bare.
    pub fn model_label(&self) -> Option<String> {
        Some(self.model_spec()?.short_label())
    }

    /// The model the turn ran on, when the metadata names one completely.
    pub fn model_spec(&self) -> Option<crate::model_spec::ModelSpec> {
        use crate::model_spec::ModelSpec;
        ModelSpec::from_parts(
            self.endpoint.as_deref()?,
            self.model.as_deref()?,
            self.effort.as_deref()?,
        )
    }
}

/// What a turn cut short records. One with a reply or commits is part of the
/// discussion and stays cancelled; one with neither has nothing to show and fails.
pub fn cut_short_outcome(
    reply: &str,
    commits: &[String],
) -> (crate::conversation::NodeStatus, Option<String>) {
    use crate::conversation::CANCELLED_ERROR;
    use crate::conversation::NodeStatus;
    if crate::conversation::files::has_content(reply) || !commits.is_empty() {
        (NodeStatus::Cancelled, None)
    } else {
        (NodeStatus::Failed, Some(CANCELLED_ERROR.to_string()))
    }
}

/// Reads the metadata of a node or a sub-agent run; one left running by a previous
/// instance of Explorer is marked interrupted on disk.
pub(crate) fn read(dir: &camino::Utf8Path) -> Metadata {
    use crate::conversation::NodeStatus;
    use crate::conversation::files;
    let mut meta = std::fs::read_to_string(dir.join(files::METADATA))
        .map(|text| Metadata::parse(&text))
        .unwrap_or_default();
    if meta.status == Some(NodeStatus::Running) {
        meta.status = Some(NodeStatus::Failed);
        meta.error = Some("interrupted".to_string());
        if let Err(e) = write(dir, &meta) {
            tracing::warn!("failed to rewrite interrupted run {dir}: {e}");
        }
    }
    meta
}

/// Writes the metadata of a node or a sub-agent run through.
pub(crate) fn write(dir: &camino::Utf8Path, meta: &Metadata) -> std::io::Result<()> {
    use crate::conversation::files;
    crate::util::fs::write_atomic(&dir.join(files::METADATA), &meta.to_text())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::test_support::temp_root;
    use crate::conversation::test_support::write_node;

    #[test]
    fn metadata_round_trips_every_key() {
        use crate::conversation::NodeStatus;
        use crate::conversation::NodeType;
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
            scratch_reset: true,
            favorite: true,
            command: Some("review".into()),
            no_system_prompt: true,
            session_id: Some("1234-uuid".into()),
            tokens_in: Some(84_210),
            cached_in: Some(81_920),
            secs: Some(95),
            error: Some("rate limited: try later".into()),
            role: Some("reviewer".into()),
            description: Some("Review the change".into()),
            resumed_from: Some("2026-10-05T14-32-07Z/1".into()),
        };
        assert_eq!(Metadata::parse(&meta.to_text()), meta);
    }

    #[test]
    fn metadata_omits_unset_keys_and_ignores_unknown_ones() {
        use crate::conversation::NodeStatus;
        assert_eq!(Metadata::default().to_text(), "");
        let meta = Metadata::parse("status: done\nwat: 1\nnot a pair\n");
        assert_eq!(meta.status, Some(NodeStatus::Done));
        assert!(!meta.favorite);
    }

    #[test]
    fn a_cut_short_turn_with_a_reply_or_commits_stays_cancelled() {
        use crate::conversation::CANCELLED_ERROR;
        use crate::conversation::NodeStatus;
        let (status, error) = cut_short_outcome("a partial answer", &[]);
        assert_eq!(status, NodeStatus::Cancelled);
        assert_eq!(error, None);
        let (status, error) = cut_short_outcome("", &["abc1234".to_string()]);
        assert_eq!(status, NodeStatus::Cancelled);
        assert_eq!(error, None);
        let (status, error) = cut_short_outcome("  \n  ", &[]);
        assert_eq!(status, NodeStatus::Failed);
        assert_eq!(error.as_deref(), Some(CANCELLED_ERROR));
    }

    #[test]
    fn marking_a_turn_cut_short_writes_its_commits_and_the_head_it_ended_on() {
        use crate::conversation::NodeStatus;
        let mut meta = Metadata {
            head_after: Some("stale".into()),
            ..Default::default()
        };
        meta.mark_cut_short(
            "a partial answer",
            &["abc1234".to_string()],
            Some("abc1234"),
        );
        assert_eq!(meta.status, Some(NodeStatus::Cancelled));
        assert_eq!(meta.error, None);
        assert_eq!(meta.commits, ["abc1234"]);
        assert_eq!(meta.head_after.as_deref(), Some("abc1234"));

        let mut meta = Metadata::default();
        meta.mark_cut_short("   ", &[], None);
        assert_eq!(meta.status, Some(NodeStatus::Failed));
        assert!(meta.commits.is_empty());
        assert_eq!(meta.head_after, None);
    }

    #[test]
    fn a_missing_metadata_file_reads_as_the_default_and_is_not_written() {
        let (_tmp, root) = temp_root();
        let dir = root.join("a1");
        write_node(&dir, &[]);
        assert_eq!(read(&dir), Metadata::default());
        assert!(!dir.join("metadata.md").exists());
    }
}
