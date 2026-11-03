use super::llm::{self, LlmTarget};
use super::subagent;
use super::tools::{self, ToolContext};
use super::turn::Recorder;
use super::{Message, ToolCall};
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::pin::Pin;
use std::time::{Duration, Instant};

/// Threshold for detecting excessive Chinese characters (0.0 to 1.0).
const CHINESE_DETECTION_THRESHOLD: f32 = 0.5;

/// Checks if text contains excessive Chinese characters (CJK Unified Ideographs and
/// related blocks). Returns true if 50% or more of characters are Chinese.
fn has_excessive_chinese(text: &str) -> bool {
    let total_chars: u32 = text.chars().count() as u32;
    if total_chars == 0 {
        return false;
    }
    let chinese_chars = text
        .chars()
        .filter(|c| {
            matches!(
                c,
                '\u{4E00}'..='\u{9FFF}'  // CJK Unified Ideographs
                | '\u{3400}'..='\u{4DBF}'  // CJK Extension A
                | '\u{F900}'..='\u{FAFF}'  // CJK Compatibility Ideographs
            )
        })
        .count() as u32;
    (chinese_chars as f32 / total_chars as f32) > CHINESE_DETECTION_THRESHOLD
}

/// What the loop tells a run nearing its time budget.
#[derive(Debug, PartialEq, Eq)]
enum Nudge {
    /// Sent once, as the budget nears its end.
    Soft,
    /// Sent every round past the budget.
    Firm,
}

/// The wrap-up nudge of a run on a time budget: the soft one once at `budget - min(2
/// minutes, half the budget)`, then the firm one every round past the budget.
fn time_nudge(elapsed: Duration, budget: Duration, warned: bool) -> Option<Nudge> {
    if elapsed >= budget {
        return Some(Nudge::Firm);
    }
    let lead = Duration::from_secs(120).min(budget / 2);
    (!warned && elapsed >= budget - lead).then_some(Nudge::Soft)
}

fn nudge_text(nudge: Nudge, elapsed: Duration, budget: Duration) -> String {
    let (elapsed, budget) = (elapsed.as_secs() / 60, budget.as_secs() / 60);
    match nudge {
        Nudge::Soft => format!(
            "[Note from Explorer: you have been running for {elapsed}m of the {budget}m \
             you were given. Time is nearly up: finish the step you are on and write \
             your final answer, saying plainly what you did and what remains undone.]"
        ),
        Nudge::Firm => format!(
            "[Note from Explorer: you have been running for {elapsed}m of the {budget}m \
             you were given. Do not start anything new. Write your final answer now, \
             saying plainly what you did and what remains undone. An unfinished task \
             reported clearly is worth more than a finished one reported late.]"
        ),
    }
}

/// What bounds one run of the loop.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Start and duration of the run's time budget; a turn has none.
    pub budget: Option<(Instant, Duration)>,
    /// Times one call may return the same result before the loop steps in; 0 is off.
    pub repeat_limit: u32,
}

/// serde_json sorts the keys of the arguments, so equal arguments print alike.
type Signature = (String, String, u64);

fn signature(call: &ToolCall, result: &str) -> Signature {
    let mut hasher = DefaultHasher::new();
    result.hash(&mut hasher);
    (
        call.name.clone(),
        call.arguments.to_string(),
        hasher.finish(),
    )
}

/// What the stall detector asks of the loop.
#[derive(Debug, PartialEq, Eq)]
enum Stall {
    /// A call returned the same result `limit` times: tell the model.
    Nudge,
    /// A call already warned about did it again `limit` times: stop the run.
    Fail,
}

/// Flags a call that keeps returning the same result; a cycle of calls counts too.
struct StallDetector {
    limit: usize,
    window: VecDeque<Signature>,
    warned: HashSet<Signature>,
}

impl StallDetector {
    fn new(limit: u32) -> Self {
        Self {
            limit: limit as usize,
            window: VecDeque::new(),
            warned: HashSet::new(),
        }
    }

