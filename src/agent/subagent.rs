use std::time::{Duration, Instant};

/// Name of the tool that calls a sub-agent.
pub const TOOL: &str = "agent";

const DESCRIPTION: &str = "Delegate a task to a sub-agent: a fresh agent that sees \
nothing of this conversation and returns only its final answer. It works in the same \
repository with the same tools as you, so it can change files and run commands, but it \
cannot call sub-agents itself. Use it to keep your own context focused: a long \
investigation, a change and its verification, a review. It runs while you wait, one at a \
time.\n\n\
Its prompt is all it knows, so make it complete on its own: what to do, whether you \
expect it to change code or only to research, how to verify its work, whether to commit, \
and exactly what to report back. Its answer is not shown to the user; relay what \
matters. Every call starts a fresh sub-agent: it cannot be continued.";

/// A sub-agent role, read from `<config_dir>/agents/<name>.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Role {
    pub name: String,
    /// When to call it, as the `agent` tool tells the model.
    pub description: String,
    /// Model the role runs on, ahead of the `subagent-model` setting.
    pub model: Option<crate::model_spec::ModelSpec>,
    /// Wall-clock budget of one run, from the role's `budget-minutes`.
    pub budget: Option<Duration>,
    /// The role's system prompt.
    pub prompt: String,
}

impl Role {
    /// Reads a role file: a metadata block of `key: value` lines between two `---`
    /// lines, with a `description`, an optional `model` and `budget-minutes`, then the
    /// system prompt.
    pub fn parse(name: &str, text: &str) -> Result<Self, String> {
        use crate::model_spec::ModelSpec;
        let (metadata, prompt) = crate::util::text::split_metadata(text);
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err("it has no prompt".to_string());
        }
        let mut description = None;
        let mut model = None;
        let mut budget = None;
        for (key, value) in crate::util::text::key_values(metadata) {
            match key {
                "description" => description = Some(value.to_string()),
                "model" => {
                    model = Some(ModelSpec::parse(value).ok_or_else(|| {
                        format!("model '{value}' is not of the form endpoint/model/effort")
                    })?);
                }
                "budget-minutes" => {
                    let secs = value
                        .parse::<u64>()
                        .ok()
                        .filter(|minutes| *minutes >= 1)
                        .and_then(|minutes| minutes.checked_mul(60))
                        .ok_or_else(|| {
                            format!("budget-minutes '{value}' is not a positive whole number")
                        })?;
                    budget = Some(Duration::from_secs(secs));
                }
                _ => {}
            }
        }
        Ok(Self {
            name: name.to_string(),
            description: description.ok_or("it has no description")?,
            model,
            budget,
            prompt: prompt.to_string(),
        })
    }
}

/// The sub-agent roles, from `<config_dir>/agents/<name>.md`.
pub fn load_roles() -> Vec<Role> {
    crate::util::fs::config_dir()
        .map(|dir| read_roles(&dir.join("agents").into_std_path_buf()))
        .unwrap_or_default()
}

/// The roles of the `.md` files in `dir`, by name. A file that cannot make a role is
/// logged and skipped; a missing directory yields none.
fn read_roles(dir: &std::path::Path) -> Vec<Role> {
    crate::agent::commands::read_markdown(dir, "a role", Role::parse, |role| &role.name)
}

/// The `agent` tool as advertised to the model, with the roles it offers.
pub fn spec(roles: &[Role]) -> crate::agent::llm::ToolSpec {
    use crate::agent::llm::ToolSpec;
    let mut description = DESCRIPTION.to_string();
    let mut properties = serde_json::json!({
        "description": {"type": "string", "description": "A short title of the task, 3 to 5 words"},
        "prompt": {"type": "string", "description": "The task, complete on its own"}
    });
    if !roles.is_empty() {
        description.push_str(
            "\n\nRoles, to pass as `role`; without one, a general-purpose sub-agent runs:\n",
        );
        for role in roles {
            description.push_str(&format!("- {}: {}\n", role.name, role.description));
        }
        let names: Vec<&str> = roles.iter().map(|role| role.name.as_str()).collect();
        properties["role"] = serde_json::json!({
            "type": "string",
            "enum": names,
            "description": "Role of the sub-agent"
        });
    }
    ToolSpec {
        name: TOOL.to_string(),
        description,
        schema: serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": ["description", "prompt"]
        }),
    }
}

