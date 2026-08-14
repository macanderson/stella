//! The engine's port boundary. `stella-core`
//! never imports a provider SDK, a filesystem call, or a terminal library —
//! it drives through these traits, mirroring the TS engine's `ports.ts`.

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::{Provider, ToolOutput, ToolSchema};

/// Executes one tool call. Implemented by `stella-tools::ToolRegistry` (and
/// by test doubles). The engine treats it as a black box that never panics.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Schemas advertised to the model.
    fn schemas(&self) -> Vec<ToolSchema>;

    /// Execute a tool by name. Unknown names return an error `ToolOutput`,
    /// never an Err — tool failures are model-visible data, not engine
    /// failures.
    async fn execute(&self, name: &str, input: &Value) -> ToolOutput;

    /// USD spent by sub-agents this executor dispatched since the last call,
    /// **taken** (not peeked) — the engine folds it into the turn's
    /// [`crate::BudgetGuard`] at the next step-boundary budget check.
    ///
    /// # Why the executor and not the engine
    ///
    /// A `task`-style tool spawns a child turn *from inside*
    /// `ToolExecutor::execute`, while the engine holds the budget guard
    /// mutably for the whole turn — so the tool structurally cannot charge
    /// the parent as it goes. This is the same "written by one object,
    /// drained by another" seam [`crate::mcp_usage`] uses, and the executor
    /// that dispatched the child is exactly the thing that knows what it
    /// cost. Draining at the *step boundary* (rather than after the turn) is
    /// what keeps `--spend-limit` a hard ceiling once turns nest: the parent's
    /// `evaluate()` always sees child spend to date.
    ///
    /// # Decorators MUST forward this
    ///
    /// The default returns `0.0`, which reads as "this executor never
    /// dispatches sub-agents" — correct for a leaf, and **wrong for a
    /// wrapper**. A decorator that forgets to forward silently drops real
    /// money out of the accounting, and no compiler will say so. Every
    /// wrapper in this workspace forwards; the shipped composition is pinned
    /// end-to-end by `stella-cli`'s `the_production_tool_stack_forwards_sub_agent_spend`,
    /// which asserts through the real stack rather than a hypothetical one.
    ///
    /// Destructive by contract: two drains of the same spend would charge it
    /// twice, so an implementation must reset its ledger here.
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        0.0
    }

    /// The parked-wait request a tool deposited during its last `execute`,
    /// **taken** (not peeked) — the engine consumes it at the next step
    /// boundary and parks the turn on the engine's own clock instead of
    /// letting the model poll (#1471, `crate::waiting`).
    ///
    /// The same "written by one object, drained by another" seam as
    /// [`Self::drain_sub_agent_spend_usd`], for the same structural reason:
    /// a tool returns only a `ToolOutput`, and by the time the engine is at
    /// a boundary where parking is safe (invariant 6 — between model calls,
    /// never mid-tool) the tool call is long finished.
    ///
    /// # Decorators MUST forward this
    ///
    /// The default returns `None`, which reads as "no tool of mine ever
    /// asks to wait" — correct for a leaf, and **wrong for a wrapper**. A
    /// decorator that forgets to forward silently turns parked waits off
    /// for every surface composed through it, and the model falls back to
    /// burning steps on polling — the exact regression #1471 removes. The
    /// shipped composition is pinned by `stella-cli`'s
    /// `the_production_tool_stack_forwards_wait_requests`.
    ///
    /// Destructive by contract: two drains of one request would park twice.
    fn drain_wait_request(&self) -> Option<crate::waiting::WaitRequest> {
        None
    }

    /// Names of tools that are safe to run **concurrently with each other and
    /// with read-only calls** despite not declaring `read_only` — the
    /// executor-level claim behind the driver's dispatch grouping. The
    /// shipped case is the sub-agent spawn tool: its children only ever read
    /// (they run behind [`ReadOnlyTools`]) and the dispatcher carves budget
    /// per child, so sibling spawns in one step have no ordering an observer
    /// of mutable state could distinguish — yet the tool is honestly not
    /// `read_only` (that flag also fences children out of it, and gates
    /// permission surfaces on spend).
    ///
    /// Deliberately an executor method rather than a third `ToolSchema`
    /// claim: dispatch grouping is a scheduling fact about *this* executor's
    /// composition, not a wire-level property an external (MCP) tool may
    /// self-declare — an external tool that wants concurrency claims
    /// `read_only`, which is already the grouping key. Defaults to empty, the
    /// safe direction. This set widens ONLY dispatch grouping: speculation
    /// keeps its `read_only && speculation_safe` conjunction, and the
    /// evidence-side consumers of the read-only set (`confident_zero`, the
    /// mutating-action counts) are untouched — "runs concurrently" is not an
    /// artifact claim.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        std::collections::HashSet::new()
    }

    /// Slugs of skills whose inline invocation is currently live in this
    /// executor's stack (#2682) — read, never drained: the tool layer owns
    /// the spans and ends them structurally when their guards drop.
    ///
    /// The engine consults this at the overflow-summarization boundary
    /// (#2685): a live skill's invocation body folded into a summary is
    /// procedure text the model is mid-way through executing, and the
    /// working-set restoration re-attaches it verbatim
    /// (`driver::restore`). The same "written by one object, read by
    /// another" seam as [`Self::drain_wait_request`], for the same
    /// structural reason: invocation tracking belongs to the tool stack,
    /// and the engine only ever sees the stack through this trait. No
    /// shipped executor mounts a skill-invocation plane today, so every
    /// production implementation answers empty.
    ///
    /// # Decorators MUST forward this
    ///
    /// The default returns an empty `Vec`, which reads as "this executor
    /// mounts no skill-invocation plane" — correct for a leaf, and **wrong
    /// for a wrapper**. A decorator that forgets to forward silently stops
    /// active procedure text surviving summarization for every surface
    /// composed through it, and no compiler will say so.
    fn active_skill_slugs(&self) -> Vec<String> {
        Vec::new()
    }

    /// Long-running services this executor launched that are still up, read
    /// (never taken) at the boundary where a turn is about to complete —
    /// the end-of-turn assertion (#2764, `crate::driver::live_services`).
    ///
    /// # Why the executor and not the engine
    ///
    /// The engine holds no process table and must not: a started child is a
    /// concretion of `stella-tools`, and reaching for it here would be
    /// invariant 1 in reverse. What the engine needs is not the table, it is
    /// the *answer* — "did this turn declare a service and leave it up?" —
    /// which the executor that spawned the child is exactly the thing that
    /// knows.
    ///
    /// Non-destructive, unlike the two drains above, and the difference is
    /// the point: this is a question about state that outlives the turn, not
    /// a ledger to be settled by whoever asks first. Two callers must get
    /// the same answer, and asking must change nothing — least of all stop
    /// anything, which the assertion this feeds is explicitly not for.
    ///
    /// # Decorators MUST forward this
    ///
    /// The default returns empty, which reads as "nothing I run outlives a
    /// turn" — correct for a leaf, and **wrong for a wrapper**. A decorator
    /// that forgets to forward turns the assertion off for every surface
    /// composed through it, silently and with no compiler complaint: the
    /// agent goes back to declaring a service done without ever being asked
    /// whether it is still listening. The shipped composition is pinned
    /// end-to-end by `stella-cli`'s
    /// `the_production_tool_stack_forwards_live_services`.
    fn live_services(&self) -> Vec<LiveService> {
        Vec::new()
    }
}

