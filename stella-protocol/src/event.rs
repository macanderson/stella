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
    /// Planning: the ordered steps the worker is about to attempt.
    Plan,
    /// The interactive approval gate a large plan passes through (L-E5).
    ScopeReview,
    /// Witness authoring: before the worker executes, an independent model
    /// (the judge's resolution, never the worker's transcript) writes the
    /// witness test — a test that FAILS on the current code and will pass
    /// once the goal is met — arming the deterministic flip oracle (L-E11).
    /// The witness is visible to the worker (iterating against a failing
    /// test is where convergence comes from); integrity comes from tamper
    /// exclusion at verify time, not from hiding the test.
    Witness,
    /// The worker's own tool-calling loop — the steps that actually change
    /// the workspace.
    Execute,
    /// The deterministic verification ladder: the flip oracle, the touched
    /// tests, the diff budget.
    Verify,
    /// The model judge, reached only when the deterministic ladder came back
    /// inconclusive (L-E11).
    Judge,
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
    /// The model judge, reached only on inconclusive deterministic evidence.
    Judge,
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

/// One event in the turn's stream. Every stage boundary emits an event;
/// nothing user-visible is derived from internal state that isn't also in
/// this stream.
///
/// `remote = "Self"` keeps the derived codec as a pair of *inherent*
/// associated functions instead of the trait impls, so the hand-written
/// [`Serialize`]/[`Deserialize`] impls below can delegate to it after routing
/// [`AgentEvent::Unknown`] around it. Without that indirection the forward-
/// compat fallback would mean hand-writing a visitor for all 34 variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case", remote = "Self")]
pub enum AgentEvent {
    /// The turn entered a new pipeline stage. Every stage boundary emits one,
    /// in the order the pipeline walks them.
    Stage { name: StageKind },
    /// The step's answer text, in full — the authoritative, durable record.
    /// The field name `delta` is wire-frozen legacy and does NOT mean a
    /// fragment: the live fragments are [`AgentEvent::TextDelta`], which the
    /// journal deliberately drops. Consumers must REPLACE any accumulated
    /// preview with this value, never append it to one.
    Text { delta: String },
    /// One in-order fragment of the answer text, emitted live while the
    /// model call streams. Strictly a best-effort preview: the step's
    /// following `Text` event carries the full text and is authoritative —
    /// consumers must REPLACE any accumulated deltas with it, never merge
    /// (a retried model call re-streams its deltas from the start, so the
    /// accumulation can be garbled; there is no reset marker). Additive to
    /// the stream-json wire contract: consumers must tolerate `text_delta`
    /// lines appearing between events, and persistence layers may drop them
    /// (the `Text` event is the durable record).
    TextDelta { text: String },
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
        /// `"exact_repeat"` | `"short_cycle"` — mirrors
        /// `stella-core::loop_detect::LoopVerdict` (kept as a string here so
        /// `stella-protocol` never depends on `stella-core`).
        kind: String,
        /// Tool names of the repeated signature, in cycle order (one entry
        /// for an exact repeat).
        pattern: Vec<String>,
        /// Consecutive identical calls (exact repeat) or full cycles (short
        /// cycle) observed.
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
        /// The engine's RAW (uncalibrated) pre-call estimate of the input it
        /// sent — paired with `input_tokens` this is one drift sample, the
        /// feedback that calibrates future estimates per model
        /// (`stella-core::estimator::Calibration`). Raw by contract:
        /// consumers rebuild the correction from these pairs, and a
        /// corrected estimate here would compound the correction on every
        /// round trip. `0` means no estimate was taken (pre-drift emitters —
        /// hence `serde(default)`, so old streams still parse).
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
    },
    /// A judge model's assessment of a goal-driven loop after one working
    /// round. `met == true` ends the loop; `met == false` feeds `reasoning`
    /// back to the worker as course-correction. `cost_usd` is the judge
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
        /// The CGP usage report for this recall (`docs/context-reuse.md` §2):
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
        /// and the pipeline's triage/judge/plan/guidance roles — take 1, 2, …
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
    /// touched-tests-green) or the model judge (L-E11: deterministic-first;
    /// model judges handle only inconclusive evidence).
    JudgeVerdict {
        passed: bool,
        evidence: JudgeEvidence,
    },
    /// Interactive gate before large plans execute (L-E5): the pipeline
    /// pauses on this event and waits for approval above configured
    /// thresholds; headless requires a flag to bypass.
    ScopeReview { proposal: ScopeProposal },
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
    JudgeVerdict => "judge_verdict",
    ScopeReview => "scope_review",
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

/// Evidence backing a `JudgeVerdict`. `deterministic` distinguishes the
/// flip-oracle/tests ladder from a model judge's opinion — the two are
/// never conflated (L-E11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct JudgeEvidence {
    /// One line naming what was checked and what it showed.
    pub summary: String,
    /// `true` when the verdict came from the deterministic ladder (a
    /// fail→pass flip of the same normalized test command, touched-tests
    /// green, diff budget) rather than a model judge.
    pub deterministic: bool,
    /// Pointers to the underlying artifacts (`trace:t1#verify`, a test
    /// command, a diff), so a reader can go check the summary rather than
    /// take it on faith.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    /// The full ladder input snapshot this verdict was decided from (#865).
    /// `replay` answers "why did this run fast-submit / revise / judge?"
    /// from here without re-deriving, and a judge escalation renders it into
    /// the prompt (#864) so the judge sees *why* the ladder was inconclusive
    /// rather than a diff cold. Absent on events recorded before it existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ladder: Option<Box<LadderSnapshot>>,
}

/// One flip-oracle observation, in the order the pipeline made it — together
/// they are the oracle trace a verdict carries (#864).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OracleObservation {
    /// Which tree the observation ran against.
    pub tree: ProofTree,
    /// Whether the tracked command's assertions passed. Infra outcomes never
    /// appear here — an unobservable run is not an oracle observation.
    pub passed: bool,
}

