//! Durable adapter for paid one-shot calls that are not part of an engine turn.
//!
//! This is the standalone counterpart to `stella-pipeline`'s `metered_raw_call`
//! chokepoint, and it carries the same two mechanisms for the same reason.
//! Every call through here is a bounded call with a written output contract —
//! a JSON lessons array, an authored manifest, a domain list — and a reasoning
//! model bills its thinking against the one `max_output_tokens` number on the
//! wire. #2128 taught the pipeline to notice that and left this path untouched,
//! so post-turn reflection kept dispatching a reasoning model against a bare
//! 2,048-token cap; it came back empty with `finish_reason: length`, which
//! parses to zero lessons exactly like a turn with nothing to learn, and the
//! context lifecycle sat frozen for nine days (#2174).
//!
//! In the same order the pipeline applies them:
//!
//! 1. [`standalone_bounds`] declares each role's written-output contract, and
//!    [`role_output_cap`] pads it with thinking room. Prevention — it makes
//!    starvation rare. #2174 gave reflection this and left the other three
//!    roles each carrying whatever number their own call site picked, which is
//!    the state reflection had just been rescued from (#2444).
//! 2. The starvation retry below recovers the calls that starve anyway. That
//!    is the actual guarantee.

use std::path::Path;
use std::time::Duration;

use stella_core::starvation::{starved_of_output, starved_retry_cap, with_reasoning_headroom};
use stella_core::{
    AccountedCall, AccountedCallError, BudgetGuard, RetryPolicy, run_accounted_call,
};
use stella_protocol::{
    AgentEvent, CompletionRequest, CompletionResult, ModelCallRole, Provider, ReasoningEffort,
};
use stella_store::Store;
use tokio::sync::mpsc;

use crate::agent;
use crate::runtime::TokioSleeper;

#[derive(Debug)]
pub(crate) struct StandaloneCompletion {
    pub(crate) result: CompletionResult,
    pub(crate) cost_usd: f64,
    pub(crate) events: Vec<AgentEvent>,
}

#[derive(Debug)]
pub(crate) struct StandaloneCallError {
    pub(crate) message: String,
    pub(crate) cost_usd: f64,
    pub(crate) events: Vec<AgentEvent>,
}

/// Role-shaped request bounds `(visible output, effort)` for the standalone
/// chokepoint — the counterpart to `stella-pipeline`'s `management_bounds`
/// (`crates/stella-pipeline/src/pipeline/raw_usage.rs`), and exhaustive over
/// [`ModelCallRole`] for the same reason: a new standalone role must decide
/// its bounds here rather than inherit whatever number its call site happened
/// to pick in isolation.
///
/// Each arm names the role's **visible-output** contract; the thinking room a
/// reasoning model spends before writing any of it is added on top by
/// [`with_reasoning_headroom`] in [`role_output_cap`]. So the numbers here stay
/// readable against the prompts that justify them (transcribed in
/// `docs/prompts/`) instead of silently encoding someone's guess at a reasoning
/// budget.
///
/// The three authoring/inference roles inherit `effort` rather than pinning it
/// low the way `management_bounds` pins triage. That is a decision, not an
/// omission: triage writes a three-line classification whose value is the
/// routing choice, while these three roles' *product* is the written artifact.
/// Buying less deliberation for a call whose output is another role's system
/// prompt is a quality change nobody has measured, and this chokepoint's
/// concern is bounds. Reflection keeps the low pin it already had (#2174).
fn standalone_bounds(role: ModelCallRole) -> (Option<u32>, Option<ReasoningEffort>) {
    match role {
        // A whole agent definition: three frontmatter lines plus a body that
        // its own prompt requires be "a complete, self-contained system
        // prompt" — this role's output IS another role's system prompt
        // (`docs/prompts/agent-author.md`). Sized against real definitions of
        // exactly that shape rather than a guess: five sampled files span
        // 3.0-9.0 KB, so ~870-2,590 tokens at the estimator's 3.5 bytes each.
        // The 1,200 this call site sent is *below the median artifact it asks
        // for*, and that was the whole budget — reasoning is billed against
        // the same number, so the visible half was smaller still.
        ModelCallRole::AgentAuthor => (Some(4_096), None),
        // One `SKILL.md`: frontmatter plus "a concise markdown body". Same
        // measurement, four sampled single-file skills spanning 2.4-10 KB, so
        // ~700-2,855 tokens; the 1,200 sent here truncates three of the four.
        ModelCallRole::SkillAuthor => (Some(4_096), None),
        // A 4-10 element JSON array — name, one-line description, and a short
        // path list per domain (`docs/prompts/domain-inference.md`). 2,048 is
        // the number `infer_domains` already chose and it fits the contract;
        // what it lacked was room to think before the first `[`.
        //
        // `ingest_cmd/extract.rs` shares this role for document extraction,
        // whose output is a whole claim list sized to the source file. It
        // states its own cap and therefore keeps it — see [`role_output_cap`].
        ModelCallRole::DomainInference => (Some(2_048), None),
        // At most three short lesson objects plus a five-field self-review.
        // 512 was the original and was enough only for a model that answers
        // with bare JSON; one that narrates first spends the allowance on
        // prose and never reaches the array, losing every lesson from every
        // turn — silently, because a truncated response parses to zero lessons
        // exactly like an empty one. Pinned low because the written contract
        // is that small and an unset effort leaves the provider's default
        // allowance in force: unbounded thinking spent deciding, most turns,
        // to return an empty list.
        ModelCallRole::Reflection => (Some(2_048), Some(ReasoningEffort::Low)),
        // Never dispatched through this chokepoint today. If one ever is,
        // inheriting the caller's request unchanged is exactly the behaviour
        // it has now — but the arm is spelled out rather than swept into a
        // catch-all so that adding a standalone role is a decision someone
        // makes on purpose.
        ModelCallRole::Unknown
        | ModelCallRole::Triage
        | ModelCallRole::Research
        | ModelCallRole::Plan
        | ModelCallRole::PlanRepair
        | ModelCallRole::WitnessAuthor
        | ModelCallRole::WitnessRepair
        | ModelCallRole::Worker
        | ModelCallRole::DistressGuidance
        | ModelCallRole::Verdict
        | ModelCallRole::Summarization => (None, None),
    }
}

