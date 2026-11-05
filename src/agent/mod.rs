pub mod agent_loop;
pub mod claude_code;
pub mod commands;
pub mod context;
pub mod llm;
pub mod mcp;
pub mod models;
pub mod subagent;
pub mod tools;
pub mod turn;

/// How a turn runs: for the teacher, or for the executor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The agent explains and advises; what it changes is discarded.
    Feedback,
    /// The agent carries out an instruction and commits the result.
    Commit,
}

/// A place to run models, as declared in `config.kdl`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub name: String,
    pub kind: EndpointKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndpointKind {
    /// An OpenAI-compatible server, driven by our own agent loop.
    OpenAi {
        /// Base URL to which `chat/completions` and `models` are appended.
        url: String,
        token: String,
    },
    /// An OpenCode Go subscription: an OpenAI-compatible server that also reports how
    /// much of the plan is used.
    OpenCodeGo { url: String, token: String },
    /// A Kimi Code membership: an OpenAI-compatible server that also reports its
    /// quota windows at `<base>/usages`.
    KimiCode { url: String, token: String },
    /// A Cline membership: an OpenAI-compatible server that also reports how much of
    /// its Cline Pass is used at `<base>/users/me/plan/usage-limits`. Its plan models
    /// are listed at `<base>/ai/cline/recommended-models`, not `<base>/models`.
    Cline { url: String, token: String },
    /// OpenRouter: an OpenAI-compatible gateway for many providers, which also reports
    /// what the key has cost and the limit it enforces, at `<base>/key`.
    OpenRouter { url: String, token: String },
    /// The local `claude` CLI, which runs its own loop and tools with its own login.
    ClaudeCode { models: Vec<String> },
}

impl EndpointKind {
    /// The `kind` recorded in a node's metadata.
    pub fn label(&self) -> &'static str {
        match self {
            EndpointKind::OpenAi { .. } => "openai",
            EndpointKind::OpenCodeGo { .. } => crate::config::OPENCODE_GO_KIND,
            EndpointKind::KimiCode { .. } => crate::config::KIMI_CODE_KIND,
            EndpointKind::Cline { .. } => crate::config::CLINE_KIND,
            EndpointKind::OpenRouter { .. } => crate::config::OPENROUTER_KIND,
            EndpointKind::ClaudeCode { .. } => crate::config::CLAUDE_CODE_KIND,
        }
    }

    /// The name the status bar gives to the spending behind this endpoint; `None` when
    /// it reports no usage.
    pub fn usage_label(&self) -> Option<&'static str> {
        match self {
            EndpointKind::OpenAi { .. } => None,
            EndpointKind::OpenCodeGo { .. } => Some("OpenCode"),
            EndpointKind::KimiCode { .. } => Some("Kimi"),
            EndpointKind::Cline { .. } => Some("Cline"),
            EndpointKind::OpenRouter { .. } => Some("OpenRouter"),
            EndpointKind::ClaudeCode { .. } => Some("Claude"),
        }
    }
}

/// A model ready to run: where it lives and how to reach it.
#[derive(Clone, Debug)]
pub enum Backend {
    OpenAi(llm::LlmTarget),
    ClaudeCode(claude_code::ClaudeTarget),
}

/// Review rounds of a commit when the `agent` block sets none.
pub const DEFAULT_REVIEW_ROUNDS: u32 = 3;

/// Times a tool call may return the same result before the run is stuck, by default.
pub const DEFAULT_REPEAT_LIMIT: u32 = 5;

