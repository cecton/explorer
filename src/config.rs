use crate::agent::commands::{self, Command};
use crate::agent::subagent::Role;
use crate::agent::{
    AgentConfig, CLAUDE_CODE_KIND, DEFAULT_REVIEW_ROUNDS, Endpoint, EndpointKind, ModelSpec,
    OPENCODE_GO_KIND,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct Config {
    /// Fallback theme used only when a project has no session (or no session-saved
    /// theme) yet. Theme selection normally lives in the session; this can only be
    /// changed by hand-editing the config file, not from the UI.
    pub theme: Option<String>,
    /// `(action-slug, key-spec)` pairs from the `keybindings` block, applied on top of the
    /// default keymap by [`crate::keymap::Keymap::apply_overrides`].
    pub keybindings: Vec<(String, String)>,
    /// Settings for the status-bar time counters, from the `counters { … }` block.
    pub counters: CountersConfig,
    /// The `agent { … }` block.
    pub agent: AgentSettings,
}

/// The `agent` block as written in `config.kdl`: endpoints still without their tokens.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AgentSettings {
    pub endpoints: Vec<EndpointDecl>,
    pub summary_model: Option<ModelSpec>,
    pub subagent_model: Option<ModelSpec>,
    pub review_rounds: Option<u32>,
}

/// Settings for the status-bar session/project time counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CountersConfig {
    /// Whether to display the counters on the right of the status bar (`show`).
    pub show: bool,
    /// Whether terminal output ("stuff moving") counts as active time even without
    /// keyboard input (`render-counts-as-active`).
    pub render_counts_as_active: bool,
}

impl Default for CountersConfig {
    fn default() -> Self {
        Self {
            show: true,
            render_counts_as_active: true,
        }
    }
}

impl Config {
    pub fn load() -> Result<Option<Self>, String> {
        let path = config_path().ok_or("could not determine config directory")?;
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(format!(
                    "error reading config file '{}': {e}",
                    path.display()
                ));
            }
        };

        parse(&content).map(Some)
    }
}

/// An endpoint as declared, before its token is looked up.
#[derive(Debug, PartialEq, Eq)]
pub enum EndpointDecl {
    OpenAi { name: String, url: String },
    OpenCodeGo { name: String, url: String },
    ClaudeCode { name: String, models: Vec<String> },
}

/// Models offered for a `claude-code` endpoint that lists none.
const CLAUDE_CODE_DEFAULT_MODELS: [&str; 3] = ["opus", "sonnet", "haiku"];

/// Where an `opencode-go` endpoint that gives no `url` is reached.
const OPENCODE_GO_URL: &str = "https://opencode.ai/zen/go/v1";

fn auth_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "explorer").map(|dirs| dirs.data_dir().join("auth.kdl"))
}

/// Endpoint tokens from `<data_dir>/auth.kdl`, one `name "token"` node per endpoint.
/// A missing or unreadable file yields no token.
pub fn load_auth() -> HashMap<String, String> {
    let Some(path) = auth_path() else {
        return HashMap::new();
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => parse_auth(&content).unwrap_or_else(|e| {
            tracing::warn!("ignoring {}: {e}", path.display());
            HashMap::new()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => {
            tracing::warn!("cannot read {}: {e}", path.display());
            HashMap::new()
        }
    }
}

fn parse_auth(content: &str) -> Result<HashMap<String, String>, String> {
    let doc: kdl::KdlDocument = content.parse().map_err(|e: kdl::KdlError| e.to_string())?;
    let mut tokens = HashMap::new();
    for node in doc.nodes() {
        let name = node.name().value();
        let token = string_arg(node)
            .ok_or_else(|| format!("auth entry '{name}' must have a string token"))?;
        tokens.insert(name.to_string(), token.to_string());
    }
    Ok(tokens)
}

/// Custom slash commands, from `<config_dir>/commands/<name>.md`.
pub fn load_commands() -> Vec<Command> {
    directories::ProjectDirs::from("", "", "explorer")
        .map(|dirs| read_commands(&dirs.config_dir().join("commands")))
        .unwrap_or_default()
}

/// The commands of the `.md` files in `dir`, by name. A file that cannot make a
/// command is logged and skipped; a missing directory yields none.
fn read_commands(dir: &Path) -> Vec<Command> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<Command> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()? != "md" {
                return None;
            }
            let name = path.file_stem()?.to_str()?;
            if !commands::is_command_name(name) {
                tracing::warn!(
                    "ignoring {}: a command name is made of lowercase letters, digits and dashes",
                    path.display()
                );
                return None;
            }
            if commands::is_built_in(name) {
                tracing::warn!("ignoring {}: '{name}' is built in", path.display());
                return None;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| tracing::warn!("cannot read {}: {e}", path.display()))
                .ok()?;
            let command = Command::parse(name, &text);
            if command.is_none() {
                tracing::warn!("ignoring {}: it has no prompt", path.display());
            }
            command
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// Reads the sub-agent roles from `<config_dir>/agents/<name>.md`.
pub fn load_roles() -> Vec<Role> {
    directories::ProjectDirs::from("", "", "explorer")
        .map(|dirs| read_roles(&dirs.config_dir().join("agents")))
        .unwrap_or_default()
}

/// The roles of the `.md` files in `dir`, by name. A file that cannot make a role is
/// logged and skipped; a missing directory yields none.
fn read_roles(dir: &Path) -> Vec<Role> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<Role> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()? != "md" {
                return None;
            }
            let name = path.file_stem()?.to_str()?;
            if !commands::is_command_name(name) {
                tracing::warn!(
                    "ignoring {}: a role name is made of lowercase letters, digits and dashes",
                    path.display()
                );
                return None;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| tracing::warn!("cannot read {}: {e}", path.display()))
                .ok()?;
            Role::parse(name, &text)
                .map_err(|reason| tracing::warn!("ignoring {}: {reason}", path.display()))
                .ok()
        })
        .collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// Joins the declared endpoints with their tokens. An endpoint without a token is
