//! The event vocabulary — plain enum variants flowing from `stella-core` to
//! whichever renderer (TUI or the JSON serializer) is listening.
//! `--output-format stream-json` is a `serde_json` serialization of this
//! exact enum, one line per event: a stable, versioned machine interface.
//!
//! The vocabulary is additive-only: later variants are appended as the
//! context/media/fleet crates land, never a breaking rename.
//!
//! "Additive" is directional, but both directions are now survivable. New
//! *fields* ride `serde(default)`, so a newer reader parses every older
//! stream. New *variants* travel backwards through
//! [`AgentEvent::Unknown`]: an unrecognized `"type"` deserializes into that
//! variant with the original JSON object preserved whole in `payload`, and
//! re-serializes without losing a key or a value. An older binary therefore
//! *skips* an event from the future instead of failing the whole stream.
//!
//! Round-tripping an unknown event preserves its *content*, not its exact
//! bytes: [`serde_json::Value`] holds object keys in a `BTreeMap`, so they
//! come back sorted rather than in their original order. JSON object key
//! order carries no meaning (RFC 8259 §4), so nothing downstream may depend
//! on it — but a consumer diffing raw lines should compare parsed values.
//!
//! The backwards direction is variant-shaped, not field-shaped, and the
//! difference is easy to over-read. A *known* tag carrying a field this build
//! has never heard of parses fine — serde ignores unrecognized keys — but that
//! field is gone the moment the event is re-serialized, because nothing
//! captured it. A proxy or `replay::to_jsonl` relaying a newer stream
//! therefore passes new *events* through whole and silently narrows new
//! *fields* on events it already knows. Only [`AgentEvent::Unknown`] preserves
//! an object verbatim; capturing stray keys on the typed variants would take a
//! `serde(flatten)` overflow map on each one, and no variant carries one.
//!
//! The tolerance is deliberately narrow, and the line matters: the fallback
//! fires **only for an unrecognized tag**. A *recognized* tag whose body does
//! not match its variant is still a hard error, because that is a real
//! encoder bug or a corrupt record — silently degrading it to `Unknown` would
//! turn data corruption into a shrug. See [`KNOWN_TYPE_TAGS`].
//!
//! Note this makes `AgentEvent`'s `Deserialize` impl specific to
//! self-describing formats (it buffers through [`serde_json::Value`] to read
//! the tag before dispatching). That is the format this type has always been
//! defined against — `--output-format stream-json` *is* `serde_json` — so the
//! constraint costs nothing in portability. It does cost work: every decode
//! materializes the whole event as a [`serde_json::Value`] first, so a body's
//! strings are allocated twice on the way in. That is paid on the journal
//! replay path, not on the live emit path (serialization is unaffected), and
//! it is the price of reading the tag before dispatching without hand-writing
//! a visitor per variant.
//!
//! ## Money is `f64`, and that carries one hard edge
//!
//! Every dollar field on this stream — `cost_usd`, `spent_usd`, `limit_usd`,
//! `estimated_cost_usd` — is an `f64`, because JSON has no decimal type and a
//! string-encoded decimal would be a breaking wire change for every existing
//! consumer. At the magnitudes involved (fractions of a cent, summed over
//! thousands of steps) binary rounding is far below a billable unit, so the
//! representation is not the problem.
//!
//! The **non-finite** case is. `serde_json` writes `NaN` and `±Infinity` as
//! JSON `null`, so an emitter that lets a division by a zero estimate reach one
//! of these fields — `calibration_factor` is the realistic path, see
//! [`AgentEvent::Compaction`] — produces a line that this crate then refuses to
//! read back: `null` is not an `f64`, the tag is known, and by the rule above
//! that is a hard error rather than an [`AgentEvent::Unknown`]. For a bare
//! `f64` the whole event is lost, and for an `Option<f64>` it is worse — `null`
//! parses as `None`, so an infinite `limit_usd` comes back as "no limit set".
//! Emitters must keep these fields finite; the wire cannot express the
//! difference.
//!
//! The tolerance stops at `AgentEvent`'s own tag. The enums *nested* inside a
//! variant — [`ModelCallRole`], [`StageKind`], [`PolicyKind`], [`CiStatus`],
//! and their peers — are closed, so a value one of them does not know is a
//! body that does not fit a known tag, i.e. a hard error. Only [`BlockKind`]
//! and [`CacheZone`] carry a `serde(other)` catch-all today. Adding a variant
//! to a nested vocabulary is therefore still a one-directional change, and
//! the reader that meets it drops the whole event, not just the field.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::context_event::CompiledContextFrameBuilt;
// The ladder's own wire vocabulary lives in `crate::ladder`; a verdict event
// carries it, and `ProofStep::Oracle` names the tree an observation ran
// against. Re-exported rather than imported so `event::LadderSnapshot` — the
// path these types had before the move — still resolves for every reader.
pub use crate::ladder::{LadderRung, LadderSnapshot, OracleObservation, ProofTree};
// The proof-step vocabulary moved to `crate::proof` the same way (#1787), with
// the same contract: `event::ProofStep` still resolves for every reader.
pub use crate::proof::ProofStep;
use crate::subagent_event::SubAgentPhase;
use crate::tool::{ToolCall, ToolOutput};

// The context-receipts vocabulary lives in `crate::receipt` and is re-exported
// here unchanged: these types are part of this module's public surface (they
// only ever appear as `AgentEvent` payloads), and every consumer that spells
// `stella_protocol::event::BlockKind` must keep resolving after the split.
pub use crate::receipt::{
    BlockKind, BlockOrigin, CacheZone, ContextFrameRef, ContextProviderUsage, ContextUsage,
    ManifestEntry, ProviderShare,
};

/// A named point in the turn's data flow. Exactly one stage vocabulary
/// exists in this workspace — never duplicated per-crate (the TS-era
/// `StageKind` duplication this structurally forbids, L-E1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StageKind {
    /// Prompt classification and routing: how hard is this turn, and which
    /// tier should serve it.
    Triage,
    /// Context recall: the frames the context plane put in front of the model
    /// before it planned anything.
    ContextRecall,
    /// Pre-plan research: triage named questions, and parallel read-only
    /// sub-agents answer them so the planner names files it has evidence for
    /// rather than guesses (#1778). Skipped whenever triage named none.
    Research,
    /// Planning: the ordered steps the worker is about to attempt.
    Plan,
    /// The interactive approval gate a large plan passes through (L-E5).
    ScopeReview,
    /// Witness authoring: after the worker executes — once the warrant has
    /// read the diff and found something worth proving — an independent
    /// model (the verifier's resolution, never the worker's transcript)
    /// writes the witness test in a pristine snapshot of the pre-execution
    /// tree: a test that FAILS there and will pass once the goal is met,
    /// arming the deterministic flip oracle (L-E11). The witness is visible
    /// to the worker's revise turns (iterating against a failing test is
    /// where convergence comes from); integrity comes from tamper exclusion
    /// at verify time, not from hiding the test.
    Witness,
    /// The worker's own tool-calling loop — the steps that actually change
    /// the workspace.
    Execute,
    /// The deterministic verification ladder: the flip oracle, the touched
    /// tests, the diff budget.
    Verify,
    /// The verifier's verdict, reached only when the deterministic ladder came
    /// back inconclusive (L-E11). Named for the output rather than the model:
    /// a stage called `Verifier` sitting next to `Verify` hid which of the two
    /// was proof and which was opinion.
    ///
    /// Aliased for the same reason as the other renames in this pass: the
    /// stage shipped on the wire as `judge`, so every recorded session names
    /// it that way. Reading them is not optional — replay, the observatory and
    /// the golden fixtures all parse stored streams. `verifier` is aliased too
    /// because it was this stage's name for the length of one commit on this
    /// branch, and a stream recorded against that build must still read.
    #[serde(alias = "judge", alias = "verifier")]
    Verdict,
    /// Post-turn self-reflection: the agent reviews its own performance on
    /// the completed turn and records improvement memories into the context
    /// plane, tagged with the workspace's inferred domains, for recall on
    /// future relevant turns.
    Reflect,
    /// Context write-back: episode summaries and fact upserts landing in the
    /// context plane (close-not-delete, L-C3).
    ContextWrite,
    /// The turn is done. The last stage boundary a turn emits.
    Complete,
}