/// The deterministic evidence the ladder decided a verdict from, snapshotted
/// at decision time (#865). Everything here existed when the decision was
/// made; attaching it to the verdict is what makes "why?" answerable later
/// without re-deriving — and re-deriving is exactly what a replay of an
/// event stream cannot do, because the world the probes read is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LadderSnapshot {
    /// The normalized test command the flip oracle locked onto, when it
    /// armed at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracked_command: Option<String>,
    /// The oracle's observations in order (baseline, candidate runs, the
    /// pre-submit confirmation). Infra runs are absent by construction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub oracle_trace: Vec<OracleObservation>,
    /// Whether the oracle's flip was achieved — after the confirmation run,
    /// so an unconfirmed flip reads `false` here with `unstable_flip: true`.
    pub flip_achieved: bool,
    /// A flip was observed but its confirmation re-run did not pass (#859).
    pub unstable_flip: bool,
    /// A would-be flip was refused because the passing run named its tests
    /// and none of the baseline's failing tests were among them — the pass
    /// demonstrably fixed a *different* failure (#867), most concretely a
    /// deleted or renamed failing test. `serde(default)` so pre-#867
    /// snapshots keep parsing.
    #[serde(default)]
    pub flip_refused_different_failure: bool,
    /// Touched-tests result: `None` is "could not be observed", not a pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub touched_tests_passed: Option<bool>,
    /// Why the test run observed nothing, when it didn't (`timed_out`,
    /// `infra_failure`) — the #860 distinction between "the suite failed"
    /// and "the suite could not be watched".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_infra: Option<String>,
    /// Lines changed, and the budget they were judged against.
    pub diff_lines: u32,
    pub diff_budget: u32,
    /// Whether the diff probe could read the working tree at all.
    pub diff_available: bool,
    /// Mutating file touches the recorder observed.
    pub file_change_events: u32,
    /// Dispatched tool calls capable of changing the workspace.
    pub mutating_actions: u32,
    /// New lint/typecheck errors/warnings over the pre-execution baseline
    /// (#861); zeros when the probe was unavailable or never consulted.
    pub new_diag_errors: u32,
    pub new_diag_warnings: u32,
    /// The witness-tamper check's result: `None` when no witness was armed,
    /// `Some(true)` when every witness artifact matched its pinned identity.
    /// `Some(false)` never reaches a verdict — tampering aborts the
    /// candidate — so its presence here is the *stated* proof the check ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness_intact: Option<bool>,
}

/// Which code state a [`ProofStep::Oracle`] observation was made against.
///
/// The distinction is the whole content of a flip: the same command failing in
/// `Baseline` and passing in `Candidate` is proof, while either result twice
/// against one tree is a tree observed twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProofTree {
    /// The pre-execution tree — the code as it was before this turn touched it.
    Baseline,
    /// The executed tree — the code as this turn left it.
    Candidate,
}

/// One step of the proof a turn builds for its own work, in the order the
/// pipeline makes the observation. Carried by [`AgentEvent::Proof`].
///
/// Additive to the wire contract in both directions: an older reader sees the
/// whole event as [`AgentEvent::Unknown`], and a reader that knows `Proof` but
/// not a future step tags it `Unknown` at the step level rather than guessing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProofStep {
    /// What assurance this turn is going to buy, stated by triage **before**
    /// any of it happens.
    ///
    /// Emitted first, and the reason the rail can be honest at all. Every
    /// other step reports something that *did* happen, so a turn where the
    /// answer is "we decided not to" produced no steps and left the surface
    /// with nothing to say — which is exactly the case that dominates in
    /// practice. A declared plan turns that silence into a statement: the
    /// witness row reads "waived by triage" from the first second of the
    /// turn instead of implying a test is still coming.
    Assurance {
        /// Whether an independently authored witness test was called for.
        witness: bool,
        /// Whether a model judge was called for on inconclusive evidence.
        judge: bool,
    },
    /// The warrant read the diff and answered "does this change need a test".
    /// Emitted once per candidate, before any witness is bought — a change
    /// with nothing to prove is a *stated* outcome here, never silence.
    Warrant {
        required: bool,
        /// The stated reason when no test is warranted; `None` when one is.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// Size of the change the answer was read from.
        diff_lines: u32,
    },
    /// An independent model authored the failing witness, and its bytes are
    /// pinned: any later change to `path` fails the candidate closed.
    WitnessAuthored {
        path: String,
        /// The command that arms the flip oracle.
        command: String,
        /// The accepted artifact's fingerprint — what tamper exclusion compares
        /// against for the rest of the run.
        fingerprint: String,
    },
    /// A warranted witness could not be *produced* (no independent author, an
    /// author that got stuck, a failed graft). The work stands; it is simply
    /// unproven, and saying so is the point.
    WitnessUnavailable { reason: String },
    /// **Every** evidence channel was blind, so the ladder abstained rather
    /// than judging: no flip oracle, no test result, a working tree the diff
    /// probe could not read, and no recorded file change.
    ///
    /// Distinct from a failing verdict, and the distinction is the point. The
    /// turn this exists for ended with a judge asserting a file "likely does
    /// not exist" while the file sat in the container — a claim about the
    /// *instruments* delivered as a claim about the *work* (#973). Without a
    /// step of its own, an abstention reaches the rail as `✓ passed · model
    /// judge`, which is the same silent outcome in the other direction.
    VerificationUnavailable { reason: String },
    /// The flip oracle observed one run of the tracked command against one
    /// tree. A fail in `Baseline` followed by a pass in `Candidate` is the
    /// flip; anything else is not.
    Oracle {
        command: String,
        passed: bool,
        tree: ProofTree,
    },
}

/// What a `ScopeReview` gate presents for approval before a large plan
/// executes (L-E5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
mod tests {
    use super::*;

