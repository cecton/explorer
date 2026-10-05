pub mod agent_loop;
pub mod claude_code;
pub mod context;
pub mod llm;
pub mod models;
pub mod tools;
pub mod turn;

use serde::{Deserialize, Serialize};
use std::fmt;

/// How a prompt is sent: to the teacher, or to the executor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// The agent explains and advises; the repository is left untouched.
    #[default]
    Feedback,
    /// The agent carries out an instruction and commits the result.
    Commit,
}

impl Mode {
    pub fn toggled(self) -> Self {
        match self {
            Mode::Feedback => Mode::Commit,
            Mode::Commit => Mode::Feedback,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mode::Feedback => "feedback",
            Mode::Commit => "commit",
        }
    }
}

/// Reasoning effort offered for every model. `Default` sends no effort parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effort {
    Default,
    Low,
    Medium,
    High,
}

impl Effort {
    pub const ALL: [Effort; 4] = [Effort::Default, Effort::Low, Effort::Medium, Effort::High];

    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Default => "default",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|effort| effort.as_str() == value)
    }
}

/// `endpoint/model/effort`. The model id may itself contain `/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSpec {
    pub endpoint: String,
    pub model: String,
    pub effort: Effort,
}

impl ModelSpec {
    pub fn parse(text: &str) -> Option<Self> {
        let (endpoint, rest) = text.split_once('/')?;
        let (model, effort) = rest.rsplit_once('/')?;
        Self::from_parts(endpoint, model, effort)
    }

    pub fn from_parts(endpoint: &str, model: &str, effort: &str) -> Option<Self> {
        if endpoint.is_empty() || model.is_empty() {
            return None;
        }
        Some(Self {
            endpoint: endpoint.to_string(),
            model: model.to_string(),
            effort: Effort::parse(effort)?,
        })
    }
}

impl fmt::Display for ModelSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}/{}",
            self.endpoint,
            self.model,
            self.effort.as_str()
        )
    }
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
    /// The local `claude` CLI, which runs its own loop and tools with its own login.
    ClaudeCode { models: Vec<String> },
}

impl EndpointKind {
    /// The `kind` recorded in a node's metadata.
    pub fn label(&self) -> &'static str {
        match self {
            EndpointKind::OpenAi { .. } => "openai",
            EndpointKind::ClaudeCode { .. } => CLAUDE_CODE_KIND,
        }
    }
}

pub const CLAUDE_CODE_KIND: &str = "claude-code";

/// A model ready to run: where it lives and how to reach it.
#[derive(Clone, Debug)]
pub enum Backend {
    OpenAi(llm::LlmTarget),
    ClaudeCode(claude_code::ClaudeTarget),
}

/// The `agent` block of `config.kdl`, joined with the tokens of `auth.kdl`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentConfig {
    /// Only endpoints that have a token; the others were skipped with a warning.
    pub endpoints: Vec<Endpoint>,
    /// Model used for titles and summaries; neither is generated without it.
    pub summary_model: Option<ModelSpec>,
}

impl AgentConfig {
    pub fn endpoint(&self, name: &str) -> Option<&Endpoint> {
        self.endpoints.iter().find(|endpoint| endpoint.name == name)
    }

    /// The backend `spec` designates, if its endpoint is usable.
    pub fn backend(&self, spec: &ModelSpec) -> Option<Backend> {
        Some(match &self.endpoint(&spec.endpoint)?.kind {
            EndpointKind::OpenAi { url, token } => Backend::OpenAi(llm::LlmTarget {
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
}

/// One message of a transcript, in our own form so that `transcript.jsonl` does not
/// depend on any provider or library.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_spec_parses_ids_containing_slashes() {
        let spec = ModelSpec::parse("openrouter/anthropic/claude-opus/high").unwrap();
        assert_eq!(spec.endpoint, "openrouter");
        assert_eq!(spec.model, "anthropic/claude-opus");
        assert_eq!(spec.effort, Effort::High);
        assert_eq!(spec.to_string(), "openrouter/anthropic/claude-opus/high");
    }

    #[test]
    fn model_spec_rejects_missing_parts_and_unknown_efforts() {
        assert!(ModelSpec::parse("kimi/kimi-k2").is_none());
        assert!(ModelSpec::parse("kimi/kimi-k2/extreme").is_none());
        assert!(ModelSpec::parse("/kimi-k2/low").is_none());
        assert!(ModelSpec::parse("kimi//low").is_none());
        assert!(ModelSpec::parse("nonsense").is_none());
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