/// The work-log line of a call: `agent reviewer: Review the commits`.
pub fn describe(args: &serde_json::Value) -> String {
    let description = args
        .get("description")
        .and_then(serde_json::Value::as_str)
        .and_then(|text| text.lines().next())
        .unwrap_or("");
    match args.get("role").and_then(serde_json::Value::as_str) {
        Some(role) => format!("{TOOL} {role}: {description}"),
        None => format!("{TOOL}: {description}"),
    }
}

/// What a run needs to call sub-agents: nested loops of the run's own process.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Agents {
    pub tool: crate::agent::llm::ToolSpec,
    /// The roles that can run, by name.
    pub roles: Vec<(String, Delegate)>,
    /// What a call that names no role runs on.
    pub general: Delegate,
    /// `YYYY-MM-DD`, for the repository note.
    pub date: String,
    /// A feedback turn's sub-agents get the feedback note: they must not commit either.
    pub feedback: bool,
}

/// How one kind of sub-agent runs, resolved by the turn so the process needs no config.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Delegate {
    /// The role's prompt, or the default one.
    pub system: String,
    pub target: crate::agent::llm::LlmTarget,
    /// The model's `endpoint/model/effort`, named in the notes of a rerun.
    pub spec: String,
    /// The caller's model, to rerun on when this one fails; none when it is the caller's.
    pub fallback: Option<Fallback>,
    /// The role's time budget, when it sets one.
    pub budget_secs: Option<u64>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Fallback {
    pub target: crate::agent::llm::LlmTarget,
    pub spec: String,
}

/// The sub-agents a turn on `caller` may call; a role that cannot run here is left out.
pub fn agents(
    config: &crate::agent::AgentConfig,
    caller: &crate::model_spec::ModelSpec,
    caller_target: &crate::agent::llm::LlmTarget,
    date: &str,
    mode: crate::agent::Mode,
) -> Agents {
    use crate::agent::llm::LlmTarget;
    use crate::agent::{Backend, Mode};
    use crate::model_spec::ModelSpec;
    let openai = |spec: &ModelSpec| match config.backend(spec) {
        Some(Backend::OpenAi(target)) => Some(target),
        _ => None,
    };
    // The sub-agent model when it can run here, else the caller's model.
    let default = match &config.subagent_model {
        Some(spec) => match openai(spec) {
            Some(target) => (spec.clone(), target),
            None => {
                tracing::warn!(
                    "sub-agent model {spec} is not an OpenAI-compatible endpoint with a token"
                );
                (caller.clone(), caller_target.clone())
            }
        },
        None => (caller.clone(), caller_target.clone()),
    };
    let delegate = |role: Option<&Role>, (spec, target): (ModelSpec, LlmTarget)| Delegate {
        system: role
            .map_or(crate::agent::context::SUBAGENT_PROMPT, |role| &role.prompt)
            .to_string(),
        target,
        spec: spec.to_string(),
        fallback: (spec != *caller).then(|| Fallback {
            target: caller_target.clone(),
            spec: caller.to_string(),
        }),
        budget_secs: role
            .and_then(|role| role.budget)
            .map(|budget| budget.as_secs()),
    };
    let general = delegate(None, default.clone());
    let mut offered = Vec::new();
    let mut roles = Vec::new();
    for role in &config.roles {
        // A role's own model that cannot run takes the role out of the tool.
        let model = match &role.model {
            Some(spec) => match openai(spec) {
                Some(target) => (spec.clone(), target),
                None => {
                    tracing::warn!(
                        "sub-agent model {spec} is not an OpenAI-compatible endpoint with a token"
                    );
                    continue;
                }
            },
            None => default.clone(),
        };
        offered.push(role.clone());
        roles.push((role.name.clone(), delegate(Some(role), model)));
    }
    Agents {
        tool: spec(&offered),
        roles,
        general,
        date: date.to_string(),
        feedback: mode == Mode::Feedback,
    }
}