/// Budget enforcement mode: `off` (no metering),
/// `observed` (meter + warn), `enforced` (hard stop with a clean turn
/// abort — never a mid-tool kill).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BudgetMode {
    /// No metering at all — spend is neither tracked nor reported.
    Off,
    /// Spend is metered and a breach warns, but nothing is ever denied.
    Observed,
    /// A breach aborts the turn at the next clean boundary.
    Enforced,
}

/// Which budget limit a [`AgentEvent::BudgetDenied`] tripped — mirrors
/// `stella-core::budget::BudgetAxis` (kept separate so `stella-protocol`
/// never depends on `stella-core`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BudgetScope {
    /// The per-turn limit — reset at every `run_turn`.
    Turn,
    /// The per-session limit — accumulated across every turn of the session.
    Session,
}

/// What kind of policy-plane decision a [`AgentEvent::PolicyDecision`]
/// records (receipts spec §6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PolicyKind {
    /// A blocking policy chain evaluated a tool call or side effect.
    Evaluated,
    /// A policy denied the call/side effect.
    Blocked,
    /// A policy deferred the call to human approval.
    ApprovalRequested,
    /// A payload-hygiene detector flagged secret-shaped content.
    SecretDetected,
}

/// Concrete purpose of one provider call. This is more precise than the
/// router's tier role: repair and guidance calls must remain distinguishable
/// in the paid-call ledger even when they share a provider/model.
///
/// This vocabulary grows, and it is **not** forward-tolerant: [`Self::Unknown`]
/// is the `serde(default)` for an *absent* `role`, not a `serde(other)`
/// catch-all for an unrecognized one. A role token this build has never seen
/// fails its whole event — `step_usage`, `step_manifest`, `usage_incomplete` —
/// because a known `"type"` with a body that does not fit stays a hard error by
/// design (see the module docs). Adding a variant here is therefore a
/// one-directional change in a way adding an [`AgentEvent`] variant no longer
/// is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ModelCallRole {
    /// Legacy events written before call-role attribution existed. The default
    /// for an absent `role` field only — an unrecognized one is an error.
    #[default]
    Unknown,
    /// Prompt classification and tier routing.
    Triage,
    /// A read-only research sub-agent answering one of triage's pre-plan
    /// questions (#1778).
    Research,
    /// Authoring the ordered plan.
    Plan,
    /// Re-authoring a plan the parser or the scope gate rejected.
    PlanRepair,
    /// Writing the witness test that arms the flip oracle.
    WitnessAuthor,
    /// Fixing a witness that did not fail on the current code.
    WitnessRepair,
    /// The tool-calling loop that actually changes the workspace.
    Worker,
    /// Course-correction handed to a worker that is looping or stuck.
    DistressGuidance,
    /// The verifier's verdict call, on inconclusive deterministic evidence.
    ///
    /// Aliased: this call role shipped as `judge`, so every recorded model
    /// call in every stored session names it that way.
    #[serde(alias = "judge")]
    Verdict,
    /// Generating an agent definition.
    AgentAuthor,
    /// Generating a skill definition.
    SkillAuthor,
    /// Inferring the workspace's domains, for memory tagging and recall.
    DomainInference,
    /// Post-turn self-reflection writing improvement memories.
    Reflection,
    /// The overflow summarizer that replaces a history span with a summary.
    Summarization,
}

/// Content-free reason a provider attempt cannot contribute a truthful usage
/// envelope. Error bodies and prompts are deliberately unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum UsageIncompleteReason {
    /// The provider returned a failure after dispatch, so the request was
    /// received and may have been billed even though no usage frame arrived.
    ProviderError,
    /// The client-side deadline elapsed with the call still in flight. The
    /// server may have completed the work regardless.
    Timeout,
    /// The caller dropped the turn (hard cancel) while a paid provider
    /// attempt was still in flight — the call may have real server-side
    /// cost whose usage is unknowable. Emitted by the engine's drop guard,
    /// which is armed only for exactly that window (a call that settles
    /// normally reports through its ordinary `StepUsage` envelope instead).
    Cancelled,
}

/// `serde(default)` value for [`AgentEvent::RetriesExhausted::retryable`]:
/// event logs recorded before the field existed (#926) replay as if every
/// failure were a genuine, potentially-retryable exhaustion — the reading
/// that was already implied by the event's name before this distinction
/// existed — rather than being silently reclassified as terminal.
fn retries_exhausted_retryable_default() -> bool {
    true
}