/// The `agent` block of `config.kdl`, joined with the tokens of `auth.kdl`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentConfig {
    /// Only endpoints that have a token; the others were skipped with a warning.
    pub endpoints: Vec<Endpoint>,
    /// Model used for titles and summaries; neither is generated without it.
    pub summary_model: Option<crate::model_spec::ModelSpec>,
    /// Custom slash commands.
    pub commands: Vec<commands::Command>,
    /// Model of the sub-agents whose role names none; without it they run on the model
    /// of the turn that calls them.
    pub subagent_model: Option<crate::model_spec::ModelSpec>,
    /// Sub-agent roles the `agent` tool offers.
    pub roles: Vec<subagent::Role>,
    /// Most review rounds a commit turn runs on each of its commits; at least 1.
    pub review_rounds: u32,
    /// Times one tool call may return the same result before a nudge; 0 is off.
    pub repeat_limit: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            endpoints: Vec::new(),
            summary_model: None,
            commands: Vec::new(),
            subagent_model: None,
            roles: Vec::new(),
            review_rounds: DEFAULT_REVIEW_ROUNDS,
            repeat_limit: DEFAULT_REPEAT_LIMIT,
        }
    }
}

impl AgentConfig {
    pub fn endpoint(&self, name: &str) -> Option<&Endpoint> {
        self.endpoints.iter().find(|endpoint| endpoint.name == name)
    }

    /// The backend `spec` designates, if its endpoint is usable.
    pub fn backend(&self, spec: &crate::model_spec::ModelSpec) -> Option<Backend> {
        Some(match &self.endpoint(&spec.endpoint)?.kind {
            EndpointKind::OpenAi { url, token }
            | EndpointKind::OpenCodeGo { url, token }
            | EndpointKind::KimiCode { url, token }
            | EndpointKind::Cline { url, token }
            | EndpointKind::OpenRouter { url, token } => Backend::OpenAi(llm::LlmTarget {
                url: url.clone(),
                token: token.clone(),
                model: spec.model.clone(),
                effort: spec.effort,
            }),
            EndpointKind::ClaudeCode { .. } => Backend::ClaudeCode(claude_code::ClaudeTarget {
                model: spec.model.clone(),
                effort: spec.effort,
            }),
        })
    }

    /// Builds the configuration from what the config file and the config directory
    /// hold: an endpoint without a token is logged and skipped, so none of its models
    /// is ever listed.
    pub fn resolve(
        settings: crate::config::AgentSettings,
        tokens: &std::collections::HashMap<String, String>,
        commands: Vec<commands::Command>,
        roles: Vec<subagent::Role>,
    ) -> Self {
        use crate::agent::Endpoint;
        use crate::agent::EndpointKind;
        use crate::config::EndpointDecl;
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
        Self {
            endpoints,
            summary_model: settings.summary_model,
            commands,
            subagent_model: settings.subagent_model,
            roles,
            review_rounds: settings.review_rounds.unwrap_or(DEFAULT_REVIEW_ROUNDS),
            repeat_limit: settings.repeat_limit.unwrap_or(DEFAULT_REPEAT_LIMIT),
        }
    }
}

