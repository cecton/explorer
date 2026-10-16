use super::claude_code::{self, ClaudeTarget, Invocation, Resume, RunError};
use super::commands::split_metadata;
use super::llm::{LlmTarget, ToolSpec};
use super::tools::ToolContext;
use super::turn::{self, LiveSubagent, Recorder};
use super::{
    AgentConfig, Backend, Endpoint, Message, Mode, ModelSpec, ToolCall, agent_loop, context,
};
use crate::conversation::{self, ConversationTree, Metadata, NodeId, NodeStatus};
use crate::tui::AppSignal;
use camino::{Utf8Path, Utf8PathBuf};
use parking_lot::RwLock;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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
matters.\n\n\
The result opens with `agent_id: <id>`. Pass that id as `agent_id` to continue the same \
sub-agent, with its earlier messages and work, instead of starting a fresh one.";

/// A sub-agent role, read from `<config_dir>/agents/<name>.md`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Role {
    pub name: String,
    /// When to call it, as the `agent` tool tells the model.
    pub description: String,
    /// Model the role runs on, ahead of the `subagent-model` setting.
    pub model: Option<ModelSpec>,
    /// The role's system prompt.
    pub prompt: String,
}

impl Role {
    /// Reads a role file: a metadata block of `key: value` lines between two `---`
    /// lines, with a `description` and an optional `model`, then the system prompt.
    pub fn parse(name: &str, text: &str) -> Result<Self, String> {
        let (metadata, prompt) = split_metadata(text);
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err("it has no prompt".to_string());
        }
        let mut description = None;
        let mut model = None;
        for line in metadata.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            match key.trim() {
                "description" => description = Some(value.to_string()),
                "model" => {
                    model = Some(ModelSpec::parse(value).ok_or_else(|| {
                        format!("model '{value}' is not of the form endpoint/model/effort")
                    })?);
                }
                _ => {}
            }
        }
        Ok(Self {
            name: name.to_string(),
            description: description.ok_or("it has no description")?,
            model,
            prompt: prompt.to_string(),
        })
    }
}

/// What a turn lends to the sub-agents it calls.
#[derive(Clone)]
pub struct Subagents {
    pub config: Arc<AgentConfig>,
    /// Mode of the calling turn: the sub-agents of a feedback turn must not commit
    /// either.
    pub mode: Mode,
    /// `YYYY-MM-DD`, for the repository note.
    pub date: String,
    pub repo: Option<gix::ThreadSafeRepository>,
    /// Model of the calling turn, for the sub-agents nothing else assigns one to.
    pub spec: ModelSpec,
}

/// The `agent` tool as advertised to the model, with the roles it offers.
pub fn spec(roles: &[Role]) -> ToolSpec {
    let mut description = DESCRIPTION.to_string();
    let mut properties = json!({
        "description": {"type": "string", "description": "A short title of the task, 3 to 5 words"},
        "prompt": {"type": "string", "description": "The task, complete on its own"},
        "agent_id": {"type": "string", "description": "Id of an earlier sub-agent to continue, from the first line of its result"}
    });
    if !roles.is_empty() {
        description.push_str(
            "\n\nRoles, to pass as `role`; without one, a general-purpose sub-agent runs:\n",
        );
        for role in roles {
            description.push_str(&format!("- {}: {}\n", role.name, role.description));
        }
        let names: Vec<&str> = roles.iter().map(|role| role.name.as_str()).collect();
        properties["role"] = json!({
            "type": "string",
            "enum": names,
            "description": "Role of a new sub-agent; a continued one keeps its own"
        });
    }
    ToolSpec {
        name: TOOL,
        description,
        schema: json!({
            "type": "object",
            "properties": properties,
            "required": ["description", "prompt"]
        }),
    }
}

/// The work-log line of a call: `agent reviewer: Review the commits`.
pub fn describe(args: &Value) -> String {
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .and_then(|text| text.lines().next())
        .unwrap_or("");
    match args.get("role").and_then(Value::as_str) {
        Some(role) => format!("{TOOL} {role}: {description}"),
        None => format!("{TOOL}: {description}"),
    }
}

/// A sub-agent run, recorded and ready to start.
struct Launch {
    index: u32,
    dir: Utf8PathBuf,
    backend: Backend,
    endpoint: Endpoint,
    /// The role's prompt, or the default one.
    system: String,
    /// The task, preceded by the notes Explorer adds.
    message: String,
    /// Directories of the runs this one continues, oldest first.
    chain: Vec<Utf8PathBuf>,
    /// Session of OpenAI-compatible requests: the id of the first run of the chain.
    session: String,
    /// Claude Code session of the run this one continues.
    claude_session: Option<String>,
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string argument `{key}`"))
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

fn find_role<'a>(config: &'a AgentConfig, name: &str) -> Result<&'a Role, String> {
    config
        .roles
        .iter()
        .find(|role| role.name == name)
        .ok_or_else(|| format!("unknown role: {name}"))
}

