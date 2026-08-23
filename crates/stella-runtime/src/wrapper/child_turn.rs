//! `child_turn` — the host spends a model call a plugin never makes itself.
//!
//! `doc:turn-loop-wrappers` §9.3, which named this port and left it as design
//! for as long as it existed:
//!
//! > *A wrapper is handed a `ChildTurn` port, not a provider, not an `Engine`,
//! > and not a credential. It names a **role intent** (`triage`, `planner`,
//! > `witness_author`); the host resolves the intent against the user's BYOK
//! > providers, carves the budget, attaches gate/steering/hooks, runs the turn,
//! > and settles once. For an out-of-process wrapper this is a JSON request on
//! > stdio and every model call is made by the host — invariant #3 and #3245 §3,
//! > intact.*
//!
//! Every clause of that is a requirement, and this module is where each one is
//! either enforced or declared missing.
//!
//! # The port is the one the host already has
//!
//! [`ChildTurns`] performs the capability over
//! [`SubAgentDispatcher`] — the sub-agent
//! primitive `[subloop]` is already described in terms of, and the same one
//! `task_assign` runs on. That is deliberate rather than convenient: a *new*
//! port would have needed a second implementation of budget carving, spend
//! settlement, depth limiting and read-only tooling, and the second copy is
//! where the two would drift. A host that can dispatch a sub-agent can serve
//! `child_turn`, and it gets the sub-agent contract's guarantees for free:
//!
//! - **the budget is carved, never separate** — `SubAgentSpec::budget_usd` is a
//!   *request*, clamped to the parent's remaining headroom by
//!   `BudgetGuard::carve`, so a plugin cannot spend a session past its ceiling;
//! - **the child is read-only** — [`SubAgentSpec::read_only`] runs it behind
//!   `ReadOnlyTools`, enforced at execution rather than by prompt, so
//!   `child_turn` is not a way around the tool policy;
//! - **the report is clamped** — what crosses back to the plugin is the child's
//!   answer under `max_report_chars`, not its transcript;
//! - **gate, steering and hooks attach** — the dispatcher is contractually
//!   required to read the current turn's `TurnControls` and hand them to the
//!   engine it builds (`SubAgentDispatcher`'s "Interruption" section), so a
//!   paused session does not keep spending inside a plugin's child.
//!
//! # What a plugin may say, and what it may not
//!
//! [`ChildTurnArgs`] has exactly two fields — `role` and `instruction` — and
//! that is the security argument in structural form. There is no field for a
//! model, a provider, an endpoint, a key, a token ceiling, a tool grant, a
//! path, or a dollar amount, so none of them is a thing the host has to
//! remember to ignore.
//!
//! # Three refusals, and they are different questions
//!
//! 1. **Undeclared** — the intent is not a `[roles.<name>]` key in the manifest
//!    a human consented to. This is [`admissible`](super::admissible)'s rule for
//!    `BeforeTurnResponse::role`, restated at the value that arrives mid-point;
//!    a socket that enforced it on one path and not the other would be
//!    enforcing nothing.
//! 2. **Unavailable** — the intent is declared, but its `tier` names no seat
//!    this host serves. A declared gap the plugin is told about, with the
//!    tiers that *are* served named in the detail.
//! 3. **Forbidden** — the intent resolves to the **worker's** seat. This is the
//!    security half and it is not buyable: a plugin whose job is to judge the
//!    worker's output must not be able to spend the worker's own model to do
//!    it. The staged pipeline's roster *reported* this loss for an operator's
//!    own configuration (`Roster::independence_losses`;
//!    `crates/stella-pipeline`, deleted in #3865) — an operator may choose it,
//!    and a plugin may not.
//!
//! The third is compared on the **resolved seat**, never on the spelling of the
//! tier, which is the lesson `independence_losses` records in prose: while
//! every name was built in, name equality was seat equality, and the moment a
//! host may bind its own tier ([`ChildTurns::with_seat`]) a plugin could
//! otherwise nominate the worker under another name and nothing would notice.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use stella_core::{SubAgentDispatcher, SubAgentOutcome, SubAgentSpec};
use stella_plugin::{
    ChildTurnArgs, ChildTurnResult, HostCallFailure, HostCallRefusal, PluginManifest,
};
use stella_protocol::event::ModelCallRole;

