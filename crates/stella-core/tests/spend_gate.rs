// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The spend gate: what a turn buys, pinned scenario by scenario.
//!
//! A change to the step loop can leave every answer the same and still buy
//! an extra model call. This file is what fails when it does. Each test
//! below drives one turn over scripted ports and pins three numbers: model
//! calls, tool runs, and the cost the turn reports.
//!
//! Read a red run one of two ways. Either the change is a bug, and the fix
//! is in the code. Or the new numbers are the ones you meant, and you edit
//! the pin in this file in the same pull request. A reviewer then reads the
//! new price as a diff line. Never widen a pin to whatever the loop does
//! now: the worth of this file is that intent is written down.
//!
//! The ports are scripted, so the engine does no I/O (architecture rule 2).
//! An answered model call costs a flat quarter dollar here. That price is a
//! round binary number, so a sum of them is exact and a pin can be too.
//!
//! The first four scenarios are plain: each step buys one model call. The
//! rest buy a call no step asked for. Each one names the module it prices,
//! so a red run says which recovery got dearer.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use serde_json::Value;
use stella_core::budget::BudgetGuard;
use stella_core::hooks::{
    HookAction, HookExecError, HookExecResult, HookMatcher, HookRunner, Hooks,
};
use stella_core::ports::{FallbackResolver, ResolvedFallback, ToolExecutor};
use stella_core::retry::Sleeper;
use stella_core::{Engine, EngineConfig, TurnOutcome};
use stella_protocol::{
    BudgetMode, CompletionMessage, CompletionRequestRef, CompletionResult, CompletionUsage,
    Provider, ProviderError, ToolCall, ToolOutput, ToolSchema,
};
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::mpsc;

/// What one answered model call costs the scripted provider.
const CALL_COST_USD: f64 = 0.25;

/// What a turn bought.
///
/// `model_calls` counts attempts, not bills: a refused call reaches the
/// provider and takes wall clock, so it belongs in the number a reviewer
/// reads. `cost_usd` is what the turn reported, which only answered calls
/// add to.
#[derive(Debug, PartialEq)]
struct Spend {
    model_calls: u32,
    tool_calls: u32,
    cost_usd: f64,
}

/// A `Sleeper` that never waits, so a retry ladder costs no test time.
struct NoopSleeper;
#[async_trait]
impl Sleeper for NoopSleeper {
    async fn sleep(&self, _duration_ms: u64) {}
}

/// A scripted `Provider`: one entry per call, repeating the last entry once
/// the script runs out. Counts every attempt it is handed.
///
/// The id is set per instance. A fallback that resolves back to the failed
/// provider's id is refused, so the replacement needs a name of its own.
struct ScriptedProvider {
    id: &'static str,
    script: TokioMutex<Vec<Result<CompletionResult, ProviderError>>>,
    calls: Arc<AtomicU32>,
}

impl ScriptedProvider {
    fn new(
        id: &'static str,
        script: Vec<Result<CompletionResult, ProviderError>>,
        calls: Arc<AtomicU32>,
    ) -> Self {
        Self {
            id,
            script: TokioMutex::new(script),
            calls,
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        self.id
    }
    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut script = self.script.lock().await;
        if script.len() > 1 {
            script.remove(0)
        } else {
            clone_step(&script[0])
        }
    }
}

fn clone_step(
    step: &Result<CompletionResult, ProviderError>,
) -> Result<CompletionResult, ProviderError> {
    match step {
        Ok(result) => Ok(result.clone()),
        Err(error) => Err(error.clone()),
    }
}

/// A tool that always succeeds and counts the times it really ran.
struct CountingTools {
    calls: Arc<AtomicU32>,
}

#[async_trait]
impl ToolExecutor for CountingTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "bash".into(),
            description: "run a command".into(),
            input_schema: serde_json::json!({"type": "object"}),
            read_only: false,
            speculation_safe: false,
        }]
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ToolOutput::Ok {
            content: "ok".into(),
            data: None,
        }
    }
}

fn answer(text: &str) -> CompletionResult {
    CompletionResult {
        upstream_provider: None,
        text: text.into(),
        tool_calls: vec![],
        usage: CompletionUsage::reported_zero(),
        model: "scripted".into(),
        cost_usd: CALL_COST_USD,
        finish_reason: None,
    }
}

