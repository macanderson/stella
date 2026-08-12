//! Complete per-call accounting for pipeline roles that call providers directly.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use stella_core::retry::RetryPolicy;
// One home for the starvation arithmetic, shared with `stella-cli`'s
// standalone-call chokepoint: #2128 fixed this path and left that one running
// on a bare cap, which is how post-turn reflection starved for nine days
// (#2174). Two copies of these numbers is what let the second caller miss the
// first fix.
use stella_core::budget::DeadlineOutcome;
use stella_core::starvation::{starved_of_output, starved_retry_cap, with_reasoning_headroom};
use stella_core::{
    AccountedCall, AccountedCallError, BudgetGuard, ReceiptContext, run_accounted_call,
};
use stella_protocol::{
    CompletionMessage, CompletionRequest, CompletionResult, ModelCallRole, ReasoningEffort,
};

use super::stage_budget::{
    PipelineStageAbort, budget_abort, deadline_abort, deadline_closing_abort,
};
use super::{Pipeline, ResolvedRole, RoleCallOverrides};

pub(super) struct RawCall<'r, 'a> {
    pub(super) role: ModelCallRole,
    pub(super) resolved: &'r ResolvedRole<'a>,
    pub(super) messages: Vec<CompletionMessage>,
    pub(super) policy: RetryPolicy,
    pub(super) overrides: &'r RoleCallOverrides,
    /// The per-call wall clock, normally `EngineConfig::model_timeout` — the
    /// same posture-keyed ceiling the worker path got in #1211/#1277.
    ///
    /// **Every management role passes it.** `None` means "no clock at all",
    /// and the retry policy is not one: a provider that accepts the request
    /// and then dribbles never trips a retry, so an unbounded management call
    /// parks a headless run with no exit short of a hard cancel.
    ///
    /// The reason this is a field on the shared call rather than a default
    /// inside [`Pipeline::metered_raw_call`] is that the *consequence* of the
    /// deadline is per-role, and each role already has one written down:
    /// Verdict falls back to the deterministic heuristic, DistressGuidance to
    /// evidence-only revision, Plan and PlanRepair to the single-step plan.
    /// Those fallbacks are the point. Leaving `timeout: None` did not just
    /// risk a hang — it made the fallback arm unreachable code, so the run
    /// lost the degraded result it was designed to take (#1483, #1501).
    pub(super) timeout: Option<Duration>,
}

pub(super) enum RawCallError {
    Provider,
    Timeout,
    Budget(PipelineStageAbort),
    /// The task's wall clock stopped this call before dispatch, so it was
    /// never dispatched and cost nothing: either the deadline had already
    /// passed (#2238) or too little of it remained to fit the call at all
    /// (#2432). One variant for both, because the consequence here is
    /// identical; the difference that matters reaches the stream, where
    /// `stage_budget` gives each stop its own sentence.
    ///
    /// Separate from [`Self::Budget`] because the two want opposite handling
    /// *after* execute: a dollar breach is the run being unable to afford the
    /// next call at all, while an expired clock on a run that has already
    /// produced a diff wants a pivot — skip the remaining assurance stages and
    /// settle the work that exists, so a partially-solved task is still
    /// scorable. Every match site therefore states its own answer rather than
    /// sharing the budget arm.
    Deadline(PipelineStageAbort),
}

