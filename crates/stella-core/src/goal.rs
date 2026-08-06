//! The goal loop (`/goal`, `stella goal`): keep running working turns
//! until an independent verifier model assesses the goal as met.
//!
//! Structure per round: one full [`Engine::run_turn`] (the worker doing
//! real work), then one verifier call assessing the transcript against the
//! goal. `met == true` ends the loop; `met == false` feeds the verifier's
//! feedback back to the worker as the next user message, so every round
//! starts with concrete course-correction rather than a bare "try again".
//!
//! # Bounded by construction
//!
//! "Don't stop until the verifier says the goal is met" still terminates on
//! three backstops, each with a named reason in [`GoalOutcome::Unmet`]:
//! the round cap ([`GoalConfig::max_rounds`]), the budget (worker and verifier
//! spend both accumulate on the guard's session axis, which
//! [`BudgetGuard::begin_turn`] never resets — so a configured cap bounds
//! the whole loop's total, not each round in isolation, and the verifier
//! cannot be out-spent by hiding cost in assessments), and a turn abort
//! (loop detection, retry exhaustion, step cap — anything that ends a turn
//! uncleanly ends the goal loop too, never silently retried).
//!
//! # Graceful degradation
//!
//! A verifier that fails its call after retries ends the loop as `Unmet`
//! with the provider error in the reason — the loop never spins unjudged
//! work while the verifier is down, and never fabricates a verdict. A verifier
//! that answers but with unparseable output is treated as "not met" with
//! generic feedback (one bad verifier response wastes a round, not the run;
//! the round cap bounds the damage).
//!
//! # Telemetry
//!
//! Every verifier call emits `Stage(Verifier)`, then the verifier turn's own metering
//! (its `StepUsage` + `BudgetTick`, forwarded like any sub-agent's), then
//! `GoalVerdict` carrying the verifier call's own `cost_usd` — so a metering
//! consumer sees verifier spend with the same fidelity as worker spend, and the
//! verdict stream reconstructs the goal loop's arc without any out-of-band
//! state.

use std::borrow::Cow;

use serde::Deserialize;
use stella_protocol::{AgentEvent, CompletionMessage, MessageRole, Provider, StageKind};
use tokio::sync::mpsc::UnboundedSender;

use crate::budget::BudgetGuard;
use crate::driver::{Engine, TurnOutcome};
use crate::subagent::{SubAgentHost, SubAgentOutcome, SubAgentSpec, truncate_chars};

/// Tuning for [`Engine::run_goal`]. `Default` is sized for interactive
/// use: enough rounds to converge on a real goal, small enough that a
/// verifier stuck on "not met" can't run away with the session.
#[derive(Debug, Clone)]
pub struct GoalConfig {
    /// Hard cap on working rounds (worker turn + verifier assessment pairs).
    pub max_rounds: usize,
    /// Output-token cap for verifier calls — a verdict is small by design.
    pub verifier_max_output_tokens: Option<u32>,
    /// Cap on transcript characters shown to the verifier, tail-biased (the
    /// most recent work is what decides "met"), and always on a char
    /// boundary.
    pub verifier_transcript_chars: usize,
}

impl Default for GoalConfig {
    fn default() -> Self {
        Self {
            max_rounds: 8,
            verifier_max_output_tokens: Some(1024),
            verifier_transcript_chars: 24_000,
        }
    }
}

/// How a goal loop ended.
#[derive(Debug, Clone, PartialEq)]
pub enum GoalOutcome {
    /// The verifier assessed the goal as met.
    Met {
        rounds: usize,
        /// The verifier's reasoning for the final, passing verdict.
        verdict: String,
        /// Total spend across all rounds — worker turns and verifier calls.
        cost_usd: f64,
    },
    /// The loop ended without a passing verdict: round cap, budget, a turn
    /// abort, or an unreachable verifier. Never silent — `reason` names which.
    Unmet {
        rounds: usize,
        reason: String,
        cost_usd: f64,
    },
}

/// What the verifier model must return, as strict JSON. `reasoning` explains
/// the verdict; `feedback` (only meaningful when `met == false`) is
/// actionable course-correction handed to the worker verbatim.
#[derive(Debug, Deserialize, Clone)]
pub struct GoalVerifierVerdict {
    pub met: bool,
    #[serde(default)]
    pub reasoning: String,
    #[serde(default)]
    pub feedback: String,
}

/// The kickoff message every goal loop opens with.
///
/// Shared by [`Engine::run_goal`], the CLI's staged-pipeline goal loop, and
/// `stella-serve`'s `drive_goal`: a served goal round and a CLI goal round
/// must put byte-identical words in front of the model or they are two
/// features wearing one name. All three surfaces used to carry their own
/// copy of this string with a comment promising exactly that — this function
/// is the promise made structural.
#[must_use]
pub fn goal_kickoff_text(goal: &str) -> String {
    format!(
        "GOAL: {goal}\n\nWork toward this goal. An independent verifier will assess the \
         result after each working round from the transcript evidence; keep your work \
         verifiable (run tests, show outputs)."
    )
}

/// The message that carries a verifier's "not yet" back to the worker — the
/// other half of the [`goal_kickoff_text`] contract, shared by the same
/// three goal loops.
///
/// An empty `feedback` field falls back to the verdict's `reasoning` (the
/// verifier explained *why* even when it offered no next action); the fallback
/// lives here so no surface re-implements it differently.
#[must_use]
pub fn verifier_feedback_text(goal: &str, verdict: &GoalVerifierVerdict) -> String {
    let feedback = if verdict.feedback.trim().is_empty() {
        &verdict.reasoning
    } else {
        &verdict.feedback
    };
    format!(
        "The verifier assessed the goal as NOT yet met.\nVerifier feedback: {feedback}\n\n\
         Continue working toward the goal: {goal}"
    )
}

