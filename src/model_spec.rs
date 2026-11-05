use std::fmt;

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

    pub fn parse(value: &str) -> Option<Self> {
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
    /// `endpoint/model/effort`, where the effort may also be left out: `endpoint/model`
    /// means the same model with a `default` effort. The model id may itself contain
    /// `/`, so a last segment that is no effort keyword stays part of it.
    pub fn parse(text: &str) -> Option<Self> {
        let (endpoint, rest) = text.split_once('/')?;
        if endpoint.is_empty() || rest.is_empty() || rest.starts_with('/') || rest.ends_with('/') {
            return None;
        }
        if let Some((model, effort)) = rest.rsplit_once('/')
            && let Some(spec) = Self::from_parts(endpoint, model, effort)
        {
            return Some(spec);
        }
        Some(Self::bare(endpoint, rest))
    }

    /// `endpoint/model` with no effort: the same as a `default` effort.
    pub fn bare(endpoint: &str, model: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            model: model.to_string(),
            effort: Effort::Default,
        }
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

impl ModelSpec {
    /// `endpoint/model (effort)`, the form shown in discussion windows; a default
    /// effort is not shown.
    pub fn short_label(&self) -> String {
        match self.effort {
            Effort::Default => format!("{}/{}", self.endpoint, self.model),
            effort => format!("{}/{} ({})", self.endpoint, self.model, effort.as_str()),
        }
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
    fn short_label_shows_the_effort_except_when_default() {
        let spec = ModelSpec::parse("zai/glm-5.3-flash/default").unwrap();
        assert_eq!(spec.short_label(), "zai/glm-5.3-flash");
        let spec = ModelSpec::parse("zai/glm-5.3-flash/low").unwrap();
        assert_eq!(spec.short_label(), "zai/glm-5.3-flash (low)");
    }

    #[test]
    fn model_spec_rejects_missing_parts() {
        assert!(ModelSpec::parse("kimi/").is_none());
        assert!(ModelSpec::parse("/kimi-k2/low").is_none());
        assert!(ModelSpec::parse("kimi//low").is_none());
        assert!(ModelSpec::parse("kimi//").is_none());
        assert!(ModelSpec::parse("nonsense").is_none());
    }

    #[test]
    fn an_effort_may_be_left_out_and_the_last_segment_is_not_always_one() {
        let spec = ModelSpec::parse("opencode-go/deepseek-v4.1-flash").unwrap();
        assert_eq!(spec.endpoint, "opencode-go");
        assert_eq!(spec.model, "deepseek-v4.1-flash");
        assert_eq!(spec.effort, Effort::Default);
        assert_eq!(spec.to_string(), "opencode-go/deepseek-v4.1-flash/default");

        // A trailing non-effort word stays part of the model id.
        let spec = ModelSpec::parse("kimi/kimi-k2/extreme").unwrap();
        assert_eq!(spec.model, "kimi-k2/extreme");
        assert_eq!(spec.effort, Effort::Default);

        // A real effort is still honoured.
        let spec = ModelSpec::parse("claude/opus/high").unwrap();
        assert_eq!(spec.model, "opus");
        assert_eq!(spec.effort, Effort::High);
    }
}