/// A step that asks for one run of the scripted tool per command given.
/// Each call carries its own id and command, so the two calls of a two-tool
/// step are two units of work rather than one call sent twice.
fn tool_step(commands: &[(&str, &str)]) -> CompletionResult {
    CompletionResult {
        upstream_provider: None,
        text: String::new(),
        tool_calls: commands
            .iter()
            .map(|(call_id, command)| ToolCall {
                call_id: (*call_id).into(),
                name: "bash".into(),
                input: serde_json::json!({ "cmd": command }),
            })
            .collect(),
        usage: CompletionUsage::reported_zero(),
        model: "scripted".into(),
        cost_usd: CALL_COST_USD,
        finish_reason: None,
    }
}

/// Hands out one replacement provider on every ask. The engine's set-once
/// latch is the bound, so this can answer freely.
struct OneReplacement<'p> {
    to: &'p dyn Provider,
}

impl FallbackResolver for OneReplacement<'_> {
    fn resolve_fallback(&self, failed_provider_id: &str) -> Option<ResolvedFallback<'_>> {
        Some(ResolvedFallback {
            provider: self.to,
            reason: format!("scripted re-resolution away from `{failed_provider_id}`"),
        })
    }
}

/// A `HookRunner` that prints one decision, whatever it is asked. Exit code
/// 0: a non-zero exit is a failure, not a decision, and the Stop gate then
/// fails open.
struct DecidingRunner {
    stdout: String,
}

#[async_trait]
impl HookRunner for DecidingRunner {
    async fn run(
        &self,
        _action: &HookAction,
        _payload_json: &str,
        _cwd: &str,
    ) -> Result<HookExecResult, HookExecError> {
        Ok(HookExecResult {
            exit_code: 0,
            stdout: self.stdout.clone(),
            stderr: String::new(),
        })
    }
}

/// One scenario: the scripted ports it runs over, and the turn it drives.
struct Turn {
    script: Vec<Result<CompletionResult, ProviderError>>,
    config: EngineConfig,
    messages: Vec<CompletionMessage>,
    /// The replacement's script, when the scenario attaches a fallback.
    replacement: Option<Vec<Result<CompletionResult, ProviderError>>>,
    /// The document a `Stop` hook prints, when the scenario attaches one.
    stop_decision: Option<String>,
}

impl Turn {
    fn new(script: Vec<Result<CompletionResult, ProviderError>>) -> Self {
        Self {
            script,
            config: EngineConfig::default(),
            messages: vec![
                CompletionMessage::system("sys"),
                CompletionMessage::user("do the thing"),
            ],
            replacement: None,
            stop_decision: None,
        }
    }

    fn config(mut self, edit: impl FnOnce(&mut EngineConfig)) -> Self {
        edit(&mut self.config);
        self
    }

    fn messages(mut self, messages: Vec<CompletionMessage>) -> Self {
        self.messages = messages;
        self
    }

    fn replacement(mut self, script: Vec<Result<CompletionResult, ProviderError>>) -> Self {
        self.replacement = Some(script);
        self
    }

    /// Attach a `Stop` hook that denies every completion.
    fn stop_hook_denies(mut self, reason: &str) -> Self {
        self.stop_decision =
            Some(serde_json::json!({ "action": "deny", "reason": reason }).to_string());
        self
    }

    /// Drive the turn and report what it bought. Both providers share one
    /// counter, so `model_calls` is the turn's total.
    async fn run(self) -> (TurnOutcome, Spend) {
        let model_calls = Arc::new(AtomicU32::new(0));
        let tool_calls = Arc::new(AtomicU32::new(0));
        let provider = ScriptedProvider::new("primary", self.script, model_calls.clone());
        let replacement = self
            .replacement
            .map(|script| ScriptedProvider::new("replacement", script, model_calls.clone()));
        let tools = CountingTools {
            calls: tool_calls.clone(),
        };
        let sleeper = NoopSleeper;
        let resolver = replacement.as_ref().map(|to| OneReplacement {
            to: to as &dyn Provider,
        });
        let hooks = self.stop_decision.as_ref().map(|_| Hooks {
            stop: Some(vec![HookMatcher {
                matcher: None,
                hooks: vec![HookAction::new("stop-gate")],
            }]),
            ..Hooks::default()
        });
        let runner = self.stop_decision.map(|stdout| DecidingRunner { stdout });

        let mut engine = Engine::with_sleeper(&provider, &tools, self.config, &sleeper);
        if let Some(resolver) = &resolver {
            engine = engine.with_fallback_resolver(resolver);
        }
        if let (Some(hooks), Some(runner)) = (&hooks, &runner) {
            engine = engine.with_hooks(hooks, runner);
        }

        let mut messages = self.messages;
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let (tx, _rx) = mpsc::unbounded_channel();

        let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
        let cost_usd = match &outcome {
            TurnOutcome::Completed { cost_usd, .. } | TurnOutcome::Aborted { cost_usd, .. } => {
                *cost_usd
            }
        };
        let spend = Spend {
            model_calls: model_calls.load(Ordering::SeqCst),
            tool_calls: tool_calls.load(Ordering::SeqCst),
            cost_usd,
        };
        (outcome, spend)
    }
}