/// One long-running process a tool started and nothing has stopped.
///
/// Deliberately not a handle onto the process: this type crosses the port
/// boundary so the engine can *name* what is still up, and naming is all it
/// is ever allowed to do with it. There is no stop, no signal, and no pid —
/// #2666's whole finding is that a still-running declared service can be the
/// correct final state, so the engine must not be handed a way to end one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveService {
    /// The stable handle the model has held since the start call
    /// (`proc-3`).
    pub handle: String,
    /// The label the model gave the process at start, when it gave one.
    pub name: Option<String>,
    /// The command line it was started with, joined for display.
    pub display: String,
}

/// A read-only view over another executor: advertises only the schemas
/// marked `read_only` and refuses to execute anything else. This is how a
/// verifier gets real evidence-gathering power (`task_list`, `get_state`,
/// `list_state`, `get_environment`, read-only MCP tools) with a structural
/// guarantee it cannot mutate the workspace it is judging — the restriction
/// is enforced at execution time, not just by prompt.
pub struct ReadOnlyTools<'a> {
    inner: &'a dyn ToolExecutor,
    /// The inner executor's read-only tool names, resolved once at
    /// construction. `execute` is on the hot path of every verifier/verifier
    /// tool call, and answering "is this tool read-only?" must not
    /// re-materialize the full schema list (each schema carries its whole
    /// JSON parameter document) per call. The view is short-lived — built
    /// per assessment — so a registry whose tool set changes mid-turn is
    /// not a case this snapshot needs to chase.
    read_only_names: std::collections::HashSet<String>,
}