/// The receipt turn-slot offset of `round`'s worker calls: worker rounds
/// take even slots (0, 2, 4, …) and each round's verifier takes the odd slot
/// beside them ([`Engine::assess`] adds the `+ 1`), so no two model calls in
/// a goal arc share a `(turn_instance, step, call_seq)` receipt key within
/// one execution.
///
/// Shared by the same three goal loops as [`goal_kickoff_text`] — a surface
/// that re-derived the slot math differently would silently overwrite a
/// sibling round's step manifests in the store.
#[must_use]
pub fn goal_round_turn_offset(round: usize) -> u32 {
    u32::try_from(round.saturating_sub(1).saturating_mul(2)).unwrap_or(u32::MAX)
}

/// Public so the CLI's goal loop can pin its tool allowlist against the six
/// tools this prompt names (#1783): the prompt and the offered surface must
/// not drift apart, and the test that enforces that lives beside the
/// executor it guards.
pub const VERIFIER_SYSTEM_PROMPT: &str = "You are an impartial verifier assessing whether a coding agent \
     has fully met a stated goal. Judge from EVIDENCE, never from claims: use your read-only \
     tools (read_file, grep, glob, explorations, ci_status, search_issues) to verify the work \
     directly whenever the transcript alone is not conclusive — read the changed files, check \
     the tests exist, inspect CI. Claimed success without supporting evidence is NOT met. The \
     strongest completion evidence is a `verify_done` tool result reading WITNESS CONFIRMED \
     (the change's test fails on the previous code and passes on the new code); a merely \
     green test suite is weak evidence, since it cannot distinguish real work from vacuous \
     tests or unwired code. If you need something only the worker can provide (a trace, a \
     screenshot, a system log, an explanation), set met:false and put the request in \
     feedback — the worker acts on it next round. When decided, end your reply with ONLY a \
     JSON object, no prose after it:\n\
     {\"met\": true|false, \"reasoning\": \"why, in one or two sentences\", \
     \"feedback\": \"if not met: the single most useful next action or evidence request\"}";