/// The host's own ceiling on child turns for one plugin, when a caller states
/// none.
///
/// Four, and it is a **different bound from the host-call allowance**, not a
/// second copy of it. [`DEFAULT_HOST_MAX_CALLS`](super::DEFAULT_HOST_MAX_CALLS)
/// bounds how *chatty* a plugin may be inside one point, and is refreshed per
/// point because a plugin answering two points needs to ask at both. This one
/// bounds how much of the user's money a plugin may spend, so it is **per
/// plugin for the whole run**: refreshing it per point would make it no bound
/// at all across N rounds, which is the shape a hold ceiling that resets would
/// have.
///
/// Four rather than one because the thing `child_turn` exists to restore is a
/// research fan-out — `stella-research` replaces "a fan-out of read-only
/// sub-agents answering triage's questions" — and a fan-out of one is a
/// sequence. Small enough that a confused plugin cannot become a work session.
pub const DEFAULT_HOST_MAX_CHILD_TURNS: u32 = 4;

/// What one performed child turn cost, and what it was booked against.
///
/// The host's half of "the spend is visible", and the reason
/// `doc:turn-loop-wrappers` §9.2 prefers a declared role to a `judge`: a
/// verifier-tier call hidden inside a plugin is a call nobody can audit.
///
/// **Consumer** (invariant 10's discipline, pointed at a host report rather
/// than an event): `stella-cli`'s wrapper driver reads it after a run and
/// prints one line per child turn beside
/// [`RefusedCall`](super::RefusedCall) — `wrapper_plugin::spend_lines`, which
/// is exactly where a user looks to learn what a plugin did on their money
/// (#3576). The receipt carries the same seat independently under
/// [`Self::seat`], because that is the `ModelCallRole` the child's model calls
/// were attributed with — so the audit trail does not depend on this struct
/// being read.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildTurnSpend {
    /// The role intent the plugin named.
    pub role: String,
    /// The responsibility the host resolved it to, and the attribution the
    /// child's calls carry in `step_usage`.
    pub seat: ModelCallRole,
    /// What the child spent, in USD, as the dispatcher settled it.
    pub cost_usd: f64,
    /// Model calls the child made.
    pub steps: usize,
    /// Whether it reached a final answer.
    pub completed: bool,
}

/// A host that can run a bounded child turn for a plugin.
///
/// Object-safe and one method, so [`HostPlanes`](super::HostPlanes) can hold
/// one without becoming generic over the whole assembly. [`ChildTurns`] is the
/// shipped implementation; a host that resolves role intents some other way —
/// a server farming them out, a fixture — implements this instead of
/// re-implementing the gate above it.
#[async_trait]
pub trait ChildTurnPlane: Send + Sync {
    /// Run one child turn, or say why not.
    ///
    /// # Errors
    ///
    /// [`HostCallFailure`] for every refusal in this module's header, plus
    /// [`HostCallRefusal::Failed`] when the turn was attempted and the host
    /// could not run it. Each one reaches the plugin as an `err` answer rather
    /// than ending the point.
    async fn child_turn(&self, args: ChildTurnArgs) -> Result<ChildTurnResult, HostCallFailure>;
}