/// The wire cap for one standalone call: the caller's own cap (which always
/// wins, unchanged), else the role's visible-output contract plus
/// [`with_reasoning_headroom`]'s thinking room, else nothing.
///
/// The caller's cap is deliberately given no headroom, for the reason
/// `role_output_cap` states on the pipeline side: it is the most specific
/// statement anyone made about this call, and quietly serving a larger number
/// would make it a suggestion. The starvation retry below is what keeps
/// honouring it from being a silent loss.
///
/// **What "the caller's own cap" means here is narrower than on the pipeline
/// side, and the difference is the whole design.** `metered_raw_call` has two
/// channels — an operator's `RoleCallOverrides` and the role default — so it
/// can tell a deliberate pin from an absent one. This chokepoint takes a
/// fully-built `CompletionRequest`, where one field carries both meanings. So
/// `None` is the statement "I have no per-call reason for a number, use my
/// role's contract", and a call site states a cap only when it has one:
/// `ingest_cmd/extract.rs` sizes its first request to the document and doubles
/// it on truncation (#2226), which no per-role constant could express. Every
/// other standalone call site was picking a number in isolation, which is
/// the defect (#2444).
fn role_output_cap(role: ModelCallRole, caller: Option<u32>) -> Option<u32> {
    if let Some(explicit) = caller {
        return Some(explicit);
    }
    standalone_bounds(role).0.map(with_reasoning_headroom)
}

/// Fill a request's unstated bounds from its role's declared contract.
///
/// Pure over owned data, so the precedence is testable without a provider, a
/// store, or a tokio runtime.
fn with_role_bounds(role: ModelCallRole, mut request: CompletionRequest) -> CompletionRequest {
    request.max_output_tokens = role_output_cap(role, request.max_output_tokens);
    // Same precedence, same reason: an effort the caller stated is the more
    // specific statement about this call. Reflection relies on exactly this —
    // it forwards the operator's configured triage posture, and the role pin
    // fills in only when none was configured (#1847).
    request.effort = request.effort.or(standalone_bounds(role).1);
    request
}

