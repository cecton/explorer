use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A name standing for a shell command; what is typed after it is appended as is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Alias {
    pub name: String,
    pub command: String,
}

pub struct Config {
    /// Fallback theme used only when a project has no session (or no session-saved
    /// theme) yet. Theme selection normally lives in the session; this can only be
    /// changed by hand-editing the config file, not from the UI.
    pub theme: Option<String>,
    /// `(action-slug, key-spec)` pairs from the `keybindings` block, applied on top of the
    /// default keymap by [`crate::keymap::Keymap::apply_overrides`].
    pub keybindings: Vec<(String, String)>,
    /// The `agent { … }` block.
    pub agent: AgentSettings,
    /// The `after-agents { … }` block.
    pub after_agents: AfterAgentsConfig,
    /// The `alias <name> "<command>"` lines.
    pub aliases: Vec<Alias>,
}

/// Settings for the `after-agents` command line command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AfterAgentsConfig {
    /// Seconds any input can still cancel the command after the agents end; 0 runs it at once.
    pub grace_seconds: u64,
}

pub const DEFAULT_GRACE_SECONDS: u64 = 5;

impl Default for AfterAgentsConfig {
    fn default() -> Self {
        Self {
            grace_seconds: DEFAULT_GRACE_SECONDS,
        }
    }
}

/// The `agent` block as written in `config.kdl`: endpoints still without their tokens.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AgentSettings {
    pub endpoints: Vec<EndpointDecl>,
    pub summary_model: Option<crate::agent::ModelSpec>,
    pub subagent_model: Option<crate::agent::ModelSpec>,
    pub review_rounds: Option<u32>,
    pub repeat_limit: Option<u32>,
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
    KimiCode { name: String, url: String },
    Cline { name: String, url: String },
    OpenRouter { name: String, url: String },
    ClaudeCode { name: String, models: Vec<String> },
}

/// Models offered for a `claude-code` endpoint that lists none.
const CLAUDE_CODE_DEFAULT_MODELS: [&str; 3] = ["opus", "sonnet", "haiku"];

/// Where an `opencode-go` endpoint that gives no `url` is reached.
const OPENCODE_GO_URL: &str = "https://opencode.ai/zen/go/v1";

/// Where an `openrouter` endpoint that gives no `url` is reached.
const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1";