/// One message of a transcript, in our own form so that `transcript.jsonl` does not
/// depend on any provider or library.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    User {
        content: String,
        /// Written by Explorer, not by the user; never shown in a Discussion window.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        injected: bool,
    },
    Assistant {
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        call_id: String,
        name: String,
        content: String,
    },
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(content: &str) -> AgentConfig {
        let settings = crate::config::parse(content).expect("valid config").agent;
        AgentConfig::resolve(
            settings,
            &std::collections::HashMap::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    #[test]
    fn opencode_go_endpoint_has_a_default_url_and_needs_a_token() {
        use crate::config::EndpointDecl;
        let settings = crate::config::parse(
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
        .expect("valid config")
        .agent;
        assert_eq!(
            settings.endpoints,
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
        let tokens = crate::config::parse_auth("go \"sk-go\"").expect("valid auth");
        let agent = AgentConfig::resolve(settings, &tokens, Vec::new(), Vec::new());
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
        let settings = crate::config::parse(
            r#"
            agent {
                endpoint "kimi" {
                    kind "kimi-code"
                    url "https://api.kimi.ai/coding/v1"
                }
            }
        "#,
        )
        .expect("valid config")
        .agent;
        let tokens = crate::config::parse_auth("kimi \"sk-kimi\"").expect("valid auth");
        let agent = AgentConfig::resolve(settings, &tokens, Vec::new(), Vec::new());
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
            crate::config::parse(
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
        let settings = crate::config::parse(
            r#"
            agent {
                endpoint "cline" {
                    kind "cline"
                    url "https://api.cline.bot/api/v1"
                }
            }
        "#,
        )
        .expect("valid config")
        .agent;
        let tokens = crate::config::parse_auth("cline \"sk_4d98\"").expect("valid auth");
        let agent = AgentConfig::resolve(settings, &tokens, Vec::new(), Vec::new());
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
            crate::config::parse(
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
    fn review_rounds_default_to_three_and_must_be_at_least_one() {
        assert_eq!(resolve("theme \"night_owl\"").review_rounds, 3);
        assert_eq!(resolve("agent {\n review-rounds 5\n}").review_rounds, 5);
        assert!(crate::config::parse("agent {\n review-rounds 0\n}").is_err());
        assert!(crate::config::parse("agent {\n review-rounds -2\n}").is_err());
        assert!(crate::config::parse("agent {\n review-rounds \"3\"\n}").is_err());
    }

    #[test]
    fn repeat_limit_defaults_to_five_and_is_zero_or_at_least_two() {
        assert_eq!(resolve("theme \"night_owl\"").repeat_limit, 5);
        assert_eq!(resolve("agent {\n repeat-limit 3\n}").repeat_limit, 3);
        assert_eq!(resolve("agent {\n repeat-limit 0\n}").repeat_limit, 0);
        assert!(crate::config::parse("agent {\n repeat-limit 1\n}").is_err());
        assert!(crate::config::parse("agent {\n repeat-limit -2\n}").is_err());
        assert!(crate::config::parse("agent {\n repeat-limit \"5\"\n}").is_err());
    }

    #[test]
    fn endpoint_without_a_token_is_skipped() {
        use crate::config::AgentSettings;
        use crate::config::EndpointDecl;
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
        let tokens =
            crate::config::parse_auth("router \"sk-or\"\nunused \"x\"").expect("valid auth");
        let agent = AgentConfig::resolve(settings, &tokens, Vec::new(), Vec::new());
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
    fn a_summary_and_a_subagent_model_are_kept_as_written() {
        let agent = resolve(
            "agent {\n summary-model \"router/openai/gpt-mini/low\"\n subagent-model \"kimi/kimi-k2/high\"\n}",
        );
        assert_eq!(
            agent.summary_model.expect("summary model").to_string(),
            "router/openai/gpt-mini/low"
        );
        assert_eq!(
            agent.subagent_model.expect("sub-agent model").to_string(),
            "kimi/kimi-k2/high"
        );
    }

    #[test]
    fn transcript_messages_round_trip_as_json_lines() {
        let messages = vec![
            Message::User {
                content: "hi".into(),
                injected: false,
            },
            Message::User {
                content: "commit now".into(),
                injected: true,
            },
            Message::Assistant {
                content: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                }],
            },
            Message::Tool {
                call_id: "call_1".into(),
                name: "read".into(),
                content: "hello".into(),
            },
            Message::Assistant {
                content: "done".into(),
                tool_calls: Vec::new(),
            },
        ];
        for message in &messages {
            let line = serde_json::to_string(message).unwrap();
            assert!(!line.contains('\n'));
            assert_eq!(&serde_json::from_str::<Message>(&line).unwrap(), message);
        }
        assert_eq!(
            serde_json::to_string(&messages[4]).unwrap(),
            r#"{"role":"assistant","content":"done"}"#
        );
        assert_eq!(
            serde_json::to_string(&messages[0]).unwrap(),
            r#"{"role":"user","content":"hi"}"#
        );
        assert_eq!(
            serde_json::to_string(&messages[1]).unwrap(),
            r#"{"role":"user","content":"commit now","injected":true}"#
        );
    }
}
