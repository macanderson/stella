//! Sub-agents over the wire (#1297): a served turn that can delegate.
//!
//! The engine machinery has been ready the whole time —
//! [`stella_core::subagent`] carves budgets, forwards metering, bounds
//! reports, and emits a `SubAgent` event vocabulary the wire already carries.
//! What a served turn lacked was the *handle*: its tool stack is whatever
//! schemas the host advertised, and none of them is `task`, so a child could
//! not be asked for. This module is that handle, plus the caps an operator
//! needs before handing it to callers.
//!
//! # Containment is unchanged
//!
//! A child is not an escape hatch from ADR-033. It runs on the same remoted
//! ports its parent does: every model call becomes a `provider_request` frame
//! the host answers, every tool call a `tool_request`. The sidecar still
//! executes nothing and still holds no keys. What is new is only that some of
//! those frames now say `role: worker` under a *child's* agent id, which is
//! why the frame learned to carry `provider_id` and `role` at all.
//!
//! Two containment properties are structural rather than prompted:
//!
//! - **A child cannot write.** Children run behind
//!   [`stella_core::ports::ReadOnlyTools`], enforced at execution time, so a
//!   host's mutating tools are simply not in a child's schema list.
//! - **A child cannot spawn a child.** `task` is advertised only to the
//!   parent, and the read-only view a child gets does not include it. Nesting
//!   is capped at one level by construction, with no depth counter to get
//!   wrong.
//!
//! # Why a child needs its own thread here too
//!
//! Same reason as `stella-cli`'s dispatcher: `ToolExecutor::execute` is
//! `#[async_trait]` and therefore `Send`, while an engine turn's future
//! deliberately is not. A tool cannot `.await` a turn. Each child gets a fresh
//! OS thread with a current-thread runtime; everything handed across is owned
//! (`Arc`s and `Copy` values), and this task only awaits a oneshot. A child
//! that panics resolves to a refusal instead of unwinding the parent's turn.
//!
//! # What the caller sets and what the operator caps
//!
//! The caller asks (per turn): whether children are allowed at all, how much
//! they may spend in total, how many steps each may take, and which provider
//! id serves them. The operator bounds every one of those in
//! [`SubAgentPolicy`], which is server configuration a request cannot raise —
//! the same shape as `MAX_SERVED_STEPS` and the reverse-request ceiling. A
//! request asking for more than the policy allows is clamped, never refused:
//! these are resource dials, and a tuning mistake in one should not fail a
//! turn.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use stella_core::ports::{ReadOnlyTools, ToolExecutor};
use stella_core::subagent::{
    SubAgentDispatcher, SubAgentHost, SubAgentOutcome, SubAgentSpec, push_sub_agent_spend,
};
use stella_core::{BudgetGuard, Engine, EventSender};
use stella_protocol::{AgentEvent, BudgetMode, ToolOutput, ToolSchema};
use tokio::sync::mpsc;

/// Operator-side ceilings on what a served turn's sub-agents may do (#1297).
///
/// Every field bounds a caller-settable knob. `Default` is the safe posture: a
/// turn gets no children unless the deployment opted in, because sub-agents
/// spend money on the host's account and a host that has not thought about
/// that should not discover it from a request body.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SubAgentPolicy {
    /// Whether callers may request sub-agents at all. `false` — the default —
    /// makes `sub_agents` on a turn request a no-op: the `task` tool is not
    /// advertised, so the model never learns it exists.
    pub enabled: bool,
    /// Ceiling on the USD a single turn's children may spend between them.
    ///
    /// A pool rather than a per-child cap because the runaway shape is many
    /// cheap children, not one expensive one. The parent's own budget guard
    /// remains the hard ceiling regardless — this is a second, tighter bound
    /// on one category of spend.
    pub max_pool_usd: f64,
    /// Ceiling on a child's model calls.
    pub max_child_steps: usize,
}