/// The seats this host serves a plugin's role intents from, before a caller
/// binds any of its own.
///
/// Four, and the omission is the interesting part: **`verifier` is deliberately
/// absent.** The seats it would resolve to are [`ModelCallRole::WitnessAuthor`]
/// and [`ModelCallRole::Verdict`], and attributing a plugin's child turn to
/// either would put a call on the receipt that the pipeline did not make —
/// `Verdict` in particular is the model verdict #2584 removed *structurally*,
/// which `Roster::apply` refuses to reassign. A host that genuinely wants to
/// serve a verifier tier says so with [`ChildTurns::with_seat`] and owns the
/// claim; it does not get it by default and then discover it on a receipt.
///
/// `worker` **is** here, and that is not a contradiction: it must resolve, so
/// that naming it is answered [`HostCallRefusal::Forbidden`] ("you may not have
/// this") rather than [`HostCallRefusal::Unavailable`] ("this host has nothing
/// behind it"). The two send a plugin author to entirely different places, and
/// hiding the independence rule behind the weaker one is how it would come to
/// be seen as an accident of configuration.
fn default_seats() -> BTreeMap<String, ModelCallRole> {
    [
        ("worker", ModelCallRole::Worker),
        ("triage", ModelCallRole::Triage),
        ("research", ModelCallRole::Research),
        ("plan", ModelCallRole::Plan),
    ]
    .into_iter()
    .map(|(tier, seat)| (tier.to_string(), seat))
    .collect()
}

/// The child-turn plane: a manifest's declared role intents, this host's
/// seats, its ceiling, and the dispatcher that actually runs the turn.
///
/// Holds the manifest's `[roles]` rather than a list of names because the
/// resolution is two hops — intent → declared `tier` → seat — and a plane
/// holding only the first hop could not perform the independence check on the
/// second, which is exactly the hole `Roster::independence_losses` documents.
pub struct ChildTurns<D> {
    plugin: String,
    dispatcher: D,
    roles: BTreeMap<String, String>,
    seats: BTreeMap<String, ModelCallRole>,
    declared_max: Option<u32>,
    ceiling: u32,
    budget_usd: Option<f64>,
    turn_instance: u32,
    spent: AtomicU32,
    ledger: Mutex<Vec<ChildTurnSpend>>,
}

impl<D> std::fmt::Debug for ChildTurns<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildTurns")
            .field("plugin", &self.plugin)
            .field("roles", &self.roles)
            .field("max_turns", &self.max_turns())
            .finish_non_exhaustive()
    }
}

// Unbounded on `D`: every method here decides, clamps or reports, and not one
// of them dispatches. Requiring `SubAgentDispatcher` to *read* `max_turns`
// would make the `Debug` impl below — the one thing a host prints when it
// cannot work out why a plugin was refused — unavailable exactly when `D` is a
// type parameter.
impl<D> ChildTurns<D> {
    /// Bind a plugin's declared role intents to the dispatcher that will spend
    /// them.
    ///
    /// The manifest is taken whole rather than its `[roles]` map, for
    /// [`HostCallGate::declare`](super::HostCallGate::declare)'s reason: the
    /// grant a human consented to at install is the authority, and a plane
    /// assembled from a hand-built map would be one nobody consented to.
    ///
    /// `[loop] max_child_turns` is the plugin's **ask** for child turns, and it
    /// is clamped against [`DEFAULT_HOST_MAX_CHILD_TURNS`]. It is its own key
    /// rather than a second meaning for `max_calls`, because the two bound
    /// different axes: `max_calls` is per point conversation and resets on every
    /// dispatch, and this one never resets. Reusing it cost `plugins/stella-goal`
    /// its second round — an arbiter that asks once per point and holds N rounds
    /// open needs N turns for the run, and had to declare `max_calls = 8` to say
    /// so (#3839).
    ///
    /// A manifest that declares only `max_calls` still gets that number, which
    /// is the only compatible reading: it is the one a human consented to, and
    /// the host's job is to clamp an ask down rather than to widen one nobody
    /// made. The clamp is the host's either way — a manifest asking for a
    /// thousand gets the ceiling, and one asking for one gets one.
    #[must_use]
    pub fn declare(manifest: &PluginManifest, dispatcher: D) -> Self {
        let roles = manifest.roles.as_ref().map_or_else(BTreeMap::new, |roles| {
            roles
                .iter()
                .map(|(name, role)| (name.clone(), role.tier.clone()))
                .collect()
        });
        Self {
            plugin: manifest.name.clone(),
            dispatcher,
            roles,
            seats: default_seats(),
            declared_max: manifest
                .loop_grant
                .max_child_turns
                .or(manifest.loop_grant.max_calls),
            ceiling: DEFAULT_HOST_MAX_CHILD_TURNS,
            budget_usd: None,
            turn_instance: 0,
            spent: AtomicU32::new(0),
            ledger: Mutex::new(Vec::new()),
        }
    }