/// One event in the turn's stream. Every stage boundary emits an event;
/// nothing user-visible is derived from internal state that isn't also in
/// this stream.
///
/// `remote = "Self"` keeps the derived codec as a pair of *inherent*
/// associated functions instead of the trait impls, so the hand-written
/// [`Serialize`]/[`Deserialize`] impls below can delegate to it after routing
/// [`AgentEvent::Unknown`] around it. Without that indirection the forward-
/// compat fallback would mean hand-writing a visitor for every variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case", remote = "Self")]
pub enum AgentEvent {
    /// The turn entered a new pipeline stage. Every stage boundary emits one,
    /// in the order the pipeline walks them.
    Stage { name: StageKind },
    /// The step's answer text, in full — the authoritative, durable record.
    /// Not a fragment, despite the live preview sibling
    /// [`AgentEvent::TextDelta`]: consumers must REPLACE any accumulated
    /// preview with this value, never append it to one.
    ///
    /// Wire history (#1886): this field was spelled `delta` (and
    /// `text_delta`'s payload was spelled `text` — each variant carried the
    /// other's natural name). Serialization writes the self-describing name;
    /// the alias keeps every recorded stream replaying. Raw-JSONL readers
    /// must stay bilingual the same way: `text` first, legacy `delta` back.
    Text {
        #[serde(alias = "delta")]
        text: String,
    },
    /// One in-order fragment of the answer text, emitted live while the
    /// model call streams. Strictly a best-effort preview: the step's
    /// following `Text` event carries the full text and is authoritative —
    /// consumers must REPLACE any accumulated deltas with it, never merge
    /// (a retried model call re-streams its deltas from the start, so the
    /// accumulation can be garbled; there is no reset marker). Additive to
    /// the stream-json wire contract: consumers must tolerate `text_delta`
    /// lines appearing between events, and persistence layers may drop them
    /// (the `Text` event is the durable record). Wire history: `delta` was
    /// spelled `text` before #1886 — the alias accepts both; see [`AgentEvent::Text`].
    TextDelta {
        #[serde(alias = "text")]
        delta: String,
    },
    /// One in-order fragment of the model's thinking/extended-reasoning
    /// stream, for the providers that expose it. Unlike [`AgentEvent::Text`]
    /// this really is a fragment — consumers accumulate, and the journal
    /// coalesces a consecutive run into one record.
    Reasoning { delta: String },
    /// The model requested a tool call and the engine is about to run it. The
    /// matching [`AgentEvent::ToolResult`] correlates by `call.call_id`.
    ToolStart { call: ToolCall },
    /// A tool call finished, successfully or not — `output` is the typed
    /// [`ToolOutput`], never a bare string, so a failure is inspectable
    /// without sniffing prose. Correlates to its [`AgentEvent::ToolStart`] by
    /// `call_id`.
    ToolResult {
        call_id: String,
        output: ToolOutput,
        duration_ms: u64,
        /// True when this result was produced by speculative execution: the
        /// call was read-only and began executing while the model was still
        /// streaming the rest of its response, so `duration_ms` (the real
        /// execution time) overlapped the model call instead of following
        /// it. `serde(default)` so streams recorded before this field parse.
        #[serde(default)]
        speculated: bool,
    },
    /// A speculatively-executed read-only call (`stella-core::speculation`)
    /// whose result never reached the transcript: its stream attempt failed
    /// and the pool was dropped, or the committed call diverged from what
    /// was announced so the pooled result was rejected at harvest. The
    /// tool's real I/O still ran — this is the event-log's record of that
    /// work, so call counts reconcile with what actually executed rather
    /// than silently diverging. `reason` is a short stable token
    /// (`"attempt_failed"`, `"harvest_mismatch"`). Additive to the wire
    /// contract: consumers recorded before speculation existed never see it.
    SpeculationDiscarded {
        call_id: String,
        name: String,
        reason: String,
    },
    /// A model call is being retried with backoff. Flushed only for steps
    /// that COMMIT — a step that exhausts its retries reports the whole
    /// doomed sequence through [`AgentEvent::RetriesExhausted`] instead.
    Retry {
        /// 1-indexed ordinal of the attempt that FAILED and triggered this
        /// retry — the initial call is attempt 1 (mirrors
        /// `stella-core::retry::RetryAttempt::attempt`).
        attempt: u32,
        reason: String,
    },
    /// A user message queued mid-turn was injected at a step boundary
    /// (`stella-core` steering) — the transcript's record that the model
    /// was steered, and when.
    Steered { text: String },
    /// Loop detection fired (receipts spec §6.3, #364 gap 3): the typed
    /// twin of the prose steer/abort, so receipts can parse the decision
    /// instead of string-matching an `Error` prefix. Emitted on BOTH
    /// outcomes — the first detection steers (`aborted: false`) and a
    /// detection that persists past the warning aborts (`aborted: true`).
    /// Additive to the wire contract: older consumers never see it.
    LoopDetected {
        turn_instance: u32,
        /// `"exact_repeat"` | `"short_cycle"` | `"stagnation"` — mirrors
        /// `stella-core::loop_detect::LoopVerdict` (kept as a string here so
        /// `stella-protocol` never depends on `stella-core`).
        kind: String,
        /// Tool names of the repeated signature, in cycle order (one entry
        /// for an exact repeat or a stagnating tool).
        pattern: Vec<String>,
        /// Consecutive identical calls (exact repeat), full cycles (short
        /// cycle), or consecutive no-progress calls (stagnation) observed.
        repeats: usize,
        /// The human-readable evidence — same text the paired
        /// `Steered`/`Error` carries.
        evidence: String,
        /// `false`: first detection, the turn was steered and continues.
        /// `true`: detection persisted after the warning, the turn aborted.
        aborted: bool,
    },
    /// An enforced budget stopped the turn (receipts spec §6.3, #364 gap
    /// 3): the typed twin of the prose "budget exceeded" `Error`. Only ever
    /// emitted in `BudgetMode::Enforced` — observed mode warns without
    /// denying.
    BudgetDenied {
        /// Which limit tripped.
        scope: BudgetScope,
        spent_usd: f64,
        limit_usd: f64,
        mode: BudgetMode,
    },
    /// A model call failed terminally after exhausting its retries
    /// (receipts spec §6.3, #364 gap 3). The per-attempt reasons were
    /// previously lost on the failure path — `Retry` events only flush for
    /// steps that COMMIT — so this is the durable record of the doomed
    /// attempts. Emitted just before the paired `Error`.
    RetriesExhausted {
        turn_instance: u32,
        /// Total dispatched attempts that failed (the initial call plus
        /// every retry). Equals `reasons.len()`.
        attempts: u32,
        /// Per-attempt failure reasons, oldest first.
        reasons: Vec<String>,
        /// Whether the LAST attempt's error was of a retryable class —
        /// mirrors the paired [`AgentEvent::Error`]'s `retryable` field,
        /// computed from the same `ProviderError::is_retryable()` call.
        /// `false` means retrying again could never have helped: the
        /// clearest case is `ProviderError::Auth` failing on attempt 1,
        /// where `attempts` is 1 and no retry was ever attempted despite
        /// this event's name (#926) — a receipts/telemetry consumer that
        /// only sees `RetriesExhausted` would otherwise record a
        /// reliability incident for what is really a bad credential.
        /// `#[serde(default = "retries_exhausted_retryable_default")]`:
        /// event logs recorded before this field existed predate the
        /// distinction, so on replay they read as "genuinely retryable"
        /// — the pre-#926 behavior — rather than being silently
        /// reclassified as terminal.
        #[serde(default = "retries_exhausted_retryable_default")]
        retryable: bool,
    },
    /// One decision from the extension/policy audit plane bridged into the
    /// event stream (receipts spec §6.4, #364 gap 6). The `HookBus` audit
    /// events (`policy.evaluated`/`policy.blocked`, `approval.requested`,
    /// `secret.detected`) were process-ephemeral — hosts map them onto this
    /// variant so the journal carries the policy plane too. Content-free by
    /// design: `subject` names the tool/capability/path, NEVER a secret
    /// value or file contents.
    PolicyDecision {
        kind: PolicyKind,
        /// The tool name, capability, or workspace-relative path the
        /// decision was about.
        subject: String,
        /// Short outcome token — e.g. `"allow"`, `"deny"`, `"modify"`, a
        /// detector's kind list — never content.
        outcome: String,
    },
    /// A compaction pass ran (`stella-core::compaction`). Fields mirror
    /// `CompactionReport` — kept as a flat struct here (not a re-exported
    /// type) so `stella-protocol` never depends on `stella-core` (dependency
    /// direction: core depends on protocol, never the reverse).
    Compaction {
        before_tokens: u64,
        after_tokens: u64,
        evicted: usize,
        deduped: usize,
        /// Older results of a repeated identical call, stubbed as stale.
        /// `serde(default)` so journals written before these fields parse.
        #[serde(default)]
        superseded: usize,
        /// Large old outputs middle-out truncated instead of dropped whole.
        #[serde(default)]
        aged: usize,
        /// Messages replaced by a model-written history summary — the
        /// overflow fallback when eviction alone cannot reach budget.
        #[serde(default)]
        summarized: usize,
        /// The `block_id`s each pass stubbed (spec §6.2) — identities, not just
        /// counts, so the receipt records *which* blocks left context and a
        /// later pass can prove a block was evicted before it was ever cited or
        /// referenced (the wasted-carry signal). For the pure passes each vec's
        /// length equals its count field (`summarized_blocks` is the documented
        /// exception). `serde(default)` — absent on pre-identity journals.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        evicted_blocks: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        deduped_blocks: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        superseded_blocks: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        aged_blocks: Vec<String>,
        /// The `block_id`s of the tool-result blocks folded into an
        /// overflow-summary splice (spec §6.2). Unlike the pure passes — which
        /// stub tool-result blocks one-for-one, so their vec length equals the
        /// count — the summary replaces a whole message span whose `summarized`
        /// count also covers user/assistant text carrying no block identity;
        /// this vector is the identity-bearing (tool-result) subset that left
        /// context, so `summarized_blocks.len()` may be less than `summarized`.
        /// `serde(default)` — absent on pre-identity journals.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        summarized_blocks: Vec<String>,
        /// The replacement bytes each in-place rewrite left behind, one entry
        /// per digest — what lets reconstruction resolve a compacted block to
        /// the bytes the model received rather than the pre-compaction output
        /// under the same `call_id` (#1667); see [`CompactionRewrite`].
        /// `serde(default)` — absent on journals written before rewrites were
        /// journaled, whose compacted blocks surface as digest mismatches.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rewrites: Vec<crate::CompactionRewrite>,
        /// The budget this pass actually compared against — the raw compaction
        /// budget divided by the model's calibration factor — and that factor.
        /// The event's `before/after_tokens` are raw estimates; these are the
        /// numbers the eviction loop's stopping condition used, so the receipt
        /// lines up with the decision (#364 item 1). `0` on pre-receipt journals.
        #[serde(default)]
        effective_budget_tokens: u64,
        /// The per-model calibration factor the pass divided by. `serde(default)`
        /// makes this `0.0` on a pre-receipt journal, which is a sentinel, NOT a
        /// factor: recovering the raw budget as `effective * factor` yields 0 and
        /// dividing by it yields infinity. A consumer must read `0.0` as "this
        /// journal predates calibration" and skip the derivation, exactly as it
        /// reads `effective_budget_tokens == 0`. (The identity factor is `1.0`;
        /// the default cannot be changed to it without rewriting how every
        /// already-written journal decodes.)
        #[serde(default)]
        calibration_factor: f64,
    },
    /// Emitted after every provider/media call that spends money. The TUI HUD
    /// renders spend live from this stream; nothing user-visible about spend
    /// is derived from state that isn't also in this event.
    BudgetTick {
        spent_usd: f64,
        limit_usd: Option<f64>,
        mode: BudgetMode,
        /// Session-scoped spend at this tick — `spent_usd`/`limit_usd` are
        /// turn-scoped, so a HUD cannot otherwise reconstruct session state
        /// (or see a session-axis breach) from this stream. `None` when the
        /// emitter does not track a session axis, and on events serialized
        /// before these fields existed (hence `serde(default)`, so older
        /// streams still parse).
        #[serde(default)]
        session_spent_usd: Option<f64>,
        /// The configured per-session limit, when one is set. `None` mirrors
        /// `session_spent_usd`.
        #[serde(default)]
        session_limit_usd: Option<f64>,
    },
    /// One committed model call — the metering record. Emitted exactly once
    /// per step that lands, carrying the normalized usage envelope plus
    /// everything a metering/billing pipeline needs to price and audit the
    /// call; aggregate a turn by summing its `StepUsage` events.
    StepUsage {
        step: usize,
        /// Exact call purpose. Missing legacy values deserialize as
        /// [`ModelCallRole::Unknown`].
        #[serde(default)]
        role: ModelCallRole,
        /// Provider which actually served this call, never the session's
        /// configured default. Empty only on legacy events.
        #[serde(default)]
        provider: String,
        /// Authoritative model output for calls that do not emit a separate
        /// [`AgentEvent::Text`] (pipeline management and compaction calls).
        /// Execute calls leave this `None`, avoiding duplicate transcript
        /// text while keeping older event consumers compatible.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_text: Option<String>,
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        cached_input_tokens: u64,
        /// Tokens written to the provider's prompt cache by this call
        /// (`CompletionUsage::cache_write_tokens`). Reported separately from
        /// `input_tokens`, never a subset of it. `0` when the provider does
        /// not report cache writes (the OpenAI-compatible dialects) — hence
        /// `serde(default)`, so streams serialized before this field existed
        /// still parse.
        #[serde(default)]
        cache_write_tokens: u64,
        /// The reasoning share of `output_tokens`, when the provider breaks it
        /// out (`CompletionUsage::reasoning_tokens`). Already inside
        /// `output_tokens` — a diagnostic split, never its own cost line.
        ///
        /// Absent means the provider does not report it (every Anthropic
        /// Messages API call, which folds thinking into `output_tokens`);
        /// `0` means it reported no reasoning on this call. A consumer that
        /// reads absent as zero would conclude the entire Anthropic-direct
        /// route never thinks, so this stays `Option` rather than defaulting.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_tokens: Option<u64>,
        /// The engine's RAW (uncalibrated) pre-call estimate of the input it
        /// sent — paired with `input_tokens` (plus cache-write tokens, which
        /// are real prompt tokens split out only for pricing) this is one
        /// drift sample, the feedback that calibrates future estimates per
        /// model (`stella-core::estimator::Calibration`). Raw by contract:
        /// consumers rebuild the correction from these pairs, and a
        /// corrected estimate here would compound the correction on every
        /// round trip. Attachment weight is excluded — the media estimate is
        /// a deliberate ~80× over-estimate of billed tokens, right for
        /// context pressure and poison as a drift sample. `0` means no
        /// estimate was taken (pre-drift emitters — hence `serde(default)`,
        /// so old streams still parse).
        #[serde(default)]
        estimated_input_tokens: u64,
        cost_usd: f64,
        duration_ms: u64,
        retries: u32,
        tool_calls: usize,
        /// Whether the provider supplied a truthful usage envelope. Missing
        /// legacy values fail closed to `false`.
        #[serde(default)]
        complete: bool,
        /// Why generation stopped, as the provider reported it
        /// ([`crate::completion::FinishReason`]). `Length` is the
        /// only *ground truth* a consumer has that this step was cut off at
        /// the output ceiling — the "we stopped first" event.
        ///
        /// It is here because it was previously nowhere: the engine knew the
        /// reason and dropped it at this boundary, so every downstream reader
        /// had to *infer* truncation from step shape. The benchmark harness
        /// inferred it as "≥16384 output tokens and no tool call", which was
        /// right when the output ceiling was 16K and became a false positive
        /// the moment the ceiling moved to 64K — the reading behind the
        /// unexplained `cap_hits: 106` in the GLM-5.2 head-to-head. An
        /// inferred cap hit cannot be told from a long answer; a reported one
        /// can.
        ///
        /// `None` means the provider did not report a reason (or the stream
        /// predates this field — hence `serde(default)`), and must never be
        /// read as "not truncated": absence of the signal is not evidence of
        /// a clean stop.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish_reason: Option<crate::completion::FinishReason>,
    },
    /// A provider call failed or timed out after dispatch, so local accounting
    /// cannot prove that no billable work occurred. Content-free by design.
    UsageIncomplete {
        role: ModelCallRole,
        provider: String,
        model: String,
        reason: UsageIncompleteReason,
        duration_ms: u64,
        /// Number of retries completed before the failure, when known.
        retries: Option<u32>,
        /// Accounting the adapter had already observed when the attempt died.
        ///
        /// "Incomplete" is not the same as "unknown", and this field is the
        /// difference. A stream cut mid-answer has usually already been told
        /// what the prompt cost, so `Some` here turns a bare warning into a
        /// number: how much of this attempt we can actually account for.
        /// `None` is the honest answer for a failure that learned nothing —
        /// a connect timeout, a cancelled call, a 5xx with no stream.
        ///
        /// Token counts are content-free, so carrying them keeps this event
        /// inside its no-prompts-no-bodies contract.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partial: Option<crate::completion::PartialUsage>,
    },
    /// A verifier model's assessment of a goal-driven loop after one working
    /// round. `met == true` ends the loop; `met == false` feeds `reasoning`
    /// back to the worker as course-correction. `cost_usd` is the verifier
    /// call's own spend.
    GoalVerdict {
        round: usize,
        met: bool,
        reasoning: String,
        cost_usd: f64,
    },
    /// A provider's circuit breaker opened and the router fell back to the
    /// next configured provider of the same role's tier. Never silent
    /// (L-M7) — no mid-turn family switch happens without this event.
    ProviderFallback {
        from: String,
        to: String,
        reason: String,
    },
    /// A file was read/created/modified/deleted by the agent, carrying both
    /// the authoritative line delta and a diff for display.
    ///
    /// The single emission point is `ToolRegistry::record_touch` — the same
    /// place that writes the session's file-touch ledger and its telemetry
    /// payload — so the TUI, the audit log and the exported JSON can no longer
    /// disagree about what a turn changed. (This doc once claimed one emission
    /// point "by construction" while the deck in fact synthesized its own
    /// events from tool inputs, in a wrapper that knew only four tool names and
    /// sat on one of three tool stacks. Files edited in bulk, or by a worker
    /// lane, were reported as `+0 -0`.)
    ///
    /// `added`/`removed` are the counts the recorder derived from the real pre-
    /// and post-images (`file_touch::line_diff`). Consumers **must** use them
    /// rather than counting `+`/`-` lines in `diff`: the diff is a bounded,
    /// deliberately coarse rendering of the changed region, and re-deriving
    /// from it is what made the two disagree. Reads carry `0/0` and no diff;
    /// consumers that only care about mutations filter on `kind`.
    FileChange {
        path: String,
        kind: FileChangeKind,
        /// `serde(default)` so journals written before the counts existed
        /// parse — those replay as `0/0`, which is what they recorded.
        #[serde(default)]
        added: u32,
        #[serde(default)]
        removed: u32,
        diff: Option<String>,
    },
    /// Context recall completed: which frames reached the prompt, from which
    /// providers, at what token cost. Every frame carries a human
    /// `citation_label`, never a raw id (L-C4).
    ContextRecall {
        frames: Vec<ContextFrameRef>,
        provider_mix: Vec<ProviderShare>,
        tokens: u32,
        /// The CGP usage report for this recall (`docs/spec/adaptive-context/context-reuse.md` §2):
        /// per-provider frame counts and token costs against the requested
        /// budget, so context cost is meterable rather than merely visible.
        /// Optional and defaulted — streams recorded before the report existed
        /// still deserialize (the additive contract), and a recall path with
        /// no CGP host behind it has none to report.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<ContextUsage>,
        /// Wall-clock milliseconds the recall itself took (#875).
        ///
        /// Recall sits on the **first-token path of every turn**, so a slow
        /// one delays everything after it — and until this existed, a cold
        /// store, a large corpus or a wedged embedding call was
        /// indistinguishable from a fast recall right up until it became a
        /// timeout. Defaulted, so older streams still deserialize; `0` there
        /// means "not measured", not "instant".
        #[serde(default)]
        latency_ms: u32,
        /// Whether the IVF approximate-nearest-neighbour accelerator fired,
        /// or `None` when the recall path did not report it. Latency alone
        /// cannot be acted on: a slow recall with a cold index is a different
        /// problem, with a different fix, from one that used the index and
        /// was still slow.
        ///
        /// Tri-state on purpose. `stella-context` knows the answer, but the
        /// CGP host fan-out production recall goes through does not carry it
        /// across the provider-result boundary — closing that is a change to
        /// the provider contract, not to this event. A plain `bool` would
        /// report `false` on every real turn, which reads as "the index never
        /// fires" rather than "nobody said". `None` says what is true.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        used_ann_index: Option<bool>,
    },
    /// Context write-back completed: episode summaries, fact upserts,
    /// supersession (bi-temporal, close-not-delete per L-C3).
    ContextWrite {
        provider: String,
        upserts: u32,
        superseded: u32,
    },
    /// A context block first became eligible to enter the prompt (spec §4).
    /// The birth record that makes the per-step manifest an index over the fold.
    ///
    /// Digest, not bytes, for the kinds the journal already carries whole
    /// (`ToolResult`, `ToolCall`/`ToolStart`, assistant `Text`): those preimages
    /// are resolved from the originating event at reconstruction time, never
    /// re-stored. For the two kinds the fold does NOT carry — the system prefix
    /// and the assembled user/recall message — `content` carries the bytes so
    /// the step is reconstructable (spec §5.3). "Content-free" is therefore a
    /// claim about two specific things and no others: the *export* projection,
    /// which strips `content`, and the *wire shape of journal-resolvable
    /// kinds*, which never carry bytes at all. It is **not** a claim about this
    /// event stream, which gap-kind blocks do put prompt bytes on: everything
    /// emitted here also reaches `--output-format stream-json` on stdout and
    /// any durable stream file configured behind it. Treat a raw event stream
    /// as carrying prompt content and redirect it accordingly. Additive:
    /// consumers recorded before receipts existed simply never see this event.
    BlockRegistered {
        /// `blk_<24 hex of sha256(kind \0 content)>`. Byte-identical blocks
        /// share an id, so dedup/supersession become identities not counts.
        block_id: String,
        kind: BlockKind,
        origin: BlockOrigin,
        /// Estimated tokens at birth (the engine's estimator).
        token_cost: u32,
        /// `"sha256:<full hex>"` — verifies the preimage on reconstruction.
        content_digest: String,
        /// Human label for recall frames / memory nodes, when the block has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        citation_label: Option<String>,
        /// The preimage for gap kinds the journal cannot resolve (the system
        /// prefix, the assembled user/recall message). `None` for
        /// journal-resolvable kinds (tool I/O, assistant text) — those never
        /// carry bytes here. Redacted by the content-free export projection,
        /// but present on the live event stream, so it reaches stream-json
        /// stdout: this is the one field on `AgentEvent` that can carry raw
        /// prompt text, and the only thing keeping it off a remote sink is
        /// where the operator points the stream.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
    },
    /// The ordered receipt of exactly what the model saw on one step (spec §5):
    /// the block sequence sent, in wire order, plus the budget the compaction
    /// pass actually compared against this step. Emitted immediately before the
    /// step's model call commits. Content-free (block ids + small ints); the
    /// preimages are resolved from the fold at inspection time. This is the
    /// record that makes any past step reconstructable and auditable.
    StepManifest {
        /// Monotonic per session — groups the steps of one `run_turn`.
        turn_instance: u32,
        step: usize,
        /// Disambiguates the several model calls that can share one
        /// `(turn_instance, step)`. The engine's own worker call is always 0;
        /// auxiliary calls that ride the same step — the overflow summarizer,
        /// and the pipeline's triage/verifier/plan/guidance roles — take 1, 2, …
        /// from a per-execution counter. Without it a summarizer receipt and
        /// the worker receipt it precedes collide on the primary key and the
        /// auxiliary one is silently replaced. `serde(default)` so manifests
        /// persisted before this field existed still decode (as the worker 0).
        #[serde(default)]
        call_seq: u64,
        role: ModelCallRole,
        provider: String,
        model: String,
        /// Blocks in wire order; index 0 is the system prefix.
        blocks: Vec<ManifestEntry>,
        /// The budget the compaction pass actually compared against THIS step —
        /// the raw budget divided by the model's calibration factor. Evented so
        /// the receipt's numbers line up with the decision that was made (the
        /// `Compaction` event's raw before/after do not, on their own — #364).
        effective_budget_tokens: u64,
        /// The per-model calibration factor applied to the raw budget.
        calibration_factor: f64,
        /// Sum of block token costs, pre-call (the engine's raw estimate).
        estimated_input_tokens: u64,
        /// This manifest's identity as a **compiled context frame** — ADR 0006
        /// as amended: the compiled frame is this manifest extended, not a
        /// parallel aggregate, so its id and hash are fields here rather than a
        /// second record of the same call.
        ///
        /// `Some` only when `context.lifecycle.enabled` is on; `None`
        /// otherwise and on every manifest recorded before the frame existed.
        /// The hash covers what entered the prompt and deliberately excludes
        /// the accounting around it — `provider`, `model`, `call_seq`, the two
        /// budget numbers, and each entry's `resident_since_step` — so two runs
        /// of identical work agree even when served by different models. See
        /// `stella_core::receipts::compiled_frame` for the exact preimage.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compiled_frame: Option<CompiledContextFrameBuilt>,
    },
    /// One observable step of the proof this turn is building for itself.
    ///
    /// The pipeline already decided all of this — it read the diff, bought or
    /// declined a witness, watched a command fail and then pass — and until
    /// now kept every step to itself. A run that proves its work looked
    /// identical to one that did not, because the only proof artifact on the
    /// stream was the verdict at the end. These are the intermediate
    /// observations, emitted as they are made, so a renderer can show the
    /// proof accumulating **beside** the work instead of announcing it after.
    ///
    /// Strictly observability: no consumer decides anything from these, and
    /// the verdict remains the authority on whether the work is verified.
    Proof { step: ProofStep },
    /// A verification verdict — from the deterministic ladder (flip oracle,
    /// touched-tests-green) or the model verifier (L-E11: deterministic-first;
    /// model verifiers handle only inconclusive evidence).
    ///
    /// Aliased: this event shipped on the wire as `judge_verdict`. Without the
    /// alias a stored stream's verdict line does not fail loudly — serde skips
    /// to the next variant and the event simply *disappears*, which is how the
    /// golden trajectories came to be one event short rather than unparseable.
    #[serde(alias = "judge_verdict")]
    Verdict {
        passed: bool,
        evidence: VerdictEvidence,
    },
    /// Interactive gate before large plans execute (L-E5): the pipeline
    /// pauses on this event and waits for approval above configured
    /// thresholds; headless requires a flag to bypass.
    ScopeReview { proposal: ScopeProposal },
    /// Interactive gate before a mutating tool call writes (#1265): the host
    /// pauses on this event and waits for the reviewer to choose which of the
    /// proposed hunks to apply.
    ///
    /// A sibling of `ScopeReview` at a different granularity — that one gates a
    /// *plan* before it runs, this one gates a *write* before it lands, so a
    /// call carrying one wanted and one unwanted change stops being
    /// all-or-nothing. The answer returns through the host's own decision
    /// channel and the card is cleared by a `ToolResult` carrying this
    /// proposal's `id`, exactly as `AskUser` is; there is no separate answer
    /// event. Runs with no reviewer never emit this at all — the gate is not
    /// installed rather than installed and auto-approving, because a mutation
    /// gate that answers itself is worse than no gate.
    HunkReview { proposal: HunkProposal },
    /// The agent asked the user a multiple-choice question (the `ask_user`
    /// tool). BINDING renderer contract: present the structured `options`
    /// AND always exactly one additional free-text option — the user can
    /// always answer in their own words, on every question, without the
    /// model having to list that affordance itself. The answer returns as
    /// the tool call's ordinary `ToolResult`; there is no separate answer
    /// event. Headless runs fail this tool with a named error instead of
    /// hanging on input that will never arrive.
    AskUser {
        /// Correlates the eventual answer (the ToolResult's `call_id`)
        /// back to this question.
        id: String,
        question: String,
        options: Vec<String>,
    },
    /// A media generation job changed state. Video jobs are async and
    /// long-lived; this event is how the TUI shows progress without polling
    /// shared state (L-T1).
    MediaProgress {
        artifact_id: String,
        kind: MediaKind,
        state: MediaJobState,
    },
    /// A media artifact landed under `.stella/artifacts/` with a manifest
    /// row.
    MediaComplete { artifact: MediaArtifactRef },
    /// A commit landed (fleet ledger / pipeline execute stage).
    Commit { sha: String, message: String },
    /// A pull request was opened or changed status (fleet PR/CI monitor).
    /// `number` and `ci` ride `serde(default)` so streams recorded before
    /// they existed still parse (additive-only wire contract).
    Pr {
        url: String,
        status: PrStatus,
        /// The PR number (e.g. 183 for `…/pull/183`). `None` on streams
        /// recorded before the field existed or when the monitor could not
        /// parse one from the URL.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        number: Option<u64>,
        /// The head commit's aggregate CI verdict, when observed. Absent
        /// means "not polled yet", never "passing".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ci: Option<CiStatus>,
    },
    /// The turn's task board changed (an agent called one of the `task_*`
    /// tools). Carries the FULL board snapshot, not a delta — the render
    /// fold stays pure and any single event reconstructs the checklist,
    /// which is what makes dead-session replay show the board as it was.
    TaskUpdate { tasks: Vec<TaskItem> },
    /// A bounded child turn started or finished — the `Started`/`Finished`
    /// bracket IS the attribution for every event emitted between them.
    /// See [`crate::subagent_event`] for what the child forwards and what it
    /// deliberately drops at that boundary.
    SubAgent { phase: SubAgentPhase },
    /// The turn failed. `retryable` is the source's own classification (see
    /// [`crate::error::ProviderError::is_retryable`]), never re-derived from
    /// `message` by a consumer.
    Error { message: String, retryable: bool },
    /// The turn finished — the stream's terminator, and the only event a
    /// consumer may treat as "nothing more is coming". `cost_usd` is the
    /// turn's total spend and `model` the model that served its last committed
    /// call; both are summaries of the `StepUsage` events that preceded it, not
    /// a separate source of truth. A turn that fails ends on
    /// [`AgentEvent::Error`] instead, never on both.
    Complete { model: String, cost_usd: f64 },
    /// An event whose `"type"` this binary does not recognize — almost always
    /// one emitted by a NEWER stella than the one reading. The whole original
    /// JSON object is preserved in `payload` (tag included), so a proxy,
    /// recorder, or replay tool can pass a future event through without
    /// understanding it. Object keys may come back sorted; no value is lost.
    ///
    /// This is the variant that makes the vocabulary safe to extend. Consumers
    /// should treat it as inert: count it, log it, pass it on — never fail on
    /// it, and never try to guess its semantics from `event_type`.
    ///
    /// `serde(skip)` keeps it out of the derived codec entirely; the
    /// hand-written impls below construct and flatten it. It therefore has no
    /// wire tag of its own and can never be produced by a literal
    /// `{"type":"unknown"}` — that input is an unknown tag like any other and
    /// round-trips as `event_type: "unknown"`.
    ///
    /// Producer contract for anyone *constructing* this variant by hand
    /// (synthesizing a passthrough event, rewriting a recorded stream):
    /// `payload` must be a JSON **object** carrying a `"type"` key equal to
    /// `event_type`, and `event_type` must not be one of [`KNOWN_TYPE_TAGS`].
    /// Serialization emits `payload` verbatim, so a non-object payload, or one
    /// whose tag disagrees, produces a stream-json line this crate cannot read
    /// back; a known tag here makes [`AgentEvent::type_tag`] name a variant
    /// this value is not, so a consumer dispatching on the tag rather than the
    /// variant is misrouted. The decode side is only ever fed objects with an
    /// unrecognized tag, so nothing checks either invariant for you.
    #[serde(skip)]
    Unknown {
        /// The unrecognized `"type"` value, lifted out for cheap matching.
        event_type: String,
        /// The complete original object, including its `"type"` field.
        payload: Value,
    },
}