impl<'a> ReadOnlyTools<'a> {
    pub fn new(inner: &'a dyn ToolExecutor) -> Self {
        let read_only_names = inner
            .schemas()
            .into_iter()
            .filter(|s| s.read_only)
            .map(|s| s.name)
            .collect();
        Self {
            inner,
            read_only_names,
        }
    }
}

#[async_trait]
impl ToolExecutor for ReadOnlyTools<'_> {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.inner
            .schemas()
            .into_iter()
            .filter(|s| s.read_only)
            .collect()
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        let allowed = self.read_only_names.contains(name);
        if !allowed {
            return ToolOutput::error(format!(
                "`{name}` is not available here: this context is read-only (verification/\
                     judging) and may only use read-only tools"
            ));
        }
        self.inner.execute(name, input).await
    }

    /// Forwarded, not zeroed. A sub-agent runs behind this view, so a
    /// *grandchild* it dispatched settles here first — into the child's own
    /// carve — and only then into the parent as part of the child's total.
    /// Each level charges once, which is what nesting means; zeroing here
    /// would make a grandchild's spend invisible to the carve that is
    /// supposed to bound it.
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.inner.drain_sub_agent_spend_usd()
    }

    /// Forwarded: a verifier legitimately waits on external state (CI is the
    /// canonical read-only evidence source), and its probe replays through
    /// this same view — so the read-only restriction holds while parked too.
    fn drain_wait_request(&self) -> Option<crate::waiting::WaitRequest> {
        self.inner.drain_wait_request()
    }

    /// Forwarded: the inner stack owns the invocation spans, and a child
    /// running behind this view still deserves its live procedure text to
    /// survive summarization (see the port's contract).
    fn active_skill_slugs(&self) -> Vec<String> {
        self.inner.active_skill_slugs()
    }

    /// Forwarded, and the read-only filter is deliberately not applied to
    /// it. A service is up or it is not, whatever view happens to be asking
    /// — this reports state the workspace is in, not a capability this view
    /// grants. Filtering here would let a sub-agent turn end silently on a
    /// service its parent started, which is the same invisibility the
    /// assertion exists to remove.
    fn live_services(&self) -> Vec<LiveService> {
        self.inner.live_services()
    }

    // `parallel_safe_names` deliberately keeps the empty default rather than
    // forwarding (contrast with the spend drain above): the one shipped
    // parallel-safe tool is the sub-agent spawn, and this view exists to
    // strip exactly that from a child — advertising a name whose execution
    // the same view refuses would be an empty promise.
}

/// A name-scoped view over another executor: advertises and executes only
/// an explicitly granted set of tool names, refusing everything else at
/// execution time — the same structural-enforcement shape as
/// [`ReadOnlyTools`], pointed at a *grant* instead of a mutability class.
///
/// This is how a forked skill's `allowed-tools` frontmatter reaches a child
/// turn (#2682): the caller resolves the grant to concrete advertised names
/// (groups and policy algebra live in `stella-tools::skill_grant`, which this
/// crate must not import) and the child runs behind this view. It only ever
/// narrows — a name the inner executor does not advertise stays unavailable
/// whatever the grant says, so a grant can never re-enable a tool an
/// operator or org scope denied below.
pub struct GrantedTools<'a> {
    inner: &'a dyn ToolExecutor,
    granted: std::collections::HashSet<String>,
}

impl<'a> GrantedTools<'a> {
    pub fn new(inner: &'a dyn ToolExecutor, granted: &[String]) -> Self {
        Self {
            inner,
            granted: granted.iter().cloned().collect(),
        }
    }
}

