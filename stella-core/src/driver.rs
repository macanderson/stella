//! The step-driver: `Engine::run_turn`. One
//! model call per step, message accumulation, `AgentEvent` emission at
//! every boundary, retry+backoff, compaction, tool-output budget checks,
//! loop detection, and (a first, structural cut of) malformed-call repair —
//! wiring together every other module in this crate.
//!
//! `Engine` drives through `&dyn Provider` (`stella_protocol`) and
//! `&dyn ToolExecutor` (`crate::ports`) — no adapter-specific code and no
//! direct filesystem access live here. Everything
//! *inside* one step (compaction, loop detection, budget evaluation) is the
//! plain synchronous logic from the other modules in this crate; `run_turn`
//! is the one place that sequences them against real I/O.
//!
//! # Deferred-flush events (L-E10)
//!
//! `retry_with_backoff_observed` returns committed retry
//! history while synchronously exposing each failed provider attempt to the
//! accounting path. Ordinary retry narration stays deferred until success;
//! content-free `UsageIncomplete` envelopes are durable immediately because
//! a later successful attempt cannot recover the failed call's usage. A
//! caller-side hard cancel that drops the turn while an attempt is still in
//! flight emits one `Cancelled` envelope from a drop guard
//! (`CancelUsageGuard`) armed for exactly that window.
//!
//! # Retry re-executes only speculation-safe read-only calls, never a mutating one
//!
//! `retry_with_backoff_observed` wraps the model call
//! (`Provider::complete_observed`) together with that attempt's speculation
//! pump (`crate::speculation`). The exactly-once guarantee is scoped to
//! MUTATING tools: a mutating call runs once, after a model call has
//! already succeeded and returned tool calls to run — never inside the
//! retried closure — so a retried step structurally cannot re-execute a
//! non-idempotent tool. See the test
//! `retry_never_re_executes_a_tool_call` below, which proves it by counting
//! real executions against a flaky scripted provider.
//!
//! SPECULATED tools carry no such guarantee: a call announced by the
//! stream DOES execute inside the retried attempt closure and can run
//! more than once per step (a failed attempt runs it, the retry re-announces
//! and runs it again). Which is why eligibility takes two schema claims,
//! not one (#923): `read_only` (safe for the *workspace* — the call
//! mutates nothing) AND `speculation_safe` (safe to run twice — no
//! metered network read, no rate-limited API, no write to internal state
//! like `codegraph.db` on the read path). A tool that is read-only but
//! not speculation-safe runs only at dispatch, exactly once per committed
//! call, with no hook to attach and nothing to remember. See
//! `crate::speculation` for the overlap semantics: user hooks are
//! excluded from that overlap so they still fire exactly once per committed
//! call, and every speculative execution that never commits is reported as a
//! `SpeculationDiscarded` event so the I/O it ran stays accountable (#370).
//!
//! # Budget is checked between steps, never mid-tool
//!
//! Per [`crate::budget`]'s module contract, `run_turn` only consults
//! [`crate::budget::BudgetGuard::evaluate`]/`record_spend` immediately
//! after a model call completes and before the next one (or before
//! executing this step's tool calls) — an `AbortTurn` outcome ends the turn
//! cleanly, it never interrupts a tool already in flight.
//!
//! # Malformed-call repair
//!
//! Every adapter's stream aggregator in `stella-model` falls back to
//! `serde_json::Value::Null` when a tool call's streamed argument JSON
//! doesn't parse. `run_turn`
//! recognizes that sentinel structurally: rather than handing `Null` to a
//! tool that expects an object, it short-circuits to a named
//! `ToolOutput::Error` telling the model its own JSON was malformed, so the
//! model can retry with corrected syntax on the next step. This is a real,
//! if first-cut, repair — dialect-specific tuning
//! ("malformed-call repair tuned to the failure shapes GLM actually
//! produces") is a documented follow-up, not faked here.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use stella_protocol::{
    AgentEvent, CompletionMessage, CompletionRequest, CompletionRequestRef, FinishReason,
    MessageRole, Provider, ProviderError, ReasoningEffort, StageKind, ToolCall, ToolOutput,
    ToolResult,
};

use crate::budget::{BudgetAxis, BudgetGuard, BudgetOutcome};
use crate::bus;
use crate::compaction::compact_measured;
use crate::receipts::TranscriptRevision;
use lifecycle::{step_outcome_label, turn_outcome_payload};

pub mod lifecycle;
mod loop_evidence;
mod truncation;
use crate::estimator::{CalibrationMap, estimate_conversation_tokens};
use crate::event_sender::EventSender;
use crate::hooks::{HookEvent, HookPayload, HookRunner, Hooks, any_matcher_matches, run_hooks};
use crate::loop_detect::{LoopDetectionConfig, LoopVerdict, detect_loop};
use crate::ports::ToolExecutor;
use crate::receipts::ReceiptLedger;
use crate::retry::{RetryOutcome, RetryPolicy, Sleeper, retry_with_backoff_observed};
use crate::speculation::{SpeculationGate, SpeculationPool, SpeculativeResult};
use crate::step::{
    BorrowedTurn, CancelUsageGuard, CompactionPass, SpeculationDropGuard, StepOutcome,
    StreamProgress, SummarizerHealth, TurnState, bounded_generation,
};
pub(crate) use truncation::CONTINUATION_MARKER_PREFIX;
use truncation::{
    ContinuationBudget, ContinuationPlan, MAX_LENGTH_CONTINUATIONS, TIME_EXHAUSTED_PARTIAL,
    plan_continuation,
};
// Named only by the tests that pin the nudge's exact body; the production path
// reaches it through `Continuation::into_parts`.
#[cfg(test)]
use truncation::LENGTH_CONTINUATION_NUDGE;
// The latch count itself is only ever named by the test that pins it against
// the number of failures the summarizer is actually allowed (`audit_fixes`);
// the production path reads it through `SummarizerHealth::is_latched`.
#[cfg(test)]
use crate::step::SUMMARIZER_FAILURE_LATCH;
use crate::{AccountedCall, AccountedCallError, run_accounted_call};
use loop_evidence::{ResultIdentities, recent_call_records, snapshot_result_identities};
use tokio::sync::mpsc::UnboundedSender;

mod settlement;
use settlement::{BudgetWarnings, emit_budget_warning, record_settled_cost};

/// Everything about a turn's execution that isn't the provider/tools
/// themselves: prompt shape, retry/compaction/loop tuning, and hard
/// backstops. `Default` gives sensible starting values for `stella-cli`.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub effort: Option<ReasoningEffort>,
    /// Thinking-mode enable/disable forwarded to every completion —
    /// `CompletionRequest::reasoning` semantics (`None` = provider default).
    pub reasoning: Option<bool>,
    /// Sampling/routing overrides forwarded to every completion —
    /// `CompletionRequest::params` semantics (each adapter forwards the
    /// subset its dialect supports).
    pub params: Option<stella_protocol::GenerationParams>,
    pub retry_policy: RetryPolicy,
    pub loop_detection: LoopDetectionConfig,
    /// Compaction fires once the estimated conversation size exceeds this
    /// many tokens (`crate::estimator`). When calibration is attached
    /// ([`Engine::with_calibration`]) the comparison uses the
    /// drift-corrected estimate, so this budget is honored in the model's
    /// own observed tokens rather than raw heuristic tokens.
    pub compaction_budget_tokens: u64,
    /// Age-based tool-result retention (#1285): results older than this many
    /// tool-bearing steps are middle-out aged on every step, batched so the
    /// prompt-cache prefix is rewritten once per several steps rather than
    /// per step (`compaction::RETENTION_MIN_BATCH`). This is what holds the
    /// standing context roughly flat through a long turn — the budget above
    /// fires only near the context ceiling, which on a long trial is the very
    /// end of exactly the runs it should have shaped from the middle
    /// (measured: 4× more input per step than a comparator that trims old
    /// tool output, and ~quadratic total input growth). `None` disables the
    /// pass, restoring pure budget-triggered compaction.
    pub tool_result_horizon_steps: Option<usize>,
    /// When eviction/dedup/aging alone cannot reach the compaction budget
    /// (the oversized content is protected user/assistant text, or already
    /// stubbed), replace the oldest span of the conversation with a
    /// model-written summary instead of letting the next call overflow the
    /// provider's context window. Costs one cheap completion, metered into
    /// the same [`BudgetGuard`] as every other call.
    pub summarize_overflow: bool,
    /// Messages at the conversation tail the summarizer never touches —
    /// the recent work the model is actively reasoning over.
    pub summarize_keep_recent: usize,
    /// Hard backstop on step count, independent of loop detection — belt
    /// and suspenders, never the *primary* stuck-loop defense (that's
    /// `crate::loop_detect`).
    pub max_steps: usize,
    /// Hard backstop on how long one tool dispatch may run, independent of
    /// each tool's own bound — the same belt-and-suspenders philosophy as
    /// [`Self::max_steps`], and never the *primary* mechanism.
    ///
    /// Per-tool bounds are the real defense (bash clamps its own timeout,
    /// scripts cap out, MCP bounds each call), but each is that tool's own
    /// responsibility. A tool that misses its bound — a future native tool,
    /// an MCP server that overrides its timeout upward, a wedged blocking
    /// read — would otherwise park the step forever: loop detection, budget
    /// checks and soft-stop all fire at *step boundaries*, so a headless
    /// `stella run` hangs indefinitely with no way out but a hard cancel.
    ///
    /// When the ceiling trips, the call resolves to a `ToolOutput::Error`
    /// the model can react to, turning "hung forever" into one failed call
    /// the loop routes around. Deliberately generous (15 minutes) so it
    /// never pre-empts a legitimately slow tool. `None` disables the
    /// backstop entirely, restoring the unbounded await.
    ///
    /// Note this is the one place the engine may interrupt a tool in
    /// flight; the budget-abort invariant (clean aborts at step boundaries,
    /// never mid-tool) is unaffected, because a trip is surfaced as a tool
    /// *result*, not as a turn abort.
    pub tool_timeout: Option<Duration>,
    /// Backstop on a **single generation** — one provider dispatch, excluding
    /// the backoff sleeps between attempts. `None` restores the unbounded
    /// await.
    ///
    /// Without it nothing above the transport bounds a model call: the only
    /// limit is whichever reqwest deadline the adapter happens to carry, and
    /// those are per-read stalls, not whole-response bounds. Bedrock is the
    /// worst case — it is unary, so its 600s `UNARY_READ_TIMEOUT` covers the
    /// entire generation, and its expiry used to classify as retryable
    /// `Transport`, so one wedged request cost four full 600s attempts
    /// (~40 minutes) before surfacing.
    ///
    /// A trip is reported as `ProviderError::Terminal`, never `Transport`,
    /// following `stella-serve`'s reverse-RPC deadline: a provider that is
    /// simply not answering must not be handed the same unbounded wait again
    /// once per retry, which would multiply the very window the deadline
    /// exists to close.
    ///
    /// 13.6 minutes of SILENCE, not of elapsed time: `bounded_generation`
    /// measures idle gaps between stream fragments, so a generation that
    /// keeps producing is never cut however long it runs. The number is
    /// measured rather than chosen. It was 10 minutes (matching
    /// `UNARY_READ_TIMEOUT`) on the reasoning that no generation a caller
    /// still wants goes longer without progress. Context for the size: on
    /// the Terminal-Bench 2.1 gate a comparator on the same model, same API
    /// and same effort earned reward `1.0` on single steps of 624s and 756s
    /// — steps that stream throughout and are untouched by an idle bound,
    /// but that shaped the margin chosen here for a provider that has
    /// genuinely stopped answering.
    ///
    /// The rule fixing the constant is "never be the side that stops first":
    /// 60s above the longest single step the comparator was ever rewarded for
    /// (756s). Raising it is what makes a raised output ceiling reachable —
    /// the two are one budget, and moving either alone provably does nothing,
    /// which is why the earlier 16384 → 32000 → 64000 attempts each traded
    /// one truncation mode for another instead of clearing it.
    ///
    /// This bounds the *streaming* path, which is the real request path:
    /// adapters bound their streams by `STREAM_IDLE_TIMEOUT` (120s between
    /// chunks, not a total), so nothing under this cuts a generation that
    /// keeps producing. A non-streaming adapter still carries its own
    /// `UNARY_READ_TIMEOUT` (600s) and will bind before this does — that
    /// ceiling is untouched here and remains a per-adapter transport concern.
    pub model_timeout: Option<Duration>,
    /// Wall clock one turn may spend, used to decide whether a length
    /// continuation is affordable. `None` — the default — leaves the
    /// continuation allowance a pure count, exactly as before.
    ///
    /// This is not a timeout and nothing enforces it: no future is cancelled
    /// when it elapses. It exists so the engine can decline to *start* work it
    /// cannot finish. The constraint being modelled is external — a harness
    /// kills a trial on elapsed time (Terminal-Bench at 900s) — and an engine
    /// blind to it will spend its whole continuation allowance and be
    /// destroyed mid-continuation, turning a truthful truncated answer into an
    /// `AgentTimeoutError` that reads at the results layer exactly like the
    /// agent failing the task.
    ///
    /// Set it slightly below the real deadline: the useful behaviour is
    /// stopping with an honest partial while time remains, not racing the
    /// harness to the last second.
    pub turn_budget: Option<Duration>,
    /// Working directory reported to lifecycle hooks (`crate::hooks`) as the
    /// `cwd` of every [`HookPayload`]. Kept here — rather than sniffed via
    /// `std::env::current_dir()` inside the engine — so `stella-core`
    /// performs no I/O of its own: the caller
    /// (which already knows the workspace root) supplies the real path, and
    /// the `"."` default keeps hook-free turns unaffected. Only read when
    /// hooks are actually configured.
    pub cwd: String,
    /// Monotonic per-session turn index stamped onto this turn's context
    /// receipts (`BlockRegistered`/`StepManifest`, spec §3). Groups a
    /// `run_turn`'s steps across executions in one session without an
    /// event-order correlation. `0` when the caller does not track turns
    /// (the receipt is still valid — `(execution_id, step)` disambiguates
    /// within an execution).
    pub turn_instance: u32,
    /// `context.lifecycle.enabled` — Phase 2 (#713). Gates the compiled-frame
    /// identity on this turn's step manifests. The setting ships on; this
    /// field still defaults `false` so a programmatically-built config opts in
    /// deliberately rather than inheriting a product default it never read.
    pub lifecycle_enabled: bool,
    /// Where this engine's turns write their resume point, if anywhere.
    ///
    /// `None` — the default — is the behaviour every caller had before: a turn
    /// interrupted mid-flight is gone, and the host recovers by re-dispatching
    /// the prompt and paying for the work again. Attaching a sink makes the
    /// turn itself recoverable; see [`crate::step::CheckpointSink`] for the failure
    /// contract, and [`Engine::resume_turn`] for the other half.
    ///
    /// Kept here rather than passed to `run_turn` because it is a property of
    /// the host's durability arrangement, not of any one turn — the same
    /// reasoning that puts [`Self::cwd`] here rather than making the engine
    /// sniff it.
    pub checkpoint_sink: Option<Arc<dyn crate::step::CheckpointSink>>,
    /// A host-supplied "the goal is already met — stop now" signal, consulted
    /// at every step boundary.
    ///
    /// `None` — the default — is exactly the behaviour every caller had
    /// before: the turn runs until the model stops asking for tools, the
    /// budget bites, or the step cap does.
    ///
    /// This exists because those are all *external* stopping conditions, and
    /// an agent that has already met its goal will happily keep spending
    /// against them. Measured on Terminal-Bench 2.1 (run `or1`, task
    /// `pypi-server`): Stella wrote a correct solution, then spent its
    /// remaining wall clock re-litigating whether the verifier could see its
    /// commits, and the harness killed it mid-sentence. The trial scored 1.0
    /// only because the grader reads files on disk after the agent phase —
    /// the same behaviour on a task where the late flailing *edits* the
    /// answer converts a pass into a failure.
    ///
    /// The host owns the predicate because only the host knows what "done"
    /// means. `stella-pipeline` supplies one that fires when its flip oracle
    /// observes the tracked test go fail→pass.
    pub turn_halt: Option<Arc<dyn TurnHalt>>,
}