/// The single source of truth for the variant ↔ wire-tag mapping.
///
/// [`AgentEvent::type_tag`] and [`KNOWN_TYPE_TAGS`] both expand from this one
/// list, so the two can never disagree. That matters more than it looks: the
/// deserializer decides "is this tag from the future?" by consulting
/// `KNOWN_TYPE_TAGS`, so a real tag missing from it would quietly demote every
/// one of its events to [`AgentEvent::Unknown`] — data loss with no error.
/// Generating both from one list makes that class of bug unrepresentable.
///
/// The `E0004` tripwire survives the move: the generated match is still
/// exhaustive, so adding a variant without adding it here fails
/// `cargo build -p stella-protocol` at the invocation below.
macro_rules! agent_event_tags {
    ($($variant:ident => $tag:literal,)*) => {
        impl AgentEvent {
            /// The stable discriminant tag for this event — identical to the
            /// string `serde` writes as the `"type"` field on the stream-json
            /// wire. Allocation-free, so logs, metrics, and tests can name an
            /// event without serializing it.
            ///
            /// For [`AgentEvent::Unknown`] this returns the *preserved
            /// original* tag, not a placeholder — an unrecognized event still
            /// reports truthfully what it was on the wire.
            ///
            /// Which means the return value is no longer drawn from a closed
            /// set: an `Unknown` tag is arbitrary, unbounded, externally
            /// authored text. Grouping, indexing, or labelling a metric by
            /// this string is safe only against [`KNOWN_TYPE_TAGS`] — bucket
            /// anything outside it as one `unknown` cohort rather than letting
            /// a foreign stream drive cardinality.
            #[must_use]
            pub fn type_tag(&self) -> &str {
                match self {
                    $(AgentEvent::$variant { .. } => $tag,)*
                    AgentEvent::Unknown { event_type, .. } => event_type.as_str(),
                }
            }
        }

        /// Every `"type"` tag this build decodes into a typed variant.
        ///
        /// The deserializer's forward-compat fallback keys off exactly this
        /// list: a tag present here must parse into its variant or fail loudly;
        /// a tag absent from it becomes [`AgentEvent::Unknown`]. Consumers can
        /// also use it to detect that a stream came from a newer stella.
        pub const KNOWN_TYPE_TAGS: &[&str] = &[$($tag,)*];
    };
}

