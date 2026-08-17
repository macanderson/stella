// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Reactive recovery from a refused output ceiling at the model-call boundary.
//!
//! A gateway prices a request against the ceiling the caller *asks for*, so a
//! session configured with a large `max_output_tokens` is refused the moment
//! the balance falls below the price of the ask — while still holding credit
//! enough for dozens of real calls. That rejection used to arrive as
//! `ProviderError::Terminal` and end the turn: three benchmark runs were lost
//! or maimed by it, every trial dead against a balance the provider itself
//! said could fund a smaller ask.
//!
//! These tests drive real turns through scripted providers that reject with
//! [`ProviderError::OutputBudgetExceeded`] and assert both sides of the
//! contract: a refused ceiling is re-asked smaller (the witness), and the
//! ladder is latched so endless refusal aborts after a bounded number of
//! paid attempts instead of looping (the death-spiral guard).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::Value;
use stella_protocol::{CompletionResult, ToolSchema};

use super::super::*;
use crate::driver::output_budget_recovery::SessionOutputCeilings;

/// Rejects the first `refusals` calls as an unaffordable ceiling, naming
/// `affordable` each time, then completes. Records the ceiling every attempt
/// actually asked for — the whole question this file exists to answer.
struct RefuseCeilingThenComplete {
    refusals: u32,
    affordable: Option<u32>,
    calls: AtomicU32,
    asked: Mutex<Vec<Option<u32>>>,
}