/// The default sub-agent pool ceiling: two dollars per turn.
///
/// Generous enough that ordinary delegation never trips it, small enough that
/// a model looping on `task` cannot quietly spend a deployment's month on
/// research. The same number `stella-cli`'s dispatcher uses, deliberately: one
/// surface having a laxer default than the other is how a capability's cost
/// profile drifts between them.
pub const DEFAULT_SUB_AGENT_POOL_USD: f64 = 2.0;

/// The default ceiling on a child's model calls. Enough for a real
/// search-and-read task; small enough that a confused child cannot become a
/// work session.
pub const DEFAULT_MAX_CHILD_STEPS: usize = 16;

impl Default for SubAgentPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_pool_usd: DEFAULT_SUB_AGENT_POOL_USD,
            max_child_steps: DEFAULT_MAX_CHILD_STEPS,
        }
    }
}

impl SubAgentPolicy {
    /// The permissive posture: children allowed, at the default ceilings.
    #[must_use]
    pub fn allowed() -> Self {
        Self {
            enabled: true,
            ..Self::default()
        }
    }

    /// This policy with a different pool ceiling.
    #[must_use]
    pub fn with_pool_limit(mut self, usd: f64) -> Self {
        self.max_pool_usd = usd;
        self
    }

    /// Clamp a caller's request against this policy — the one place a request
    /// value becomes an effective one, so "the operator caps it" is a single
    /// fact rather than a rule each field remembers.
    #[must_use]
    pub fn clamp(&self, requested: SubAgentRequest) -> Option<EffectiveSubAgents> {
        if !self.enabled || !requested.enabled {
            return None;
        }
        Some(EffectiveSubAgents {
            pool_usd: requested
                .pool_limit_usd
                .map_or(self.max_pool_usd, |asked| asked.min(self.max_pool_usd))
                .max(0.0),
            child_steps: requested.max_steps.map_or(self.max_child_steps, |asked| {
                asked.clamp(1, self.max_child_steps)
            }),
            provider_id: requested.provider_id,
        })
    }
}

/// What a caller asked for on `POST /v1/turns`, before the policy sees it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SubAgentRequest {
    /// Whether this turn wants the `task` tool at all.
    pub enabled: bool,
    /// Requested pool ceiling; clamped down to the policy's.
    pub pool_limit_usd: Option<f64>,
    /// Requested per-child step cap; clamped into `1..=policy.max_child_steps`.
    pub max_steps: Option<usize>,
    /// Provider id the children's calls announce. `None` uses the turn's own —
    /// the single-model deployment, and the default.
    pub provider_id: Option<String>,
}

/// A caller's request after the operator's ceilings have been applied.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveSubAgents {
    pub pool_usd: f64,
    pub child_steps: usize,
    pub provider_id: Option<String>,
}

/// Runs sub-agents for one served turn.
///
/// Holds owned handles (`Arc`) rather than borrows because each child is moved
/// onto its own thread; the parent's turn may be dropped while a child is
/// still settling, and the child's own thread is what charges the ledger in
/// that case.
pub(crate) struct ServedSubAgents {
    provider: Arc<dyn stella_protocol::Provider>,
    tools: Arc<dyn ToolExecutor>,
    config: stella_core::EngineConfig,
    /// The turn's shared spend pool. Behind a `Mutex` because sibling `task`
    /// calls from one step run concurrently; the lock is never held across a
    /// child's run, so two siblings can each carve against the same headroom
    /// before either settles — an overshoot bounded by one child's cap and
    /// caught by the parent's guard at the next step boundary.
    pool: Arc<Mutex<BudgetGuard>>,
    /// Where a child's events go. The parent turn's own channel: a child's
    /// metering must land in the stream of the turn that asked for it, which
    /// is what lets a host meter a delegating turn at all.
    events: mpsc::UnboundedSender<AgentEvent>,
    /// The ledger the engine drains into the parent's budget at the next step
    /// boundary. A tool cannot charge the guard directly — the engine holds it
    /// mutably for the whole turn — so this indirection is what keeps a
    /// caller's budget a hard ceiling over its children's spend.
    spend: stella_core::subagent::SubAgentSpendLedger,
}