/// A caller's answer to "is the goal already met?", asked once per step
/// boundary.
///
/// Debug is a supertrait for the same reason [`crate::step::CheckpointSink`]
/// carries it: [`EngineConfig`] derives `Debug`, and a config that cannot be
/// printed is a config nobody can diagnose.
pub trait TurnHalt: Send + Sync + std::fmt::Debug {
    /// `Some(reason)` ends the turn cleanly at the next step boundary;
    /// `None` lets it continue.
    ///
    /// Called once per committed step, so it must be cheap and must not
    /// block. It is asked *between* steps and never mid-tool, which is what
    /// lets the turn end with a well-paired transcript.
    fn halt_reason(&self) -> Option<String>;
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            // 16k, not 8k: reasoning models (e.g. glm-5.2) can spend their whole
            // output budget on chain-of-thought and get cut off before emitting
            // any answer. 16k gives the answer room to land after reasoning and
            // is within every seeded catalog model's output ceiling — and it is
            // only the fallback: a model whose catalog entry declares its own
            // output ceiling overrides this at engine assembly
            // (stella-cli::agent::engine::tuned_engine_config).
            max_output_tokens: Some(16384),
            temperature: Some(0.0),
            effort: None,
            reasoning: None,
            params: None,
            retry_policy: RetryPolicy::standard(),
            loop_detection: LoopDetectionConfig::default(),
            compaction_budget_tokens: 150_000,
            // Eight tool-bearing steps of verbatim results, matching
            // `summarize_keep_recent`'s notion of "the recent work the model
            // is actively reasoning over". Older results keep head+tail; the
            // stub says how to re-fetch the rest.
            tool_result_horizon_steps: Some(8),
            summarize_overflow: true,
            summarize_keep_recent: 8,
            max_steps: 200,
            tool_timeout: Some(Duration::from_secs(15 * 60)),
            model_timeout: Some(Duration::from_secs(816)),
            // Off by default: only a caller that knows its own deadline can
            // supply a true one, and a guessed budget would decline work the
            // caller had time for.
            turn_budget: None,
            cwd: ".".to_string(),
            turn_instance: 0,
            lifecycle_enabled: false,
            // Off by default for the same reason `turn_budget` is: only a
            // caller that owns durable storage can say where a resume point
            // belongs, and a default location invented here would write one
            // process's turns into another's session.
            checkpoint_sink: None,
            // Off by default: a turn with no host-supplied notion of "done"
            // must behave exactly as it always has. Only a caller holding a
            // real completion signal — the pipeline's flip oracle — can say
            // the goal is met, and inventing one here would end turns that
            // had more to do.
            turn_halt: None,
        }
    }
}

/// How a turn ended.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnOutcome {
    /// The model produced a final text response with no further tool
    /// calls.
    Completed { text: String, cost_usd: f64 },
    /// The turn ended before completion: budget enforced, a loop was
    /// detected, retries were exhausted, or the step cap was hit. Always a
    /// *clean* abort — never mid-tool (see module docs).
    Aborted { reason: String, cost_usd: f64 },
}

/// The per-turn memos a step mutates but a [`crate::step::Checkpoint`]
/// deliberately does not carry.
///
/// All four exist to make repeated work cheap or repeated noise quiet within
/// one turn: the receipt ledger remembers which blocks it already registered,
/// the warning ledger claims the first observed-mode breach per axis, the
/// result identities remember what a call really produced before compaction
/// stubbed it (#554), and the summarizer latch stops a failing cheap model
/// re-firing every step. They live in one struct, held by
/// [`crate::step::TurnState`], because that is the whole set of per-turn state
/// whose types are internal to this module — a resumed turn rebuilds them
/// rather than deserializing four private caches out of a wire format.
pub(crate) struct TurnMemos {
    /// Block registry + residency for this turn's context receipts.
    receipts: ReceiptLedger,
    /// Observed-mode budget warnings already surfaced, per axis.
    warnings: BudgetWarnings,
    /// Pre-compaction tool-result identities ([`snapshot_result_identities`]).
    identities: ResultIdentities,
    /// Overflow-summarizer give-up latch.
    health: SummarizerHealth,
    /// Bumped by every pass that rewrites the transcript in place rather than
    /// appending to it, which is what tells the two position-keyed memos above
    /// — the result identities' positional half and the receipt ledger's block
    /// digests — that their keys no longer name the bytes behind them. It
    /// lives with the memos it invalidates, and is the reason a resumed turn
    /// can start it back at zero: everything keyed by it was rebuilt too.
    revision: TranscriptRevision,
}

impl TurnMemos {
    /// Fresh memos for a turn keyed against `turn_instance`, with
    /// `context.lifecycle.enabled` as configured.
    pub(crate) fn new(turn_instance: u32, lifecycle_enabled: bool) -> Self {
        Self {
            receipts: ReceiptLedger::new(turn_instance).with_lifecycle(lifecycle_enabled),
            warnings: BudgetWarnings::default(),
            identities: ResultIdentities::default(),
            health: SummarizerHealth::default(),
            revision: TranscriptRevision::default(),
        }
    }

    /// Record that the transcript was rewritten in place, invalidating both
    /// position-keyed memos at their next sync.
    pub(crate) fn mark_rewritten(&mut self) {
        self.revision.rewritten();
    }
}

/// The two references a turn needs to fire lifecycle hooks: the parsed
/// workspace [`Hooks`] config and the [`HookRunner`] execution port that
/// actually spawns the commands (the real process I/O `stella-core` never
/// performs — see `crate::hooks`). Bundled so the engine carries a single
/// `Option`: `None` means hooks are entirely off and the turn path is
/// byte-for-byte the same as before this seam existed. `Copy` because both
/// fields are shared references.
#[derive(Clone, Copy)]
pub(crate) struct HooksHandle<'a> {
    hooks: &'a Hooks,
    runner: &'a dyn HookRunner,
}

/// The step-driver. Holds no conversation state of its own — `run_turn`
/// takes the message history by `&mut` reference so callers (one-shot CLI,
/// REPL, fleet worker) own persistence and can inspect history after an
/// aborted turn.
pub struct Engine<'a> {
    pub(crate) provider: &'a dyn Provider,
    pub(crate) tools: &'a dyn ToolExecutor,
    pub(crate) sleeper: &'a dyn Sleeper,
    pub(crate) config: EngineConfig,
    pub(crate) call_role: stella_protocol::ModelCallRole,
    /// Lifecycle hooks, off by default. Attached via [`Engine::with_hooks`]
    /// so `with_sleeper` keeps its existing signature. When `None`,
    /// no hook is ever consulted and the turn path adds zero work.
    pub(crate) hooks: Option<HooksHandle<'a>>,
    /// Token-drift calibration (`crate::estimator::CalibrationMap`), off by
    /// default. Attached via [`Engine::with_calibration`]; the caller owns
    /// the map across turns (and seeds it from persisted telemetry at
    /// session start), the engine feeds it every committed step's
    /// (estimated, actual) pair and reads the correction back into the
    /// compaction decision. When `None` the turn path is exactly the
    /// uncalibrated engine.
    pub(crate) calibration: Option<&'a CalibrationMap>,
    /// Boundary pause gate ([`crate::ports::TurnGate`]), off by default.
    /// Attached via [`Engine::with_gate`]; consulted once per step, before
    /// any model call — a paused turn parks at that safe boundary and
    /// spends nothing until resumed. `None` adds zero work.
    pub(crate) gate: Option<&'a dyn crate::ports::TurnGate>,
    /// Step-boundary steering ([`crate::ports::TurnSteering`]), off by
    /// default. Attached via [`Engine::with_steering`]; drained once per
    /// step at the same boundary as the pause gate — queued user messages
    /// become the model's next observation, and a latched soft stop ends
    /// the turn keeping every completed step. `None` adds zero work.
    pub(crate) steering: Option<&'a dyn crate::ports::TurnSteering>,
    /// Extension hook bus ([`crate::bus::HookBus`]), off by default.
    /// Attached via [`Engine::with_bus`]; receives the turn/step/model-call
    /// lifecycle events an out-of-process host uses to observe and, per
    /// ADR-033, apply policy at (#1133). `None` adds zero work — every emit
    /// site is behind the same `if let Some(bus)`.
    ///
    /// Strictly **observer-only**. These names are not on the blocking
    /// allowlist, so `emit_named` dispatches them without a policy chain: an
    /// extension can watch a step begin, it cannot veto one. The interception
    /// points stay where they already are, on the tool-call path.
    pub(crate) bus: Option<&'a crate::bus::HookBus>,
}

/// Why a turn that never produced a terminal step ends.
///
/// A function rather than an inline `format!` because it is now written by
/// two loops: [`Engine::run_turn_with_sender`] and any host driving
/// [`Engine::run_step`] itself (`stella-serve`, #1129). This string reaches a
/// transcript, a log, and a user's terminal — two copies of it would drift,
/// and the drift would read as two different failures.
#[must_use]
pub fn step_cap_reason(max_steps: usize) -> String {
    format!(
        "reached the step cap ({max_steps}) without completing — this is the belt-and-suspenders \
         backstop; loop detection should normally catch a stuck turn first"
    )
}

/// The most recent non-empty assistant text in a turn's transcript.
///
/// Used only by the halt path ([`TurnHalt`]), which ends a turn at a step
/// boundary and therefore has no "final" model text of its own to report. An
/// assistant message that only made tool calls carries empty `content`, so
/// this walks back to the last thing the model actually *said* rather than
/// reporting a blank answer for a turn that did real work.
///
/// `None` when the model has said nothing yet — a turn halted after a first
/// step of pure tool calls — which the caller renders as the halt reason.
fn last_assistant_text(state: &crate::step::TurnState) -> Option<String> {
    state
        .messages
        .iter()
        .rev()
        .find(|message| {
            message.role == MessageRole::Assistant && !message.content.trim().is_empty()
        })
        .map(|message| message.content.clone())
}

/// Upper bound on tool calls from one step executing concurrently. Tools
/// are I/O-bound (process spawns, file reads), so this caps descriptor and
/// process pressure, not CPU.
const MAX_CONCURRENT_TOOL_CALLS: usize = 8;

/// `SpeculationDiscarded.reason` — a speculative pool was dropped because
/// its stream attempt failed (or the turn was cancelled) before its
/// read-only work could be harvested.
pub(crate) const SPECULATION_DISCARD_ATTEMPT_FAILED: &str = "attempt_failed";
/// `SpeculationDiscarded.reason` — a speculative pool entry was rejected at
/// dispatch because the committed call diverged from what was announced, or
/// no committed call ever claimed it.
const SPECULATION_DISCARD_HARVEST_MISMATCH: &str = "harvest_mismatch";
/// `SpeculationDiscarded.reason` — a committed step's pool ran read-only I/O
/// but the turn aborted on an enforced budget before `dispatch_completion`
/// could harvest it, so it is discarded on the abort unwind instead (#460).
const SPECULATION_DISCARD_BUDGET_ABORT: &str = "budget_abort";

const BUDGET_ABORT_TOOL_RESULT: &str = "not executed — turn aborted on budget";

/// The `ToolOutput::Error` that closes a `tool_use` left open when the soft
/// stop lands — same repair the cancel exit performs at this boundary
/// (`crate::step::close_open_tool_calls`), with wording that names who
/// stopped the turn.
const SOFT_STOP_TOOL_RESULT: &str = "not executed — turn stopped by user at a step boundary";

/// One committed model call plus the step-scoped context the phases after
/// it consume: the pre-call raw token estimate (drift feedback + telemetry
/// — raw, never calibrated, attachments excluded, see
/// [`Engine::run_model_call`]) and the read-only tool set for dispatch
/// scheduling. The step's `StepUsage`
/// metering record (retry and duration figures included) was already
/// emitted by [`Engine::run_model_call`] at the no-await settlement
/// boundary — it is deliberately NOT carried here.
struct CommittedStep {
    result: CompletionResultAlias,
    budget_outcome: BudgetOutcome,
    /// Names of tools whose schemas declare `read_only`, snapshotted from
    /// the same `schemas()` call the request itself was built from.
    read_only_tools: HashSet<String>,
    /// Read-only calls executed speculatively while THIS committed
    /// attempt's response was still streaming (`crate::speculation`).
    /// Dispatch harvests matching entries instead of re-executing; a failed
    /// attempt's pool never gets here — it is dropped with the attempt.
    speculation: SpeculationPool,
    estimated_input_tokens: u64,
}

