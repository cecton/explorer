//! The files a node or a sub-agent run keeps, and the reading and writing of the ones
//! that have a shape.

/// The metadata of a node or a run: `key: value` lines.
pub(crate) const METADATA: &str = "metadata.md";
/// The prompt as sent to the model, after a slash command expanded it.
pub(crate) const PROMPT: &str = "prompt.md";
/// The prompt as typed, kept only when it differs from [`PROMPT`].
pub(crate) const INPUT: &str = "input.md";
/// What the turn answered, for an answer that called no tools.
pub(crate) const REPLY: &str = "reply.md";
/// One line per tool call and the text of answers that called one; display only.
pub(crate) const ACTIVITY: &str = "activity.md";
/// The messages a child node replays.
pub(crate) const TRANSCRIPT: &str = "transcript.jsonl";

/// What the summary model made of the node.
pub(crate) const TITLE: &str = "title.md";
/// What the summary model made of the node's outcome.
pub(crate) const SUMMARY: &str = "summary.md";
/// Directory holding the sub-agent runs of a node's turn; never a node of its own.
pub(crate) const AGENTS_DIR: &str = "agents";
/// Directory holding the output logs of a node's background commands; never a node of
/// its own.
pub const PROCESSES_DIR: &str = "processes";

/// A node's [`PROMPT`]; a missing file reads as empty text.
pub fn read_prompt(dir: &camino::Utf8Path) -> String {
    read(dir, PROMPT)
}

/// A node's [`REPLY`]; a missing file reads as empty text.
pub fn read_reply(dir: &camino::Utf8Path) -> String {
    read(dir, REPLY)
}

/// A node's [`PROMPT`] and [`REPLY`] at once, for callers that want both.
pub fn read_prompt_and_reply(dir: &camino::Utf8Path) -> (String, String) {
    (read_prompt(dir), read_reply(dir))
}

/// A node's [`INPUT`]: its prompt as typed, kept only when a slash command expanded it.
pub fn read_typed_input(dir: &camino::Utf8Path) -> Option<String> {
    std::fs::read_to_string(dir.join(INPUT)).ok()
}

/// What the user typed for a node: its [`INPUT`], or the prompt when it was sent as
/// typed. This is the text to give back for editing.
pub fn read_input(dir: &camino::Utf8Path) -> String {
    read_typed_input(dir).unwrap_or_else(|| read_prompt(dir))
}

/// The lines of a node's [`ACTIVITY`], the work log of its turn; empty without one.
pub fn read_work_log(dir: &camino::Utf8Path) -> Vec<String> {
    std::fs::read_to_string(dir.join(ACTIVITY))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// A node's [`TITLE`], one line: the summary model writes a title, not a paragraph.
pub(crate) fn read_title(dir: &camino::Utf8Path) -> Option<String> {
    read_trimmed(dir, TITLE).map(|text| crate::util::text::single_line(&text))
}

/// A node's [`SUMMARY`].
pub(crate) fn read_summary(dir: &camino::Utf8Path) -> Option<String> {
    read_trimmed(dir, SUMMARY)
}

/// A node's [`INPUT`] when it says something, trimmed.
pub(crate) fn read_input_file(dir: &camino::Utf8Path) -> Option<String> {
    read_trimmed(dir, INPUT)
}

/// A node's [`PROMPT`] when it says something, trimmed.
pub(crate) fn read_prompt_file(dir: &camino::Utf8Path) -> Option<String> {
    read_trimmed(dir, PROMPT)
}

/// Writes a node's [`TITLE`].
pub fn write_title(dir: &camino::Utf8Path, title: &str) -> std::io::Result<()> {
    crate::util::fs::write_atomic(&dir.join(TITLE), title)
}

/// Writes a node's [`SUMMARY`].
pub fn write_summary(dir: &camino::Utf8Path, summary: &str) -> std::io::Result<()> {
    crate::util::fs::write_atomic(&dir.join(SUMMARY), summary)
}

/// Writes a node's [`REPLY`].
pub fn write_reply(dir: &camino::Utf8Path, reply: &str) -> std::io::Result<()> {
    crate::util::fs::write_atomic(&dir.join(REPLY), reply)
}

/// Whether a turn left anything of its own: reply text or commits. A turn with content
/// is part of the discussion even when its run was cut short; one without has nothing
/// to say and nothing to build on.
pub fn has_content(reply: &str) -> bool {
    !reply.trim().is_empty()
}

fn read(dir: &camino::Utf8Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

fn read_trimmed(dir: &camino::Utf8Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name))
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::test_support::temp_root;
    use crate::conversation::test_support::write_node;

    #[test]
    fn a_missing_file_reads_as_empty_and_a_written_one_reads_back() {
        let (_tmp, root) = temp_root();
        let dir = root.join("a1");
        write_node(&dir, &[]);
        assert_eq!(read_prompt(&dir), "");
        assert_eq!(read_reply(&dir), "");
        assert!(read_title(&dir).is_none());
        assert!(read_summary(&dir).is_none());

        write_reply(&dir, "done").unwrap();
        write_title(&dir, "A title\nwrapped").unwrap();
        write_summary(&dir, "What it came to.").unwrap();
        assert_eq!(read_reply(&dir), "done");
        assert_eq!(read_title(&dir).as_deref(), Some("A title wrapped"));
        assert_eq!(read_summary(&dir).as_deref(), Some("What it came to."));
    }

    #[test]
    fn an_empty_title_file_is_no_title() {
        let (_tmp, root) = temp_root();
        let dir = root.join("a1");
        write_node(&dir, &[(TITLE, "  \n")]);
        assert!(read_title(&dir).is_none());
    }

    #[test]
    fn the_input_is_the_typed_prompt_or_the_prompt_as_sent() {
        let (_tmp, root) = temp_root();
        let dir = root.join("a1");
        write_node(&dir, &[(PROMPT, "Review the change.\n\nfoo")]);
        assert!(read_typed_input(&dir).is_none());
        assert_eq!(read_input(&dir), "Review the change.\n\nfoo");

        write_node(&dir, &[(INPUT, "/review foo")]);
        assert_eq!(read_input(&dir), "/review foo");
    }

    #[test]
    fn a_work_log_reads_one_entry_per_line() {
        let (_tmp, root) = temp_root();
        let dir = root.join("a1");
        write_node(&dir, &[(ACTIVITY, "Reading.\n- read a.txt\n")]);
        assert_eq!(read_work_log(&dir), ["Reading.", "- read a.txt"]);
        write_node(&root.join("a2"), &[]);
        assert!(read_work_log(&root.join("a2")).is_empty());
    }

    #[test]
    fn content_is_any_text_that_is_not_blank() {
        assert!(has_content("a word"));
        assert!(!has_content(""));
        assert!(!has_content(" \n\t"));
    }
}