pub(crate) async fn complete_standalone(
    workspace_root: &Path,
    provider: &dyn Provider,
    role: ModelCallRole,
    kind: &str,
    model_hint: &str,
    budget_limit: Option<f64>,
    request: CompletionRequest,
) -> Result<StandaloneCompletion, StandaloneCallError> {
    // Before anything reads the request, and in particular before the
    // starvation retry is planned below: `starved_retry_cap` declines to retry
    // an unset cap, so a role whose contract is applied here is also a role
    // whose starved call becomes recoverable.
    let request = with_role_bounds(role, request);
    let store = Store::open(workspace_root).map_err(|error| StandaloneCallError {
        message: format!("accounting store unavailable before model dispatch: {error}"),
        cost_usd: 0.0,
        events: Vec::new(),
    })?;
    let execution_id = store
        .begin_execution(
            kind,
            "content-free system operation",
            provider.id(),
            model_hint,
        )
        .map_err(|error| StandaloneCallError {
            message: format!("accounting execution unavailable before model dispatch: {error}"),
            cost_usd: 0.0,
            events: Vec::new(),
        })?;
    let mut budget: BudgetGuard = agent::build_budget_guard(budget_limit);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let events = stella_core::EventSender::new(tx.clone());
    // Kept for the starvation retry, and ONLY when one could change the
    // outcome — the same trade `metered_raw_call` makes: one `Vec` copy of a
    // bounded prompt against a provider round trip whose result would
    // otherwise be discarded as "the model had nothing to say".
    let raised = starved_retry_cap(request.max_output_tokens);
    let retry = raised.map(|raised| {
        // The retry is the SAME request with one number changed. Every tuning
        // field rides along: a retry that quietly dropped the pinned effort
        // would be a different call answering a different question.
        (
            raised,
            request.messages.clone(),
            request.tools.clone(),
            (request.temperature, request.effort, request.reasoning),
            request.params,
        )
    });
    let mut outcome = dispatch(provider, role, model_hint, request, &mut budget, &events).await;
    // What a superseded attempt already spent. Every arm below adds it, because
    // the retry is a real paid call: reporting only the surviving attempt's
    // cost would understate the turn by exactly the amount the starvation cost
    // us, which is the number an operator most wants to see.
    let mut superseded_cost = 0.0;
    // A provider that stopped at the token limit with nothing visible to show
    // for it has stated that the budget ran out before the first answer token.
    // Every other outcome falls through unchanged. The retry emits its own
    // `StepUsage` — the honest record, since it is not free and the telemetry
    // must not imply it was.
    if let (Ok(result), Some((raised, messages, tools, (temperature, effort, reasoning), params))) =
        (&outcome, retry)
        && starved_of_output(result)
    {
        superseded_cost = result.cost_usd;
        outcome = dispatch(
            provider,
            role,
            model_hint,
            CompletionRequest {
                messages,
                max_output_tokens: Some(raised),
                temperature,
                effort,
                tools,
                reasoning,
                params,
            },
            &mut budget,
            &events,
        )
        .await;
    }
    // BOTH senders, and the order is not cosmetic: `events` holds a clone of
    // `tx`, so dropping only `tx` leaves the channel open and the drain below
    // blocks forever on a `recv()` that can never return `None`. Before the
    // starvation retry the sender was built inline at the single dispatch and
    // died with that statement; naming it so two dispatches can share it is
    // what made the lifetime explicit — and this the explicit end of it.
    drop(events);
    drop(tx);
    let mut persistence_complete = true;
    let mut seq = 0;
    let mut settled_events = Vec::new();
    while let Some(event) = rx.recv().await {
        persistence_complete &=
            agent::persist_event(&store, execution_id, seq, &event, provider.id());
        settled_events.push(event);
        seq += 1;
    }
    match outcome {
        Ok(result) => {
            let cost_usd = superseded_cost + result.cost_usd;
            let complete = persistence_complete
                && store
                    .finish_execution_accounted(
                        execution_id,
                        "completed",
                        cost_usd,
                        persistence_complete,
                    )
                    .is_ok();
            if !complete {
                return Err(StandaloneCallError {
                    message: "model call settled but its accounting closeout failed".into(),
                    cost_usd,
                    events: settled_events,
                });
            }
            Ok(StandaloneCompletion {
                result,
                cost_usd,
                events: settled_events,
            })
        }
        Err(AccountedCallError::Budget { result, .. }) => {
            let cost_usd = superseded_cost + result.cost_usd;
            let _ = store.finish_execution_accounted(
                execution_id,
                "aborted",
                cost_usd,
                persistence_complete,
            );
            Err(StandaloneCallError {
                message: "model call settled over the configured budget".into(),
                cost_usd,
                events: settled_events,
            })
        }
        Err(AccountedCallError::Provider(error)) => {
            let _ =
                store.finish_execution_accounted(execution_id, "failed", superseded_cost, false);
            Err(StandaloneCallError {
                message: error.to_string(),
                cost_usd: superseded_cost,
                events: settled_events,
            })
        }
        Err(AccountedCallError::Timeout) => {
            let _ =
                store.finish_execution_accounted(execution_id, "failed", superseded_cost, false);
            Err(StandaloneCallError {
                message: "model call timed out".into(),
                cost_usd: superseded_cost,
                events: settled_events,
            })
        }
        // Unreachable in practice — this path builds its guard from
        // `build_budget_guard`, which arms no task deadline — but stated
        // rather than swept into a catch-all, so arming one here later is a
        // decision someone makes on purpose.
        Err(AccountedCallError::Deadline { .. }) => {
            let _ = store.finish_execution_accounted(execution_id, "aborted", 0.0, false);
            Err(StandaloneCallError {
                message: "model call skipped: the task deadline had already passed".into(),
                cost_usd: 0.0,
                events: settled_events,
            })
        }
    }
}