    /// Set this host's ceiling on child turns, whatever the manifest asks for.
    #[must_use]
    pub fn with_max_turns(mut self, turns: u32) -> Self {
        self.ceiling = turns;
        self
    }

    /// Request a USD carve for each child turn.
    ///
    /// An ask, not an authority — `BudgetGuard::carve` clamps it to the
    /// parent's remaining headroom. `None`, the default, requests the whole
    /// headroom, which is still bounded whenever the session is.
    #[must_use]
    pub fn with_budget_usd(mut self, budget_usd: Option<f64>) -> Self {
        self.budget_usd = budget_usd;
        self
    }

    /// Bind one `tier` to the seat this host serves it from.
    ///
    /// Overrides the default table for that tier, and adds one the default
    /// table does not carry (`verifier` is the standing example — see
    /// `default_seats`). Binding a tier to [`ModelCallRole::Worker`] does not
    /// buy a plugin the worker's seat: the independence refusal compares the
    /// resolved seat, so renaming the worker only changes which word gets
    /// refused.
    #[must_use]
    pub fn with_seat(mut self, tier: impl Into<String>, seat: ModelCallRole) -> Self {
        self.seats.insert(tier.into(), seat);
        self
    }

    /// The receipt turn slot the child's manifests are recorded under.
    ///
    /// Context receipts key on `(execution_id, turn_instance, step, call_seq)`
    /// and every turn restarts `step` at 0, so a child sharing the parent's
    /// slot would overwrite the parent's manifests in the store.
    #[must_use]
    pub fn with_turn_instance(mut self, turn_instance: u32) -> Self {
        self.turn_instance = turn_instance;
        self
    }

    /// The effective ceiling on child turns, after the clamp.
    #[must_use]
    pub fn max_turns(&self) -> u32 {
        self.declared_max.unwrap_or(self.ceiling).min(self.ceiling)
    }

