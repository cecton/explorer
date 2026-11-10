//! Symbol highlights: the hover then references round-trip that turns a click in a
//! preview into a qualified name and the places that name appears.

#[derive(Clone, Debug)]
pub(super) enum RefQueryPhase {
    Hover,
    References,
}

pub(super) struct PendingRefQuery {
    pub(super) file_idx: usize,
    pub(super) line: u32,
    pub(super) character: u32,
    pub(super) word: Option<String>,
    pub(super) phase: RefQueryPhase,
    pub(super) qualified_name: Option<String>,
    pub(super) group_id: Option<usize>,
}

/// The qualified name a hover answer states, or `None` when it states none.
///
/// A markdown answer wraps the name in its first code span. A plain-text answer names a
/// parent type on its first line, which for a struct field is the parent alone, so `word`
/// — what the user clicked — is appended to keep two fields of one struct apart.
pub(super) fn qualified_name(
    contents: &lsp_types::HoverContents,
    word: Option<&str>,
) -> Option<String> {
    let lsp_types::HoverContents::Markup(content) = contents else {
        return None;
    };
    let name = match content.kind {
        lsp_types::MarkupKind::Markdown => {
            let mut parts = content.value.split('`');
            parts.nth(1)?.to_string()
        }
        lsp_types::MarkupKind::PlainText => {
            let parent = content.value.lines().next()?;
            match word {
                Some(w) => {
                    let last_seg = parent.rsplit("::").next().unwrap_or("");
                    if last_seg != w && !parent.contains(&format!("::{w}")) {
                        format!("{parent}::{w}")
                    } else {
                        parent.to_string()
                    }
                }
                None => parent.to_string(),
            }
        }
    };
    name.contains("::").then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markdown(value: &str) -> lsp_types::HoverContents {
        lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
            kind: lsp_types::MarkupKind::Markdown,
            value: value.to_string(),
        })
    }

    fn plain_text(value: &str) -> lsp_types::HoverContents {
        lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
            kind: lsp_types::MarkupKind::PlainText,
            value: value.to_string(),
        })
    }

    #[test]
    fn a_markdown_answer_names_the_first_code_span() {
        assert_eq!(
            qualified_name(&markdown("`crate::loader::FileKey`\n\nA key."), None),
            Some("crate::loader::FileKey".to_string())
        );
    }

    #[test]
    fn a_markdown_answer_without_a_code_span_names_nothing() {
        assert_eq!(qualified_name(&markdown("A doc comment only."), None), None);
    }

    /// A name without `::` is what a keyword or a builtin answers: the group resolves to
    /// nothing rather than hanging on a query that cannot find references.
    #[test]
    fn a_name_that_is_not_qualified_names_nothing() {
        assert_eq!(qualified_name(&markdown("`String`"), None), None);
        assert_eq!(qualified_name(&plain_text("u8"), Some("u8")), None);
    }

    #[test]
    fn a_plain_text_answer_names_its_first_line() {
        assert_eq!(
            qualified_name(&plain_text("crate::loader::FileKey\n\nA key."), None),
            Some("crate::loader::FileKey".to_string())
        );
    }

    /// A struct field hovers as its parent type alone, so the clicked word finishes the
    /// name; without it two fields of one struct would share a group.
    #[test]
    fn a_plain_text_field_answer_appends_the_clicked_word() {
        assert_eq!(
            qualified_name(&plain_text("crate::loader::FileKey"), Some("index")),
            Some("crate::loader::FileKey::index".to_string())
        );
    }

    #[test]
    fn a_plain_text_answer_already_naming_the_word_gains_no_suffix() {
        assert_eq!(
            qualified_name(&plain_text("crate::loader::FileKey"), Some("FileKey")),
            Some("crate::loader::FileKey".to_string())
        );
        assert_eq!(
            qualified_name(&plain_text("crate::loader::FileKey::index"), Some("index")),
            Some("crate::loader::FileKey::index".to_string())
        );
    }

    #[test]
    fn an_answer_that_is_not_markup_names_nothing() {
        assert_eq!(
            qualified_name(
                &lsp_types::HoverContents::Scalar(lsp_types::MarkedString::String(
                    "crate::loader::FileKey".to_string()
                )),
                None
            ),
            None
        );
        assert_eq!(
            qualified_name(&lsp_types::HoverContents::Array(Vec::new()), None),
            None
        );
    }

    #[test]
    fn an_empty_plain_text_answer_names_nothing() {
        assert_eq!(qualified_name(&plain_text(""), None), None);
    }
}