agent_event_tags! {
    Stage => "stage",
    Text => "text",
    TextDelta => "text_delta",
    Reasoning => "reasoning",
    ToolStart => "tool_start",
    ToolResult => "tool_result",
    SpeculationDiscarded => "speculation_discarded",
    Retry => "retry",
    Steered => "steered",
    LoopDetected => "loop_detected",
    BudgetDenied => "budget_denied",
    RetriesExhausted => "retries_exhausted",
    PolicyDecision => "policy_decision",
    Compaction => "compaction",
    BudgetTick => "budget_tick",
    StepUsage => "step_usage",
    UsageIncomplete => "usage_incomplete",
    GoalVerdict => "goal_verdict",
    ProviderFallback => "provider_fallback",
    FileChange => "file_change",
    ContextRecall => "context_recall",
    ContextWrite => "context_write",
    BlockRegistered => "block_registered",
    StepManifest => "step_manifest",
    Proof => "proof",
    Verdict => "verdict",
    ScopeReview => "scope_review",
    HunkReview => "hunk_review",
    AskUser => "ask_user",
    MediaProgress => "media_progress",
    MediaComplete => "media_complete",
    Commit => "commit",
    Pr => "pr",
    TaskUpdate => "task_update",
    SubAgent => "sub_agent",
    Error => "error",
    Complete => "complete",
}