impl<'a> Engine<'a> {
    /// Construct an engine with an injected [`Sleeper`]. This is the only
    /// constructor — `stella-core` exports the port, never a production
    /// impl, so the caller wires a real sleeper (the CLI's tokio-backed
    /// one) and tests wire a no-op to run retries with zero real
    /// wall-clock delay.
    pub fn with_sleeper(
        provider: &'a dyn Provider,
        tools: &'a dyn ToolExecutor,
        config: EngineConfig,
        sleeper: &'a dyn Sleeper,
    ) -> Self {
        Self {
            provider,
            tools,
            sleeper,
            config,
            call_role: stella_protocol::ModelCallRole::Worker,
            hooks: None,
            calibration: None,
            gate: None,
            steering: None,
            bus: None,
        }
    }

    /// Attribute this engine's provider calls to a concrete pipeline role.
    /// Ordinary execution defaults to [`stella_protocol::ModelCallRole::Worker`].
    /// The role this engine attributes its model calls to — the reader for
    /// [`Engine::with_call_role`].
    ///
    /// Public because a host that drives [`Engine::run_step`] itself owns the
    /// turn framing (`stella-serve`), and the `agent.turn.started` payload
    /// names the role. Without this the host would have to keep its own copy
    /// of the engine's default, which is a second source of truth for a value
    /// it does not own.
    #[must_use]
    pub fn call_role(&self) -> stella_protocol::ModelCallRole {
        self.call_role
    }

    pub fn with_call_role(mut self, role: stella_protocol::ModelCallRole) -> Self {
        self.call_role = role;
        self
    }