impl ServedSubAgents {
    pub(crate) fn new(
        provider: Arc<dyn stella_protocol::Provider>,
        tools: Arc<dyn ToolExecutor>,
        config: stella_core::EngineConfig,
        effective: &EffectiveSubAgents,
        events: mpsc::UnboundedSender<AgentEvent>,
        spend: stella_core::subagent::SubAgentSpendLedger,
    ) -> Self {
        Self {
            provider,
            tools,
            config,
            pool: Arc::new(Mutex::new(BudgetGuard::new(
                BudgetMode::Enforced,
                None,
                Some(effective.pool_usd),
            ))),
            events,
            spend,
        }
    }
}

#[async_trait]
impl SubAgentDispatcher for ServedSubAgents {
    async fn dispatch(&self, spec: SubAgentSpec) -> SubAgentOutcome {
        let provider = Arc::clone(&self.provider);
        let tools = Arc::clone(&self.tools);
        let pool = Arc::clone(&self.pool);
        let spend = Arc::clone(&self.spend);
        let events = self.events.clone();
        let config = self.config.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("stella-serve-subagent".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        let _ = tx.send(SubAgentOutcome::Refused {
                            reason: format!("sub-agent runtime could not be built: {err}"),
                        });
                        return;
                    }
                };
                runtime.block_on(async move {
                    // Read-only, enforced at execution time rather than by
                    // prompt — and the reason nesting is structurally capped
                    // at one level: `task` is not in this view.
                    let read_only = ReadOnlyTools::new(&*tools);
                    let engine = Engine::with_sleeper(
                        &*provider,
                        &read_only,
                        config,
                        &crate::remote::TokioSleeper,
                    );
                    // Carve from the shared pool, run, settle. The lock is
                    // taken twice and never held across the run.
                    let mut carve = {
                        let guard = pool.lock().unwrap_or_else(|p| p.into_inner());
                        *guard
                    };
                    let sender = EventSender::new(events);
                    let outcome = engine
                        .run_sub_agent_with_sender(
                            SubAgentHost::new(&*provider),
                            &spec,
                            &mut carve,
                            &sender,
                        )
                        .await;
                    // Settling is the CHILD's job, from the child's own
                    // thread: a parent hard-cancelled mid-`task` never
                    // resumes the await below, and a child that really spent
                    // money must land in a ledger regardless.
                    let cost = outcome.cost_usd();
                    if cost > 0.0 {
                        push_sub_agent_spend(&spend, cost);
                        let mut guard = pool.lock().unwrap_or_else(|p| p.into_inner());
                        guard.record_spend(cost);
                    }
                    let _ = tx.send(outcome);
                });
            });
        if let Err(err) = spawned {
            return SubAgentOutcome::Refused {
                reason: format!("sub-agent thread could not be started: {err}"),
            };
        }
        rx.await.unwrap_or_else(|_| SubAgentOutcome::Refused {
            reason: "the sub-agent thread ended without reporting".to_string(),
        })
    }
}

/// The name the model calls to delegate. Identical to the CLI's, so a prompt
/// written for one surface works on the other.
pub(crate) const TASK_TOOL: &str = "task";

/// The system prompt every child gets — the CLI's, verbatim, for the same
/// reason the goal loop's seed message is verbatim: two surfaces running one
/// capability must put the same words in front of the model.
const CHILD_SYSTEM_PROMPT: &str = "You are a research sub-agent. You have been given one \
     specific question by a parent agent that cannot see anything you do — only your final \
     message reaches it. Use your read-only tools to investigate thoroughly; being exhaustive \
     costs the parent nothing, because your intermediate work is discarded. Then answer in \
     one dense paragraph (plus a short code excerpt or file:line list where that IS the \
     answer). Report what you FOUND, with concrete paths and identifiers — never what you \
     did, and never a plan. If you could not determine the answer, say so plainly and state \
     what you ruled out; a confident wrong answer is far worse than an honest gap.";

/// Ceiling on the characters a child may hand back — the context-economy
/// guarantee in mechanism form, and deliberately not model-settable.
const REPORT_CHARS: usize = 8_000;

