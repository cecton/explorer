use std::sync::Arc;
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
    pub model: Option<crate::agent::ModelSpec>,
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
        use crate::agent::ModelSpec;
        let (metadata, prompt) = crate::agent::commands::split_metadata(text);
        let prompt = prompt.trim();
        if prompt.is_empty() {
            return Err("it has no prompt".to_string());
        }
        let mut description = None;
        let mut model = None;
        let mut budget = None;
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

/// What a turn lends to the sub-agents it calls.
#[derive(Clone)]
pub struct Subagents {
    pub config: Arc<crate::agent::AgentConfig>,
    /// Mode of the calling turn: the sub-agents of a feedback turn must not commit
    /// either.
    pub mode: crate::agent::Mode,
    /// `YYYY-MM-DD`, for the repository note.
    pub date: String,
    pub repo: Option<gix::ThreadSafeRepository>,
    /// Model of the calling turn, for the sub-agents nothing else assigns one to.
    pub spec: crate::agent::ModelSpec,
}

/// The `agent` tool as advertised to the model, with the roles it offers.
pub fn spec(roles: &[Role]) -> crate::agent::llm::ToolSpec {
    use crate::agent::llm::ToolSpec;
    let mut description = DESCRIPTION.to_string();
    let mut properties = serde_json::json!({
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
        properties["role"] = serde_json::json!({
            "type": "string",
            "enum": names,
            "description": "Role of a new sub-agent; a continued one keeps its own"
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

/// A sub-agent run, recorded and ready to start.
struct Launch {
    index: u32,
    dir: camino::Utf8PathBuf,
    backend: crate::agent::Backend,
    endpoint: crate::agent::Endpoint,
    /// Model of this run, named in the notes of a retry.
    spec: crate::agent::ModelSpec,
    /// The role's name, for the retry's metadata.
    role: Option<String>,
    /// The role's prompt, or the default one.
    system: String,
    /// The role's time budget, when it sets one.
    budget: Option<Duration>,
    /// The task as typed, for the retry's `prompt.md`.
    prompt: String,
    description: String,
    /// The task, preceded by the notes Explorer adds.
    message: String,
    /// Directories of the runs this one continues, oldest first.
    chain: Vec<camino::Utf8PathBuf>,
    /// Session of OpenAI-compatible requests: the id of the first run of the chain.
    session: String,
    /// Claude Code session of the run this one continues, unless a commit caller resumes
    /// a run made under a feedback node.
    claude_session: Option<String>,
    /// Mode of the calling turn: a commit caller replays its chain without the feedback
    /// note.
    mode: crate::agent::Mode,
    /// Model to rerun on when this one fails: the caller's, for a fresh run on an
    /// OpenAI-compatible endpoint. `claude` has no such failures to recover from, and
    /// a continued run keeps its own model.
    fallback: Option<crate::agent::ModelSpec>,
}

fn str_arg<'a>(args: &'a serde_json::Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("missing string argument `{key}`"))
}

fn find_role<'a>(config: &'a crate::agent::AgentConfig, name: &str) -> Result<&'a Role, String> {
    config
        .roles
        .iter()
        .find(|role| role.name == name)
        .ok_or_else(|| format!("unknown role: {name}"))
}

/// Whether a resumed run may fork the Claude Code session of `chain`. A fork replays the
/// chain's messages, and a run under a feedback node opened with the feedback note: a
/// commit caller only forks a chain run under commit nodes.
fn forkable(
    tree: &crate::conversation::ConversationTree,
    mode: crate::agent::Mode,
    chain: &[(crate::conversation::NodeId, u32)],
) -> bool {
    use crate::agent::Mode;
    use crate::conversation::NodeType;
    mode == Mode::Feedback
        || chain.iter().all(|(node, _)| {
            tree.node(node)
                .is_some_and(|node| node.meta.node_type == Some(NodeType::Commit))
        })
}

/// Resolves a call into a run: its role, model and message. The run is recorded under
/// `node` when this returns. Reads files and runs git.
fn prepare(
    tree: &parking_lot::RwLock<crate::conversation::ConversationTree>,
    root: &camino::Utf8Path,
    node: &crate::conversation::NodeId,
    subagents: &Subagents,
    args: &serde_json::Value,
) -> Result<Launch, String> {
    use crate::agent::EndpointKind;
    use crate::agent::Mode;
    use crate::conversation::Metadata;
    use crate::conversation::NodeStatus;
    let config = &subagents.config;
    let description = str_arg(args, "description")?;
    let prompt = str_arg(args, "prompt")?;
    let resumed = match args.get("agent_id").and_then(serde_json::Value::as_str) {
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
                .and_then(serde_json::Value::as_str)
                .map(|name| find_role(config, name))
                .transpose()?;
            let spec = role
                .and_then(|role| role.model.clone())
                .or_else(|| config.subagent_model.clone())
                .unwrap_or_else(|| subagents.spec.clone());
            (role, spec)
        }
    };
    // A failure worth retrying comes from an OpenAI-compatible server. A rerun lands on
    // the caller's model unless the run is on it already.
    let fallback = match config
        .endpoint(&spec.endpoint)
        .map(|endpoint| &endpoint.kind)
    {
        Some(
            EndpointKind::OpenAi { .. }
            | EndpointKind::OpenCodeGo { .. }
            | EndpointKind::KimiCode { .. }
            | EndpointKind::Cline { .. }
            | EndpointKind::OpenRouter { .. },
        ) if source.is_none() && spec != subagents.spec => Some(subagents.spec.clone()),
        _ => None,
    };
    let endpoint = config.endpoint(&spec.endpoint).cloned();
    let (Some(endpoint), Some(backend)) = (endpoint, config.backend(&spec)) else {
        return Err(format!(
            "endpoint '{}' is not configured or has no token",
            spec.endpoint
        ));
    };

    let head = subagents.repo.as_ref().and_then(crate::git::head_id);
    let environment = Some(crate::agent::context::environment_note(
        root,
        head.as_deref(),
        &subagents.date,
    ));
    let moved = source.as_ref().and_then(|(meta, _)| {
        crate::agent::turn::moved_note(root, meta.head_after.as_deref(), head.as_deref())
    });
    let workspace = (subagents.mode == Mode::Feedback)
        .then(|| crate::agent::context::WORKSPACE_NOTE.to_string());
    let message = crate::agent::context::with_notes(&[environment, moved, workspace], prompt);

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
            .map(|(source, index)| crate::conversation::agent_id(source, *index)),
        ..Default::default()
    };
    let mut tree = tree.write();
    let index = tree
        .create_agent(node, crate::util::time::now_secs(), meta, prompt)
        .map_err(|e| format!("could not record the sub-agent: {e}"))?;
    let dir = tree
        .agent_dir(node, index)
        .ok_or("the new sub-agent has no directory")?;
    let chain = source.map(|(_, chain)| chain).unwrap_or_default();
    let session = match chain.first() {
        Some((first, first_index)) => crate::conversation::agent_id(first, *first_index),
        None => crate::conversation::agent_id(node, index),
    };
    let claude_session = resumed
        .as_ref()
        .filter(|_| forkable(&tree, subagents.mode, &chain))
        .and_then(|(source, index)| tree.agent(source, *index)?.meta.session_id.clone());
    Ok(Launch {
        index,
        dir,
        backend,
        endpoint,
        spec: spec.clone(),
        role: role.map(|role| role.name.clone()),
        system: role
            .map_or(crate::agent::context::SUBAGENT_PROMPT, |role| &role.prompt)
            .to_string(),
        budget: role.and_then(|role| role.budget),
        prompt: prompt.to_string(),
        description: description.to_string(),
        message,
        chain: chain
            .iter()
            .filter_map(|(node, index)| tree.agent_dir(node, *index))
            .collect(),
        session,
        claude_session,
        mode: subagents.mode,
        fallback,
    })
}