    fn record(&mut self, signature: Signature) -> Option<Stall> {
        if self.limit == 0 {
            return None;
        }
        if self.window.len() == 2 * self.limit {
            self.window.pop_front();
        }
        self.window.push_back(signature.clone());
        let count = self
            .window
            .iter()
            .filter(|seen| **seen == signature)
            .count();
        if count < self.limit {
            return None;
        }
        self.window.clear();
        Some(if self.warned.insert(signature) {
            Stall::Nudge
        } else {
            Stall::Fail
        })
    }
}

/// The call as the model sent it, not as it meant to send it.
fn quoted_call(call: &ToolCall) -> String {
    format!("{} {}", call.name, call.arguments)
}

fn stall_text(call: &ToolCall, limit: u32) -> String {
    format!(
        "[Note from Explorer: the call `{}` has now returned the same result {limit} \
         times. These are the arguments you sent, whatever you meant to send. Repeating \
         it will not change the outcome: send different arguments, take another \
         approach, or write your answer.]",
        quoted_call(call)
    )
}

fn stall_error(call: &ToolCall, limit: u32) -> String {
    format!(
        "the call `{}` returned the same result {limit} more times after Explorer \
         warned about it; the run was stopped",
        quoted_call(call)
    )
}

/// Repairs a transcript that ends inside a round: an assistant message asking for
/// tool calls whose results never arrived, because the turn died mid-stream or
/// mid-tool. The calls are answered with a note, so replaying the transcript is a
/// valid sequence that shows where the work stopped.
pub fn repair_tail(messages: &mut Vec<Message>) {
    let Some(Message::Assistant { tool_calls, .. }) = messages.last_mut() else {
        return;
    };
    if tool_calls.is_empty() {
        return;
    }
    let calls = std::mem::take(tool_calls);
    for call in calls {
        messages.push(Message::Tool {
            call_id: call.id,
            name: call.name,
            content: "[interrupted before this call ran]".to_string(),
        });
    }
}

/// Our own agent loop, until an answer asks for no tool; boxed, as sub-agents recurse into it.
pub fn run<'a>(
    context: &'a ToolContext,
    target: &'a LlmTarget,
    session: &'a str,
    system: &'a str,
    messages: &'a mut Vec<Message>,
    recorder: &'a mut Recorder,
    limits: Limits,
) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
    Box::pin(run_loop(
        context, target, session, system, messages, recorder, limits,
    ))
}

