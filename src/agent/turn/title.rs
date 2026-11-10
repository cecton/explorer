//! The title and summary of a finished node: one commit-message-style call to the
//! summary model, which the tree pane runs again for a node it finds undescribed.

const TITLE_MAX_CHARS: usize = 100;
pub(crate) const REPLY_MAX_CHARS: usize = 8000;
const COMMITS_MAX: usize = 20;

/// What the summary model reads: the prompt, the end of the reply and the commits made.
fn title_input(prompt: &str, reply: &str, commits: &[String]) -> String {
    let reply = match reply.char_indices().nth(REPLY_MAX_CHARS) {
        Some((end, _)) => &reply[..end],
        None => reply,
    };
    let mut input = format!(
        "<prompt>\n{}\n</prompt>\n\n<reply>\n{}\n</reply>\n",
        prompt.trim(),
        reply.trim()
    );
    if !commits.is_empty() {
        input.push_str(&format!(
            "\n<commits>\n{}\n</commits>\n",
            commits.join("\n")
        ));
    }
    input.push_str("\nWrite the title and the summary of the turn recorded above.\n");
    input
}

/// Splits the model's answer into the title (first line) and the summary (the rest).
pub(crate) fn parse_title_and_summary(answer: &str) -> Option<(String, String)> {
    let answer = answer.trim();
    let (first, rest) = answer.split_once('\n').unwrap_or((answer, ""));
    let title: String = crate::util::text::single_line(first)
        .chars()
        .take(TITLE_MAX_CHARS)
        .collect();
    (!title.is_empty()).then(|| (title, crate::util::text::single_line(rest)))
}

/// One answer without tools from the summary model.
pub(crate) async fn complete(
    backend: &crate::agent::Backend,
    session: &str,
    system: &str,
    user: &str,
) -> Result<String, String> {
    use crate::agent::Backend;
    match backend {
        Backend::OpenAi(target) => crate::agent::llm::complete(target, session, system, user).await,
        Backend::ClaudeCode(target) => {
            crate::agent::claude_code::complete(target, system, user).await
        }
    }
}

/// Writes the title and summary of a node from its prompt, reply and commits.
#[allow(clippy::too_many_arguments)]
pub async fn retitle(
    node: &crate::conversation::NodeId,
    node_dir: &camino::Utf8Path,
    session: &str,
    root: &camino::Utf8Path,
    head_before: Option<String>,
    head_after: Option<String>,
    backend: &crate::agent::Backend,
    main_tx: &crate::signal::MainTx,
) {
    let root = root.to_path_buf();
    let commits = tokio::task::spawn_blocking(move || match (head_before, head_after) {
        (Some(before), Some(after)) => crate::git::log_oneline(&root, &before, &after, COMMITS_MAX),
        _ => Vec::new(),
    })
    .await
    .unwrap_or_default();
    let (prompt, reply) = crate::conversation::files::read_prompt_and_reply(node_dir);
    let input = title_input(&prompt, &reply, &commits);
    title_and_summarize(node, node_dir, session, backend, &input, main_tx).await;
}

/// Replaces `title.md` and `summary.md` of a finished node with what the model makes of
/// its turn.
async fn title_and_summarize(
    node: &crate::conversation::NodeId,
    node_dir: &camino::Utf8Path,
    session: &str,
    backend: &crate::agent::Backend,
    input: &str,
    main_tx: &crate::signal::MainTx,
) {
    use crate::signal::AppSignal;
    let answer = match complete(
        backend,
        session,
        crate::agent::context::TITLE_SUMMARY_PROMPT,
        input,
    )
    .await
    {
        Ok(answer) => answer,
        Err(e) => {
            tracing::warn!("title generation failed: {e}");
            return;
        }
    };
    let Some((title, summary)) = parse_title_and_summary(&answer) else {
        tracing::warn!("title generation returned no title");
        return;
    };
    if let Err(e) = crate::conversation::files::write_title(node_dir, &title) {
        tracing::warn!("failed to write the title of {node_dir}: {e}");
    }
    crate::signal::send(
        main_tx,
        AppSignal::NodeTitleReady {
            node: node.clone(),
            title,
        },
    );
    if summary.is_empty() {
        return;
    }
    if let Err(e) = crate::conversation::files::write_summary(node_dir, &summary) {
        tracing::warn!("failed to write the summary of {node_dir}: {e}");
    }
    crate::signal::send(
        main_tx,
        AppSignal::NodeSummaryReady {
            node: node.clone(),
            summary,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_title_and_summary() {
        let (title, summary) =
            parse_title_and_summary("  Add a thing\n\nIt does this\nand that.\n").unwrap();
        assert_eq!(title, "Add a thing");
        assert_eq!(summary, "It does this and that.");
    }

    #[test]
    fn title_only_answer_has_empty_summary() {
        let (title, summary) = parse_title_and_summary("Just a title").unwrap();
        assert_eq!(title, "Just a title");
        assert_eq!(summary, "");
    }

    #[test]
    fn title_is_capped_and_empty_answer_rejected() {
        let (title, _) = parse_title_and_summary(&"x".repeat(300)).unwrap();
        assert_eq!(title.chars().count(), TITLE_MAX_CHARS);
        assert!(parse_title_and_summary("  \n ").is_none());
    }

    #[test]
    fn title_input_truncates_reply_and_lists_commits() {
        let reply = "é".repeat(REPLY_MAX_CHARS + 10);
        let input = title_input("do it", &reply, &["abc1234 feat: x".to_string()]);
        assert!(input.starts_with("<prompt>\ndo it\n</prompt>\n\n<reply>\n"));
        assert_eq!(input.matches('é').count(), REPLY_MAX_CHARS);
        assert!(input.contains("\n<commits>\nabc1234 feat: x\n</commits>\n"));
        assert!(input.ends_with("recorded above.\n"));
    }
}