/// The sub-agents of one run, called one at a time.
pub struct Subagents {
    pub agents: Agents,
    pub context: std::sync::Arc<crate::agent::tools::ToolContext>,
    /// The session the run's sub-agents extend: each call's requests carry it with `-agent-<n>`.
    pub session: String,
    pub repeat_limit: u32,
    runs: u32,
}

/// The loop's sink for a sub-agent: nothing is recorded, only the final answer kept.
#[derive(Default)]
struct Quiet {
    answer: String,
}

impl crate::agent::sink::Sink for Quiet {
    fn injected(
        &mut self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<crate::agent::Message>> + Send + '_>>
    {
        Box::pin(std::future::ready(Vec::new()))
    }

    fn settle(&mut self) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + '_>> {
        Box::pin(std::future::ready(false))
    }

    fn message(&mut self, message: &crate::agent::Message) {
        use crate::agent::Message;
        if let Message::Assistant {
            content,
            tool_calls,
        } = message
            && tool_calls.is_empty()
        {
            self.answer.clone_from(content);
        }
    }

    fn begin_answer(&mut self) {}

    fn chunk(&mut self, _is_thinking: bool, _text: &str) {}

    fn flush_partial_answer(&mut self) {}

    fn answer_calls_tools(&mut self) {}

    fn request_size(&mut self, _tokens_in: Option<u64>, _cached_in: Option<u64>) {}

    fn tool_call(&mut self, _description: String) {}

    fn activity(&mut self, _activity: Option<String>) {}

    fn run_agent<'a>(
        &'a mut self,
        _call: &'a crate::agent::ToolCall,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = String> + Send + 'a>> {
        Box::pin(std::future::ready(
            "error: a sub-agent may not start sub-agents".to_string(),
        ))
    }
}

impl Subagents {
    pub fn new(
        agents: Agents,
        context: std::sync::Arc<crate::agent::tools::ToolContext>,
        session: String,
        repeat_limit: u32,
    ) -> Self {
        Self {
            agents,
            context,
            session,
            repeat_limit,
            runs: 0,
        }
    }

    /// Runs one call of the `agent` tool: the final answer, or a failure as text.
    pub async fn run(&mut self, call: &crate::agent::ToolCall) -> String {
        let prompt = match crate::agent::tools::str_arg(&call.arguments, "prompt") {
            Ok(prompt) => prompt.to_string(),
            Err(error) => return format!("error: {error}"),
        };
        let delegate = match call
            .arguments
            .get("role")
            .and_then(serde_json::Value::as_str)
        {
            Some(name) => match self.agents.roles.iter().find(|(role, _)| role == name) {
                Some((_, delegate)) => delegate.clone(),
                None => return format!("error: unknown role: {name}"),
            },
            None => self.agents.general.clone(),
        };
        self.runs += 1;
        let session = format!("{}-agent-{}", self.session, self.runs);
        let message = {
            let root = self.context.root.clone();
            let date = self.agents.date.clone();
            let feedback = self.agents.feedback;
            tokio::task::spawn_blocking(move || {
                use crate::agent::context::Notes;
                let head = gix::ThreadSafeRepository::open(root.as_std_path())
                    .ok()
                    .as_ref()
                    .and_then(crate::git::head_id);
                Notes {
                    root: &root,
                    head: head.as_deref(),
                    date: &date,
                    compacted_parent: None,
                    parent_head: None,
                    feedback,
                }
                .message(&prompt)
            })
            .await
        };
        let message = match message {
            Ok(message) => message,
            Err(error) => return format!("error: {error}"),
        };
        let budget = delegate.budget_secs.map(Duration::from_secs);
        let started = Instant::now();
        let mut result = self
            .execute(
                &delegate.system,
                budget,
                &delegate.target,
                &session,
                &message,
            )
            .await;
        let mut note = String::new();
        let mut secs = started.elapsed().as_secs();
        if let (Err(error), Some(fallback)) = (&result, &delegate.fallback) {
            let error = error.clone();
            let started = Instant::now();
            result = self
                .execute(
                    &delegate.system,
                    budget,
                    &fallback.target,
                    &session,
                    &message,
                )
                .await;
            secs = started.elapsed().as_secs();
            match &mut result {
                Ok(_) => {
                    note = format!(
                        "[Note from Explorer: the first run on {} failed: {error}. This answer \
                         comes from a rerun on {}.]\n\n",
                        delegate.spec, fallback.spec
                    );
                }
                Err(retry_error) => {
                    *retry_error = format!(
                        "{error}; the rerun on {} failed: {retry_error}",
                        fallback.spec
                    );
                }
            }
        }
        let over_budget = match budget {
            Some(budget) if secs >= budget.as_secs() => format!(
                "[Note from Explorer: the sub-agent hit its {}m time budget; its answer \
                 may be partial.]\n\n",
                budget.as_secs() / 60
            ),
            _ => String::new(),
        };
        match result {
            Err(error) => format!("{over_budget}error: the sub-agent failed: {error}"),
            Ok(answer) if answer.trim().is_empty() => {
                format!("{over_budget}{note}(the sub-agent gave no answer)")
            }
            Ok(answer) => format!("{over_budget}{note}{}", answer.trim()),
        }
    }

