/// The kind of server behind an endpoint named in `config.kdl`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    /// An OpenAI-compatible server that reports no usage.
    OpenAi,
    /// An OpenCode Go subscription: an OpenAI-compatible server that also reports how
    /// much of the plan is used.
    OpenCodeGo,
    /// A Kimi Code membership: an OpenAI-compatible server that also reports its
    /// quota windows at `<base>/usages`.
    KimiCode,
    /// A Cline membership: an OpenAI-compatible server that also reports how much of
    /// its Cline Pass is used at `<base>/users/me/plan/usage-limits`. Its plan models
    /// are listed at `<base>/ai/cline/recommended-models`, not `<base>/models`.
    Cline,
    /// OpenRouter: an OpenAI-compatible gateway for many providers, which also reports
    /// what the key has cost and the limit it enforces, at `<base>/key`.
    OpenRouter,
}

impl Provider {
    /// The provider whose `kind` in `config.kdl` is `label`; `None` for the kinds that
    /// name no provider, which is the `claude` CLI alone.
    pub fn from_label(label: &str) -> Option<Self> {
        Some(match label {
            "openai" => Self::OpenAi,
            "opencode-go" => Self::OpenCodeGo,
            "kimi-code" => Self::KimiCode,
            "cline" => Self::Cline,
            "openrouter" => Self::OpenRouter,
            _ => return None,
        })
    }

    /// The `kind` written in `config.kdl` and recorded in a node's metadata.
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::OpenCodeGo => "opencode-go",
            Self::KimiCode => "kimi-code",
            Self::Cline => "cline",
            Self::OpenRouter => "openrouter",
        }
    }

    /// The name the status bar gives the spending behind this provider; `None` for one
    /// that reports no usage.
    pub fn short_label(self) -> Option<&'static str> {
        Some(match self {
            Self::OpenAi => return None,
            Self::OpenCodeGo => "OpenCode",
            Self::KimiCode => "Kimi",
            Self::Cline => "Cline",
            Self::OpenRouter => "OpenRouter",
        })
    }

    /// Where an endpoint of this provider that gives no `url` is reached; `None` when
    /// its `url` is required.
    pub fn default_url(self) -> Option<&'static str> {
        match self {
            Self::OpenCodeGo => Some("https://opencode.ai/zen/go/v1"),
            Self::OpenRouter => Some("https://openrouter.ai/api/v1"),
            Self::OpenAi | Self::KimiCode | Self::Cline => None,
        }
    }

    /// Where this provider lists its models, under the endpoint's base URL.
    pub fn models_path(self) -> &'static str {
        match self {
            Self::Cline => "ai/cline/recommended-models",
            _ => "models",
        }
    }
}

/// The `kind` recorded in a node's metadata for a `claude-code` endpoint.
pub const CLAUDE_CODE_KIND: &str = "claude-code";

/// A place to run models, as declared in `config.kdl`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub name: String,
    pub kind: EndpointKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndpointKind {
    /// A server Explorer reaches itself, driven by our own agent loop.
    Http {
        /// Base URL, to which the provider's paths are appended.
        url: String,
        /// The token `auth.kdl` gives this endpoint; empty until it is resolved.
        token: String,
        provider: Provider,
    },
    /// The local `claude` CLI, which runs its own loop and tools with its own login.
    ClaudeCode { models: Vec<String> },
}

impl EndpointKind {
    /// The `kind` recorded in a node's metadata.
    pub fn label(&self) -> &'static str {
        match self {
            EndpointKind::Http { provider, .. } => provider.label(),
            EndpointKind::ClaudeCode { .. } => CLAUDE_CODE_KIND,
        }
    }

    /// The name the status bar gives the spending behind this endpoint; `None` when it
    /// reports no usage.
    pub fn usage_label(&self) -> Option<&'static str> {
        match self {
            EndpointKind::Http { provider, .. } => provider.short_label(),
            EndpointKind::ClaudeCode { .. } => Some("Claude"),
        }
    }

    /// The provider behind this endpoint; `None` for the `claude` CLI, whose spending
    /// its own records hold.
    pub fn provider(&self) -> Option<Provider> {
        match self {
            EndpointKind::Http { provider, .. } => Some(*provider),
            EndpointKind::ClaudeCode { .. } => None,
        }
    }
}