/// The host's tool set with `task` layered on top (#1297).
///
/// Everything the host advertised passes through to the remoted executor
/// unchanged; only `task` is executed here, by dispatching a child. A wrapper
/// rather than a registry because serve owns no registry — its tools are
/// schemas the host supplied, and this is the one tool the *engine* implements.
pub(crate) struct DelegatingTools<'a> {
    inner: &'a dyn ToolExecutor,
    dispatcher: Arc<dyn SubAgentDispatcher>,
    child_steps: usize,
    /// The ledger children settle into. Drained through
    /// [`ToolExecutor::drain_sub_agent_spend_usd`] at the engine's next
    /// step-boundary budget check, which is what keeps the caller's `budget`
    /// a hard ceiling over spend its children incurred.
    spend: stella_core::subagent::SubAgentSpendLedger,
}

impl<'a> DelegatingTools<'a> {
    pub(crate) fn new(
        inner: &'a dyn ToolExecutor,
        dispatcher: Arc<dyn SubAgentDispatcher>,
        child_steps: usize,
        spend: stella_core::subagent::SubAgentSpendLedger,
    ) -> Self {
        Self {
            inner,
            dispatcher,
            child_steps,
            spend,
        }
    }

    /// The `task` schema, as the model sees it.
    fn task_schema() -> ToolSchema {
        ToolSchema {
            name: TASK_TOOL.into(),
            description: "Delegate a self-contained research question to a sub-agent that \
                 investigates with read-only tools and returns only its findings. Its \
                 intermediate work never enters this conversation, so use it when answering \
                 would otherwise mean reading many files you do not need to keep. Not for work \
                 that must edit files (the sub-agent cannot write), and not for a single lookup \
                 you already know the location of."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "A 3-6 word label for this delegation."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The question, stated in full. The sub-agent sees ONLY \
                             this — it cannot read the conversation, so include every name, \
                             path and constraint it needs."
                    }
                },
                "required": ["description", "prompt"],
                "additionalProperties": false
            }),
            // Truthfully not read-only: it spends money.
            read_only: false,
            speculation_safe: false,
        }
    }
}

#[async_trait]
impl ToolExecutor for DelegatingTools<'_> {
    fn schemas(&self) -> Vec<ToolSchema> {
        let mut schemas = self.inner.schemas();
        // Appended, not inserted: a host that already advertises its own
        // `task` keeps it, and the engine dispatches by name to whichever the
        // model picked — which is the host's, since `execute` below only
        // claims the name when the host did not.
        if !schemas.iter().any(|schema| schema.name == TASK_TOOL) {
            schemas.push(Self::task_schema());
        }
        schemas
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        if name != TASK_TOOL || self.inner.schemas().iter().any(|s| s.name == TASK_TOOL) {
            return self.inner.execute(name, input).await;
        }
        let Some(prompt) = input
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
        else {
            return ToolOutput::Error {
                message: "missing required string field `prompt` — the sub-agent cannot see \
                     this conversation, so the question must be self-contained"
                    .into(),
            };
        };
        let description = input
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .unwrap_or("sub-agent");
        let spec = SubAgentSpec {
            agent_id: slug(description),
            system_prompt: Some(CHILD_SYSTEM_PROMPT.to_string()),
            instruction: prompt.to_string(),
            max_steps: self.child_steps,
            max_report_chars: REPORT_CHARS,
            // Not model-settable: this tool delegates *research*. A child that
            // could edit would need the parent's review gates around it.
            write_access: false,
            depth: 1,
            ..SubAgentSpec::default()
        };
        render(&self.dispatcher.dispatch(spec).await)
    }

    fn drain_sub_agent_spend_usd(&self) -> f64 {
        // Both halves: the host's executor may have its own ledger, and the
        // children dispatched here push onto the one the session owns. Losing
        // either would let spend escape the parent's guard.
        self.inner.drain_sub_agent_spend_usd()
            + stella_core::subagent::drain_sub_agent_spend(&self.spend)
    }
}