/// Role-shaped request bounds `(max_output_tokens, effort)` for the
/// management chokepoint, applied between the caller's explicit overrides
/// (which always win) and the engine config's worker-tier base.
///
/// The base is the wrong default here: `engine.max_output_tokens` is seeded
/// from the model catalog's full ceiling (64k on current Claude rows) and an
/// unset `effort` leaves the provider's own default (high) reasoning
/// allowance in force — dispatching a role whose written output contract is
/// two to six lines with a 64k allowance and unbounded thinking. The bounded
/// shape is the one the engine's own overflow summarizer already pins
/// (`stella-core`'s `run_compaction_pass`: 1,200 tokens, `effort: Low`).
///
/// Each arm names the role's **visible-output** contract;
/// [`with_reasoning_headroom`] adds thinking room on top in
/// [`role_output_cap`], so
/// the numbers here stay readable against the prompts that justify them
/// instead of silently encoding someone's guess at a thinking budget.
///
/// Exhaustive over [`ModelCallRole`] on purpose: a new role dispatched
/// through this chokepoint must decide its bounds here, not inherit the
/// worker ceiling by omission.
fn management_bounds(role: ModelCallRole) -> (Option<u32>, Option<ReasoningEffort>) {
    match role {
        // Three-line classification, already under the L-M4 decision-latency
        // ceiling: pinned-low effort is not only cheaper, it keeps the call
        // inside the ceiling so the fast route actually gets taken.
        ModelCallRole::Triage => (Some(512), Some(ReasoningEffort::Low)),
        // A small ordered-JSON plan. Effort is pinned low, not inherited
        // (#2869): a high-effort session let a reasoning model spend its
        // entire 8k-token headroom thinking and answer with nothing, twice —
        // the starvation retry re-sends through this same closure, so an
        // inherited high effort loses the identical bet at a 3.4x larger
        // cap. Plan's output contract is three to eight short strings; that
        // does not need session-level deliberation to produce.
        ModelCallRole::Plan | ModelCallRole::PlanRepair => (Some(4096), Some(ReasoningEffort::Low)),
        // "PASS or FAIL, then one line" / "at most 6 lines" by their own
        // prompts. Effort inherited: both are judgment calls on evidence.
        ModelCallRole::Verdict | ModelCallRole::DistressGuidance => (Some(1024), None),
        // The only raw `Worker` call is the conversational fast path, whose
        // instructions say "reply briefly and warmly in plain prose".
        ModelCallRole::Worker => (Some(2048), Some(ReasoningEffort::Low)),
        // Never dispatched through this chokepoint today; if one ever is,
        // inheriting the engine base is exactly the pre-existing behavior.
        // (`Research` runs as an engine sub-agent turn, never a raw call —
        // its bounds live on its `SubAgentSpec`.)
        ModelCallRole::Unknown
        | ModelCallRole::Research
        | ModelCallRole::WitnessAuthor
        | ModelCallRole::WitnessRepair
        | ModelCallRole::AgentAuthor
        | ModelCallRole::SkillAuthor
        | ModelCallRole::DomainInference
        | ModelCallRole::Reflection
        | ModelCallRole::Summarization => (None, None),
    }
}

/// The wire cap for one management call: the caller's explicit override
/// (which always wins, unchanged), else the role's visible-output contract
/// plus [`with_reasoning_headroom`]'s thinking room, else the engine base.
///
/// The override is deliberately NOT given headroom. It is the most specific
/// statement anyone made about this call — an operator who pinned 512 asked
/// for 512, and quietly serving 4,608 would make the setting a suggestion.
/// A starved override still gets the retry below, which is the difference
/// between honoring a number and silently discarding its result.
fn role_output_cap(
    role: ModelCallRole,
    overrides: &RoleCallOverrides,
    base: Option<u32>,
) -> Option<u32> {
    if let Some(explicit) = overrides.max_output_tokens {
        return Some(explicit);
    }
    match management_bounds(role).0 {
        Some(contract) => Some(with_reasoning_headroom(contract)),
        None => base,
    }
}

