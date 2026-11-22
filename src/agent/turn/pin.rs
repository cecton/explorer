//! What a turn on the own agent loop records, pinned end to end against a scripted
//! server: the files of the node and of its sub-agent run, the live counters, the
//! requests the model received and the outcome the main loop is told. The loop is going
//! to move to a process of its own; these tests say what it must keep doing.

#[cfg(test)]
mod tests {
    use crate::agent::Mode;
    use crate::agent::fake_llm::{FakeLlm, Reply};
    use crate::agent::turn::live::LiveTurn;
    use crate::agent::turn::{TurnHandle, TurnRequest};
    use crate::conversation::NodeStatus;
    use crate::signal::AppSignal;
    use crate::util::test_support::{commit, temp_repo, temp_root};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::time::Duration;

    /// A turn on a fake server, with the node it runs on.
    struct Setup {
        _tmp: tempfile::TempDir,
        _store: tempfile::TempDir,
        root: camino::Utf8PathBuf,
        node_dir: camino::Utf8PathBuf,
        request: TurnRequest,
        server: FakeLlm,
    }

    async fn setup(mode: Mode, script: Vec<Reply>) -> Setup {
        use crate::agent::AgentConfig;
        use crate::agent::mcp::McpHost;
        use crate::conversation::ConversationTree;
        use crate::conversation::metadata::Metadata;
        use crate::endpoint::{Endpoint, EndpointKind, Provider};
        use crate::model_spec::ModelSpec;
        let (tmp, root) = temp_repo();
        let head = commit(&root, "hello.txt", "hello world\n", "init");
        let server = FakeLlm::start(script).await;
        // Outside the repository, so the tree never shows up as uncommitted work.
        let (store_tmp, store) = temp_root();
        let mut tree = ConversationTree::load(&store);
        let node = tree
            .create_node(None, 1, Metadata::default(), "do the thing", None)
            .unwrap();
        let node_dir = tree.node_dir(&node).unwrap();
        let endpoint = Endpoint {
            name: "fake".to_string(),
            kind: EndpointKind::Http {
                url: server.url.clone(),
                token: "secret".to_string(),
                provider: Provider::OpenAi,
            },
        };
        let spec = ModelSpec::parse("fake/model").unwrap();
        let agent = Arc::new(AgentConfig {
            endpoints: vec![endpoint.clone()],
            ..AgentConfig::default()
        });
        let backend = agent.backend(&spec).unwrap();
        let request = TurnRequest {
            mode,
            system_prompt: true,
            node,
            node_dir: node_dir.clone(),
            ancestor_dirs: Vec::new(),
            parent_head: None,
            head: Some(head),
            root: root.clone(),
            repo: Some(gix::open(root.as_std_path()).unwrap().into_sync()),
            prompt: "do the thing".to_string(),
            date: "2026-11-22".to_string(),
            backend,
            endpoint,
            spec,
            agent,
            summary_backend: None,
            session: "root".to_string(),
            parent_session: None,
            continue_session: None,
            compact: None,
            compacted_parent: None,
            tree: Arc::new(parking_lot::RwLock::new(tree)),
            mcp: Arc::new(McpHost::default()),
        };
        Setup {
            _tmp: tmp,
            _store: store_tmp,
            root,
            node_dir,
            request,
            server,
        }
    }

    /// Runs the turn until `TurnFinished`, handing the handle to `during` once it started.
    async fn run_turn(
        setup: &Setup,
        during: impl AsyncFnOnce(&TurnHandle),
    ) -> (
        TurnHandle,
        crate::conversation::metadata::TurnOutcome,
        Vec<AppSignal>,
    ) {
        let (main_tx, mut main_rx) = tokio::sync::mpsc::channel(64);
        let handle = crate::agent::turn::spawn(setup.request.clone(), main_tx);
        during(&handle).await;
        let mut signals = Vec::new();
        let outcome = tokio::time::timeout(Duration::from_secs(60), async {
            while let Some(signal) = main_rx.recv().await {
                let r3bl_tui::TerminalWindowMainThreadSignal::ApplyAppSignal(signal) = signal
                else {
                    continue;
                };
                if let AppSignal::TurnFinished { outcome, .. } = signal {
                    return outcome;
                }
                signals.push(signal);
            }
            panic!("the turn never finished");
        })
        .await
        .expect("the turn did not finish in time");
        (handle, outcome, signals)
    }