fn auth_path() -> Option<PathBuf> {
    Some(
        crate::util::fs::data_dir()?
            .join("auth.kdl")
            .into_std_path_buf(),
    )
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
pub fn load_commands() -> Vec<crate::agent::commands::Command> {
    crate::util::fs::config_dir()
        .map(|dir| read_commands(&dir.join("commands").into_std_path_buf()))
        .unwrap_or_default()
}

/// The commands of the `.md` files in `dir`, by name. A file that cannot make a
/// command is logged and skipped; a missing directory yields none.
fn read_commands(dir: &Path) -> Vec<crate::agent::commands::Command> {
    use crate::agent::commands::Command;
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
            if !crate::agent::commands::is_command_name(name) {
                tracing::warn!(
                    "ignoring {}: a command name is made of lowercase letters, digits and dashes",
                    path.display()
                );
                return None;
            }
            if crate::agent::commands::is_built_in(name) {
                tracing::warn!("ignoring {}: '{name}' is built in", path.display());
                return None;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| tracing::warn!("cannot read {}: {e}", path.display()))
                .ok()?;
            let command = crate::agent::commands::Command::parse(name, &text);
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
pub fn load_roles() -> Vec<crate::agent::subagent::Role> {
    crate::util::fs::config_dir()
        .map(|dir| read_roles(&dir.join("agents").into_std_path_buf()))
        .unwrap_or_default()
}

/// The roles of the `.md` files in `dir`, by name. A file that cannot make a role is
/// logged and skipped; a missing directory yields none.
fn read_roles(dir: &Path) -> Vec<crate::agent::subagent::Role> {
    use crate::agent::subagent::Role;
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
            if !crate::agent::commands::is_command_name(name) {
                tracing::warn!(
                    "ignoring {}: a role name is made of lowercase letters, digits and dashes",
                    path.display()
                );
                return None;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| tracing::warn!("cannot read {}: {e}", path.display()))
                .ok()?;
            crate::agent::subagent::Role::parse(name, &text)
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
    commands: Vec<crate::agent::commands::Command>,
    roles: Vec<crate::agent::subagent::Role>,
) -> crate::agent::AgentConfig {
    use crate::agent::AgentConfig;
    use crate::agent::Endpoint;
    use crate::agent::EndpointKind;
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
            EndpointDecl::KimiCode { name, url } => {
                let token = token_of(&name)?;
                Some(Endpoint {
                    name,
                    kind: EndpointKind::KimiCode { url, token },
                })
            }
            EndpointDecl::Cline { name, url } => {
                let token = token_of(&name)?;
                Some(Endpoint {
                    name,
                    kind: EndpointKind::Cline { url, token },
                })
            }
            EndpointDecl::OpenRouter { name, url } => {
                let token = token_of(&name)?;
                Some(Endpoint {
                    name,
                    kind: EndpointKind::OpenRouter { url, token },
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
        review_rounds: settings
            .review_rounds
            .unwrap_or(crate::agent::DEFAULT_REVIEW_ROUNDS),
        repeat_limit: settings
            .repeat_limit
            .unwrap_or(crate::agent::DEFAULT_REPEAT_LIMIT),
    }
}

fn string_arg(node: &kdl::KdlNode) -> Option<&str> {
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .and_then(|e| e.value().as_string())
}

fn parse_agent(node: &kdl::KdlNode) -> Result<AgentSettings, String> {
    use crate::agent::ModelSpec;
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
                    crate::agent::OPENCODE_GO_KIND => EndpointDecl::OpenCodeGo {
                        url: url.unwrap_or_else(|| OPENCODE_GO_URL.to_string()),
                        name,
                    },
                    crate::agent::KIMI_CODE_KIND => EndpointDecl::KimiCode {
                        url: url.ok_or_else(|| format!("endpoint '{name}' must have a url"))?,
                        name,
                    },
                    crate::agent::CLINE_KIND => EndpointDecl::Cline {
                        url: url.ok_or_else(|| format!("endpoint '{name}' must have a url"))?,
                        name,
                    },
                    crate::agent::OPENROUTER_KIND => EndpointDecl::OpenRouter {
                        url: url.unwrap_or_else(|| OPENROUTER_URL.to_string()),
                        name,
                    },
                    crate::agent::CLAUDE_CODE_KIND => {
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
            "repeat-limit" => {
                let limit = child
                    .entries()
                    .iter()
                    .find(|e| e.name().is_none())
                    .and_then(|e| e.value().as_integer())
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n != 1)
                    .ok_or(
                        "agent.repeat-limit must have an integer argument of 0 (off) or at least 2",
                    )?;
                settings.repeat_limit = Some(limit);
            }
            _ => {}
        }
    }
    Ok(settings)
}

fn config_path() -> Option<PathBuf> {
    Some(
        crate::util::fs::config_dir()?
            .join("config.kdl")
            .into_std_path_buf(),
    )
}

fn parse(content: &str) -> Result<Config, String> {
    let doc: kdl::KdlDocument = content.parse().map_err(|e: kdl::KdlError| e.to_string())?;

    let mut theme = None;
    let mut keybindings = Vec::new();
    let mut agent = AgentSettings::default();
    let mut after_agents = AfterAgentsConfig::default();
    let mut aliases: Vec<Alias> = Vec::new();

    for node in doc.nodes() {
        match node.name().value() {
            "alias" => {
                let mut arguments = node
                    .entries()
                    .iter()
                    .filter(|e| e.name().is_none())
                    .map(|e| e.value().as_string());
                let (Some(Some(name)), Some(Some(command)), None) =
                    (arguments.next(), arguments.next(), arguments.next())
                else {
                    return Err("alias must have a name and a command, both strings".to_string());
                };
                if name.is_empty() || name.contains(char::is_whitespace) {
                    return Err(format!("alias name '{name}' must be one word"));
                }
                if aliases.iter().any(|known| known.name == name) {
                    return Err(format!("alias '{name}' is defined twice"));
                }
                aliases.push(Alias {
                    name: name.to_string(),
                    command: command.to_string(),
                });
            }
            "after-agents" => {
                let Some(children) = node.children() else {
                    continue;
                };
                for child in children.nodes() {
                    if child.name().value() == "grace-seconds" {
                        after_agents.grace_seconds = child
                            .entries()
                            .iter()
                            .find(|e| e.name().is_none())
                            .and_then(|e| e.value().as_integer())
                            .and_then(|seconds| u64::try_from(seconds).ok())
                            .ok_or(
                                "after-agents.grace-seconds must have a non-negative integer argument",
                            )?;
                    }
                }
            }
            "agent" => agent = parse_agent(node)?,
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
        agent,
        after_agents,
        aliases,
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
        use crate::agent::Endpoint;
        use crate::agent::EndpointKind;
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
    fn kimi_code_endpoint_needs_a_url_and_a_token() {
        use crate::agent::Endpoint;
        use crate::agent::EndpointKind;
        let config = parse(
            r#"
            agent {
                endpoint "kimi" {
                    kind "kimi-code"
                    url "https://api.kimi.ai/coding/v1"
                }
            }
        "#,
        )
        .expect("valid config");
        assert_eq!(
            config.agent.endpoints,
            [EndpointDecl::KimiCode {
                name: "kimi".to_string(),
                url: "https://api.kimi.ai/coding/v1".to_string()
            }]
        );
        let tokens = parse_auth("kimi \"sk-kimi\"").expect("valid auth");
        let agent = resolve_agent(config.agent, &tokens, Vec::new(), Vec::new());
        assert_eq!(
            agent.endpoints,
            [Endpoint {
                name: "kimi".to_string(),
                kind: EndpointKind::KimiCode {
                    url: "https://api.kimi.ai/coding/v1".to_string(),
                    token: "sk-kimi".to_string()
                }
            }]
        );
        assert!(
            parse(
                r#"
            agent {
                endpoint "kimi" {
                    kind "kimi-code"
                }
            }
        "#,
            )
            .is_err()
        );
    }

    #[test]
    fn cline_endpoint_needs_a_url_and_a_token() {
        use crate::agent::Endpoint;
        use crate::agent::EndpointKind;
        let config = parse(
            r#"
            agent {
                endpoint "cline" {
                    kind "cline"
                    url "https://api.cline.bot/api/v1"
                }
            }
        "#,
        )
        .expect("valid config");
        assert_eq!(
            config.agent.endpoints,
            [EndpointDecl::Cline {
                name: "cline".to_string(),
                url: "https://api.cline.bot/api/v1".to_string()
            }]
        );
        let tokens = parse_auth("cline \"sk_4d98\"").expect("valid auth");
        let agent = resolve_agent(config.agent, &tokens, Vec::new(), Vec::new());
        assert_eq!(
            agent.endpoints,
            [Endpoint {
                name: "cline".to_string(),
                kind: EndpointKind::Cline {
                    url: "https://api.cline.bot/api/v1".to_string(),
                    token: "sk_4d98".to_string()
                }
            }]
        );
        assert!(
            parse(
                r#"
            agent {
                endpoint "cline" {
                    kind "cline"
                }
            }
        "#,
            )
            .is_err()
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
    fn repeat_limit_defaults_to_five_and_is_zero_or_at_least_two() {
        let tokens = HashMap::new();
        let resolve = |content: &str| {
            let config = parse(content).expect("valid config");
            resolve_agent(config.agent, &tokens, Vec::new(), Vec::new()).repeat_limit
        };
        assert_eq!(resolve("theme \"night_owl\""), 5);
        assert_eq!(resolve("agent {\n repeat-limit 3\n}"), 3);
        assert_eq!(resolve("agent {\n repeat-limit 0\n}"), 0);
        assert!(parse("agent {\n repeat-limit 1\n}").is_err());
        assert!(parse("agent {\n repeat-limit -2\n}").is_err());
        assert!(parse("agent {\n repeat-limit \"5\"\n}").is_err());
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
        use crate::agent::EndpointKind;
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
            repeat_limit: None,
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

    #[test]
    fn after_agents_grace_defaults_to_five_seconds_and_must_be_an_integer() {
        assert_eq!(
            parse("").unwrap().after_agents.grace_seconds,
            DEFAULT_GRACE_SECONDS
        );
        let config = parse("after-agents { grace-seconds 0 }").unwrap();
        assert_eq!(config.after_agents.grace_seconds, 0);
        assert!(parse("after-agents { grace-seconds \"x\" }").is_err());
        assert!(parse("after-agents { grace-seconds -1 }").is_err());
    }

    #[test]
    fn aliases_are_read_in_order() {
        let config =
            parse("alias poweroff \"systemctl poweroff\"\nalias notify \"notify-send %project\"\n")
                .unwrap();
        let alias = |name: &str, command: &str| Alias {
            name: name.to_string(),
            command: command.to_string(),
        };
        assert_eq!(
            config.aliases,
            vec![
                alias("poweroff", "systemctl poweroff"),
                alias("notify", "notify-send %project"),
            ]
        );
        assert!(parse("").unwrap().aliases.is_empty());
    }

    #[test]
    fn aliases_reject_bad_declarations() {
        assert!(parse("alias a \"x\"\nalias a \"y\"").is_err());
        assert!(parse("alias \"\" \"x\"").is_err());
        assert!(parse("alias \"a b\" \"x\"").is_err());
        assert!(parse("alias a").is_err());
        assert!(parse("alias a 3").is_err());
    }
}