impl<'a> Pipeline<'a> {
    /// One metered raw provider completion. Successful calls emit exactly one
    /// `StepUsage` before budget enforcement can return; failures/timeouts emit
    /// one content-free `UsageIncomplete`. All raw roles use this chokepoint.
    pub(super) async fn metered_raw_call(
        &self,
        call: RawCall<'_, 'a>,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<CompletionResult, RawCallError> {
        // Park BEFORE dispatch while a supervisor holds the pause gate — the
        // engines park at their own step boundaries, and this chokepoint is
        // what extends the same discipline to every management call (triage,
        // verifier, guidance, conversational). Pre-spend, never mid-call: the
        // boundary contract is the budget guard's (L-E6).
        if let Some(gate) = self.turn_gate {
            gate.wait_if_paused().await;
        }
        // Destructured, so the caller-built message list is MOVED into the
        // request instead of cloned. The conversational fast path hands this
        // the whole running transcript, so the clone was a per-call copy of
        // the entire history for no benefit — nothing reads `call` afterwards.
        let RawCall {
            role,
            resolved,
            messages,
            policy,
            overrides,
            timeout,
        } = call;
        let messages = match &overrides.prompt {
            Some(prompt) => {
                let mut with_system = Vec::with_capacity(messages.len() + 1);
                with_system.push(CompletionMessage::system(prompt.clone()));
                with_system.extend(messages);
                with_system
            }
            None => messages,
        };
        let engine = &self.config.engine;
        // The real pre-call estimate, not 0: this pairs with the provider's
        // reported usage on `StepUsage` as an estimator drift sample, and a
        // hardcoded zero recorded "estimated 0, actual N" against every
        // management call — poisoning calibration and hiding a 10k-token
        // verifier prompt from anything reading the estimate.
        let estimated_input_tokens =
            stella_core::estimator::estimate_conversation_tokens(&messages);
        let cap = role_output_cap(role, overrides, engine.max_output_tokens);
        // Kept for the starvation retry (#2128), and ONLY when one could
        // change the outcome. An earlier revision moved the list into the
        // request precisely to avoid this copy, on the grounds that nothing
        // read it afterwards — true then, and no longer: a call that comes
        // back empty because the cap was spent on reasoning is recoverable,
        // and recovering it means re-sending what it sent. One `Vec` copy of
        // a management prompt against a provider round trip is the cheaper
        // half of that trade.
        let retry_messages = starved_retry_cap(cap).map(|_| messages.clone());
        let request = |messages, max_output_tokens| CompletionRequest {
            messages,
            max_output_tokens,
            temperature: overrides.temperature.or(engine.temperature),
            effort: overrides
                .effort
                .or(management_bounds(role).1)
                .or(engine.effort),
            reasoning: overrides.reasoning.or(engine.reasoning),
            params: overrides.params.or(engine.params),
            tools: Vec::new(),
        };
        let result = self
            .dispatch_raw(
                &call_meta(role, resolved),
                request(messages, cap),
                policy,
                timeout,
                estimated_input_tokens,
                budget,
                total,
            )
            .await?;
        // A provider that stopped at the token limit with nothing visible to
        // show for it has stated that the budget ran out before the first
        // answer token — the #2128 signature. Every other outcome returns
        // here unchanged.
        let Some(raised) = starved_of_output(&result)
            .then(|| starved_retry_cap(cap))
            .flatten()
        else {
            return Ok(result);
        };
        let Some(messages) = retry_messages else {
            return Ok(result);
        };
        // Loud, never silent: before this, an empty triage collapsed the
        // research/plan/scope/witness stages to defaults that read like
        // decisions, and an empty verdict degraded to the heuristic — both
        // with nothing in the transcript naming the cause.
        self.warn(format!(
            "the {role:?} call returned no output: it stopped at its {} token limit with an \
             empty response, which on a reasoning model means the whole budget went to \
             reasoning. Retrying once at {raised}.",
            cap.map_or_else(|| "(unset)".to_string(), |c| c.to_string()),
        ));
        let retried = self
            .dispatch_raw(
                &call_meta(role, resolved),
                request(messages, Some(raised)),
                RetryPolicy::deterministic(),
                timeout,
                estimated_input_tokens,
                budget,
                total,
            )
            .await?;
        // The retry's own emptiness is not worth a third call, but it IS
        // worth saying: the caller is about to take a degraded path, and this
        // is the only place that knows why.
        if starved_of_output(&retried) {
            self.warn(format!(
                "the {role:?} retry at {raised} tokens returned no output either; this call's \
                 result is unusable and the stage will take its degraded path"
            ));
        }
        Ok(retried)
    }

    /// One metered dispatch: everything [`Pipeline::metered_raw_call`] does
    /// per attempt, so its starvation retry (#2128) re-sends through exactly
    /// the same accounting seam the first attempt used. Both attempts are
    /// real paid calls and each emits its own `StepUsage`, which is the
    /// honest record — the retry is not free and the telemetry must not
    /// imply it was.
    #[allow(clippy::too_many_arguments)]
    async fn dispatch_raw(
        &self,
        meta: &RawCallMeta<'_>,
        request: CompletionRequest,
        retry_policy: RetryPolicy,
        timeout: Option<Duration>,
        estimated_input_tokens: u64,
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Result<CompletionResult, RawCallError> {
        // The anticipatory rung (#2432). #2238 put a wall-clock check at
        // `run_accounted_call`'s pre-dispatch seam, but a reactive one: it
        // refuses only work that is ALREADY too late, so a verdict call
        // dispatched with 2s of task clock left and taking 60s overruns by 58s.
        //
        // The reserve is supplied here rather than there because that seam
        // measures nothing — no pace estimate, no per-role history — and a
        // margin invented in a place with no basis is a second, invisible
        // policy on top of the operator's `--turn-budget`, which
        // `driver::settlement::check_budget` forbids itself for exactly this
        // reason. This caller does have a basis, so it is the caller that
        // anticipates. The engine's step loop does the same thing with the
        // basis it has (`TurnState::last_step`, which it measures).
        //
        // Provenance of the number, stated because a reserve with none is the
        // invented policy above: it is the measured wall clock of the LAST
        // completed call of this same role (`super::role_pace`), the pipeline's
        // equivalent of the `TurnState::last_step` the engine forecasts from.
        // A role with no history yet reports `None` and is never refused —
        // anticipation requires a measurement, and this seam makes none up.
        //
        // The call's own `timeout` was the cheaper candidate and is wrong: it
        // is an IDLE ceiling (`model_timeout` defaults to 816s), so reserving
        // it would refuse every management call on any run whose deadline is
        // under ~14 minutes — a stop far larger than the overrun it prevents.
        //
        // An unarmed deadline is untouched: `check_deadline_with_reserve`
        // answers `Continue` when none is set, so a run nobody is timing keeps
        // #2238's behaviour exactly.
        //
        // Invariant 6 holds — this is still before dispatch, between model
        // calls, with nothing in flight to interrupt. Invariant 2 holds: the
        // clock is read here and handed in, never read inside the guard.
        if let Some(reserve) = self.role_pace.forecast(meta.role)
            && let DeadlineOutcome::Closing { remaining } =
                budget.check_deadline_with_reserve(Instant::now(), reserve)
        {
            return Err(RawCallError::Deadline(deadline_closing_abort(
                remaining, reserve,
            )));
        }
        let dispatched_at = Instant::now();
        match run_accounted_call(
            AccountedCall {
                provider: meta.provider,
                role: meta.role,
                model_hint: meta.model_id.to_string(),
                request,
                retry_policy,
                timeout,
                estimated_input_tokens,
                // Management roles assemble their own prompts — the role's task
                // prompt, an optional settings-supplied system override, and a
                // rendered transcript that exists nowhere else. This receipt is
                // the only record of what any of them actually sent.
                receipt: Some(ReceiptContext {
                    turn_instance: 0,
                    step: 0,
                    call_seq: self.raw_call_seq.fetch_add(1, Ordering::Relaxed),
                    // Phase 2 (#713): a management role rides the session's
                    // engine config, so it inherits the same lifecycle switch.
                    lifecycle_enabled: self.config.engine.lifecycle_enabled,
                }),
            },
            budget,
            &self.events,
            self.sleeper,
        )
        .await
        {
            Ok(result) => {
                *total += result.cost_usd;
                // Only a completed call teaches the forecast anything (#2432).
                // A refusal or an early death would report the failure's
                // duration rather than the work's, talking the estimate down
                // exactly when a run is in trouble.
                self.role_pace.observe(meta.role, dispatched_at.elapsed());
                // Breaker feedback (#2673): the provider served the call, so
                // the router's next resolution may trust it again.
                self.router.record_success(meta.provider.id());
                Ok(result)
            }
            // The two transport-class verdicts the breaker counts. Deadline
            // is deliberately neither: nothing was dispatched (#2238), so it
            // is a fact about the run's clock, not about the provider.
            Err(AccountedCallError::Provider(_)) => {
                self.router.record_failure(meta.provider.id());
                Err(RawCallError::Provider)
            }
            Err(AccountedCallError::Timeout) => {
                self.router.record_failure(meta.provider.id());
                Err(RawCallError::Timeout)
            }
            // Nothing was dispatched, so `total` is untouched — the run's
            // settled cost stays exactly what it was before this call.
            Err(AccountedCallError::Deadline { overrun }) => {
                Err(RawCallError::Deadline(deadline_abort(overrun)))
            }
            Err(AccountedCallError::Budget { result, outcome }) => {
                *total += result.cost_usd;
                // The completion COMMITTED and was paid for — a healthy
                // provider whose spend tripped OUR budget is a success from
                // the breaker's point of view.
                self.router.record_success(meta.provider.id());
                Err(RawCallError::Budget(
                    budget_abort(outcome).expect("budget error carries abort outcome"),
                ))
            }
        }
    }
}

/// The routing facts both attempts of a [`Pipeline::metered_raw_call`] share.
/// Borrowed rather than cloned per attempt: the retry sends to the same
/// resolved provider and model, and re-resolving could silently retry
/// somewhere else.
struct RawCallMeta<'r> {
    role: ModelCallRole,
    provider: &'r dyn stella_protocol::Provider,
    model_id: &'r str,
}

fn call_meta<'r>(role: ModelCallRole, resolved: &'r ResolvedRole<'_>) -> RawCallMeta<'r> {
    RawCallMeta {
        role,
        provider: resolved.provider,
        model_id: &resolved.model_ref.model_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #2128 witness for the prevention half: every capped role reaches the
    /// wire with reasoning headroom above its written output contract, so a
    /// reasoning model has room to think before its first answer token. The
    /// old caps (512 triage, 1024 verdict) are exactly the numbers that came
    /// back empty across a whole benchmark match.
    #[test]
    fn every_capped_role_carries_reasoning_headroom() {
        let none = RoleCallOverrides::default();
        for (role, contract) in [
            (ModelCallRole::Triage, 512),
            (ModelCallRole::Verdict, 1024),
            (ModelCallRole::DistressGuidance, 1024),
            (ModelCallRole::Plan, 4096),
            (ModelCallRole::PlanRepair, 4096),
            (ModelCallRole::Worker, 2048),
        ] {
            let cap = role_output_cap(role, &none, Some(64_000))
                .unwrap_or_else(|| panic!("{role:?} is a capped role"));
            assert_eq!(
                cap,
                with_reasoning_headroom(contract),
                "{role:?} must budget its output contract PLUS thinking room"
            );
            assert!(
                cap < 64_000,
                "{role:?} must still be far under the engine base — the cap is \
                 what keeps a runaway role call bounded"
            );
        }
    }

    /// An operator's explicit cap is honored exactly, headroom included or
    /// not: it is the most specific statement about the call, and quietly
    /// serving a larger number would make the setting a suggestion. The
    /// starvation retry is what keeps honoring it from being a silent loss.
    #[test]
    fn an_explicit_override_is_never_widened() {
        let pinned = RoleCallOverrides {
            max_output_tokens: Some(512),
            ..RoleCallOverrides::default()
        };
        assert_eq!(
            role_output_cap(ModelCallRole::Triage, &pinned, Some(64_000)),
            Some(512)
        );
    }

    /// #2869 witness: Plan and PlanRepair pin low effort instead of
    /// inheriting the session's, so a high-effort session cannot drive a
    /// reasoning model to spend the entire cap thinking and answer with
    /// nothing. Fails on the old code, where both roles inherited (`None`)
    /// and a `high`-effort session reached the wire unbounded.
    #[test]
    fn plan_roles_pin_low_effort_rather_than_inherit() {
        for role in [ModelCallRole::Plan, ModelCallRole::PlanRepair] {
            assert_eq!(
                management_bounds(role).1,
                Some(ReasoningEffort::Low),
                "{role:?} must pin low effort so an inherited high-effort session \
                 cannot spend the whole output cap on reasoning"
            );
        }
    }

    /// An uncapped role inherits the engine base, exactly as before.
    #[test]
    fn an_uncapped_role_still_inherits_the_engine_base() {
        let none = RoleCallOverrides::default();
        assert_eq!(
            role_output_cap(ModelCallRole::Reflection, &none, Some(64_000)),
            Some(64_000)
        );
    }
}