/// Drive one turn over the scripted ports and report what it bought.
async fn run_turn(script: Vec<Result<CompletionResult, ProviderError>>) -> (TurnOutcome, Spend) {
    Turn::new(script).run().await
}

/// Fail with the scenario named, the pin, and what the loop just bought.
fn assert_spend(scenario: &str, got: &Spend, want: &Spend) {
    assert_eq!(
        got, want,
        "spend gate: the scenario `{scenario}` changed price.\n  \
         pinned: {want:?}\n  bought: {got:?}\n  \
         Fix the loop, or edit this pin in the same pull request."
    );
}

#[tokio::test]
async fn a_one_step_answer_buys_one_model_call() {
    let (outcome, spend) = run_turn(vec![Ok(answer("done"))]).await;

    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "done".into(),
            cost_usd: CALL_COST_USD,
        }
    );
    assert_spend(
        "one-step answer",
        &spend,
        &Spend {
            model_calls: 1,
            tool_calls: 0,
            cost_usd: CALL_COST_USD,
        },
    );
}

#[tokio::test]
async fn two_tools_in_one_step_buy_one_model_call_between_them() {
    let (outcome, spend) = run_turn(vec![
        Ok(tool_step(&[("call_1", "ls"), ("call_2", "pwd")])),
        Ok(answer("done")),
    ])
    .await;

    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "done".into(),
            cost_usd: 2.0 * CALL_COST_USD,
        }
    );
    assert_spend(
        "two-tool step",
        &spend,
        &Spend {
            model_calls: 2,
            tool_calls: 2,
            cost_usd: 2.0 * CALL_COST_USD,
        },
    );
}

#[tokio::test]
async fn a_rate_limited_call_is_retried_once_and_billed_once() {
    let refused = ProviderError::RateLimited {
        message: "429".into(),
        retry_after_ms: Some(10),
    };
    let (outcome, spend) = run_turn(vec![Err(refused), Ok(answer("done"))]).await;

    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "done".into(),
            cost_usd: CALL_COST_USD,
        }
    );
    assert_spend(
        "retry after a 429",
        &spend,
        &Spend {
            model_calls: 2,
            tool_calls: 0,
            cost_usd: CALL_COST_USD,
        },
    );
}

#[tokio::test]
async fn a_stuck_loop_is_steered_once_and_then_stopped() {
    // The script never runs out, so the only thing that can end this turn
    // is loop detection. Three no-progress calls earn one warning and a
    // fourth ends the turn, which is where the pinned four comes from.
    let (outcome, spend) = run_turn(vec![Ok(tool_step(&[("call_1", "ls")]))]).await;

    match &outcome {
        TurnOutcome::Aborted { reason, .. } => {
            assert!(reason.contains("stuck-loop"), "unexpected reason: {reason}");
        }
        other => panic!("expected a stuck-loop abort, got {other:?}"),
    }
    assert_spend(
        "loop-detector trip",
        &spend,
        &Spend {
            model_calls: 4,
            tool_calls: 4,
            cost_usd: 4.0 * CALL_COST_USD,
        },
    );
}

// The paths that buy a model call no step asked for. Each scenario below
// drives one arm and pins what it costs. A change that makes one of them
// fire more often is the cost regression this file exists to catch.

/// A transcript with a span the summarizer can fold. The middle has to be
/// long enough to clear the four-message floor once the kept tail is set
/// aside.
fn long_transcript() -> Vec<CompletionMessage> {
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("do the thing"),
    ];
    for note in 0..19 {
        messages.push(CompletionMessage::user(format!("earlier note {note}")));
    }
    messages
}

