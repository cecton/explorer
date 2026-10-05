use crate::agent::{AgentConfig, CLAUDE_CODE_KIND, Endpoint, EndpointKind, ModelSpec};
use std::collections::HashMap;
use std::path::PathBuf;

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
    ClaudeCode { name: String, models: Vec<String> },
}

/// Models offered for a `claude-code` endpoint that lists none.
const CLAUDE_CODE_DEFAULT_MODELS: [&str; 3] = ["opus", "sonnet", "haiku"];

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

/// Joins the declared endpoints with their tokens. An endpoint without a token is
/// logged and left out, so none of its models is ever listed.
pub fn resolve_agent(settings: AgentSettings, tokens: &HashMap<String, String>) -> AgentConfig {
    let endpoints = settings
        .endpoints
        .into_iter()
        .filter_map(|decl| match decl {
            EndpointDecl::ClaudeCode { name, models } => Some(Endpoint {
                name,
                kind: EndpointKind::ClaudeCode { models },
            }),
            EndpointDecl::OpenAi { name, url } => match tokens.get(&name) {
                Some(token) => Some(Endpoint {
                    name,
                    kind: EndpointKind::OpenAi {
                        url,
                        token: token.clone(),
                    },
                }),
                None => {
                    tracing::warn!("endpoint '{name}' has no token in auth.kdl; skipping it");
                    None
                }
            },
        })
        .collect();
    AgentConfig {
        endpoints,
        summary_model: settings.summary_model,
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
    fn agent_block_is_empty_when_absent() {
        let config = parse("theme \"night_owl\"").expect("valid config");
        assert_eq!(config.agent, AgentSettings::default());
    }

    #[test]
    fn agent_block_rejects_malformed_entries() {
        assert!(parse("agent {\n endpoint \"a\" {\n }\n}").is_err());
        assert!(parse("agent {\n endpoint \"a/b\" {\n url \"u\"\n }\n}").is_err());
        assert!(parse("agent {\n endpoint \"a\" {\n url \"u\"\n kind \"wat\"\n }\n}").is_err());
        assert!(parse("agent {\n summary-model \"a/b\"\n}").is_err());
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
        };
        let tokens = parse_auth("router \"sk-or\"\nunused \"x\"").expect("valid auth");
        let agent = resolve_agent(settings, &tokens);
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