impl Engine<'_> {
    /// Drive working turns until `verifier` assesses `goal` as met, or a
    /// backstop ends the loop (see module docs). Appends to `messages`
    /// like [`Engine::run_turn`] does, so the caller's conversation
    /// history contains the full goal arc — including verifier feedback
    /// messages — afterward.
    pub async fn run_goal(
        &self,
        verifier: &dyn Provider,
        goal: &str,
        messages: &mut Vec<CompletionMessage>,
        budget: &mut BudgetGuard,
        events: &UnboundedSender<AgentEvent>,
        goal_config: &GoalConfig,
    ) -> GoalOutcome {
        let starting_cost_usd = budget.session_spent_usd();
        messages.push(CompletionMessage::user(goal_kickoff_text(goal)));

        for round in 1..=goal_config.max_rounds {
            budget.begin_turn();
            // Each round is its own turn for receipt purposes — see
            // `goal_round_turn_offset` for the even/odd slot rule.
            let round_offset = goal_round_turn_offset(round);
            let round_engine =
                self.with_turn_instance(self.config.turn_instance.saturating_add(round_offset));
            match round_engine.run_turn(messages, budget, events).await {
                TurnOutcome::Completed { .. } => {}
                TurnOutcome::Aborted { reason, .. } => {
                    return GoalOutcome::Unmet {
                        rounds: round,
                        reason: format!("working turn aborted: {reason}"),
                        cost_usd: budget.session_spent_usd() - starting_cost_usd,
                    };
                }
            }

            let _ = events.send(AgentEvent::Stage {
                name: StageKind::Verdict,
            });
            let (verdict, verifier_cost) = match round_engine
                .assess(verifier, goal, messages, budget, events, goal_config)
                .await
            {
                Ok(pair) => pair,
                Err(reason) => {
                    // Verifier unreachable (or its turn hit the budget) after
                    // retries: stop rather than loop unjudged (module docs,
                    // graceful degradation). Spend was already recorded by
                    // the verifier turn itself.
                    return GoalOutcome::Unmet {
                        rounds: round,
                        reason: format!("verifier unavailable: {reason}"),
                        cost_usd: budget.session_spent_usd() - starting_cost_usd,
                    };
                }
            };

            // Verifier spend was metered inside its own engine turn
            // (StepUsage + BudgetTick already emitted; recorded against the
            // shared budget guard). The verdict event carries this call's
            // own cost.
            let _ = events.send(AgentEvent::GoalVerdict {
                round,
                met: verdict.met,
                reasoning: verdict.reasoning.clone(),
                cost_usd: verifier_cost,
            });

            if verdict.met {
                return GoalOutcome::Met {
                    rounds: round,
                    verdict: verdict.reasoning,
                    cost_usd: budget.session_spent_usd() - starting_cost_usd,
                };
            }

            messages.push(CompletionMessage::user(verifier_feedback_text(
                goal, &verdict,
            )));
        }

        GoalOutcome::Unmet {
            rounds: goal_config.max_rounds,
            reason: format!(
                "round cap ({}) reached without a passing verdict",
                goal_config.max_rounds
            ),
            cost_usd: budget.session_spent_usd() - starting_cost_usd,
        }
    }

    /// One verifier assessment, run as a **sub-agent** (`crate::subagent`): the
    /// verifier sees the goal + transcript tail in its own private transcript
    /// and may gather its own evidence through a read-only view of the SAME
    /// tool registry the worker used (read files, grep, check CI) —
    /// structurally unable to mutate the workspace it is judging. Spend is
    /// carved from `budget` and settles back into it. `Err` carries the
    /// abort reason (provider failure or budget) after retries were
    /// exhausted.
    ///
    /// # Why this is the primitive and not a hand-rolled engine
    ///
    /// It used to build its own [`Engine`] with [`Engine::with_sleeper`],
    /// which cannot carry the pause gate, steering, or lifecycle hooks —
    /// those are builder-set private fields. So a paused session kept
    /// spending inside the verifier, a soft stop could not end a verifier turn,
    /// and `PreToolUse`/`PostToolUse` hooks never fired for the evidence
    /// the verifier gathered. Routing through the sub-agent primitive carries
    /// all three, and the next nested turn someone adds inherits the fix
    /// rather than repeating the bug.
    ///
    /// # The verifier's evidence never enters the goal transcript
    ///
    /// Whatever the verifier reads to reach its verdict lives and dies in the
    /// child's transcript. Only the verdict crosses back — which is why a
    /// verifier can afford to be thorough without every later worker round
    /// paying to re-send its evidence.
    pub async fn assess(
        &self,
        verifier: &dyn Provider,
        goal: &str,
        messages: &[CompletionMessage],
        budget: &mut BudgetGuard,
        events: &UnboundedSender<AgentEvent>,
        goal_config: &GoalConfig,
    ) -> Result<(GoalVerifierVerdict, f64), String> {
        let transcript = render_transcript_tail(messages, goal_config.verifier_transcript_chars);
        // The odd slot beside this engine's turn: the verifier's own step loop
        // restarts at step 0 with call_seq 0, so without a turn of its own
        // its manifests would overwrite the worker receipts they are meant
        // to sit beside.
        let turn_instance = self.config.turn_instance.saturating_add(1);
        let spec = SubAgentSpec {
            agent_id: format!("verifier-{turn_instance}"),
            system_prompt: Some(VERIFIER_SYSTEM_PROMPT.to_string()),
            instruction: format!(
                "GOAL:\n{goal}\n\nAGENT TRANSCRIPT (most recent last):\n{transcript}\n\n\
                 Has the goal been fully met? Verify with your tools where the transcript \
                 is not conclusive."
            ),
            // A verdict needs a handful of evidence lookups, not a work
            // session.
            max_steps: 8,
            max_output_tokens: goal_config.verifier_max_output_tokens,
            temperature: Some(0.0),
            // Sized so a verdict is never clipped: a clipped JSON object
            // fails [`parse_verdict`] and silently costs a round. The
            // verifier's answer is already token-capped above, so this only
            // has to stay clear of that bound.
            max_report_chars: goal_config
                .verifier_max_output_tokens
                .map_or(VERDICT_REPORT_CHARS, |tokens| {
                    (tokens as usize).saturating_mul(CHARS_PER_TOKEN_CEILING)
                }),
            budget_usd: None,
            write_access: false,
            role: stella_protocol::ModelCallRole::Verdict,
            turn_instance,
            depth: 1,
            compaction_budget_tokens: None,
        };

        match self
            .run_sub_agent(SubAgentHost::new(verifier), &spec, budget, events)
            .await
        {
            SubAgentOutcome::Completed(report) => {
                let verdict =
                    parse_verdict(&report.summary).unwrap_or_else(|| GoalVerifierVerdict {
                        met: false,
                        reasoning: "verifier response was not parseable JSON — treated as not met"
                            .into(),
                        feedback: format!(
                            "Continue working toward the goal; the previous assessment was \
                         unreadable (verifier said: {})",
                            truncate_chars(&report.summary, 500)
                        ),
                    });
                Ok((verdict, report.cost_usd))
            }
            // A partial judgement is not a judgement. The loop's contract is
            // that it never fabricates a verdict, so salvaged text is
            // deliberately NOT parsed here — an assessment that did not
            // finish ends the loop with its reason instead.
            SubAgentOutcome::Incomplete { reason, .. } | SubAgentOutcome::Refused { reason } => {
                Err(reason)
            }
        }
    }
}

/// Report cap for a verifier whose output tokens are unbounded — generous,
/// since the only thing it protects against is a runaway verdict.
const VERDICT_REPORT_CHARS: usize = 32_000;

/// Chars-per-token upper bound used to turn an output-token cap into a
/// character cap. Deliberately above any real tokenizer's ratio so the
/// derived cap can never be the thing that clips a verdict.
const CHARS_PER_TOKEN_CEILING: usize = 8;

/// Extract the verdict from verifier output that may wrap its JSON in prose or
/// a code fence.
///
/// Parsed from the END, matching `VERIFIER_SYSTEM_PROMPT`'s own contract that
/// the JSON object terminates the reply: candidate `{` offsets are tried
/// LAST-first against the final `}`, and the first span that parses wins.
/// The old outermost-span rule (`first '{' ..= last '}'`) rejected any reply
/// whose earlier prose contained a brace — quoted Rust (`if x { … }`), a
/// cited JSON tool result — silently converting a verifier's `met: true` into
/// "not parseable → not met" and, at temperature 0, deterministically
/// re-converting it every round until the round cap scored a solved goal as
/// `Unmet`.
fn parse_verdict(text: &str) -> Option<GoalVerifierVerdict> {
    let end = text.rfind('}')?;
    text.match_indices('{')
        .rev()
        .filter(|&(start, _)| start < end)
        .find_map(|(start, _)| serde_json::from_str(&text[start..=end]).ok())
}