#[async_trait]
impl ToolExecutor for GrantedTools<'_> {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.inner
            .schemas()
            .into_iter()
            .filter(|s| self.granted.contains(&s.name))
            .collect()
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        if !self.granted.contains(name) {
            return ToolOutput::error(format!(
                "`{name}` is not available here: this context is scoped to an \
                     explicitly granted tool set (a skill's allowed-tools)"
            ));
        }
        self.inner.execute(name, input).await
    }

    /// Forwarded, not zeroed, for the same reason as [`ReadOnlyTools`]: a
    /// grandchild dispatched behind this view still settles into the carve
    /// that bounds it.
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.inner.drain_sub_agent_spend_usd()
    }

    /// Forwarded for the same reason [`ReadOnlyTools`] forwards it: a tool
    /// running behind this view legitimately parks on external state, and
    /// its probe replays through this same grant — so the scope holds while
    /// parked too. Leaving it defaulted would silently turn parked waits off
    /// for any turn scoped by a skill's `allowed-tools`, dropping the model
    /// back to polling (the #1471 regression) and leaving the child's
    /// undrained request to be picked up by the parent's next drain.
    fn drain_wait_request(&self) -> Option<crate::waiting::WaitRequest> {
        self.inner.drain_wait_request()
    }

    /// Forwarded for the same reason [`ReadOnlyTools`] forwards it: the
    /// grant narrows which tools run, not which invocations are live.
    fn active_skill_slugs(&self) -> Vec<String> {
        self.inner.active_skill_slugs()
    }

    /// Forwarded for the same reason [`ReadOnlyTools`] forwards it: a grant
    /// narrows which tools this view will *run*, and a live service is state
    /// the workspace is in rather than a capability this view hands out.
    /// Leaving it defaulted would turn the end-of-turn assertion (#2764) off
    /// for any turn scoped by a skill's `allowed-tools`, silently — which is
    /// the invisibility that assertion exists to remove.
    fn live_services(&self) -> Vec<LiveService> {
        self.inner.live_services()
    }
}

/// Time source, injectable for deterministic tests. Only the trait lives
/// here — the production implementation belongs to the binary that wires
/// the engine (the CLI's `runtime` module), so `stella-core` never carries
/// a concrete time source of its own.
pub trait Clock: Send + Sync {
    /// Monotonic milliseconds since an arbitrary epoch.
    fn now_ms(&self) -> u64;
}

/// Call-outcome feedback into provider routing — the write half of
/// [`crate::router::Router`]'s circuit breaker (§7 reliability rules; L-M7),
/// which implements it. The engine reports each *logical* model call's
/// terminal verdict, retries collapsed: a call that eventually committed is
/// one success, retries exhausted (or an unretryable transport-class error)
/// is one failure. `&self` because the reporter holds the router shared —
/// resolution keeps reading the same reference concurrently, and requiring
/// `&mut` is exactly what left the breaker without a production caller
/// (#2673).
pub trait ProviderOutcomes: Send + Sync {
    /// A call served by `provider_id` committed.
    fn record_success(&self, provider_id: &str);
    /// A transport-class terminal failure from `provider_id`.
    fn record_failure(&self, provider_id: &str);
}

/// Mid-turn provider fallback (#2679) — the read half of the routing seam
/// whose write half is [`ProviderOutcomes`]: when a model call's retry
/// ladder exhausts against the active provider, the engine asks this port
/// for a replacement instead of aborting the turn. The production impl
/// re-resolves the engine's role through the same
/// [`crate::router::Router::resolve`] that chose the primary — never a
/// hardcoded model list — and the failing calls already fed that router's
/// circuit breaker through [`ProviderOutcomes`] (#2673), so resolution
/// naturally routes around the sick provider. `&self` for the same reason as
/// [`ProviderOutcomes`]: the holder shares the router.
pub trait FallbackResolver: Send + Sync {
    /// Re-resolve the engine's role after `failed_provider_id` exhausted its
    /// retry ladder. `None` means "no fallback": nothing else is configured,
    /// every alternative is breaker-open, or resolution lands back on the
    /// failed provider itself — the engine then surfaces the exhaustion
    /// exactly as it did before this port existed.
    fn resolve_fallback(&self, failed_provider_id: &str) -> Option<ResolvedFallback<'_>>;
}

/// A replacement provider from [`FallbackResolver::resolve_fallback`]. The
/// adapter is owned by the resolver — the engine only borrows it for the
/// rest of the turn — and `reason` is the resolver's human-readable account
/// of why the route changed (e.g. [`crate::router::FallbackInfo::reason`]);
/// it rides `AgentEvent::ProviderFallback` verbatim (L-M7: fallback is
/// always visible, never a silent mid-turn family switch).
pub struct ResolvedFallback<'p> {
    /// The replacement adapter. Calls after the swap go here.
    pub provider: &'p dyn Provider,
    /// Why the route changed, verbatim onto the fallback event.
    pub reason: String,
}