/// The summarizer buys a second call at this step. The pure passes cannot
/// get under the budget, so a model writes the summary before the worker
/// call goes out.
///
/// A budget of one token forces the arm. The shipped default is 150k, and a
/// scripted port should not have to build a transcript that large.
#[tokio::test]
async fn the_overflow_summarizer_buys_one_extra_model_call() {
    let (outcome, spend) = Turn::new(vec![Ok(answer("SUMMARY")), Ok(answer("done"))])
        .messages(long_transcript())
        .config(|config| config.compaction_budget_tokens = 1)
        .run()
        .await;

    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "done".into(),
            cost_usd: 2.0 * CALL_COST_USD,
        },
        "the summarizer's spend folds into the turn total"
    );
    assert_spend(
        "overflow summarizer (driver::restore's summarize_overflow_span)",
        &spend,
        &Spend {
            model_calls: 2,
            tool_calls: 0,
            cost_usd: 2.0 * CALL_COST_USD,
        },
    );
}

/// A mid-turn provider fallback. The ladder ends on the first attempt,
/// because the failure is not retryable. The resolver hands over a
/// replacement and the step re-runs there.
#[tokio::test]
async fn a_provider_fallback_buys_one_call_on_the_replacement() {
    let (outcome, spend) = Turn::new(vec![Err(ProviderError::Terminal("wedged".into()))])
        .replacement(vec![Ok(answer("rescued"))])
        .run()
        .await;

    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "rescued".into(),
            cost_usd: CALL_COST_USD,
        },
        "the turn finishes on the replacement, and only its call bills"
    );
    assert_spend(
        "provider fallback after an exhausted ladder (driver::model_fallback)",
        &spend,
        &Spend {
            model_calls: 2,
            tool_calls: 0,
            cost_usd: CALL_COST_USD,
        },
    );
}

/// The same failure with nothing to fall back on. The swap is what buys the
/// extra call, so the turn ends on the refused attempt.
#[tokio::test]
async fn an_exhausted_ladder_with_no_fallback_buys_nothing_further() {
    let (outcome, spend) = run_turn(vec![Err(ProviderError::Terminal("wedged".into()))]).await;

    match &outcome {
        TurnOutcome::Aborted { reason, .. } => {
            assert!(reason.contains("wedged"), "unexpected reason: {reason}");
        }
        other => panic!("expected a failed model call, got {other:?}"),
    }
    assert_spend(
        "exhausted ladder with no fallback (driver::model_fallback declining)",
        &spend,
        &Spend {
            model_calls: 1,
            tool_calls: 0,
            cost_usd: 0.0,
        },
    );
}

/// The provider refuses the ceiling this turn asked for and names one it
/// can fund. The step re-runs under the clamp. The refused call bills
/// nothing, so the added spend is one attempt.
#[tokio::test]
async fn an_output_budget_clamp_re_asks_the_same_step_once() {
    let refused = ProviderError::OutputBudgetExceeded {
        message: "can only afford 8000".into(),
        affordable_output_tokens: Some(8_000),
    };
    let (outcome, spend) = run_turn(vec![Err(refused), Ok(answer("done"))]).await;

    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "done".into(),
            cost_usd: CALL_COST_USD,
        }
    );
    assert_spend(
        "output-budget clamp re-ask (driver::output_budget_recovery)",
        &spend,
        &Spend {
            model_calls: 2,
            tool_calls: 0,
            cost_usd: CALL_COST_USD,
        },
    );
}

/// The bound on that ladder. A ceiling under the floor an answer needs arms
/// no rung, so the refusal is terminal on the first attempt.
#[tokio::test]
async fn an_unfundable_output_ceiling_arms_no_rung() {
    let refused = ProviderError::OutputBudgetExceeded {
        message: "can only afford 500".into(),
        affordable_output_tokens: Some(500),
    };
    let (outcome, spend) = run_turn(vec![Err(refused)]).await;

    match &outcome {
        TurnOutcome::Aborted { reason, .. } => {
            assert!(
                reason.contains("output budget"),
                "unexpected reason: {reason}"
            );
        }
        other => panic!("expected an unrecovered output-budget failure, got {other:?}"),
    }
    assert_spend(
        "output-budget ladder refusing to arm (driver::output_budget_recovery)",
        &spend,
        &Spend {
            model_calls: 1,
            tool_calls: 0,
            cost_usd: 0.0,
        },
    );
}