/// One metered dispatch, so the starvation retry re-sends through exactly the
/// accounting seam the first attempt used.
async fn dispatch(
    provider: &dyn Provider,
    role: ModelCallRole,
    model_hint: &str,
    request: CompletionRequest,
    budget: &mut BudgetGuard,
    events: &stella_core::EventSender,
) -> Result<CompletionResult, AccountedCallError> {
    run_accounted_call(
        AccountedCall {
            provider,
            role,
            model_hint: model_hint.to_string(),
            request,
            retry_policy: RetryPolicy::deterministic(),
            timeout: Some(Duration::from_secs(120)),
            estimated_input_tokens: 0,
            receipt: None,
        },
        budget,
        events,
        &TokioSleeper,
    )
    .await
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use stella_protocol::{CompletionRequestRef, CompletionUsage, ProviderError};

    use super::*;

    struct PaidProvider;

    #[async_trait]
    impl Provider for PaidProvider {
        fn id(&self) -> &str {
            "paid-test"
        }

        async fn complete_ref(
            &self,
            _request: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            Ok(CompletionResult {
                text: "[]".into(),
                tool_calls: Vec::new(),
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 10,
                    output_tokens: 2,
                    ..CompletionUsage::default()
                },
                model: "paid-model".into(),
                cost_usd: 0.0125,
                finish_reason: None,
            })
        }
    }

    fn request() -> CompletionRequest {
        CompletionRequest {
            messages: Vec::new(),
            max_output_tokens: Some(32),
            temperature: None,
            effort: None,
            tools: Vec::new(),
            reasoning: None,
            params: None,
        }
    }

    #[tokio::test]
    async fn all_four_standalone_paid_call_sites_return_and_persist_exact_cost() {
        let root = tempfile::tempdir().expect("root");
        for (role, kind) in [
            (ModelCallRole::AgentAuthor, "agent_author"),
            (ModelCallRole::SkillAuthor, "skill_author"),
            (ModelCallRole::DomainInference, "domain_inference"),
            (ModelCallRole::Reflection, "reflection"),
        ] {
            let outcome = complete_standalone(
                root.path(),
                &PaidProvider,
                role,
                kind,
                "paid-model",
                None,
                request(),
            )
            .await
            .expect("accounted call");
            assert_eq!(outcome.cost_usd, 0.0125);
            assert!(outcome.events.iter().any(|event| matches!(
                event,
                AgentEvent::StepUsage { role: actual, .. } if actual == &role
            )));
        }

        let store = Store::open(root.path()).expect("store");
        assert_eq!(store.count("telemetry").expect("telemetry count"), 4);
        let json = store
            .export_all_json()
            .expect("export")
            .into_iter()
            .find_map(|(table, json)| (table == "telemetry").then_some(json))
            .expect("telemetry");
        for role in [
            "agent_author",
            "skill_author",
            "domain_inference",
            "reflection",
        ] {
            assert!(json.contains(role), "missing persisted role {role}: {json}");
        }
    }

    /// Records the request shape that actually reached the wire.
    #[derive(Default)]
    struct CapturingProvider {
        shape: std::sync::Mutex<Vec<(Option<u32>, Option<ReasoningEffort>)>>,
    }

    #[async_trait]
    impl Provider for CapturingProvider {
        fn id(&self) -> &str {
            "capturing"
        }

        async fn complete_ref(
            &self,
            request: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            self.shape
                .lock()
                .expect("shape lock")
                .push((request.max_output_tokens, request.effort));
            Ok(CompletionResult {
                text: "[]".into(),
                tool_calls: Vec::new(),
                // `reported: true` or the usage event lands incomplete and the
                // accounting closeout fails the call before any assertion runs.
                usage: CompletionUsage {
                    reported: true,
                    output_tokens: 2,
                    ..CompletionUsage::default()
                },
                model: "capturing".into(),
                cost_usd: 0.0,
                finish_reason: None,
            })
        }
    }

    /// #2444 witness: every standalone role reaches the provider with a cap
    /// strictly above its written-output contract, so a reasoning model has
    /// room to think before its first visible token.
    ///
    /// Before this, exactly one of the four had that property. The other three
    /// each carried whatever number their call site picked in isolation —
    /// agent and skill authoring at 1,200, which is *below the median artifact
    /// each is asked to write* — and a reasoning model bills its thinking
    /// against that same number, so the visible half was smaller still. That is
    /// the state reflection was in when the learning plane froze for nine days
    /// (#2174); this asserts the other three are out of it.
    #[tokio::test]
    async fn every_standalone_role_reaches_the_wire_with_room_to_think() {
        for (role, kind, contract) in [
            (ModelCallRole::AgentAuthor, "agent_author", 4_096),
            (ModelCallRole::SkillAuthor, "skill_author", 4_096),
            (ModelCallRole::DomainInference, "domain_inference", 2_048),
            (ModelCallRole::Reflection, "reflection", 2_048),
        ] {
            let root = tempfile::tempdir().expect("root");
            let provider = CapturingProvider::default();
            complete_standalone(
                root.path(),
                &provider,
                role,
                kind,
                "capturing",
                None,
                CompletionRequest {
                    // The call site states no cap: it has no per-call reason
                    // for one, so the role's declared contract governs.
                    max_output_tokens: None,
                    ..request()
                },
            )
            .await
            .expect("accounted call");

            let (cap, _) = provider.shape.lock().expect("shape lock")[0];
            let cap = cap.unwrap_or_else(|| {
                panic!(
                    "{role:?} dispatched with NO cap at all — its contract is \
                     declared but never applied"
                )
            });
            assert_eq!(
                cap,
                with_reasoning_headroom(contract),
                "{role:?} must budget its written contract PLUS thinking room"
            );
            assert!(
                cap > contract,
                "{role:?} sent its written contract alone ({contract}), which \
                 is the #2174 defect itself"
            );
        }
    }

    /// The one call site with a per-call reason for its number keeps it.
    ///
    /// `ingest_cmd/extract.rs` shares `DomainInference` and sizes its request
    /// to the document, doubling on truncation — a per-document ladder no
    /// per-role constant can express. Widening it to the role contract would
    /// *shrink* a large document's budget from up to 131,072 down to 6,144 and
    /// silently cut every long extraction in half.
    #[tokio::test]
    async fn a_call_site_that_states_its_own_cap_is_never_widened() {
        let root = tempfile::tempdir().expect("root");
        let provider = CapturingProvider::default();
        complete_standalone(
            root.path(),
            &provider,
            ModelCallRole::DomainInference,
            "ingest_extraction",
            "capturing",
            None,
            CompletionRequest {
                max_output_tokens: Some(65_536),
                ..request()
            },
        )
        .await
        .expect("accounted call");
        assert_eq!(
            provider.shape.lock().expect("shape lock")[0].0,
            Some(65_536),
            "a stated cap is the most specific statement about the call and \
             must reach the wire unchanged"
        );
    }

    /// The role's effort pin fills an unstated effort and yields to a stated
    /// one — the precedence reflection's operator posture depends on (#1847).
    #[test]
    fn a_stated_effort_outranks_the_role_pin() {
        let unstated = with_role_bounds(
            ModelCallRole::Reflection,
            CompletionRequest {
                effort: None,
                ..request()
            },
        );
        assert_eq!(unstated.effort, Some(ReasoningEffort::Low));
        let stated = with_role_bounds(
            ModelCallRole::Reflection,
            CompletionRequest {
                effort: Some(ReasoningEffort::High),
                ..request()
            },
        );
        assert_eq!(
            stated.effort,
            Some(ReasoningEffort::High),
            "an operator's configured posture outranks the role's own default"
        );
    }

    /// A role that does not dispatch through here is forwarded untouched — the
    /// arm exists so adding one is a decision, not so it changes behaviour.
    #[test]
    fn a_role_outside_this_chokepoint_is_forwarded_unchanged() {
        assert_eq!(role_output_cap(ModelCallRole::Triage, None), None);
        assert_eq!(role_output_cap(ModelCallRole::Worker, None), None);
    }

    /// #2174 witness: a call whose whole budget went to reasoning is retried
    /// with real room, and both attempts are paid for honestly.
    ///
    /// The provider here reproduces the shape execution 63 actually returned:
    /// empty text with `finish_reason: length`, which every caller reads as
    /// "the model had nothing to say". `metered_raw_call` learned to recognise
    /// it in #2128; this chokepoint did not, so post-turn reflection kept
    /// discarding a recoverable call and the context lifecycle sat frozen for
    /// nine days with every surface reporting health.
    #[tokio::test]
    async fn a_call_starved_by_its_own_reasoning_is_retried_with_room() {
        use std::sync::atomic::{AtomicU32, Ordering};

        /// Starves on the first attempt, answers on the second — and records
        /// the cap it was asked for each time.
        #[derive(Default)]
        struct StarvingProvider {
            attempts: AtomicU32,
            caps: std::sync::Mutex<Vec<Option<u32>>>,
        }

        #[async_trait]
        impl Provider for StarvingProvider {
            fn id(&self) -> &str {
                "starving"
            }

            async fn complete_ref(
                &self,
                request: CompletionRequestRef<'_>,
            ) -> Result<CompletionResult, ProviderError> {
                self.caps
                    .lock()
                    .expect("caps lock")
                    .push(request.max_output_tokens);
                let first = self.attempts.fetch_add(1, Ordering::SeqCst) == 0;
                Ok(CompletionResult {
                    text: if first { String::new() } else { "[]".into() },
                    tool_calls: Vec::new(),
                    usage: CompletionUsage {
                        reported: true,
                        output_tokens: 2,
                        ..CompletionUsage::default()
                    },
                    model: "starving-model".into(),
                    cost_usd: 0.01,
                    finish_reason: first.then_some(stella_protocol::FinishReason::Length),
                })
            }
        }

        let root = tempfile::tempdir().expect("root");
        let provider = StarvingProvider::default();
        let outcome = complete_standalone(
            root.path(),
            &provider,
            ModelCallRole::Reflection,
            "reflection",
            "starving-model",
            None,
            CompletionRequest {
                max_output_tokens: Some(6_144),
                ..request()
            },
        )
        .await
        .expect("the retry answers");

        assert_eq!(
            outcome.result.text, "[]",
            "the caller must receive the RETRY's answer, not the empty first \
             one — an empty lessons array and an unread one are the same bytes"
        );
        assert_eq!(
            *provider.caps.lock().expect("caps lock"),
            vec![
                Some(6_144),
                Some(stella_core::starvation::STARVED_RETRY_CAP)
            ],
            "the retry must buy real room; re-sending the same cap would only \
             starve again"
        );
        assert!(
            (outcome.cost_usd - 0.02).abs() < f64::EPSILON,
            "both attempts are paid calls and the reported cost must say so — \
             charging for one understates the turn by exactly what the \
             starvation cost: {}",
            outcome.cost_usd
        );
    }

    #[tokio::test]
    async fn over_limit_call_persists_exact_cost_before_model_output_can_apply() {
        let root = tempfile::tempdir().expect("root");
        let error = complete_standalone(
            root.path(),
            &PaidProvider,
            ModelCallRole::SkillAuthor,
            "skill_author",
            "paid-model",
            Some(0.001),
            request(),
        )
        .await
        .expect_err("settled call exceeds guard");
        assert_eq!(error.cost_usd, 0.0125);
        let store = Store::open(root.path()).expect("store");
        let rollup = store
            .execution_rollup(1, root.path())
            .expect("rollup")
            .expect("execution");
        assert_eq!(rollup.cost_usd, 0.0125);
        assert_eq!(rollup.outcome, "aborted");
    }
}