// Adding a variant? The `E0004` at `agent_event_tags!` above is the first
// tripwire. Add the tag there, then propagate the variant to every downstream
// matcher — the tag table alone is not enough:
//
// **Compile-enforced** — also exhaustive, so they will not build until you add
// an arm; but each break surfaces one crate at a time (CI stops at the first
// failing crate), which is exactly how #415's variant reached `main` before
// breaking `stella-pipeline` (#421) then `stella-tui` (#422):
//   - `stella-pipeline` `replay::event_signature`
//   - `stella-tui` `model::Model::apply`
//   - `stella-tui` `textline::event_line`
//   - `stella-tui` `deck::trace_of`
//
// **Silent** — wildcard / `matches!` arms the compiler CANNOT catch, so a new
// variant falls through to a default and is wrong only at runtime. These are
// the real trap; audit them by hand:
//   - `stella-pipeline` `replay::structural_diff` volatile keep-set: add the
//     variant if it is a run-to-run artifact absent from older golden streams,
//     or it will shift every aligned position of the diff.
//   - `stella-tui` `deck::event_intensity` and `deck::status_from_event`: give
//     the variant an intensity / agent status if it should register on the
//     fleet deck.
//
// The same duty applies to the other exhaustively-matched cross-crate enums
// this pattern warns about (`ToolOutput`, `BudgetOutcome`).
//
// Note that none of this is about *wire* safety any more — an older reader
// survives your new variant via `AgentEvent::Unknown`. It is about this
// workspace's own renderers staying complete.

