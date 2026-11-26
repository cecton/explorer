use std::time::Duration;

const FEEDBACK_PROMPT: &str = include_str!("../../prompts/feedback.md");
const COMMIT_PROMPT: &str = include_str!("../../prompts/commit.md");
const CONVERSATION_TOOL_PROMPT: &str = include_str!("../../prompts/conversation_tool.md");
const KNOWLEDGE_TOOL_PROMPT: &str = include_str!("../../prompts/knowledge_tool.md");
pub const TITLE_SUMMARY_PROMPT: &str = include_str!("../../prompts/title_summary.md");
const COMPACT_PROMPT: &str = include_str!("../../prompts/compact.md");
const UPDATE_PROMPT: &str = include_str!("../../prompts/update.md");
/// System prompt of a sub-agent called without a role.
pub const SUBAGENT_PROMPT: &str = include_str!("../../prompts/subagent.md");

/// Stands in the commit prompt for the most review rounds of a commit.
const REVIEW_ROUNDS_PLACEHOLDER: &str = "{review_rounds}";

/// Commits listed in a repository-moved note before it says there are more.
pub const NOTE_MAX_COMMITS: usize = 20;

/// Times a commit turn is asked to commit what it left behind before Explorer discards it.
const MAX_COMMIT_NUDGES: usize = 2;

/// The system prompt of a turn: mode instructions and the project's own instructions.
/// Without a `mode`, the mode instructions are left out. `review_rounds` is the most
/// review rounds of each commit, from the configuration. It holds nothing that changes
/// between turns: see [`environment_note`].
pub fn system_prompt(
    mode: Option<crate::agent::Mode>,
    review_rounds: u32,
    root: &camino::Utf8Path,
) -> String {
    let lead = mode.map(|mode| mode_prompt(mode, review_rounds));
    with_project(lead.as_deref(), root)
}

/// The system prompt of a sub-agent: its role's prompt instead of a mode's, and the
/// project's own instructions.
pub fn subagent_system_prompt(role: &str, root: &camino::Utf8Path) -> String {
    with_project(Some(role.trim_end()), root)
}

fn with_project(lead: Option<&str>, root: &camino::Utf8Path) -> String {
    let mut out = String::new();
    if let Some(lead) = lead {
        out.push_str(lead);
        out.push_str("\n\n");
    }
    out.push_str(CONVERSATION_TOOL_PROMPT.trim_end());
    out.push_str("\n\n");
    out.push_str(KNOWLEDGE_TOOL_PROMPT.trim_end());
    if let Ok(agents) = std::fs::read_to_string(root.join("AGENTS.md")) {
        out.push_str("\n\n## Project instructions (AGENTS.md)\n\n");
        out.push_str(agents.trim());
    }
    out
}

/// What is appended to Claude Code's own system prompt, which reads the project's
/// instructions itself.
pub fn claude_system_prompt(mode: Option<crate::agent::Mode>, review_rounds: u32) -> String {
    let mode = mode
        .map(|mode| mode_prompt(mode, review_rounds))
        .unwrap_or_default();
    format!("{mode}\n\n{}", KNOWLEDGE_TOOL_PROMPT.trim_end())
        .trim()
        .to_string()
}

fn mode_prompt(mode: crate::agent::Mode, review_rounds: u32) -> String {
    use crate::agent::Mode;
    match mode {
        Mode::Feedback => FEEDBACK_PROMPT.trim_end().to_string(),
        Mode::Commit => COMMIT_PROMPT
            .trim_end()
            .replace(REVIEW_ROUNDS_PLACEHOLDER, &review_rounds.to_string()),
    }
}

/// The state of the repository when the prompt was sent. It belongs to the message,
/// not to the system prompt: HEAD moves on every commit, and anything that changes
/// ahead of the replayed ancestors throws away the endpoint's prefix cache.
pub fn environment_note(root: &camino::Utf8Path, head: Option<&str>, date: &str) -> String {
    let head = match head {
        Some(head) => head.to_string(),
        None => "none yet (no commit)".to_string(),
    };
    format!(
        "[Note from Explorer: the state of the repository when this message was sent.]\n\n\
         - Repository root: {root}\n- Date: {date}\n- HEAD: {head}"
    )
}

/// The time budget of a run, closing its system prompt: the budget, when the run
/// started and when the budget expires. A run that gets no warnings from the agent
/// loop checks the time against it with `date`.
pub fn budget_note(budget: Duration) -> String {
    let now = crate::util::time::now_secs();
    budget_note_at(budget, now, crate::util::time::local_offset_secs(now))
}