/// logged and left out, so none of its models is ever listed.
pub fn resolve_agent(
    settings: AgentSettings,
    tokens: &HashMap<String, String>,
    commands: Vec<Command>,
    roles: Vec<Role>,
) -> AgentConfig {
    let token_of = |name: &str| {
        let token = tokens.get(name).cloned();
        if token.is_none() {
            tracing::warn!("endpoint '{name}' has no token in auth.kdl; skipping it");
        }
        token
    };
    let endpoints = settings
        .endpoints
        .into_iter()
        .filter_map(|decl| match decl {
            EndpointDecl::ClaudeCode { name, models } => Some(Endpoint {
                name,
                kind: EndpointKind::ClaudeCode { models },
            }),
            EndpointDecl::OpenAi { name, url } => {
                let token = token_of(&name)?;
                Some(Endpoint {
                    name,
                    kind: EndpointKind::OpenAi { url, token },
                })
            }
            EndpointDecl::OpenCodeGo { name, url } => {
                let token = token_of(&name)?;
                Some(Endpoint {
                    name,
                    kind: EndpointKind::OpenCodeGo { url, token },
                })
            }
        })
        .collect();
    AgentConfig {
        endpoints,
        summary_model: settings.summary_model,
        commands,
        subagent_model: settings.subagent_model,
        roles,
        review_rounds: settings.review_rounds.unwrap_or(DEFAULT_REVIEW_ROUNDS),
    }
}

fn string_arg(node: &kdl::KdlNode) -> Option<&str> {
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .and_then(|e| e.value().as_string())
}

fn parse_agent(node: &kdl::KdlNode) -> Result<AgentSettings, String> {
    let mut settings = AgentSettings::default();
    let Some(children) = node.children() else {
        return Ok(settings);
    };
    for child in children.nodes() {
        match child.name().value() {
            "endpoint" => {
                let name = string_arg(child).ok_or("agent.endpoint must have a string name")?;
                if name.is_empty() || name.contains('/') {
                    return Err(format!(
                        "endpoint name '{name}' must be non-empty and must not contain '/'"
                    ));
                }
                let mut url = None;
                let mut kind = "openai";
                let mut models: Vec<String> = Vec::new();
                for field in child.children().map_or(&[][..], |c| c.nodes()) {
                    let values: Vec<&str> = field
                        .entries()
                        .iter()
                        .filter(|e| e.name().is_none())
                        .filter_map(|e| e.value().as_string())
                        .collect();
                    let value = values.first().copied().ok_or_else(|| {
                        format!(
                            "endpoint '{name}': {} must have a string argument",
                            field.name().value()
                        )
                    })?;
                    match field.name().value() {
                        "url" => url = Some(value.to_string()),
                        "kind" => kind = value,
                        "models" => models = values.iter().map(|v| v.to_string()).collect(),
                        _ => {}
                    }
                }
                let name = name.to_string();
                settings.endpoints.push(match kind {
                    "openai" => EndpointDecl::OpenAi {
                        url: url.ok_or_else(|| format!("endpoint '{name}' must have a url"))?,
                        name,
                    },
                    OPENCODE_GO_KIND => EndpointDecl::OpenCodeGo {
                        url: url.unwrap_or_else(|| OPENCODE_GO_URL.to_string()),
                        name,
                    },
                    CLAUDE_CODE_KIND => {
                        if models.is_empty() {
                            models = CLAUDE_CODE_DEFAULT_MODELS.map(str::to_string).to_vec();
                        }
                        EndpointDecl::ClaudeCode { name, models }
                    }
                    other => return Err(format!("endpoint '{name}': unknown kind '{other}'")),
                });
            }
            "summary-model" => {
                let value =
                    string_arg(child).ok_or("agent.summary-model must have a string argument")?;
                let spec = ModelSpec::parse(value).ok_or_else(|| {
                    format!("summary-model '{value}' is not of the form endpoint/model/effort")
                })?;
                settings.summary_model = Some(spec);
            }
            "subagent-model" => {
                let value =
                    string_arg(child).ok_or("agent.subagent-model must have a string argument")?;
                let spec = ModelSpec::parse(value).ok_or_else(|| {
                    format!("subagent-model '{value}' is not of the form endpoint/model/effort")
                })?;
                settings.subagent_model = Some(spec);
            }
            "review-rounds" => {
                let rounds = child
                    .entries()
                    .iter()
                    .find(|e| e.name().is_none())
                    .and_then(|e| e.value().as_integer())
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n >= 1)
                    .ok_or("agent.review-rounds must have an integer argument of at least 1")?;
                settings.review_rounds = Some(rounds);
            }
            _ => {}
        }
    }
    Ok(settings)
}