/// Turn a child's outcome into a model-visible result.
///
/// An incomplete child is an `Ok` carrying its partial finding plus a plain
/// statement of what stopped it — not an `Error`. The salvaged text is real
/// evidence the parent paid for, and burying it in an error invites the model
/// to discard it and redo the work.
fn render(outcome: &SubAgentOutcome) -> ToolOutput {
    match outcome {
        SubAgentOutcome::Completed(report) => ToolOutput::Ok {
            content: if report.truncated {
                format!(
                    "{}\n\n[report truncated at {REPORT_CHARS} characters — ask a narrower \
                     question if the answer was cut off]",
                    report.summary
                )
            } else {
                report.summary.clone()
            },
        },
        SubAgentOutcome::Incomplete { report, reason } if !report.summary.is_empty() => {
            ToolOutput::Ok {
                content: format!(
                    "{}\n\n[partial: the sub-agent stopped before finishing — {reason}. Treat \
                     the above as incomplete evidence, not a final answer.]",
                    report.summary
                ),
            }
        }
        SubAgentOutcome::Incomplete { reason, .. } => ToolOutput::Error {
            message: format!(
                "the sub-agent stopped before producing anything: {reason} — do the work \
                 directly, or ask a narrower question"
            ),
        },
        SubAgentOutcome::Refused { reason } => ToolOutput::Error {
            message: format!(
                "the sub-agent was not started: {reason} — do the work directly with your \
                 own tools"
            ),
        },
    }
}

/// A short, stable, filesystem-safe id from the model's description, for event
/// attribution.
fn slug(description: &str) -> String {
    let mut out = String::new();
    for ch in description.chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 32 {
            break;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "sub-agent".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The operator's ceilings bound every caller-settable knob, and a
    /// deployment that has not opted in gets no children whatever the request
    /// asks for.
    #[test]
    fn the_policy_caps_what_a_caller_may_ask_for() {
        let policy = SubAgentPolicy::allowed().with_pool_limit(1.0);
        let asked = SubAgentRequest {
            enabled: true,
            pool_limit_usd: Some(500.0),
            max_steps: Some(10_000),
            provider_id: Some("cheap-model".into()),
        };
        let effective = policy.clamp(asked.clone()).expect("children are allowed");
        assert_eq!(
            effective.pool_usd, 1.0,
            "the pool ceiling is the operator's"
        );
        assert_eq!(
            effective.child_steps, DEFAULT_MAX_CHILD_STEPS,
            "the step ceiling is the operator's"
        );
        assert_eq!(
            effective.provider_id.as_deref(),
            Some("cheap-model"),
            "which model serves children is the caller's to choose"
        );

        assert!(
            SubAgentPolicy::default().clamp(asked.clone()).is_none(),
            "a deployment that has not opted in gets no children"
        );
        assert!(
            policy
                .clamp(SubAgentRequest {
                    enabled: false,
                    ..asked
                })
                .is_none(),
            "and a caller that did not ask gets none either"
        );
    }

    /// A request below the ceilings is honored as written — the policy is a
    /// bound, not an override.
    #[test]
    fn a_modest_request_is_honored_verbatim() {
        let effective = SubAgentPolicy::allowed()
            .clamp(SubAgentRequest {
                enabled: true,
                pool_limit_usd: Some(0.25),
                max_steps: Some(4),
                provider_id: None,
            })
            .expect("allowed");
        assert_eq!(effective.pool_usd, 0.25);
        assert_eq!(effective.child_steps, 4);
        assert_eq!(effective.provider_id, None);
    }

    /// Zero steps is not a request for a zero-step child (which would abort
    /// with a misleading "reached the step cap (0)") — it floors at one, the
    /// same reading `validate_max_steps` gives the turn-level knob.
    #[test]
    fn a_zero_step_request_floors_at_one() {
        let effective = SubAgentPolicy::allowed()
            .clamp(SubAgentRequest {
                enabled: true,
                max_steps: Some(0),
                ..SubAgentRequest::default()
            })
            .expect("allowed");
        assert_eq!(effective.child_steps, 1);
    }
}