    /// A shallow copy of this engine whose receipts key against
    /// `turn_instance`. Context receipts are persisted under
    /// `(execution_id, turn_instance, step, call_seq)` and every `run_turn`
    /// restarts `step` at 0 — so a caller that drives several turns inside
    /// one execution (the goal loop's judged rounds, each round's judge
    /// assessment) must give each of them its own turn instance or the
    /// later manifests silently overwrite the earlier ones in the store.
    /// Everything else about the engine (provider, tools, hooks, gates,
    /// calibration, role) is carried over unchanged.
    #[must_use]
    pub fn with_turn_instance(&self, turn_instance: u32) -> Engine<'a> {
        Engine {
            provider: self.provider,
            tools: self.tools,
            sleeper: self.sleeper,
            config: EngineConfig {
                turn_instance,
                ..self.config.clone()
            },
            call_role: self.call_role,
            hooks: self.hooks,
            calibration: self.calibration,
            gate: self.gate,
            steering: self.steering,
            bus: self.bus,
        }
    }

    /// A shallow copy of this engine that consults `halt` at every step
    /// boundary and ends the turn as [`TurnOutcome::Completed`] when it
    /// fires.
    ///
    /// Takes `&self` and returns a new engine — the [`Self::with_turn_instance`]
    /// shape rather than the consuming-builder shape — because the caller that
    /// knows the goal is met is `stella-pipeline`, which is handed an
    /// `&Engine` it does not own and needs a per-candidate variant of it.
    ///
    /// Deliberately NOT expressed through [`crate::ports::TurnSteering`]'s
    /// soft stop, which is the other step-boundary exit: that one is a *user*
    /// asking to stop and returns `Aborted`, which reaches the CLI as a
    /// non-zero exit and is scored by benchmark harnesses as the agent
    /// crashing. "The goal is met" is a success, and has to end the turn as
    /// one.
    pub fn with_turn_halt(&self, halt: Arc<dyn TurnHalt>) -> Engine<'a> {
        Engine {
            provider: self.provider,
            tools: self.tools,
            sleeper: self.sleeper,
            config: EngineConfig {
                turn_halt: Some(halt),
                ..self.config.clone()
            },
            call_role: self.call_role,
            hooks: self.hooks,
            calibration: self.calibration,
            gate: self.gate,
            steering: self.steering,
            bus: self.bus,
        }
    }

    /// Attach lifecycle hooks (`crate::hooks`) to an engine, opt-in. Kept a
    /// builder so [`Engine::with_sleeper`] retains its signature and every
    /// existing call site is unchanged — an engine
    /// built without this is exactly the pre-hooks engine. Takes both the
    /// parsed [`Hooks`] config and the [`HookRunner`] that executes the
    /// commands, because [`crate::hooks::run_hooks`] needs the port to run
    /// anything (the config alone spawns nothing).
    pub fn with_hooks(mut self, hooks: &'a Hooks, runner: &'a dyn HookRunner) -> Self {
        self.hooks = Some(HooksHandle { hooks, runner });
        self
    }

    /// Attach token-drift calibration, opt-in and by reference for the same
    /// reason `run_turn` borrows `messages`: the caller (CLI session, REPL,
    /// fleet worker) owns state that outlives any single turn — engines are
    /// constructed per turn, calibration accumulates per session. An engine
    /// built without this estimates exactly as before.
    pub fn with_calibration(mut self, calibration: &'a CalibrationMap) -> Self {
        self.calibration = Some(calibration);
        self
    }

    /// Attach a boundary pause gate — Pause/Resume at step granularity,
    /// never mid-tool.
    pub fn with_gate(mut self, gate: &'a dyn crate::ports::TurnGate) -> Self {
        self.gate = Some(gate);
        self
    }

    /// Attach step-boundary steering — mid-turn user messages and the soft
    /// stop, at step granularity, never mid-tool.
    pub fn with_steering(mut self, steering: &'a dyn crate::ports::TurnSteering) -> Self {
        self.steering = Some(steering);
        self
    }

    /// The step cap this engine enforces — the loop bound a host driving
    /// [`Engine::run_step`] itself must apply, since the cap belongs to the
    /// engine's config and a host tracking its own copy is a second source of
    /// truth for the same number (#1129).
    #[must_use]
    pub fn max_steps(&self) -> usize {
        self.config.max_steps
    }

    /// Attach an extension hook bus, so the turn, step and model-call
    /// boundaries this engine already crosses become observable (#1133).
    ///
    /// Before this, the only production emitter on the bus was the tool
    /// registry: an extension could see every tool call and nothing about the
    /// turn that made them — not that a turn started, not that a model call
    /// began or what it cost. The catalog and `emit_named` both already
    /// existed; what was missing was the call sites, which are here because
    /// this is where the boundaries are.
    ///
    /// Observer-only, by construction: see the `bus` field.
    pub fn with_bus(mut self, bus: &'a crate::bus::HookBus) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Emit one lifecycle event, if a bus is attached.
    ///
    /// The payload closure is only called when there *is* a bus, so a
    /// bus-less engine — every CLI turn today — does not pay to build a
    /// `Value` nobody will read.
    pub(crate) fn emit_lifecycle(&self, name: &str, payload: impl FnOnce() -> serde_json::Value) {
        if let Some(bus) = self.bus {
            bus.emit_named(name, payload());
        }
    }

    /// Fire `SessionStart` hooks once and return any stdout they produced —
    /// the additional system-prompt context described in `crate::hooks`.
    ///
    /// This is deliberately NOT called from [`Engine::run_turn`].
    /// `SessionStart` is a session-level event ("runs once before the
    /// turn"), but `run_turn` is per-turn and a REPL or fleet worker calls
    /// it many times per session — firing it inside would re-run session
    /// setup on every turn. Prompt assembly is the caller's concern anyway
    /// (`run_turn` takes history by `&mut` and never owns the system
    /// prompt), so the caller invokes this once, before the first turn, and
    /// folds the returned context into the system message it builds.
    /// Returns `None` when no hooks are attached or the hooks printed
    /// nothing.
    pub async fn run_session_start_hooks(&self) -> Option<String> {
        let handle = self.hooks?;
        let outcome = run_hooks(
            handle.runner,
            Some(handle.hooks),
            &HookPayload::session_start(self.config.cwd.clone()),
        )
        .await;
        if outcome.output.is_empty() {
            None
        } else {
            Some(outcome.output)
        }
    }

    /// Drive one turn to completion or a clean abort, appending every
    /// message to `messages` and streaming an `AgentEvent` for every
    /// boundary over `events`. `budget` is `&mut` because spend
    /// accumulates across the turn (and, via `BudgetGuard::begin_turn`,
    /// across turns in the same session — the caller decides when to reset
    /// it, `run_turn` only reads and records).
    ///
    /// Every step is the same fixed phase sequence, one sub-method per
    /// phase: compaction, loop detection, the between-steps budget check,
    /// the model call (with retry+backoff), bookkeeping for the committed
    /// call, then dispatch — complete the turn or execute its tool calls.
    pub async fn run_turn(
        &self,
        messages: &mut Vec<CompletionMessage>,
        budget: &mut BudgetGuard,
        events: &UnboundedSender<AgentEvent>,
    ) -> TurnOutcome {
        let events = EventSender::new(events.clone());
        self.run_turn_with_sender(messages, budget, &events).await
    }

    /// [`Self::run_turn`] with a caller-supplied ordered event boundary.
    /// Existing callers use an ordinary Tokio sender; benchmark callers use
    /// this form so append+flush completes synchronously before a paid-call
    /// producer can advance to another request.
    ///
    /// The body is a loop over [`Self::run_step`] and nothing else — the step
    /// is the only implementation of a step, so a host that drives steps
    /// itself and a caller that drives whole turns cannot diverge (#971).
    pub async fn run_turn_with_sender(
        &self,
        messages: &mut Vec<CompletionMessage>,
        budget: &mut BudgetGuard,
        events: &EventSender,
    ) -> TurnOutcome {
        let _ = events.send(AgentEvent::Stage {
            name: StageKind::Execute,
        });
        self.emit_lifecycle(bus::names::AGENT_TURN_STARTED, || {
            lifecycle::turn_started_payload(messages.len(), self.config.max_steps, self.call_role)
        });
        // The turn's state is OWNED for the duration and written back to the
        // caller's borrows when it drops — including on the hard-cancel path,
        // where the future is dropped mid-step and there is no exit to copy
        // back from (see `BorrowedTurn`).
        let mut turn = BorrowedTurn::adopt(messages, budget, &self.config);
        while turn.state.step < self.config.max_steps {
            if let Some(outcome) = self
                .run_step(&mut turn.state, events)
                .await
                .into_turn_outcome()
            {
                self.emit_lifecycle(bus::names::AGENT_TURN_COMPLETED, || {
                    turn_outcome_payload(&outcome, turn.state.step)
                });
                self.discard_checkpoint();
                return outcome;
            }
            // The checkpoint seam (#971). `StepOutcome::Continue` is the one
            // moment the transcript is guaranteed well-paired — no `tool_use`
            // without its `tool_result` — and `state.step` has already been
            // advanced by `run_step`, so the snapshot names the step that runs
            // NEXT rather than the one that just finished. Writing here and
            // nowhere else is what makes a resumed turn indistinguishable from
            // one that was never interrupted.
            self.persist_checkpoint(&turn.state);

            // "The goal is already met" — the one stopping condition that is
            // not a limit being hit. Asked here, at a committed step
            // boundary, for the same reason the checkpoint is written here:
            // `StepOutcome::Continue` is the moment the transcript is
            // guaranteed well-paired, so ending now can never orphan a
            // `tool_use` from its `tool_result`. Asking mid-step could.
            //
            // Placed AFTER the first step rather than at the top of the loop
            // so a turn always does something: a halt predicate that is
            // already true on entry describes a goal met before this turn
            // began, which is the caller's business to notice, not a reason
            // to return an empty turn.
            //
            // `Completed`, never `Aborted`. This turn did its work; `Aborted`
            // is the failure arm and reaches the CLI as a non-zero exit,
            // which Harbor — and any other host that reads exit codes —
            // scores identically to the agent crashing.
            if let Some(reason) = self
                .config
                .turn_halt
                .as_ref()
                .and_then(|halt| halt.halt_reason())
            {
                // Prefer the model's own last words; fall back to the
                // predicate's reason when the final step was pure tool calls
                // (an assistant message that only called tools has empty
                // `content`), so the turn never reports an empty answer.
                let text = last_assistant_text(&turn.state).unwrap_or_else(|| reason.clone());
                let outcome = TurnOutcome::Completed {
                    text,
                    cost_usd: turn.state.total_cost_usd,
                };
                self.emit_lifecycle(bus::names::AGENT_TURN_COMPLETED, || {
                    turn_outcome_payload(&outcome, turn.state.step)
                });
                self.discard_checkpoint();
                return outcome;
            }
        }

        let reason = step_cap_reason(self.config.max_steps);
        let _ = events.send(AgentEvent::Error {
            message: reason.clone(),
            retryable: false,
        });
        let outcome = TurnOutcome::Aborted {
            reason,
            cost_usd: turn.state.total_cost_usd,
        };
        // The step-cap exit is a turn ending like any other. Emitting only
        // from the loop above would leave an observer's `turn.started`
        // unpaired on precisely the runaway turn it most wants to see.
        self.emit_lifecycle(bus::names::AGENT_TURN_COMPLETED, || {
            turn_outcome_payload(&outcome, turn.state.step)
        });
        // A turn that ran into the step cap is over, however unhappily. Leaving
        // its checkpoint behind would offer a resume that replays a turn whose
        // whole problem was that it would not stop.
        self.discard_checkpoint();
        outcome
    }

    /// Write `state`'s resume point through the configured
    /// [`crate::step::CheckpointSink`], if the host attached one.
    ///
    /// Public because a host that drives [`Self::run_step`] itself — as
    /// `stella-serve` does — owns its own loop and must reach the same seam.
    /// Sharing this method rather than letting each driver write its own is
    /// what keeps a served turn and a CLI turn equally recoverable.
    ///
    /// Silent on every failure path, by the sink contract: an unwritable
    /// checkpoint leaves the turn exactly as recoverable as it was before the
    /// sink existed, and failing a live turn to report a durability
    /// *improvement* would be strictly worse than not offering one.
    pub fn persist_checkpoint(&self, state: &crate::step::TurnState) {
        let Some(sink) = self.config.checkpoint_sink.as_ref() else {
            return;
        };
        if let Ok(json) = state.to_checkpoint().to_json() {
            sink.persist(&json);
        }
    }

    /// Drop any resume point for the turn that just ended.
    ///
    /// Called on every terminal path — completion, abort, and the step cap —
    /// because a checkpoint that outlives its turn is worse than none: it
    /// invites a resume that re-runs work the caller already saw finish.
    pub fn discard_checkpoint(&self) {
        if let Some(sink) = self.config.checkpoint_sink.as_ref() {
            sink.discard();
        }
    }

    /// Run exactly ONE committed step against `state`, the unit a durable host
    /// checkpoints and cancels between (#971).
    ///
    /// The fixed phase sequence, in order: cancellation check → pause gate →
    /// cancellation check → steering drain and soft stop → budget check →
    /// result-identity snapshot → compaction pass → loop detection → budget
    /// check → the model call (with retry+backoff) → committed-result
    /// bookkeeping → dispatch. Every per-step `AgentEvent` [`Self::run_turn`]
    /// emits is emitted here, in the same order — `run_turn` IS this method in
    /// a loop, so there is one code path and no second implementation to
    /// drift. What `run_turn` adds around the loop is turn framing only: the
    /// initial `Stage(Execute)` and, on the step-cap exit, a non-retryable
    /// `Error` carrying [`step_cap_reason`] — a host driving steps itself owns
    /// both (see `stella-engine`'s crate docs).
    ///
    /// `StepOutcome::Continue` means the step committed and `state.step` has
    /// advanced; anything else ends the turn and leaves `state.step` naming
    /// the step that ended it. A checkpoint is only guaranteed to hold a
    /// well-paired transcript (every `tool_use` answered) after `Continue`.
    ///
    /// # Stopping: cancel, don't drop
    ///
    /// [`crate::step::CancelToken`] is checked at the top of the step and
    /// again when the pause gate releases — the same safe boundary the budget
    /// enforcer and the soft stop use, and never between announcing a tool
    /// call and recording its result.
    ///
    /// Dropping the returned future stops the step too, but it is not a
    /// boundary: it lands wherever the future was awaiting. A tool may be
    /// half-way through mutating the workspace with its result never reaching
    /// the transcript (leaving an unpaired `tool_use` the next provider call
    /// rejects); an in-flight model call may already be billed, leaving only
    /// the `UsageIncomplete { Cancelled }` envelope `CancelUsageGuard` emits;
    /// speculative read-only work in flight is lost with one
    /// `SpeculationDiscarded` each. Prefer the token, and expect to discard
    /// the turn's history when you cannot.
    ///
    /// # This future is `!Send`
    ///
    /// The step borrows `&dyn Provider` / `&dyn ToolExecutor` and holds
    /// non-`Send` futures across awaits, so it cannot be `tokio::spawn`ed onto
    /// a multi-thread runtime. Drive it on a current-thread runtime — a host
    /// serving many sessions gives each its own OS thread with its own
    /// current-thread runtime and bridges with `Send` channels.
    pub async fn run_step(&self, state: &mut TurnState, events: &EventSender) -> StepOutcome {
        // No bus, no wrapper: a CLI turn must not pay for a boundary nobody
        // observes, and the `step` copy plus two closures below are exactly
        // that cost.
        let Some(_) = self.bus else {
            return self.run_step_inner(state, events).await;
        };
        // Read before the step runs, because a committed step advances it —
        // `started` and `completed` have to name the same step or an observer
        // cannot pair them.
        let step = state.step;
        self.emit_lifecycle(
            bus::names::AGENT_STEP_STARTED,
            || serde_json::json!({ "step": step }),
        );
        let outcome = self.run_step_inner(state, events).await;
        // Emitted here rather than at each of the step's exits, and that is
        // the whole reason this wrapper exists: `run_step_inner` returns from
        // roughly a dozen places (cancel, soft stop, four budget checks, loop
        // detection, model failure, dispatch), and a `completed` emit
        // distributed across all of them is one added early-return away from
        // being silently unpaired.
        self.emit_lifecycle(bus::names::AGENT_STEP_COMPLETED, || {
            serde_json::json!({
                "step": step,
                "outcome": step_outcome_label(&outcome),
                "cost_usd": state.total_cost_usd,
            })
        });
        outcome
    }

    /// The step itself. See [`Engine::run_step`] for why the lifecycle
    /// emission wraps this rather than living inside it.
    ///
    /// One honest gap: a caller that **drops** the turn future mid-step gets
    /// no `agent.step.completed`, because there is no return to emit it from.
    /// That is the same loss a hard drop already inflicts on usage accounting
    /// and speculation (see `crate::step::CancelToken`), and the reason the
    /// engine's docs tell hosts to cancel rather than drop. A *cancel*
    /// returns normally and does emit.
    async fn run_step_inner(&self, state: &mut TurnState, events: &EventSender) -> StepOutcome {
        // Before the gate, not after: a turn that is both paused and cancelled
        // must not park waiting for a resume that is never coming.
        if let Some(cancelled) = state.cancel_outcome(events) {
            return cancelled;
        }
        // Pause parks HERE — after the previous step fully settled and
        // before any new model call, mirroring the budget-abort
        // boundary. Resuming continues the very same turn.
        if let Some(gate) = self.gate {
            gate.wait_if_paused().await;
        }
        // A cancel that arrived while parked is answered on release rather
        // than one whole step later.
        if let Some(cancelled) = state.cancel_outcome(events) {
            return cancelled;
        }
        // Steering rides the same safe boundary as the pause gate:
        // queued user messages land BEFORE compaction (so the pass sees
        // them) and before the model call (so it answers them this
        // step). Drain precedes the soft-stop check deliberately — a
        // steer typed just before Esc is preserved in history for the
        // next turn instead of evaporating with the per-turn tap.
        if let Some(steering) = self.steering {
            for text in steering.drain_steering() {
                let _ = events.send(AgentEvent::Steered { text: text.clone() });
                state.messages.push(CompletionMessage::user(text));
            }
            if steering.soft_stop_requested() {
                // A user choice, not a failure: no Error event, and the
                // caller keeps every completed step (unlike the hard
                // cancel, which drops the future and truncates). Open
                // `tool_use`s are closed exactly as the cancel exit closes
                // them: the engine's own path never leaves one open at this
                // boundary, but history a caller handed in (or appended to)
                // mid-turn can, and the kept transcript must stay valid for
                // the next turn on this exit for the same reason it must on
                // that one.
                crate::step::close_open_tool_calls(
                    &mut state.messages,
                    SOFT_STOP_TOOL_RESULT,
                    events,
                );
                return StepOutcome::Aborted {
                    reason: SOFT_STOP_REASON.to_string(),
                    cost_usd: state.total_cost_usd,
                };
            }
        }
        if let Some(aborted) = self.check_budget(
            &mut state.budget,
            &mut state.total_cost_usd,
            &mut state.memos.warnings,
            events,
        ) {
            return aborted.into();
        }
        // BEFORE compaction, never after: the pass rewrites tool results
        // in place, and loop detection runs on the rewritten history in
        // this very step (#554).
        let revision = state.memos.revision;
        snapshot_result_identities(&state.messages, &mut state.memos.identities, revision);
        let pass = self
            .run_compaction_pass(
                &mut state.messages,
                state.calibration_model.as_deref(),
                &mut state.budget,
                &mut state.memos.health,
                state.step,
                events,
            )
            .await;
        state.total_cost_usd += pass.cost_usd;
        if pass.rewrote {
            state.mark_transcript_rewritten();
        }
        // The manifest reports the budget compaction just compared against.
        let (effective_budget, calibration_factor) =
            self.effective_compaction_budget(state.calibration_model.as_deref());
        state
            .memos
            .receipts
            .set_effective_budget(effective_budget, calibration_factor);
        // Set immediately after the pass that may have rewritten the
        // transcript, so the ledger's digest memo cannot serve a stale block.
        let revision = state.memos.revision;
        state.memos.receipts.set_transcript_revision(revision);

        if let Some(aborted) = self.check_loop_detection(
            &mut state.messages,
            &state.memos.identities,
            &mut state.loop_steered,
            state.total_cost_usd,
            events,
        ) {
            return aborted.into();
        }
        if let Some(aborted) = self.check_budget(
            &mut state.budget,
            &mut state.total_cost_usd,
            &mut state.memos.warnings,
            events,
        ) {
            return aborted.into();
        }

        // The model-call boundary. `run_model_call` spans the whole retry
        // ladder, so `started`/`completed` bracket the *logical* call rather
        // than each attempt — a retried call is one request from an
        // observer's point of view, and the retries are already reported as
        // their own `AgentEvent`s.
        self.emit_lifecycle(bus::names::MODEL_REQUEST_STARTED, || {
            lifecycle::model_request_started_payload(
                state.step,
                state.messages.len(),
                self.call_role,
            )
        });
        // Wall clock around the whole call including its retries, because that
        // is what a continuation would actually cost again — not the duration
        // of the one attempt that happened to succeed.
        let step_started = std::time::Instant::now();
        let committed = match self
            .run_model_call(
                state.step,
                &state.messages,
                &mut state.budget,
                &mut state.memos.receipts,
                &mut state.memos.warnings,
                events,
            )
            .await
        {
            Ok(committed) => committed,
            Err(reason) => {
                // `reason` is the engine's own summary of why the call could
                // not commit (retries exhausted, an unretryable provider
                // error). It is the same string that reaches the transcript
                // and `AgentEvent::Error`, so emitting it here adds no
                // exposure that the turn does not already have.
                self.emit_lifecycle(
                    bus::names::MODEL_REQUEST_FAILED,
                    || serde_json::json!({ "step": state.step, "reason": reason }),
                );
                return StepOutcome::Aborted {
                    reason,
                    cost_usd: state.total_cost_usd,
                };
            }
        };
        self.emit_lifecycle(bus::names::MODEL_REQUEST_COMPLETED, || {
            lifecycle::model_request_completed_payload(state.step, &committed.result)
        });
        state.last_step = Some(step_started.elapsed());
        state.calibration_model = Some(committed.result.model.clone());
        state.total_cost_usd += committed.result.cost_usd;

        if let Some(aborted) = self.handle_committed_result(
            &committed,
            state.total_cost_usd,
            &mut state.messages,
            &mut state.memos.warnings,
            events,
        ) {
            // The budget-abort unwind never reaches `dispatch_completion`,
            // which is where a committed step's speculation pool is
            // otherwise harvested or discarded. Any read-only calls this
            // step already speculated would drop silently here — account
            // for them so #370's guarantee holds on the abort path too
            // (#460). `handle_committed_result` only borrows `committed`,
            // so the pool is still ours to move.
            self.discard_speculation_pool(
                committed.speculation,
                SPECULATION_DISCARD_BUDGET_ABORT,
                events,
            );
            return aborted.into();
        }

        // Only meaningful once a step has been timed and a budget configured:
        // the forecast for one more continuation is what the last one cost, and
        // without a deadline there is nothing to compare it against.
        let continuation_budget =
            self.config
                .turn_budget
                .zip(state.last_step)
                .map(|(budget, last_step)| ContinuationBudget {
                    remaining: budget.saturating_sub(state.started_at.elapsed()),
                    last_step,
                });

        if let Some(completed) = self
            .dispatch_completion(
                committed,
                state.total_cost_usd,
                &mut state.messages,
                &mut state.length_continuations,
                continuation_budget,
                events,
            )
            .await
        {
            return completed.into();
        }

        // Advanced only by a step that committed and continued, so the index
        // a checkpoint carries is always "the step that runs next".
        state.step += 1;
        StepOutcome::Continue
    }

    /// A fresh [`TurnState`] over `messages`, keyed to this engine's receipt
    /// settings — the step-scoped counterpart of handing `run_turn` a history
    /// and a meter.
    #[must_use]
    pub fn new_turn(&self, messages: Vec<CompletionMessage>, budget: BudgetGuard) -> TurnState {
        TurnState::new(messages, budget, &self.config)
    }

    /// A [`TurnState`] resumed from a durable snapshot. See
    /// [`TurnState::from_checkpoint`] for exactly what resuming rebuilds
    /// rather than restores.
    ///
    /// # Who calls this, and who deliberately does not
    ///
    /// This is for a host that drives [`Self::run_step`] itself and owns its
    /// own loop — it hands back a state to keep stepping. Neither shipping
    /// surface is one today, and that is a declaration rather than an
    /// oversight:
    ///
    /// - **The CLI** resumes an interrupted turn at *transcript* granularity
    ///   instead. Its turns are dispatched through `stella-pipeline`, which
    ///   owns turn framing and builds its own state via `run_turn`, so there is
    ///   no seam to hand a resumed `TurnState` to without threading a
    ///   checkpoint through the entire verification ladder. What it does do is
    ///   reopen the conversation at the checkpoint's step boundary, so the
    ///   completed steps' work is not re-run — see `stella-parity`'s
    ///   `turn.checkpoint_resume` row.
    /// - **`stella-serve`** stores one and hands it back on request but accepts
    ///   none in return, so continuing is the host's call (same row, API side).
    ///
    /// So the production callers are embedders, and the in-tree exercise of it
    /// is `stella-engine`'s test suite. Anything that changes on either surface
    /// should change that row in the same PR.
    #[must_use]
    pub fn resume_turn(&self, checkpoint: crate::step::Checkpoint) -> TurnState {
        TurnState::from_checkpoint(checkpoint, &self.config)
    }

    /// The compaction budget this turn's next step will actually compare
    /// against, and the calibration factor that produced it. Single source of
    /// truth for both the compaction pass and the step manifest, so the
    /// receipt's `effective_budget_tokens` is exactly the number the decision
    /// used (#364 item 1). Returns the configured budget with factor `1.0`
    /// when calibration is off.
    ///
    /// Drift correction enters here: `compact` compares the RAW estimate
    /// against the budget it is given, so dividing the configured budget
    /// by the correction factor is exactly comparing the CALIBRATED
    /// estimate (raw × factor) against the configured budget — including
    /// the eviction loop's stopping condition — without threading a factor
    /// through compaction's incremental bookkeeping. A factor > 1 (we
    /// under-estimate this model's tokenizer) shrinks the effective budget
    /// and compacts earlier; the factor's clamp (`crate::estimator`)
    /// bounds how far either way a noisy sample can move this.
    fn effective_compaction_budget(&self, calibration_model: Option<&str>) -> (u64, f64) {
        match self.calibration {
            Some(calibration) => {
                let factor = calibration.factor(calibration_model);
                (
                    (self.config.compaction_budget_tokens as f64 / factor) as u64,
                    factor,
                )
            }
            None => (self.config.compaction_budget_tokens, 1.0),
        }
    }

    /// Compaction, before every model call, per the running estimate
    /// (L-E3 dedup+evict, stable system prefix — the system message is
    /// index 0 and `compact()` never touches it). The budget it compares
    /// against is [`Self::effective_compaction_budget`]'s, so the pass and
    /// the step manifest can never disagree about which number was used.
    ///
    /// Returns the summarizer's spend (0.0 on the overwhelmingly common
    /// no-summarization path) so `run_turn` folds it into the turn total.
    async fn run_compaction_pass(
        &self,
        messages: &mut Vec<CompletionMessage>,
        calibration_model: Option<&str>,
        budget: &mut BudgetGuard,
        health: &mut SummarizerHealth,
        step: usize,
        events: &EventSender,
    ) -> CompactionPass {
        let (compaction_budget, factor) = self.effective_compaction_budget(calibration_model);
        // Whether anything was rewritten IN PLACE, decided at the mutation sites
        // below and reported through a `#[must_use]` return.
        let mut rewrote = false;
        // Post-pass size, for the overflow decision below. `compact_measured`
        // returns the count it already computed on every path — including both
        // `None` paths, the common case — so this walks the transcript once per
        // step rather than eagerly re-deriving a number the callee just discarded.
        let retention = self
            .config
            .tool_result_horizon_steps
            .map(|keep_recent_steps| crate::compaction::RetentionPolicy { keep_recent_steps });
        let (after_tokens, report) = compact_measured(messages, compaction_budget, retention);
        if let Some(report) = report {
            // `Some` means a pass actually stubbed, aged or superseded something,
            // so positions at or after the first rewrite name different bytes now.
            rewrote = true;
            let _ = events.send(AgentEvent::Compaction {
                before_tokens: report.before_tokens,
                after_tokens,
                evicted: report.evicted,
                deduped: report.deduped,
                superseded: report.superseded,
                aged: report.aged,
                summarized: 0,
                // Identities, not just counts (spec §6.2): which blocks each
                // pass stubbed, and the budget the decision actually used.
                evicted_blocks: report.evicted_blocks,
                deduped_blocks: report.deduped_blocks,
                superseded_blocks: report.superseded_blocks,
                aged_blocks: report.aged_blocks,
                // The pure passes never summarize — the overflow fallback below
                // owns that path and its block identities.
                summarized_blocks: Vec::new(),
                effective_budget_tokens: compaction_budget,
                calibration_factor: factor,
            });
        }
        // Overflow fallback: still over budget after every pure pass means
        // the weight is in PROTECTED content (user/assistant text, the
        // latest tool result) — without this, the next provider call
        // eventually hard-fails on context overflow.
        if self.config.summarize_overflow && after_tokens > compaction_budget {
            // The summarizer splices a span down to one message, shifting every
            // position after it. Reported unconditionally when it RUNS rather than
            // only when it splices: over-invalidating costs one turn's memo on a
            // rare path, under-invalidating serves a digest for bytes that moved.
            // That asymmetry is why this is a revision counter, not a heuristic.
            let cost_usd = self
                .summarize_overflow_span(
                    messages,
                    budget,
                    compaction_budget,
                    factor,
                    health,
                    step,
                    events,
                )
                .await;
            return CompactionPass {
                cost_usd,
                rewrote: true,
            };
        }
        CompactionPass {
            cost_usd: 0.0,
            rewrote,
        }
    }

    /// Replace the oldest viable span with a model-written summary. Failures
    /// leave the conversation untouched. Returns the summarizer call's spend.
    /// `step` is carried only to key this call's receipt against the step it
    /// rides, the same way [`Self::apply_overflow_summary`] carries the budget
    /// pair it reports.
    #[allow(clippy::too_many_arguments)]
    async fn summarize_overflow_span(
        &self,
        messages: &mut Vec<CompletionMessage>,
        budget: &mut BudgetGuard,
        compaction_budget: u64,
        factor: f64,
        health: &mut SummarizerHealth,
        step: usize,
        events: &EventSender,
    ) -> f64 {
        // Span start: after the system prompt and the FIRST user message —
        // the task statement anchors every later step and must survive
        // verbatim. A Tool message can't open the kept tail either side of
        // the span (its assistant partner would be summarized away and the
        // provider rejects orphaned tool results), so both bounds walk off
        // Tool messages.
        let first_user = messages
            .iter()
            .position(|m| m.role == MessageRole::User)
            .unwrap_or(0);
        let mut start = first_user + 1;
        while start < messages.len() && messages[start].role == MessageRole::Tool {
            start += 1;
        }
        let mut end = messages
            .len()
            .saturating_sub(self.config.summarize_keep_recent);
        // `end == messages.len()` (keep_recent 0) has no kept tail to walk
        // off — indexing it would be out of bounds, not a Tool message.
        while end > start && end < messages.len() && messages[end].role == MessageRole::Tool {
            end -= 1;
        }
        // A tiny span isn't worth a model call — and this guard is also the
        // convergence backstop: once a summary message occupies the span,
        // the next over-budget step finds nothing left to replace and skips
        // instead of summarizing its own summary every step.
        if end <= start || end - start < 4 {
            return 0.0;
        }
        // Give-up latch: once the summarizer has failed this many steps in a
        // row this turn, stop re-firing it — each attempt is a completion and
        // its latency. The next model call overflows and surfaces one clear
        // failure instead of one per remaining step.
        if health.is_latched() {
            return 0.0;
        }
        // After the guards, not before: a latched or tiny-span step returns
        // above without needing this number, and the walk is Θ(transcript)
        // on precisely the steps whose transcript is at its largest.
        let before_tokens = crate::estimator::estimate_conversation_tokens(messages);
        let rendered = crate::summarize::render_span_for_summary(&messages[start..end]);
        let request = CompletionRequest {
            messages: vec![
                CompletionMessage::system(crate::summarize::SUMMARIZE_SYSTEM),
                CompletionMessage::user(&rendered),
            ],
            max_output_tokens: Some(1_200),
            temperature: Some(0.0),
            effort: Some(ReasoningEffort::Low),
            tools: vec![],
            reasoning: None,
            params: None,
        };
        let estimated_input_tokens = estimate_conversation_tokens(&request.messages);
        let result = match run_accounted_call(
            AccountedCall {
                provider: self.provider,
                role: stella_protocol::ModelCallRole::Summarization,
                model_hint: "unknown".into(),
                request,
                // The last line of defense before a terminal context
                // overflow — a transient blip here must be retried, not
                // fast-failed into an oversized, doomed next call.
                retry_policy: RetryPolicy::standard(),
                // The same generation deadline every worker call runs under.
                // `None` left a wedged summarizer provider parking the whole
                // turn indefinitely — the one un-bounded await on the step
                // path — and a trip lands in the Timeout arm below, which
                // counts toward the give-up latch like any other failure.
                timeout: self.config.model_timeout,
                estimated_input_tokens,
                // The summarizer rewrites the conversation it is called on, so
                // its own input is the only record of what it was given to
                // compress — and the span it replaces is gone from the history
                // afterwards. Reserved seat 1 at this step; compaction runs at
                // most once per step, so it collides with nothing.
                receipt: Some(crate::ReceiptContext {
                    turn_instance: self.config.turn_instance,
                    step,
                    call_seq: crate::receipts::RECEIPT_SEQ_SUMMARIZER,
                    lifecycle_enabled: self.config.lifecycle_enabled,
                }),
            },
            budget,
            events,
            self.sleeper,
        )
        .await
        {
            Ok(result) => result,
            // The summary was generated and paid for before the budget
            // tripped. Splicing it in only shrinks the context the resumed
            // session reloads, so apply it rather than discard the spend.
            Err(AccountedCallError::Budget { result, .. }) => {
                let text = result.text.trim();
                if !text.is_empty() {
                    self.apply_overflow_summary(
                        messages,
                        start,
                        end,
                        before_tokens,
                        text,
                        compaction_budget,
                        factor,
                        events,
                    );
                }
                return result.cost_usd;
            }
            // A silent 0.0 hid a failing summarizer that re-fired every step
            // until the provider hard-failed. Surface it and count it toward
            // the give-up latch.
            Err(AccountedCallError::Provider(e)) => {
                health.record_failure();
                let _ = events.send(AgentEvent::Error {
                    message: format!("overflow summarizer failed ({e}); context left intact"),
                    retryable: true,
                });
                return 0.0;
            }
            Err(AccountedCallError::Timeout) => {
                health.record_failure();
                let _ = events.send(AgentEvent::Error {
                    message: "overflow summarizer timed out; context left intact".to_string(),
                    retryable: true,
                });
                return 0.0;
            }
        };
        let cost_usd = result.cost_usd;
        let text = result.text.trim();
        if text.is_empty() {
            // An empty summary shrinks nothing, so the next step re-fires on
            // the same span — the same non-progress the error paths latch
            // against, and just as invisible if left unannounced.
            health.record_failure();
            let _ = events.send(AgentEvent::Error {
                message: "overflow summarizer returned an empty summary; context left intact"
                    .to_string(),
                retryable: true,
            });
            return cost_usd;
        }
        health.reset();
        self.apply_overflow_summary(
            messages,
            start,
            end,
            before_tokens,
            text,
            compaction_budget,
            factor,
            events,
        );
        cost_usd
    }

    /// Splice `text` (trimmed, non-empty) over `messages[start..end]` as the
    /// overflow summary and announce the resulting shrink. Shared by the
    /// normal path and the budget-abort path: a paid summary is applied even
    /// when the turn is about to abort, since it only shrinks the context the
    /// resumed session reloads.
    #[allow(clippy::too_many_arguments)]
    fn apply_overflow_summary(
        &self,
        messages: &mut Vec<CompletionMessage>,
        start: usize,
        end: usize,
        before_tokens: u64,
        text: &str,
        compaction_budget: u64,
        factor: f64,
        events: &EventSender,
    ) {
        let replaced = end - start;
        // Name which blocks left context (spec §6.2): the identity-bearing
        // tool-result blocks in the span the summary folds away, keyed the same
        // way the pure passes key theirs (`receipts::tool_result_block_id`), so
        // a receipt consumer can reconcile the summary splice against the
        // manifest. Captured before the splice mutates `messages`. Text
        // messages in the span carry no block identity, so this is a subset of
        // the `summarized` count, not a one-for-one list.
        let summarized_blocks: Vec<String> = messages[start..end]
            .iter()
            .flat_map(|m| m.tool_results.iter())
            .map(|r| crate::receipts::tool_result_block_id(&r.output))
            .collect();
        let summary = CompletionMessage::user(format!(
            "{SUMMARY_MARKER_PREFIX} to fit context — full detail was compacted away; \
             re-read files or re-run tools for specifics]\n\n{text}"
        ));
        messages.splice(start..end, std::iter::once(summary));
        let _ = events.send(AgentEvent::Compaction {
            before_tokens,
            after_tokens: crate::estimator::estimate_conversation_tokens(messages),
            evicted: 0,
            deduped: 0,
            superseded: 0,
            aged: 0,
            summarized: replaced,
            // The summary path runs none of the pure passes, so their block
            // vectors stay empty; the folded blocks are named in
            // `summarized_blocks`. It targets the same effective budget.
            evicted_blocks: Vec::new(),
            deduped_blocks: Vec::new(),
            superseded_blocks: Vec::new(),
            aged_blocks: Vec::new(),
            summarized_blocks,
            effective_budget_tokens: compaction_budget,
            calibration_factor: factor,
        });
    }

    /// Loop detection, before spending a model call on a step that's
    /// already stuck. Progress-aware: each call is paired with the output
    /// it produced ([`recent_call_records`]), so only repeats and cycles
    /// with byte-identical outputs count — legitimate polling (identical
    /// input, changing output) never trips it (`crate::loop_detect`).
    /// `result_identities` is the turn's pre-compaction snapshot
    /// ([`snapshot_result_identities`]) — compaction runs immediately
    /// before this check and rewrites older results in place, so without it
    /// a repeat streak reads as `[stub, stub, real]` and never counts.
    ///
    /// Steer first, abort second: the FIRST detection of a turn injects a
    /// warning through the same seam as user steering — a user-role
    /// message the model answers this very step, plus its `Steered`
    /// transcript event — and lets the turn continue. Only a detection
    /// after that warning is the turn's clean abort (`Some`): the model
    /// was told, kept looping anyway, and more steps would be pure waste.
    fn check_loop_detection(
        &self,
        messages: &mut Vec<CompletionMessage>,
        result_identities: &ResultIdentities,
        loop_steered: &mut bool,
        total_cost_usd: f64,
        events: &EventSender,
    ) -> Option<TurnOutcome> {
        let records = recent_call_records(messages, result_identities);
        let verdict = detect_loop(&records, self.config.loop_detection);
        // The typed twin of the prose steer/abort (receipts spec §6.3): a
        // receipt parses this instead of string-matching "stuck-loop
        // detected:" prefixes. Emitted for both outcomes — `aborted` says
        // whether this detection steered or killed the turn. Matching the
        // verdict is also the healthy-turn early return, so there is exactly
        // one place that decides "is this a loop".
        let (kind, pattern, repeats) = match &verdict {
            LoopVerdict::NoLoop => return None,
            LoopVerdict::ExactRepeat { tool, count, .. } => {
                ("exact_repeat", vec![tool.clone()], *count)
            }
            LoopVerdict::ShortCycle { pattern, repeats } => (
                "short_cycle",
                pattern.iter().map(|c| c.name.clone()).collect(),
                *repeats,
            ),
        };
        let reason = verdict
            .evidence()
            .unwrap_or_else(|| "loop detected".to_string());
        let _ = events.send(AgentEvent::LoopDetected {
            turn_instance: self.config.turn_instance,
            kind: kind.to_string(),
            pattern,
            repeats,
            evidence: reason.clone(),
            aborted: *loop_steered,
        });
        if !*loop_steered {
            *loop_steered = true;
            let text = format!(
                "{LOOP_STEER_PREFIX}] you appear to be looping: {reason}. Repeating the \
                 same call cannot produce new information — change strategy: vary the \
                 arguments, try a different tool, or report what is blocking you. If you \
                 keep looping, the turn will be aborted."
            );
            let _ = events.send(AgentEvent::Steered { text: text.clone() });
            messages.push(CompletionMessage::user(text));
            return None;
        }
        let reason = format!("stuck-loop detected (persisted after a steering warning): {reason}");
        let _ = events.send(AgentEvent::Error {
            message: reason.clone(),
            retryable: false,
        });
        Some(TurnOutcome::Aborted {
            reason,
            cost_usd: total_cost_usd,
        })
    }

    /// Whether any configured `PreToolUse`/`PostToolUse` hook matches
    /// `name`. Such a tool is never speculated: its hooks must fire exactly
    /// once, on the committed dispatch path (`PreToolUse` gates *before*
    /// execution) — a speculative attempt that later fails would otherwise
    /// fire phantom hook side effects for a call that never reached the
    /// transcript (#370). Matching goes through `hooks::any_matcher_matches`,
    /// which shares its predicate with the `select_matchers` the hook runner
    /// itself uses, so the two can never disagree — and answers without
    /// allocating the matcher `Vec` this only measured for emptiness (#560).
    /// `false` whenever hooks are off, keeping hook-free turns byte-identical.
    fn tool_has_matching_hook(&self, name: &str) -> bool {
        let Some(handle) = self.hooks else {
            return false;
        };
        [HookEvent::PreToolUse, HookEvent::PostToolUse]
            .into_iter()
            .any(|event| any_matcher_matches(event, handle.hooks.matchers_for(event), Some(name)))
    }

    /// One model call with retry+backoff (`crate::retry`). On commit,
    /// flushes the step's deferred `Retry` events (module docs, L-E10) and
    /// returns the result bundled with the request-time snapshots the
    /// later phases consume; on exhausted retries, emits the terminal
    /// error and returns the turn's clean abort.
    ///
    /// The estimate captured here is the raw (uncalibrated) estimate of
    /// exactly what this step sends — recorded against the provider's
    /// reported usage by [`Engine::handle_committed_result`]. Raw, not
    /// calibrated: the drift ratio is actual/raw, and recording a
    /// corrected estimate would compound corrections on every feedback
    /// pass.
    async fn run_model_call(
        &self,
        step: usize,
        messages: &[CompletionMessage],
        budget: &mut BudgetGuard,
        receipts: &mut ReceiptLedger,
        warnings: &mut BudgetWarnings,
        events: &EventSender,
    ) -> Result<CommittedStep, String> {
        let tools_schema = self.tools.schemas();
        let read_only_tools: HashSet<String> = tools_schema
            .iter()
            .filter(|s| s.read_only)
            .map(|s| s.name.clone())
            .collect();
        // Speculation eligibility is the CONJUNCTION of the two schema
        // claims: `read_only` (mutates no workspace state) and
        // `speculation_safe` (tolerates the duplicate run a stream retry
        // causes — #923). The gate still needs the full read-only set for
        // its mutation fence, so both sets are carried, one never
        // subtracted from the other. A tool claiming `speculation_safe`
        // without `read_only` is ignored here — a mutating call can never
        // run before its step commits, whatever its author believes about
        // idempotence.
        let speculation_safe_tools: HashSet<String> = tools_schema
            .iter()
            .filter(|s| s.read_only && s.speculation_safe)
            .map(|s| s.name.clone())
            .collect();
        // Speculatable tools a configured hook matches are excluded as well
        // (they run only on the committed dispatch path so their hooks fire
        // exactly once — #370). Built from the eligibility set, not the
        // read-only set: hook-gating only ever needs to veto a call that
        // would otherwise be speculated. Empty whenever hooks are off.
        let hook_gated: HashSet<String> = speculation_safe_tools
            .iter()
            .filter(|name| self.tool_has_matching_hook(name))
            .cloned()
            .collect();
        // The drift-sample estimate: text only, attachments excluded. The
        // attachment weight is a deliberate ~80× over-estimate of billed
        // media tokens (right for context pressure, poison for calibration —
        // see `estimate_conversation_attachment_tokens`), and this value is
        // what StepUsage persists as the drift sample and what
        // `handle_committed_result` records. The compaction decision keeps
        // the full attachment-weighted estimate via its own walk in
        // `run_compaction_pass`.
        let estimated_input_tokens = estimate_conversation_tokens(messages).saturating_sub(
            crate::estimator::estimate_conversation_attachment_tokens(messages),
        );
        let req_config = &self.config;
        // Built ONCE, outside the attempt closure below, and borrowed from
        // there. `CompletionRequestRef` is `Copy` (slices + scalars), so each
        // attempt takes its own view for free instead of deep-copying the whole
        // transcript and every tool schema — the per-attempt cost the `FnMut`
        // bound used to force on an owning request (#921).
        let req = CompletionRequestRef {
            messages,
            max_output_tokens: req_config.max_output_tokens,
            temperature: req_config.temperature,
            effort: req_config.effort,
            reasoning: req_config.reasoning,
            params: req_config.params,
            tools: &tools_schema,
        };
        let speculation_read_only = read_only_tools.clone();
        let speculation_safe = speculation_safe_tools;
        let speculation_hook_gated = hook_gated;
        // The gate forwards answer fragments as `TextDelta` previews. Deliberately NOT rolled back
        // on a failed attempt: a retry's deltas re-stream from the start
        // with no reset marker — the eventual `Text` event is authoritative
        // and consumers replace the preview with it (protocol docs).
        let delta_events = events.clone();
        // What separates a wedged provider from a slow one, for the model
        // deadline. The gate itself ticks this clock from EVERY observer
        // method — text, reasoning, streamed calls, argument fragments — so
        // a generation whose whole output is tool calls still registers as
        // producing (a sender-tap here only saw text/reasoning events and
        // read call-only streams as total silence). Per-attempt, like the
        // pool it sits beside — a retry starts its own idle clock rather
        // than inheriting the previous attempt's silence.
        let stream_progress = StreamProgress::default();
        let attempt_progress = stream_progress.clone();
        // The pump reports its discarded read-only work (a failed attempt's
        // pool, or a hard cancel mid-drain) as `SpeculationDiscarded` events
        // so I/O that actually ran is never silently lost (#370).
        let pump_events = events.clone();
        // Each attempt runs the provider call and the speculation pump
        // concurrently: the pump executes read-only calls the moment the
        // adapter announces them (`crate::speculation`), so their wall-clock
        // overlaps the stream instead of following it. The gate (and with
        // it the channel's send half) drops when the provider call resolves,
        // which is what lets the pump finish draining. A failed attempt
        // drops its pool with the attempt — safe to waste for the workspace,
        // but each completed entry emits a `SpeculationDiscarded` event on
        // the way out (#370) — and the retry builds a fresh channel and pool.
        // Stored true exactly around each attempt's dispatch, so the drop
        // guard below can tell "a paid call may be in flight" apart from "the
        // ladder is asleep between attempts" — the failed attempt before a
        // sleep already reported its own envelope through the observer.
        let attempt_in_flight = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let attempt_in_flight_latch = attempt_in_flight.clone();
        let attempt: RetryAttemptFn = Box::new(move || {
            let read_only = speculation_read_only.clone();
            let safe = speculation_safe.clone();
            let gated = speculation_hook_gated.clone();
            let progress = attempt_progress.clone();
            let gate_progress = progress.clone();
            let in_flight = attempt_in_flight_latch.clone();
            let delta_tx = delta_events.clone();
            let pump_tx = pump_events.clone();
            Box::pin(async move {
                in_flight.store(true, std::sync::atomic::Ordering::SeqCst);
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                let mut pump: SpeculationFuture<'_> = Box::pin(self.pump_speculations(rx, pump_tx));
                let mut complete = Box::pin(async move {
                    let gate =
                        SpeculationGate::new(read_only, safe, gated, tx, delta_tx, gate_progress);
                    self.provider.complete_observed_ref(req, &gate).await
                    // `gate` (and its sender) drop here → the pump's
                    // stream ends once in-flight executions drain.
                });
                let result = tokio::select! {
                    // `biased` makes the invariant below structural rather
                    // than incidental: `complete` owns the gate (and with
                    // it the channel's send half), so the pump can only
                    // finish after `complete` has. Polling `complete`
                    // first means the pump arm cannot be taken by an
                    // unlucky randomized poll order (#560).
                    //
                    // That arm is believed unreachable, but it reports a
                    // typed terminal error rather than panicking: this is
                    // library code on a provider-driven path, where
                    // invariant 5 (no panics on runtime data) outranks
                    // asserting a structural claim (#618 item 17).
                    biased;
                    result = bounded_generation(self.config.model_timeout, &progress, &mut complete) => result,
                    _ = &mut pump => Err(ProviderError::Terminal(
                        "speculation pump ended before the model call that feeds it; \
                         the speculation gate holds the channel open for the whole call, \
                         so this indicates the gate was dropped early"
                            .into(),
                    )),
                };
                in_flight.store(false, std::sync::atomic::Ordering::SeqCst);
                drop(complete);
                result.map(|result| (result, pump))
            })
        });

        let call_started = std::time::Instant::now();
        // Armed for exactly the interval where a paid attempt may be in
        // flight: a caller-side hard cancel that drops this future mid-await
        // still leaves one content-free `Cancelled` envelope behind.
        // Disarmed on BOTH normal exits — a success reports through its
        // `StepUsage`, and a terminal failure's attempts already reported
        // through the per-attempt observer below. The shared latch narrows
        // "in flight" to the dispatch itself: a drop landing in a backoff
        // sleep emits nothing, because the failed attempt before the sleep
        // already reported its own envelope.
        let mut cancel_guard = CancelUsageGuard {
            events: events.clone(),
            role: self.call_role,
            provider: self.provider.id().to_string(),
            started: call_started,
            armed: true,
            attempt_in_flight,
        };
        let incomplete_events = events.clone();
        // Every failed attempt's reason, accumulated through the observer:
        // retry.rs returns retry history only for calls that COMMIT, so on
        // exhaustion this is the sole record of the doomed attempts
        // (receipts spec §6.3 — RetriesExhausted). A std Mutex, never held
        // across an await and contention-free — the observer runs serially
        // within this call.
        let attempt_reasons: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let RetryOutcome {
            value: (result, speculation_future),
            retries,
            ..
        } = match retry_with_backoff_observed(
            &self.config.retry_policy,
            self.sleeper,
            attempt,
            // Per-attempt duration (retry.rs times each dispatch
            // individually): the failed call's own latency, never
            // cumulative across earlier attempts or backoff sleeps.
            |attempt, error, attempt_duration| {
                attempt_reasons
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(error.to_string());
                let _ = incomplete_events.send(AgentEvent::UsageIncomplete {
                    role: self.call_role,
                    provider: self.provider.id().to_string(),
                    model: "unknown".into(),
                    reason: stella_protocol::UsageIncompleteReason::ProviderError,
                    duration_ms: attempt_duration.as_millis() as u64,
                    retries: Some(attempt.saturating_sub(1)),
                });
            },
        )
        .await
        {
            Ok(outcome) => {
                cancel_guard.disarm();
                outcome
            }
            Err(error) => {
                cancel_guard.disarm();
                let reasons =
                    std::mem::take(&mut *attempt_reasons.lock().unwrap_or_else(|p| p.into_inner()));
                // Shared with the paired `Error` event below: whether the
                // FINAL attempt's error is of a retryable class. `false`
                // means every attempt (typically just one — see
                // `retry_with_backoff_observed`, which bails on the first
                // non-retryable error) was doomed from the start, not
                // exhausted by an actual retry loop (#926).
                let retryable = error.is_retryable();
                let _ = events.send(AgentEvent::RetriesExhausted {
                    turn_instance: self.config.turn_instance,
                    attempts: reasons.len() as u32,
                    reasons,
                    retryable,
                });
                let message = error.to_string();
                let _ = events.send(AgentEvent::Error {
                    message: message.clone(),
                    retryable,
                });
                return Err(format!("model call failed: {message}"));
            }
        };
        let budget_outcome = record_settled_cost(budget, result.cost_usd, warnings, events);
        let call_duration_ms = call_started.elapsed().as_millis() as u64;

        // Deferred-flush: these `Retry` events only reach the wire now
        // that the step has actually committed (see module docs).
        for attempt in &retries {
            let _ = events.send(AgentEvent::Retry {
                attempt: attempt.attempt,
                reason: attempt.reason.clone(),
            });
        }

        // The context receipt for this step: register any newly-seen blocks,
        // then the ordered manifest of exactly what was sent. Emitted just
        // before StepUsage so the pair — what the model saw, what it cost —
        // lands together at the settled boundary, and the served model/provider
        // are already known.
        receipts.emit_step_receipt(
            messages,
            // The same estimate `StepUsage` reports below, not a second walk.
            estimated_input_tokens,
            step,
            crate::receipts::ServedBy {
                role: self.call_role,
                provider: self.provider.id(),
                model: &result.model,
            },
            events,
        );

        // Cost and usage settle at one no-await boundary. Speculative tool
        // work may still be draining; cancellation in that interval must not
        // preserve spend while losing its per-call accounting envelope.
        let _ = events.send(AgentEvent::StepUsage {
            step,
            role: self.call_role,
            provider: self.provider.id().to_string(),
            // The engine's own step already streams its answer as a `Text`
            // event; duplicating it here would double the transcript.
            output_text: None,
            model: result.model.clone(),
            input_tokens: result.usage.input_tokens,
            output_tokens: result.usage.output_tokens,
            cached_input_tokens: result.usage.cached_input_tokens,
            cache_write_tokens: result.usage.cache_write_tokens,
            estimated_input_tokens,
            cost_usd: result.cost_usd,
            duration_ms: call_duration_ms,
            retries: retries.len() as u32,
            tool_calls: result.tool_calls.len(),
            complete: result.usage.is_complete(),
            // The provider's own stop reason, forwarded rather than inferred:
            // `Length` here is the only truthful "this step hit the output
            // ceiling" signal any consumer gets.
            finish_reason: result.finish_reason,
        });
        let speculation = speculation_future.await;

        Ok(CommittedStep {
            result,
            budget_outcome,
            read_only_tools,
            speculation,
            estimated_input_tokens,
        })
    }

    /// Receive announced calls from the [`SpeculationGate`] and execute them
    /// concurrently (same cap as dispatch) while the model call streams,
    /// collecting outputs into the attempt's [`SpeculationPool`]. Runs until
    /// the gate drops the send half AND every in-flight execution finishes —
    /// speculated calls are exactly the calls dispatch would run first, so
    /// draining them is never wasted time on the committed path.
    ///
    /// Completed executions accumulate inside a [`SpeculationDropGuard`]: on
    /// the committed path the pump runs to completion and hands the pool to
    /// dispatch (disarming the guard), but if this future is dropped first —
    /// its stream attempt failed, or the turn was hard-cancelled mid-drain —
    /// the guard emits one `SpeculationDiscarded` per already-completed entry
    /// so the read-only I/O that ran is never lost from the event log (#370).
    async fn pump_speculations(
        &self,
        mut rx: tokio::sync::mpsc::UnboundedReceiver<ToolCall>,
        events: EventSender,
    ) -> SpeculationPool {
        let announced = futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx));
        let mut in_flight = announced
            .map(|call| async move {
                let started = std::time::Instant::now();
                let output = self.execute_with_repair(&call, None).await;
                (call, output, started.elapsed().as_millis() as u64)
            })
            .buffer_unordered(MAX_CONCURRENT_TOOL_CALLS);

        let mut guard = SpeculationDropGuard {
            events,
            pool: SpeculationPool::new(),
            armed: true,
        };
        while let Some((call, output, duration_ms)) = in_flight.next().await {
            let call_id = call.call_id.clone();
            let displaced = guard.pool.insert(
                call_id.clone(),
                SpeculativeResult {
                    name: call.name,
                    input: call.input,
                    output,
                    duration_ms,
                },
            );
            // The pool is keyed by `call_id`, and a call_id is only unique
            // within ONE response — `loop_evidence::CallIdentityKey` names the
            // adapters that recycle them. A second announcement under the same
            // id evicts the first, whose tool already ran real I/O and can
            // never be harvested or discarded at dispatch. Account for it here
            // rather than dropping it silently (#370).
            if let Some(displaced) = displaced {
                let _ = guard.events.send(AgentEvent::SpeculationDiscarded {
                    call_id,
                    name: displaced.name,
                    reason: SPECULATION_DISCARD_HARVEST_MISMATCH.to_string(),
                });
            }
        }
        guard.harvest()
    }

    /// Emit `SpeculationDiscarded` for every entry a committed step's pool
    /// left un-harvested — read-only calls that ran real I/O but never made it
    /// into the committed transcript (#370). `reason` names why the pool was
    /// dropped (harvest mismatch on the normal path, budget abort on the
    /// enforced-limit unwind) so the accounting stays reconcilable per site.
    fn discard_speculation_pool(&self, pool: SpeculationPool, reason: &str, events: &EventSender) {
        for (call_id, result) in pool {
            let _ = events.send(AgentEvent::SpeculationDiscarded {
                call_id,
                name: result.name,
                reason: reason.to_string(),
            });
        }
    }

    /// Bookkeeping for the call that just committed: drift feedback into
    /// the attached calibration. Its cost was settled — and its single
    /// `StepUsage` metering record emitted — synchronously at the
    /// provider-success boundary in [`Engine::run_model_call`], before this
    /// method can be reached; the carried outcome decides whether `Some` is
    /// the turn's clean abort. That abort is issued only after delivering
    /// what was already paid for (see body), never as a mid-tool kill.
    fn handle_committed_result(
        &self,
        committed: &CommittedStep,
        total_cost_usd: f64,
        messages: &mut Vec<CompletionMessage>,
        warnings: &mut BudgetWarnings,
        events: &EventSender,
    ) -> Option<TurnOutcome> {
        let result = &committed.result;

        // Drift feedback: the provider's reported input tokens PLUS its
        // cache-write tokens against the raw estimate, keyed by the model
        // that actually served the call. Cache writes are real prompt tokens
        // the provider read — adapters split them out of `input_tokens` for
        // pricing, not because they were not sent — and omitting them fed a
        // falsely low actual on every cache-writing step (worst on a
        // cache-enabled session's first call, where nearly the whole prompt
        // is a cache write). `record` ignores zero-sided pairs, so a
        // provider omitting usage never poisons the state.
        if let Some(calibration) = self.calibration {
            calibration.record(
                &result.model,
                committed.estimated_input_tokens,
                result
                    .usage
                    .input_tokens
                    .saturating_add(result.usage.cache_write_tokens),
            );
        }

        // Observed-mode breaches were already surfaced when the cost settled
        // (`record_settled_cost`); the per-turn ledger dedups, so this is a
        // no-op after the first breach — present so every Warn observer on
        // the path honors the contract. Only `enforced` reaches the abort.
        emit_budget_warning(committed.budget_outcome, warnings, events);
        let BudgetOutcome::AbortTurn {
            axis,
            spent_usd,
            limit_usd,
        } = committed.budget_outcome
        else {
            return None;
        };

        // The call that just landed is the one that pushed spend over the
        // limit — it already committed (its result is real, its cost
        // already happened), so deliver what was paid for: emit its text
        // and append it to history, THEN abort before dispatching
        // anything further (its tool calls, if any, never run — recorded
        // so the transcript shows what was cut). Still not a mid-tool
        // kill. Trimmed guard: whitespace-only text is not a deliverable
        // answer and must not stream a blank `Text` event.
        if !result.text.trim().is_empty() {
            let _ = events.send(AgentEvent::Text {
                delta: result.text.clone(),
            });
        }
        messages.push(CompletionMessage {
            role: MessageRole::Assistant,
            // The elision discipline holds on the abort path too: a resumed
            // session reloads this vec, and a truncated partial retained
            // whole here is re-sent on its every later step.
            content: if result.finish_reason == Some(FinishReason::Length) {
                truncation::retained_partial(&result.text)
            } else {
                result.text.clone()
            },
            tool_calls: result.tool_calls.clone(),
            tool_results: Vec::new(),
            attachments: Vec::new(),
        });
        // The assistant message above may carry `tool_calls` that never
        // ran (we abort before dispatching them). A recorded `tool_use`
        // with no matching `tool_result` is a broken history: when a
        // REPL caller reuses this `messages` vec, the next turn's first
        // provider call is hard-rejected ("tool_use must be followed by
        // tool_result"). Close the pairing with a synthetic error result
        // per un-run call so resumption stays valid — through the same
        // helper the cancellation path uses, since a second copy of this
        // repair is a second place for it to rot (the results are likewise
        // mirrored onto the event stream, with no `ToolStart`, because this
        // step's `StepUsage` already reported `tool_calls: N`).
        crate::step::close_open_tool_calls(messages, BUDGET_ABORT_TOOL_RESULT, events);
        // The typed twin of the prose denial (receipts spec §6.3). Mode is
        // `Enforced` by construction — only enforced budgets abort.
        let _ = events.send(AgentEvent::BudgetDenied {
            scope: match axis {
                BudgetAxis::Turn => stella_protocol::BudgetScope::Turn,
                BudgetAxis::Session => stella_protocol::BudgetScope::Session,
            },
            spent_usd,
            limit_usd,
            mode: stella_protocol::BudgetMode::Enforced,
        });
        let axis = match axis {
            BudgetAxis::Turn => "turn",
            BudgetAxis::Session => "session",
        };
        let reason = format!(
            "budget exceeded after this call: spent ${spent_usd:.4} against a ${limit_usd:.2} \
             {axis} limit"
        );
        let _ = events.send(AgentEvent::Error {
            message: reason.clone(),
            retryable: false,
        });
        Some(TurnOutcome::Aborted {
            reason,
            cost_usd: total_cost_usd,
        })
    }

    /// Deliver a committed step's result: emit its text, then either
    /// finish the turn (no tool calls — `Some(Completed)`) or record the
    /// assistant message, execute its tool calls, record their results,
    /// and return `None` so the loop takes another step. Consumes the
    /// step: the result's text moves into the `Completed` outcome.
    ///
    /// A tool-less step that ended at the output-token limit is the one
    /// no-tool shape that does NOT finish the turn (up to
    /// [`MAX_LENGTH_CONTINUATIONS`] times): the model was cut off, not done,
    /// so the step is recorded and the turn continues with
    /// [`LENGTH_CONTINUATION_NUDGE`]. `length_continuations` is the turn's
    /// running count ([`TurnState`]'s, threaded rather than owned so the
    /// bound survives across steps).
    async fn dispatch_completion(
        &self,
        committed: CommittedStep,
        total_cost_usd: f64,
        messages: &mut Vec<CompletionMessage>,
        length_continuations: &mut u32,
        continuation_budget: Option<ContinuationBudget>,
        events: &EventSender,
    ) -> Option<TurnOutcome> {
        let CommittedStep {
            result,
            read_only_tools,
            speculation,
            ..
        } = committed;

        // Trimmed guard, matching the empty-turn check below: a
        // whitespace-only response must not stream a blank `Text` event and
        // then abort as "no text" — events and history stay consistent.
        if !result.text.trim().is_empty() {
            let _ = events.send(AgentEvent::Text {
                delta: result.text.clone(),
            });
        }

        if result.tool_calls.is_empty() {
            // A committed step that runs no tools can still have speculated
            // read-only calls off a divergent stream (announced, then dropped
            // from the commit): none are harvested here, so account for the
            // discarded I/O rather than dropping the pool silently (#370).
            self.discard_speculation_pool(
                speculation,
                SPECULATION_DISCARD_HARVEST_MISMATCH,
                events,
            );
            // A step that ended at the output-token limit with no tool call is
            // not a finished turn — it ran out of room. `plan_continuation`
            // owns what happens next and what history keeps (see
            // `driver::truncation`); `None` means the turn's allowance is
            // spent and the terminal handling below applies.
            let mut out_of_time = false;
            if result.finish_reason == Some(FinishReason::Length) {
                match plan_continuation(
                    &result.text,
                    result.usage.output_tokens,
                    *length_continuations,
                    continuation_budget,
                ) {
                    ContinuationPlan::Continue(plan) => {
                        *length_continuations += 1;
                        let (note, appended) = plan.into_parts();
                        let _ = events.send(AgentEvent::Text { delta: note });
                        messages.extend(appended);
                        return None;
                    }
                    // Declined because the turn is nearly out of wall clock,
                    // not because it ran out of tries. The abort below is the
                    // wrong ending for that: it exits nonzero, which a harness
                    // records as the agent dying — the exact outcome the
                    // deadline exists to avoid. Stopping early is only worth
                    // anything if stopping produces a result, so this path
                    // completes the turn with whatever it has.
                    ContinuationPlan::OutOfTime => out_of_time = true,
                    ContinuationPlan::AllowanceSpent => {}
                }
            }
            // A turn that produced neither a tool call NOR any visible text is
            // never a real completion: the model was cut off at its output
            // limit (usually mid-reasoning) or returned nothing at all.
            // Recording it as `Completed` shows the user a silent, blank turn
            // (the "turn ends with no feedback" defect). Surface why and abort
            // cleanly so the caller can retry instead of swallowing it.
            //
            // Except out of time, which is a deliberate stop rather than a
            // failure to produce anything: it substitutes a truthful stand-in
            // below and ends as an ordinary result.
            if result.text.trim().is_empty() && !out_of_time {
                let reason = match result.finish_reason {
                    // Advised `/compact` until #712; no such command exists,
                    // and compaction is automatic every step anyway.
                    Some(FinishReason::Length) => format!(
                        "The model reached its output-token limit ({} tokens) before producing \
                         any visible response — its budget was spent on reasoning — and did so \
                         on every one of its {MAX_LENGTH_CONTINUATIONS} continuations. Retry \
                         or raise the output cap; context compacts automatically each step.",
                        result.usage.output_tokens
                    ),
                    _ => format!(
                        "The model returned an empty response this turn — no text and no tool \
                         call ({} output tokens). Retry, or switch to a different model.",
                        result.usage.output_tokens
                    ),
                };
                let _ = events.send(AgentEvent::Error {
                    message: reason.clone(),
                    retryable: true,
                });
                return Some(TurnOutcome::Aborted {
                    reason,
                    cost_usd: total_cost_usd,
                });
            }
            // A non-empty answer truncated with the continuation allowance
            // spent: keep the partial answer (already emitted above) but tell
            // the user it was cut off, so a mid-thought stop is never
            // mistaken for a full one. The verification ladder's no-op rung
            // is what stops a turn that still did nothing from reporting
            // success past this point.
            if result.finish_reason == Some(FinishReason::Length) {
                let _ = events.send(AgentEvent::Text {
                    delta: if out_of_time {
                        // Names the deadline, not the token limit, because the
                        // token limit is what happened and the deadline is why
                        // nothing followed it. A reader who sees only "output
                        // limit" concludes the cap is mispriced and raises it,
                        // which is the one change that cannot help here.
                        format!(
                            "\n\n⚠ Stopped at the output-token limit ({} tokens) with too little \
                             time left in this turn to finish another continuation — ending here \
                             with what the turn has already done.",
                            result.usage.output_tokens
                        )
                    } else {
                        format!(
                            "\n\n⚠ Response was truncated at the output-token limit ({} tokens); \
                             ask to continue if it was cut off mid-thought.",
                            result.usage.output_tokens
                        )
                    },
                });
            }
            // Empty only on the out-of-time path — every other empty-text route
            // aborted above — where the transcript and the turn's final text
            // still owe an honest account of why nothing came back.
            let text = if result.text.trim().is_empty() {
                TIME_EXHAUSTED_PARTIAL.to_string()
            } else {
                result.text
            };
            // History keeps the elided form of a truncated partial; the
            // outcome below keeps it whole (`retained_partial`'s contract).
            let retained = if result.finish_reason == Some(FinishReason::Length) {
                truncation::retained_partial(&text)
            } else {
                text.clone()
            };
            messages.push(CompletionMessage {
                role: MessageRole::Assistant,
                content: retained,
                tool_calls: Vec::new(),
                tool_results: Vec::new(),
                attachments: Vec::new(),
            });
            let _ = events.send(AgentEvent::Stage {
                name: StageKind::Complete,
            });
            let _ = events.send(AgentEvent::Complete {
                model: result.model.clone(),
                cost_usd: total_cost_usd,
            });
            return Some(TurnOutcome::Completed {
                text,
                cost_usd: total_cost_usd,
            });
        }

        messages.push(CompletionMessage {
            role: MessageRole::Assistant,
            // A length-truncated step that still carried tool calls proceeds
            // normally — but its narration was cut off mid-stream, and
            // retained whole it is the same compounding debris as a tool-less
            // truncation (`retained_partial`). A natural stop keeps its text.
            content: if result.finish_reason == Some(FinishReason::Length) {
                truncation::retained_partial(&result.text)
            } else {
                result.text.clone()
            },
            tool_calls: result.tool_calls.clone(),
            tool_results: Vec::new(),
            attachments: Vec::new(),
        });

        let tool_results = self
            .execute_tool_calls(&result.tool_calls, &read_only_tools, speculation, events)
            .await;

        messages.push(CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results,
            attachments: Vec::new(),
        });

        None
    }

    /// Execute one step's tool calls, preserving sequential semantics for
    /// anything that can mutate: consecutive read-only calls (per
    /// `ToolSchema::read_only`) form a group executed concurrently (capped
    /// at [`MAX_CONCURRENT_TOOL_CALLS`]); every mutating call is its own
    /// barrier, executed alone, in call order. So `[read, read, edit,
    /// read]` runs the two reads in parallel, then the edit alone, then the
    /// final read — an observer of any *mutable* state cannot distinguish
    /// this schedule from fully-sequential execution, while the common
    /// "read five files" step gets real concurrency.
    ///
    /// `ToolStart` fires when a call actually starts; `ToolResult` fires as
    /// each call completes (so results from one parallel group may
    /// interleave — consumers correlate by `call_id`, which the TUI already
    /// does). The returned `Vec<ToolResult>` is always in original call
    /// order, so message history is deterministic regardless of completion
    /// order.
    ///
    /// `speculation` holds this step's speculatively-executed read-only
    /// calls (`crate::speculation`). A call is *harvested* — its recorded
    /// output delivered without re-executing — only when the pool entry
    /// matches the committed call exactly (id, name, AND input); any
    /// mismatch falls through to normal execution and the stale entry is
    /// discarded. Harvested calls emit `ToolStart` immediately followed by
    /// `ToolResult { speculated: true }` carrying the real (overlapped)
    /// execution duration.
    ///
    /// # A hard cancel here leaves `messages` mid-pair
    ///
    /// [`Self::dispatch_completion`] appends the assistant `tool_use` message
    /// BEFORE awaiting this, and the answering `Tool` message only after. A
    /// caller-side hard cancel (dropping the turn future) in that window
    /// therefore leaves an unpaired `tool_use` in the borrowed history — the
    /// same broken shape [`Self::handle_committed_result`] explicitly repairs
    /// on the budget-abort path, and the one the next provider call rejects
    /// outright. It is deliberately not repaired here: the contract is that a
    /// hard cancel truncates the whole turn out of history caller-side (see
    /// [`crate::ports::TurnSteering`], which contrasts exactly this against the
    /// soft stop). A caller that KEEPS a hard-cancelled turn's messages must
    /// close the pairing itself.
    async fn execute_tool_calls(
        &self,
        calls: &[ToolCall],
        read_only_tools: &HashSet<String>,
        mut speculation: SpeculationPool,
        events: &EventSender,
    ) -> Vec<ToolResult> {
        let mut indexed: Vec<(usize, ToolResult)> = Vec::with_capacity(calls.len());
        let mut i = 0;
        while i < calls.len() {
            let group_end = if read_only_tools.contains(&calls[i].name) {
                let mut end = i + 1;
                while end < calls.len() && read_only_tools.contains(&calls[end].name) {
                    end += 1;
                }
                end
            } else {
                i + 1
            };

            // Plain copy for the closures: borrowing the loop variable
            // itself would conflict with advancing it below (E0506).
            let group_start = i;
            let speculation = &mut speculation;
            let group_futures =
                calls[group_start..group_end]
                    .iter()
                    .enumerate()
                    .map(|(offset, call)| {
                        let _ = events.send(AgentEvent::ToolStart { call: call.clone() });
                        let index = group_start + offset;
                        let harvested = match speculation.remove(&call.call_id) {
                            Some(s) if s.name == call.name && s.input == call.input => Some(s),
                            Some(stale) => {
                                // The committed call diverged from what was
                                // announced: reject the pooled result and
                                // re-execute below. The speculative execution
                                // still ran real I/O — record it (#370).
                                let _ = events.send(AgentEvent::SpeculationDiscarded {
                                    call_id: call.call_id.clone(),
                                    name: stale.name,
                                    reason: SPECULATION_DISCARD_HARVEST_MISMATCH.to_string(),
                                });
                                None
                            }
                            None => None,
                        };
                        async move {
                            match harvested {
                                Some(s) => (index, call, s.output, s.duration_ms, true),
                                None => {
                                    let started = std::time::Instant::now();
                                    let output = self.execute_with_repair(call, Some(events)).await;
                                    let duration_ms = started.elapsed().as_millis() as u64;
                                    (index, call, output, duration_ms, false)
                                }
                            }
                        }
                    });
            let mut in_flight = futures_util::stream::iter(group_futures)
                .buffer_unordered(MAX_CONCURRENT_TOOL_CALLS);
            while let Some((index, call, output, duration_ms, speculated)) = in_flight.next().await
            {
                let _ = events.send(AgentEvent::ToolResult {
                    call_id: call.call_id.clone(),
                    output: output.clone(),
                    duration_ms,
                    speculated,
                });
                indexed.push((
                    index,
                    ToolResult {
                        call_id: call.call_id.clone(),
                        output,
                    },
                ));
            }
            drop(in_flight);

            i = group_end;
        }
        // Any pool entry no committed call ever claimed ran real I/O that
        // never reached the transcript — account for it, don't drop it (#370).
        self.discard_speculation_pool(speculation, SPECULATION_DISCARD_HARVEST_MISMATCH, events);
        indexed.sort_by_key(|(index, _)| *index);
        indexed.into_iter().map(|(_, result)| result).collect()
    }

    /// Execute one tool call, first checking for the malformed-input
    /// sentinel every adapter's stream aggregator falls back to (see module
    /// docs) rather than handing a tool `Null` and getting back a confusing
    /// tool-specific error.
    ///
    /// The malformed-input check comes *before* any hook fires: a `Null`
    /// call is the model's own broken JSON, structurally short-circuited —
    /// it never reaches the executor, so it is not a real tool invocation
    /// and no `PreToolUse`/`PostToolUse` hook is fired for it. When no hooks
    /// are attached this is exactly the previous body:
    /// `self.tools.execute(...)`.
    ///
    /// `events` carries non-blocking hook diagnostics (a hook that failed to
    /// spawn, or exited non-zero on an event that cannot block) to the turn
    /// stream as one non-fatal `Error` per call. `None` on the speculative
    /// path: speculation emits no events until harvest, and a failed
    /// attempt's hook noise must not reach the wire with it.
    ///
    /// The whole dispatch — hooks included — runs under
    /// [`EngineConfig::tool_timeout`], the engine-level backstop for a tool
    /// that blows past its own bound. On a trip the in-flight future is
    /// dropped (so a `PostToolUse` hook does not fire, exactly as for a
    /// tool that never returned) and the model sees a `ToolOutput::Error`
    /// instead of the step parking forever. Dropping cancels at the next
    /// await point: a tool already blocked inside `spawn_blocking` keeps
    /// its thread until the process exits — the engine is unwedged, the
    /// thread is not.
    async fn execute_with_repair(
        &self,
        call: &ToolCall,
        events: Option<&EventSender>,
    ) -> ToolOutput {
        let Some(limit) = self.config.tool_timeout else {
            return self.dispatch_tool_call(call, events).await;
        };
        match tokio::time::timeout(limit, self.dispatch_tool_call(call, events)).await {
            Ok(output) => output,
            Err(_) => ToolOutput::Error {
                message: format!(
                    "tool `{}` exceeded the engine's {}s dispatch ceiling and was abandoned \
                     before it returned — it produced no result, and any work it had already \
                     done outside this process may or may not have landed. This backstop fires \
                     only when a tool overruns its own timeout, so an identical retry will \
                     likely hang the same way: narrow the input, or reach the goal another way.",
                    call.name,
                    limit.as_secs()
                ),
            },
        }
    }

    /// One tool dispatch, unbounded — the body [`Self::execute_with_repair`]
    /// wraps in the timeout backstop.
    async fn dispatch_tool_call(
        &self,
        call: &ToolCall,
        events: Option<&EventSender>,
    ) -> ToolOutput {
        if call.input.is_null() {
            return ToolOutput::Error {
                message: format!(
                    "malformed tool call: `{}`'s arguments were not valid JSON (the model's \
                     streamed output didn't parse) — retry this call with well-formed JSON \
                     arguments",
                    call.name
                ),
            };
        }
        match self.hooks {
            None => self.tools.execute(&call.name, &call.input).await,
            Some(handle) => self.execute_with_hooks(handle, call, events).await,
        }
    }

    /// Wrap a single (well-formed) executor invocation in its `PreToolUse` /
    /// `PostToolUse` hooks. Only reached when hooks are attached.
    ///
    /// `PreToolUse` fires first: if it blocks (a hook exited non-zero, or
    /// failed to even run — per `crate::hooks`'s contract), the tool is NOT
    /// executed and the model instead sees a `ToolOutput::Error` naming the
    /// block, exactly as the engine surfaces every other tool failure as
    /// model-visible data rather than an engine error. Otherwise the tool
    /// runs and `PostToolUse` fires as a pure observation — its outcome can
    /// never block or alter the result, so a failing post-hook cannot abort
    /// the turn. Non-blocking failures from either phase (spawn failures,
    /// non-zero exits on events that cannot block) are no longer discarded:
    /// they surface as one non-fatal `Error { retryable: true }` on the
    /// turn stream when an event channel is present.
    async fn execute_with_hooks(
        &self,
        handle: HooksHandle<'a>,
        call: &ToolCall,
        events: Option<&EventSender>,
    ) -> ToolOutput {
        let pre = run_hooks(
            handle.runner,
            Some(handle.hooks),
            &HookPayload::pre_tool_use(self.config.cwd.clone(), &call.name, call.input.clone()),
        )
        .await;
        if pre.blocked {
            let message = match pre.reason {
                Some(reason) => format!(
                    "tool `{}` was blocked by a PreToolUse hook: {reason}",
                    call.name
                ),
                None => format!("tool `{}` was blocked by a PreToolUse hook", call.name),
            };
            return ToolOutput::Error { message };
        }
        let mut diagnostics = pre.diagnostics;

        let output = self.tools.execute(&call.name, &call.input).await;

        let result_str = match &output {
            ToolOutput::Ok { content } => content.clone(),
            ToolOutput::Error { message } => message.clone(),
        };
        // Observation only — a non-zero PostToolUse exit never blocks or
        // rewrites the result; its failures ride `diagnostics` instead.
        let post = run_hooks(
            handle.runner,
            Some(handle.hooks),
            &HookPayload::post_tool_use(
                self.config.cwd.clone(),
                &call.name,
                call.input.clone(),
                result_str,
            ),
        )
        .await;
        diagnostics.extend(post.diagnostics);

        if !diagnostics.is_empty()
            && let Some(events) = events
        {
            let _ = events.send(AgentEvent::Error {
                message: format!(
                    "hook problem(s) around tool `{}` (non-blocking): {}",
                    call.name,
                    diagnostics.join("; ")
                ),
                retryable: true,
            });
        }

        output
    }
}