fn config_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "explorer").map(|dirs| {
        let mut path = dirs.config_dir().to_path_buf();
        path.push("config.kdl");
        path
    })
}

fn parse(content: &str) -> Result<Config, String> {
    let doc: kdl::KdlDocument = content.parse().map_err(|e: kdl::KdlError| e.to_string())?;

    let mut theme = None;
    let mut keybindings = Vec::new();
    let mut counters = CountersConfig::default();
    let mut agent = AgentSettings::default();

    for node in doc.nodes() {
        match node.name().value() {
            "agent" => agent = parse_agent(node)?,
            "counters" => {
                let Some(children) = node.children() else {
                    continue;
                };
                for child in children.nodes() {
                    let name = child.name().value();
                    let val = child
                        .entries()
                        .iter()
                        .find(|e| e.name().is_none())
                        .and_then(|e| e.value().as_bool())
                        .ok_or_else(|| format!("counters.{name} must have a boolean argument"))?;
                    match name {
                        "show" => counters.show = val,
                        "render-counts-as-active" => counters.render_counts_as_active = val,
                        _ => {}
                    }
                }
            }
            "theme" => {
                if let Some(arg) = node.entries().iter().find(|e| e.name().is_none()) {
                    if let Some(val) = arg.value().as_string() {
                        theme = Some(val.to_string());
                    } else {
                        return Err("theme node argument must be a string".to_string());
                    }
                } else {
                    return Err("theme node must have a string argument".to_string());
                }
            }
            "keybindings" => {
                let Some(children) = node.children() else {
                    continue;
                };
                for bind in children.nodes() {
                    let slug = bind.name().value().to_string();
                    let arg = bind
                        .entries()
                        .iter()
                        .find(|e| e.name().is_none())
                        .and_then(|e| e.value().as_string())
                        .ok_or_else(|| {
                            format!("keybinding '{slug}' must have a string key argument")
                        })?;
                    keybindings.push((slug, arg.to_string()));
                }
            }
            _ => {}
        }
    }

    Ok(Config {
        theme,
        keybindings,
        counters,
        agent,
    })
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn config_path_uses_project_dirs() {
        let path = config_path().expect("config_path should return Some");
        let file_name = path.file_name().expect("path has file name");
        assert_eq!(file_name, "config.kdl");
        assert!(path.to_string_lossy().contains("explorer"));
    }

    #[test]
    fn parses_theme_and_keybindings_block() {
        let content = r#"
            theme "night_owl"
            keybindings {
                quit "Q"
                open-terminal "t"
                resize-grow "ctrl+down"
                leader "alt+`"
            }
        "#;
        let config = parse(content).expect("valid config");
        assert_eq!(config.theme.as_deref(), Some("night_owl"));
        assert_eq!(
            config.keybindings,
            vec![
                ("quit".to_string(), "Q".to_string()),
                ("open-terminal".to_string(), "t".to_string()),
                ("resize-grow".to_string(), "ctrl+down".to_string()),
                ("leader".to_string(), "alt+`".to_string()),
            ]
        );
    }

    #[test]
    fn counters_default_when_block_absent() {
        let config = parse("theme \"night_owl\"").expect("valid config");
        assert_eq!(config.counters, CountersConfig::default());
        assert!(config.counters.show);
        assert!(config.counters.render_counts_as_active);
    }

    #[test]
    fn counters_block_toggles_both_options() {
        let config = parse("counters {\n    show #false\n    render-counts-as-active #false\n}")
            .expect("valid config");
        assert!(!config.counters.show);
        assert!(!config.counters.render_counts_as_active);
    }

    #[test]
    fn counters_block_partial_keeps_other_default() {
        // Only `show` set -> render-counts-as-active stays at its default (true).
        let config = parse("counters {\n    show #false\n}").expect("valid config");
        assert!(!config.counters.show);
        assert!(config.counters.render_counts_as_active);
    }

    #[test]
    fn counters_option_requires_a_boolean_argument() {
        assert!(parse("counters {\n    show \"yes\"\n}").is_err());
        assert!(parse("counters {\n    show\n}").is_err());
    }

    #[test]
    fn counters_ignores_unknown_child_nodes() {
        let config = parse("counters {\n    show #false\n    wat #true\n}").expect("valid config");
        assert!(!config.counters.show);
    }

    #[test]
    fn keybindings_are_empty_when_absent() {
        let config = parse("theme \"night_owl\"").expect("valid config");
        assert!(config.keybindings.is_empty());
    }

    #[test]
    fn agent_block_parses_endpoints_and_summary_model() {
        let config = parse(
            r#"
            agent {
                endpoint "kimi" {
                    url "https://api.moonshot.ai/v1"
                }
                endpoint "router" {
                    kind "openai"
                    url "https://openrouter.ai/api/v1"
                }
                summary-model "router/openai/gpt-mini/low"
                subagent-model "kimi/kimi-k2/high"
            }
        "#,
        )
        .expect("valid config");
        assert_eq!(
            config.agent.endpoints,
            [
                EndpointDecl::OpenAi {
                    name: "kimi".to_string(),
                    url: "https://api.moonshot.ai/v1".to_string()
                },
                EndpointDecl::OpenAi {
                    name: "router".to_string(),
                    url: "https://openrouter.ai/api/v1".to_string()
                },
            ]
        );
        let summary = config.agent.summary_model.expect("summary model");
        assert_eq!(summary.to_string(), "router/openai/gpt-mini/low");
        let subagent = config.agent.subagent_model.expect("sub-agent model");
        assert_eq!(subagent.to_string(), "kimi/kimi-k2/high");
    }

    #[test]
    fn claude_code_endpoint_needs_no_url_and_has_default_models() {
        let config = parse(
            r#"
            agent {
                endpoint "claude" {
                    kind "claude-code"
                }
                endpoint "max" {
                    kind "claude-code"
                    models "opus" "fable"
                }
            }
        "#,
        )
        .expect("valid config");
        assert_eq!(
            config.agent.endpoints,
            [
                EndpointDecl::ClaudeCode {
                    name: "claude".to_string(),
                    models: vec!["opus".into(), "sonnet".into(), "haiku".into()],
                },
                EndpointDecl::ClaudeCode {
                    name: "max".to_string(),
                    models: vec!["opus".into(), "fable".into()],
                },
            ]
        );
    }

    #[test]
    fn opencode_go_endpoint_has_a_default_url_and_needs_a_token() {
        let config = parse(
            r#"
            agent {
                endpoint "go" {
                    kind "opencode-go"
                }
                endpoint "mirror" {
                    kind "opencode-go"
                    url "https://mirror/v1"
                }
            }
        "#,
        )
        .expect("valid config");
        assert_eq!(
            config.agent.endpoints,
            [
                EndpointDecl::OpenCodeGo {
                    name: "go".to_string(),
                    url: "https://opencode.ai/zen/go/v1".to_string()
                },
                EndpointDecl::OpenCodeGo {
                    name: "mirror".to_string(),
                    url: "https://mirror/v1".to_string()
                },
            ]
        );
        let tokens = parse_auth("go \"sk-go\"").expect("valid auth");
        let agent = resolve_agent(config.agent, &tokens, Vec::new(), Vec::new());
        assert_eq!(
            agent.endpoints,
            [Endpoint {
                name: "go".to_string(),
                kind: EndpointKind::OpenCodeGo {
                    url: "https://opencode.ai/zen/go/v1".to_string(),
                    token: "sk-go".to_string()
                }
            }]
        );
    }

    #[test]
    fn commands_are_read_from_markdown_files_named_after_them() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("review.md"), "Review the change below.\n").unwrap();
        std::fs::write(
            dir.join("plan-it.md"),
            "---\nsystem-prompt: off\nargument: topic\n---\nPlan this.\n",
        )
        .unwrap();
        std::fs::write(dir.join("Bad Name.md"), "x").unwrap();
        std::fs::write(dir.join("compact.md"), "x").unwrap();
        std::fs::write(dir.join("commit.md"), "x").unwrap();
        std::fs::write(dir.join("empty.md"), "---\nargument: x\n---\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();

        let commands = read_commands(dir);
        let names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["plan-it", "review"]);
        assert_eq!(commands[0].prompt, "Plan this.");
        assert!(!commands[0].system_prompt);
        assert_eq!(commands[0].argument.as_deref(), Some("topic"));
        assert_eq!(commands[1].prompt, "Review the change below.");
        assert!(commands[1].system_prompt);

        assert!(read_commands(&dir.join("missing")).is_empty());
    }

    #[test]
    fn agent_block_is_empty_when_absent() {
        let config = parse("theme \"night_owl\"").expect("valid config");
        assert_eq!(config.agent, AgentSettings::default());
    }

    #[test]
    fn agent_block_rejects_malformed_entries() {
        assert!(parse("agent {\n endpoint \"a\" {\n }\n}").is_err());
        assert!(parse("agent {\n endpoint \"a/b\" {\n url \"u\"\n }\n}").is_err());
        assert!(parse("agent {\n endpoint \"a\" {\n url \"u\"\n kind \"wat\"\n }\n}").is_err());
        assert!(parse("agent {\n summary-model \"nonsense\"\n}").is_err());
        assert!(parse("agent {\n subagent-model \"a//low\"\n}").is_err());
    }

    #[test]
    fn review_rounds_default_to_three_and_must_be_at_least_one() {
        let tokens = HashMap::new();
        let resolve = |content: &str| {
            let config = parse(content).expect("valid config");
            resolve_agent(config.agent, &tokens, Vec::new(), Vec::new()).review_rounds
        };
        assert_eq!(resolve("theme \"night_owl\""), 3);
        assert_eq!(resolve("agent {\n review-rounds 5\n}"), 5);
        assert!(parse("agent {\n review-rounds 0\n}").is_err());
        assert!(parse("agent {\n review-rounds -2\n}").is_err());
        assert!(parse("agent {\n review-rounds \"3\"\n}").is_err());
    }

    #[test]
    fn roles_are_read_from_markdown_files_and_need_a_description() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(
            dir.join("reviewer.md"),
            "---\ndescription: Reviews commits\nmodel: claude/opus/high\n---\nYou review.\n",
        )
        .unwrap();
        std::fs::write(dir.join("vague.md"), "You do things.\n").unwrap();
        std::fs::write(dir.join("Bad Name.md"), "---\ndescription: x\n---\ny\n").unwrap();

        let roles = read_roles(dir);
        assert_eq!(roles.len(), 1);
        assert_eq!(roles[0].name, "reviewer");
        assert_eq!(roles[0].description, "Reviews commits");
        assert_eq!(roles[0].prompt, "You review.");
        let model = roles[0].model.as_ref().expect("role model");
        assert_eq!(model.to_string(), "claude/opus/high");

        assert!(read_roles(&dir.join("missing")).is_empty());
    }

    #[test]
    fn endpoint_without_a_token_is_skipped() {
        let settings = AgentSettings {
            endpoints: vec![
                EndpointDecl::OpenAi {
                    name: "kimi".to_string(),
                    url: "https://k".to_string(),
                },
                EndpointDecl::OpenAi {
                    name: "router".to_string(),
                    url: "https://r".to_string(),
                },
                EndpointDecl::ClaudeCode {
                    name: "claude".to_string(),
                    models: vec!["opus".to_string()],
                },
            ],
            summary_model: None,
            subagent_model: None,
            review_rounds: None,
        };
        let tokens = parse_auth("router \"sk-or\"\nunused \"x\"").expect("valid auth");
        let agent = resolve_agent(settings, &tokens, Vec::new(), Vec::new());
        let names: Vec<&str> = agent.endpoints.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["router", "claude"]);
        assert_eq!(
            agent.endpoints[0].kind,
            EndpointKind::OpenAi {
                url: "https://r".to_string(),
                token: "sk-or".to_string()
            }
        );
        assert!(agent.endpoint("kimi").is_none());
    }

    #[test]
    fn auth_entry_without_a_token_is_an_error() {
        assert!(parse_auth("kimi").is_err());
        assert!(parse_auth("kimi 3").is_err());
    }

    #[test]
    fn keybinding_without_key_argument_is_an_error() {
        assert!(parse("keybindings {\n quit\n }").is_err());
    }
}