impl AgentEvent {
    /// Whether this event arrived with a `"type"` this build does not know —
    /// i.e. it was emitted by a newer stella. Consumers that want to surface
    /// "there is something here I cannot render" ask this rather than matching
    /// the variant directly.
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        matches!(self, AgentEvent::Unknown { .. })
    }
}

impl Serialize for AgentEvent {
    /// Every known variant delegates to the derived codec, so the wire format
    /// is unchanged. [`AgentEvent::Unknown`] bypasses it and re-emits the
    /// object it was parsed from — a future event therefore survives a
    /// decode/encode round-trip with every key and value intact (though
    /// possibly reordered), which is what lets a recorder, a proxy, or
    /// `replay::to_jsonl` handle a stream it only partly understands without
    /// corrupting the part it does not.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            AgentEvent::Unknown { payload, .. } => payload.serialize(serializer),
            known => AgentEvent::serialize(known, serializer),
        }
    }
}

impl<'de> Deserialize<'de> for AgentEvent {
    /// Reads the `"type"` tag, then dispatches.
    ///
    /// An unrecognized tag becomes [`AgentEvent::Unknown`] with the whole
    /// original object preserved. Everything else — including an object with
    /// no `"type"` at all, or a non-string one — goes to the derived codec,
    /// which still rejects a body that does not fit its variant.
    ///
    /// The asymmetry is the point. A tag we have never seen is a newer stella
    /// talking to an older reader, which is normal and must not break the
    /// stream. A tag we *do* know carrying a body that does not fit is an
    /// encoder bug or a corrupt record, and laundering that into `Unknown`
    /// would convert a loud failure into silent data loss.
    ///
    /// Two things the [`Value`] hop costs, both invisible until you look for
    /// them (#672):
    ///
    ///  - **Duplicate keys stop being an error.** Deserializing a struct
    ///    directly rejects `{"type":"text","delta":"a","delta":"b"}` as a
    ///    duplicate field; a [`Value`] is a map, so the second key overwrites
    ///    the first and the derived codec only ever sees the last one. A
    ///    duplicated field is therefore last-wins rather than corruption
    ///    the reader refuses — narrower than the "a known tag with a bad body
    ///    is loud" doctrine above claims. Producers must not rely on it either
    ///    way: a stream with duplicate keys is malformed input this decoder
    ///    happens to survive, not a supported shape.
    ///  - **Errors lose their position.** The inner failure is a
    ///    `serde_json::Error` re-wrapped through `D::Error::custom`, so
    ///    "missing field `delta` at line 1 column 15" arrives as
    ///    "missing field `delta`". The *reason* survives; the offset into the
    ///    offending journal line does not.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error as _;

        let value = Value::deserialize(deserializer)?;
        match value.get("type").and_then(Value::as_str) {
            Some(tag) if !KNOWN_TYPE_TAGS.contains(&tag) => Ok(AgentEvent::Unknown {
                event_type: tag.to_owned(),
                payload: value,
            }),
            _ => AgentEvent::deserialize(value).map_err(D::Error::custom),
        }
    }
}