/// The boxed-future shape [`retry_with_backoff_observed`] needs from its
/// `attempt_fn` — named here purely to keep the call site in
/// [`Engine::run_model_call`] readable. Each attempt yields the completion
/// AND its still-live speculation future as one value. The caller settles the billed completion synchronously before
/// awaiting that future, closing the cancellation window without moving the
/// mutable budget ledger into concurrent work.
type RetryAttemptFn<'a> = Box<
    dyn FnMut() -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            (CompletionResultAlias, SpeculationFuture<'a>),
                            ProviderError,
                        >,
                    > + 'a,
            >,
        > + 'a,
>;
type CompletionResultAlias = stella_protocol::CompletionResult;
type SpeculationFuture<'a> = Pin<Box<dyn Future<Output = SpeculationPool> + 'a>>;

/// Prefix of the overflow summarizer's marker message
/// ([`Engine::summarize_overflow_span`]). Shared with
/// [`recent_call_records`]: the marker is User-role on the wire, but it is
/// NOT a real user turn and must not act as a loop-detection window
/// boundary.
pub(crate) const SUMMARY_MARKER_PREFIX: &str = "[earlier history summarized";

/// Prefix of the engine-injected stuck-loop steering message
/// ([`Engine::check_loop_detection`]). User-role on the wire like every
/// steer, but engine-generated, not a real user turn — treating it as a
/// window boundary would erase the very evidence that triggered it, and
/// the abort-on-re-detection would need a whole fresh threshold's worth of
/// looping instead of one more no-progress call.
pub(crate) const LOOP_STEER_PREFIX: &str = "[stuck-loop warning";
/// The [`TurnOutcome::Aborted`] reason of a user-requested soft stop —
/// callers match on this to render "stopped" rather than "failed", and to
/// keep (never truncate) the turn's completed work.
pub const SOFT_STOP_REASON: &str = "stopped at step boundary by user — completed steps kept";

#[cfg(test)]
mod tests;
