//! The engine's port boundary. `stella-core`
//! never imports a provider SDK, a filesystem call, or a terminal library —
//! it drives through these traits, mirroring the TS engine's `ports.ts`.

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::{ToolOutput, ToolSchema};

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
    /// what keeps `--budget` a hard ceiling once turns nest: the parent's
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
}

/// A read-only view over another executor: advertises only the schemas
/// marked `read_only` and refuses to execute anything else. This is how a
/// verifier gets real evidence-gathering power (read files, grep, check
/// saved explorations) with a structural guarantee it cannot mutate the
/// workspace it is judging — the restriction is enforced at execution
/// time, not just by prompt.
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
            return ToolOutput::Error {
                message: format!(
                    "`{name}` is not available here: this context is read-only (verification/\
                     judging) and may only use read-only tools"
                ),
            };
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

    // `parallel_safe_names` deliberately keeps the empty default rather than
    // forwarding (contrast with the spend drain above): the one shipped
    // parallel-safe tool is the sub-agent spawn, and this view exists to
    // strip exactly that from a child — advertising a name whose execution
    // the same view refuses would be an empty promise.
}

/// Time source, injectable for deterministic tests. Only the trait lives
/// here — the production implementation belongs to the binary that wires
/// the engine (the CLI's `runtime` module), so `stella-core` never carries
/// a concrete time source of its own.
pub trait Clock: Send + Sync {
    /// Monotonic milliseconds since an arbitrary epoch.
    fn now_ms(&self) -> u64;
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