fn budget_note_at(budget: Duration, now: i64, offset: i32) -> String {
    let minutes = budget.as_secs() / 60;
    format!(
        "[Note from Explorer: this run has a time budget of {minutes} minutes. It is \
         now about {}; the budget expires at about {}. The agent loop warns you as \
         the end nears; when you are warned or the time is past, finish what you are \
         doing and write your final answer. A run that gets no such warning checks \
         the time with `date` itself.]",
        crate::util::time::local_hhmm(now, offset),
        crate::util::time::local_hhmm(now + budget.as_secs() as i64, offset),
    )
}

/// The messages `transcript.jsonl` of a node holds, or `None` without a usable one.
fn read_transcript(dir: &camino::Utf8Path) -> Option<Vec<crate::agent::Message>> {
    use crate::agent::Message;
    let text = std::fs::read_to_string(dir.join(crate::conversation::files::TRANSCRIPT)).ok()?;
    let messages: Vec<Message> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .ok()?;
    (!messages.is_empty()).then_some(messages)
}

/// Repairs a transcript that ends inside a round: an assistant message asking for
/// tool calls, some or all of whose results never arrived, because the turn died
/// mid-stream or mid-tool. Each call without a result is answered with a note, so
/// replaying the transcript is a valid sequence that shows where the work stopped.
fn repair_tail(messages: &mut Vec<crate::agent::Message>) {
    use crate::agent::Message;
    let Some(start) = messages
        .iter()
        .rposition(|m| matches!(m, Message::Assistant { .. }))
    else {
        return;
    };
    let Message::Assistant { tool_calls, .. } = &messages[start] else {
        return;
    };
    let missing: Vec<Message> = tool_calls
        .iter()
        .filter(|call| {
            !messages[start + 1..]
                .iter()
                .any(|m| matches!(m, Message::Tool { call_id, .. } if *call_id == call.id))
        })
        .map(|call| Message::Tool {
            call_id: call.id.clone(),
            name: call.name.clone(),
            content: "[interrupted before this call ran]".to_string(),
        })
        .collect();
    messages.extend(missing);
}

/// The messages the ancestors `dirs` contribute, oldest first. A commit turn never reads the
/// feedback turns before it as instructions to itself, so their feedback note is removed.
pub fn replay(
    dirs: &[camino::Utf8PathBuf],
    mode: crate::agent::Mode,
) -> Vec<crate::agent::Message> {
    use crate::agent::Mode;
    let mut messages: Vec<_> = dirs.iter().flat_map(|dir| ancestor_messages(dir)).collect();
    if mode == Mode::Commit {
        strip_feedback_note(&mut messages);
    }
    messages
}

/// What an ancestor contributes to the context: its exact transcript, or, when it has
/// none, its prompt and reply as plain text. A transcript cut inside a round is
/// repaired, so replaying it is a valid sequence.
fn ancestor_messages(dir: &camino::Utf8Path) -> Vec<crate::agent::Message> {
    use crate::agent::Message;
    if let Some(mut messages) = read_transcript(dir) {
        repair_tail(&mut messages);
        return messages;
    }
    let (prompt, reply) = crate::conversation::files::read_prompt_and_reply(dir);
    let mut messages = Vec::new();
    if !prompt.trim().is_empty() {
        messages.push(Message::User {
            content: prompt,
            injected: false,
        });
        if !reply.trim().is_empty() {
            messages.push(Message::Assistant {
                content: reply,
                tool_calls: Vec::new(),
            });
        }
    }
    messages
}