/// What happened to a file in a `FileChange` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    /// Content was successfully read — no mutation, never a diff. Rides the
    /// same event so the files-touched panel sees reads without a second
    /// data path.
    Read,
    /// The file did not exist before this change.
    Created,
    /// An existing file's contents changed.
    Modified,
    /// The file was removed.
    Deleted,
}

impl FileChangeKind {
    /// Whether this kind mutated the file — what the pipeline's zero-diff
    /// guard and inline transcript diffs key on. Reads are observability,
    /// not change.
    #[must_use]
    pub fn is_mutation(self) -> bool {
        !matches!(self, FileChangeKind::Read)
    }
}

/// Evidence backing a `Verdict`. `deterministic` distinguishes the
/// flip-oracle/tests ladder from a model verifier's opinion — the two are
/// never conflated (L-E11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct VerdictEvidence {
    /// One line naming what was checked and what it showed.
    pub summary: String,
    /// `true` when the verdict came from the deterministic ladder (a
    /// fail→pass flip of the same normalized test command, touched-tests
    /// green, diff budget) rather than a model verifier.
    pub deterministic: bool,
    /// Pointers to the underlying artifacts (`trace:t1#verify`, a test
    /// command, a diff), so a reader can go check the summary rather than
    /// take it on faith.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    /// The full ladder input snapshot this verdict was decided from (#865).
    /// `replay` answers "why did this run fast-submit / revise / verifier?"
    /// from here without re-deriving, and a verifier escalation renders it into
    /// the prompt (#864) so the verifier sees *why* the ladder was inconclusive
    /// rather than a diff cold. Absent on events recorded before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ladder: Option<Box<LadderSnapshot>>,
}

/// What a `ScopeReview` gate presents for approval before a large plan
/// executes (L-E5).
///
/// The fields after `estimated_cost_usd` are the scope-card grid's facts
/// (repo/branch, read/write globs, shell policy) — all additive
/// (`serde(default)`), so streams recorded before they existed parse with
/// every one absent, and a proposal that names none serializes exactly as it
/// always has.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ScopeProposal {
    /// One line describing the work, for the approval prompt's headline.
    pub summary: String,
    /// The plan's steps, in the order the worker will attempt them.
    pub steps: Vec<String>,
    /// How many files the plan expects to touch — the magnitude the gate's
    /// thresholds are compared against.
    pub estimated_files: u32,
    /// Projected spend, when the planner could estimate one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
    /// The repository the scope binds to (`owner/name`, or a workspace
    /// path), when the planner named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The branch the work lands on, when named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Path globs the plan intends to WRITE within. Empty = not stated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write_globs: Vec<String>,
    /// Path globs the plan reads beyond its write set. Empty = not stated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_globs: Vec<String>,
    /// The shell policy in force for the run (e.g. `allowlisted`,
    /// `read-only`, `none`), when stated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_policy: Option<String>,
}

/// What a `HunkReview` gate presents for per-hunk approval before a mutating
/// tool call writes anything (#1265).
///
/// The hunks are a **flat, ordered list across every file the call touches**,
/// not a per-file tree: the reviewer's answer is a set of indices into this
/// list, and one flat coordinate space is what keeps that answer unambiguous
/// when two files change in the same call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HunkProposal {
    /// Correlates the decision — and the synthetic `ToolResult` that clears the
    /// card — back to this review. Distinct from the model's tool-call id: one
    /// call raises one review, but the review is the host's object, not the
    /// model's.
    pub id: String,
    /// The tool whose write is being reviewed (`apply_edits`, `edit_file`,
    /// `write_file`) — the card names it so a reviewer knows what declining
    /// costs.
    pub tool: String,
    /// Every proposed hunk, in file-then-position order.
    pub hunks: Vec<ProposedHunk>,
}

/// One reviewable hunk: which file, what it does, and how it renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ProposedHunk {
    /// Workspace-relative path of the file this hunk changes.
    pub path: String,
    /// The hunk as unified-diff text, `@@` header included — ready for
    /// `stella_tui::diff::body_lines` with no further parsing.
    pub diff: String,
    /// Lines this hunk adds. Authoritative: taken from the decomposition, never
    /// re-counted from `diff` (which is capped and carries context lines).
    pub lines_added: u32,
    /// Lines this hunk removes, on the same terms.
    pub lines_removed: u32,
}

/// Which kind of media artifact a job produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// A raster image.
    Image,
    /// Vector artwork emitted as SVG source.
    Svg,
    /// A video clip — the only kind whose job is asynchronous and long-lived.
    Video,
}

/// Lifecycle of an async media job. `Failed` carries the reason inline —
/// a failed job must never be distinguishable only by the absence of a
/// success event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MediaJobState {
    /// Accepted by the provider, not yet started.
    Queued,
    /// Generation is under way.
    Running,
    /// The artifact landed; a [`AgentEvent::MediaComplete`] follows.
    Succeeded,
    /// Generation failed terminally.
    Failed {
        /// Why it failed, inline — a failed job is never signalled only by
        /// the absence of a success event.
        reason: String,
    },
}

/// A completed media artifact: id + kind + where it landed on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MediaArtifactRef {
    /// The artifact id, matching the `artifact_id` its
    /// [`AgentEvent::MediaProgress`] events carried.
    pub id: String,
    /// What was produced.
    pub kind: MediaKind,
    /// Path under `.stella/artifacts/` (the generation tools may never
    /// write outside it).
    pub path: String,
    /// Human label for citation/display.
    pub label: String,
}

/// A pull request's status as observed by the fleet monitor. Reconciled
/// against the live source before rendering, never served from cache
/// alone (L-V3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum PrStatus {
    /// Opened as a draft — not yet asking for review.
    Draft,
    /// Open and reviewable.
    Open,
    /// Merged into its base branch.
    Merged,
    /// Closed without merging.
    Closed,
}

/// Aggregate CI verdict for a PR's head commit, as observed by the
/// fleet monitor (`gh pr checks`). Reconciled against the live source
/// before rendering, never served from cache alone (L-V3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum CiStatus {
    /// Checks exist but none have started reporting.
    Pending,
    /// At least one check is still running and none have failed.
    Running,
    /// Every check reported and all of them succeeded.
    Passing,
    /// At least one check failed — terminal for this head commit.
    Failing,
}

/// One entry on the turn's task board (the `task_*` tools). The board is
/// session-scoped working state — what the agent has planned, is doing,
/// and has finished — mirrored to the store for cross-session findability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TaskItem {
    /// Stable per-session ordinal id ("1", "2", …) — what `task_complete`
    /// / `task_cancel` / `task_assign` reference.
    pub id: String,
    /// Imperative title ("Fix the auth redirect loop").
    pub subject: String,
    /// What needs to be done, if the creator elaborated beyond the subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Where the task is in its lifecycle ([`TaskStatus`]).
    pub status: TaskStatus,
    /// Which agent lane owns the task: `None` until claimed, `Some("lead")`
    /// for the lead, or the sub-agent lane id once `task_assign` spawned a
    /// dedicated worker for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
}

/// Lifecycle of a `TaskItem`. Terminal states are `Completed` and
/// `Cancelled`; a cancelled task keeps its row (the board is an audit
/// surface, not just a scheduler).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Created and not yet started.
    Pending,
    /// Claimed by a lane and being worked.
    InProgress,
    /// Finished successfully. Terminal.
    Completed,
    /// Abandoned. Terminal, and the row is kept — the board is an audit
    /// surface, not just a scheduler.
    Cancelled,
}

impl TaskStatus {
    /// Whether the task can still change state. Terminal tasks reject
    /// further transitions (enforced by the board logic in `stella-core`).
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, TaskStatus::Pending | TaskStatus::InProgress)
    }
}

#[cfg(test)]
mod tests;