async fn run_loop(
    context: &ToolContext,
    target: &LlmTarget,
    session: &str,
    system: &str,
    messages: &mut Vec<Message>,
    recorder: &mut Recorder,
    limits: Limits,
) -> Result<(), String> {
    let tool_specs = tools::specs(context);
    let mut warned = false;
    let mut stalls = StallDetector::new(limits.repeat_limit);
    loop {
        // What the user sent while the turn was working joins here, after the tool
        // results of the round before: the next request carries it.
        let injector = recorder.injector();
        for content in injector.take() {
            messages.push(injector.sent(&content));
        }
        if let Some((started, limit)) = limits.budget {
            let elapsed = started.elapsed();
            if let Some(nudge) = time_nudge(elapsed, limit, warned) {
                warned |= nudge == Nudge::Soft;
                let wrap_up = Message::User {
                    content: nudge_text(nudge, elapsed, limit),
                    injected: true,
                };
                recorder.message(&wrap_up);
                messages.push(wrap_up);
            }
        }
        recorder.begin_answer();
        let response = match llm::stream_chat(
            target,
            session,
            system,
            messages,
            &tool_specs,
            |is_thinking, text| {
                if is_thinking {
                    recorder.reasoning(text);
                } else {
                    recorder.text(text);
                }
            },
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                // Whatever streamed before the failure joins the transcript as the
                // incomplete answer it stands for; a /continue replay shows the model
                // where it stopped, instead of making it redo the round from scratch.
                recorder.flush_partial_answer();
                return Err(error);
            }
        };
        if let Some(tokens) = response.tokens_in {
            recorder.tokens_in(tokens);
        }
        recorder.cached_in(response.cached_in);

        // Detect if response contains excessive Chinese before moving the text.
        let detected_chinese = has_excessive_chinese(&response.text);

        let answer = Message::Assistant {
            content: response.text,
            tool_calls: response.tool_calls.clone(),
        };
        recorder.message(&answer);
        messages.push(answer);

        // Inject correction if excessive Chinese was detected.
        if detected_chinese {
            let correction = Message::User {
                content: "Continue responding in English only. Your previous response contained too much non-English text.".to_string(),
                injected: true,
            };
            recorder.message(&correction);
            messages.push(correction);
        }

        if response.tool_calls.is_empty() {
            // An answer that calls no tools ends the turn, unless something the user
            // sent arrived while it streamed: that keeps the loop going.
            if !injector.pending() {
                return Ok(());
            }
            continue;
        }
        recorder.answer_calls_tools();
        // Acted on once every call of the round has its result.
        let mut failed: Option<&ToolCall> = None;
        let mut nudged: Vec<&ToolCall> = Vec::new();
        for call in &response.tool_calls {
            recorder.tool_call(tools::describe(call));
            let content = match &context.subagents {
                Some(subagents) if call.name == subagent::TOOL => {
                    subagent::run(context, subagents, call, recorder).await
                }
                _ => tools::run(context, call).await,
            };
            match stalls.record(signature(call, &content)) {
                Some(Stall::Fail) => failed = Some(call),
                Some(Stall::Nudge) => nudged.push(call),
                None => {}
            }
            let result = Message::Tool {
                call_id: call.id.clone(),
                name: call.name.clone(),
                content,
            };
            recorder.message(&result);
            messages.push(result);
        }
        recorder.activity(None);
        if let Some(call) = failed {
            return Err(stall_error(call, limits.repeat_limit));
        }
        // Each stuck call gets its own note, since the detector has already warned it.
        for call in nudged {
            let nudge = Message::User {
                content: stall_text(call, limits.repeat_limit),
                injected: true,
            };
            recorder.message(&nudge);
            messages.push(nudge);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        super::ToolCall, Nudge, Stall, StallDetector, nudge_text, repair_tail, signature,
        stall_text, time_nudge,
    };
    use crate::agent::Message;
    use std::time::Duration;

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c".into(),
            name: name.into(),
            arguments,
        }
    }

    /// Feeds the detector `times` copies of one call and result; what the last asked.
    fn repeat(
        detector: &mut StallDetector,
        call: &ToolCall,
        result: &str,
        times: usize,
    ) -> Vec<Option<Stall>> {
        (0..times)
            .map(|_| detector.record(signature(call, result)))
            .collect()
    }

    #[test]
    fn the_same_call_with_new_results_is_progress() {
        let mut detector = StallDetector::new(5);
        let build = call("shell", serde_json::json!({"command": "cargo build"}));
        for attempt in 0..20 {
            let result = format!("error E0382 at line {attempt}");
            assert_eq!(detector.record(signature(&build, &result)), None);
        }
    }

    #[test]
    fn a_stuck_call_is_nudged_then_fails_when_it_repeats_after_the_nudge() {
        let mut detector = StallDetector::new(5);
        let shot = call("screenshot", serde_json::json!({"save_path": null}));
        let seen = repeat(&mut detector, &shot, "saved_to: null", 5);
        assert_eq!(seen[..4], [None, None, None, None]);
        assert_eq!(seen[4], Some(Stall::Nudge));

        // The grace period: as many fresh repeats again before the run fails.
        let seen = repeat(&mut detector, &shot, "saved_to: null", 5);
        assert_eq!(seen[..4], [None, None, None, None]);
        assert_eq!(seen[4], Some(Stall::Fail));
    }

    #[test]
    fn another_stuck_call_after_a_nudge_gets_its_own_nudge() {
        let mut detector = StallDetector::new(5);
        let first = call(
            "screenshot",
            serde_json::json!({"pixels_per_point": 1, "save_path": null}),
        );
        let second = call("screenshot", serde_json::json!({"save_path": null}));
        assert_eq!(repeat(&mut detector, &first, "r", 5)[4], Some(Stall::Nudge));
        assert_eq!(
            repeat(&mut detector, &second, "r", 5)[4],
            Some(Stall::Nudge)
        );
        assert_eq!(repeat(&mut detector, &first, "r", 5)[4], Some(Stall::Fail));
    }

    #[test]
    fn a_cycle_between_two_stuck_calls_counts() {
        let mut detector = StallDetector::new(5);
        let a = call("read", serde_json::json!({"path": "a"}));
        let b = call("read", serde_json::json!({"path": "b"}));
        let mut seen = Vec::new();
        for _ in 0..5 {
            seen.push(detector.record(signature(&a, "same a")));
            seen.push(detector.record(signature(&b, "same b")));
        }
        assert_eq!(seen[8], Some(Stall::Nudge));
        assert!(seen[..8].iter().all(Option::is_none));
    }

    #[test]
    fn a_limit_of_zero_never_steps_in() {
        let mut detector = StallDetector::new(0);
        let shot = call("screenshot", serde_json::json!({}));
        assert!(
            repeat(&mut detector, &shot, "r", 50)
                .iter()
                .all(Option::is_none)
        );
    }

    #[test]
    fn a_stall_note_quotes_the_arguments_as_sent() {
        let shot = call("screenshot", serde_json::json!({"save_path": null}));
        let text = stall_text(&shot, 5);
        assert!(text.contains("`screenshot {\"save_path\":null}`"));
        assert!(text.contains("same result 5 times"));
    }

    #[test]
    fn a_budget_warns_once_then_nags_every_round_past_it() {
        let budget = Duration::from_secs(900);
        assert_eq!(time_nudge(Duration::from_secs(779), budget, false), None);
        assert_eq!(
            time_nudge(Duration::from_secs(780), budget, false),
            Some(Nudge::Soft)
        );
        assert_eq!(time_nudge(Duration::from_secs(781), budget, true), None);
        assert_eq!(
            time_nudge(Duration::from_secs(900), budget, true),
            Some(Nudge::Firm)
        );
        assert_eq!(
            time_nudge(Duration::from_secs(1200), budget, false),
            Some(Nudge::Firm)
        );

        // A budget under four minutes warns halfway instead.
        let small = Duration::from_secs(60);
        assert_eq!(time_nudge(Duration::from_secs(29), small, false), None);
        assert_eq!(
            time_nudge(Duration::from_secs(30), small, false),
            Some(Nudge::Soft)
        );
    }

    #[test]
    fn a_nudge_says_how_long_the_run_has_been_going_and_how_much_it_got() {
        let budget = Duration::from_secs(900);
        let soft = nudge_text(Nudge::Soft, Duration::from_secs(780), budget);
        assert!(soft.contains("13m of the 15m"));
        assert!(soft.contains("what remains undone"));
        let firm = nudge_text(Nudge::Firm, Duration::from_secs(960), budget);
        assert!(firm.contains("16m of the 15m"));
        assert!(firm.contains("Do not start anything new"));
    }

    #[test]
    fn a_tail_left_inside_a_round_gets_synthetic_tool_results() {
        let mut messages = vec![
            Message::User {
                content: "hi".into(),
                injected: false,
            },
            Message::Assistant {
                content: "looking".into(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                }],
            },
        ];
        repair_tail(&mut messages);
        assert_eq!(
            messages[2..],
            [Message::Tool {
                call_id: "c1".into(),
                name: "read".into(),
                content: "[interrupted before this call ran]".into(),
            }]
        );

        // A valid tail is left alone.
        let mut done = vec![Message::Assistant {
            content: "done".into(),
            tool_calls: Vec::new(),
        }];
        repair_tail(&mut done);
        assert_eq!(done.len(), 1);
    }
}