/// Runs one sub-agent run to its end, recording its live output and work-log
/// entries; whatever streamed before a failure stays in the record's `reply.md` and
/// `transcript.jsonl`.
async fn execute(
    context: &crate::agent::tools::ToolContext,
    subagents: &Subagents,
    recorder: &mut crate::agent::turn::Recorder,
    launch: &Launch,
    backend: &crate::agent::Backend,
) -> Outcome {
    use crate::agent::Backend;
    use crate::agent::turn::LiveSubagent;
    let mut child = recorder.subagent(launch.dir.clone());
    let started = Instant::now();
    recorder.live.lock().subagent = Some(LiveSubagent {
        index: launch.index,
        started,
        live: Arc::clone(&child.live),
    });
    // Composed here, not in `prepare`, so every run — a fallback rerun included —
    // names its own start and deadline in the budget note.
    let system = match launch.budget {
        Some(budget) => format!(
            "{}\n\n{}",
            launch.system.trim_end(),
            crate::agent::context::budget_note(budget)
        ),
        None => launch.system.clone(),
    };
    let result = match backend {
        Backend::OpenAi(target) => {
            run_own_loop(
                context, subagents, launch, target, &system, started, &mut child,
            )
            .await
        }
        Backend::ClaudeCode(target) => {
            run_claude_code(context, launch, target, &system, &mut child).await
        }
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
    Outcome {
        index: launch.index,
        spec: launch.spec.clone(),
        endpoint: launch.endpoint.clone(),
        result,
        reply,
        secs: started.elapsed().as_secs(),
        tokens_in,
        cached_in,
        session_id,
        usage,
    }
}

/// What one run left: where it is recorded, whether it ended cleanly, and what the
/// record's metadata keeps.
struct Outcome {
    index: u32,
    spec: crate::agent::ModelSpec,
    endpoint: crate::agent::Endpoint,
    result: Result<(), String>,
    reply: String,
    secs: u64,
    tokens_in: Option<u64>,
    cached_in: Option<u64>,
    session_id: Option<String>,
    usage: Option<crate::agent::usage::Usage>,
}

impl Outcome {
    /// The sub-agent's error, if any; the metadata records it.
    fn error(&self) -> Option<&str> {
        self.result.as_ref().err().map(String::as_str)
    }

    /// The status the record ends with.
    fn status(&self) -> crate::conversation::NodeStatus {
        use crate::conversation::NodeStatus;
        match &self.result {
            Err(_) => NodeStatus::Failed,
            Ok(()) => NodeStatus::Done,
        }
    }
}

/// The fallback run of a failed OpenAI-backed launch: a fresh record on `spec`'s
/// backend, same role, prompt and notes, keeping no chain. Reads and writes only the
/// tree, like `prepare` does; the backend resolution happens on the caller's side,
/// where `config` lives.
async fn prepare_retry(
    context: &crate::agent::tools::ToolContext,
    launch: &Launch,
    spec: &crate::agent::ModelSpec,
    config: &crate::agent::AgentConfig,
) -> Result<Launch, String> {
    use crate::conversation::Metadata;
    use crate::conversation::NodeStatus;
    let (endpoint, backend) = (
        config.endpoint(&spec.endpoint).cloned(),
        config.backend(spec),
    );
    let (Some(endpoint), Some(backend)) = (endpoint, backend) else {
        return Err(format!(
            "the fallback endpoint '{}' is not configured or has no token",
            spec.endpoint
        ));
    };
    let tree = Arc::clone(&context.tree);
    let node = context.node.clone();
    let spec = spec.clone();
    let meta = Metadata {
        endpoint: Some(spec.endpoint.clone()),
        kind: Some(endpoint.kind.label().to_string()),
        model: Some(spec.model.clone()),
        effort: Some(spec.effort.as_str().to_string()),
        status: Some(NodeStatus::Running),
        role: launch.role.clone(),
        description: Some(launch.description.clone()),
        ..Default::default()
    };
    let prompt = launch.prompt.clone();
    let message = launch.message.clone();
    let system = launch.system.clone();
    let recorded = tokio::task::spawn_blocking(move || {
        let mut tree = tree.write();
        let index = tree
            .create_agent(&node, crate::util::time::now_secs(), meta, &prompt)
            .map_err(|e| format!("could not record the sub-agent: {e}"))?;
        let dir = tree
            .agent_dir(&node, index)
            .ok_or("the new sub-agent has no directory")?;
        Ok::<_, String>((index, dir))
    })
    .await
    .map_err(|e| e.to_string())?;
    let (index, dir) = recorded?;
    Ok(Launch {
        index,
        dir,
        backend,
        endpoint,
        spec: spec.clone(),
        role: launch.role.clone(),
        system,
        budget: launch.budget,
        prompt: launch.prompt.clone(),
        description: launch.description.clone(),
        message,
        chain: Vec::new(),
        session: crate::conversation::agent_id(&context.node, index),
        claude_session: None,
        mode: launch.mode,
        fallback: None,
    })
}

/// Runs one call of the `agent` tool for the turn of `context.node`, and returns its
/// result for the model: the id of the run, then the sub-agent's final answer. A
/// failure is returned as text, never raised.
pub async fn run(
    context: &crate::agent::tools::ToolContext,
    subagents: &Subagents,
    call: &crate::agent::ToolCall,
    recorder: &mut crate::agent::turn::Recorder,
) -> String {
    use crate::tui::state::AppSignal;
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
    recorder
        .send(AppSignal::SubagentStarted {
            node: context.node.clone(),
        })
        .await;

    let mut outcome = execute(context, subagents, recorder, &launch, &launch.backend).await;
    // A failed run is concluded at once, so a cancel mid-retry cannot leave it Running;
    // the combined error is only what the caller model reads, never a record's.
    let first_error = outcome.result.as_ref().err().cloned();
    match (first_error.as_deref(), launch.fallback.as_ref()) {
        (Some(error), Some(spec)) => {
            conclude(
                context,
                subagents,
                subagents.repo.as_ref(),
                &outcome,
                recorder,
            )
            .await;
            match prepare_retry(context, &launch, spec, &subagents.config).await {
                Ok(retry) => {
                    recorder.log(format!(
                        "first run on {} failed ({error}); retrying on {}",
                        launch.spec, retry.spec
                    ));
                    let retried =
                        execute(context, subagents, recorder, &retry, &retry.backend).await;
                    conclude(
                        context,
                        subagents,
                        subagents.repo.as_ref(),
                        &retried,
                        recorder,
                    )
                    .await;
                    let retry_spec = retried.spec.clone();
                    let retry_error = retried.result.as_ref().err().cloned();
                    outcome = match retry_error {
                        Some(retry_error) => Outcome {
                            result: Err(format!(
                                "{error}; the rerun on {retry_spec} failed: {retry_error}"
                            )),
                            ..retried
                        },
                        None => retried,
                    };
                }
                Err(e) => {
                    recorder.log(format!(
                        "first run on {} failed ({error}); no fallback: {e}",
                        launch.spec
                    ));
                }
            }
        }
        _ => {
            conclude(
                context,
                subagents,
                subagents.repo.as_ref(),
                &outcome,
                recorder,
            )
            .await
        }
    }
    crate::agent::turn::report_usage(
        outcome.endpoint.clone(),
        outcome.usage.clone(),
        recorder.main_tx(),
    );

    let id = crate::conversation::agent_id(&context.node, outcome.index);
    let over_budget = match launch.budget {
        Some(budget) if outcome.secs >= budget.as_secs() => format!(
            "[Note from Explorer: the sub-agent hit its {}m time budget; its answer \
             may be partial.]\n\n",
            budget.as_secs() / 60
        ),
        _ => String::new(),
    };
    match &outcome.result {
        Err(error) => {
            format!("agent_id: {id}\n\n{over_budget}error: the sub-agent failed: {error}")
        }
        Ok(()) => {
            let note = first_error
                .as_deref()
                .and_then(|error| retry_note(&launch, error))
                .unwrap_or_default();
            let answer = if outcome.reply.is_empty() {
                "(the sub-agent gave no answer)"
            } else {
                outcome.reply.as_str()
            };
            format!("agent_id: {id}\n\n{over_budget}{note}{answer}")
        }
    }
}

/// Closes the record of `outcome`: its final status, error, size of its requests and
/// session on disk, and a `SubagentFinished` for the tree and open windows.
async fn conclude(
    context: &crate::agent::tools::ToolContext,
    subagents: &Subagents,
    repo: Option<&gix::ThreadSafeRepository>,
    outcome: &Outcome,
    recorder: &mut crate::agent::turn::Recorder,
) {
    use crate::tui::state::AppSignal;
    let id = crate::conversation::agent_id(&context.node, outcome.index);
    let updated = context
        .tree
        .write()
        .update_agent_meta(&context.node, outcome.index, |meta| {
            meta.status = Some(outcome.status());
            meta.error = outcome.error().map(str::to_string);
            meta.secs = Some(outcome.secs);
            meta.head_after = repo.and_then(crate::git::head_id);
            meta.tokens_in = outcome.tokens_in;
            meta.cached_in = outcome.cached_in;
            meta.session_id = outcome.session_id.clone();
        });
    if let Err(e) = updated {
        tracing::warn!("failed to save the outcome of sub-agent {id}: {e}");
    }
    title_run(context, subagents, outcome, recorder.main_tx());
    recorder
        .send(AppSignal::SubagentFinished {
            node: context.node.clone(),
            index: outcome.index,
        })
        .await;
}

/// Asks the summary model what the finished run came to, and writes its answer as the
/// run's `title.md`. Detached: the caller is waiting on the run's answer, which the
/// title does not belong to.
fn title_run(
    context: &crate::agent::tools::ToolContext,
    subagents: &Subagents,
    outcome: &Outcome,
    main_tx: crate::agent::turn::MainTx,
) {
    use crate::tui::state::AppSignal;
    let Some(backend) = title_backend(subagents, outcome) else {
        return;
    };
    let (node, index) = (context.node.clone(), outcome.index);
    let session = crate::conversation::agent_id(&node, index);
    let dir = match context.tree.read().agent_dir(&node, index) {
        Some(dir) => dir,
        None => return,
    };
    let reply = outcome.reply.clone();
    tokio::spawn(async move {
        let task = std::fs::read_to_string(dir.join("prompt.md")).unwrap_or_default();
        let input = run_title_input(&task, &reply);
        let answer = match crate::agent::turn::complete(
            &backend,
            &session,
            crate::agent::context::SUBAGENT_TITLE_PROMPT,
            &input,
        )
        .await
        {
            Ok(answer) => answer,
            Err(e) => {
                tracing::warn!("title generation for sub-agent {session} failed: {e}");
                return;
            }
        };
        let Some((title, _)) = crate::agent::turn::parse_title_and_summary(&answer) else {
            tracing::warn!("title generation for sub-agent {session} returned no title");
            return;
        };
        if let Err(e) = crate::util::fs::write_atomic(&dir.join("title.md"), &title) {
            tracing::warn!("failed to write the title of sub-agent {session}: {e}");
        }
        let _ = main_tx
            .send(r3bl_tui::TerminalWindowMainThreadSignal::ApplyAppSignal(
                AppSignal::AgentTitleReady { node, index, title },
            ))
            .await;
    });
}

/// The backend that titles a finished run, or `None` when the run has nothing to
/// describe or no summary model is configured.
fn title_backend(subagents: &Subagents, outcome: &Outcome) -> Option<crate::agent::Backend> {
    if !crate::conversation::has_content(&outcome.reply) {
        return None;
    }
    subagents
        .config
        .summary_model
        .as_ref()
        .and_then(|summary| subagents.config.backend(summary))
}

/// What the summary model reads for one run: the task and the end of its answer. The
/// task says what was asked; the end of the answer is where the run's verdict is.
fn run_title_input(task: &str, reply: &str) -> String {
    let reply = match reply
        .char_indices()
        .rev()
        .nth(crate::agent::turn::REPLY_MAX_CHARS - 1)
    {
        Some((start, _)) => &reply[start..],
        None => reply,
    };
    format!(
        "<task>\n{}\n</task>\n\n<reply>\n{}\n</reply>\n\n\
         Write the title of the run recorded above.\n",
        task.trim(),
        reply.trim()
    )
}

/// What the caller gets back before a fallback run's answer: the primary spec, its
/// error and the spec that answered. `None` when the answer is the first run's own.
fn retry_note(launch: &Launch, first_error: &str) -> Option<String> {
    launch.fallback.as_ref().map(|fallback| {
        format!(
            "[Note from Explorer: the first run on {} failed: {first_error}. This answer \
             comes from a rerun on {fallback}.]\n\n",
            launch.spec
        )
    })
}

async fn run_own_loop(
    context: &crate::agent::tools::ToolContext,
    subagents: &Subagents,
    launch: &Launch,
    target: &crate::agent::llm::LlmTarget,
    system: &str,
    started: Instant,
    recorder: &mut crate::agent::turn::Recorder,
) -> Result<(), String> {
    use crate::agent::Message;
    use crate::agent::Mode;
    use crate::agent::agent_loop::Limits;
    use crate::agent::tools::ToolContext;
    let (system, mut messages) = {
        let lead = system.to_string();
        let root = context.root.clone();
        let chain = launch.chain.clone();
        let mode = launch.mode;
        tokio::task::spawn_blocking(move || {
            let mut messages: Vec<Message> = chain
                .iter()
                .flat_map(|dir| crate::agent::context::ancestor_messages(dir))
                .collect();
            if mode == Mode::Commit {
                crate::agent::context::strip_workspace_note(&mut messages);
            }
            (
                crate::agent::context::subagent_system_prompt(&lead, &root),
                messages,
            )
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
        mcp: context.mcp.clone(),
    };
    crate::agent::agent_loop::run(
        &tool_context,
        target,
        &launch.session,
        &system,
        &mut messages,
        recorder,
        Limits {
            budget: launch.budget.map(|budget| (started, budget)),
            repeat_limit: subagents.config.repeat_limit,
        },
    )
    .await
}

async fn run_claude_code(
    context: &crate::agent::tools::ToolContext,
    launch: &Launch,
    target: &crate::agent::claude_code::ClaudeTarget,
    system: &str,
    recorder: &mut crate::agent::turn::Recorder,
) -> Result<(), String> {
    use crate::agent::claude_code::Invocation;
    use crate::agent::claude_code::Resume;
    use crate::agent::claude_code::RunError;
    let invocation = |prompt, resume| Invocation {
        target,
        root: &context.root,
        system,
        prompt,
        resume,
        mcp: &context.mcp,
    };
    // Fork the session of the run continued; when it is gone, start a fresh one that
    // opens with the earlier runs as plain text.
    if let Some(session) = launch.claude_session.as_deref() {
        match crate::agent::claude_code::run(
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
        tokio::task::spawn_blocking(move || crate::agent::context::handover_preamble(&chain))
            .await
            .map_err(|e| e.to_string())?
    };
    let opening = match preamble {
        Some(preamble) => format!("{preamble}\n\n{}", launch.message),
        None => launch.message.clone(),
    };
    crate::agent::claude_code::run(&invocation(&opening, Resume::Fresh), recorder)
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

    #[test]
    fn only_a_finished_run_with_something_to_describe_and_a_summary_model_is_titled() {
        use crate::agent::AgentConfig;
        use crate::agent::Endpoint;
        use crate::agent::EndpointKind;
        use crate::agent::Mode;
        use crate::agent::ModelSpec;
        let config = |summary: Option<ModelSpec>| Subagents {
            config: Arc::new(AgentConfig {
                endpoints: vec![Endpoint {
                    name: "router".to_string(),
                    kind: EndpointKind::OpenAi {
                        url: "https://r".to_string(),
                        token: "sk-or".to_string(),
                    },
                }],
                summary_model: summary,
                ..Default::default()
            }),
            mode: Mode::Commit,
            date: "2026-11-01".to_string(),
            repo: None,
            spec: crate::agent::ModelSpec::parse("router/x/low").unwrap(),
        };
        let outcome = |result: Result<(), String>, reply: &str| Outcome {
            index: 1,
            spec: crate::agent::ModelSpec::parse("router/x/low").unwrap(),
            endpoint: Endpoint {
                name: "router".to_string(),
                kind: EndpointKind::OpenAi {
                    url: "https://r".to_string(),
                    token: "sk-or".to_string(),
                },
            },
            result,
            reply: reply.to_string(),
            secs: 1,
            tokens_in: None,
            cached_in: None,
            session_id: None,
            usage: None,
        };
        let summary = Some(crate::agent::ModelSpec::parse("router/x/low").unwrap());

        // A finished run with an answer, and a summary model to ask.
        assert!(title_backend(&config(summary.clone()), &outcome(Ok(()), "done")).is_some());
        // Nothing to describe: an empty answer, whatever the status.
        assert!(title_backend(&config(summary.clone()), &outcome(Ok(()), "  ")).is_none());
        assert!(title_backend(&config(summary.clone()), &outcome(Err("x".into()), "")).is_none());
        // A failed run that still answered is described like any other.
        assert!(
            title_backend(
                &config(summary.clone()),
                &outcome(Err("x".into()), "half an answer")
            )
            .is_some()
        );
        // No summary model, nothing asked.
        assert!(title_backend(&config(None), &outcome(Ok(()), "done")).is_none());
        // An endpoint without a token resolves to no backend.
        let unknown = Some(crate::agent::ModelSpec::parse("gone/x/low").unwrap());
        assert!(title_backend(&config(unknown), &outcome(Ok(()), "done")).is_none());
    }

    #[test]
    fn the_run_input_keeps_the_end_of_a_long_answer() {
        let reply = format!("{}\nVerdict: merge.", "x".repeat(20_000));
        let input = run_title_input("Review it", &reply);
        assert!(input.starts_with("<task>\nReview it\n</task>\n\n<reply>\n"));
        assert!(input.ends_with(
            "Verdict: merge.\n</reply>\n\nWrite the title of the run recorded above.\n"
        ));
        assert!(input.len() < 20_000);
        // The cap counts characters, and never splices one, as the node path's does.
        let multibyte = "é".repeat(crate::agent::turn::REPLY_MAX_CHARS + 10);
        let input = run_title_input("Task", &multibyte);
        assert_eq!(
            input.matches('é').count(),
            crate::agent::turn::REPLY_MAX_CHARS
        );
        assert_eq!(
            run_title_input("Task", "Short."),
            "<task>\nTask\n</task>\n\n<reply>\nShort.\n</reply>\n\n\
             Write the title of the run recorded above.\n"
        );
    }

    #[test]
    fn a_commit_caller_forks_only_a_chain_under_commit_nodes() {
        use crate::agent::Mode;
        use crate::conversation::ConversationTree;
        use crate::conversation::Metadata;
        use crate::conversation::NodeId;
        use crate::conversation::NodeType;
        let tmp = tempfile::tempdir().unwrap();
        let root = camino::Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let mut tree = ConversationTree::load(&root);
        let mut node = |parent: Option<&NodeId>, secs, node_type| {
            let meta = Metadata {
                node_type: Some(node_type),
                ..Default::default()
            };
            tree.create_node(parent, secs, meta, "x", None).unwrap()
        };
        let feedback = node(None, 1, NodeType::Feedback);
        let commit = node(Some(&feedback), 2, NodeType::Commit);
        let later = node(Some(&commit), 3, NodeType::Commit);

        let under_commits = [(commit.clone(), 1), (later.clone(), 1)];
        assert!(forkable(&tree, Mode::Commit, &under_commits));
        assert!(forkable(&tree, Mode::Feedback, &under_commits));

        let through_feedback = [(feedback, 1), (later, 1)];
        assert!(!forkable(&tree, Mode::Commit, &through_feedback));
        assert!(forkable(&tree, Mode::Feedback, &through_feedback));
    }
}
