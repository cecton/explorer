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

/// A model ready to run: where it lives and how to reach it.
#[derive(Clone, Debug)]
pub enum Backend {
    OpenAi(llm::LlmTarget),
    ClaudeCode(claude_code::ClaudeTarget),
}

impl Backend {
    /// One answer without tools and without a saved session: what the title and summary
    /// of a finished node are made of, which never reach the conversation tree.
    pub async fn complete(
        &self,
        session: &str,
        system: &str,
        user: &str,
    ) -> Result<String, String> {
        match self {
            Backend::OpenAi(target) => llm::complete(target, session, system, user).await,
            Backend::ClaudeCode(target) => claude_code::complete(target, system, user).await,
        }
    }
}

/// Review rounds of a commit when the `agent` block sets none.
pub const DEFAULT_REVIEW_ROUNDS: u32 = 3;

/// Times a tool call may return the same result before the run is stuck, by default.
pub const DEFAULT_REPEAT_LIMIT: u32 = 5;

/// The `agent` block of `config.kdl`, joined with the tokens of `auth.kdl`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentConfig {
    /// Only endpoints that have a token; the others were skipped with a warning.
    pub endpoints: Vec<crate::endpoint::Endpoint>,
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
    pub fn endpoint(&self, name: &str) -> Option<&crate::endpoint::Endpoint> {
        self.endpoints.iter().find(|endpoint| endpoint.name == name)
    }

    /// The backend `spec` designates, if its endpoint is usable.
    pub fn backend(&self, spec: &crate::model_spec::ModelSpec) -> Option<Backend> {
        use crate::endpoint::EndpointKind;
        Some(match &self.endpoint(&spec.endpoint)?.kind {
            EndpointKind::Http { url, token, .. } => Backend::OpenAi(llm::LlmTarget {
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
        let endpoints = settings
            .endpoints
            .into_iter()
            .filter_map(|endpoint| endpoint.resolved(tokens))
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

    /// The endpoint `name` reaches `url` on `provider`, as `config.kdl` declares it.
    fn declared(
        name: &str,
        url: &str,
        provider: crate::endpoint::Provider,
    ) -> crate::endpoint::Endpoint {
        use crate::endpoint::EndpointKind;
        crate::endpoint::Endpoint {
            name: name.to_string(),
            kind: EndpointKind::Http {
                url: url.to_string(),
                token: String::new(),
                provider,
            },
        }
    }

    /// The endpoint `name` reaches `url` with `token`, once its token is resolved.
    fn live(
        name: &str,
        url: &str,
        token: &str,
        provider: crate::endpoint::Provider,
    ) -> crate::endpoint::Endpoint {
        use crate::endpoint::EndpointKind;
        crate::endpoint::Endpoint {
            name: name.to_string(),
            kind: EndpointKind::Http {
                url: url.to_string(),
                token: token.to_string(),
                provider,
            },
        }
    }

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
        use crate::endpoint::Provider;
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
                declared("go", "https://opencode.ai/zen/go/v1", Provider::OpenCodeGo),
                declared("mirror", "https://mirror/v1", Provider::OpenCodeGo),
            ]
        );
        let tokens = crate::config::parse_auth("go \"sk-go\"").expect("valid auth");
        let agent = AgentConfig::resolve(settings, &tokens, Vec::new(), Vec::new());
        assert_eq!(
            agent.endpoints,
            [live(
                "go",
                "https://opencode.ai/zen/go/v1",
                "sk-go",
                Provider::OpenCodeGo
            )]
        );
    }

    #[test]
    fn kimi_code_endpoint_needs_a_url_and_a_token() {
        use crate::endpoint::Provider;
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
            [live(
                "kimi",
                "https://api.kimi.ai/coding/v1",
                "sk-kimi",
                Provider::KimiCode
            )]
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
        use crate::endpoint::Provider;
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
            [live(
                "cline",
                "https://api.cline.bot/api/v1",
                "sk_4d98",
                Provider::Cline
            )]
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
        use crate::endpoint::EndpointKind;
        use crate::endpoint::Provider;
        let settings = AgentSettings {
            endpoints: vec![
                declared("kimi", "https://k", Provider::OpenAi),
                declared("router", "https://r", Provider::OpenAi),
                crate::endpoint::Endpoint {
                    name: "claude".to_string(),
                    kind: EndpointKind::ClaudeCode {
                        models: vec!["opus".to_string()],
                    },
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
            agent.endpoints[0],
            live("router", "https://r", "sk-or", Provider::OpenAi)
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
