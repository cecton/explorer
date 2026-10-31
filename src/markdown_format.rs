//! The markdown formatter behind `fmt-md`, the `format_markdown` tool and commit turns.

use camino::{Utf8Path, Utf8PathBuf};
use dprint_core::configuration::NewLineKind;
use dprint_plugin_markdown::configuration::{Configuration, ConfigurationBuilder, TextWrap};

/// Directories (relative to the repository root) whose markdown is not ours to format.
const EXCLUDED: [&str; 3] = ["vendor", "target", "tests/fixtures"];

fn configuration() -> Configuration {
    // Maintain keeps the author's line breaks; Auto keeps a file's own CRLF or LF endings.
    ConfigurationBuilder::new()
        .text_wrap(TextWrap::Maintain)
        .new_line_kind(NewLineKind::Auto)
        .build()
}

/// `text` in the formatter's layout: tables aligned, prose line breaks kept.
pub fn format_markdown(text: &str) -> Result<String, String> {
    let formatted = dprint_plugin_markdown::format_text(text, &configuration(), |_, _, _| Ok(None))
        .map_err(|e| e.to_string())?;
    Ok(formatted.unwrap_or_else(|| text.to_string()))
}

/// Whether a repository-relative path is a markdown file the formatter owns.
pub fn is_formatted(relative: &Utf8Path) -> bool {
    relative.as_str().ends_with(".md") && !EXCLUDED.iter().any(|dir| relative.starts_with(dir))
}

/// Replaces each directory with the files git lists under it; other paths are kept.
pub fn expand_directories(
    root: &Utf8Path,
    relatives: Vec<Utf8PathBuf>,
) -> Result<Vec<Utf8PathBuf>, String> {
    let mut expanded = Vec::new();
    for relative in relatives {
        if root.join(&relative).is_dir() {
            expanded.extend(crate::git::files_under(root, &relative)?);
        } else {
            expanded.push(relative);
        }
    }
    Ok(expanded)
}

/// Formats the files in place and returns the paths rewritten; a read error writes nothing.
pub fn format_files(
    root: &Utf8Path,
    relatives: &[Utf8PathBuf],
) -> Result<Vec<Utf8PathBuf>, String> {
    let mut rewrites: Vec<(&Utf8PathBuf, Utf8PathBuf, String)> = Vec::new();
    for relative in relatives {
        if !is_formatted(relative) || rewrites.iter().any(|(listed, _, _)| *listed == relative) {
            continue;
        }
        let path = root.join(relative);
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{relative}: {e}"))?;
        let formatted = format_markdown(&text).map_err(|e| format!("{relative}: {e}"))?;
        if formatted != text {
            rewrites.push((relative, path, formatted));
        }
    }
    let mut changed = Vec::new();
    for (relative, path, formatted) in rewrites {
        std::fs::write(&path, formatted).map_err(|e| format!("{relative}: {e}"))?;
        changed.push(relative.clone());
    }
    Ok(changed)
}

