/// What a typed prompt becomes once a leading slash command is resolved.
#[derive(Debug, PartialEq, Eq)]
pub enum Expansion {
    /// A prompt to send. `command` names the custom command it was expanded from.
    Prompt {
        text: String,
        command: Option<String>,
    },
    /// The built-in `/compact`, with the instructions typed after it.
    Compact { instructions: String },
    /// The built-in `/commit`, with the additional instructions typed after it.
    Commit { instructions: String },
}

/// Names of the built-in commands, which a custom command cannot take.
pub const COMPACT: &str = "compact";
pub const COMMIT: &str = "commit";

/// The prompt a `/commit` sends: a go-ahead, then the instructions typed after it.
pub fn commit_prompt(instructions: &str) -> String {
    let go_ahead = "OK, go ahead.";
    if instructions.is_empty() {
        go_ahead.to_string()
    } else {
        format!("{go_ahead}\n\nAdditional instructions:\n\n{instructions}")
    }
}

/// Splits `/name rest` when the prompt starts with a command name: lowercase letters,
/// digits and dashes, followed by whitespace or the end of the text.
fn split_command(typed: &str) -> Option<(&str, &str)> {
    let body = typed.strip_prefix('/')?;
    let end = body
        .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
        .unwrap_or(body.len());
    let (name, rest) = body.split_at(end);
    let followed_by_space = rest.chars().next().is_none_or(char::is_whitespace);
    (!name.is_empty() && followed_by_space).then_some((name, rest.trim()))
}

/// Resolves a leading slash command. A custom command becomes its base prompt, two
/// newlines, then the rest of the typed text; an unknown name leaves the text as typed,
/// so a prompt that begins with a path is harmless.
pub fn expand(typed: &str, commands: &[(String, String)]) -> Expansion {
    let verbatim = || Expansion::Prompt {
        text: typed.to_string(),
        command: None,
    };
    let Some((name, rest)) = split_command(typed) else {
        return verbatim();
    };
    if name == COMPACT {
        return Expansion::Compact {
            instructions: rest.to_string(),
        };
    }
    if name == COMMIT {
        return Expansion::Commit {
            instructions: rest.to_string(),
        };
    }
    let Some((_, base)) = commands.iter().find(|(command, _)| command == name) else {
        return verbatim();
    };
    let text = if rest.is_empty() {
        base.clone()
    } else {
        format!("{base}\n\n{rest}")
    };
    Expansion::Prompt {
        text,
        command: Some(name.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands() -> Vec<(String, String)> {
        vec![("review".to_string(), "Review the change below.".to_string())]
    }

    fn prompt(text: &str, command: Option<&str>) -> Expansion {
        Expansion::Prompt {
            text: text.to_string(),
            command: command.map(str::to_string),
        }
    }

    #[test]
    fn custom_command_expands_to_base_prompt_then_arguments() {
        assert_eq!(
            expand("/review  the parser\nplease", &commands()),
            prompt(
                "Review the change below.\n\nthe parser\nplease",
                Some("review")
            )
        );
        assert_eq!(
            expand("/review", &commands()),
            prompt("Review the change below.", Some("review"))
        );
    }

    #[test]
    fn unknown_names_and_paths_are_sent_as_typed() {
        for typed in [
            "/unknown do it",
            "/home/user/file.rs is broken",
            "/Review caps",
            "/review-all",
            "/ review",
            " /review leading space",
            "no slash /review",
        ] {
            assert_eq!(expand(typed, &commands()), prompt(typed, None), "{typed}");
        }
    }

    #[test]
    fn compact_is_built_in_and_keeps_its_instructions() {
        assert_eq!(
            expand("/compact", &commands()),
            Expansion::Compact {
                instructions: String::new()
            }
        );
        assert_eq!(
            expand("/compact keep the API decisions", &commands()),
            Expansion::Compact {
                instructions: "keep the API decisions".to_string()
            }
        );
        assert_eq!(
            expand("/compacted", &commands()),
            prompt("/compacted", None)
        );
    }

    #[test]
    fn commit_is_built_in_and_sends_a_go_ahead() {
        assert_eq!(
            expand("/commit", &commands()),
            Expansion::Commit {
                instructions: String::new()
            }
        );
        assert_eq!(
            expand("/commit  but keep the old name\n", &commands()),
            Expansion::Commit {
                instructions: "but keep the old name".to_string()
            }
        );
        assert_eq!(expand("/commits", &commands()), prompt("/commits", None));
        assert_eq!(commit_prompt(""), "OK, go ahead.");
        assert_eq!(
            commit_prompt("but keep the old name"),
            "OK, go ahead.\n\nAdditional instructions:\n\nbut keep the old name"
        );
    }
}