    fn read_file(dir: &camino::Utf8Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).unwrap_or_default()
    }

    fn transcript(dir: &camino::Utf8Path) -> Vec<Value> {
        read_file(dir, crate::conversation::files::TRANSCRIPT)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// The node's transcript with the repository path of the run made constant.
    fn transcript_of(setup: &Setup) -> Vec<Value> {
        transcript(&setup.node_dir)
            .into_iter()
            .map(|message| {
                serde_json::from_str(
                    &message
                        .to_string()
                        .replace(setup.root.as_str(), "<root>")
                        .replace(setup.request.head.as_deref().unwrap(), "<head>"),
                )
                .unwrap()
            })
            .collect()
    }

    /// The text of the last user message of request `n` the server received.
    fn last_user_text(setup: &Setup, n: usize) -> String {
        let requests = setup.server.requests();
        let messages = requests[n]["messages"].as_array().unwrap();
        let last = messages
            .iter()
            .rev()
            .find(|message| message["role"] == "user")
            .unwrap();
        last["content"].as_str().unwrap().to_string()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_turn_with_tools_steering_and_a_subagent_records_all_of_it() {
        // The live turn exists only once the turn is spawned; the first request waits for it.
        let slot: Arc<parking_lot::Mutex<Option<Arc<parking_lot::Mutex<LiveTurn>>>>> =
            Arc::default();
        let steer = {
            let slot = Arc::clone(&slot);
            move || {
                for _ in 0..1000 {
                    if let Some(live) = slot.lock().as_ref() {
                        live.lock().injected.push_back("also check x".to_string());
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                panic!("the turn never started");
            }
        };
        let script = vec![
            Reply::new()
                .on_request(steer)
                .thinking("I should look")
                .text("Looking.")
                .call("c1", "read", json!({"path": "hello.txt"}))
                .call("c2", "shell", json!({"command": "echo hi"}))
                .usage(100, Some(40)),
            Reply::new()
                .call(
                    "c3",
                    "agent",
                    json!({"description": "Check it", "prompt": "check the file"}),
                )
                .usage(150, Some(120)),
            Reply::new().text("sub answer").usage(10, None),
            Reply::new().text("All done.").usage(200, Some(50)),
        ];
        let setup = setup(Mode::Feedback, script).await;
        let (handle, outcome, signals) = run_turn(&setup, async |handle| {
            *slot.lock() = Some(Arc::clone(&handle.live));
        })
        .await;

        assert_eq!(outcome.status, NodeStatus::Done);
        assert_eq!(outcome.error, None);
        assert_eq!(outcome.tokens_in, Some(200));
        assert_eq!(outcome.cached_in, Some(40));
        assert!(outcome.commits.is_empty());
        assert!(!outcome.reset);
        assert_eq!(outcome.session_id, None);
        let started = signals
            .iter()
            .filter(|signal| matches!(signal, AppSignal::SubagentStarted { .. }))
            .count();
        let finished = signals
            .iter()
            .filter(|signal| matches!(signal, AppSignal::SubagentFinished { .. }))
            .count();
        assert_eq!((started, finished), (1, 1));

        {
            let live = handle.live.lock();
            assert_eq!(live.reply, "All done.");
            assert_eq!(
                live.work_log,
                [
                    "Looking.",
                    "- read hello.txt",
                    "- shell echo hi",
                    "- agent: Check it"
                ]
            );
            assert_eq!(live.tokens_in, Some(200));
            assert_eq!(live.cached_in, Some(40));
            assert_eq!(live.activity, None);
            assert_eq!(live.session_id, None);
            assert!(live.prompt.ends_with("do the thing\n\nalso check x"));
        }

        assert_eq!(read_file(&setup.node_dir, "reply.md"), "All done.");
        assert_eq!(
            read_file(&setup.node_dir, "activity.md"),
            "Looking.\n- read hello.txt\n- shell echo hi\n- agent: Check it\n"
        );
        assert_eq!(
            read_file(&setup.node_dir, "prompt.md"),
            "do the thing\n\nalso check x"
        );
        let messages = transcript_of(&setup);
        let roles: Vec<&str> = messages
            .iter()
            .map(|message| message["role"].as_str().unwrap())
            .collect();
        assert_eq!(
            roles,
            [
                "user",
                "assistant",
                "tool",
                "tool",
                "user",
                "assistant",
                "tool",
                "assistant"
            ]
        );
        assert_eq!(messages[1]["content"], "Looking.");
        assert_eq!(messages[1]["tool_calls"][0]["name"], "read");
        assert_eq!(messages[1]["tool_calls"][1]["name"], "shell");
        assert_eq!(messages[2]["content"], "1\thello world\n");
        assert_eq!(messages[3]["content"], "hi\n");
        assert_eq!(messages[4]["content"], "also check x");
        assert_eq!(messages[5]["tool_calls"][0]["name"], "agent");
        assert_eq!(
            messages[6]["content"],
            "agent_id: 1970-01-01T00-00-01Z/1\n\nsub answer"
        );
        assert_eq!(messages[7]["content"], "All done.");
        assert_eq!(
            messages[0]["content"],
            format!(
                "[Note from Explorer: the state of the repository when this message was sent.]\n\n\
                 - Repository root: <root>\n- Date: 2026-11-22\n- HEAD: <head>\n\n{}\n\ndo the thing",
                crate::agent::context::WORKSPACE_NOTE
            )
        );

        let agent_dir = setup.node_dir.join("agents").join("1");
        assert_eq!(read_file(&agent_dir, "reply.md"), "sub answer");
        assert_eq!(read_file(&agent_dir, "prompt.md"), "check the file");
        let sub_messages = transcript(&agent_dir);
        assert_eq!(sub_messages.last().unwrap()["content"], "sub answer");

        let requests = setup.server.requests();
        assert_eq!(requests.len(), 4);
        assert_eq!(last_user_text(&setup, 1), "also check x");
        assert!(last_user_text(&setup, 2).ends_with("check the file"));
        assert_eq!(requests[0]["model"], "model");
        assert_eq!(requests[0]["stream"], true);
    }

    #[tokio::test]
    async fn a_cancelled_turn_keeps_what_it_streamed() {
        let script = vec![Reply::new().thinking("hm").text("partial").hang()];
        let setup = setup(Mode::Feedback, script).await;
        let (handle, outcome, _) = run_turn(&setup, async |handle| {
            for _ in 0..2000 {
                if handle.live.lock().reply == "partial" {
                    handle.cancel();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("the reply never streamed");
        })
        .await;

        // A cancelled turn with a reply stays part of the discussion.
        assert_eq!(outcome.status, NodeStatus::Cancelled);
        assert_eq!(outcome.error, None);
        assert_eq!(handle.live.lock().reply, "partial");
        assert_eq!(read_file(&setup.node_dir, "reply.md"), "partial");
        // The answer in progress is not in the transcript: only the prompt is.
        let messages = transcript_of(&setup);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
    }

    #[tokio::test]
    async fn a_failed_request_fails_the_turn_with_the_servers_message() {
        let setup = setup(Mode::Feedback, Vec::new()).await;
        let (_, outcome, _) = run_turn(&setup, async |_| {}).await;
        assert_eq!(outcome.status, NodeStatus::Failed);
        assert!(
            outcome
                .error
                .as_deref()
                .unwrap()
                .contains("the script has no reply left"),
            "{:?}",
            outcome.error
        );
    }

    #[tokio::test]
    async fn a_commit_turn_is_nudged_until_the_tree_is_clean() {
        let script = vec![
            Reply::new().call("c1", "shell", json!({"command": "echo x > new.txt"})),
            Reply::new().text("Done."),
            // The nudge: the answer above left the tree dirty.
            Reply::new().call(
                "c2",
                "shell",
                json!({"command": "git add -A && git commit -q -m 'feat: add new'"}),
            ),
            Reply::new().text("Committed."),
        ];
        let setup = setup(Mode::Commit, script).await;
        let (handle, outcome, _) = run_turn(&setup, async |_| {}).await;

        assert_eq!(outcome.status, NodeStatus::Done);
        assert_eq!(outcome.commits.len(), 1);
        assert!(!outcome.reset);
        assert_eq!(
            outcome.head_after.as_deref(),
            outcome.commits.last().map(String::as_str)
        );
        assert_eq!(
            last_user_text(&setup, 2),
            "The working tree still has uncommitted changes:\n\n?? new.txt\n\nCommit them now."
        );
        assert_eq!(setup.server.requests().len(), 4);
        // The nudge is recorded in the transcript but never shown.
        let messages = transcript_of(&setup);
        assert!(messages.iter().any(|message| {
            message["content"]
                .as_str()
                .is_some_and(|text| text.starts_with("The working tree still has"))
        }));
        assert_eq!(handle.live.lock().reply, "Done.\n\nCommitted.");
        assert_eq!(
            read_file(&setup.node_dir, "reply.md"),
            "Done.\n\nCommitted."
        );
    }
}
