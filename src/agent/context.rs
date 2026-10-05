use super::Message;
use camino::Utf8Path;

const FEEDBACK_PROMPT: &str = include_str!("../../prompts/feedback.md");
pub const TITLE_PROMPT: &str = include_str!("../../prompts/title.md");
pub const SUMMARY_PROMPT: &str = include_str!("../../prompts/summary.md");

/// Commits listed in a repository-moved note before it says there are more.
pub const NOTE_MAX_COMMITS: usize = 20;

/// The system prompt of a turn: mode instructions, the project's own instructions,
/// and the state of the repository when the prompt was sent.
pub fn system_prompt(root: &Utf8Path, head: Option<&str>, date: &str) -> String {
    let mut out = String::from(FEEDBACK_PROMPT.trim_end());
    if let Ok(agents) = std::fs::read_to_string(root.join("AGENTS.md")) {
        out.push_str("\n\n## Project instructions (AGENTS.md)\n\n");
        out.push_str(agents.trim());
    }
    out.push_str("\n\n## Environment\n\n");
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
        messages.push(Message::User { content: prompt });
        if !reply.trim().is_empty() {
            messages.push(Message::Assistant {
                content: reply,
                tool_calls: Vec::new(),
            });
        }
    }
    messages
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
    Message::User { content }
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
                    content: "[note]\n\nhi".into()
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
                    content: "question".into()
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
                content: "[note]\n\ndo it".into()
            }
        );
        assert_eq!(
            user_message(None, "do it"),
            Message::User {
                content: "do it".into()
            }
        );
    }

    #[test]
    fn system_prompt_includes_project_instructions_and_environment() {
        let (_tmp, root) = temp_dir();
        std::fs::write(root.join("AGENTS.md"), "Use tabs.\n").unwrap();
        let prompt = system_prompt(&root, Some("abc123"), "2026-10-05");
        assert!(prompt.starts_with("You are the coding agent inside Explorer"));
        assert!(prompt.contains("## Project instructions (AGENTS.md)\n\nUse tabs."));
        assert!(prompt.ends_with("- Date: 2026-10-05\n- HEAD: abc123\n"));
    }
}
