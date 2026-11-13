use std::collections::HashMap;
use std::path::PathBuf;

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
    /// default keymap by [`crate::tui::keymap::Keymap::apply_overrides`].
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

/// Which Cargo features rust-analyzer should analyze. Set via the `features` /
/// `all-features` / `default-features` command line commands; changing it requires
/// restarting the rust-analyzer process since these are initialization-time-only
/// settings (rust-analyzer doesn't hot-reload `cargo.*` via `didChangeConfiguration`).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum CargoFeatureSelection {
    #[default]
    Default,
    All,
    Explicit {
        no_default_features: bool,
        features: Vec<String>,
    },
}

/// The `agent` block as written in `config.kdl`: endpoints still without their tokens.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AgentSettings {
    pub endpoints: Vec<crate::endpoint::Endpoint>,
    pub summary_model: Option<crate::model_spec::ModelSpec>,
    pub subagent_model: Option<crate::model_spec::ModelSpec>,
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

/// Models offered for a `claude-code` endpoint that lists none.
const CLAUDE_CODE_DEFAULT_MODELS: [&str; 3] = ["opus", "sonnet", "haiku"];

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

pub(crate) fn parse_auth(content: &str) -> Result<HashMap<String, String>, String> {
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

fn string_arg(node: &kdl::KdlNode) -> Option<&str> {
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .and_then(|e| e.value().as_string())
}

fn parse_agent(node: &kdl::KdlNode) -> Result<AgentSettings, String> {
    use crate::model_spec::ModelSpec;
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
                use crate::endpoint::EndpointKind;
                use crate::endpoint::Provider;
                let receiver = match Provider::from_label(kind) {
                    Some(provider) => {
                        let url = match (url, provider.default_url()) {
                            (Some(url), _) => url,
                            (None, Some(default)) => default.to_string(),
                            (None, None) => {
                                return Err(format!("endpoint '{name}' must have a url"));
                            }
                        };
                        EndpointKind::Http {
                            url,
                            token: String::new(),
                            provider,
                        }
                    }
                    None if kind == crate::endpoint::CLAUDE_CODE_KIND => {
                        if models.is_empty() {
                            models = CLAUDE_CODE_DEFAULT_MODELS.map(str::to_string).to_vec();
                        }
                        EndpointKind::ClaudeCode { models }
                    }
                    None => return Err(format!("endpoint '{name}': unknown kind '{kind}'")),
                };
                settings.endpoints.push(crate::endpoint::Endpoint {
                    name,
                    kind: receiver,
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

pub(crate) fn parse(content: &str) -> Result<Config, String> {
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
        use crate::endpoint::EndpointKind;
        use crate::endpoint::Provider;
        let declared = |name: &str, url: &str| crate::endpoint::Endpoint {
            name: name.to_string(),
            kind: EndpointKind::Http {
                url: url.to_string(),
                token: String::new(),
                provider: Provider::OpenAi,
            },
        };
        assert_eq!(
            config.agent.endpoints,
            [
                declared("kimi", "https://api.moonshot.ai/v1"),
                declared("router", "https://openrouter.ai/api/v1"),
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
        use crate::endpoint::EndpointKind;
        let claude = |name: &str, models: &[&str]| crate::endpoint::Endpoint {
            name: name.to_string(),
            kind: EndpointKind::ClaudeCode {
                models: models.iter().map(|model| model.to_string()).collect(),
            },
        };
        assert_eq!(
            config.agent.endpoints,
            [
                claude("claude", &["opus", "sonnet", "haiku"]),
                claude("max", &["opus", "fable"]),
            ]
        );
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
