//! [`AgentEvent`] itself — the tagged enum every variant of the wire
//! vocabulary lives on.
//!
//! Split out of `event.rs` when the enum's own bulk (roughly a thousand
//! lines of variants and their doc comments) crowded that file past the
//! 1500-line ratchet (#3776) — a pure move, `use super::*` carrying over
//! every type the variants reference. The module doc comment, the
//! hand-written [`serde::Serialize`]/[`serde::Deserialize`] impls that route
//! [`AgentEvent::Unknown`] around the derived codec, and the sibling `tests`
//! module all stay in `event.rs`; the smaller peer enums
//! ([`super::StageScope`], [`super::PolicyKind`], …) split the same way, to
//! `event/scopes.rs`.

use super::*;

/// One event in the turn's stream. Every stage boundary emits an event;
/// nothing user-visible is derived from internal state that isn't also in
/// this stream.
///
/// `remote = "Self"` keeps the derived codec as a pair of *inherent*
/// associated functions instead of the trait impls, so the hand-written
/// [`Serialize`]/[`Deserialize`] impls in `event.rs` can delegate to it after
/// routing [`AgentEvent::Unknown`] around it. Without that indirection the
/// forward-compat fallback would mean hand-writing a visitor for every variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case", remote = "Self")]
pub enum AgentEvent {
    /// A stage boundary was crossed. `scope` says **whose** stage it is, and
    /// it is not decoration (#3398).
    ///
    /// Two disjoint authorities emit stages. The engine emits three kinds,
    /// once per turn ([`crate::StageScope::Turn`]). A wrapper — the staged pipeline,
    /// a goal loop — emits its own vocabulary once per run
    /// ([`crate::StageScope::Run`]). Before this field existed the pipeline dropped
    /// the engine's copies outright, because a consumer receiving both had no
    /// way to tell them apart and several branch on stage transitions.
    ///
    /// The deck is the reason this is a required field rather than a hint: it
    /// treats a stage transition away from a scope review as the human's
    /// approval of that review. A turn-scoped stage arriving while a scope
    /// gate is open would forge consent nobody gave, so every consumer that
    /// branches on stages must be able to select the scope it means.
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "A stage boundary was crossed. `scope` says whose stage it is, and it is not decoration: two disjoint authorities emit stages. The engine emits its own, once per turn, with scope `turn`. A wrapper -- the staged pipeline, a goal loop -- emits its own vocabulary once per run, with scope `run`. A consumer that branches on stage transitions must select on `scope` first, or it will see two interleaved vocabularies as one and read a wrapper's backwards-looking boundary as an engine regression."
        )
    )]
    Stage {
        /// Which stage. An **open** vocabulary ([`crate::StageName`]): one of
        /// [`StageKind`]'s twelve when this host emitted the boundary, or a
        /// contributed stage's own word. On the wire it is a plain string
        /// either way, so the twelve encode exactly as they always have.
        #[cfg_attr(
            feature = "schema",
            schemars(
                description = "Which stage the boundary belongs to, as a plain string. An OPEN vocabulary: the host's own boundaries take the names listed in this field's type examples, and a stage contributed by an installed plugin takes whatever name that plugin declared. Every one of the host's own names encodes exactly as it always has, so an existing consumer keeps reading; a consumer must branch on the names it knows and keep a default arm, because a name it has never seen is now reachable."
            )
        )]
        name: crate::StageName,
        scope: StageScope,
    },
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
    /// (`"attempt_failed"`, `"harvest_mismatch"`, `"budget_abort"` — the
    /// budget guard ending the turn with a pool still in flight is the third
    /// producer, and went unlisted here until #3156). Additive to the wire
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
    /// A message was injected into the turn at a step boundary — the
    /// transcript's record that the model was steered, and when.
    ///
    /// Three rungs emit this and `cause` is what tells them apart: a person's
    /// mid-turn message, the stuck-loop nudge, and the stalled-turn nudge.
    /// Before it, a consumer could only match the English prose (#3622).
    Steered {
        text: String,
        /// Who or what steered. `serde(default)` so streams recorded before
        /// this field parse — as [`SteerCause::Unknown`], never as
        /// [`SteerCause::User`], which would relabel the whole recorded
        /// history as human input.
        #[serde(default)]
        cause: SteerCause,
    },
    /// The turn parked on an engine-side wait (#1471, #1857): a tool
    /// deposited a wait request and the engine is now probing on its own
    /// clock — zero model calls until the watched state changes or the
    /// deadline expires. The typed twin of the prose "⏳ Parked" narration,
    /// so a consumer can tell a park from model text, render a live
    /// heartbeat, and attribute the wall-clock gap. The matching
    /// [`AgentEvent::TurnWoken`] ends the span. Additive to the wire
    /// contract: consumers recorded before parked waits existed never see it.
    TurnParked {
        /// What the wait is for — the tool's human-readable description of
        /// the watched condition (e.g. "CI for branch main settles").
        description: String,
        /// Seconds between engine-side probes of the watched state.
        poll_interval_secs: u64,
        /// Seconds the park may last before it wakes with a timeout.
        deadline_secs: u64,
    },
    /// The parked turn woke and the next model call is imminent — closes the
    /// span its [`AgentEvent::TurnParked`] opened. Additive to the wire
    /// contract, like its twin.
    TurnWoken {
        /// `"changed"` | `"deadline_expired"` — mirrors
        /// `stella-core::waiting::WakeReason` (kept as a string here so
        /// `stella-protocol` never depends on `stella-core`).
        reason: String,
        /// Engine-side probes spent while parked — the poll history the
        /// transcript deliberately never carries.
        polls_used: u64,
    },
    /// Loop detection fired (receipts spec §6.3, #364 gap 3): the typed
    /// twin of the prose steer/abort, so receipts can parse the decision
    /// instead of string-matching an `Error` prefix. Emitted on BOTH
    /// outcomes — the first detection steers (`aborted: false`) and a
    /// detection that persists past the warning aborts (`aborted: true`).
    /// Additive to the wire contract: older consumers never see it.
    LoopDetected {
        turn_instance: u32,
        /// `"exact_repeat"` | `"short_cycle"` | `"stagnation"` |
        /// `"interleaved_repeat"` | `"monotonic_sweep"` — mirrors
        /// `stella-core::loop_detect::LoopVerdict` (kept as a string here so
        /// `stella-protocol` never depends on `stella-core`).
        kind: String,
        /// Tool names of the repeated signature, in cycle order (one entry
        /// for an exact repeat, a stagnating tool, an interleaved repeat, or
        /// a monotonic sweep).
        pattern: Vec<String>,
        /// Consecutive identical calls (exact repeat), full cycles (short
        /// cycle), consecutive no-progress calls (stagnation), occurrences
        /// anywhere in the window (interleaved repeat), or times the sweep
        /// wrapped back to its start (monotonic sweep) observed.
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
        /// under the same `call_id` (#1667); see `CompactionRewrite`.
        /// `serde(default)` — absent on journals written before rewrites were
        /// journaled, whose compacted blocks surface as digest mismatches.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rewrites: Vec<CompactionRewrite>,
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
        /// Wall clock left before the task deadline at this tick — the third
        /// axis, and the only one a journal could not otherwise state (#2240).
        ///
        /// `None` means **no deadline was armed**, which is exactly the
        /// distinction that used to require reading argv: a trial killed by its
        /// harness emitted dozens of these against a dollar cap it never
        /// approached, while the 900s wall clock that actually stopped it
        /// appeared nowhere in the journal. `Some(0)` is the opposite fact — a
        /// deadline is armed and has already passed.
        ///
        /// Milliseconds rather than a `Duration` because this is a wire type
        /// (invariant 4): a whole-millisecond integer round-trips through JSON
        /// byte-for-byte, where a float of seconds would not.
        ///
        /// `serde(default)` — absent on every journal written before this
        /// field existed, where it reads as "unarmed". That is the honest
        /// decode: those journals genuinely could not say otherwise.
        #[serde(default)]
        deadline_remaining_ms: Option<u64>,
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
        ///
        /// For a *gateway* this names the gateway (`openrouter`), which is as
        /// far as this field can honestly go — the silicon behind it rides in
        /// `upstream_provider`.
        #[serde(default)]
        provider: String,
        /// The upstream the gateway routed to, when it names one
        /// (`CompletionResult::upstream_provider`). `None` on direct
        /// endpoints, where `provider` is already the answer.
        ///
        /// Without this a run through OpenRouter records `openrouter` for
        /// every call and cannot say which vendor served any of them, so two
        /// arms of a benchmark could differ in model provider while both
        /// traces claimed to be identical.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        upstream_provider: Option<String>,
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
        #[cfg_attr(
            feature = "schema",
            schemars(
                description = "Why generation stopped, as the provider reported it. `length` is the only ground truth a consumer has that this step was cut off at the output ceiling -- the \"we stopped first\" signal. Absent on older streams, so treat a missing value as unknown rather than as a natural stop."
            )
        )]
        finish_reason: Option<crate::completion::FinishReason>,
        /// The reasoning effort the dispatched request actually carried
        /// (`CompletionRequest::effort`) — the *resolved* value, after
        /// auto-mode and any per-model downgrade, never the configured one
        /// (#4565).
        ///
        /// `None` means the request pinned no effort (or the stream predates
        /// this field — hence `serde(default)`), and must never be read as
        /// "effort low": absence of the pin is not a pin.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(
            feature = "schema",
            schemars(
                description = "The reasoning effort the dispatched request actually carried -- the resolved value after auto-mode and per-model downgrade, never the configured one. Absent when the request pinned no effort or the stream predates this field; absence is not a pin."
            )
        )]
        effort: Option<crate::completion::ReasoningEffort>,
        /// The output-token ceiling the dispatched request asked for
        /// (`CompletionRequest::max_output_tokens`) — the *effective* per-call
        /// value after the turn's standing clamp
        /// (`output_budget_recovery`), so it can move between steps of one
        /// turn (#4565). Paired with `finish_reason == Length` it says what
        /// ceiling the cut-off happened at.
        ///
        /// `None` means the request asked for no ceiling (or the stream
        /// predates this field — hence `serde(default)`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(
            feature = "schema",
            schemars(
                description = "The output-token ceiling the dispatched request asked for -- the effective per-call value after the turn's standing clamp, so it can move between steps of one turn. Paired with finish_reason == length it names the ceiling the cut-off happened at. Absent when the request asked for no ceiling or the stream predates this field."
            )
        )]
        max_output_tokens: Option<u32>,
        /// The sampling temperature the dispatched request carried
        /// (`CompletionRequest::temperature`), and
        /// [`params`](AgentEvent::StepUsage::params) beside it: together they
        /// are the generation shape of the ask, the third row of the profile
        /// card's gap that #4565 left (#4621).
        ///
        /// Two fields rather than one folded struct because
        /// [`CompletionRequest`](crate::CompletionRequest) itself carries them
        /// as two — mirroring its shape one-for-one leaves nothing to keep in
        /// sync, where a purpose-built struct would be a second vocabulary that
        /// silently stops matching the first time `GenerationParams` grows a
        /// field.
        ///
        /// `None` means the request pinned no temperature (or the stream
        /// predates this field — hence `serde(default)`), and must never be
        /// read as `0.0`: an unpinned temperature is the provider's own
        /// default, which is not a number this side knows.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(
            feature = "schema",
            schemars(
                description = "The sampling temperature the dispatched request carried. Absent when the request pinned none, or when the stream predates this field; absence is not zero -- an unpinned temperature is the provider's own default."
            )
        )]
        temperature: Option<f32>,
        /// The sampling and routing overrides the dispatched request carried
        /// (`CompletionRequest::params`): `top_p`, `top_k`, the penalties,
        /// `seed`, `verbosity`, `service_tier` (#4621).
        ///
        /// Content-free like the rest of this event — every field is a number
        /// or a closed enum, and none of them can hold prompt text. Tool
        /// schemas are the ask-side fact deliberately still absent: they are
        /// content-bearing, so they stay off the event and in the wire log.
        ///
        /// The value that went on the wire, so a run's sampling posture is
        /// answerable from the trace rather than from the settings file the
        /// operator had at the time — which is a different question whenever a
        /// seat, a sub-agent or a mid-run switch routed the call somewhere
        /// else. `None` means the request carried no overrides at all (or the
        /// stream predates this field).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(
            feature = "schema",
            schemars(
                description = "The sampling and routing overrides the dispatched request carried -- top_p, top_k, the penalties, seed, verbosity, service_tier. Content-free: every field is a number or a closed enum. Absent when the request carried no overrides, or when the stream predates this field."
            )
        )]
        params: Option<crate::completion::GenerationParams>,
        /// Which sub-agent spent this call, when a sub-agent did (#4383).
        ///
        /// `None` is the lead's own call. It is stamped at the sub-agent
        /// boundary (`stella_core::subagent`'s `child_sender`), so a nested
        /// child names *itself* rather than its parent — the innermost spender
        /// is the one a cost question is about.
        ///
        /// The bracket in [`crate::subagent_event::SubAgentPhase`] was meant to
        /// be the whole attribution mechanism, and it is enough for everything
        /// it was designed for. It is not enough for **this**: the engine
        /// dispatches independent delegates concurrently, so several children's
        /// events interleave on one stream and no `Started`/`Finished` pair
        /// brackets any particular call. Session `ses-1787465453163-60967` has
        /// five delegates completing within one second of each other; its
        /// ninety telemetry rows all read `worker`, and no join over the
        /// bracket can undo that.
        ///
        /// Content-free like the rest of this event: an opaque handle
        /// (`plugin:vera/worker#0`), never instruction text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(
            feature = "schema",
            schemars(
                description = "Which sub-agent spent this call. Absent means the lead's own call, which is the ordinary case. Stamped at the sub-agent boundary, so a nested child names itself rather than its parent. The `sub_agent` started/finished bracket cannot answer this: independent delegates are dispatched concurrently, so several children's events interleave on one stream and no bracket pair encloses any particular call. An opaque handle, never instruction text."
            )
        )]
        sub_agent_id: Option<String>,
    },
    /// A provider call failed or timed out after dispatch, so local accounting
    /// cannot prove that no billable work occurred. Content-free by design.
    UsageIncomplete {
        role: ModelCallRole,
        provider: String,
        /// The model the failed call was dispatched to.
        ///
        /// Per-call attribution, on the same contract as
        /// [`AgentEvent::StepUsage`]'s `provider`: this names what was
        /// actually being called, never the session's configured default.
        /// Sourced from [`crate::Provider::model`] (or the caller's own model
        /// hint) rather than hardcoded — until #2831 every one of these rows
        /// said [`UNKNOWN_MODEL`], which made a per-model failure census
        /// uncomputable and left mid-turn fallback unable to say which model
        /// had been failing.
        ///
        /// [`UNKNOWN_MODEL`] survives as the one documented spelling of "no
        /// model could be named here", and is expected to be rare.
        #[cfg_attr(
            feature = "schema",
            schemars(
                description = "The model the failed call was dispatched to. Per-call attribution, on the same contract as a `step_usage` event's `provider`: this names what was actually being called, never the session's configured default."
            )
        )]
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
        /// Which sub-agent's call died, when a sub-agent's did (#4383). Same
        /// contract as [`AgentEvent::StepUsage`]'s field of this name, and here
        /// for the same reason it is there: a delegate abandoned at the
        /// engine's dispatch ceiling lands one flagged row, and a row nobody
        /// can attribute explains an execution's `usage_complete = 0` without
        /// saying whose call it was.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(
            feature = "schema",
            schemars(
                description = "Which sub-agent's call died. Absent means the lead's own. Same contract as a `step_usage` event's field of this name: an abandoned delegate lands one flagged row, and a row nobody can attribute explains an execution's incomplete usage without saying whose call it was."
            )
        )]
        sub_agent_id: Option<String>,
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
    /// A provider substitution: the router fell back to the next configured
    /// provider of the same role's tier at resolution time (a breaker-open
    /// skip), or the engine swapped mid-turn after an exhausted retry ladder
    /// (`stella-core`'s `driver/model_fallback`, #2679). Never silent
    /// (L-M7) — no provider switch happens without this event.
    ProviderFallback {
        from: String,
        to: String,
        reason: String,
    },
    /// A file was created/modified/deleted during a turn, carrying both the
    /// authoritative line delta and a diff for display.
    ///
    /// **Observability, never evidence.** Nothing may found a claim about what
    /// changed on counting these — #2873 removed the last three decisions that
    /// did, and the tally survives only as a recorded-only field on
    /// `LadderSnapshot`. The reason is sharper than "it might be incomplete":
    /// the two producers below answer two *different* questions, and only one
    /// of them is about the agent.
    ///
    /// # The two producers, and what each one's answer means
    ///
    /// 1. **Candidate adoption** — `Pipeline::deliver_winner` (the built-in
    ///    staged pipeline's `pipeline/delivery.rs`, deleted in #3865), one
    ///    event per
    ///    `AdoptedChange`, emitted beside the `CandidateWorkspace::attribute_adopted`
    ///    call that writes the same rows to the host's durable ledger (#2907).
    ///    This one **was** attribution: adoption measured a candidate against a
    ///    sealed baseline, so it could tell the agent's edits from anyone
    ///    else's. **It has no producer in this workspace any more** — that
    ///    crate was deleted in #3865 — so every event on this stream today is
    ///    the second kind. Read the distinction below as the contract a
    ///    re-homed adoption producer would have to meet, not as two live
    ///    sources (#3881).
    /// 2. **The shared-tree turn boundary** — `stella-cli`'s `turn_files`, over
    ///    `WorkJournal::snapshot_worktree` (#3413). This one is **not**
    ///    attribution. It answers *what changed in the tree during this turn*,
    ///    which is the honest question a whole-tree measurement can answer: a
    ///    user editing a file in another window mid-turn lands here
    ///    indistinguishably from the agent's own writes.
    ///
    /// A consumer that needs "what did the agent do" takes it from the git diff
    /// of the tree, or from adoption. This stream is for showing a human what
    /// moved.
    ///
    /// # Why an engine-only turn is measured rather than hooked (#3413)
    ///
    /// It once emitted from the tools, and for a while after that from nowhere:
    /// the 12-tool purge (#3244) deleted every file-writing built-in and the
    /// file-CRUD ledger that emitted these, and this doc went on naming a
    /// `ToolRegistry::record_touch` that no longer existed. The file built-ins
    /// have since been restored, so a tool hook is now available — and it is
    /// still not right. A hook on `write_file` / `edit_file` / `delete_file`
    /// would report a *subset* of the turn while looking exhaustive: `bash`
    /// mutates the tree without naming a path, and so do MCP servers and
    /// custom script tools, none of which describes its paths in any schema
    /// the engine reads. And
    /// synthesizing these from tool *inputs* is the known defect, not the
    /// design: a wrapper that did exactly that, knowing four hard-coded tool
    /// names and sitting on one of three tool stacks, is what reported files
    /// edited in bulk or by a worker lane as `+0 -0` (#2290).
    ///
    /// So the answer is a measurement, taken once per turn at the boundary.
    /// The cost is one `git add -A` plus a `write-tree` against a dedicated
    /// index, after the model has answered.
    ///
    /// `added`/`removed` are what the producer measured — git's `--numstat`
    /// against the two trees, or, for adoption, numstat plus the patch it
    /// applied. Consumers **must** use them rather than counting `+`/`-` lines
    /// in `diff`: the diff is a bounded, deliberately coarse rendering of the
    /// changed region, and re-deriving from it is what made the two disagree.
    /// A binary file carries `0/0` and its kind.
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
        /// The upstream the gateway routed this call to, when it names one —
        /// the same contract as [`AgentEvent::StepUsage`]'s field of this
        /// name, carried here because this event is what the store projects
        /// into the durable `step_receipt` row (#3054): without it a stored
        /// receipt says `openrouter` for every gateway call and `stella
        /// inspect` cannot answer "which vendor served this call" for a past
        /// execution. `None` on direct endpoints, where `provider` is
        /// already the answer, and on every manifest recorded before this
        /// field existed (hence `serde(default)`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        upstream_provider: Option<String>,
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
        /// The pure-`sleep` seconds this turn has **asked for** across the
        /// detector's window, as of this step — the number the stall rung
        /// (`stella_core::driver::loop_escalation`) decides on, recorded
        /// rather than thrown away once it has decided (#3621).
        ///
        /// Requested, never executed: it is read off the calls' own text so
        /// the same transcript classifies the same way every step, which is
        /// what keeps the rung deterministic (invariant #2). A call killed by
        /// the shell's own timeout still contributes its full request, so
        /// this is an upper bound on wall clock. Executed seconds are already
        /// derivable from `ToolResult.duration_ms`, and the gap between the
        /// two is the signal — see #3624.
        ///
        /// `None` is "this emitter did not classify", not "zero seconds": the
        /// replay/reconstruction path rebuilds a receipt from stored messages
        /// with no detector window around it, and a reader must not count that
        /// as a turn that slept for nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stall_seconds_requested: Option<u64>,
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
    ///
    /// **No producer in this workspace** (#4454): #4448 removed
    /// `crates/stella-media`, and CLAUDE.md's tool-surface rule forbids the
    /// media built-in that would have been its caller (#3236, #3845). This
    /// variant and [`AgentEvent::MediaComplete`] stay as the wire contract an
    /// out-of-tree media MCP surface speaks, and because dropping the tags
    /// would demote an older recording's media events to
    /// [`AgentEvent::Unknown`]. Retiring them belongs to a `PROTOCOL_VERSION`
    /// bump, which #4454 decides.
    MediaProgress {
        artifact_id: String,
        kind: MediaKind,
        state: MediaJobState,
    },
    /// A media artifact landed under `.stella/artifacts/` with a manifest
    /// row. Producerless here for the reason
    /// [`AgentEvent::MediaProgress`] gives (#4454).
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
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "A bounded child turn started or finished -- the started/finished bracket IS the attribution for every event emitted between them. The child forwards a deliberately narrowed set of events across that boundary; see the sub-agent payload for what it carries and what it drops."
        )
    )]
    SubAgent { phase: SubAgentPhase },
    /// What the pipeline did with the winning candidate's workspace, and why
    /// (#2942). Emitted exactly once per isolated run, at the single point the
    /// decision is taken — so "was this candidate delivered?" is answered on
    /// the wire rather than inferred from whether a [`AgentEvent::FileChange`]
    /// burst happened to follow the verdict.
    ///
    /// `root` is the candidate workspace path, absent only when no workspace
    /// was ever created. It is a per-run temporary directory, so it is a
    /// run-to-run artifact and no golden comparison may key on its value.
    ///
    /// **Nothing in this workspace emits it.** Its sole producer was
    /// `Pipeline::deliver_winner` in `crates/stella-pipeline`, deleted in
    /// #3865, and the raw step-loop that remains has no candidates to choose
    /// between. #3881's decision is to keep it, for two reasons that are
    /// independent of each other: recorded journals and `stella-events.jsonl`
    /// files already carry the tag and stay readable, and best-of-N delivery is
    /// exactly the decision a wrapper plugin reports back over the socket
    /// (`doc:wrapper-socket`), so the wire shape a re-homed producer would need
    /// is this one. Its consumers are unaffected either way — see
    /// `event::tags`'s row, which declares `Surfaced` because the Observatory's
    /// journal query still selects it.
    ///
    /// See [`crate::delivery_event`] for why the outcome is a sum type and what
    /// the counts are measured from.
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "What the pipeline did with the winning candidate's workspace, and why. \
Emitted exactly once per isolated run, at the single point the decision is \
taken. The `delivery` object is internally tagged on its own `outcome` field, \
so a reader selects an arm by that field rather than by the presence or \
absence of a sibling key."
        )
    )]
    CandidateDelivery {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root: Option<String>,
        /// Deliberately **not** `serde(flatten)`: `AgentEvent` is internally
        /// tagged through a `remote = "Self"` codec and carries a `schemars`
        /// derive, and flattening a second internally-tagged enum into that is
        /// where both the wire schema and the forward-compat fallback stop
        /// agreeing with the Rust type. One nested object costs a `jq` reader
        /// `.delivery.outcome` instead of `.outcome`, which is cheaper than a
        /// generated schema that lies.
        delivery: DeliveryOutcome,
    },
    /// The turn failed. `retryable` is the source's own classification (see
    /// [`crate::error::ProviderError::is_retryable`]), never re-derived from
    /// `message` by a consumer.
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "The turn failed. `retryable` is the source's own classification of the failure and must be read as given -- never re-derived by matching on `message`, whose wording is not part of this contract."
        )
    )]
    Error { message: String, retryable: bool },
    /// **One turn** finished. `cost_usd` is that turn's spend and `model` the
    /// model that served its last committed call; both summarize the
    /// `StepUsage` events that preceded it, not a separate source of truth. A
    /// turn that fails ends on [`AgentEvent::Error`] instead, never on both.
    ///
    /// This is **not** "the work is over" (#3379). A wrapper — the staged
    /// pipeline, goal mode — runs several turns, so several of these appear in
    /// one run's stream, in order. The run's ending is [`AgentEvent::RunComplete`]
    /// and it appears exactly once.
    ///
    /// It was called `Complete` until #3379, and the rename is the point: one
    /// word meant "this turn is over" when the engine said it and "the whole
    /// job is over" when the pipeline said it, and nothing reading the journal
    /// could tell which contract it was holding. The old name is not aliased —
    /// every call site was moved.
    TurnComplete { model: String, cost_usd: f64 },
    /// **The run** finished — the stream's terminator, and the only event a
    /// consumer may treat as "nothing more is coming".
    ///
    /// Emitted exactly once, by whoever owns the run: a wrapper if one is
    /// driving (after its "another turn?" answer is *no*), otherwise the host
    /// that asked for the single turn. `cost_usd` is the whole run's spend
    /// across every turn it contained, so it is `>=` any single
    /// [`AgentEvent::TurnComplete`]'s.
    ///
    /// A run that ends in failure ends on [`AgentEvent::Error`] with
    /// `retryable: false` instead, exactly as it did before this event
    /// existed — never on both.
    ///
    /// # The one-directional contract (#3379)
    ///
    /// The engine always finishes its turn and always says so; a wrapper that
    /// wants more work asks for another turn. It never suppresses, rewrites,
    /// or re-emits an engine event to manufacture an ending. Before this
    /// existed the pipeline did exactly that — it dropped the engine's
    /// terminal events and emitted its own in their place — which is a
    /// two-way connection between the engine and one of its callers.
    RunComplete { model: String, cost_usd: f64 },
    /// The workspace's own steering — memories, rules and published context
    /// records, skills, commands, agents — was on disk and was **not** loaded,
    /// because the authority in `withheld_by` refused it (#2302, #3616).
    ///
    /// Emitted once per run, before the turn opens, and only when something
    /// was actually held back: a notice every repository sees is one nobody
    /// reads. It is the machine-readable twin of the stderr line, so a harness
    /// running `--output-format stream-json` learns that this session was not
    /// steered by the repository it is sitting in without scraping the human
    /// channel.
    ///
    /// **Counts only** — never a filename, never a body, never the workspace
    /// path. The withheld text is repository-controlled, and a refusal that
    /// echoed it would be the exfiltration channel the refusal exists to
    /// prevent.
    SteeringWithheld {
        withheld_by: Withholder,
        memories: usize,
        records: usize,
        skills: usize,
        commands: usize,
        agents: usize,
    },
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