/// The ancestors as plain text, opening the first message of a session that cannot
/// replay them: a lossy handover from another backend, or from a session that is gone.
/// `None` when no ancestor has anything to say.
pub fn handover_preamble(ancestor_dirs: &[camino::Utf8PathBuf]) -> Option<String> {
    let mut out = String::new();
    for dir in ancestor_dirs {
        let (prompt, reply) = crate::conversation::files::read_prompt_and_reply(dir);
        if prompt.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("## User\n\n{}\n\n", prompt.trim()));
        if !reply.trim().is_empty() {
            out.push_str(&format!("## Assistant\n\n{}\n\n", reply.trim()));
        }
    }
    (!out.is_empty()).then(|| {
        format!(
            "[Note from Explorer: this conversation started before this session. Its earlier \
             messages follow as plain text, without the tool calls that were made.]\n\n\
             {out}[End of the earlier messages. The new message follows.]"
        )
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Feedback {
    InPlace,
    Sandboxed,
}

impl Feedback {
    pub fn of(mode: crate::agent::Mode, sandboxed: bool) -> Option<Self> {
        use crate::agent::Mode;
        (mode == Mode::Feedback).then_some(if sandboxed {
            Self::Sandboxed
        } else {
            Self::InPlace
        })
    }

    fn note(self) -> &'static str {
        match self {
            Self::InPlace => FEEDBACK_NOTE,
            Self::Sandboxed => SANDBOX_NOTE,
        }
    }
}

/// Note opening every feedback prompt whose turn works in the repository itself.
pub const FEEDBACK_NOTE: &str = "[Note from Explorer: this is a feedback turn — advisory \
only. Building to verify an idea is fine; committing is never allowed, however imperative \
the request reads. What a feedback turn changes does not stay: the tree is hard-reset \
when it ends, and a commit made here is not recorded to this conversation. You cannot \
keep a change in the git history — you can only present a change to the user.]";

/// Note opening every feedback prompt whose turn works in a copy of the repository.
pub const SANDBOX_NOTE: &str = "[Note from Explorer: this is a feedback turn — advisory \
only. Building to verify an idea is fine; committing is never allowed, however imperative \
the request reads. You work in a sandbox copy of the repository, shown at the repository's \
own path and discarded when this turn ends: treat it as the real project, and never try to \
reach the original directory behind it. A commit made here is not recorded to this \
conversation. You cannot keep a change in the git history — you can only present a change \
to the user.]";

/// Removes both feedback notes from replayed messages, so a commit turn never reads them.
fn strip_feedback_note(messages: &mut [crate::agent::Message]) {
    use crate::agent::Message;
    for message in messages {
        let Message::User { content, .. } = message else {
            continue;
        };
        for note in [FEEDBACK_NOTE, SANDBOX_NOTE] {
            if content.contains(note) {
                *content = content
                    .replace(&format!("{note}\n\n"), "")
                    .replace(note, "");
            }
        }
    }
}

/// Note telling the agent which commits happened since the node it continues from.
/// `commits` are `<short hash> <title>` lines, newest first, possibly one more than
/// [`NOTE_MAX_COMMITS`] to signal that older ones exist.
pub fn repository_moved_note(commits: &[String]) -> String {
    let mut note = String::from(
        "[Note from Explorer: the repository has changed since the previous message of \
         this conversation. Commits made since then, newest first:\n",
    );
    for commit in commits.iter().take(NOTE_MAX_COMMITS) {
        note.push_str(&format!("- {commit}\n"));
    }
    if commits.len() > NOTE_MAX_COMMITS {
        note.push_str("- (older commits not listed)\n");
    }
    if commits.is_empty() {
        note.push_str("- (history was rewritten; no new commit to list)\n");
    }
    note.push_str("Trust the files on disk over what you remember.]");
    note
}

/// Note opening the first message after a compact node: its summary stands in for
/// everything said before.
pub fn compaction_note(summary: &str) -> String {
    format!(
        "[Note from Explorer: the earlier part of this conversation was compacted. Its \
         summary follows and replaces the earlier messages.]\n\n{}\n\n[End of the summary.]",
        summary.trim()
    )
}

/// What a `/compact` turn asks the model, with the instructions the user typed.
pub fn compact_request(instructions: &str) -> String {
    let base = COMPACT_PROMPT.trim_end();
    match instructions.trim() {
        "" => base.to_string(),
        extra => format!("{base}\n\nAdditional instructions from the user: {extra}"),
    }
}

/// What a `/continue` turn asks the model: the state of the interrupted turn of the
/// node it replays, then how to go on. `interrupted` is how the turn ended, `None` for
/// one that ran to completion; `discarded` whether the uncommitted work of that turn
/// was discarded when it ended.
pub fn continue_request(interrupted: Option<&str>, discarded: bool, instructions: &str) -> String {
    let mut note = match interrupted {
        Some(interrupted) => format!(
            "[Note from Explorer: your previous turn on this conversation was interrupted: \
             {interrupted}. Everything above is what you did and said before the \
             interruption. Continue where you left off; if the failure came from something \
             in your approach, adapt."
        ),
        None => "[Note from Explorer: your previous turn on this conversation ran to its \
                 end. Everything above is what you did and said. Carry on from there."
            .to_string(),
    };
    if discarded {
        note.push_str(
            " The changes that turn left uncommitted were discarded when it ended: the \
             working tree holds nothing of that turn but the commits it made. Redo them \
             from what you know of that turn: the conversation above holds every edit \
             and command you made.",
        );
    } else {
        note.push_str(" That turn left no uncommitted work behind, so nothing was lost.");
    }
    note.push(']');
    match instructions.trim() {
        "" => note,
        extra => format!("{note}\n\nAdditional instructions from the user: {extra}"),
    }
}

/// The text of a message as sent: the prompt, preceded by the notes Explorer adds.
pub fn with_notes(notes: &[Option<String>], prompt: &str) -> String {
    let mut parts: Vec<&str> = notes.iter().flatten().map(String::as_str).collect();
    parts.push(prompt);
    parts.join("\n\n")
}

/// What an `/update` turn asks the model, with the instructions the user typed.
pub fn update_request(instructions: &str) -> String {
    let base = UPDATE_PROMPT.trim_end();
    match instructions.trim() {
        "" => base.to_string(),
        extra => format!("{base}\n\nAdditional instructions from the user: {extra}"),
    }
}

/// The notes that open a message rather than its system prompt, because HEAD moves on
/// every commit and anything that changes ahead of the replayed ancestors throws the
/// endpoint's prefix cache away. Only the fields a caller has apply. Reads files and runs
/// git.
pub struct Notes<'a> {
    pub root: &'a camino::Utf8Path,
    /// HEAD when this message is sent.
    pub head: Option<&'a str>,
    /// `YYYY-MM-DD`, the day the message is sent.
    pub date: &'a str,
    /// Directory of a compacted parent, whose reply stands in for what came before.
    pub compacted_parent: Option<&'a camino::Utf8Path>,
    /// HEAD when the turn or run this message continues ended.
    pub parent_head: Option<&'a str>,
    /// How the turn runs when it is a feedback one, which may not commit.
    pub feedback: Option<Feedback>,
}

