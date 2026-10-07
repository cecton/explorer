/// A custom slash command, read from `<config_dir>/commands/<name>.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    /// What the command sends, before the text typed after its name.
    pub prompt: String,
    /// False when the turn gets no mode prompt; it then keeps its work, like a commit
    /// turn, instead of being reset.
    pub system_prompt: bool,
    /// What the command expects after its name. Display only.
    pub argument: Option<String>,
    /// Display only.
    pub description: Option<String>,
}

impl Command {
    /// Reads a command file: an optional metadata block of `key: value` lines between
    /// two `---` lines, then the base prompt. `None` when the prompt is empty.
    pub fn parse(name: &str, text: &str) -> Option<Self> {
        let (metadata, prompt) = split_metadata(text);
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return None;
        }
        let mut command = Self {
            name: name.to_string(),
            prompt: prompt.to_string(),
            system_prompt: true,
            argument: None,
            description: None,
        };
        for line in metadata.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            match key.trim() {
                "system-prompt" => command.system_prompt = value != "off",
                "argument" => command.argument = Some(value.to_string()),
                "description" => command.description = Some(value.to_string()),
                _ => {}
            }
        }
        Some(command)
    }
}

/// Splits a command file into its metadata block and what follows it. The block opens
/// with `---` on the first line; without a closing `---` the file has no metadata.
fn split_metadata(text: &str) -> (&str, &str) {
    let mut offset = 0;
    let mut block_start = None;
    for line in text.split_inclusive('\n') {
        let is_rule = line.trim_end() == "---";
        match block_start {
            None if is_rule => block_start = Some(line.len()),
            None => break,
            Some(start) if is_rule => return (&text[start..offset], &text[offset + line.len()..]),
            Some(_) => {}
        }
        offset += line.len();
    }
    ("", text)
}

/// What a typed prompt becomes once a leading slash command is resolved.
#[derive(Debug, PartialEq, Eq)]
pub enum Expansion<'a> {
    /// A prompt to send. `command` is the custom command it was expanded from.
    Prompt {
        text: String,
        command: Option<&'a Command>,
    },
    /// The built-in `/compact`, with the instructions typed after it.
    Compact { instructions: String },
    /// The built-in `/commit`, with the additional instructions typed after it.
    Commit { instructions: String },
}

/// Names of the built-in commands, which a custom command cannot take.
pub const COMPACT: &str = "compact";
pub const COMMIT: &str = "commit";

/// Each built-in command with what it expects after its name and what it does.
const BUILT_INS: [(&str, &str, &str); 2] = [
    (
        COMMIT,
        "[additional instructions]",
        "Carry out what the conversation settled on and commit it",
    ),
    (
        COMPACT,
        "[instructions]",
        "Summarize the conversation into a node to continue from",
    ),
];

/// Whether `name` can follow the slash of a command: lowercase letters, digits and
/// dashes.
pub fn is_command_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub fn is_built_in(name: &str) -> bool {
    BUILT_INS.iter().any(|(built_in, ..)| *built_in == name)
}

/// Hint for a draft that is a command name with nothing typed after it: what the
/// command expects, continuing the draft, then its description on the next line.
pub fn placeholder(typed: &str, commands: &[Command]) -> Option<String> {
    let (name, rest) = split_command(typed)?;
    if !rest.is_empty() {
        return None;
    }
    let (argument, description) = match BUILT_INS.iter().find(|(built_in, ..)| *built_in == name) {
        Some((_, argument, description)) => (Some(argument.to_string()), Some(*description)),
        None => {
            let command = commands.iter().find(|command| command.name == name)?;
            (
                command
                    .argument
                    .as_ref()
                    .map(|argument| format!("<{argument}>")),
                command.description.as_deref(),
            )
        }
    };
    let mut hint = String::new();
    if let Some(argument) = argument {
        if !typed.ends_with(char::is_whitespace) {
            hint.push(' ');
        }
        hint.push_str(&argument);
    }
    if let Some(description) = description {
        hint.push('\n');
        hint.push_str(description);
    }
    (!hint.is_empty()).then_some(hint)
}

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
    let end = body.find(char::is_whitespace).unwrap_or(body.len());
    let (name, rest) = body.split_at(end);
    is_command_name(name).then_some((name, rest.trim()))
}