/// Boundary pause gate — polled by the engine between model calls, the same
/// safe boundary as budget aborts (L-E6: never mid-tool). `wait_if_paused`
/// returns immediately when the turn may proceed and parks (await) while it
/// is paused; the driver flips the underlying state from supervisor input.
/// A port so `stella-core` stays I/O-free — the CLI implements it over a
/// watch channel.
#[async_trait::async_trait]
pub trait TurnGate: Send + Sync {
    async fn wait_if_paused(&self);
}

/// Step-boundary steering — polled by the engine at the same safe boundary
/// as [`TurnGate`] (L-E6: never mid-tool). Turns "wait, pause, or kill"
/// into "steer": user messages queued while a turn runs are injected as
/// the model's next observation instead of waiting for the turn to end,
/// and a soft stop ends the turn at the boundary KEEPING the work so far —
/// unlike the caller-side hard cancel, which drops the turn future and
/// truncates the whole turn (all paid tokens) out of history. A port so
/// `stella-core` stays I/O-free — the CLI implements it over shared state
/// fed by the deck's input loop.
pub trait TurnSteering: Send + Sync {
    /// Drain the user messages queued since the last boundary, oldest
    /// first. Non-destructive peeks are deliberately not offered: whatever
    /// this returns WILL be injected, so the implementation owns dedup.
    fn drain_steering(&self) -> Vec<String>;
    /// True when the user asked to end the turn at the next boundary. The
    /// engine reads this once per step; the implementation should latch it
    /// (a stop request must not evaporate between steps).
    fn soft_stop_requested(&self) -> bool;
}

/// [`TurnGate`] and [`TurnSteering`] in owned form, so something that
/// outlives a turn can hold them.
///
/// [`Engine`](crate::Engine) takes both by `&'a dyn` — right for a driver,
/// which builds them on the stack of the turn it is about to run. It is
/// wrong for anything *session*-scoped: a sub-agent dispatcher lives across
/// every turn of the session and cannot name a single turn's lifetime, so it
/// needs `Arc`s it can clone onto a child's thread instead.
///
/// Both fields are `Option` because not every driver has both: a worker lane
/// pauses but does not steer, and a headless run does neither. The deck's
/// lead turn has both — it steers (`>`), and since #1219 it pauses (`p`) —
/// so a driver publishes what it has, which may be one, the other, or the
/// pair.
///
/// # Empty is a real state, not an error
///
/// A holder that was never given controls, or whose turn has ended, sees
/// [`TurnControls::default`] — and a child run under it behaves exactly as
/// children did before controls existed: bounded by its step cap and its
/// budget carve, just not interruptible. Degrading to the old behavior beats
/// refusing to run.
#[derive(Clone, Default)]
pub struct TurnControls {
    /// The turn's pause gate, when its driver has one.
    pub gate: Option<std::sync::Arc<dyn TurnGate>>,
    /// The turn's steering tap, when its driver has one.
    pub steering: Option<std::sync::Arc<dyn TurnSteering>>,
}

impl TurnControls {
    /// No controls — what a holder reads between turns.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Publish `gate` as the turn's pause gate.
    #[must_use]
    pub fn with_gate(mut self, gate: std::sync::Arc<dyn TurnGate>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// Publish `steering` as the turn's steering tap.
    ///
    /// What a *child* gets from this is deliberately narrower than what the
    /// parent has: see [`crate::subagent::ChildSteering`], which forwards the
    /// soft stop and never the queued messages.
    #[must_use]
    pub fn with_steering(mut self, steering: std::sync::Arc<dyn TurnSteering>) -> Self {
        self.steering = Some(steering);
        self
    }

    /// Whether both seams are absent.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.gate.is_none() && self.steering.is_none()
    }
}

impl std::fmt::Debug for TurnControls {
    /// Neither port is `Debug` (they are implemented over channels and locks
    /// that have no useful rendering), so this reports which seams are
    /// present — which is the only thing a log or a failing assertion wants.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnControls")
            .field("gate", &self.gate.is_some())
            .field("steering", &self.steering.is_some())
            .finish()
    }
}