impl Notes<'_> {
    /// The message: the notes, then the prompt.
    pub fn message(&self, prompt: &str) -> String {
        let environment = Some(environment_note(self.root, self.head, self.date));
        let summary = self.compacted_parent.map(|dir| {
            let (_, summary) = crate::conversation::files::read_prompt_and_reply(dir);
            compaction_note(&summary)
        });
        let moved = self.moved_note();
        let feedback = self.feedback.map(|feedback| feedback.note().to_string());
        with_notes(&[environment, summary, moved, feedback], prompt)
    }

    /// What changed in the repository since the message this one continues, when HEAD
    /// moved.
    fn moved_note(&self) -> Option<String> {
        let (parent_head, head) = (self.parent_head?, self.head?);
        if parent_head == head {
            return None;
        }
        let commits = crate::git::log_oneline(self.root, parent_head, head, NOTE_MAX_COMMITS + 1);
        Some(repository_moved_note(&commits))
    }
}

/// Hidden message asking a commit turn to commit what it left uncommitted, or `None` once
/// it has been asked [`MAX_COMMIT_NUDGES`] times or the tree is clean. The one place both
/// backends close a commit turn through.
pub async fn next_commit_nudge(
    root: &camino::Utf8Path,
    sent: usize,
) -> Result<Option<crate::agent::Message>, String> {
    if sent >= MAX_COMMIT_NUDGES {
        return Ok(None);
    }
    let root = root.to_path_buf();
    let status = tokio::task::spawn_blocking(move || crate::git::status_short(&root))
        .await
        .map_err(|e| e.to_string())??;
    Ok((!status.trim().is_empty()).then(|| commit_nudge(&status)))
}