/// Resolves a call into a run: its role, model and message. The run is recorded under
/// `node` when this returns. Reads files and runs git.
fn prepare(
    tree: &RwLock<ConversationTree>,
    root: &Utf8Path,
    node: &NodeId,
    subagents: &Subagents,
    args: &Value,
) -> Result<Launch, String> {
    let config = &subagents.config;
    let description = str_arg(args, "description")?;
    let prompt = str_arg(args, "prompt")?;
    let resumed = match args.get("agent_id").and_then(Value::as_str) {
        Some(id) => Some(tree.read().resumable_agent(node, id)?),
        None => None,
    };
    let source = resumed.as_ref().and_then(|(source, index)| {
        let tree = tree.read();
        Some((
            tree.agent(source, *index)?.meta.clone(),
            tree.agent_chain(source, *index),
        ))
    });
    let (role, spec) = match &source {
        // A continued run keeps its model: a role file may have changed since, and a
        // Claude Code session only continues on its own.
        Some((meta, _)) => {
            let role = meta
                .role
                .as_deref()
                .map(|name| find_role(config, name))
                .transpose()?;
            let spec = meta
                .model_spec()
                .ok_or("the sub-agent to continue does not record its model")?;
            (role, spec)
        }
        None => {
            let role = args
                .get("role")
                .and_then(Value::as_str)
                .map(|name| find_role(config, name))
                .transpose()?;
            let spec = role
                .and_then(|role| role.model.clone())
                .or_else(|| config.subagent_model.clone())
                .unwrap_or_else(|| subagents.spec.clone());
            (role, spec)
        }
    };
    let endpoint = config.endpoint(&spec.endpoint).cloned();
    let (Some(endpoint), Some(backend)) = (endpoint, config.backend(&spec)) else {
        return Err(format!(
            "endpoint '{}' is not configured or has no token",
            spec.endpoint
        ));
    };

    let head = subagents.repo.as_ref().and_then(crate::git::head_id);
    let environment = Some(context::environment_note(
        root,
        head.as_deref(),
        &subagents.date,
    ));
    let moved = source
        .as_ref()
        .and_then(|(meta, _)| turn::moved_note(root, meta.head_after.as_deref(), head.as_deref()));
    let workspace = (subagents.mode == Mode::Feedback).then(|| context::WORKSPACE_NOTE.to_string());
    let message = context::with_notes(&[environment, moved, workspace], prompt);

    let meta = Metadata {
        endpoint: Some(spec.endpoint.clone()),
        kind: Some(endpoint.kind.label().to_string()),
        model: Some(spec.model.clone()),
        effort: Some(spec.effort.as_str().to_string()),
        status: Some(NodeStatus::Running),
        role: role.map(|role| role.name.clone()),
        description: Some(description.to_string()),
        resumed_from: resumed
            .as_ref()
            .map(|(source, index)| conversation::agent_id(source, *index)),
        ..Default::default()
    };
    let mut tree = tree.write();
    let index = tree
        .create_agent(node, now_secs(), meta, prompt)
        .map_err(|e| format!("could not record the sub-agent: {e}"))?;
    let dir = tree
        .agent_dir(node, index)
        .ok_or("the new sub-agent has no directory")?;
    let chain = source.map(|(_, chain)| chain).unwrap_or_default();
    let session = match chain.first() {
        Some((first, first_index)) => conversation::agent_id(first, *first_index),
        None => conversation::agent_id(node, index),
    };
    let claude_session = resumed
        .as_ref()
        .and_then(|(source, index)| tree.agent(source, *index)?.meta.session_id.clone());
    Ok(Launch {
        index,
        dir,
        backend,
        endpoint,
        system: role
            .map_or(context::SUBAGENT_PROMPT, |role| &role.prompt)
            .to_string(),
        message,
        chain: chain
            .iter()
            .filter_map(|(node, index)| tree.agent_dir(node, *index))
            .collect(),
        session,
        claude_session,
    })
}