/// The empty tree: the base of a repository that had no commit before the turn.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Amends the markdown a commit turn committed into HEAD; skipped while the tree is dirty.
pub fn format_committed(root: &Utf8Path, before: Option<&str>, after: Option<&str>) -> bool {
    let Some(after) = after else {
        return false;
    };
    if before == Some(after) {
        return false;
    }
    let before = before.unwrap_or(EMPTY_TREE);
    let result = (|| {
        if !crate::git::status_short(root)?.trim().is_empty() {
            return Ok(false);
        }
        let files = crate::git::files_changed_between(root, before, after)?;
        if format_files(root, &files)?.is_empty() {
            return Ok(false);
        }
        crate::git::amend_all(root)?;
        Ok::<_, String>(true)
    })();
    result.unwrap_or_else(|error| {
        tracing::warn!("cannot format the markdown of the turn: {error}");
        false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_line_breaks_of_prose() {
        let text = "A short line\nand another one\nthat stays.\n";
        assert_eq!(format_markdown(text).unwrap(), text);
    }

    #[test]
    fn keeps_crlf_line_endings() {
        let text = "A short line\r\nand another one\r\n";
        assert_eq!(format_markdown(text).unwrap(), text);
    }

    #[test]
    fn aligns_tables() {
        let formatted = format_markdown("|a|bbb|\n|-|-|\n|cc|d|\n").unwrap();
        assert_eq!(formatted, "| a  | bbb |\n| -- | --- |\n| cc | d   |\n");
    }

    #[test]
    fn leaves_fences_alone() {
        let text = "```rust\nfn  main( ) {}\n```\n";
        assert_eq!(format_markdown(text).unwrap(), text);
    }

    #[test]
    fn is_idempotent() {
        let text =
            "# Title\n\n* one\n* two\n    * nested\n\n|a|b|\n|-|-|\n|1|22|\n\n\n\nText   here.\n";
        let once = format_markdown(text).unwrap();
        assert_ne!(once, text);
        assert_eq!(format_markdown(&once).unwrap(), once);
    }

    #[test]
    fn owns_markdown_outside_the_excluded_directories() {
        assert!(is_formatted(Utf8Path::new("README.md")));
        assert!(is_formatted(Utf8Path::new("prompts/commit.md")));
        assert!(!is_formatted(Utf8Path::new("src/main.rs")));
        assert!(!is_formatted(Utf8Path::new("vendor/x/README.md")));
        assert!(!is_formatted(Utf8Path::new("target/doc/a.md")));
        assert!(!is_formatted(Utf8Path::new("tests/fixtures/jqwik/a.md")));
    }

    #[test]
    fn formats_files_in_place_and_reports_the_changed_ones() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        std::fs::write(root.join("a.md"), "|a|b|\n|-|-|\n|1|2|\n").unwrap();
        std::fs::write(root.join("b.md"), "fine\n").unwrap();
        std::fs::write(root.join("c.txt"), "|a|b|\n|-|-|\n").unwrap();
        let relatives = ["a.md", "b.md", "c.txt"].map(Utf8PathBuf::from);
        let changed = format_files(root, &relatives).unwrap();
        assert_eq!(changed, [Utf8PathBuf::from("a.md")]);
        assert_eq!(
            std::fs::read_to_string(root.join("a.md")).unwrap(),
            "| a | b |\n| - | - |\n| 1 | 2 |\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("c.txt")).unwrap(),
            "|a|b|\n|-|-|\n"
        );
        assert!(format_files(root, &relatives).unwrap().is_empty());
        std::fs::write(root.join("a.md"), "|a|b|\n|-|-|\n").unwrap();
        let twice = ["a.md", "a.md"].map(Utf8PathBuf::from);
        assert_eq!(
            format_files(root, &twice).unwrap(),
            [Utf8PathBuf::from("a.md")]
        );
    }

    #[test]
    fn writes_nothing_when_a_listed_file_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        std::fs::write(root.join("a.md"), "|a|b|\n|-|-|\n").unwrap();
        let relatives = ["a.md", "missing.md"].map(Utf8PathBuf::from);
        assert!(format_files(root, &relatives).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("a.md")).unwrap(),
            "|a|b|\n|-|-|\n"
        );
    }

    #[test]
    fn lists_and_formats_untracked_files_with_non_ascii_names() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        git(root, &["init", "-q"]);
        std::fs::write(root.join("café.md"), "|a|b|\n|-|-|\n").unwrap();
        let changes = crate::git::working_tree_changes(root).unwrap();
        assert_eq!(changes, [Utf8PathBuf::from("café.md")]);
        assert_eq!(
            format_files(root, &changes).unwrap(),
            [Utf8PathBuf::from("café.md")]
        );
    }

    #[test]
    fn a_directory_expands_to_its_markdown_without_ignored_or_excluded_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        git(root, &["init", "-q"]);
        for dir in ["docs/deep", "vendor", "build"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let table = "|a|b|\n|-|-|\n";
        for file in [
            "top.md",
            "docs/a.md",
            "docs/deep/b.md",
            "vendor/c.md",
            "build/d.md",
        ] {
            std::fs::write(root.join(file), table).unwrap();
        }
        std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
        let from_root = expand_directories(root, vec![Utf8PathBuf::from("")]).unwrap();
        let mut changed = format_files(root, &from_root).unwrap();
        changed.sort();
        assert_eq!(
            changed,
            ["docs/a.md", "docs/deep/b.md", "top.md"].map(Utf8PathBuf::from)
        );
        std::fs::write(root.join("docs/a.md"), table).unwrap();
        let from_docs = expand_directories(root, vec![Utf8PathBuf::from("docs")]).unwrap();
        assert_eq!(
            format_files(root, &from_docs).unwrap(),
            [Utf8PathBuf::from("docs/a.md")]
        );
    }

    #[test]
    fn a_directory_name_with_glob_characters_is_taken_literally() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        git(root, &["init", "-q"]);
        std::fs::create_dir_all(root.join("a[1]")).unwrap();
        std::fs::create_dir_all(root.join("a1")).unwrap();
        std::fs::write(root.join("a[1]/x.md"), "|a|b|\n|-|-|\n").unwrap();
        std::fs::write(root.join("a1/y.md"), "|a|b|\n|-|-|\n").unwrap();
        assert_eq!(
            expand_directories(root, vec![Utf8PathBuf::from("a[1]")]).unwrap(),
            [Utf8PathBuf::from("a[1]/x.md")]
        );
    }

    fn git(root: &Utf8Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn formats_what_a_turn_committed_into_its_newest_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "t"]);
        git(root, &["config", "user.email", "t@t"]);
        std::fs::write(root.join("a.txt"), "1").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", "init"]);
        let before = git(root, &["rev-parse", "HEAD"]);

        std::fs::write(root.join("a.md"), "|a|b|\n|-|-|\n|1|2|\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", "docs: table"]);
        let after = git(root, &["rev-parse", "HEAD"]);

        assert!(format_committed(root, Some(&before), Some(&after)));
        assert_ne!(git(root, &["rev-parse", "HEAD"]), after);
        assert_eq!(git(root, &["log", "-1", "--format=%s"]), "docs: table");
        assert!(git(root, &["status", "--short"]).is_empty());
        let head = git(root, &["rev-parse", "HEAD"]);
        assert!(!format_committed(root, Some(&before), Some(&head)));
    }

    #[test]
    fn formats_the_first_commit_of_a_repository_without_commits() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(tmp.path()).unwrap();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "t"]);
        git(root, &["config", "user.email", "t@t"]);
        std::fs::write(root.join("a.md"), "|a|b|\n|-|-|\n|1|2|\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", "docs: table"]);
        let after = git(root, &["rev-parse", "HEAD"]);

        assert!(format_committed(root, None, Some(&after)));
        assert_ne!(git(root, &["rev-parse", "HEAD"]), after);
        assert_eq!(git(root, &["log", "-1", "--format=%s"]), "docs: table");
        assert_eq!(git(root, &["rev-list", "--count", "HEAD"]), "1");
        assert_eq!(
            std::fs::read_to_string(root.join("a.md")).unwrap(),
            "| a | b |\n| - | - |\n| 1 | 2 |\n"
        );
        assert!(git(root, &["status", "--short"]).is_empty());
    }
}
