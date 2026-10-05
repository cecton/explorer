use super::{Message, Mode};
use camino::Utf8Path;

const FEEDBACK_PROMPT: &str = include_str!("../../prompts/feedback.md");
const COMMIT_PROMPT: &str = include_str!("../../prompts/commit.md");
pub const TITLE_PROMPT: &str = include_str!("../../prompts/title.md");
pub const SUMMARY_PROMPT: &str = include_str!("../../prompts/summary.md");

/// Commits listed in a repository-moved note before it says there are more.
pub const NOTE_MAX_COMMITS: usize = 20;

/// The system prompt of a turn: mode instructions, the project's own instructions,
/// and the state of the repository when the prompt was sent.
pub fn system_prompt(mode: Mode, root: &Utf8Path, head: Option<&str>, date: &str) -> String {
    let mut out = String::from(mode_prompt(mode));
    if let Ok(agents) = std::fs::read_to_string(root.join("AGENTS.md")) {
        out.push_str("\n\n## Project instructions (AGENTS.md)\n\n");
        out.push_str(agents.trim());
    }
    out.push_str(&environment(root, head, date));
    out
}

/// What is appended to Claude Code's own system prompt: it loads the project's
/// instructions itself, so only the mode and the environment are added.
pub fn claude_system_prompt(mode: Mode, root: &Utf8Path, head: Option<&str>, date: &str) -> String {
    format!("{}{}", mode_prompt(mode), environment(root, head, date))
}

fn mode_prompt(mode: Mode) -> &'static str {
    match mode {
        Mode::Feedback => FEEDBACK_PROMPT,
        Mode::Commit => COMMIT_PROMPT,
    }
    .trim_end()
}

fn environment(root: &Utf8Path, head: Option<&str>, date: &str) -> String {
    let mut out = String::from("\n\n## Environment\n\n");
    out.push_str(&format!("- Repository root: {root}\n- Date: {date}\n"));
    match head {
        Some(head) => out.push_str(&format!("- HEAD: {head}\n")),
        None => out.push_str("- HEAD: none yet (no commit)\n"),
    }
    out
}

/// The messages `transcript.jsonl` of a node holds, or `None` without a usable one.
fn read_transcript(dir: &Utf8Path) -> Option<Vec<Message>> {
    let text = std::fs::read_to_string(dir.join("transcript.jsonl")).ok()?;
    let messages: Vec<Message> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .ok()?;
    (!messages.is_empty()).then_some(messages)
}

/// What an ancestor contributes to the context: its exact transcript, or, when it has
/// none, its prompt and reply as plain text.
pub fn ancestor_messages(dir: &Utf8Path) -> Vec<Message> {
    if let Some(messages) = read_transcript(dir) {
        return messages;
    }
    let (prompt, reply) = crate::conversation::read_prompt_and_reply(dir);
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
        let (prompt, reply) = crate::conversation::read_prompt_and_reply(dir);
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

/// The user message as sent: the prompt, preceded by a note when there is one.
pub fn user_message(note: Option<&str>, prompt: &str) -> Message {
    let content = match note {
        Some(note) => format!("{note}\n\n{prompt}"),
        None => prompt.to_string(),
    };
    Message::User {
        content,
        injected: false,
    }
}

/// Hidden message sent when a commit turn stops while the working tree is still dirty.
/// `status` is the output of `git status --short`.
pub fn commit_nudge(status: &str) -> Message {
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
    use camino::Utf8PathBuf;

    fn temp_dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        (tmp, dir)
    }

    #[test]
    fn ancestor_with_transcript_is_replayed_exactly() {
        let (_tmp, dir) = temp_dir();
        std::fs::write(dir.join("prompt.md"), "ignored").unwrap();
        std::fs::write(
            dir.join("transcript.jsonl"),
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
        let (_tmp, dir) = temp_dir();
        std::fs::write(dir.join("prompt.md"), "question").unwrap();
        std::fs::write(dir.join("reply.md"), "answer").unwrap();
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
        std::fs::write(dir.join("transcript.jsonl"), "not json\n").unwrap();
        assert_eq!(ancestor_messages(&dir).len(), 2);
    }

    #[test]
    fn empty_ancestor_contributes_nothing() {
        let (_tmp, dir) = temp_dir();
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
    fn user_message_puts_the_note_before_the_prompt() {
        assert_eq!(
            user_message(Some("[note]"), "do it"),
            Message::User {
                content: "[note]\n\ndo it".into(),
                injected: false,
            }
        );
        assert_eq!(
            user_message(None, "do it"),
            Message::User {
                content: "do it".into(),
                injected: false,
            }
        );
    }

    #[test]
    fn system_prompt_includes_project_instructions_and_environment() {
        let (_tmp, root) = temp_dir();
        std::fs::write(root.join("AGENTS.md"), "Use tabs.\n").unwrap();
        let prompt = system_prompt(Mode::Feedback, &root, Some("abc123"), "2026-10-05");
        assert!(prompt.starts_with("You are the coding agent inside Explorer"));
        assert!(prompt.contains("**feedback mode**"));
        let commit = system_prompt(Mode::Commit, &root, None, "2026-10-05");
        assert!(commit.contains("**commit mode**"));
        assert!(commit.ends_with("- HEAD: none yet (no commit)\n"));
        assert!(prompt.contains("## Project instructions (AGENTS.md)\n\nUse tabs."));
        assert!(prompt.ends_with("- Date: 2026-10-05\n- HEAD: abc123\n"));
    }

    #[test]
    fn commit_nudge_is_marked_as_injected() {
        let Message::User { content, injected } = commit_nudge(" M a.txt\n?? b.txt\n") else {
            panic!("a nudge is a user message");
        };
        assert!(injected);
        assert!(content.contains(" M a.txt\n?? b.txt\n\nCommit them now."));
    }

    #[test]
    fn claude_prompt_has_mode_and_environment_but_not_project_instructions() {
        let (_tmp, root) = temp_dir();
        std::fs::write(root.join("AGENTS.md"), "Use tabs.\n").unwrap();
        let prompt = claude_system_prompt(Mode::Commit, &root, Some("abc"), "2026-10-05");
        assert!(prompt.contains("**commit mode**"));
        assert!(!prompt.contains("Use tabs."));
        assert!(prompt.ends_with("- HEAD: abc\n"));
    }

    #[test]
    fn handover_preamble_replays_ancestors_as_text() {
        let (_tmp, root) = temp_dir();
        let first = root.join("a");
        let second = root.join("b");
        let empty = root.join("c");
        for dir in [&first, &second, &empty] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(first.join("prompt.md"), "question one\n").unwrap();
        std::fs::write(first.join("reply.md"), "answer one\n").unwrap();
        std::fs::write(second.join("prompt.md"), "question two").unwrap();

        let preamble = handover_preamble(&[first, second, empty.clone()]).unwrap();
        let body =
            "## User\n\nquestion one\n\n## Assistant\n\nanswer one\n\n## User\n\nquestion two\n\n";
        assert!(preamble.contains(body), "{preamble}");
        assert!(preamble.starts_with("[Note from Explorer:"));
        assert!(handover_preamble(&[empty]).is_none());
    }
}