/// Runs one call of the `agent` tool for the turn of `context.node`, and returns its
/// result for the model: the id of the run, then the sub-agent's final answer. A
/// failure is returned as text, never raised.
pub async fn run(
    context: &ToolContext,
    subagents: &Subagents,
    call: &ToolCall,
    recorder: &mut Recorder,
) -> String {
    let launch = {
        let tree = Arc::clone(&context.tree);
        let root = context.root.clone();
        let node = context.node.clone();
        let subagents = subagents.clone();
        let args = call.arguments.clone();
        tokio::task::spawn_blocking(move || prepare(&tree, &root, &node, &subagents, &args))
            .await
            .map_err(|e| e.to_string())
            .and_then(|prepared| prepared)
    };
    let launch = match launch {
        Ok(launch) => launch,
        Err(error) => return format!("error: {error}"),
    };
    let id = conversation::agent_id(&context.node, launch.index);
    recorder
        .send(AppSignal::SubagentStarted {
            node: context.node.clone(),
        })
        .await;

    let mut child = recorder.subagent(launch.dir.clone());
    let started = Instant::now();
    recorder.live.lock().subagent = Some(LiveSubagent {
        index: launch.index,
        started,
        live: Arc::clone(&child.live),
    });
    let result = match &launch.backend {
        Backend::OpenAi(target) => run_own_loop(context, &launch, target, &mut child).await,
        Backend::ClaudeCode(target) => run_claude_code(context, &launch, target, &mut child).await,
    };
    child.flush_reply();
    recorder.live.lock().subagent = None;

    let (reply, tokens_in, cached_in, session_id, usage) = {
        let live = child.live.lock();
        (
            live.reply.trim().to_string(),
            live.tokens_in,
            live.cached_in,
            live.session_id.clone(),
            live.usage.clone(),
        )
    };
    let head_after = subagents.repo.as_ref().and_then(crate::git::head_id);
    let error = result.as_ref().err().cloned();
    let updated = context
        .tree
        .write()
        .update_agent_meta(&context.node, launch.index, |meta| {
            meta.status = Some(match error {
                Some(_) => NodeStatus::Failed,
                None => NodeStatus::Done,
            });
            meta.error = error;
            meta.secs = Some(started.elapsed().as_secs());
            meta.head_after = head_after;
            meta.tokens_in = tokens_in;
            meta.cached_in = cached_in;
            meta.session_id = session_id;
        });
    if let Err(e) = updated {
        tracing::warn!("failed to save the outcome of sub-agent {id}: {e}");
    }
    recorder
        .send(AppSignal::SubagentFinished {
            node: context.node.clone(),
            index: launch.index,
        })
        .await;
    turn::report_usage(launch.endpoint, usage, recorder.main_tx());

    match result {
        Err(error) => format!("agent_id: {id}\n\nerror: the sub-agent failed: {error}"),
        Ok(()) if reply.is_empty() => format!("agent_id: {id}\n\n(the sub-agent gave no answer)"),
        Ok(()) => format!("agent_id: {id}\n\n{reply}"),
    }
}

async fn run_own_loop(
    context: &ToolContext,
    launch: &Launch,
    target: &LlmTarget,
    recorder: &mut Recorder,
) -> Result<(), String> {
    let (system, mut messages) = {
        let role = launch.system.clone();
        let root = context.root.clone();
        let chain = launch.chain.clone();
        tokio::task::spawn_blocking(move || {
            let messages: Vec<Message> = chain
                .iter()
                .flat_map(|dir| context::ancestor_messages(dir))
                .collect();
            (context::subagent_system_prompt(&role, &root), messages)
        })
        .await
        .map_err(|e| e.to_string())?
    };
    let message = Message::User {
        content: launch.message.clone(),
        injected: false,
    };
    recorder.message(&message);
    messages.push(message);
    let tool_context = ToolContext {
        root: context.root.clone(),
        tree: Arc::clone(&context.tree),
        node: context.node.clone(),
        subagents: None,
        background: Arc::clone(&context.background),
    };
    agent_loop::run(
        &tool_context,
        target,
        &launch.session,
        &system,
        &mut messages,
        recorder,
    )
    .await
}

async fn run_claude_code(
    context: &ToolContext,
    launch: &Launch,
    target: &ClaudeTarget,
    recorder: &mut Recorder,
) -> Result<(), String> {
    let invocation = |prompt, resume| Invocation {
        target,
        root: &context.root,
        system: &launch.system,
        prompt,
        resume,
    };
    // Fork the session of the run continued; when it is gone, start a fresh one that
    // opens with the earlier runs as plain text.
    if let Some(session) = launch.claude_session.as_deref() {
        match claude_code::run(
            &invocation(&launch.message, Resume::Fork(session)),
            recorder,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(RunError::SessionGone) => {}
            Err(RunError::Failed(error)) => return Err(error),
        }
    }
    let preamble = {
        let chain = launch.chain.clone();
        tokio::task::spawn_blocking(move || context::handover_preamble(&chain))
            .await
            .map_err(|e| e.to_string())?
    };
    let opening = match preamble {
        Some(preamble) => format!("{preamble}\n\n{}", launch.message),
        None => launch.message.clone(),
    };
    claude_code::run(&invocation(&opening, Resume::Fresh), recorder)
        .await
        .map_err(|error| match error {
            RunError::SessionGone => "the Claude Code session to continue is gone".to_string(),
            RunError::Failed(error) => error,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_spec_lists_the_roles_and_offers_a_role_only_when_there_are_some() {
        let bare = spec(&[]);
        assert!(bare.schema["properties"].get("role").is_none());
        assert!(!bare.description.contains("Roles"));

        let reviewer = Role::parse("reviewer", "---\ndescription: Reviews\n---\nP\n").unwrap();
        let with_role = spec(&[reviewer]);
        assert_eq!(
            with_role.schema["properties"]["role"]["enum"],
            json!(["reviewer"])
        );
        assert!(with_role.description.ends_with("- reviewer: Reviews\n"));
    }

    #[test]
    fn a_call_is_described_by_its_role_and_title() {
        let call = json!({"role": "reviewer", "description": "Review it\nsecond line"});
        assert_eq!(describe(&call), "agent reviewer: Review it");
        assert_eq!(describe(&json!({"description": "Fix"})), "agent: Fix");
    }
}