    #[test]
    fn agent_event_roundtrips_with_type_tag() {
        let event = AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "call_1".into(),
                name: "read_file".into(),
                input: serde_json::json!({ "path": "src/main.rs" }),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"tool_start\""), "{json}");
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            AgentEvent::ToolStart { call } => assert_eq!(call.name, "read_file"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn text_delta_roundtrips_with_its_own_type_tag() {
        // `text_delta` is additive on the wire: a distinct tag from `text`,
        // so a pre-delta consumer that skips unknown lines keeps parsing the
        // authoritative `text` events unchanged.
        let event = AgentEvent::TextDelta { text: "Hel".into() };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"text_delta\""), "{json}");
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            AgentEvent::TextDelta { text } => assert_eq!(text, "Hel"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn tool_result_roundtrips_and_streams_without_speculated_still_parse() {
        // Round-trip with the flag set.
        let event = AgentEvent::ToolResult {
            call_id: "call_1".into(),
            output: ToolOutput::Ok {
                content: "x".into(),
            },
            duration_ms: 42,
            speculated: true,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            AgentEvent::ToolResult { speculated, .. } => assert!(speculated),
            other => panic!("unexpected variant: {other:?}"),
        }

        // A stream recorded BEFORE the field existed must still parse, with
        // the safe default (not speculated).
        let old = r#"{"type":"tool_result","call_id":"c","output":{"ok":{"content":""}},"duration_ms":1}"#;
        match serde_json::from_str::<AgentEvent>(old) {
            Ok(AgentEvent::ToolResult { speculated, .. }) => {
                assert!(!speculated, "missing field must default to false")
            }
            other => panic!("old stream must parse: {other:?}"),
        }
    }

    #[test]
    fn speculation_discarded_roundtrips_and_names_the_reason() {
        let event = AgentEvent::SpeculationDiscarded {
            call_id: "c1".into(),
            name: "read_file".into(),
            reason: "attempt_failed".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            json.contains("\"type\":\"speculation_discarded\""),
            "{json}"
        );
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            AgentEvent::SpeculationDiscarded {
                call_id,
                name,
                reason,
            } => {
                assert_eq!(call_id, "c1");
                assert_eq!(name, "read_file");
                assert_eq!(reason, "attempt_failed");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn budget_tick_roundtrips_with_session_axis() {
        let event = AgentEvent::BudgetTick {
            spent_usd: 0.42,
            limit_usd: Some(2.5),
            mode: BudgetMode::Enforced,
            session_spent_usd: Some(1.75),
            session_limit_usd: Some(10.0),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            AgentEvent::BudgetTick {
                spent_usd,
                limit_usd,
                mode,
                session_spent_usd,
                session_limit_usd,
            } => {
                assert_eq!(spent_usd, 0.42);
                assert_eq!(limit_usd, Some(2.5));
                assert_eq!(mode, BudgetMode::Enforced);
                assert_eq!(session_spent_usd, Some(1.75));
                assert_eq!(session_limit_usd, Some(10.0));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn budget_tick_without_session_fields_parses_with_none() {
        // A stream recorded BEFORE the session axis existed must still parse,
        // with both new fields defaulting to `None` (not `0.0`, which would
        // read as a real "spent nothing").
        let old = r#"{"type":"budget_tick","spent_usd":0.42,"limit_usd":2.5,"mode":"enforced"}"#;
        match serde_json::from_str::<AgentEvent>(old) {
            Ok(AgentEvent::BudgetTick {
                session_spent_usd,
                session_limit_usd,
                ..
            }) => {
                assert_eq!(session_spent_usd, None);
                assert_eq!(session_limit_usd, None);
            }
            other => panic!("old stream must parse: {other:?}"),
        }
    }

    #[test]
    fn compaction_event_carries_counts_and_block_identities() {
        let event = AgentEvent::Compaction {
            before_tokens: 10_000,
            after_tokens: 4_000,
            evicted: 2,
            deduped: 1,
            superseded: 1,
            aged: 1,
            summarized: 3,
            evicted_blocks: vec!["blk_ev1".into(), "blk_ev2".into()],
            deduped_blocks: vec!["blk_dd1".into()],
            superseded_blocks: vec!["blk_sup".into()],
            aged_blocks: vec!["blk_age".into()],
            // Fewer identities than the `summarized` count: the summary folded
            // three messages but only two were identity-bearing tool results.
            summarized_blocks: vec!["blk_sum1".into(), "blk_sum2".into()],
            effective_budget_tokens: 136_363,
            calibration_factor: 1.1,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"compaction\""), "{json}");
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            AgentEvent::Compaction {
                before_tokens,
                after_tokens,
                evicted_blocks,
                summarized,
                summarized_blocks,
                effective_budget_tokens,
                ..
            } => {
                assert!(after_tokens < before_tokens);
                // Identities, not just counts — which blocks left context.
                assert_eq!(evicted_blocks, vec!["blk_ev1", "blk_ev2"]);
                // The summary names its folded tool-result blocks, and the vec
                // may be shorter than the message count it replaced.
                assert_eq!(summarized_blocks, vec!["blk_sum1", "blk_sum2"]);
                assert!(summarized_blocks.len() < summarized);
                assert_eq!(effective_budget_tokens, 136_363);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn legacy_compaction_without_identities_still_parses() {
        // A journal written before §6.2 has counts but no identity fields; it
        // must still deserialize (additive contract), with empty identity vecs.
        let old =
            r#"{"type":"compaction","before_tokens":9,"after_tokens":4,"evicted":1,"deduped":0}"#;
        match serde_json::from_str::<AgentEvent>(old).unwrap() {
            AgentEvent::Compaction {
                evicted,
                evicted_blocks,
                effective_budget_tokens,
                ..
            } => {
                assert_eq!(evicted, 1);
                assert!(evicted_blocks.is_empty());
                assert_eq!(effective_budget_tokens, 0);
            }
            other => panic!("old compaction must parse: {other:?}"),
        }
    }

    #[test]
    fn provider_fallback_is_never_silent_it_names_both_ends() {
        let event = AgentEvent::ProviderFallback {
            from: "zai".into(),
            to: "anthropic".into(),
            reason: "circuit breaker open after 3 consecutive transport failures".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"from\":\"zai\""), "{json}");
        assert!(json.contains("\"to\":\"anthropic\""), "{json}");
    }

    #[test]
    fn file_change_carries_the_delta_and_the_diff_on_the_single_event_path() {
        let event = AgentEvent::FileChange {
            path: "src/lib.rs".into(),
            kind: FileChangeKind::Modified,
            added: 12,
            removed: 3,
            diff: Some("@@ -1 +1 @@\n-old\n+new".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"file_change\""), "{json}");
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            AgentEvent::FileChange {
                kind,
                added,
                removed,
                diff,
                ..
            } => {
                assert_eq!(kind, FileChangeKind::Modified);
                assert_eq!(
                    (added, removed),
                    (12, 3),
                    "the recorder's counts survive the wire — consumers must \
                     not have to recount the diff text"
                );
                assert!(diff.is_some());
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    /// Journals written before the counts existed must still replay. They
    /// recorded no delta, so they come back as `0/0` — the honest answer for a
    /// stream that never measured one.
    #[test]
    fn a_file_change_without_counts_still_parses() {
        let old = r#"{"type":"file_change","path":"a.rs","kind":"modified","diff":null}"#;
        match serde_json::from_str::<AgentEvent>(old).unwrap() {
            AgentEvent::FileChange {
                path,
                added,
                removed,
                ..
            } => {
                assert_eq!(path, "a.rs");
                assert_eq!((added, removed), (0, 0));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn read_kind_serializes_and_is_the_only_non_mutation() {
        assert_eq!(
            serde_json::to_string(&FileChangeKind::Read).unwrap(),
            "\"read\""
        );
        let back: FileChangeKind = serde_json::from_str("\"read\"").unwrap();
        assert_eq!(back, FileChangeKind::Read);
        assert!(!FileChangeKind::Read.is_mutation());
        for kind in [
            FileChangeKind::Created,
            FileChangeKind::Modified,
            FileChangeKind::Deleted,
        ] {
            assert!(kind.is_mutation(), "{kind:?} is a mutation");
        }
    }

    #[test]
    fn context_recall_frames_always_carry_a_citation_label() {
        let event = AgentEvent::ContextRecall {
            frames: vec![ContextFrameRef {
                id: None, // not-yet-materialized frames carry no id (L-C4)
                citation_label: "engine step-driver (driver.rs)".into(),
                provider: "code-graph".into(),
                source: "code-graph".into(),
                kind: "symbol".into(),
                uri: Some("file:///repo/stella-core/src/driver.rs".into()),
                method: Some("tree-sitter/symbol-extract".into()),
                token_cost: 120,
                block_id: None,
                content_digest: None,
            }],
            provider_mix: vec![ProviderShare {
                provider: "code-graph".into(),
                frames: 1,
            }],
            tokens: 120,
            usage: None,
            latency_ms: 0,
            used_ann_index: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("citation_label"), "{json}");
        assert!(
            !json.contains("\"id\""),
            "absent id must be omitted: {json}"
        );
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            AgentEvent::ContextRecall { frames, .. } => {
                let frame = &frames[0];
                assert_eq!(frame.provider, "code-graph");
                assert_eq!(frame.source, "code-graph");
                assert_eq!(frame.kind, "symbol");
                assert_eq!(
                    frame.uri.as_deref(),
                    Some("file:///repo/stella-core/src/driver.rs")
                );
                assert_eq!(frame.method.as_deref(), Some("tree-sitter/symbol-extract"));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    /// WITNESS (#452): the usage-report envelope rides the recall event and
    /// round-trips byte-for-byte (AGENTS.md invariant 4), so context cost is a
    /// meterable record rather than a number that dies with the turn.
    #[test]
    fn context_usage_report_round_trips_and_stays_content_free() {
        let usage = ContextUsage {
            budget_requested: 1200,
            budget_consumed: 210,
            as_of: "2026-07-24T00:00:00Z".into(),
            providers: vec![
                ContextProviderUsage {
                    provider_id: "workspace-memory".into(),
                    frames_served: 2,
                    frames_rejected: 0,
                    token_cost: 90,
                },
                ContextProviderUsage {
                    provider_id: "code-graph".into(),
                    frames_served: 1,
                    frames_rejected: 3,
                    token_cost: 120,
                },
            ],
        };
        assert!(
            usage.is_consistent(),
            "budget_consumed must re-sum from the per-provider costs"
        );
        assert_eq!(usage.total_frames_served(), 3);

        let event = AgentEvent::ContextRecall {
            frames: vec![],
            provider_mix: vec![],
            tokens: 210,
            usage: Some(usage.clone()),
            latency_ms: 0,
            used_ann_index: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        match &back {
            AgentEvent::ContextRecall { usage: round, .. } => {
                assert_eq!(round.as_ref(), Some(&usage), "byte-for-byte round trip")
            }
            other => panic!("unexpected variant: {other:?}"),
        }
        assert_eq!(
            json,
            serde_json::to_string(&back).unwrap(),
            "re-serialization must be byte-identical"
        );

        // Content-free by construction: the envelope carries provider ids,
        // counts, costs, and a timestamp — never frame text, titles, URIs, or
        // query text (AGENTS.md invariant 3, #466).
        let value = serde_json::to_value(&usage).unwrap();
        let mut fields: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
        fields.sort();
        assert_eq!(
            fields,
            ["as_of", "budget_consumed", "budget_requested", "providers"],
            "no field may be added to the usage envelope without a content review"
        );
    }

    /// An inconsistent total is *checkable*, never a silent misbill (§2).
    #[test]
    fn a_tampered_usage_total_fails_the_arithmetic_identity() {
        let usage = ContextUsage {
            budget_requested: 1200,
            budget_consumed: 999,
            as_of: "2026-07-24T00:00:00Z".into(),
            providers: vec![ContextProviderUsage {
                provider_id: "workspace-memory".into(),
                frames_served: 1,
                frames_rejected: 0,
                token_cost: 90,
            }],
        };
        assert!(!usage.is_consistent());
    }

    /// The accounting clock is a bare `String` (#568), so a receipt whose
    /// `as_of` is junk sorts wrong in whatever tool eventually reads it. The
    /// shape is checkable here, at the type, rather than three crates away.
    #[test]
    fn as_of_wellformedness_is_checkable_on_the_receipt() {
        let usage_at = |as_of: &str| ContextUsage {
            budget_requested: 1200,
            budget_consumed: 0,
            as_of: as_of.into(),
            providers: vec![],
        };

        // The shape every host in this workspace stamps, plus the variants
        // RFC 3339 also permits — rejecting these would make the predicate a
        // false-alarm generator on valid receipts.
        for good in [
            "2026-07-24T00:00:00Z",
            "2026-07-24t00:00:00z",
            "2026-07-24T00:00:00.123456Z",
            "2026-07-24T00:00:00+00:00",
            "2026-07-24T12:30:59.5-05:00",
        ] {
            assert!(usage_at(good).as_of_is_wellformed(), "{good}");
        }

        // Missing the offset, wrong separators, a truncated prefix, junk, and
        // the empty string a `Default`-ish construction would leave behind.
        for bad in [
            "2026-07-24T00:00:00",
            "2026-07-24 00:00:00Z",
            "2026/07/24T00:00:00Z",
            "2026-07-24T00:00:00.Z",
            "2026-07-24T00:00:00+0000",
            "24 July 2026",
            "",
        ] {
            assert!(!usage_at(bad).as_of_is_wellformed(), "{bad}");
        }
    }

    /// A recall event recorded before the usage report existed must still
    /// deserialize — the additive contract.
    #[test]
    fn a_recall_event_without_a_usage_report_still_parses() {
        let legacy = r#"{"type":"context_recall","frames":[],"provider_mix":[],"tokens":0}"#;
        match serde_json::from_str::<AgentEvent>(legacy).unwrap() {
            AgentEvent::ContextRecall { usage, .. } => assert!(usage.is_none()),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn context_recall_from_a_pre_provenance_stream_still_parses() {
        let legacy = r#"{"type":"context_recall","frames":[{"citation_label":"driver.rs","source":"code-graph","token_cost":12}],"provider_mix":[{"provider":"code-graph","frames":1}],"tokens":12}"#;
        match serde_json::from_str::<AgentEvent>(legacy) {
            Ok(AgentEvent::ContextRecall { frames, .. }) => {
                let frame = &frames[0];
                assert!(frame.provider.is_empty());
                assert_eq!(frame.source, "code-graph");
                assert!(frame.kind.is_empty());
                assert_eq!(frame.uri, None);
                assert_eq!(frame.method, None);
            }
            other => panic!("old stream must parse: {other:?}"),
        }
    }

    #[test]
    fn media_job_failure_carries_its_reason_inline() {
        let state = MediaJobState::Failed {
            reason: "provider rejected the prompt".into(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let back: MediaJobState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, state);
    }

    #[test]
    fn judge_verdict_distinguishes_deterministic_from_model_evidence() {
        let event = AgentEvent::JudgeVerdict {
            passed: true,
            evidence: JudgeEvidence {
                summary: "flip oracle: fail→pass on `cargo test -p x`".into(),
                deterministic: true,
                evidence_refs: vec!["trace:t1#verify".into()],
                ladder: None,
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(
            !json.contains("ladder"),
            "an absent snapshot must not serialize a null field"
        );
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            AgentEvent::JudgeVerdict { evidence, .. } => assert!(evidence.deterministic),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    /// #865 wire compatibility, both directions: a verdict recorded before
    /// snapshots existed parses (`ladder` absent → `None`), and a snapshot
    /// roundtrips with its oracle trace intact.
    #[test]
    fn ladder_snapshot_is_additive_and_roundtrips() {
        let legacy = r#"{"type":"judge_verdict","passed":true,
            "evidence":{"summary":"ok","deterministic":true}}"#;
        let back: AgentEvent = serde_json::from_str(legacy).unwrap();
        match back {
            AgentEvent::JudgeVerdict { evidence, .. } => assert!(evidence.ladder.is_none()),
            other => panic!("unexpected variant: {other:?}"),
        }

        let event = AgentEvent::JudgeVerdict {
            passed: true,
            evidence: JudgeEvidence {
                summary: "flip + confirmation".into(),
                deterministic: true,
                evidence_refs: vec![],
                ladder: Some(Box::new(LadderSnapshot {
                    tracked_command: Some("cargo test -p x".into()),
                    oracle_trace: vec![
                        OracleObservation {
                            tree: ProofTree::Baseline,
                            passed: false,
                        },
                        OracleObservation {
                            tree: ProofTree::Candidate,
                            passed: true,
                        },
                    ],
                    flip_achieved: true,
                    unstable_flip: false,
                    flip_refused_different_failure: false,
                    touched_tests_passed: Some(true),
                    test_infra: None,
                    diff_lines: 12,
                    diff_budget: 400,
                    diff_available: true,
                    file_change_events: 2,
                    mutating_actions: 3,
                    new_diag_errors: 0,
                    new_diag_warnings: 0,
                    witness_intact: Some(true),
                })),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            AgentEvent::JudgeVerdict { evidence, .. } => {
                let snapshot = evidence.ladder.expect("snapshot survives the wire");
                assert_eq!(snapshot.oracle_trace.len(), 2);
                assert!(snapshot.flip_achieved);
                assert_eq!(snapshot.tracked_command.as_deref(), Some("cargo test -p x"));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn ask_user_roundtrips_and_carries_structured_options() {
        let event = AgentEvent::AskUser {
            id: "call_q1".into(),
            question: "Which database should the migration target?".into(),
            options: vec!["local (5433)".into(), "staging".into()],
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"ask_user\""), "{json}");
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            AgentEvent::AskUser { options, .. } => assert_eq!(options.len(), 2),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn scope_review_and_pr_events_roundtrip() {
        for event in [
            AgentEvent::ScopeReview {
                proposal: ScopeProposal {
                    summary: "refactor the auth module".into(),
                    steps: vec!["step 1".into(), "step 2".into()],
                    estimated_files: 12,
                    estimated_cost_usd: Some(1.25),
                },
            },
            AgentEvent::Pr {
                url: "https://github.com/x/y/pull/1".into(),
                status: PrStatus::Open,
                number: Some(1),
                ci: Some(CiStatus::Running),
            },
            AgentEvent::Commit {
                sha: "abc123".into(),
                message: "feat: x".into(),
            },
        ] {
            let json = serde_json::to_string(&event).unwrap();
            let _back: AgentEvent = serde_json::from_str(&json).unwrap();
        }
    }

    #[test]
    fn pr_event_from_a_pre_ci_stream_still_parses() {
        // Backward compatibility: a `pr` line serialized before `number`
        // and `ci` existed must deserialize with both absent — absent ci
        // means "not polled yet", never "passing".
        let legacy = r#"{"type":"pr","url":"https://github.com/x/y/pull/183","status":"open"}"#;
        match serde_json::from_str::<AgentEvent>(legacy) {
            Ok(AgentEvent::Pr { number, ci, .. }) => {
                assert_eq!(number, None);
                assert_eq!(ci, None);
            }
            other => panic!("old stream must parse: {other:?}"),
        }
    }

    #[test]
    fn task_update_roundtrips_a_full_board_snapshot() {
        let event = AgentEvent::TaskUpdate {
            tasks: vec![
                TaskItem {
                    id: "1".into(),
                    subject: "Map the auth module".into(),
                    description: None,
                    status: TaskStatus::Completed,
                    owner: Some("lead".into()),
                },
                TaskItem {
                    id: "2".into(),
                    subject: "Fix the redirect loop".into(),
                    description: Some("token refresh races the redirect".into()),
                    status: TaskStatus::InProgress,
                    owner: Some("sub:2".into()),
                },
                TaskItem {
                    id: "3".into(),
                    subject: "Add a witness test".into(),
                    description: None,
                    status: TaskStatus::Pending,
                    owner: None,
                },
            ],
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"task_update\""), "{json}");
        // Absent optionals are omitted, not serialized as null.
        assert!(!json.contains("null"), "{json}");
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            AgentEvent::TaskUpdate { tasks } => {
                assert_eq!(tasks.len(), 3);
                assert_eq!(tasks[1].status, TaskStatus::InProgress);
                assert_eq!(tasks[1].owner.as_deref(), Some("sub:2"));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn task_status_open_vs_terminal() {
        assert!(TaskStatus::Pending.is_open());
        assert!(TaskStatus::InProgress.is_open());
        assert!(!TaskStatus::Completed.is_open());
        assert!(!TaskStatus::Cancelled.is_open());
    }

    #[test]
    fn stream_json_is_one_line_per_event() {
        let events = [
            AgentEvent::Stage {
                name: StageKind::Triage,
            },
            AgentEvent::Text { delta: "hi".into() },
            AgentEvent::Complete {
                model: "glm-5.2".into(),
                cost_usd: 0.001,
            },
        ];
        let lines: Vec<String> = events
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        for line in &lines {
            assert!(!line.contains('\n'));
        }
    }

    #[test]
    fn step_usage_roundtrips_as_a_complete_metering_record() {
        let event = AgentEvent::StepUsage {
            step: 3,
            role: ModelCallRole::Plan,
            provider: "zai".into(),
            output_text: Some(r#"["inspect", "patch"]"#.into()),
            model: "glm-5.2".into(),
            input_tokens: 12_000,
            output_tokens: 450,
            cached_input_tokens: 9_000,
            cache_write_tokens: 2_500,
            estimated_input_tokens: 11_200,
            cost_usd: 0.0042,
            duration_ms: 1_830,
            retries: 1,
            tool_calls: 4,
            complete: true,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"step_usage\""), "{json}");
        assert!(json.contains("\"role\":\"plan\""), "{json}");
        assert!(json.contains("\"cache_write_tokens\":2500"), "{json}");
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        match back {
            AgentEvent::StepUsage {
                step,
                role,
                output_text,
                cached_input_tokens,
                cache_write_tokens,
                estimated_input_tokens,
                retries,
                tool_calls,
                ..
            } => {
                assert_eq!(step, 3);
                assert_eq!(role, ModelCallRole::Plan);
                assert_eq!(output_text.as_deref(), Some(r#"["inspect", "patch"]"#));
                assert_eq!(cached_input_tokens, 9_000);
                assert_eq!(cache_write_tokens, 2_500);
                assert_eq!(estimated_input_tokens, 11_200);
                assert_eq!(retries, 1);
                assert_eq!(tool_calls, 4);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn step_usage_from_a_pre_drift_stream_still_parses() {
        // Backward compatibility: a `step_usage` line serialized before
        // `estimated_input_tokens` existed must deserialize with the field
        // defaulting to 0 ("no estimate was taken") — the stream-json wire
        // format is versioned by being additive-only.
        let legacy = r#"{"type":"step_usage","step":3,"model":"glm-5.2","input_tokens":12000,
            "output_tokens":450,"cached_input_tokens":9000,"cost_usd":0.0042,
            "duration_ms":1830,"retries":1,"tool_calls":4}"#;
        let back: AgentEvent = serde_json::from_str(legacy).unwrap();
        match back {
            AgentEvent::StepUsage {
                role,
                output_text,
                estimated_input_tokens,
                input_tokens,
                ..
            } => {
                assert_eq!(estimated_input_tokens, 0);
                assert_eq!(role, ModelCallRole::Unknown);
                assert_eq!(output_text, None);
                assert_eq!(input_tokens, 12_000);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn step_usage_from_a_pre_cache_write_stream_still_parses() {
        // Backward compatibility: a `step_usage` line serialized before
        // `cache_write_tokens` existed (but after `estimated_input_tokens`)
        // must deserialize with the field defaulting to 0 ("provider
        // reported no cache writes") — the additive-only wire contract.
        let legacy = r#"{"type":"step_usage","step":3,"model":"glm-5.2","input_tokens":12000,
            "output_tokens":450,"cached_input_tokens":9000,"estimated_input_tokens":11200,
            "cost_usd":0.0042,"duration_ms":1830,"retries":1,"tool_calls":4}"#;
        let back: AgentEvent = serde_json::from_str(legacy).unwrap();
        match back {
            AgentEvent::StepUsage {
                cache_write_tokens,
                cached_input_tokens,
                estimated_input_tokens,
                ..
            } => {
                assert_eq!(cache_write_tokens, 0);
                assert_eq!(cached_input_tokens, 9_000);
                assert_eq!(estimated_input_tokens, 11_200);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn goal_verdict_roundtrips_both_outcomes() {
        for met in [true, false] {
            let event = AgentEvent::GoalVerdict {
                round: 2,
                met,
                reasoning: "tests now pass".into(),
                cost_usd: 0.001,
            };
            let json = serde_json::to_string(&event).unwrap();
            assert!(json.contains("\"type\":\"goal_verdict\""), "{json}");
            let back: AgentEvent = serde_json::from_str(&json).unwrap();
            match back {
                AgentEvent::GoalVerdict { met: b, round, .. } => {
                    assert_eq!(b, met);
                    assert_eq!(round, 2);
                }
                other => panic!("unexpected variant: {other:?}"),
            }
        }
    }

    #[test]
    fn step_usage_preserves_call_identity_and_completeness() {
        let json = r#"{"type":"step_usage","step":3,"role":"plan_repair","provider":"anthropic","model":"claude-sonnet-4-5","input_tokens":12000,"output_tokens":300,"cached_input_tokens":9000,"cache_write_tokens":12,"estimated_input_tokens":11000,"cost_usd":0.09,"duration_ms":1400,"retries":1,"tool_calls":0,"complete":true}"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        let roundtrip = serde_json::to_value(event).unwrap();
        assert_eq!(roundtrip["role"], "plan_repair");
        assert_eq!(roundtrip["provider"], "anthropic");
        assert_eq!(roundtrip["complete"], true);
    }

    #[test]
    fn legacy_step_usage_without_completeness_fails_closed() {
        let legacy = r#"{"type":"step_usage","step":1,"model":"old","input_tokens":10,"output_tokens":2,"cached_input_tokens":0,"cost_usd":0.01,"duration_ms":10,"retries":0,"tool_calls":0}"#;
        let event: AgentEvent = serde_json::from_str(legacy).unwrap();
        let roundtrip = serde_json::to_value(event).unwrap();
        assert_eq!(roundtrip["complete"], false);
    }

    #[test]
    fn usage_incomplete_is_a_closed_content_free_signal() {
        let json = r#"{"type":"usage_incomplete","role":"judge","provider":"anthropic","model":"claude-sonnet-4-5","reason":"timeout","duration_ms":2500,"retries":null}"#;
        let event: AgentEvent = serde_json::from_str(json).unwrap();
        let roundtrip = serde_json::to_value(event).unwrap();
        assert_eq!(roundtrip["type"], "usage_incomplete");
        assert_eq!(roundtrip["reason"], "timeout");
        assert_eq!(roundtrip.as_object().unwrap().len(), 7);
    }

    #[test]
    fn block_registered_carries_bytes_only_for_gap_kinds() {
        // Journal-resolvable kinds (tool I/O, assistant text) carry NO content —
        // their preimage is resolved from the originating event, never re-stored.
        let tool = AgentEvent::BlockRegistered {
            block_id: "blk_0123456789abcdef01234567".into(),
            kind: BlockKind::ToolResult,
            origin: BlockOrigin {
                turn_instance: 2,
                step: 5,
                call_id: Some("call_9".into()),
                memory_id: None,
            },
            token_cost: 480,
            content_digest: "sha256:deadbeef".into(),
            citation_label: None,
            content: None,
        };
        let value = serde_json::to_value(&tool).unwrap();
        assert_eq!(value["type"], "block_registered");
        assert_eq!(value["kind"], "tool_result");
        assert_eq!(value["origin"]["call_id"], "call_9");
        assert!(
            value.get("content").is_none() && value.get("output").is_none(),
            "a journal-resolvable block must not carry payload bytes: {value}"
        );

        // Gap kinds the journal cannot resolve (the system prefix, the assembled
        // user message) DO carry their bytes — local-only, stripped on export —
        // so the step stays reconstructable (spec §5.3).
        let system = AgentEvent::BlockRegistered {
            block_id: "blk_sys0000000000000000000".into(),
            kind: BlockKind::SystemPrefix,
            origin: BlockOrigin {
                turn_instance: 0,
                step: 0,
                call_id: None,
                memory_id: None,
            },
            token_cost: 300,
            content_digest: "sha256:beef".into(),
            citation_label: None,
            content: Some("you are a careful engineer".into()),
        };
        let value = serde_json::to_value(&system).unwrap();
        assert_eq!(value["content"], "you are a careful engineer");

        let back: AgentEvent = serde_json::from_str(&value.to_string()).unwrap();
        match back {
            AgentEvent::BlockRegistered { kind, content, .. } => {
                assert_eq!(kind, BlockKind::SystemPrefix);
                assert_eq!(content.as_deref(), Some("you are a careful engineer"));
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn step_manifest_preserves_block_order_and_the_effective_budget() {
        let event = AgentEvent::StepManifest {
            turn_instance: 1,
            step: 3,
            call_seq: 0,
            role: ModelCallRole::Worker,
            provider: "anthropic".into(),
            model: "claude-opus".into(),
            blocks: vec![
                ManifestEntry {
                    block_id: "blk_sys".into(),
                    cache_zone: CacheZone::StablePrefix,
                    token_cost: 1200,
                    resident_since_step: 0,
                    message_index: 0,
                    call_id: None,
                },
                ManifestEntry {
                    block_id: "blk_tail".into(),
                    cache_zone: CacheZone::Volatile,
                    token_cost: 90,
                    resident_since_step: 3,
                    message_index: 3,
                    call_id: Some("call_7".into()),
                },
            ],
            effective_budget_tokens: 136_363,
            calibration_factor: 1.1,
            estimated_input_tokens: 1290,
            compiled_frame: None,
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "step_manifest");
        // Order is load-bearing — the manifest IS the wire sequence.
        assert_eq!(value["blocks"][0]["block_id"], "blk_sys");
        assert_eq!(value["blocks"][1]["block_id"], "blk_tail");
        assert_eq!(value["effective_budget_tokens"], 136_363);
        // A lifecycle-off manifest keeps the frame off the wire entirely
        // rather than serializing a null: this is the widest event in the
        // stream, it is emitted once per step, and the default state of the
        // switch that produces the frame is off.
        assert!(value.get("compiled_frame").is_none(), "{value}");
        let back: AgentEvent = serde_json::from_str(&value.to_string()).unwrap();
        match back {
            AgentEvent::StepManifest { blocks, .. } => {
                assert_eq!(blocks.len(), 2);
                assert_eq!(blocks[0].cache_zone, CacheZone::StablePrefix);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn a_manifest_from_a_pre_frame_stream_still_parses() {
        // Phase 2 (#713) added `compiled_frame`. Every `serde(default)` in this
        // crate owes a hand-written pre-field literal that proves the default;
        // this is that literal for the frame. A stored manifest recorded by any
        // build before the compiled frame existed must still decode, because
        // the journal is read back by newer binaries than wrote it.
        let legacy = r#"{"type":"step_manifest","turn_instance":0,"step":0,"role":"worker",
            "provider":"anthropic","model":"opus","blocks":[],
            "effective_budget_tokens":1,"calibration_factor":1.0,
            "estimated_input_tokens":1}"#;
        let event: AgentEvent = serde_json::from_str(legacy).unwrap();
        match event {
            AgentEvent::StepManifest {
                compiled_frame,
                call_seq,
                ..
            } => {
                assert_eq!(compiled_frame, None, "absent frame ⇒ the lifecycle was off");
                assert_eq!(call_seq, 0);
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn a_manifest_carrying_a_compiled_frame_round_trips() {
        let event = AgentEvent::StepManifest {
            turn_instance: 0,
            step: 0,
            call_seq: 0,
            role: ModelCallRole::Worker,
            provider: "anthropic".into(),
            model: "opus".into(),
            blocks: vec![],
            effective_budget_tokens: 1,
            calibration_factor: 1.0,
            estimated_input_tokens: 1,
            compiled_frame: Some(crate::CompiledContextFrameBuilt {
                compiled_frame_id: "cf_abc".into(),
                frame_hash: "sha256:abc".into(),
            }),
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["compiled_frame"]["compiled_frame_id"], "cf_abc");
        assert_eq!(value["compiled_frame"]["frame_hash"], "sha256:abc");
        let back: AgentEvent = serde_json::from_str(&value.to_string()).unwrap();
        match back {
            AgentEvent::StepManifest { compiled_frame, .. } => {
                assert_eq!(compiled_frame.unwrap().compiled_frame_id, "cf_abc");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn a_manifest_entry_from_a_pre_occurrence_stream_still_parses() {
        // #684 added per-occurrence `call_id`, and #389 `message_index`, to the
        // manifest entry. Every `serde(default)` in this crate owes a
        // hand-written pre-field literal that proves the default — these two
        // landed without one.
        let legacy = r#"{"block_id":"blk_a","token_cost":12,"resident_since_step":0}"#;
        let entry: ManifestEntry = serde_json::from_str(legacy).unwrap();
        assert_eq!(entry.cache_zone, CacheZone::Cacheable);
        assert_eq!(entry.message_index, 0);
        assert_eq!(entry.call_id, None);

        // A non-tool entry keeps the id off the wire entirely rather than
        // serializing a null — the manifest is the widest event in the stream
        // and every omitted key is real tokens on a receipt.
        let wire = serde_json::to_value(&entry).unwrap();
        assert!(wire.get("call_id").is_none(), "{wire}");
    }

    #[test]
    fn context_frame_ref_without_receipt_fields_still_parses() {
        // A recall frame recorded before receipts existed carries no block_id
        // or content_digest — it must still deserialize (additive contract).
        let old = r#"{"citation_label":"auth module","source":"stella-context","token_cost":42}"#;
        let frame: ContextFrameRef = serde_json::from_str(old).unwrap();
        assert_eq!(frame.citation_label, "auth module");
        assert!(frame.block_id.is_none());
        assert!(frame.content_digest.is_none());
    }

    #[test]
    fn unknown_block_kind_degrades_to_other_not_a_parse_error() {
        // A newer emitter may name a block kind this reader has never heard of.
        // The additive contract requires it to degrade, not reject the event.
        let kind: BlockKind = serde_json::from_str("\"some_future_kind\"").unwrap();
        assert_eq!(kind, BlockKind::Other);
        let zone: CacheZone = serde_json::from_str("\"some_future_zone\"").unwrap();
        assert_eq!(zone, CacheZone::Other);
    }

    #[test]
    fn type_tag_matches_the_serde_type_wire_tag() {
        // `type_tag()` must return exactly the `"type"` string serde writes,
        // since both come from the same `snake_case` variant name. The match's
        // exhaustiveness is compiler-enforced (a new variant cannot escape a
        // tag), so this only pins that the hand-written strings are correct —
        // cross-checked against serde for a representative sample, weighted to
        // the recently added variants most prone to a copy-paste tag.
        let sample = vec![
            AgentEvent::Stage {
                name: StageKind::Triage,
            },
            AgentEvent::Text { delta: "hi".into() },
            AgentEvent::TextDelta { text: "h".into() },
            AgentEvent::Reasoning { delta: "r".into() },
            AgentEvent::SpeculationDiscarded {
                call_id: "c".into(),
                name: "n".into(),
                reason: "attempt_failed".into(),
            },
            AgentEvent::Retry {
                attempt: 1,
                reason: "x".into(),
            },
            AgentEvent::Steered { text: "s".into() },
            AgentEvent::LoopDetected {
                turn_instance: 1,
                kind: "exact_repeat".into(),
                pattern: vec!["read".into()],
                repeats: 2,
                evidence: "e".into(),
                aborted: false,
            },
            AgentEvent::BudgetDenied {
                scope: BudgetScope::Turn,
                spent_usd: 1.0,
                limit_usd: 0.5,
                mode: BudgetMode::Enforced,
            },
            AgentEvent::RetriesExhausted {
                turn_instance: 1,
                attempts: 3,
                reasons: vec!["t".into()],
            },
            AgentEvent::PolicyDecision {
                kind: PolicyKind::Blocked,
                subject: "write_file".into(),
                outcome: "deny".into(),
            },
            AgentEvent::BudgetTick {
                spent_usd: 0.1,
                limit_usd: None,
                mode: BudgetMode::Off,
                session_spent_usd: None,
                session_limit_usd: None,
            },
            AgentEvent::UsageIncomplete {
                role: ModelCallRole::Worker,
                provider: "z".into(),
                model: "m".into(),
                reason: UsageIncompleteReason::Timeout,
                duration_ms: 1,
                retries: None,
            },
            AgentEvent::GoalVerdict {
                round: 1,
                met: true,
                reasoning: "ok".into(),
                cost_usd: 0.0,
            },
            AgentEvent::ProviderFallback {
                from: "a".into(),
                to: "b".into(),
                reason: "r".into(),
            },
            AgentEvent::Commit {
                sha: "abc".into(),
                message: "m".into(),
            },
            AgentEvent::Pr {
                url: "u".into(),
                status: PrStatus::Open,
                number: None,
                ci: None,
            },
            AgentEvent::Error {
                message: "m".into(),
                retryable: false,
            },
            AgentEvent::Complete {
                model: "m".into(),
                cost_usd: 0.0,
            },
        ];
        for event in &sample {
            let value = serde_json::to_value(event).unwrap();
            let wire = value
                .get("type")
                .and_then(|tag| tag.as_str())
                .unwrap_or_else(|| panic!("event has no string `type` tag: {event:?}"));
            assert_eq!(
                event.type_tag(),
                wire,
                "type_tag disagrees with the serde wire tag for {event:?}"
            );
        }
        // Pin two exact tags so a wholesale serde-rename change is caught too.
        assert_eq!(
            AgentEvent::TextDelta {
                text: String::new()
            }
            .type_tag(),
            "text_delta"
        );
        assert_eq!(
            AgentEvent::SpeculationDiscarded {
                call_id: String::new(),
                name: String::new(),
                reason: String::new(),
            }
            .type_tag(),
            "speculation_discarded"
        );
    }

    // ---- forward compatibility: events from a newer stella ----------------

    #[test]
    fn an_unrecognized_type_tag_degrades_to_unknown_rather_than_failing() {
        // The whole point of the change. A reader built before this variant
        // existed must not fail the line — it must keep going.
        let line = r#"{"type":"quantum_entangled","turn":7,"nested":{"a":[1,2]}}"#;
        let event: AgentEvent = serde_json::from_str(line).expect("future event must parse");
        let AgentEvent::Unknown {
            event_type,
            payload,
        } = &event
        else {
            panic!("expected Unknown, got {event:?}");
        };
        assert_eq!(event_type, "quantum_entangled");
        assert_eq!(payload["turn"], 7);
        assert_eq!(payload["nested"]["a"][1], 2);
        assert!(event.is_unknown());
    }

    #[test]
    fn an_unknown_event_round_trips_without_losing_data() {
        // A recorder or proxy must be able to pass a future event through
        // without corrupting it. Compare parsed values, not raw bytes: object
        // key order is not preserved (and is not meaningful).
        let line = r#"{"type":"from_the_future","alpha":1,"beta":["x",null,true]}"#;
        let event: AgentEvent = serde_json::from_str(line).unwrap();
        let reserialized = serde_json::to_string(&event).unwrap();

        let before: Value = serde_json::from_str(line).unwrap();
        let after: Value = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(before, after, "round-trip changed the event's content");
        // The tag must survive: it lives only inside `payload`.
        assert_eq!(after["type"], "from_the_future");
    }

    #[test]
    fn a_known_tag_with_a_malformed_body_is_still_a_hard_error() {
        // The load-bearing negative test. Forward compatibility is scoped to
        // *unrecognized tags only*; a recognized tag whose body does not fit
        // is an encoder bug or a corrupt record, and must stay loud. If this
        // ever degrades to Unknown, corruption becomes indistinguishable from
        // a version skew and the store's `skipped` counter starts lying.
        let err = serde_json::from_str::<AgentEvent>(r#"{"type":"text"}"#)
            .expect_err("`text` without `delta` must not parse");
        assert!(
            !format!("{err}").is_empty(),
            "a known tag with a bad body must produce a real error"
        );

        // Same for a wrong field type under a known tag.
        assert!(
            serde_json::from_str::<AgentEvent>(r#"{"type":"retry","attempt":"one","reason":"x"}"#)
                .is_err(),
            "`retry.attempt` is a u32; a string must not be laundered into Unknown"
        );

        // And an object with no tag at all keeps the derived error path.
        assert!(serde_json::from_str::<AgentEvent>(r#"{"delta":"hi"}"#).is_err());
    }

    #[test]
    fn every_known_type_tag_resolves_to_a_typed_variant() {
        // Proves the variant→tag list in `agent_event_tags!` has no typo.
        //
        // Combined with two structural facts this is airtight: the generated
        // match is exhaustive (so every variant is listed) and duplicate arms
        // would be unreachable (so each is listed once) — meaning
        // `KNOWN_TYPE_TAGS.len()` equals the variant count. If every listed tag
        // is additionally a *real* serde name, as asserted below, the mapping is
        // a bijection and no real tag can be missing from the list. A missing
        // one would silently demote all of its events to `Unknown`.
        //
        // The probe is a bare `{"type": tag}`, and today every variant has at
        // least one required field, so it always errors. That is expected — the
        // assertion is on WHICH error. `missing field ...` proves serde routed
        // the tag to a variant; `unknown variant ...` proves it did not, which
        // is exactly the typo this test exists to catch. Asserting only on the
        // `Ok` arm would assert nothing at all.
        for tag in KNOWN_TYPE_TAGS {
            let probe = serde_json::json!({ "type": tag });
            match serde_json::from_value::<AgentEvent>(probe) {
                Ok(event) => assert!(
                    !event.is_unknown(),
                    "`{tag}` is in KNOWN_TYPE_TAGS but deserialized as Unknown — \
                     the tag string does not match any serde variant name"
                ),
                Err(err) => assert!(
                    !err.to_string().contains("unknown variant"),
                    "`{tag}` is in KNOWN_TYPE_TAGS but serde has no variant by \
                     that name, so every event carrying it decodes as Unknown: {err}"
                ),
            }
        }

        let mut sorted = KNOWN_TYPE_TAGS.to_vec();
        sorted.sort_unstable();
        let before = sorted.len();
        sorted.dedup();
        assert_eq!(before, sorted.len(), "duplicate tag in KNOWN_TYPE_TAGS");
    }

    #[test]
    fn type_tag_of_an_unknown_event_is_the_preserved_wire_tag() {
        // `type_tag()` tells the truth even for an event it cannot decode, so
        // logs and metrics can name a future event correctly.
        let event: AgentEvent = serde_json::from_str(r#"{"type":"newer_thing"}"#).unwrap();
        assert_eq!(event.type_tag(), "newer_thing");
        assert!(!KNOWN_TYPE_TAGS.contains(&event.type_tag()));
    }

    #[test]
    fn a_literal_unknown_tag_is_not_privileged() {
        // `Unknown` is `serde(skip)`, so it has no wire tag of its own. An
        // event literally tagged `"unknown"` is just another unrecognized tag
        // and must round-trip as one rather than being mistaken for the
        // fallback variant's own encoding.
        let line = r#"{"type":"unknown","event_type":"spoof","payload":{}}"#;
        let event: AgentEvent = serde_json::from_str(line).unwrap();
        assert_eq!(event.type_tag(), "unknown");

        // It round-trips as the opaque object it is — the decoy `event_type`
        // and `payload` keys stay ordinary payload data, not the variant's
        // own fields.
        let after: Value = serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
        assert_eq!(after, serde_json::from_str::<Value>(line).unwrap());
        assert_eq!(after["event_type"], "spoof");
    }

    #[test]
    fn a_known_event_wire_format_is_unchanged_by_the_fallback() {
        // The hand-written codec must be a pass-through for known variants —
        // no tag rename, no extra wrapper, no reordering.
        let event = AgentEvent::Text {
            delta: "hello".into(),
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"type":"text","delta":"hello"}"#
        );
        let back: AgentEvent = serde_json::from_str(r#"{"type":"text","delta":"hello"}"#).unwrap();
        assert!(matches!(back, AgentEvent::Text { delta } if delta == "hello"));
    }
}