    /// Every child turn this plane performed, in the order it performed them.
    #[must_use]
    pub fn spends(&self) -> Vec<ChildTurnSpend> {
        // A poisoned lock means an unrelated call panicked mid-record; the
        // recorded spends are still exactly what they were, and losing the
        // report to someone else's panic would be the silence this whole
        // apparatus refuses (`HostCallGate::refusals` takes the same line).
        self.ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// What this plane spent in total, in USD.
    #[must_use]
    pub fn spent_usd(&self) -> f64 {
        self.spends().iter().map(|spend| spend.cost_usd).sum()
    }

    /// Resolve one role intent to the seat the turn will be spent at, or say
    /// why it will not be.
    ///
    /// Public because it is the whole security argument and a host that wants
    /// to answer "what would this plugin be allowed to spend?" before spending
    /// anything should be able to ask without running a turn.
    ///
    /// # Errors
    ///
    /// [`HostCallRefusal::Undeclared`] when the manifest declares no such role,
    /// [`HostCallRefusal::Unavailable`] when its tier names no seat this host
    /// serves, and [`HostCallRefusal::Forbidden`] when it resolves to the
    /// worker's seat.
    pub fn resolve(&self, role: &str) -> Result<ModelCallRole, HostCallFailure> {
        let Some(tier) = self.roles.get(role) else {
            let declared = if self.roles.is_empty() {
                "none".to_string()
            } else {
                self.roles.keys().cloned().collect::<Vec<_>>().join(", ")
            };
            return Err(HostCallFailure::new(
                HostCallRefusal::Undeclared,
                format!(
                    "plugin \"{}\" declares no [roles.{role}], so the host will not run a turn at \
                     it; declared role intents: {declared}",
                    self.plugin
                ),
            ));
        };
        let Some(&seat) = self.seats.get(tier) else {
            return Err(HostCallFailure::new(
                HostCallRefusal::Unavailable,
                format!(
                    "role intent \"{role}\" asks for the \"{tier}\" tier, which this host binds to \
                     no responsibility; served tiers: {}",
                    self.seats.keys().cloned().collect::<Vec<_>>().join(", ")
                ),
            ));
        };
        if seat == ModelCallRole::Worker {
            return Err(HostCallFailure::new(
                HostCallRefusal::Forbidden,
                format!(
                    "role intent \"{role}\" resolves to the worker's own seat, and a plugin may \
                     not spend the model whose work it is judging; name a tier that resolves \
                     elsewhere"
                ),
            ));
        }
        Ok(seat)
    }

    fn record(&self, spend: ChildTurnSpend) {
        self.ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(spend);
    }
}

#[async_trait]
impl<D: SubAgentDispatcher> ChildTurnPlane for ChildTurns<D> {
    async fn child_turn(&self, args: ChildTurnArgs) -> Result<ChildTurnResult, HostCallFailure> {
        // Resolution first, and before anything is spent: a refusal the plugin
        // could never have bought must not cost it an allowance, which is the
        // order `PointChannel::call` already takes for an undeclared call.
        let seat = self.resolve(&args.role)?;

        // `fetch_add` rather than load-then-store: a plugin pipelining two
        // calls must not be able to spend the last unit twice.
        let taken = self.spent.fetch_add(1, Ordering::Relaxed);
        let max_turns = self.max_turns();
        if taken >= max_turns {
            return Err(HostCallFailure::new(
                HostCallRefusal::AllowanceSpent,
                format!(
                    "this plugin's allowance of {max_turns} child turn(s) is spent; answer with \
                     what you have"
                ),
            ));
        }

        // Built through `read_only`, so the child cannot mutate the workspace —
        // a plugin's child turn is evidence-gathering, and a write arm here
        // would be a tool policy a human never consented to. Only the fields
        // the host owns are then set: the plugin contributed a role name and an
        // instruction, and nothing else on this spec is reachable from the
        // wire.
        let spec = SubAgentSpec {
            role: seat,
            // The plugin's OWN word for this role, passed through untouched.
            //
            // `role` above is the receipt label, drawn from a closed enum this
            // workspace owns; this is the routing key, drawn from the
            // plugin's manifest and meaningful only to the plugin that wrote
            // it and the user who assigned a model to it. Sending the name
            // rather than the enum is what lets a plugin declare a role core
            // has never heard of — a `reviewer`, a `second-opinion` — and
            // still have the user's model choice reach it. Nothing below this
            // line may branch on the contents.
            seat: Some(args.role.clone()),
            turn_instance: self.turn_instance,
            budget_usd: self.budget_usd,
            ..SubAgentSpec::read_only(
                format!("plugin:{}/{}#{taken}", self.plugin, args.role),
                args.instruction,
            )
        };

        let outcome = self.dispatcher.dispatch(spec).await;
        if let SubAgentOutcome::Refused { reason } = &outcome {
            // Nothing was spent — `SubAgentOutcome::Refused` is "never reached
            // its first model call, cost exactly zero" — so nothing is
            // recorded, and the plugin is told the host tried rather than that
            // it was refused for asking.
            return Err(HostCallFailure::new(
                HostCallRefusal::Failed,
                format!(
                    "the host could not run a child turn at \"{}\": {reason}",
                    args.role
                ),
            ));
        }

        let completed = matches!(outcome, SubAgentOutcome::Completed(_));
        let report = outcome.summary().to_string();
        self.record(ChildTurnSpend {
            role: args.role.clone(),
            seat,
            cost_usd: outcome.cost_usd(),
            steps: outcome.report().map_or(0, |report| report.steps),
            completed,
        });
        Ok(ChildTurnResult {
            role: args.role,
            seat: seat_token(seat),
            report,
            completed,
        })
    }
}

/// Serving a plane a host still holds a handle on.
///
/// [`HostPlanes::with_child_turns`](super::HostPlanes::with_child_turns) takes
/// the plane by value, and the plane is also the thing that knows what was
/// spent ([`ChildTurns::spends`]). Without this a host would have to choose
/// between installing the plane and being able to report on it, which is the
/// silence [`HostCallGate::refusals`](super::HostCallGate::refusals) exists to
/// prevent, in the one direction where the fact is money.
#[async_trait]
impl<P: ChildTurnPlane + ?Sized> ChildTurnPlane for std::sync::Arc<P> {
    async fn child_turn(&self, args: ChildTurnArgs) -> Result<ChildTurnResult, HostCallFailure> {
        (**self).child_turn(args).await
    }
}

/// A seat's wire token, as `step_usage` records it.
///
/// Through `serde_json` rather than a second hand-written table, for the
/// reason the staged pipeline's `roster::responsibility_token` gave
/// (`crates/stella-pipeline`, deleted in #3865): the token a plugin
/// reads must be the token the telemetry carries, and two spellings of that is
/// how the last one drifted. The fallback is unreachable for a fieldless enum
/// and exists only to keep this total (invariant 5 — no `expect` on a value a
/// caller supplied).
fn seat_token(seat: ModelCallRole) -> String {
    serde_json::to_value(seat)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{seat:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use stella_core::{SubAgentReport, SubAgentSpendLedger, push_sub_agent_spend};

    /// A dispatcher that records every spec it was handed and answers with a
    /// fixed report, so a test can ask what the *host* was asked to run.
    ///
    /// The log is an `Arc` rather than a field read through the dispatcher,
    /// because [`ChildTurns`] takes its dispatcher by value: sharing the
    /// recording is how a test inspects it after handing the dispatcher over.
    #[derive(Default, Clone)]
    struct Recording {
        specs: Arc<Mutex<Vec<SubAgentSpec>>>,
        ledger: SubAgentSpendLedger,
    }

    #[async_trait]
    impl SubAgentDispatcher for Recording {
        async fn dispatch(&self, spec: SubAgentSpec) -> SubAgentOutcome {
            let answer = format!("answered: {}", spec.instruction);
            self.specs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(spec);
            push_sub_agent_spend(&self.ledger, 0.01);
            SubAgentOutcome::Completed(SubAgentReport {
                summary: answer,
                truncated: false,
                cost_usd: 0.01,
                steps: 2,
                absorbed_messages: 4,
            })
        }
    }

    /// A dispatcher with nothing behind it.
    struct NoEngine;

    #[async_trait]
    impl SubAgentDispatcher for NoEngine {
        async fn dispatch(&self, _spec: SubAgentSpec) -> SubAgentOutcome {
            SubAgentOutcome::Refused {
                reason: "no provider is configured".to_string(),
            }
        }
    }

    fn manifest(roles: &str, max_calls: &str) -> PluginManifest {
        PluginManifest::from_toml_str(&format!(
            "name = \"grader\"\n\n[loop]\nparticipation = \"steering\"\npoints = \
             [\"after_turn\"]\ncalls = [\"child_turn\"]\n{max_calls}\n\n[subloop]\nstages = \
             [\"research\"]\n{roles}"
        ))
        .expect("the manifest loads")
    }

    fn ask(role: &str) -> ChildTurnArgs {
        ChildTurnArgs {
            role: role.to_string(),
            instruction: "does the diff drop the retry?".to_string(),
        }
    }

    #[tokio::test]
    async fn a_declared_role_intent_runs_a_turn_the_host_makes() {
        let dispatcher = Recording::default();
        let plane = ChildTurns::declare(
            &manifest("[roles.reviewer]\ntier = \"research\"", ""),
            dispatcher.clone(),
        );

        let result = plane
            .child_turn(ask("reviewer"))
            .await
            .expect("a declared, resolvable, non-worker role runs");
        assert_eq!(result.role, "reviewer");
        assert_eq!(result.seat, "research");
        assert_eq!(result.report, "answered: does the diff drop the retry?");
        assert!(result.completed);

        let specs = dispatcher
            .specs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(specs.len(), 1, "exactly one call, and the host made it");
        assert_eq!(specs[0].role, ModelCallRole::Research);
        assert!(
            !specs[0].write_access,
            "a plugin's child turn is read-only, enforced at execution"
        );

        let spends = plane.spends();
        assert_eq!(spends.len(), 1);
        assert_eq!(spends[0].seat, ModelCallRole::Research);
        assert!((plane.spent_usd() - 0.01).abs() < f64::EPSILON);
    }

    /// `admissible`'s rule for `BeforeTurnResponse::role`, restated on the
    /// value that arrives mid-point — enforced on one path and not the other
    /// is enforced nowhere.
    #[tokio::test]
    async fn a_role_the_manifest_never_declared_is_refused() {
        let plane = ChildTurns::declare(
            &manifest("[roles.reviewer]\ntier = \"research\"", ""),
            Recording::default(),
        );
        let failure = plane
            .child_turn(ask("auditor"))
            .await
            .expect_err("an undeclared role intent is refused");
        assert_eq!(failure.refusal, HostCallRefusal::Undeclared);
        assert!(failure.detail.contains("reviewer"), "{failure}");
        assert!(plane.spends().is_empty(), "a refusal spends nothing");
    }

    /// The security half. Declared, resolvable, and still refused.
    #[tokio::test]
    async fn a_role_intent_that_resolves_to_the_worker_is_forbidden() {
        let plane = ChildTurns::declare(
            &manifest("[roles.grader]\ntier = \"worker\"", ""),
            Recording::default(),
        );
        let failure = plane
            .child_turn(ask("grader"))
            .await
            .expect_err("the worker's seat is not for sale");
        assert_eq!(failure.refusal, HostCallRefusal::Forbidden);
        assert!(plane.spends().is_empty());
    }

    /// The seat is compared, never the spelling: a host binding its own tier to
    /// the worker does not open a side door.
    #[tokio::test]
    async fn renaming_the_worker_does_not_buy_a_plugin_the_worker() {
        let plane = ChildTurns::declare(
            &manifest("[roles.grader]\ntier = \"cheap\"", ""),
            Recording::default(),
        )
        .with_seat("cheap", ModelCallRole::Worker);
        let failure = plane
            .child_turn(ask("grader"))
            .await
            .expect_err("the resolved seat is what is checked");
        assert_eq!(failure.refusal, HostCallRefusal::Forbidden);
    }

    /// A tier this host binds to nothing is a declared gap the plugin is told
    /// about, with the served tiers named — never a silent empty answer.
    #[tokio::test]
    async fn an_unserved_tier_names_the_tiers_that_are_served() {
        let plane = ChildTurns::declare(
            &manifest("[roles.grader]\ntier = \"verifier\"", ""),
            Recording::default(),
        );
        let failure = plane
            .child_turn(ask("grader"))
            .await
            .expect_err("`verifier` binds to no seat by default");
        assert_eq!(failure.refusal, HostCallRefusal::Unavailable);
        assert!(failure.detail.contains("research"), "{failure}");

        // ...and a host that wants it says so, and owns the claim.
        let bound = ChildTurns::declare(
            &manifest("[roles.grader]\ntier = \"verifier\"", ""),
            Recording::default(),
        )
        .with_seat("verifier", ModelCallRole::WitnessAuthor);
        let result = bound.child_turn(ask("grader")).await.expect("now served");
        assert_eq!(result.seat, "witness_author");
    }

    /// The manifest's number is an ask; the ceiling is the host's.
    #[tokio::test]
    async fn a_plugin_asking_for_more_child_turns_than_the_ceiling_is_clamped() {
        let plane = ChildTurns::declare(
            &manifest("[roles.reviewer]\ntier = \"research\"", "max_calls = 100"),
            Recording::default(),
        )
        .with_max_turns(2);
        assert_eq!(plane.max_turns(), 2, "100 was an ask, 2 is the answer");

        for _ in 0..2 {
            plane.child_turn(ask("reviewer")).await.expect("within");
        }
        let failure = plane
            .child_turn(ask("reviewer"))
            .await
            .expect_err("the third is over the ceiling");
        assert_eq!(failure.refusal, HostCallRefusal::AllowanceSpent);
        assert_eq!(plane.spends().len(), 2, "the clamp is on spend, not on ask");
    }

    /// A manifest asking for less than the ceiling gets what it asked for: the
    /// clamp is `min`, not "the host's number wins".
    #[tokio::test]
    async fn a_modest_manifest_is_taken_at_its_word() {
        let plane = ChildTurns::declare(
            &manifest("[roles.reviewer]\ntier = \"research\"", "max_calls = 1"),
            Recording::default(),
        );
        assert_eq!(plane.max_turns(), 1);
        plane.child_turn(ask("reviewer")).await.expect("the first");
        assert_eq!(
            plane
                .child_turn(ask("reviewer"))
                .await
                .expect_err("the second")
                .refusal,
            HostCallRefusal::AllowanceSpent
        );
    }

    /// **The #3839 witness.** A plugin that asks for one host call per point
    /// and two child turns for the run gets both, rather than having the
    /// per-point number cap its second round.
    ///
    /// `plugins/stella-goal` is the shape: an arbiter holds a round open, asks
    /// once in each, and before `max_child_turns` existed the honest
    /// `max_calls = 1` made round 2 `AllowanceSpent` — so the manifest had to
    /// declare `max_calls = 8` and stop answering the question it was asked.
    #[tokio::test]
    async fn a_per_point_allowance_no_longer_caps_the_whole_run() {
        let plane = ChildTurns::declare(
            &manifest(
                "[roles.reviewer]\ntier = \"research\"",
                "max_calls = 1\nmax_child_turns = 2",
            ),
            Recording::default(),
        );
        assert_eq!(plane.max_turns(), 2);
        plane.child_turn(ask("reviewer")).await.expect("round 1");
        plane.child_turn(ask("reviewer")).await.expect("round 2");
        assert_eq!(
            plane
                .child_turn(ask("reviewer"))
                .await
                .expect_err("round 3 is past what the manifest asked for")
                .refusal,
            HostCallRefusal::AllowanceSpent
        );
    }

    /// A manifest written before the split still gets the number a human
    /// consented to. The host clamps an ask down; it does not widen one nobody
    /// made, which is what "absent means the host's ceiling" would have done to
    /// every `max_calls = 1` manifest already installed.
    #[tokio::test]
    async fn a_manifest_declaring_only_the_per_point_number_keeps_it() {
        let plane = ChildTurns::declare(
            &manifest("[roles.reviewer]\ntier = \"research\"", "max_calls = 1"),
            Recording::default(),
        );
        assert_eq!(plane.max_turns(), 1);
    }

    /// A host with no engine behind it reports that it tried, and records no
    /// spend — a refused child costs exactly zero.
    #[tokio::test]
    async fn a_dispatcher_with_no_engine_is_a_failure_the_plugin_reads() {
        let plane = ChildTurns::declare(
            &manifest("[roles.reviewer]\ntier = \"research\"", ""),
            NoEngine,
        );
        let failure = plane
            .child_turn(ask("reviewer"))
            .await
            .expect_err("nothing ran");
        assert_eq!(failure.refusal, HostCallRefusal::Failed);
        assert!(failure.detail.contains("no provider"), "{failure}");
        assert!(plane.spends().is_empty());
    }

    /// A manifest with no `[roles]` at all can ask for nothing, and is told so
    /// in a sentence that does not read as a bug in the host.
    #[tokio::test]
    async fn a_plugin_declaring_no_roles_is_told_it_declared_none() {
        let plane = ChildTurns::declare(
            &PluginManifest::from_toml_str(
                "name = \"quiet\"\n\n[loop]\nparticipation = \"steering\"\npoints = \
                 [\"after_turn\"]\ncalls = [\"child_turn\"]",
            )
            .expect("loads"),
            Recording::default(),
        );
        let failure = plane
            .child_turn(ask("anything"))
            .await
            .expect_err("no roles, no turns");
        assert_eq!(failure.refusal, HostCallRefusal::Undeclared);
        assert!(failure.detail.contains("none"), "{failure}");
    }
}