impl Endpoint {
    /// This endpoint with the token `auth.kdl` gives it; `None` when it needs one and
    /// has none, which is logged so that none of its models is ever listed.
    pub fn resolved(mut self, tokens: &std::collections::HashMap<String, String>) -> Option<Self> {
        let EndpointKind::Http { token, .. } = &mut self.kind else {
            return Some(self);
        };
        let Some(found) = tokens.get(&self.name) else {
            tracing::warn!(
                "endpoint '{}' has no token in auth.kdl; skipping it",
                self.name
            );
            return None;
        };
        *token = found.clone();
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(provider: Provider) -> EndpointKind {
        EndpointKind::Http {
            url: String::new(),
            token: String::new(),
            provider,
        }
    }

    #[test]
    fn every_label_reads_back_as_its_provider() {
        for provider in [
            Provider::OpenAi,
            Provider::OpenCodeGo,
            Provider::KimiCode,
            Provider::Cline,
            Provider::OpenRouter,
        ] {
            assert_eq!(Provider::from_label(provider.label()), Some(provider));
        }
        assert_eq!(Provider::from_label(CLAUDE_CODE_KIND), None);
        assert_eq!(Provider::from_label("wat"), None);
    }

    #[test]
    fn only_a_plain_server_reports_no_usage_and_has_no_default_url() {
        assert_eq!(Provider::OpenAi.short_label(), None);
        assert_eq!(Provider::OpenAi.default_url(), None);
        assert_eq!(Provider::KimiCode.default_url(), None);
        assert_eq!(Provider::Cline.default_url(), None);
        assert_eq!(Provider::OpenCodeGo.short_label(), Some("OpenCode"));
        assert_eq!(Provider::KimiCode.short_label(), Some("Kimi"));
        assert_eq!(Provider::Cline.short_label(), Some("Cline"));
        assert_eq!(Provider::OpenRouter.short_label(), Some("OpenRouter"));
        assert_eq!(
            Provider::OpenCodeGo.default_url(),
            Some("https://opencode.ai/zen/go/v1")
        );
        assert_eq!(
            Provider::OpenRouter.default_url(),
            Some("https://openrouter.ai/api/v1")
        );
    }

    #[test]
    fn only_cline_lists_its_models_elsewhere() {
        assert_eq!(Provider::Cline.models_path(), "ai/cline/recommended-models");
        for provider in [
            Provider::OpenAi,
            Provider::OpenCodeGo,
            Provider::KimiCode,
            Provider::OpenRouter,
        ] {
            assert_eq!(provider.models_path(), "models");
        }
    }

    #[test]
    fn a_kind_names_what_it_records_and_what_it_spends() {
        assert_eq!(http(Provider::Cline).label(), "cline");
        assert_eq!(http(Provider::Cline).usage_label(), Some("Cline"));
        assert_eq!(http(Provider::OpenAi).usage_label(), None);
        let claude = EndpointKind::ClaudeCode {
            models: vec!["opus".to_string()],
        };
        assert_eq!(claude.label(), CLAUDE_CODE_KIND);
        assert_eq!(claude.usage_label(), Some("Claude"));
    }

    #[test]
    fn a_token_resolves_only_an_endpoint_that_needs_one() {
        let kimi = Endpoint {
            name: "kimi".to_string(),
            kind: http(Provider::KimiCode),
        };
        assert_eq!(kimi.clone().resolved(&Default::default()), None);
        let tokens = [("kimi".to_string(), "sk-kimi".to_string())].into();
        assert_eq!(
            kimi.resolved(&tokens),
            Some(Endpoint {
                name: "kimi".to_string(),
                kind: EndpointKind::Http {
                    url: String::new(),
                    token: "sk-kimi".to_string(),
                    provider: Provider::KimiCode,
                },
            })
        );
        let claude = Endpoint {
            name: "claude".to_string(),
            kind: EndpointKind::ClaudeCode {
                models: vec!["opus".to_string()],
            },
        };
        assert_eq!(claude.clone().resolved(&Default::default()), Some(claude));
    }
}