/// A parked wait for a 429 that will not clear. The inline ladder spends
/// its retries, then one park buys one more attempt, which answers. Seven
/// refused calls: the first try plus the six retries the default policy
/// allows. The park adds one call.
#[tokio::test]
async fn a_parked_rate_limit_buys_one_attempt_per_park() {
    let refused = || -> Result<CompletionResult, ProviderError> {
        Err(ProviderError::RateLimited {
            message: "429, no stated window".into(),
            retry_after_ms: None,
        })
    };
    let mut script: Vec<Result<CompletionResult, ProviderError>> =
        (0..7).map(|_| refused()).collect();
    script.push(Ok(answer("done")));

    let (outcome, spend) = run_turn(script).await;

    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "done".into(),
            cost_usd: CALL_COST_USD,
        }
    );
    assert_spend(
        "parked wait after an exhausted inline ladder (driver::rate_limit)",
        &spend,
        &Spend {
            model_calls: 8,
            tool_calls: 0,
            cost_usd: CALL_COST_USD,
        },
    );
}

/// The bound on the park. A stated backoff no park can afford fails fast on
/// the first attempt. A shorter wait earns the same refusal, so it would
/// spend wall clock and buy nothing. The inline ladder never runs either: a
/// hint past its ceiling cannot be slept inline.
#[tokio::test]
async fn a_stated_backoff_no_park_can_afford_buys_one_attempt() {
    let refused = ProviderError::RateLimited {
        message: "429, come back tomorrow".into(),
        retry_after_ms: Some(24 * 60 * 60 * 1000),
    };
    let (outcome, spend) = run_turn(vec![Err(refused)]).await;

    match &outcome {
        TurnOutcome::Aborted { reason, .. } => {
            assert!(
                reason.contains("failing fast instead of waiting past the budget"),
                "unexpected reason: {reason}"
            );
        }
        other => panic!("expected a fast failure, got {other:?}"),
    }
    assert_spend(
        "unaffordable stated backoff (driver::rate_limit declining to park)",
        &spend,
        &Spend {
            model_calls: 1,
            tool_calls: 0,
            cost_usd: 0.0,
        },
    );
}

/// The loop steer priced on its own, apart from the abort above. Three
/// no-progress calls earn one warning. A model that obeys it answers on the
/// next call, and the steer buys that call.
#[tokio::test]
async fn a_loop_steer_the_model_obeys_buys_one_more_call() {
    let (outcome, spend) = run_turn(vec![
        Ok(tool_step(&[("call_1", "ls")])),
        Ok(tool_step(&[("call_2", "ls")])),
        Ok(tool_step(&[("call_3", "ls")])),
        Ok(answer("done")),
    ])
    .await;

    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "done".into(),
            cost_usd: 4.0 * CALL_COST_USD,
        }
    );
    assert_spend(
        "loop steer obeyed (driver::loop_escalation)",
        &spend,
        &Spend {
            model_calls: 4,
            tool_calls: 3,
            cost_usd: 4.0 * CALL_COST_USD,
        },
    );
}

/// The prove-it re-ask. A gated turn that changed the workspace and
/// declares done is sent back once. One extra call, and one only: the nudge
/// is on the transcript, so the next declaration stands.
#[tokio::test]
async fn the_prove_it_re_ask_buys_one_more_call() {
    let (outcome, spend) = Turn::new(vec![
        Ok(tool_step(&[("call_1", "ls")])),
        Ok(answer("done")),
        Ok(answer("checked, and done")),
    ])
    .config(|config| config.completion_gate = true)
    .run()
    .await;

    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "checked, and done".into(),
            cost_usd: 3.0 * CALL_COST_USD,
        }
    );
    assert_spend(
        "prove-it re-ask under the completion gate (driver::confident_zero)",
        &spend,
        &Spend {
            model_calls: 3,
            tool_calls: 1,
            cost_usd: 3.0 * CALL_COST_USD,
        },
    );
}

/// A Stop hook that denies every completion. The turn is held open for the
/// hook's whole allowance and then finishes. Four calls: the declaration,
/// one per held round, and the one that stands.
#[tokio::test]
async fn a_denying_stop_hook_buys_one_call_per_held_round() {
    let (outcome, spend) = Turn::new(vec![Ok(answer("done"))])
        .stop_hook_denies("the work is unproven")
        .run()
        .await;

    assert_eq!(
        outcome,
        TurnOutcome::Completed {
            text: "done".into(),
            cost_usd: 4.0 * CALL_COST_USD,
        },
        "the allowance is spent and the last declaration stands"
    );
    assert_spend(
        "Stop hook denying and continuing (driver::user_hooks)",
        &spend,
        &Spend {
            model_calls: 4,
            tool_calls: 0,
            cost_usd: 4.0 * CALL_COST_USD,
        },
    );
}