/// Render the conversation for the verifier: role-labeled, tool activity
/// summarized, tail-biased to `max_chars` on message boundaries (the verifier
/// needs the most recent evidence, and a truncated JSON blob mid-message
/// would read as agent malfunction).
fn render_transcript_tail(messages: &[CompletionMessage], max_chars: usize) -> String {
    let mut rendered: Vec<String> = Vec::with_capacity(messages.len());
    for message in messages {
        if message.role == MessageRole::System {
            continue;
        }
        let mut block = String::new();
        let label = match message.role {
            MessageRole::User => "USER",
            MessageRole::Assistant => "ASSISTANT",
            MessageRole::Tool => "TOOL RESULTS",
            MessageRole::System => unreachable!("filtered above"),
        };
        block.push_str(label);
        block.push_str(": ");
        if !message.content.is_empty() {
            block.push_str(&message.content);
        }
        for call in &message.tool_calls {
            block.push_str(&format!(
                "\n  [called {}({})]",
                call.name,
                truncate_chars(&call.input.to_string(), 200)
            ));
        }
        for result in &message.tool_results {
            let (status, body) = match &result.output {
                stella_protocol::ToolOutput::Ok { content } => ("ok", content),
                stella_protocol::ToolOutput::Error { message } => ("error", message),
            };
            block.push_str(&format!(
                "\n  [{} {}: {}]",
                result.call_id,
                status,
                truncate_chars(body, 400)
            ));
        }
        rendered.push(block);
    }

    // Take whole blocks from the end until the budget is spent. The newest
    // block is always kept — the verifier needs *some* evidence — but it is
    // truncated when it alone exceeds the budget, so a single oversized
    // assistant message can no longer push the verifier prompt arbitrarily
    // past `max_chars` and force the verifier's own turn to compact it.
    let mut kept: Vec<Cow<'_, str>> = Vec::new();
    let mut used = 0usize;
    for block in rendered.iter().rev() {
        let cost = block.chars().count() + 2;
        if used + cost > max_chars {
            if kept.is_empty() {
                kept.push(Cow::Owned(truncate_chars(block, max_chars)));
            }
            break;
        }
        used += cost;
        kept.push(Cow::Borrowed(block.as_str()));
    }
    kept.reverse();
    kept.join("\n\n")
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    use async_trait::async_trait;
    use serde_json::Value;
    use stella_protocol::event::BudgetMode;
    use stella_protocol::{
        CompletionRequestRef, CompletionResult, CompletionUsage, ModelCallRole, ProviderError,
        ToolOutput, ToolSchema,
    };
    use tokio::sync::mpsc;

    use super::*;
    use crate::driver::EngineConfig;
    use crate::ports::ToolExecutor;
    use crate::retry::Sleeper;

    /// Sleeper that never really sleeps — goal tests run instantly.
    struct NoSleep;
    #[async_trait]
    impl Sleeper for NoSleep {
        async fn sleep(&self, _duration_ms: u64) {}
    }

    /// A provider that returns a fixed sequence of results, then errors.
    struct ScriptedProvider {
        script: Mutex<Vec<Result<CompletionResult, ProviderError>>>,
        calls: AtomicU32,
    }

    impl ScriptedProvider {
        fn new(script: Vec<Result<CompletionResult, ProviderError>>) -> Self {
            Self {
                script: Mutex::new(script),
                calls: AtomicU32::new(0),
            }
        }
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        fn id(&self) -> &str {
            "scripted"
        }

        async fn complete_ref(
            &self,
            _request: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut script = self.script.lock().unwrap();
            if script.is_empty() {
                return Err(ProviderError::transport("script exhausted"));
            }
            script.remove(0)
        }
    }

    struct NoTools;
    #[async_trait]
    impl ToolExecutor for NoTools {
        fn schemas(&self) -> Vec<ToolSchema> {
            Vec::new()
        }
        async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
            ToolOutput::Ok {
                content: String::new(),
            }
        }
    }

    fn text_result(text: &str, cost: f64) -> CompletionResult {
        CompletionResult {
            text: text.into(),
            tool_calls: vec![],
            usage: CompletionUsage {
                reported: true,
                ..CompletionUsage::default()
            },
            model: "scripted".into(),
            cost_usd: cost,
            finish_reason: None,
        }
    }

    fn collect_events(
        mut rx: mpsc::UnboundedReceiver<AgentEvent>,
    ) -> impl FnOnce() -> Vec<AgentEvent> {
        move || {
            let mut events = Vec::new();
            while let Ok(event) = rx.try_recv() {
                events.push(event);
            }
            events
        }
    }

    /// Receipts key on `(execution_id, turn_instance, step, call_seq)` and
    /// every turn restarts `step` at 0 — so inside one goal execution, each
    /// worker round and each verifier assessment must claim its own
    /// `turn_instance` (worker even, verifier odd) or later manifests silently
    /// overwrite earlier ones in the store. This pins the slot assignment.
    #[tokio::test]
    async fn goal_rounds_and_verifiers_never_share_a_receipt_key() {
        let worker = ScriptedProvider::new(vec![
            Ok(text_result("attempt one", 0.01)),
            Ok(text_result("attempt two", 0.01)),
        ]);
        let verifier = ScriptedProvider::new(vec![
            Ok(text_result(
                r#"{"met": false, "reasoning": "not yet", "feedback": "keep going"}"#,
                0.001,
            )),
            Ok(text_result(
                r#"{"met": true, "reasoning": "done", "feedback": ""}"#,
                0.001,
            )),
        ]);
        let tools = NoTools;
        let engine = Engine::with_sleeper(&worker, &tools, EngineConfig::default(), &NoSleep);
        let mut messages = vec![CompletionMessage::system("sys")];
        let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
        let (tx, rx) = mpsc::unbounded_channel();
        let drain = collect_events(rx);

        let outcome = engine
            .run_goal(
                &verifier,
                "make the tests pass",
                &mut messages,
                &mut budget,
                &tx,
                &GoalConfig::default(),
            )
            .await;
        drop(tx);
        assert!(matches!(outcome, GoalOutcome::Met { rounds: 2, .. }));

        let manifests: Vec<(u32, usize, u64)> = drain()
            .into_iter()
            .filter_map(|event| match event {
                AgentEvent::StepManifest {
                    turn_instance,
                    step,
                    call_seq,
                    ..
                } => Some((turn_instance, step, call_seq)),
                _ => None,
            })
            .collect();
        // Two worker rounds + two verifier turns, one committed step each:
        // worker r1 = 0, verifier r1 = 1, worker r2 = 2, verifier r2 = 3.
        let turns: Vec<u32> = manifests.iter().map(|(turn, ..)| *turn).collect();
        assert_eq!(turns, vec![0, 1, 2, 3], "manifests: {manifests:?}");
        // And therefore no two manifests share a receipt key.
        let mut keys = manifests.clone();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), manifests.len(), "colliding receipt keys");
    }

    #[tokio::test]
    async fn goal_loop_iterates_until_the_verifier_passes_it() {
        // Worker completes a turn each round; verifier fails round 1 with
        // feedback, passes round 2.
        let worker = ScriptedProvider::new(vec![
            Ok(text_result("attempt one", 0.01)),
            Ok(text_result("attempt two", 0.01)),
        ]);
        let verifier = ScriptedProvider::new(vec![
            Ok(text_result(
                r#"{"met": false, "reasoning": "tests not run", "feedback": "run the test suite"}"#,
                0.001,
            )),
            Ok(text_result(
                r#"{"met": true, "reasoning": "all evidence present", "feedback": ""}"#,
                0.001,
            )),
        ]);
        let tools = NoTools;
        let engine = Engine::with_sleeper(&worker, &tools, EngineConfig::default(), &NoSleep);
        let mut messages = vec![CompletionMessage::system("sys")];
        let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
        let (tx, rx) = mpsc::unbounded_channel();
        let drain = collect_events(rx);

        let outcome = engine
            .run_goal(
                &verifier,
                "make the tests pass",
                &mut messages,
                &mut budget,
                &tx,
                &GoalConfig::default(),
            )
            .await;
        drop(tx);

        match outcome {
            GoalOutcome::Met {
                rounds,
                verdict,
                cost_usd,
            } => {
                assert_eq!(rounds, 2);
                assert_eq!(verdict, "all evidence present");
                // 2 worker turns + 2 verifier calls.
                assert!((cost_usd - 0.022).abs() < 1e-9, "cost was {cost_usd}");
            }
            other => panic!("expected Met, got {other:?}"),
        }

        // The verifier's feedback reached the worker as a user message.
        let feedback_message = messages
            .iter()
            .find(|m| m.role == MessageRole::User && m.content.contains("run the test suite"))
            .expect("verifier feedback must be fed back into the conversation");
        assert!(feedback_message.content.contains("NOT yet met"));

        // Verdict events tell the whole arc, and verifier spend is metered.
        let events = drain();
        let verdicts: Vec<(usize, bool)> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::GoalVerdict { round, met, .. } => Some((*round, *met)),
                _ => None,
            })
            .collect();
        assert_eq!(verdicts, vec![(1, false), (2, true)]);
        let roles: Vec<ModelCallRole> = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::StepUsage { role, .. } => Some(*role),
                _ => None,
            })
            .collect();
        assert_eq!(
            roles,
            vec![
                ModelCallRole::Worker,
                ModelCallRole::Verdict,
                ModelCallRole::Worker,
                ModelCallRole::Verdict,
            ],
            "worker and verifier calls need distinct durable roles"
        );
        assert!((budget.session_spent_usd() - 0.022).abs() < 1e-9);
    }

    #[tokio::test]
    async fn round_cap_ends_an_unconvinced_goal_loop() {
        let worker = ScriptedProvider::new(vec![
            Ok(text_result("try", 0.0)),
            Ok(text_result("try", 0.0)),
        ]);
        let verifier = ScriptedProvider::new(vec![
            Ok(text_result(
                r#"{"met": false, "reasoning": "no", "feedback": "more"}"#,
                0.0,
            )),
            Ok(text_result(
                r#"{"met": false, "reasoning": "no", "feedback": "more"}"#,
                0.0,
            )),
        ]);
        let tools = NoTools;
        let engine = Engine::with_sleeper(&worker, &tools, EngineConfig::default(), &NoSleep);
        let mut messages = vec![CompletionMessage::system("sys")];
        let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
        let (tx, _rx) = mpsc::unbounded_channel();

        let config = GoalConfig {
            max_rounds: 2,
            ..GoalConfig::default()
        };
        let outcome = engine
            .run_goal(
                &verifier,
                "impossible",
                &mut messages,
                &mut budget,
                &tx,
                &config,
            )
            .await;

        match outcome {
            GoalOutcome::Unmet { rounds, reason, .. } => {
                assert_eq!(rounds, 2);
                assert!(reason.contains("round cap"), "{reason}");
            }
            other => panic!("expected Unmet, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn session_budget_caps_total_spend_across_rounds() {
        // Issue #360: `--budget` must bound the whole loop, not each round.
        // A verifier that never passes would otherwise run every round, and
        // each round's spend is individually under the cap — only the sum
        // across rounds crosses it. The session axis (what
        // `build_budget_guard` configures) enforces that sum; `begin_turn`,
        // called at the top of every round, must not hand back the full
        // limit again.
        let worker = ScriptedProvider::new(vec![
            Ok(text_result("round work", 0.02)),
            Ok(text_result("round work", 0.02)),
            Ok(text_result("round work", 0.02)),
            Ok(text_result("round work", 0.02)),
        ]);
        let verifier = ScriptedProvider::new(vec![
            Ok(text_result(
                r#"{"met": false, "reasoning": "keep going", "feedback": "more"}"#,
                0.0,
            )),
            Ok(text_result(
                r#"{"met": false, "reasoning": "keep going", "feedback": "more"}"#,
                0.0,
            )),
            Ok(text_result(
                r#"{"met": false, "reasoning": "keep going", "feedback": "more"}"#,
                0.0,
            )),
        ]);
        let tools = NoTools;
        let engine = Engine::with_sleeper(&worker, &tools, EngineConfig::default(), &NoSleep);
        let mut messages = vec![CompletionMessage::system("sys")];
        // Mirrors `build_budget_guard(Some(0.05))`: the cap is on the session
        // axis. (A per-turn axis here would reset each round and let the loop
        // spend 0.02 × max_rounds — the pre-fix bug.)
        let mut budget = BudgetGuard::new(BudgetMode::Enforced, None, Some(0.05));
        let config = GoalConfig {
            max_rounds: 8,
            ..GoalConfig::default()
        };
        let (tx, _rx) = mpsc::unbounded_channel();

        let outcome = engine
            .run_goal(
                &verifier,
                "unreachable goal",
                &mut messages,
                &mut budget,
                &tx,
                &config,
            )
            .await;

        match outcome {
            GoalOutcome::Unmet {
                rounds,
                reason,
                cost_usd,
            } => {
                // Stopped well before the round cap, on the budget backstop…
                assert!(
                    rounds < config.max_rounds,
                    "the loop ran to the round cap ({rounds}) instead of the budget"
                );
                assert!(
                    reason.contains("budget"),
                    "the abort reason should cite the budget: {reason}"
                );
                assert!(
                    reason.contains("session"),
                    "the abort reason must name the session axis: {reason}"
                );
                // …and total spend overshot the cap by at most one round.
                assert!(
                    cost_usd > 0.05,
                    "the cap must actually be crossed: {cost_usd}"
                );
                assert!(
                    cost_usd <= 0.07,
                    "spend must not exceed the cap by more than one round: {cost_usd}"
                );
            }
            other => panic!("expected a budget-bounded Unmet, got {other:?}"),
        }
        assert!(
            budget.session_spent_usd() <= 0.07,
            "session spend {} must stay bounded by ~the configured cap",
            budget.session_spent_usd()
        );
    }

    #[tokio::test]
    async fn unparseable_verifier_output_wastes_a_round_not_the_run() {
        let worker = ScriptedProvider::new(vec![
            Ok(text_result("try", 0.0)),
            Ok(text_result("try again", 0.0)),
        ]);
        let verifier = ScriptedProvider::new(vec![
            Ok(text_result("I think it looks pretty good!", 0.0)),
            Ok(text_result(
                r#"{"met": true, "reasoning": "done", "feedback": ""}"#,
                0.0,
            )),
        ]);
        let tools = NoTools;
        let engine = Engine::with_sleeper(&worker, &tools, EngineConfig::default(), &NoSleep);
        let mut messages = vec![CompletionMessage::system("sys")];
        let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
        let (tx, _rx) = mpsc::unbounded_channel();

        let outcome = engine
            .run_goal(
                &verifier,
                "goal",
                &mut messages,
                &mut budget,
                &tx,
                &GoalConfig::default(),
            )
            .await;

        match outcome {
            GoalOutcome::Met { rounds, .. } => assert_eq!(rounds, 2),
            other => panic!("expected Met after recovering, got {other:?}"),
        }
    }

    /// Regression for the bug #922 names in `assess`: it used to build its
    /// verifier with [`Engine::with_sleeper`], which cannot carry the pause
    /// gate, steering, or hooks — so a paused session kept spending inside
    /// the verifier and a soft stop could not end a verifier turn.
    ///
    /// Routing through the sub-agent primitive carries all three. The gate
    /// is the observable one: a verifier that polls it is a verifier a pause can
    /// hold.
    #[tokio::test]
    async fn the_verifier_now_inherits_the_pause_gate_it_used_to_drop() {
        struct CountingGate(AtomicU32);
        #[async_trait]
        impl crate::ports::TurnGate for CountingGate {
            async fn wait_if_paused(&self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }

        let worker = ScriptedProvider::new(vec![Ok(text_result("did the work", 0.0))]);
        let verifier = ScriptedProvider::new(vec![Ok(text_result(
            r#"{"met": true, "reasoning": "done", "feedback": ""}"#,
            0.0,
        ))]);
        let tools = NoTools;
        let gate = CountingGate(AtomicU32::new(0));
        let engine = Engine::with_sleeper(&worker, &tools, EngineConfig::default(), &NoSleep)
            .with_gate(&gate);
        let mut messages = vec![CompletionMessage::system("sys")];
        let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
        let (tx, _rx) = mpsc::unbounded_channel();

        let outcome = engine
            .run_goal(
                &verifier,
                "goal",
                &mut messages,
                &mut budget,
                &tx,
                &GoalConfig::default(),
            )
            .await;

        assert!(matches!(outcome, GoalOutcome::Met { .. }), "{outcome:?}");
        assert_eq!(
            gate.0.load(Ordering::SeqCst),
            2,
            "one poll for the worker's step and one for the verifier's — before \
             #922 the verifier contributed none, so a paused session kept \
             spending inside it"
        );
    }

    /// The verifier's evidence-gathering never enters the goal transcript.
    ///
    /// This is the context-economy half of the same change: a verifier that
    /// reads three files to reach a verdict used to be irrelevant to the
    /// worker's history only by luck (it built a separate `Vec`); now it is
    /// structural, and the loop's message growth is exactly the messages the
    /// loop itself appends — the goal, and one feedback line per unmet round.
    #[tokio::test]
    async fn a_judged_round_grows_the_transcript_by_its_own_messages_only() {
        let worker = ScriptedProvider::new(vec![
            Ok(text_result("attempt one", 0.0)),
            Ok(text_result("attempt two", 0.0)),
        ]);
        let verifier = ScriptedProvider::new(vec![
            Ok(text_result(
                r#"{"met": false, "reasoning": "not yet", "feedback": "keep going"}"#,
                0.0,
            )),
            Ok(text_result(
                r#"{"met": true, "reasoning": "done", "feedback": ""}"#,
                0.0,
            )),
        ]);
        let tools = NoTools;
        let engine = Engine::with_sleeper(&worker, &tools, EngineConfig::default(), &NoSleep);
        let mut messages = vec![CompletionMessage::system("sys")];
        let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
        let (tx, _rx) = mpsc::unbounded_channel();

        engine
            .run_goal(
                &verifier,
                "goal",
                &mut messages,
                &mut budget,
                &tx,
                &GoalConfig::default(),
            )
            .await;

        // system + GOAL + (worker answer, verifier feedback) + worker answer.
        // Nothing the two verifiers saw or said is in here.
        assert_eq!(
            messages.len(),
            5,
            "goal transcript grew by {} messages: {:#?}",
            messages.len(),
            messages.iter().map(|m| m.role).collect::<Vec<_>>()
        );
        assert!(
            !messages
                .iter()
                .any(|message| message.content.contains(VERIFIER_SYSTEM_PROMPT)),
            "the verifier's own prompt must never leak into the worker's history"
        );
    }

    #[tokio::test]
    async fn verifier_failure_ends_the_loop_with_a_named_reason() {
        let worker = ScriptedProvider::new(vec![Ok(text_result("try", 0.0))]);
        // Auth errors are non-retryable, so the verifier fails immediately.
        let verifier = ScriptedProvider::new(vec![Err(ProviderError::Auth("bad key".into()))]);
        let tools = NoTools;
        let engine = Engine::with_sleeper(&worker, &tools, EngineConfig::default(), &NoSleep);
        let mut messages = vec![CompletionMessage::system("sys")];
        let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
        let (tx, _rx) = mpsc::unbounded_channel();

        let outcome = engine
            .run_goal(
                &verifier,
                "goal",
                &mut messages,
                &mut budget,
                &tx,
                &GoalConfig::default(),
            )
            .await;

        match outcome {
            GoalOutcome::Unmet { rounds, reason, .. } => {
                assert_eq!(rounds, 1);
                assert!(reason.contains("verifier unavailable"), "{reason}");
            }
            other => panic!("expected Unmet, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn aborted_working_turn_ends_the_goal_loop() {
        // Worker's provider errors non-retryably → run_turn aborts.
        let worker = ScriptedProvider::new(vec![Err(ProviderError::Auth("expired".into()))]);
        let verifier = ScriptedProvider::new(vec![]);
        let tools = NoTools;
        let engine = Engine::with_sleeper(&worker, &tools, EngineConfig::default(), &NoSleep);
        let mut messages = vec![CompletionMessage::system("sys")];
        let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
        let (tx, _rx) = mpsc::unbounded_channel();

        let outcome = engine
            .run_goal(
                &verifier,
                "goal",
                &mut messages,
                &mut budget,
                &tx,
                &GoalConfig::default(),
            )
            .await;

        match outcome {
            GoalOutcome::Unmet { reason, .. } => {
                assert!(reason.contains("working turn aborted"), "{reason}");
                // The verifier was never consulted about an aborted turn.
                assert_eq!(verifier.calls.load(Ordering::SeqCst), 0);
            }
            other => panic!("expected Unmet, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_aborted_turn_reports_the_spend_it_incurred_before_aborting() {
        use stella_protocol::ToolCall;

        // Round 1: the worker spends on a tool-calling step ($0.05, recorded
        // into the budget guard), then its next model call errors → the turn
        // aborts. The reported cost must reflect that $0.05, not $0.
        let tool_step = CompletionResult {
            text: String::new(),
            tool_calls: vec![ToolCall {
                call_id: "c1".into(),
                name: "noop".into(),
                input: serde_json::json!({}),
            }],
            usage: CompletionUsage {
                reported: true,
                ..CompletionUsage::default()
            },
            model: "scripted".into(),
            cost_usd: 0.05,
            finish_reason: None,
        };
        let worker = ScriptedProvider::new(vec![
            Ok(tool_step),
            Err(ProviderError::Auth("expired".into())),
        ]);
        let verifier = ScriptedProvider::new(vec![]);
        let tools = NoTools;
        let engine = Engine::with_sleeper(&worker, &tools, EngineConfig::default(), &NoSleep);
        let mut messages = vec![CompletionMessage::system("sys")];
        let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
        let (tx, _rx) = mpsc::unbounded_channel();

        let outcome = engine
            .run_goal(
                &verifier,
                "goal",
                &mut messages,
                &mut budget,
                &tx,
                &GoalConfig::default(),
            )
            .await;

        match outcome {
            GoalOutcome::Unmet { cost_usd, .. } => assert!(
                (cost_usd - 0.05).abs() < 1e-9,
                "aborted-turn spend must be reported, got {cost_usd}"
            ),
            other => panic!("expected Unmet, got {other:?}"),
        }
        assert!((budget.session_spent_usd() - 0.05).abs() < 1e-9);
    }

    #[tokio::test]
    async fn goal_outcome_cost_is_delta_from_the_guard_entry() {
        let worker = ScriptedProvider::new(vec![Ok(text_result("done", 0.01))]);
        let verifier = ScriptedProvider::new(vec![Ok(text_result(
            r#"{"met": true, "reasoning": "verified", "feedback": ""}"#,
            0.001,
        ))]);
        let tools = NoTools;
        let engine = Engine::with_sleeper(&worker, &tools, EngineConfig::default(), &NoSleep);
        let mut messages = vec![CompletionMessage::system("sys")];
        let mut budget = BudgetGuard::new(BudgetMode::Observed, None, None);
        budget.reseed_session_spend(0.75);
        let (tx, _rx) = mpsc::unbounded_channel();

        let outcome = engine
            .run_goal(
                &verifier,
                "goal",
                &mut messages,
                &mut budget,
                &tx,
                &GoalConfig::default(),
            )
            .await;

        match outcome {
            GoalOutcome::Met { cost_usd, .. } => assert!(
                (cost_usd - 0.011).abs() < 1e-9,
                "pre-existing session spend is not part of this goal: {cost_usd}"
            ),
            other => panic!("expected Met, got {other:?}"),
        }
        assert!((budget.session_spent_usd() - 0.761).abs() < 1e-9);
    }

    #[test]
    fn parse_verdict_tolerates_prose_and_fences() {
        let fenced = "Here is my assessment:\n```json\n{\"met\": true, \"reasoning\": \"ok\"}\n```";
        let verdict = parse_verdict(fenced).expect("fenced JSON must parse");
        assert!(verdict.met);
        assert!(parse_verdict("no json here at all").is_none());
        assert!(parse_verdict("} backwards {").is_none());
    }

    #[test]
    fn parse_verdict_survives_braces_in_the_verifiers_prose() {
        // The verifier is a code reviewer told to cite evidence, so its prose
        // realistically quotes Rust or JSON before the verdict. The old
        // first-'{'..last-'}' span swallowed those braces, failed to parse,
        // and silently converted `met: true` into "not met" — at temperature
        // 0, every round, until the round cap scored a solved goal Unmet.
        let brace_prose = "The fix adds `if cfg!(test) { return; }` and the tool result was \
                           {\"status\": \"passed\"} — verified.\n\
                           {\"met\": true, \"reasoning\": \"witness confirmed\"}";
        let verdict = parse_verdict(brace_prose).expect("prose braces must not break the verdict");
        assert!(
            verdict.met,
            "the verifier said met:true and must be believed"
        );
        assert_eq!(verdict.reasoning, "witness confirmed");
    }

    #[test]
    fn parse_verdict_handles_braces_inside_the_verdicts_own_strings() {
        // Braces INSIDE the JSON object's strings: the last-'{'-first scan
        // must walk past them to the object's real opening brace.
        let inner = "verdict: {\"met\": false, \"reasoning\": \"code still has `if x { }` bug\", \
                     \"feedback\": \"fix the {} arm\"}";
        let verdict = parse_verdict(inner).expect("inner braces must not break the verdict");
        assert!(!verdict.met);
        assert!(verdict.feedback.contains("{}"));
    }

    #[test]
    fn transcript_tail_keeps_whole_recent_messages() {
        let messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("first message that is quite old"),
            CompletionMessage::user("second message, most recent"),
        ];
        let tail = render_transcript_tail(&messages, 40);
        assert!(tail.contains("second message"));
        assert!(!tail.contains("first message"), "tail was: {tail}");
        // And a generous budget keeps everything, system prompt excluded.
        let full = render_transcript_tail(&messages, 10_000);
        assert!(full.contains("first message"));
        assert!(!full.contains("sys"));
    }

    #[test]
    fn transcript_tail_caps_a_single_oversized_message() {
        // One assistant message far larger than the budget: it is the only
        // block, so the tail must still carry it — truncated, not whole.
        let messages = vec![CompletionMessage::assistant("x".repeat(5_000))];
        let tail = render_transcript_tail(&messages, 40);
        assert!(tail.starts_with("ASSISTANT: xxx"), "tail was: {tail}");
        assert!(tail.ends_with('…'), "tail was: {tail}");
        // `truncate_chars` appends one ellipsis char past the cap.
        assert_eq!(tail.chars().count(), 41, "tail was: {tail}");
    }

    #[test]
    fn truncate_chars_respects_multibyte_boundaries() {
        let s = "héllo wörld";
        let cut = truncate_chars(s, 4);
        assert_eq!(cut, "héll…");
        assert_eq!(truncate_chars("short", 10), "short");
    }

    /// The three goal loops (core, CLI pipeline, serve) share these helpers so
    /// their prompts and receipt slots cannot drift — pin the exact wording
    /// and slot math the shared functions promise.
    #[test]
    fn shared_goal_loop_helpers_pin_wording_and_slot_math() {
        assert_eq!(
            goal_kickoff_text("ship it"),
            "GOAL: ship it\n\nWork toward this goal. An independent verifier will assess the \
             result after each working round from the transcript evidence; keep your work \
             verifiable (run tests, show outputs)."
        );
        let verdict = GoalVerifierVerdict {
            met: false,
            reasoning: "tests missing".into(),
            feedback: "add a test".into(),
        };
        assert_eq!(
            verifier_feedback_text("ship it", &verdict),
            "The verifier assessed the goal as NOT yet met.\nVerifier feedback: add a test\n\n\
             Continue working toward the goal: ship it"
        );
        // Whitespace-only feedback falls back to the reasoning.
        let bare = GoalVerifierVerdict {
            met: false,
            reasoning: "tests missing".into(),
            feedback: "  ".into(),
        };
        assert!(
            verifier_feedback_text("ship it", &bare).contains("Verifier feedback: tests missing")
        );
        // Worker rounds take even slots; `Engine::assess` adds the verifier's +1.
        assert_eq!(goal_round_turn_offset(1), 0);
        assert_eq!(goal_round_turn_offset(2), 2);
        assert_eq!(goal_round_turn_offset(3), 4);
        assert_eq!(goal_round_turn_offset(usize::MAX), u32::MAX);
    }
}