/// Hidden message sent when a commit turn stops while the working tree is still dirty.
/// `status` is the output of `git status --short`.
pub fn commit_nudge(status: &str) -> crate::agent::Message {
    use crate::agent::Message;
    Message::User {
        content: format!(
            "The working tree still has uncommitted changes:\n\n{}\n\nCommit them now.",
            status.trim_end()
        ),
        injected: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::test_support::{commit, git, temp_repo, temp_root};

    #[test]
    fn ancestor_with_transcript_is_replayed_exactly() {
        use crate::agent::Message;
        let (_tmp, dir) = temp_root();
        std::fs::write(dir.join(crate::conversation::files::PROMPT), "ignored").unwrap();
        std::fs::write(
            dir.join(crate::conversation::files::TRANSCRIPT),
            "{\"role\":\"user\",\"content\":\"[note]\\n\\nhi\"}\n\
             {\"role\":\"assistant\",\"content\":\"hello\"}\n",
        )
        .unwrap();
        assert_eq!(
            ancestor_messages(&dir),
            [
                Message::User {
                    content: "[note]\n\nhi".into(),
                    injected: false,
                },
                Message::Assistant {
                    content: "hello".into(),
                    tool_calls: Vec::new()
                },
            ]
        );
    }

    #[test]
    fn ancestor_without_transcript_falls_back_to_prompt_and_reply() {
        use crate::agent::Message;
        let (_tmp, dir) = temp_root();
        std::fs::write(dir.join(crate::conversation::files::PROMPT), "question").unwrap();
        std::fs::write(dir.join(crate::conversation::files::REPLY), "answer").unwrap();
        assert_eq!(
            ancestor_messages(&dir),
            [
                Message::User {
                    content: "question".into(),
                    injected: false,
                },
                Message::Assistant {
                    content: "answer".into(),
                    tool_calls: Vec::new()
                },
            ]
        );
        std::fs::write(
            dir.join(crate::conversation::files::TRANSCRIPT),
            "not json\n",
        )
        .unwrap();
        assert_eq!(ancestor_messages(&dir).len(), 2);
    }

    #[test]
    fn empty_ancestor_contributes_nothing() {
        let (_tmp, dir) = temp_root();
        assert!(ancestor_messages(&dir).is_empty());
    }

    #[test]
    fn note_lists_commits_and_flags_a_truncated_list() {
        let few = repository_moved_note(&["abc1234 fix: one".to_string()]);
        assert!(few.contains("- abc1234 fix: one\n"));
        assert!(!few.contains("older commits"));

        let many: Vec<String> = (0..=NOTE_MAX_COMMITS).map(|i| format!("h{i} t")).collect();
        let note = repository_moved_note(&many);
        assert!(note.contains("- h19 t\n"));
        assert!(!note.contains("- h20 t\n"));
        assert!(note.contains("older commits not listed"));
    }

    #[test]
    fn notes_come_before_the_prompt() {
        let notes = [
            Some("[summary]".to_string()),
            None,
            Some("[moved]".to_string()),
        ];
        assert_eq!(with_notes(&notes, "do it"), "[summary]\n\n[moved]\n\ndo it");
        assert_eq!(with_notes(&[None], "do it"), "do it");
    }

    #[test]
    fn a_message_opens_with_the_notes_its_fields_ask_for() {
        let root = camino::Utf8Path::new("/repo");
        let bare = |feedback| Notes {
            root,
            head: Some("aaa"),
            date: "2026-11-13",
            compacted_parent: None,
            parent_head: None,
            feedback,
        };
        // The state of the repository opens every message, and a feedback turn adds its
        // own note; nothing else applies when no HEAD moved and no parent was compacted.
        let feedback = bare(Some(Feedback::InPlace)).message("do it");
        assert!(feedback.starts_with("[Note from Explorer: the state of the repository"));
        assert!(feedback.contains("- Repository root: /repo"));
        assert!(feedback.contains(FEEDBACK_NOTE));
        assert!(feedback.ends_with("do it"));
        assert!(!bare(None).message("do it").contains(FEEDBACK_NOTE));
        let sandboxed = bare(Some(Feedback::Sandboxed)).message("do it");
        assert!(sandboxed.contains(SANDBOX_NOTE));
        assert!(!sandboxed.contains(FEEDBACK_NOTE));

        // The moved note only appears when HEAD is not where the continued message left
        // it: a same-HEAD parent adds nothing, since there is nothing to list.
        let moved = Notes {
            parent_head: Some("bbb"),
            ..bare(None)
        };
        assert!(
            moved
                .message("do it")
                .contains("the repository has changed")
        );
        let same = Notes {
            parent_head: Some("aaa"),
            ..bare(None)
        };
        assert!(!same.message("do it").contains("the repository has changed"));
    }

    #[test]
    fn compact_request_appends_typed_instructions() {
        assert!(compact_request("  ").starts_with("Summarize this conversation"));
        assert!(!compact_request("").contains("Additional instructions"));
        let request = compact_request(" keep the API decisions ");
        assert!(request.ends_with("Additional instructions from the user: keep the API decisions"));
    }

    #[test]
    fn compaction_note_wraps_the_summary() {
        let note = compaction_note("\nWe chose KDL.\n");
        assert!(note.starts_with("[Note from Explorer:"));
        assert!(note.ends_with("We chose KDL.\n\n[End of the summary.]"));
    }

    #[test]
    fn continue_request_reports_the_interruption_and_the_discarded_work() {
        let bare = continue_request(Some("rate limited"), true, "  ");
        assert!(bare.starts_with("[Note from Explorer:"));
        assert!(bare.contains("was interrupted: rate limited."));
        assert!(bare.contains("discarded"));
        assert!(bare.contains("the conversation above holds every edit"));
        assert!(!bare.contains("stash"));
        assert!(bare.ends_with("made.]"));

        let kept = continue_request(Some("boom"), false, "");
        assert!(kept.contains("nothing was lost"));
        assert!(!kept.contains("discarded"));

        let extra = continue_request(Some("boom"), true, " skip the refactor");
        assert!(extra.ends_with("Additional instructions from the user: skip the refactor"));

        // A turn that ran to its end was not interrupted, and the note says so.
        let done = continue_request(None, false, "");
        assert!(done.contains("ran to its end"));
        assert!(!done.contains("interrupted"));
    }

    #[test]
    fn system_prompt_holds_no_state_of_the_repository() {
        use crate::agent::Mode;
        let (_tmp, root) = temp_root();
        std::fs::write(root.join("AGENTS.md"), "Use tabs.\n").unwrap();
        let prompt = system_prompt(Some(Mode::Feedback), 3, &root);
        assert!(prompt.starts_with("You are the coding agent inside Explorer"));
        assert!(prompt.contains("**feedback mode**"));
        assert!(prompt.contains("## The conversation tree"));
        let commit = system_prompt(Some(Mode::Commit), 3, &root);
        assert!(commit.contains("**commit mode**"));
        assert!(prompt.contains("## Project instructions (AGENTS.md)\n\nUse tabs."));
        for part in ["HEAD", "Repository root", "Date", "Environment"] {
            assert!(
                !prompt.contains(part),
                "{part} is not part of the system prompt"
            );
        }
        assert!(
            commit.ends_with("Use tabs."),
            "the system prompt ends with the project instructions: {commit}"
        );
    }

    #[test]
    fn commit_prompt_states_the_configured_review_rounds() {
        use crate::agent::Mode;
        let (_tmp, root) = temp_root();
        let commit = system_prompt(Some(Mode::Commit), 7, &root);
        assert!(commit.contains("At most 7 review rounds"));
        assert!(!commit.contains(REVIEW_ROUNDS_PLACEHOLDER));
        let claude = claude_system_prompt(Some(Mode::Commit), 2);
        assert!(claude.contains("At most 2 review rounds"));
        assert!(!claude.contains(REVIEW_ROUNDS_PLACEHOLDER));
    }

    #[test]
    fn environment_note_carries_the_head_the_date_and_the_root() {
        let (_tmp, root) = temp_root();
        let note = environment_note(&root, Some("abc123"), "2026-10-05");
        assert!(note.starts_with("[Note from Explorer:"));
        assert!(note.contains(&format!("- Repository root: {root}\n")));
        assert!(note.contains("- Date: 2026-10-05\n"));
        assert!(note.ends_with("- HEAD: abc123"));
        let empty = environment_note(&root, None, "2026-10-05");
        assert!(empty.ends_with("- HEAD: none yet (no commit)"));
    }

    #[test]
    fn system_prompt_without_a_mode_keeps_everything_but_the_mode_prompt() {
        let (_tmp, root) = temp_root();
        std::fs::write(root.join("AGENTS.md"), "Use tabs.\n").unwrap();
        let prompt = system_prompt(None, 3, &root);
        assert!(prompt.starts_with("## The conversation tree"));
        assert!(!prompt.contains("You are the coding agent inside Explorer"));
        assert!(prompt.contains("## Project instructions (AGENTS.md)\n\nUse tabs."));

        // Claude reads AGENTS.md itself, so it gets the knowledge prompt alone.
        assert_eq!(
            claude_system_prompt(None, 3),
            KNOWLEDGE_TOOL_PROMPT.trim_end()
        );
    }

    #[test]
    fn both_system_prompts_describe_the_knowledge_base() {
        use crate::agent::Mode;
        let (_tmp, root) = temp_root();
        let own = system_prompt(Some(Mode::Commit), 3, &root);
        assert!(own.contains("## The knowledge base"));
        assert!(
            own.find("## The conversation tree").unwrap()
                < own.find("## The knowledge base").unwrap()
        );
        let claude = claude_system_prompt(Some(Mode::Commit), 3);
        assert!(claude.contains("## The knowledge base"));
        assert!(claude.contains("**commit mode**"), "{claude}");
        assert!(!claude.contains("## The conversation tree"));
    }

    #[test]
    fn replay_strips_the_feedback_note_only_in_commit_mode() {
        use crate::agent::{Message, Mode};
        let (_tmp, root) = temp_root();
        let first = root.join("a");
        let second = root.join("b");
        for dir in [&first, &second] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let feedback = with_notes(&[Some(FEEDBACK_NOTE.to_string())], "first");
        std::fs::write(first.join(crate::conversation::files::PROMPT), &feedback).unwrap();
        std::fs::write(second.join(crate::conversation::files::PROMPT), "second").unwrap();
        let dirs = [first, second];

        assert_eq!(
            replay(&dirs, Mode::Feedback),
            [
                Message::User {
                    content: feedback,
                    injected: false,
                },
                Message::User {
                    content: "second".into(),
                    injected: false,
                },
            ]
        );
        assert_eq!(
            replay(&dirs, Mode::Commit),
            [
                Message::User {
                    content: "first".into(),
                    injected: false,
                },
                Message::User {
                    content: "second".into(),
                    injected: false,
                },
            ]
        );
    }

    #[test]
    fn strip_feedback_note_leaves_the_other_notes_and_the_prompt() {
        use crate::agent::Message;
        let environment = Some("[Note from Explorer: env]".to_string());
        let feedback = Some(FEEDBACK_NOTE.to_string());
        let mut messages = vec![
            Message::User {
                content: with_notes(&[environment.clone(), feedback], "Fix it"),
                injected: false,
            },
            Message::Assistant {
                content: FEEDBACK_NOTE.to_string(),
                tool_calls: Vec::new(),
            },
        ];
        strip_feedback_note(&mut messages);
        assert_eq!(
            messages[0],
            Message::User {
                content: with_notes(&[environment], "Fix it"),
                injected: false,
            }
        );
        assert!(
            matches!(&messages[1], Message::Assistant { content, .. } if content == FEEDBACK_NOTE)
        );
    }

    #[test]
    fn strip_feedback_note_removes_the_sandbox_note_too() {
        use crate::agent::Message;
        let environment = Some("[Note from Explorer: env]".to_string());
        let mut messages = vec![Message::User {
            content: with_notes(
                &[environment.clone(), Some(SANDBOX_NOTE.to_string())],
                "Fix it",
            ),
            injected: false,
        }];
        strip_feedback_note(&mut messages);
        assert_eq!(
            messages[0],
            Message::User {
                content: with_notes(&[environment], "Fix it"),
                injected: false,
            }
        );
    }

    #[test]
    fn sandbox_note_says_the_copy_is_discarded_and_the_original_off_limits() {
        assert!(SANDBOX_NOTE.contains("feedback turn"));
        assert!(SANDBOX_NOTE.contains("committing is never allowed"));
        assert!(SANDBOX_NOTE.contains("discarded when this turn ends"));
        assert!(SANDBOX_NOTE.contains("never try to reach the original directory"));
        assert!(!SANDBOX_NOTE.contains("hard-reset"));
    }

    #[test]
    fn feedback_note_states_the_reset_contract() {
        assert!(FEEDBACK_NOTE.contains("feedback turn"));
        assert!(FEEDBACK_NOTE.contains("hard-reset"));
        assert!(FEEDBACK_NOTE.contains("committing is never allowed"));
        assert!(FEEDBACK_NOTE.contains("not recorded to this conversation"));
        assert!(!FEEDBACK_NOTE.contains("only way to keep a change"));
    }

    #[test]
    fn commit_nudge_is_marked_as_injected() {
        use crate::agent::Message;
        let Message::User { content, injected } = commit_nudge(" M a.txt\n?? b.txt\n") else {
            panic!("a nudge is a user message");
        };
        assert!(injected);
        assert!(content.contains(" M a.txt\n?? b.txt\n\nCommit them now."));
    }

    #[tokio::test]
    async fn a_knowledge_entry_left_staged_is_asked_for_in_a_nudge() {
        let (_tmp, root) = temp_repo();
        commit(&root, "a.txt", "1", "init");
        let dir = root.join(crate::knowledge::DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("entry.md"), "An entry.\n").unwrap();
        git(&root, &["add", "-A"]);
        // A plain commit turn is asked to commit what it left staged, knowledge included.
        use crate::agent::Message;
        let nudge = next_commit_nudge(&root, 0).await.unwrap().unwrap();
        let Message::User { content, .. } = nudge else {
            panic!("a nudge is a user message");
        };
        assert!(content.contains("docs/knowledge/entry.md"), "{content}");
    }

    #[test]
    fn claude_prompt_has_the_mode_but_not_project_instructions() {
        use crate::agent::Mode;
        let (_tmp, root) = temp_root();
        std::fs::write(root.join("AGENTS.md"), "Use tabs.\n").unwrap();
        let prompt = claude_system_prompt(Some(Mode::Commit), 3);
        assert!(prompt.contains("**commit mode**"));
        assert!(!prompt.contains("Use tabs."));
        assert!(!prompt.contains("## The conversation tree"));
        assert!(!prompt.contains("- HEAD:"));
    }

    #[test]
    fn handover_preamble_replays_ancestors_as_text() {
        let (_tmp, root) = temp_root();
        let first = root.join("a");
        let second = root.join("b");
        let empty = root.join("c");
        for dir in [&first, &second, &empty] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(
            first.join(crate::conversation::files::PROMPT),
            "question one\n",
        )
        .unwrap();
        std::fs::write(
            first.join(crate::conversation::files::REPLY),
            "answer one\n",
        )
        .unwrap();
        std::fs::write(
            second.join(crate::conversation::files::PROMPT),
            "question two",
        )
        .unwrap();

        let preamble = handover_preamble(&[first, second, empty.clone()]).unwrap();
        let body =
            "## User\n\nquestion one\n\n## Assistant\n\nanswer one\n\n## User\n\nquestion two\n\n";
        assert!(preamble.contains(body), "{preamble}");
        assert!(preamble.starts_with("[Note from Explorer:"));
        assert!(handover_preamble(&[empty]).is_none());
    }

    #[test]
    fn the_budget_note_names_the_budget_and_both_times() {
        let text = budget_note_at(Duration::from_secs(900), 14 * 3600 + 32 * 60, 0);
        assert!(text.contains("15 minutes"));
        assert!(text.contains("14:32"));
        assert!(text.contains("14:47"));
        assert!(text.contains("`date`"));
    }

    #[test]
    fn a_tail_left_inside_a_round_gets_synthetic_tool_results() {
        use crate::agent::Message;
        use crate::agent::ToolCall;
        let mut messages = vec![
            Message::User {
                content: "hi".into(),
                injected: false,
            },
            Message::Assistant {
                content: "looking".into(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                }],
            },
        ];
        repair_tail(&mut messages);
        assert!(matches!(
            &messages[1],
            Message::Assistant { tool_calls, .. } if tool_calls.len() == 1
        ));
        assert_eq!(
            messages[2..],
            [Message::Tool {
                call_id: "c1".into(),
                name: "read".into(),
                content: "[interrupted before this call ran]".into(),
            }]
        );

        // A valid tail is left alone.
        let mut done = vec![Message::Assistant {
            content: "done".into(),
            tool_calls: Vec::new(),
        }];
        repair_tail(&mut done);
        assert_eq!(done.len(), 1);
    }

    fn call(id: &str) -> crate::agent::ToolCall {
        crate::agent::ToolCall {
            id: id.into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
        }
    }

    fn result(id: &str) -> crate::agent::Message {
        use crate::agent::Message;
        Message::Tool {
            call_id: id.into(),
            name: "read".into(),
            content: "ok".into(),
        }
    }

    #[test]
    fn a_round_cut_after_some_results_answers_only_the_missing_calls() {
        use crate::agent::Message;
        let mut messages = vec![
            Message::Assistant {
                content: String::new(),
                tool_calls: vec![call("c1"), call("c2"), call("c3")],
            },
            result("c1"),
        ];
        repair_tail(&mut messages);
        let ids: Vec<&str> = messages
            .iter()
            .filter_map(|m| match m {
                Message::Tool { call_id, .. } => Some(call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["c1", "c2", "c3"]);
        assert_eq!(messages[1], result("c1"));
        assert!(matches!(
            &messages[0],
            Message::Assistant { tool_calls, .. } if tool_calls.len() == 3
        ));
    }

    #[test]
    fn a_fully_answered_round_is_left_alone() {
        use crate::agent::Message;
        let mut messages = vec![
            Message::Assistant {
                content: String::new(),
                tool_calls: vec![call("c1"), call("c2")],
            },
            result("c1"),
            result("c2"),
        ];
        let before = messages.clone();
        repair_tail(&mut messages);
        assert_eq!(messages, before);
    }
}