/// Resolves a leading slash command. A custom command becomes its base prompt, two
/// newlines, then the rest of the typed text; an unknown name leaves the text as typed,
/// so a prompt that begins with a path is harmless.
pub fn expand<'a>(typed: &str, commands: &'a [Command]) -> Expansion<'a> {
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
    let Some(command) = commands.iter().find(|command| command.name == name) else {
        return verbatim();
    };
    let text = if rest.is_empty() {
        command.prompt.clone()
    } else {
        format!("{}\n\n{rest}", command.prompt)
    };
    Expansion::Prompt {
        text,
        command: Some(command),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands() -> Vec<Command> {
        vec![Command {
            name: "review".to_string(),
            prompt: "Review the change below.".to_string(),
            system_prompt: true,
            argument: Some("what".to_string()),
            description: Some("Review a change".to_string()),
        }]
    }

    fn prompt<'a>(text: &str, command: Option<&'a Command>) -> Expansion<'a> {
        Expansion::Prompt {
            text: text.to_string(),
            command,
        }
    }

    #[test]
    fn custom_command_expands_to_base_prompt_then_arguments() {
        let commands = commands();
        assert_eq!(
            expand("/review  the parser\nplease", &commands),
            prompt(
                "Review the change below.\n\nthe parser\nplease",
                Some(&commands[0])
            )
        );
        assert_eq!(
            expand("/review", &commands),
            prompt("Review the change below.", Some(&commands[0]))
        );
    }

    #[test]
    fn command_file_has_an_optional_metadata_block() {
        let squash = Command::parse(
            "squash",
            "---\nsystem-prompt: off\nargument: commit-like\ndescription: Squash: since a ref\n\
             unknown: ignored\n---\nSquash all commits since:\n",
        )
        .unwrap();
        assert_eq!(
            squash,
            Command {
                name: "squash".to_string(),
                prompt: "Squash all commits since:".to_string(),
                system_prompt: false,
                argument: Some("commit-like".to_string()),
                description: Some("Squash: since a ref".to_string()),
            }
        );

        let plain = Command::parse("review", "Review it.\n\n---\n\nCarefully.\n").unwrap();
        assert_eq!(plain.prompt, "Review it.\n\n---\n\nCarefully.");
        assert!(plain.system_prompt);
        assert!(plain.argument.is_none() && plain.description.is_none());

        let on = Command::parse("a", "---\nsystem-prompt: on\n---\nDo it.").unwrap();
        assert!(on.system_prompt);
        let unclosed = Command::parse("a", "---\nsystem-prompt: off\nDo it.").unwrap();
        assert!(unclosed.system_prompt);
        assert_eq!(unclosed.prompt, "---\nsystem-prompt: off\nDo it.");

        assert!(Command::parse("a", "---\nargument: x\n---\n  \n").is_none());
        assert!(Command::parse("a", "").is_none());
    }

    #[test]
    fn placeholder_shows_while_nothing_follows_a_known_command() {
        let commands = commands();
        assert_eq!(
            placeholder("/review", &commands).as_deref(),
            Some(" <what>\nReview a change")
        );
        assert_eq!(
            placeholder("/review ", &commands).as_deref(),
            Some("<what>\nReview a change")
        );
        assert_eq!(
            placeholder("/commit", &commands).as_deref(),
            Some(
                " [additional instructions]\nCarry out what the conversation settled on and commit it"
            )
        );
        assert!(
            placeholder("/compact", &[])
                .unwrap()
                .starts_with(" [instructions]\n")
        );
        for typed in ["/review x", "/revie", "/unknown", "review", " /review", ""] {
            assert!(placeholder(typed, &commands).is_none(), "{typed}");
        }

        let bare = vec![Command {
            argument: None,
            ..commands[0].clone()
        }];
        assert_eq!(
            placeholder("/review", &bare).as_deref(),
            Some("\nReview a change")
        );
        let silent = vec![Command {
            argument: None,
            description: None,
            ..commands[0].clone()
        }];
        assert!(placeholder("/review", &silent).is_none());
    }

    #[test]
    fn unknown_names_and_paths_are_sent_as_typed() {
        let commands = commands();
        for typed in [
            "/unknown do it",
            "/home/user/file.rs is broken",
            "/Review caps",
            "/review-all",
            "/ review",
            " /review leading space",
            "no slash /review",
        ] {
            assert_eq!(expand(typed, &commands), prompt(typed, None), "{typed}");
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