impl RefuseCeilingThenComplete {
    fn new(refusals: u32, affordable: Option<u32>) -> Self {
        Self {
            refusals,
            affordable,
            calls: AtomicU32::new(0),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<Option<u32>> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Provider for RefuseCeilingThenComplete {
    fn id(&self) -> &str {
        "scripted"
    }

    async fn complete_ref(
        &self,
        req: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.asked.lock().unwrap().push(req.max_output_tokens);
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.refusals {
            return Err(ProviderError::OutputBudgetExceeded {
                message: "Scripted cannot afford the requested output ceiling (HTTP 402)"
                    .to_string(),
                affordable_output_tokens: self.affordable,
            });
        }
        Ok(CompletionResult {
            upstream_provider: None,
            text: "recovered".to_string(),
            tool_calls: Vec::new(),
            usage: stella_protocol::CompletionUsage::default(),
            model: "scripted-strong".to_string(),
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// A gateway that prices the *ask*: every request naming a ceiling above what
/// the balance can fund is refused with the affordable figure, and every
/// request at or under it completes. Closer to the recorded OpenRouter
/// behaviour than [`RefuseCeilingThenComplete`]'s fixed refusal count, and the
/// only shape that can show a turn paying — or not paying — a wasted 402.
struct RefuseAskAbove {
    affordable: u32,
    asked: Mutex<Vec<Option<u32>>>,
}

impl RefuseAskAbove {
    fn new(affordable: u32) -> Self {
        Self {
            affordable,
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<Option<u32>> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl Provider for RefuseAskAbove {
    fn id(&self) -> &str {
        "scripted-gateway"
    }

    async fn complete_ref(
        &self,
        req: stella_protocol::CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.asked.lock().unwrap().push(req.max_output_tokens);
        if req
            .max_output_tokens
            .is_none_or(|asked| asked > self.affordable)
        {
            return Err(ProviderError::OutputBudgetExceeded {
                message: "Scripted cannot afford the requested output ceiling (HTTP 402)"
                    .to_string(),
                affordable_output_tokens: Some(self.affordable),
            });
        }
        Ok(CompletionResult {
            upstream_provider: None,
            text: "recovered".to_string(),
            tool_calls: Vec::new(),
            usage: stella_protocol::CompletionUsage::default(),
            model: "scripted-strong".to_string(),
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

struct NoTools;

#[async_trait::async_trait]
impl ToolExecutor for NoTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        Vec::new()
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: String::new(),
            data: None,
        }
    }
}

struct NoSleep;

#[async_trait::async_trait]
impl crate::retry::Sleeper for NoSleep {
    async fn sleep(&self, _duration_ms: u64) {}
}

/// One real turn against `provider` with a configured output ceiling, and
/// optionally the session-scoped carry a host attaches (#3307). Passing the
/// same handle to two calls is what makes them two turns of ONE session
/// rather than two unrelated sessions.
async fn run_turn_with_ceiling_and_carry(
    provider: &dyn Provider,
    ceiling: u32,
    carry: Option<&std::sync::Arc<SessionOutputCeilings>>,
) -> TurnOutcome {
    let tools = NoTools;
    let sleeper = NoSleep;
    let config = EngineConfig {
        max_output_tokens: Some(ceiling),
        session_output_ceilings: carry.cloned(),
        ..EngineConfig::default()
    };
    let engine = Engine::with_sleeper(provider, &tools, config, &sleeper);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut messages = vec![CompletionMessage::user("do the thing")];
    let mut budget = BudgetGuard::new(stella_protocol::BudgetMode::Off, None, None);
    engine.run_turn(&mut messages, &mut budget, &tx).await
}

/// One real turn against `provider`, with no session carry — the shape every
/// test here had before the carry existed.
async fn run_turn_with_ceiling(provider: &dyn Provider, ceiling: u32) -> TurnOutcome {
    run_turn_with_ceiling_and_carry(provider, ceiling, None).await
}

/// The witness. On `main` the 402 is `ProviderError::Terminal` and the turn
/// aborts on the first call; with recovery the engine re-asks under the
/// figure the provider named and the turn completes.
#[tokio::test]
async fn a_refused_ceiling_is_re_asked_smaller_and_the_turn_completes() {
    let provider = RefuseCeilingThenComplete::new(1, Some(117_676));
    let outcome = run_turn_with_ceiling(&provider, 128_000).await;

    let asked = provider.asked();
    assert_eq!(asked.len(), 2, "expected a retry, got {asked:?}");
    assert_eq!(
        asked[0],
        Some(128_000),
        "the first ask is the configured one"
    );
    let retried = asked[1].expect("the retry must still name a ceiling");
    assert!(
        retried < 117_676,
        "the retry must sit under the stated affordable figure, got {retried}"
    );
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "the turn must survive a refused ceiling: {outcome:?}"
    );
}

/// A provider that names no figure still gets a smaller ask — the engine
/// halves its own rather than inventing one the provider never stated.
#[tokio::test]
async fn an_unnamed_ceiling_still_produces_a_smaller_ask() {
    let provider = RefuseCeilingThenComplete::new(1, None);
    let outcome = run_turn_with_ceiling(&provider, 64_000).await;

    let asked = provider.asked();
    assert_eq!(asked, vec![Some(64_000), Some(32_000)], "{asked:?}");
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
}

/// The #3307 witness. Two turns of ONE session against a balance that stays
/// low: the second turn must go out already reduced on its **first** attempt.
///
/// On `main` the clamp dies with the turn that learned it, so turn 2 re-sends
/// the configured 128K, is refused, and only its retry goes out reduced — one
/// wasted 402 round-trip per turn for as long as the balance stays low, which
/// on a bench trial is dozens of them.
#[tokio::test]
async fn a_learned_ceiling_survives_the_turn_and_the_next_turn_pays_no_402() {
    let provider = RefuseAskAbove::new(117_676);
    let carry = std::sync::Arc::new(SessionOutputCeilings::default());

    let first = run_turn_with_ceiling_and_carry(&provider, 128_000, Some(&carry)).await;
    let after_first = provider.asked();
    assert_eq!(
        after_first.len(),
        2,
        "turn 1 learns the hard way: {after_first:?}"
    );
    assert_eq!(
        after_first[0],
        Some(128_000),
        "turn 1 asks the configured ceiling"
    );
    let learned = after_first[1].expect("the retry must still name a ceiling");
    assert!(matches!(first, TurnOutcome::Completed { .. }), "{first:?}");

    let second = run_turn_with_ceiling_and_carry(&provider, 128_000, Some(&carry)).await;
    let asked = provider.asked();
    assert_eq!(
        asked.len(),
        3,
        "turn 2 must not buy the same 402 again — one call, not two: {asked:?}"
    );
    assert_eq!(
        asked[2],
        Some(learned),
        "turn 2's FIRST ask must already carry turn 1's clamp: {asked:?}"
    );
    assert!(
        matches!(second, TurnOutcome::Completed { .. }),
        "{second:?}"
    );
}

/// The carry is opt-in, and its absence is exactly the old behaviour: an
/// unattached host re-asks the configured ceiling every turn and pays the 402
/// again. Guards the byte-stability half of the contract (invariant 7) — a
/// session that never attaches a handle sends what it always sent.
#[tokio::test]
async fn without_a_carry_every_turn_re_asks_the_configured_ceiling() {
    let provider = RefuseAskAbove::new(117_676);

    run_turn_with_ceiling(&provider, 128_000).await;
    run_turn_with_ceiling(&provider, 128_000).await;

    let asked = provider.asked();
    assert_eq!(
        asked.len(),
        4,
        "two turns, each paying its own 402: {asked:?}"
    );
    assert_eq!(
        asked[2],
        Some(128_000),
        "turn 2 re-asks the configured ceiling with no carry attached: {asked:?}"
    );
}

/// A session whose balance was topped up must not stay capped forever.
/// Nothing observable announces a top-up, so the carry decays into a re-probe:
/// after [`REPROBE_TURNS`] turns the next call asks for the caller's full
/// ceiling again, and here the gateway now funds it.
///
/// [`REPROBE_TURNS`]: crate::driver::output_budget_recovery::REPROBE_TURNS
#[tokio::test]
async fn a_topped_up_balance_is_honoured_within_the_reprobe_period() {
    use crate::driver::output_budget_recovery::REPROBE_TURNS;

    let low = RefuseAskAbove::new(117_676);
    let carry = std::sync::Arc::new(SessionOutputCeilings::default());
    run_turn_with_ceiling_and_carry(&low, 128_000, Some(&carry)).await;
    assert!(
        carry.standing("scripted-gateway").is_some(),
        "turn 1 must have learned a ceiling"
    );

    // The top-up: the same session, now against a gateway that funds the full
    // ask. Every turn ages the carry; by the reprobe period it is forgotten.
    let topped_up = RefuseAskAbove::new(u32::MAX);
    for _ in 0..REPROBE_TURNS {
        run_turn_with_ceiling_and_carry(&topped_up, 128_000, Some(&carry)).await;
    }

    let asked = topped_up.asked();
    assert_eq!(
        asked.len(),
        usize::try_from(REPROBE_TURNS).unwrap(),
        "one call per turn, none of them refused: {asked:?}"
    );
    assert_eq!(
        asked.last().copied().flatten(),
        Some(128_000),
        "the session must be back to its configured ceiling: {asked:?}"
    );
    assert_eq!(
        carry.standing("scripted-gateway"),
        None,
        "the stale ceiling must have been forgotten, not merely unused"
    );
}

/// The death-spiral guard: a provider that refuses every ceiling must abort
/// after a bounded number of paid attempts, not clamp forever. The abort is
/// the ordinary terminal shape, indistinguishable from a failure that never
/// had a recovery to spend.
#[tokio::test]
async fn endless_refusal_aborts_after_a_bounded_number_of_attempts() {
    let provider = RefuseCeilingThenComplete::new(u32::MAX, Some(100_000));
    let outcome = run_turn_with_ceiling(&provider, 128_000).await;

    let asked = provider.asked();
    let max_rungs = usize::from(crate::driver::output_budget_recovery::MAX_RECOVERY_RUNGS);
    assert_eq!(
        asked.len(),
        max_rungs + 1,
        "one initial ask plus one per rung, got {asked:?}"
    );
    // Monotonically tighter, so the ladder cannot oscillate.
    for pair in asked.windows(2) {
        assert!(pair[1] < pair[0], "ceilings must only fall: {asked:?}");
    }
    let TurnOutcome::Aborted { reason, .. } = &outcome else {
        panic!("a spent ladder must abort: {outcome:?}");
    };
    assert!(reason.contains("model call failed"), "{reason}");
}
