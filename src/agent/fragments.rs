//! The user's own instructions for a turn's system prompt: the `.md` files of
//! `<config_dir>/prompts/` apply to both modes, and those of `<config_dir>/prompts/<mode>/`
//! to one. They are appended after the project's instructions, so the user's rules win on
//! a conflict. A file Explorer cannot read as a fragment is logged and skipped.

/// One fragment: its body, trimmed, ready to append.
pub struct Fragment {
    /// Its file stem, for the order and the log.
    pub name: String,
    pub text: String,
}

/// The fragments for `mode`, shared ones first. Read on every turn, so a file edited
/// between two turns is picked up by the next.
fn in_dir(dir: &camino::Utf8Path, mode: crate::agent::Mode) -> Vec<Fragment> {
    use crate::agent::Mode;
    let shared = dir.join("prompts");
    let mode_dir = match mode {
        Mode::Commit => shared.join("commit"),
        Mode::Feedback => shared.join("feedback"),
    };
    let mut fragments = read(&shared);
    fragments.extend(read(&mode_dir));
    fragments
}

fn read(dir: &camino::Utf8Path) -> Vec<Fragment> {
    crate::util::fs::read_markdown(
        dir.as_std_path(),
        "a prompt fragment",
        Fragment::parse,
        |fragment| &fragment.name,
    )
}

impl Fragment {
    /// The body after an optional `---` block: `enabled` is `true` unless it says
    /// otherwise, and only `true` or `false` is accepted, since a typo must not keep a
    /// fragment the user meant to switch off.
    fn parse(name: &str, text: &str) -> Result<Self, String> {
        use crate::util::text::{key_values, split_metadata};
        let (metadata, body) = split_metadata(text);
        for (key, value) in key_values(metadata) {
            if key != "enabled" {
                continue;
            }
            match value {
                "true" => {}
                "false" => return Err("it is disabled".to_string()),
                other => return Err(format!("enabled is true or false, not '{other}'")),
            }
        }
        let text = body.trim();
        if text.is_empty() {
            return Err("it has no text".to_string());
        }
        Ok(Self {
            name: name.to_string(),
            text: text.to_string(),
        })
    }
}

/// What every turn of `mode` appends to its system prompt: nothing when no fragment
/// applies, else the bodies under one heading, separated by blank lines.
pub fn block(mode: crate::agent::Mode) -> Option<String> {
    let dir = crate::util::fs::config_dir()?;
    block_in(&dir, mode)
}

fn block_in(dir: &camino::Utf8Path, mode: crate::agent::Mode) -> Option<String> {
    let fragments = in_dir(dir, mode);
    if fragments.is_empty() {
        return None;
    }
    let mut out = String::from("## User instructions\n\n");
    for (index, fragment) in fragments.iter().enumerate() {
        if index > 0 {
            out.push_str("\n\n");
        }
        out.push_str(&fragment.text);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_dir() -> (tempfile::TempDir, camino::Utf8PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        (tmp, dir)
    }

    fn write(dir: &camino::Utf8Path, path: &str, text: &str) {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_fragment_keeps_its_body_trimmed() {
        let fragment = Fragment::parse("rule", "\n\n  Be brief.\n\n").unwrap();
        assert_eq!(fragment.text, "Be brief.");
        let with_header =
            Fragment::parse("rule", "---\nenabled: true\n---\n\nBe brief.\n").unwrap();
        assert_eq!(with_header.text, "Be brief.");
    }

    #[test]
    fn enabled_is_true_or_false_and_nothing_else() {
        assert!(Fragment::parse("rule", "---\nenabled: false\n---\nBe brief.\n").is_err());
        assert!(Fragment::parse("rule", "---\nenabled: no\n---\nBe brief.\n").is_err());
        assert!(Fragment::parse("rule", "---\nenabled: true\n---\n").is_err());
        // Another key is left to the future and ignored.
        assert!(Fragment::parse("rule", "---\nwhen: commit\n---\nBe brief.\n").is_ok());
    }

    #[test]
    fn shared_fragments_come_first_then_the_mode_s_own() {
        use crate::agent::Mode;
        let (_tmp, dir) = config_dir();
        write(&dir, "prompts/10-shared.md", "Shared.");
        write(&dir, "prompts/feedback/10-mine.md", "Feedback only.");
        write(&dir, "prompts/commit/10-mine.md", "Commit only.");

        let block = block_in(&dir, Mode::Feedback).unwrap();
        assert!(
            block.starts_with("## User instructions\n\nShared.\n\nFeedback only."),
            "{block}"
        );
        let commit = block_in(&dir, Mode::Commit).unwrap();
        assert!(commit.ends_with("Commit only."), "{commit}");
        assert!(!commit.contains("Feedback only."));
    }

    #[test]
    fn a_disabled_a_bad_and_an_empty_fragment_are_skipped() {
        use crate::agent::Mode;
        let (_tmp, dir) = config_dir();
        write(&dir, "prompts/10-kept.md", "Kept.");
        write(&dir, "prompts/20-off.md", "---\nenabled: false\n---\nOff.");
        write(&dir, "prompts/30-typo.md", "---\nenabled: yes\n---\nTypo.");
        write(&dir, "prompts/40-empty.md", "---\nenabled: true\n---\n\n");
        write(&dir, "prompts/Bad Name.md", "Bad.");
        write(&dir, "prompts/notes.txt", "Not markdown.");

        assert_eq!(
            block_in(&dir, Mode::Commit).unwrap(),
            "## User instructions\n\nKept."
        );
    }

    #[test]
    fn a_missing_config_dir_has_no_fragments() {
        use crate::agent::Mode;
        let (_tmp, dir) = config_dir();
        assert!(block_in(&dir, Mode::Feedback).is_none());
    }
}