    /// One nested run of the loop on `target`; its final answer.
    async fn execute(
        &self,
        system: &str,
        budget: Option<Duration>,
        target: &crate::agent::llm::LlmTarget,
        session: &str,
        message: &str,
    ) -> Result<String, String> {
        use crate::agent::Message;
        use crate::agent::agent_loop::Limits;
        let system = match budget {
            Some(budget) => format!(
                "{}\n\n{}",
                system.trim_end(),
                crate::agent::context::budget_note(budget)
            ),
            None => system.to_string(),
        };
        let system = {
            let root = self.context.root.clone();
            tokio::task::spawn_blocking(move || {
                crate::agent::context::subagent_system_prompt(&system, &root)
            })
            .await
            .map_err(|e| e.to_string())?
        };
        let mut messages = vec![Message::User {
            content: message.to_string(),
            injected: false,
        }];
        let mut sink = Quiet::default();
        let limits = Limits {
            budget: budget.map(|budget| (Instant::now(), budget)),
            repeat_limit: self.repeat_limit,
        };
        crate::agent::agent_loop::run(
            &self.context,
            None,
            target,
            session,
            &system,
            &mut messages,
            &mut sink,
            limits,
        )
        .await?;
        Ok(sink.answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_role_needs_a_description_and_a_prompt() {
        let role = Role::parse(
            "reviewer",
            "---\ndescription: Reviews commits\nmodel: claude/opus/high\n---\nYou review.\n",
        )
        .unwrap();
        assert_eq!(role.description, "Reviews commits");
        assert_eq!(role.model.unwrap().to_string(), "claude/opus/high");
        assert_eq!(role.prompt, "You review.");

        assert!(Role::parse("x", "You review.\n").is_err());
        assert!(Role::parse("x", "---\ndescription: d\n---\n").is_err());
        assert!(Role::parse("x", "---\ndescription: d\nmodel: opus\n---\nP\n").is_err());
    }

    #[test]
    fn a_role_can_set_a_time_budget() {
        let role = Role::parse("r", "---\ndescription: d\nbudget-minutes: 15\n---\nP\n").unwrap();
        assert_eq!(role.budget, Some(Duration::from_secs(900)));

        let none = Role::parse("r", "---\ndescription: d\n---\nP\n").unwrap();
        assert_eq!(none.budget, None);
        assert!(Role::parse("r", "---\ndescription: d\nbudget-minutes: 0\n---\nP\n").is_err());
        assert!(Role::parse("r", "---\ndescription: d\nbudget-minutes: x\n---\nP\n").is_err());
        assert!(
            Role::parse(
                "r",
                "---\ndescription: d\nbudget-minutes: 9999999999999999999\n---\nP\n"
            )
            .is_err()
        );
    }

    #[test]
    fn the_spec_lists_the_roles_and_offers_a_role_only_when_there_are_some() {
        let bare = spec(&[]);
        assert!(bare.schema["properties"].get("role").is_none());
        assert!(!bare.description.contains("Roles"));

        let reviewer = Role::parse("reviewer", "---\ndescription: Reviews\n---\nP\n").unwrap();
        let with_role = spec(&[reviewer]);
        assert_eq!(
            with_role.schema["properties"]["role"]["enum"],
            serde_json::json!(["reviewer"])
        );
        assert!(with_role.description.ends_with("- reviewer: Reviews\n"));
    }

    #[test]
    fn a_call_is_described_by_its_role_and_title() {
        let call = serde_json::json!({"role": "reviewer", "description": "Review it\nsecond line"});
        assert_eq!(describe(&call), "agent reviewer: Review it");
        assert_eq!(
            describe(&serde_json::json!({"description": "Fix"})),
            "agent: Fix"
        );
    }

    fn endpoint(name: &str, kind: crate::endpoint::EndpointKind) -> crate::endpoint::Endpoint {
        crate::endpoint::Endpoint {
            name: name.to_string(),
            kind,
        }
    }

    fn http(url: &str) -> crate::endpoint::EndpointKind {
        use crate::endpoint::{EndpointKind, Provider};
        EndpointKind::Http {
            url: url.to_string(),
            token: "sk".to_string(),
            provider: Provider::OpenAi,
        }
    }

    #[test]
    fn a_role_that_cannot_run_in_an_agent_process_is_left_out() {
        use crate::agent::AgentConfig;
        use crate::agent::Mode;
        use crate::agent::llm::LlmTarget;
        use crate::endpoint::EndpointKind;
        use crate::model_spec::{Effort, ModelSpec};
        let config = AgentConfig {
            endpoints: vec![
                endpoint("fake", http("http://fake")),
                endpoint("other", http("http://other")),
                endpoint("claude", EndpointKind::ClaudeCode { models: vec![] }),
            ],
            roles: ["fast", "smart", "lost"]
                .iter()
                .zip(["other/m/low", "claude/opus/high", "gone/m/low"])
                .map(|(name, model)| {
                    Role::parse(
                        name,
                        &format!("---\ndescription: d\nmodel: {model}\n---\nP\n"),
                    )
                    .unwrap()
                })
                .collect(),
            ..AgentConfig::default()
        };
        let caller = ModelSpec::parse("fake/m/low").unwrap();
        let target = LlmTarget {
            url: "http://fake".to_string(),
            token: "sk".to_string(),
            model: "m".to_string(),
            effort: Effort::Low,
        };
        let agents = agents(&config, &caller, &target, "2026-11-23", Mode::Feedback);
        let names: Vec<&str> = agents.roles.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["fast"]);
        let fast = &agents.roles[0].1;
        assert_eq!(fast.target.url, "http://other");
        assert_eq!(fast.fallback.as_ref().unwrap().spec, "fake/m/low");
        // A call without a role runs on the caller's model, which has nothing to fall back on.
        assert_eq!(agents.general.target.url, "http://fake");
        assert!(agents.general.fallback.is_none());
        assert!(agents.feedback);
        let spec = &agents.tool.schema["properties"]["role"]["enum"];
        assert_eq!(*spec, serde_json::json!(["fast"]));
    }

    #[test]
    fn a_role_without_a_model_runs_on_the_caller_when_the_subagent_model_cannot() {
        use crate::agent::AgentConfig;
        use crate::agent::Mode;
        use crate::agent::llm::LlmTarget;
        use crate::endpoint::EndpointKind;
        use crate::model_spec::{Effort, ModelSpec};
        let config = AgentConfig {
            endpoints: vec![
                endpoint("fake", http("http://fake")),
                endpoint("claude", EndpointKind::ClaudeCode { models: vec![] }),
            ],
            subagent_model: Some(ModelSpec::parse("claude/opus/high").unwrap()),
            roles: vec![Role::parse("plain", "---\ndescription: d\n---\nP\n").unwrap()],
            ..AgentConfig::default()
        };
        let caller = ModelSpec::parse("fake/m/low").unwrap();
        let target = LlmTarget {
            url: "http://fake".to_string(),
            token: "sk".to_string(),
            model: "m".to_string(),
            effort: Effort::Low,
        };
        let agents = agents(&config, &caller, &target, "2026-11-23", Mode::Commit);
        let names: Vec<&str> = agents.roles.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["plain"]);
        let plain = &agents.roles[0].1;
        assert_eq!(plain.spec, "fake/m/low");
        assert_eq!(plain.target.url, "http://fake");
        assert!(plain.fallback.is_none());
        assert_eq!(agents.general.spec, "fake/m/low");
        assert!(!agents.feedback);
    }
}
