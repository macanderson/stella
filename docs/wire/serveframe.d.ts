// GENERATED FILE — DO NOT EDIT.
//
// Regenerate with:  bash scripts/export-agentevent-schema.sh
// Source of truth:  stella-serve/src/frame.rs
// Guarded by:       scripts/check-wire-schema.sh (`make wire-schema`)
//
// The `stella-serve` transport contract: what a host reads off
// `GET /v1/turns/{id}/events` and what it POSTs back.
//
// Frames arrive as SSE events. Each has an `id:` line carrying the frame's
// `seq` and a `data:` line carrying the JSON below.

/**
 * One event in the turn's stream. Every stage boundary emits an event;
 * nothing user-visible is derived from internal state that isn't also in
 * this stream.
 *
 * `remote = "Self"` keeps the derived codec as a pair of *inherent*
 * associated functions instead of the trait impls, so the hand-written
 * [`Serialize`]/[`Deserialize`] impls below can delegate to it after routing
 * [`AgentEvent::Unknown`] around it. Without that indirection the forward-
 * compat fallback would mean hand-writing a visitor for every variant.
 */
export type AgentEvent = {
  /**
   * Which stage the boundary belongs to, as a plain string. An OPEN vocabulary: the host's own boundaries take the names listed in this field's type examples, and a stage contributed by an installed plugin takes whatever name that plugin declared. Every one of the host's own names encodes exactly as it always has, so an existing consumer keeps reading; a consumer must branch on the names it knows and keep a default arm, because a name it has never seen is now reachable.
   */
  name: StageName;
  scope: StageScope;
  type: "stage";
} | {
  text: string;
  type: "text";
} | {
  delta: string;
  type: "text_delta";
} | {
  delta: string;
  type: "reasoning";
} | {
  call: ToolCall;
  type: "tool_start";
} | {
  call_id: string;
  duration_ms: number;
  output: ToolOutput;
  /**
   * True when this result was produced by speculative execution: the
   * call was read-only and began executing while the model was still
   * streaming the rest of its response, so `duration_ms` (the real
   * execution time) overlapped the model call instead of following
   * it. `serde(default)` so streams recorded before this field parse.
   */
  speculated?: boolean;
  type: "tool_result";
} | {
  call_id: string;
  name: string;
  reason: string;
  type: "speculation_discarded";
} | {
  /**
   * 1-indexed ordinal of the attempt that FAILED and triggered this
   * retry — the initial call is attempt 1 (mirrors
   * `stella-core::retry::RetryAttempt::attempt`).
   */
  attempt: number;
  reason: string;
  type: "retry";
} | {
  /**
   * Who or what steered. `serde(default)` so streams recorded before
   * this field parse — as [`SteerCause::Unknown`], never as
   * [`SteerCause::User`], which would relabel the whole recorded
   * history as human input.
   */
  cause?: SteerCause;
  text: string;
  type: "steered";
} | {
  /**
   * Seconds the park may last before it wakes with a timeout.
   */
  deadline_secs: number;
  /**
   * What the wait is for — the tool's human-readable description of
   * the watched condition (e.g. "CI for branch main settles").
   */
  description: string;
  /**
   * Seconds between engine-side probes of the watched state.
   */
  poll_interval_secs: number;
  type: "turn_parked";
} | {
  /**
   * Engine-side probes spent while parked — the poll history the
   * transcript deliberately never carries.
   */
  polls_used: number;
  /**
   * `"changed"` | `"deadline_expired"` — mirrors
   * `stella-core::waiting::WakeReason` (kept as a string here so
   * `stella-protocol` never depends on `stella-core`).
   */
  reason: string;
  type: "turn_woken";
} | {
  /**
   * `false`: first detection, the turn was steered and continues.
   * `true`: detection persisted after the warning, the turn aborted.
   */
  aborted: boolean;
  /**
   * The human-readable evidence — same text the paired
   * `Steered`/`Error` carries.
   */
  evidence: string;
  /**
   * `"exact_repeat"` | `"short_cycle"` | `"stagnation"` — mirrors
   * `stella-core::loop_detect::LoopVerdict` (kept as a string here so
   * `stella-protocol` never depends on `stella-core`).
   */
  kind: string;
  /**
   * Tool names of the repeated signature, in cycle order (one entry
   * for an exact repeat or a stagnating tool).
   */
  pattern: string[];
  /**
   * Consecutive identical calls (exact repeat), full cycles (short
   * cycle), or consecutive no-progress calls (stagnation) observed.
   */
  repeats: number;
  turn_instance: number;
  type: "loop_detected";
} | {
  limit_usd: number;
  mode: BudgetMode;
  /**
   * Which limit tripped.
   */
  scope: BudgetScope;
  spent_usd: number;
  type: "budget_denied";
} | {
  /**
   * Total dispatched attempts that failed (the initial call plus
   * every retry). Equals `reasons.len()`.
   */
  attempts: number;
  /**
   * Per-attempt failure reasons, oldest first.
   */
  reasons: string[];
  /**
   * Whether the LAST attempt's error was of a retryable class —
   * mirrors the paired [`AgentEvent::Error`]'s `retryable` field,
   * computed from the same `ProviderError::is_retryable()` call.
   * `false` means retrying again could never have helped: the
   * clearest case is `ProviderError::Auth` failing on attempt 1,
   * where `attempts` is 1 and no retry was ever attempted despite
   * this event's name (#926) — a receipts/telemetry consumer that
   * only sees `RetriesExhausted` would otherwise record a
   * reliability incident for what is really a bad credential.
   * `#[serde(default = "retries_exhausted_retryable_default")]`:
   * event logs recorded before this field existed predate the
   * distinction, so on replay they read as "genuinely retryable"
   * — the pre-#926 behavior — rather than being silently
   * reclassified as terminal.
   */
  retryable?: boolean;
  turn_instance: number;
  type: "retries_exhausted";
} | {
  kind: PolicyKind;
  /**
   * Short outcome token — e.g. `"allow"`, `"deny"`, `"modify"`, a
   * detector's kind list — never content.
   */
  outcome: string;
  /**
   * The tool name, capability, or workspace-relative path the
   * decision was about.
   */
  subject: string;
  type: "policy_decision";
} | {
  after_tokens: number;
  /**
   * Large old outputs middle-out truncated instead of dropped whole.
   */
  aged?: number;
  aged_blocks?: string[];
  before_tokens: number;
  /**
   * The per-model calibration factor the pass divided by. `serde(default)`
   * makes this `0.0` on a pre-receipt journal, which is a sentinel, NOT a
   * factor: recovering the raw budget as `effective * factor` yields 0 and
   * dividing by it yields infinity. A consumer must read `0.0` as "this
   * journal predates calibration" and skip the derivation, exactly as it
   * reads `effective_budget_tokens == 0`. (The identity factor is `1.0`;
   * the default cannot be changed to it without rewriting how every
   * already-written journal decodes.)
   */
  calibration_factor?: number;
  deduped: number;
  deduped_blocks?: string[];
  /**
   * The budget this pass actually compared against — the raw compaction
   * budget divided by the model's calibration factor — and that factor.
   * The event's `before/after_tokens` are raw estimates; these are the
   * numbers the eviction loop's stopping condition used, so the receipt
   * lines up with the decision (#364 item 1). `0` on pre-receipt journals.
   */
  effective_budget_tokens?: number;
  evicted: number;
  /**
   * The `block_id`s each pass stubbed (spec §6.2) — identities, not just
   * counts, so the receipt records *which* blocks left context and a
   * later pass can prove a block was evicted before it was ever cited or
   * referenced (the wasted-carry signal). For the pure passes each vec's
   * length equals its count field (`summarized_blocks` is the documented
   * exception). `serde(default)` — absent on pre-identity journals.
   */
  evicted_blocks?: string[];
  /**
   * The replacement bytes each in-place rewrite left behind, one entry
   * per digest — what lets reconstruction resolve a compacted block to
   * the bytes the model received rather than the pre-compaction output
   * under the same `call_id` (#1667); see `CompactionRewrite`.
   * `serde(default)` — absent on journals written before rewrites were
   * journaled, whose compacted blocks surface as digest mismatches.
   */
  rewrites?: CompactionRewrite[];
  /**
   * Messages replaced by a model-written history summary — the
   * overflow fallback when eviction alone cannot reach budget.
   */
  summarized?: number;
  /**
   * The `block_id`s of the tool-result blocks folded into an
   * overflow-summary splice (spec §6.2). Unlike the pure passes — which
   * stub tool-result blocks one-for-one, so their vec length equals the
   * count — the summary replaces a whole message span whose `summarized`
   * count also covers user/assistant text carrying no block identity;
   * this vector is the identity-bearing (tool-result) subset that left
   * context, so `summarized_blocks.len()` may be less than `summarized`.
   * `serde(default)` — absent on pre-identity journals.
   */
  summarized_blocks?: string[];
  /**
   * Older results of a repeated identical call, stubbed as stale.
   * `serde(default)` so journals written before these fields parse.
   */
  superseded?: number;
  superseded_blocks?: string[];
  type: "compaction";
} | {
  /**
   * Wall clock left before the task deadline at this tick — the third
   * axis, and the only one a journal could not otherwise state (#2240).
   *
   * `None` means **no deadline was armed**, which is exactly the
   * distinction that used to require reading argv: a trial killed by its
   * harness emitted dozens of these against a dollar cap it never
   * approached, while the 900s wall clock that actually stopped it
   * appeared nowhere in the journal. `Some(0)` is the opposite fact — a
   * deadline is armed and has already passed.
   *
   * Milliseconds rather than a `Duration` because this is a wire type
   * (invariant 4): a whole-millisecond integer round-trips through JSON
   * byte-for-byte, where a float of seconds would not.
   *
   * `serde(default)` — absent on every journal written before this
   * field existed, where it reads as "unarmed". That is the honest
   * decode: those journals genuinely could not say otherwise.
   */
  deadline_remaining_ms?: number | null;
  limit_usd?: number | null;
  mode: BudgetMode;
  /**
   * The configured per-session limit, when one is set. `None` mirrors
   * `session_spent_usd`.
   */
  session_limit_usd?: number | null;
  /**
   * Session-scoped spend at this tick — `spent_usd`/`limit_usd` are
   * turn-scoped, so a HUD cannot otherwise reconstruct session state
   * (or see a session-axis breach) from this stream. `None` when the
   * emitter does not track a session axis, and on events serialized
   * before these fields existed (hence `serde(default)`, so older
   * streams still parse).
   */
  session_spent_usd?: number | null;
  spent_usd: number;
  type: "budget_tick";
} | {
  /**
   * Tokens written to the provider's prompt cache by this call
   * (`CompletionUsage::cache_write_tokens`). Reported separately from
   * `input_tokens`, never a subset of it. `0` when the provider does
   * not report cache writes (the OpenAI-compatible dialects) — hence
   * `serde(default)`, so streams serialized before this field existed
   * still parse.
   */
  cache_write_tokens?: number;
  cached_input_tokens: number;
  /**
   * Whether the provider supplied a truthful usage envelope. Missing
   * legacy values fail closed to `false`.
   */
  complete?: boolean;
  cost_usd: number;
  duration_ms: number;
  /**
   * The engine's RAW (uncalibrated) pre-call estimate of the input it
   * sent — paired with `input_tokens` (plus cache-write tokens, which
   * are real prompt tokens split out only for pricing) this is one
   * drift sample, the feedback that calibrates future estimates per
   * model (`stella-core::estimator::Calibration`). Raw by contract:
   * consumers rebuild the correction from these pairs, and a
   * corrected estimate here would compound the correction on every
   * round trip. Attachment weight is excluded — the media estimate is
   * a deliberate ~80× over-estimate of billed tokens, right for
   * context pressure and poison as a drift sample. `0` means no
   * estimate was taken (pre-drift emitters — hence `serde(default)`,
   * so old streams still parse).
   */
  estimated_input_tokens?: number;
  /**
   * Why generation stopped, as the provider reported it. `length` is the only ground truth a consumer has that this step was cut off at the output ceiling -- the "we stopped first" signal. Absent on older streams, so treat a missing value as unknown rather than as a natural stop.
   */
  finish_reason?: FinishReason | null;
  input_tokens: number;
  model: string;
  /**
   * Authoritative model output for calls that do not emit a separate
   * [`AgentEvent::Text`] (pipeline management and compaction calls).
   * Execute calls leave this `None`, avoiding duplicate transcript
   * text while keeping older event consumers compatible.
   */
  output_text?: string | null;
  output_tokens: number;
  /**
   * Provider which actually served this call, never the session's
   * configured default. Empty only on legacy events.
   *
   * For a *gateway* this names the gateway (`openrouter`), which is as
   * far as this field can honestly go — the silicon behind it rides in
   * `upstream_provider`.
   */
  provider?: string;
  /**
   * The reasoning share of `output_tokens`, when the provider breaks it
   * out (`CompletionUsage::reasoning_tokens`). Already inside
   * `output_tokens` — a diagnostic split, never its own cost line.
   *
   * Absent means the provider does not report it (every Anthropic
   * Messages API call, which folds thinking into `output_tokens`);
   * `0` means it reported no reasoning on this call. A consumer that
   * reads absent as zero would conclude the entire Anthropic-direct
   * route never thinks, so this stays `Option` rather than defaulting.
   */
  reasoning_tokens?: number | null;
  retries: number;
  /**
   * Exact call purpose. Missing legacy values deserialize as
   * [`ModelCallRole::Unknown`].
   */
  role?: ModelCallRole;
  step: number;
  tool_calls: number;
  type: "step_usage";
  /**
   * The upstream the gateway routed to, when it names one
   * (`CompletionResult::upstream_provider`). `None` on direct
   * endpoints, where `provider` is already the answer.
   *
   * Without this a run through OpenRouter records `openrouter` for
   * every call and cannot say which vendor served any of them, so two
   * arms of a benchmark could differ in model provider while both
   * traces claimed to be identical.
   */
  upstream_provider?: string | null;
} | {
  duration_ms: number;
  /**
   * The model the failed call was dispatched to. Per-call attribution, on the same contract as a `step_usage` event's `provider`: this names what was actually being called, never the session's configured default.
   */
  model: string;
  /**
   * Accounting the adapter had already observed when the attempt died.
   *
   * "Incomplete" is not the same as "unknown", and this field is the
   * difference. A stream cut mid-answer has usually already been told
   * what the prompt cost, so `Some` here turns a bare warning into a
   * number: how much of this attempt we can actually account for.
   * `None` is the honest answer for a failure that learned nothing —
   * a connect timeout, a cancelled call, a 5xx with no stream.
   *
   * Token counts are content-free, so carrying them keeps this event
   * inside its no-prompts-no-bodies contract.
   */
  partial?: PartialUsage | null;
  provider: string;
  reason: UsageIncompleteReason;
  /**
   * Number of retries completed before the failure, when known.
   */
  retries?: number | null;
  role: ModelCallRole;
  type: "usage_incomplete";
} | {
  cost_usd: number;
  met: boolean;
  reasoning: string;
  round: number;
  type: "goal_verdict";
} | {
  from: string;
  reason: string;
  to: string;
  type: "provider_fallback";
} | {
  /**
   * `serde(default)` so journals written before the counts existed
   * parse — those replay as `0/0`, which is what they recorded.
   */
  added?: number;
  diff?: string | null;
  kind: FileChangeKind;
  path: string;
  removed?: number;
  type: "file_change";
} | {
  frames: ContextFrameRef[];
  /**
   * Wall-clock milliseconds the recall itself took (#875).
   *
   * Recall sits on the **first-token path of every turn**, so a slow
   * one delays everything after it — and until this existed, a cold
   * store, a large corpus or a wedged embedding call was
   * indistinguishable from a fast recall right up until it became a
   * timeout. Defaulted, so older streams still deserialize; `0` there
   * means "not measured", not "instant".
   */
  latency_ms?: number;
  provider_mix: ProviderShare[];
  tokens: number;
  type: "context_recall";
  /**
   * The CGP usage report for this recall (`docs/spec/adaptive-context/context-reuse.md` §2):
   * per-provider frame counts and token costs against the requested
   * budget, so context cost is meterable rather than merely visible.
   * Optional and defaulted — streams recorded before the report existed
   * still deserialize (the additive contract), and a recall path with
   * no CGP host behind it has none to report.
   */
  usage?: ContextUsage | null;
  /**
   * Whether the IVF approximate-nearest-neighbour accelerator fired,
   * or `None` when the recall path did not report it. Latency alone
   * cannot be acted on: a slow recall with a cold index is a different
   * problem, with a different fix, from one that used the index and
   * was still slow.
   *
   * Tri-state on purpose. `stella-context` knows the answer, but the
   * CGP host fan-out production recall goes through does not carry it
   * across the provider-result boundary — closing that is a change to
   * the provider contract, not to this event. A plain `bool` would
   * report `false` on every real turn, which reads as "the index never
   * fires" rather than "nobody said". `None` says what is true.
   */
  used_ann_index?: boolean | null;
} | {
  provider: string;
  superseded: number;
  type: "context_write";
  upserts: number;
} | {
  /**
   * `blk_<24 hex of sha256(kind \0 content)>`. Byte-identical blocks
   * share an id, so dedup/supersession become identities not counts.
   */
  block_id: string;
  /**
   * Human label for recall frames / memory nodes, when the block has one.
   */
  citation_label?: string | null;
  /**
   * The preimage for gap kinds the journal cannot resolve (the system
   * prefix, the assembled user/recall message). `None` for
   * journal-resolvable kinds (tool I/O, assistant text) — those never
   * carry bytes here. Redacted by the content-free export projection,
   * but present on the live event stream, so it reaches stream-json
   * stdout: this is the one field on `AgentEvent` that can carry raw
   * prompt text, and the only thing keeping it off a remote sink is
   * where the operator points the stream.
   */
  content?: string | null;
  /**
   * `"sha256:<full hex>"` — verifies the preimage on reconstruction.
   */
  content_digest: string;
  kind: BlockKind;
  origin: BlockOrigin;
  /**
   * Estimated tokens at birth (the engine's estimator).
   */
  token_cost: number;
  type: "block_registered";
} | {
  /**
   * Blocks in wire order; index 0 is the system prefix.
   */
  blocks: ManifestEntry[];
  /**
   * The per-model calibration factor applied to the raw budget.
   */
  calibration_factor: number;
  /**
   * Disambiguates the several model calls that can share one
   * `(turn_instance, step)`. The engine's own worker call is always 0;
   * auxiliary calls that ride the same step — the overflow summarizer,
   * and the pipeline's triage/verifier/plan/guidance roles — take 1, 2, …
   * from a per-execution counter. Without it a summarizer receipt and
   * the worker receipt it precedes collide on the primary key and the
   * auxiliary one is silently replaced. `serde(default)` so manifests
   * persisted before this field existed still decode (as the worker 0).
   */
  call_seq?: number;
  /**
   * This manifest's identity as a **compiled context frame** — ADR 0006
   * as amended: the compiled frame is this manifest extended, not a
   * parallel aggregate, so its id and hash are fields here rather than a
   * second record of the same call.
   *
   * `Some` only when `context.lifecycle.enabled` is on; `None`
   * otherwise and on every manifest recorded before the frame existed.
   * The hash covers what entered the prompt and deliberately excludes
   * the accounting around it — `provider`, `model`, `call_seq`, the two
   * budget numbers, and each entry's `resident_since_step` — so two runs
   * of identical work agree even when served by different models. See
   * `stella_core::receipts::compiled_frame` for the exact preimage.
   */
  compiled_frame?: CompiledContextFrameBuilt | null;
  /**
   * The budget the compaction pass actually compared against THIS step —
   * the raw budget divided by the model's calibration factor. Evented so
   * the receipt's numbers line up with the decision that was made (the
   * `Compaction` event's raw before/after do not, on their own — #364).
   */
  effective_budget_tokens: number;
  /**
   * Sum of block token costs, pre-call (the engine's raw estimate).
   */
  estimated_input_tokens: number;
  model: string;
  provider: string;
  role: ModelCallRole;
  step: number;
  /**
   * Monotonic per session — groups the steps of one `run_turn`.
   */
  turn_instance: number;
  type: "step_manifest";
} | {
  step: ProofStep;
  type: "proof";
} | {
  evidence: VerdictEvidence;
  passed: boolean;
  type: "verdict";
} | {
  proposal: ScopeProposal;
  type: "scope_review";
} | {
  proposal: HunkProposal;
  type: "hunk_review";
} | {
  /**
   * Correlates the eventual answer (the ToolResult's `call_id`)
   * back to this question.
   */
  id: string;
  options: string[];
  question: string;
  type: "ask_user";
} | {
  artifact_id: string;
  kind: MediaKind;
  state: MediaJobState;
  type: "media_progress";
} | {
  artifact: MediaArtifactRef;
  type: "media_complete";
} | {
  message: string;
  sha: string;
  type: "commit";
} | {
  /**
   * The head commit's aggregate CI verdict, when observed. Absent
   * means "not polled yet", never "passing".
   */
  ci?: CiStatus | null;
  /**
   * The PR number (e.g. 183 for `…/pull/183`). `None` on streams
   * recorded before the field existed or when the monitor could not
   * parse one from the URL.
   */
  number?: number | null;
  status: PrStatus;
  type: "pr";
  url: string;
} | {
  tasks: TaskItem[];
  type: "task_update";
} | {
  phase: SubAgentPhase;
  type: "sub_agent";
} | {
  /**
   * Deliberately **not** `serde(flatten)`: `AgentEvent` is internally
   * tagged through a `remote = "Self"` codec and carries a `schemars`
   * derive, and flattening a second internally-tagged enum into that is
   * where both the wire schema and the forward-compat fallback stop
   * agreeing with the Rust type. One nested object costs a `jq` reader
   * `.delivery.outcome` instead of `.outcome`, which is cheaper than a
   * generated schema that lies.
   */
  delivery: DeliveryOutcome;
  root?: string | null;
  type: "candidate_delivery";
} | {
  message: string;
  retryable: boolean;
  type: "error";
} | {
  cost_usd: number;
  model: string;
  type: "turn_complete";
} | {
  cost_usd: number;
  model: string;
  type: "run_complete";
};

/**
 * One multimodal input attached to a user message.
 */
export interface Attachment {
  /**
   * Payload size in bytes (pre-base64), for display and telemetry.
   */
  byte_len: number;
  /**
   * MIME type (`image/png`, `application/pdf`, `video/mp4`, …).
   */
  media_type: string;
  /**
   * Display name — the original filename, or a synthetic one for
   * clipboard pastes (`pasted-image.png`).
   */
  name: string;
  /**
   * Where the payload actually lives — a path at rest, inline base64 only
   * after the model layer hydrates it for one request.
   */
  source: AttachmentSource;
}

/**
 * Where an attachment's payload lives.
 */
export type AttachmentSource = {
  /**
   * Where the payload lives. Resolved by the model layer at hydration
   * time, so a moved or deleted file fails then, not at attach time.
   */
  path: string;
  type: "path";
} | {
  /**
   * The payload, base64-encoded (no data-URI prefix).
   */
  base64: string;
  type: "data";
};

/**
 * The semantic kind of one context block — one durable, individually
 * attributable unit that can enter the model's prompt. Finer-grained than a
 * `CompletionMessage`: a tool message holding several results decomposes into
 * one `ToolResult` block per `call_id`. See the session-telemetry-receipts
 * spec (`docs/spec/session-telemetry-receipts-spec.md`, §4). Forward-compat:
 * an unknown kind read from a newer emitter deserializes to [`BlockKind::Other`]
 * rather than failing the whole event.
 */
export type BlockKind = "system_prefix" | "user_goal" | "recalled_frame" | "assistant_text" | "tool_call" | "tool_result" | "steered" | "summary" | "attachment" | "other";

/**
 * Where a context block came from — the provenance stamped once at a block's
 * birth (spec §4). The join hub: a `RecalledFrame` carries the `memory_id` it
 * was recalled from; a `ToolResult`/`ToolCall` carries its `call_id`.
 */
export interface BlockOrigin {
  /**
   * Tool-call correlation id, for `ToolCall`/`ToolResult` blocks — the
   * call that *first* minted this block. Block ids are content-addressed,
   * so two distinct calls with byte-identical output share one id and only
   * the first is registered: this is birth provenance, **not** the complete
   * set of calls that carried the block. For "which calls did this block
   * serve at step N", read [`ManifestEntry::call_id`], which is recorded
   * per occurrence.
   */
  call_id?: string | null;
  /**
   * The `nod_…` memory node id, for a `RecalledFrame` that is a memory.
   */
  memory_id?: string | null;
  /**
   * The step within that turn.
   */
  step: number;
  /**
   * Monotonic per session — the `run_turn` that produced the block.
   */
  turn_instance: number;
}

/**
 * Budget enforcement mode: `off` (no metering),
 * `observed` (meter + warn), `enforced` (hard stop with a clean turn
 * abort — never a mid-tool kill).
 */
export type BudgetMode = "off" | "observed" | "enforced";

/**
 * Which budget limit a [`AgentEvent::BudgetDenied`] tripped — mirrors
 * `stella-core::budget::BudgetAxis` (kept separate so `stella-protocol`
 * never depends on `stella-core`).
 */
export type BudgetScope = "turn" | "session";

/**
 * A block's cache position relative to the provider's prompt-cache
 * breakpoints. Stella keeps the system prefix byte-stable and places volatile
 * recall after it (L-E8), so a block's zone is computable from its position at
 * manifest time. A structural hint at emission; reconciled against reported
 * usage by cache attribution (spec §7). Forward-compat via [`CacheZone::Other`].
 */
export type CacheZone = "stable_prefix" | "cacheable" | "volatile" | "other";

/**
 * One clause of a task's definition of done.
 */
export interface Check {
  /**
   * How it is settled.
   */
  mechanism: CheckMechanism;
  /**
   * Where it stands. Defaults to [`CheckOutcome::Pending`] so a plan can be
   * written before anything has run.
   */
  outcome?: CheckOutcome;
  /**
   * What must be true, in the author's words: "no inbound refs to the
   * removed symbol", "the auth suite is green".
   */
  statement: string;
}

/**
 * How a check is settled — an OPEN vocabulary. The mechanisms this host runs itself are listed in `examples` and imply their own judge; any other name is a contributed mechanism and MUST carry `judge`, so a consumer can always tell whether a machine or a model decided.
 */
export interface CheckMechanism {
  judge?: Judge;
  name: string;
}

/**
 * Where a check stands.
 *
 * [`CheckOutcome::Passed`] and [`CheckOutcome::Failed`] both carry evidence
 * because an outcome without it is exactly the self-report this module exists
 * to replace — "it passed" is a claim, and "42 tests, 0 failures" is the thing
 * that makes it checkable by someone else.
 */
export type CheckOutcome = {
  state: "pending";
} | {
  evidence: string;
  state: "passed";
} | {
  evidence: string;
  state: "failed";
};

/**
 * Aggregate CI verdict for a PR's head commit, as observed by the
 * fleet monitor (`gh pr checks`). Reconciled against the live source
 * before rendering, never served from cache alone (L-V3).
 */
export type CiStatus = "pending" | "running" | "passing" | "failing";

/**
 * One in-place compaction rewrite: the post-rewrite identity and bytes of a
 * tool result a compaction pass stubbed, deduplicated, superseded, or aged.
 *
 * `content` is the canonical serialized form of the replacement tool output —
 * the same `serde_json` serialization the receipts plane hashes — so
 * `content_digest` re-derives from `content` exactly and a consumer can
 * verify the pair without any other context.
 */
export interface CompactionRewrite {
  /**
   * The content-addressed id (`blk_…`) of the block the rewrite produced —
   * the identity the next step's manifest cites for this result.
   */
  block_id: string;
  /**
   * The replacement bytes: the serialized post-rewrite tool output.
   */
  content: string;
  /**
   * `sha256:<hex>` of `content` — the digest the block registry records,
   * and the key reconstruction resolves this preimage by.
   */
  content_digest: string;
}

/**
 * Stable-ID payload of `compiled_context_frame_built`.
 */
export interface CompiledContextFrameBuilt {
  /**
   * The compiled frame id.
   */
  compiled_frame_id: string;
  /**
   * Its byte-stable frame hash (`sha256:<hex>`).
   */
  frame_hash: string;
}

/**
 * One chat message handed to a provider, including any tool calls the
 * assistant made or tool results being reported back.
 */
export interface CompletionMessage {
  /**
   * Multimodal inputs (images, documents, audio, video) accompanying a
   * user message. `serde(default)` + skip-when-empty so envelopes
   * serialized before this field existed still parse and text-only
   * messages serialize byte-for-byte as they always have (the prompt-cache
   * stability contract).
   */
  attachments?: Attachment[];
  /**
   * The message text. Empty on an assistant message that only made tool
   * calls, and on a `Tool` message whose payload rides `tool_results`.
   */
  content?: string;
  /**
   * Who authored this message.
   */
  role: MessageRole;
  /**
   * Tool calls the assistant made in this message.
   */
  tool_calls?: ToolCall[];
  /**
   * Tool results being reported back, each naming the call it answers.
   */
  tool_results?: ToolResult[];
}

/**
 * A completion request — the same shape regardless of which provider
 * adapter ultimately serves it.
 */
export interface CompletionRequest {
  /**
   * Reasoning effort for models that support a thinking mode.
   */
  effort?: ReasoningEffort | null;
  /**
   * Upper bound on generated tokens. `None` uses the provider default.
   */
  max_output_tokens?: number | null;
  /**
   * The conversation so far, in order, starting with the system message.
   */
  messages: CompletionMessage[];
  /**
   * Optional sampling/routing overrides ([`GenerationParams`]) — each
   * adapter forwards the subset its dialect supports.
   */
  params?: GenerationParams | null;
  /**
   * Whether the model's thinking/extended-reasoning mode is enabled at
   * all. `Some(true)` asks the adapter to turn thinking on (at
   * `effort`'s level, or the adapter's default level when `effort` is
   * `None`); `Some(false)` asks it to suppress thinking; `None` keeps
   * the provider's default behavior (exactly the pre-field wire shape).
   */
  reasoning?: boolean | null;
  /**
   * Sampling temperature. `None` uses the provider default.
   */
  temperature?: number | null;
  /**
   * Tool schemas the model may call, in the engine's one internal shape
   * ([`ToolSchema`]); each adapter translates to its own dialect.
   */
  tools?: ToolSchema[];
}

/**
 * Token accounting for a single completion, normalized across providers
 * into one envelope: normalization lives in the adapter, not the caller.
 */
export interface CompletionUsage {
  /**
   * Tokens WRITTEN to the provider's prompt cache by this call
   * (Anthropic `cache_creation_input_tokens`, Bedrock
   * `cacheWriteInputTokens`). Unlike `cached_input_tokens` this is NOT a
   * subset of `input_tokens` — providers report writes separately, and
   * folding them into `input_tokens` would change cost accounting
   * (`Pricing::cost_usd` bills them on their own line at the catalog's
   * `cache_write_usd_per_mtok`, so folding would double-charge). 0 for providers
   * that never report cache writes (the OpenAI-compatible dialects).
   * `serde(default)` so envelopes serialized before this field existed
   * still parse.
   */
  cache_write_tokens?: number;
  /**
   * The subset of `input_tokens` served from the provider's prompt cache
   * — billed at the cache-read rate, not the input rate. 0 for providers
   * that never report a cache hit.
   */
  cached_input_tokens?: number;
  /**
   * Tokens the prompt cost, cache hits included.
   */
  input_tokens: number;
  /**
   * Tokens the model generated.
   */
  output_tokens: number;
  /**
   * The subset of `output_tokens` the model spent on reasoning, when the
   * provider breaks it out (`completion_tokens_details.reasoning_tokens`
   * on the OpenAI-compatible dialects, `output_tokens_details` on the
   * Responses API).
   *
   * `None` means NOT REPORTED, and is not the same fact as `Some(0)`.
   * Anthropic's Messages API folds thinking into `output_tokens` with no
   * breakdown at all, so every anthropic.rs call records `None` — while a
   * reasoning-capable model that genuinely did no thinking on a call
   * records `Some(0)`. Collapsing the two would report "this model never
   * thinks" for the entire Anthropic-direct route, which is the same class
   * of error as reading an unfilled placeholder column as a measured zero.
   *
   * Already inside `output_tokens` for billing on every provider that
   * reports it, so it is a diagnostic breakdown and never its own cost
   * line.
   */
  reasoning_tokens?: number | null;
  /**
   * The adapter observed the provider's authoritative usage-bearing
   * terminal response. This is explicit because a legitimate call can
   * report all zero counters, while a missing usage frame can accompany
   * non-empty streamed text. Legacy envelopes fail closed.
   */
  reported?: boolean;
}

/**
 * A context frame as cited in a `ContextRecall` event. `citation_label`
 * is mandatory and human-readable; the raw `id` (when the frame is
 * materialized at all) belongs only in inspectable detail views, never as
 * the primary identifier (L-C4).
 */
export interface ContextFrameRef {
  /**
   * The registry id (`blk_…`) of this frame as a context block, when
   * receipts are enabled. Joins the frame to its manifest membership and,
   * for memory frames, to the write→citation loop (spec §5.3, §9). Absent on
   * streams recorded before receipts existed.
   */
  block_id?: string | null;
  /**
   * The human-readable citation every surface renders, e.g.
   * `engine step-driver (driver.rs)`. Mandatory by design (L-C4).
   */
  citation_label: string;
  /**
   * `"sha256:<hex>"` of the exact injected text. The digest rides the wire;
   * the content itself is journaled only locally (never exported), closing
   * the recall-content gap G1 without widening the content-free export.
   */
  content_digest?: string | null;
  /**
   * The provider's own frame id, when the frame was materialized at all.
   * A detail-view identifier only — never the primary one a surface shows.
   */
  id?: string | null;
  /**
   * The protocol frame kind (`symbol`, `memory`, `graph`, ...).
   */
  kind?: string;
  /**
   * The most-derived provenance method, when declared.
   */
  method?: string | null;
  /**
   * The CGP provider leg that returned the frame. Empty only when reading
   * a stream recorded before provider provenance was added.
   */
  provider?: string;
  /**
   * The original source named by the frame's provenance chain. This is
   * deliberately distinct from [`Self::provider`]: a host adapter may be
   * `workspace-memory` while the record source remains `stella-context`.
   */
  source: string;
  /**
   * The engine's estimate of what injecting this frame cost the prompt.
   */
  token_cost: number;
  /**
   * Canonical source URI when the frame supplied one.
   */
  uri?: string | null;
}

/**
 * One provider's contribution to a recall's cost, as the CGP usage report
 * defines it (`docs/spec/adaptive-context/context-reuse.md` §2 `ProviderUsage`).
 *
 * Distinct from [`ProviderShare`], which counts only the frames that *won*
 * fusion and reached the prompt. This counts what the provider **served to
 * the host** and what the host **rejected** (a budget lie, a consent gate, a
 * failed leg), with the token cost behind each — the difference between "what
 * did the model see?" and "what did this turn cost, and who drove it?".
 *
 * Content-free by construction: a provider id, three numbers. No frame
 * titles, bodies, URIs, or query text ever enter this type.
 */
export interface ContextProviderUsage {
  /**
   * Frames the host dropped whole, e.g. a provider that misdeclared cost.
   */
  frames_rejected: number;
  /**
   * Frames accepted — they passed consent, the timeout, and the
   * budget-honesty audit.
   */
  frames_served: number;
  /**
   * The host's routing/consent key for the serving provider.
   */
  provider_id: string;
  /**
   * This provider's contribution to `budget_consumed`.
   */
  token_cost: number;
}

/**
 * The per-request roll-up of what one context recall cost: the envelope a metering pipeline bills from, and the answer to "what did this turn's context cost, and which sources drove it?". Per-provider detail rides in `providers`; `budget_consumed` is the total the host admitted.
 */
export interface ContextUsage {
  /**
   * The report's accounting snapshot (RFC 3339), stamped by the host. This
   * is the *accounting event* time, never the query's bi-temporal `as_of`
   * retrieval pin — two different clocks.
   */
  as_of: string;
  /**
   * The summed `token_cost` of every served frame.
   */
  budget_consumed: number;
  /**
   * The query's `max_tokens` — the budget this recall was allowed.
   */
  budget_requested: number;
  /**
   * One entry per provider the query reached.
   */
  providers: ContextProviderUsage[];
}

/**
 * What a diff-producing task means by done: at least one check, always. An empty array is refused rather than accepted as a contract that promises nothing.
 */
export type DefinitionOfDone = Check[];

/**
 * Why a candidate's work did not reach the real tree.
 *
 * Exhaustive over the pipeline's decision: the three arms are the two
 * `Withhold` paths of `pipeline::delivery::decide` plus the one way a
 * sanctioned delivery can still fail at the git layer.
 */
export type DeliveryDecline = "nothing_created" | "integrity_refusal" | "adopt_failed";

/**
 * What the pipeline did with the winning candidate's workspace, and why. Internally tagged on `outcome`, so a reader selects an arm by that field rather than by the presence or absence of a sibling key.
 */
export type DeliveryOutcome = {
  /**
   * Files that did not exist in the real tree before this delivery.
   */
  created: number;
  /**
   * Files the delivery removed.
   */
  deleted: number;
  /**
   * Lines added across every delivered file.
   */
  lines_added: number;
  /**
   * Lines removed across every delivered file.
   */
  lines_removed: number;
  /**
   * Files whose contents changed.
   */
  modified: number;
  outcome: "delivered";
  /**
   * Whether a **passing verdict** stood behind the work.
   *
   * `false` is the ordinary case for a run that ran out of clock or came
   * to rest on the revise rung, and it is deliberately not a reason to
   * withhold: a verdict is a claim about the work, not what decides
   * whether the work exists (#2927/#2943). Delivery never implies proof,
   * and this field is what keeps the write from reading as a pass.
   */
  proven: boolean;
} | {
  outcome: "declined";
  /**
   * Why. Typed, because a caller that has to branch on prose is
   * parsing prose.
   */
  reason: DeliveryDecline;
};

/**
 * Why a tool call failed, as a closed machine-readable set. A tool result's `message` is prose written for the model to retry against; this is the axis a measurement needs, because a per-tool error rate cannot mean anything while a tool defect, model misuse and a policy refusal all count as the same failure. The values partition failures by whose problem they are: the model's (`invalid_input`, `not_found`), the policy plane's (`permission_denied`, `refused_by_policy`), the world's (`timeout`, `environment`), or the agent's own (`internal`). There is deliberately no `abandoned` class: a call whose turn ended before it returned produced no tool result at all. An unrecognized token reads as `other`, and re-serializing writes `other` rather than the original.
 */
export type ErrorClass = "invalid_input" | "not_found" | "permission_denied" | "refused_by_policy" | "timeout" | "environment" | "internal" | "other";

/**
 * What happened to a file in a [`AgentEvent::FileChange`] event.
 *
 * Both live producers measure a tree against a tree, so every kind emitted
 * today is a mutation — see [`Self::Read`] for the one that is not, and why
 * it stays in the space anyway.
 */
export type FileChangeKind = "read" | "created" | "modified" | "deleted";

/**
 * Why the model stopped generating, normalized across providers. Lets the
 * engine tell a natural stop from a truncation (`Length`) so an empty or
 * cut-off turn is surfaced to the user instead of being recorded as a clean
 * completion (the "turn ends with no feedback" defect).
 */
export type FinishReason = "stop" | "length" | "tool_calls" | "content_filter";

/**
 * What the flip oracle found — and, when it found no flip, whether anything
 * was ever in a position to produce one (#2556).
 *
 * **A bool could not carry this, and the missing state is the one that
 * matters.** `flip_achieved: false` meant both "a tracked command ran and did
 * not go fail→pass" (a real negative about the work) and "no command was ever
 * tracked, so nothing could have flipped" (a statement about the instrument,
 * not the work). #2531 fixed that conflation in the *verifier prompt* by
 * rendering `unobserved`; the telemetry surface kept the bool, so anything
 * reading `result.json` — bench scoring included — reproduced exactly the
 * false negative the prompt had stopped making.
 *
 * The fact was *recoverable* from `flip_achieved: false` plus
 * `tracked_command: null`, and that is precisely the objection: a summary
 * layer that silently conflates "not measured" with "measured and failed"
 * unless the reader knows to join two fields is the failure mode this
 * repository's bench discipline exists to prevent. Same reasoning as
 * [`LadderSnapshot::diff_coverage`], which is a token for the same reason.
 *
 * Serialises as `unobserved` / `not_achieved` / `achieved`. A legacy bool is
 * still accepted on the way in — see the `Deserialize` impl — so snapshots
 * recorded before this existed keep parsing.
 */
export type FlipOutcome = "unobserved" | "not_achieved" | "achieved";

/**
 * Optional sampling/routing parameter overrides riding a
 * [`CompletionRequest`]. Every field is independently optional —
 * "include" semantics: `None` leaves the provider's own default in place,
 * `Some` puts the value on the wire. Each adapter forwards the subset its
 * dialect supports and silently drops the rest (a param the provider
 * can't express must never fail the request).
 */
export interface GenerationParams {
  /**
   * Penalize tokens by their frequency in the text so far.
   */
  frequency_penalty?: number | null;
  /**
   * Penalize tokens that have appeared at all in the text so far.
   */
  presence_penalty?: number | null;
  /**
   * Multiplicative repetition penalty (>1 discourages, <1 encourages).
   */
  repetition_penalty?: number | null;
  /**
   * Random seed for deterministic outputs, where supported.
   */
  seed?: number | null;
  /**
   * Which capacity tier to route to ([`ServiceTier`]).
   */
  service_tier?: ServiceTier | null;
  /**
   * Limit sampling to the k highest-probability tokens.
   */
  top_k?: number | null;
  /**
   * Nucleus sampling: cumulative-probability cutoff.
   */
  top_p?: number | null;
  /**
   * How much detail to ask for ([`Verbosity`]).
   */
  verbosity?: Verbosity | null;
}

/**
 * What a `HunkReview` gate presents for per-hunk approval before a mutating
 * tool call writes anything (#1265).
 *
 * The hunks are a **flat, ordered list across every file the call touches**,
 * not a per-file tree: the reviewer's answer is a set of indices into this
 * list, and one flat coordinate space is what keeps that answer unambiguous
 * when two files change in the same call.
 */
export interface HunkProposal {
  /**
   * Every proposed hunk, in file-then-position order.
   */
  hunks: ProposedHunk[];
  /**
   * Correlates the decision — and the synthetic `ToolResult` that clears the
   * card — back to this review. Distinct from the model's tool-call id: one
   * call raises one review, but the review is the host's object, not the
   * model's.
   */
  id: string;
  /**
   * The tool whose write is being reviewed (a custom or MCP write tool) —
   * the card names it so a reviewer knows what declining costs.
   */
  tool: string;
}

/**
 * Who settles a check.
 *
 * The axis SPEC 1's first thesis prices: deterministic work never reaches a
 * model and costs `$0.00`.
 */
export type Judge = "deterministic" | "model";

/**
 * Which rung of the evidence ladder a `verdict` event actually came to rest on. The verdict's `passed` and `deterministic` flags cannot express this on their own: several distinct outcomes share the same pair of booleans, so the rung is carried explicitly rather than inferred. Read this field, not the flags, when you need to know what was established.
 */
export type LadderRung = "submit_fast" | "revise" | "nothing_attempted" | "unverifiable" | "unverified" | "witness_unsatisfiable" | "waived";

/**
 * The deterministic evidence the ladder decided a verdict from, snapshotted
 * at decision time (#865). Everything here existed when the decision was
 * made; attaching it to the verdict is what makes "why?" answerable later
 * without re-deriving — and re-deriving is exactly what a replay of an
 * event stream cannot do, because the world the probes read is gone.
 */
export interface LadderSnapshot {
  /**
   * Whether the diff probe could read the working tree at all.
   */
  diff_available: boolean;
  diff_budget: number;
  /**
   * Whether the test run executed the lines the change added (#1291):
   * `covered`, `not_covered`, or `unmeasured`.
   *
   * A string rather than a bool because the third value is the whole
   * point. "The test did not run the changed lines" and "no coverage tool
   * could say" are different findings, and only the first is about the
   * work — collapsing them into `Option<bool>` would put the reader back
   * where #973 found them, reading a statement about the instrument as a
   * statement about the world.
   *
   * Absent on snapshots recorded before this existed, and on every run
   * where no coverage probe was wired — which a reader must treat as
   * `unmeasured`, never as either verdict.
   */
  diff_coverage?: string | null;
  /**
   * Lines changed, and the budget they were judged against.
   */
  diff_lines: number;
  /**
   * Command chains this round that reported an error while exiting `0`
   * (#2125) — the shape a cited measurement can silently stand on.
   *
   * Unlike [`Self::verify_done_flip`] and [`Self::no_test_surface`] this
   * one never changes a verdict: an errored probe makes a cited quantity
   * *unsubstantiated*, not
   * *disproven*, so it informs the verifier's opinion and withholds
   * nothing. It is here because the question it answers — how often a run
   * cites a number that stood on a broken chain — is one only aggregate
   * traces can answer, and aggregate traces read this snapshot. One-way:
   * `0` is "the closed signature vocabulary matched nothing", never "this
   * round's commands were clean".
   */
  errored_commands?: number;
  /**
   * The flip oracle's finding — after the confirmation run, so an
   * unconfirmed flip reads [`FlipOutcome::NotAchieved`] here with
   * `unstable_flip: true`.
   *
   * Three states rather than a bool, for the reason [`Self::diff_coverage`]
   * is a token rather than an `Option<bool>`: the third value is the whole
   * point (#2556). See [`FlipOutcome`].
   */
  flip: FlipOutcome;
  /**
   * A would-be flip was refused because the passing run named its tests
   * and none of the baseline's failing tests were among them — the pass
   * demonstrably fixed a *different* failure (#867), most concretely a
   * deleted or renamed failing test. `serde(default)` so pre-#867
   * snapshots keep parsing.
   */
  flip_refused_different_failure?: boolean;
  /**
   * Dispatched tool calls capable of changing the workspace.
   */
  mutating_actions: number;
  /**
   * New lint/typecheck errors/warnings over the pre-execution baseline
   * (#861); zeros when the probe was unavailable or never consulted.
   */
  new_diag_errors: number;
  new_diag_warnings: number;
  /**
   * Positive claim that this round had NO tracked test command at all
   * (#2129) — neither a configured `--test-command` nor an authored
   * witness — so "no flip" is a demand the task structurally cannot meet.
   *
   * **Recorded only — this field steers nothing.** It once turned a
   * fallback FAIL into an upward abstention, but the fallback it fed
   * (`verify::heuristic_fallback`) was deleted with the model verdict in
   * #2584, and nothing replaced the read: `ladder_decision` does not
   * consult it, and two otherwise-identical inputs differing only here
   * return the same decision. It survives because the question it answers —
   * how often a run is asked for a flip on a task that has no test surface
   * to produce one — is one only aggregate traces can answer, and aggregate
   * traces read this snapshot. Whether to retire it or give it a decision
   * role is #2638.
   *
   * `false` on snapshots recorded before this existed, which is the
   * conservative reading — the dispensation is never assumed.
   */
  no_test_surface?: boolean;
  /**
   * The oracle's observations in order (baseline, candidate runs, the
   * pre-submit confirmation). Infra runs are absent by construction.
   */
  oracle_trace?: OracleObservation[];
  /**
   * The rung this verdict came to rest on (#1043). Absent on events
   * recorded before it existed, which is the one case a reader must handle
   * as "unknown" rather than guess at — see [`LadderRung`] for why the
   * guess is not available.
   */
  rung?: LadderRung | null;
  /**
   * Why the test run observed nothing, when it didn't (`timed_out`,
   * `infra_failure`) — the #860 distinction between "the suite failed"
   * and "the suite could not be watched".
   */
  test_infra?: string | null;
  /**
   * Touched-tests result: `None` is "could not be observed", not a pass.
   */
  touched_tests_passed?: boolean | null;
  /**
   * The normalized test command the flip oracle locked onto, when it
   * armed at all.
   */
  tracked_command?: string | null;
  /**
   * A flip was observed but its confirmation re-run did not pass (#859).
   */
  unstable_flip: boolean;
  /**
   * Whether the model that graded this verdict was independent of the
   * worker that produced the work (#1795): `Some(false)` is a self-graded
   * verdict — the verdict call resolved to the worker's own model — and
   * `Some(true)` a distinct grader.
   *
   * A structured fact rather than the once-per-run prose caveat, because
   * the caveat scrolls away while the verdict is stored: a reader of a
   * stored verdict must be able to see the grader was not independent
   * without the transcript. Absent when no model verdict was bought (the
   * deterministic, waived, and abstaining rungs — nothing graded, so
   * independence is not a fact about them), when the worker's own
   * resolution failed (nothing to compare against), and on snapshots
   * recorded before this existed.
   */
  verifier_independent?: boolean | null;
  /**
   * The worker's own `verify_done` tool run printed `WITNESS CONFIRMED`
   * this round (#2129): a deterministic fail-on-baseline / pass-on-new
   * shadow observation the pipeline's flip oracle cannot make, because it
   * tracks only its own command.
   *
   * On the wire because it **changes the verdict** and nothing else here
   * records which channel carried it: it is a completion receipt in its own
   * right, able to carry [`LadderRung::SubmitFast`] with no pipeline flip
   * beside it (#2618), so a reader of a stored verdict otherwise sees a
   * deterministic pass whose reasoning cites `verify_done` with no
   * structured field to count. The two receipts pin different baselines —
   * this one the ref `verify_done` chose, `flip_achieved` the pipeline's
   * pre-execution snapshot — which is why they stay separate fields
   * rather than one "flipped" flag. `false` covers both
   * "no confirmation was observed" and "recorded before this field
   * existed"; like every other one-way channel here it is never a claim
   * that the worker's witness failed.
   */
  verify_done_flip?: boolean;
  /**
   * The witness-tamper check's result: `None` when no witness was armed,
   * `Some(true)` when every witness artifact matched its pinned identity.
   * `Some(false)` never reaches a verdict — tampering aborts the
   * candidate — so its presence here is the *stated* proof the check ran.
   */
  witness_intact?: boolean | null;
  /**
   * The mutation audit's finding (#870): `Some(true)` = the witness
   * failed under at least one trivial mutant of the changed lines (it
   * constrains the change); `Some(false)` = it stayed green under every
   * observed mutant (tautological — the deterministic credit was
   * withheld); `None` = the check never ran.
   */
  witness_mutation?: boolean | null;
}

/**
 * One block's membership in a step's manifest (spec §5): its id, its cache
 * zone at that step, its estimated token cost, and how long it has been
 * resident. Residency × cost is what makes cost-of-carry a real number.
 */
export interface ManifestEntry {
  block_id: string;
  /**
   * Cache position class relative to the last stable breakpoint. Defaults to
   * [`CacheZone::Cacheable`] on a manifest recorded before the field existed
   * — an assumption, not an observation, so a cache-attribution pass over
   * pre-field manifests is reading the default rather than a measured zone.
   */
  cache_zone?: CacheZone;
  /**
   * The tool call this *occurrence* belongs to, for `ToolCall`/`ToolResult`
   * blocks. Content-addressing collapses byte-identical blocks onto one
   * `block_id`, so [`BlockOrigin::call_id`] only ever names the first call
   * to mint it — two `git status` runs with identical output register once.
   * Recording the id per manifest entry keeps the attribution complete:
   * joining a compaction event's evicted/deduped/aged `block_id`s against
   * the manifests that carried them answers "which *calls* left context"
   * without under-reporting the duplicates. `None` for non-tool blocks, and
   * on manifests recorded before this field existed.
   */
  call_id?: string | null;
  /**
   * Which `CompletionMessage` this block belonged to, by position in the
   * sent sequence. Event-granular blocks (a tool message's several results,
   * an assistant message's text + calls) share one `message_index`, so
   * reconstruction regroups them back into the exact message boundaries
   * rather than inferring them from kinds (spec §5.1). `0` on manifests
   * recorded before reconstruction existed.
   */
  message_index?: number;
  /**
   * The step this block first entered a manifest — drives cost-of-carry.
   */
  resident_since_step: number;
  token_cost: number;
}

/**
 * A completed media artifact: id + kind + where it landed on disk.
 */
export interface MediaArtifactRef {
  /**
   * The artifact id, matching the `artifact_id` its
   * [`AgentEvent::MediaProgress`] events carried.
   */
  id: string;
  /**
   * What was produced.
   */
  kind: MediaKind;
  /**
   * Human label for citation/display.
   */
  label: string;
  /**
   * Path under `.stella/artifacts/` (the generation tools may never
   * write outside it).
   */
  path: string;
}

/**
 * Lifecycle of an async media job. `Failed` carries the reason inline —
 * a failed job must never be distinguishable only by the absence of a
 * success event.
 */
export type MediaJobState = {
  state: "queued";
} | {
  state: "running";
} | {
  state: "succeeded";
} | {
  /**
   * Why it failed, inline — a failed job is never signalled only by
   * the absence of a success event.
   */
  reason: string;
  state: "failed";
};

/**
 * Which kind of media artifact a job produces.
 */
export type MediaKind = "image" | "svg" | "video";

/**
 * Who authored one message in the conversation. Tool results are
 * represented as a `Tool` message carrying the `tool_call_id` they answer,
 * so every dialect adapter has one place to translate role framing.
 */
export type MessageRole = "system" | "user" | "assistant" | "tool";

/**
 * Concrete purpose of one provider call. This is more precise than the
 * router's tier role: repair and guidance calls must remain distinguishable
 * in the paid-call ledger even when they share a provider/model.
 *
 * This vocabulary grows, and it is **not** forward-tolerant: [`Self::Unknown`]
 * is the `serde(default)` for an *absent* `role`, not a `serde(other)`
 * catch-all for an unrecognized one. A role token this build has never seen
 * fails its whole event — `step_usage`, `step_manifest`, `usage_incomplete` —
 * because a known `"type"` with a body that does not fit stays a hard error by
 * design (see the module docs). Adding a variant here is therefore a
 * one-directional change in a way adding an [`AgentEvent`] variant no longer
 * is.
 */
export type ModelCallRole = "unknown" | "triage" | "research" | "plan" | "plan_repair" | "witness_author" | "witness_repair" | "worker" | "distress_guidance" | "verdict" | "agent_author" | "skill_author" | "domain_inference" | "reflection" | "summarization";

/**
 * One flip-oracle observation, in the order the pipeline made it — together
 * they are the oracle trace a verdict carries (#864).
 */
export interface OracleObservation {
  /**
   * Whether the tracked command's assertions passed. Infra outcomes never
   * appear here — an unobservable run is not an oracle observation.
   */
  passed: boolean;
  /**
   * Which tree the observation ran against.
   */
  tree: ProofTree;
}

/**
 * The accounting an adapter had already observed when an attempt died before its terminal usage frame arrived. A mid-stream disconnect is not a total loss of accounting: dialects that report the prompt's cost up front have already delivered exact input, cache-read and cache-write counts by the time generation is cut. Every field is a LOWER BOUND on real spend and never a substitute for a provider-attested total, which is why such a record can never be mistaken for settled accounting.
 */
export interface PartialUsage {
  /**
   * `usage` priced at the serving model's catalog rates, or `0.0` when the
   * adapter had no pricing row for the model. Never provider-attested.
   */
  cost_usd: number;
  /**
   * Whether the input-side counts came from the provider's own frame
   * rather than a local estimate. `true` is the common case for
   * Anthropic-shaped streams and `false` for the OpenAI-shaped ones, which
   * send usage only at the end — the distinction a reader needs before
   * treating `usage.input_tokens` as fact.
   */
  input_reported?: boolean;
  /**
   * Counts observed before the failure. Input-side figures are the
   * provider's own when the dialect front-loads them; `output_tokens` is
   * whatever the last usage frame stated, or an estimate over the text
   * that actually arrived when no such frame did.
   */
  usage: CompletionUsage;
}

/**
 * What kind of policy-plane decision a [`AgentEvent::PolicyDecision`]
 * records (receipts spec §6.4).
 */
export type PolicyKind = "evaluated" | "blocked" | "approval_requested" | "secret_detected";

/**
 * A pull request's status as observed by the fleet monitor. Reconciled
 * against the live source before rendering, never served from cache
 * alone (L-V3).
 */
export type PrStatus = "draft" | "open" | "merged" | "closed";

/**
 * One step of the proof a turn builds for its own work, in the order the pipeline makes the observation. Carried by the `proof` event. Note the forward-compatibility asymmetry: a reader that does not know the `proof` event preserves it whole as an `unknown` event, but a reader that knows `proof` and meets a future `kind` here fails the whole event -- this nested vocabulary is closed and has no unknown step.
 */
export type ProofStep = {
  kind: "assurance";
  /**
   * Whether a model verifier was called for on inconclusive evidence.
   *
   * Aliased: this field shipped as `judge`, and every recorded session
   * and golden fixture spells it that way. Renaming it without the
   * alias makes those streams unparseable — which is exactly what the
   * golden fixtures caught.
   */
  verifier: boolean;
  /**
   * Whether an independently authored witness test was called for.
   */
  witness: boolean;
} | {
  /**
   * Size of the change the answer was read from.
   */
  diff_lines: number;
  kind: "warrant";
  /**
   * The stated reason when no test is warranted; `None` when one is.
   */
  reason?: string | null;
  required: boolean;
} | {
  /**
   * The command that arms the flip oracle.
   */
  command: string;
  /**
   * The accepted artifact's fingerprint — what tamper exclusion compares
   * against for the rest of the run.
   */
  fingerprint: string;
  kind: "witness_authored";
  path: string;
} | {
  kind: "witness_unavailable";
  reason: string;
} | {
  kind: "verification_unavailable";
  reason: string;
} | {
  kind: "verification_unproven";
  reason: string;
} | {
  command: string;
  kind: "oracle";
  passed: boolean;
  /**
   * Which candidate replay this observation is (1-based). `None` on
   * baseline runs and on single-run oracles.
   */
  run?: number | null;
  /**
   * How many passing candidate replays the flip requires, when the
   * oracle runs more than one.
   */
  runs_required?: number | null;
  /**
   * The deterministic seed the replay pinned, when one was pinned.
   */
  seed?: number | null;
  tree: ProofTree;
} | {
  /**
   * Which candidate degraded (1-based, [`ProofStep::Oracle::run`]'s
   * convention).
   */
  candidate: number;
  kind: "verdict_degraded";
  /**
   * The stated reason a model verdict could not be rendered.
   */
  reason: string;
} | {
  kind: "triage_degraded";
  /**
   * The stated reason no model classification was available: the call
   * timed out at its ceiling, failed, could not be routed, or answered
   * off-protocol.
   */
  reason: string;
};

/**
 * Which code state a `ProofStep::Oracle` observation was made against.
 *
 * The distinction is the whole content of a flip: the same command failing in
 * `Baseline` and passing in `Candidate` is proof, while either result twice
 * against one tree is a tree observed twice.
 */
export type ProofTree = "baseline" | "candidate";

/**
 * One reviewable hunk: which file, what it does, and how it renders.
 */
export interface ProposedHunk {
  /**
   * The hunk as unified-diff text, `@@` header included — ready for
   * `stella_tui::diff::body_lines` with no further parsing.
   */
  diff: string;
  /**
   * Lines this hunk adds. Authoritative: taken from the decomposition, never
   * re-counted from `diff` (which is capped and carries context lines).
   */
  lines_added: number;
  /**
   * Lines this hunk removes, on the same terms.
   */
  lines_removed: number;
  /**
   * Workspace-relative path of the file this hunk changes.
   */
  path: string;
}

/**
 * One provider's share of a recall's frame mix.
 */
export interface ProviderShare {
  /**
   * How many of the recall's frames this provider contributed — the ones
   * that won fusion and reached the prompt, not the ones it served.
   */
  frames: number;
  /**
   * The CGP provider leg the frames came from.
   */
  provider: string;
}

/**
 * Reasoning effort forwarded to models with a thinking/extended-reasoning
 * mode. One enum, mapped per-adapter to the provider's own parameter name
 * ("reasoning_param").
 */
export type ReasoningEffort = "low" | "medium" | "high" | "xhigh" | "max";

/**
 * What a `ScopeReview` gate presents for approval before a large plan
 * executes (L-E5).
 *
 * The fields after `estimated_cost_usd` are the scope-card grid's facts
 * (repo/branch, read/write globs, shell policy) — all additive
 * (`serde(default)`), so streams recorded before they existed parse with
 * every one absent, and a proposal that names none serializes exactly as it
 * always has.
 */
export interface ScopeProposal {
  /**
   * The branch the work lands on, when named.
   */
  branch?: string | null;
  /**
   * Projected spend, when the planner could estimate one.
   */
  estimated_cost_usd?: number | null;
  /**
   * How many files the plan expects to touch — the magnitude the gate's
   * thresholds are compared against.
   */
  estimated_files: number;
  /**
   * Path globs the plan reads beyond its write set. Empty = not stated.
   */
  read_globs?: string[];
  /**
   * The repository the scope binds to (`owner/name`, or a workspace
   * path), when the planner named one.
   */
  repo?: string | null;
  /**
   * The shell policy in force for the run (e.g. `allowlisted`,
   * `read-only`, `none`), when stated.
   */
  shell_policy?: string | null;
  /**
   * The plan's steps, in the order the worker will attempt them.
   */
  steps: string[];
  /**
   * One line describing the work, for the approval prompt's headline.
   */
  summary: string;
  /**
   * Path globs the plan intends to WRITE within. Empty = not stated.
   */
  write_globs?: string[];
}

/**
 * Provider service tier: `Priority` routes to faster paid-tier capacity,
 * `Flex` to cheaper capacity with slower response times. Only applied by
 * providers that support tiered service; others use their default tier.
 */
export type ServiceTier = "auto" | "default" | "flex" | "priority";

/**
 * The name of a stage boundary — an OPEN vocabulary. The host's own boundaries are listed in `examples`; an installed plugin may contribute a stage under any other name, so a consumer must branch on the names it knows and keep a default arm rather than treating an unlisted value as invalid.
 */
export type StageName = string;

/**
 * Whose stage boundary an [`AgentEvent::Stage`] reports (#3398).
 *
 * Deliberately **not** `#[serde(default)]`. A default would silently claim
 * one scope for every historical recording, and half of them are the other
 * one — a decode ambiguity that would live in the fixtures forever. A
 * recording written before this field existed decodes through
 * [`AgentEvent::Unknown`] instead, which says "I do not know what this is"
 * rather than guessing wrong.
 */
export type StageScope = "turn" | "run";

/**
 * What put the message into the turn, for [`AgentEvent::Steered`].
 *
 * Three different things emit that event and they are different pathologies
 * with different remedies: a person interrupting, the stuck-loop rung, and
 * the stalled-turn rung (#2022). Before this field the only way to tell them
 * apart was to match the English prose — and `STALL_STEER_PREFIX` *extends*
 * `LOOP_STEER_PREFIX`, so even the prefix test was a substring test on a
 * sentence (#3622). That is precisely the practice
 * [`AgentEvent::LoopDetected`] was introduced to end for the loop rung.
 *
 * The consequence was that "the turn was steered" was one bucket, so no
 * consumer could answer *how often does the stall rung actually fire* — the
 * question that decides whether the rung earns its keep.
 */
export type SteerCause = "unknown" | "user" | "loop" | "stall";

/**
 * One point in a sub-agent's lifecycle. Exactly one `Started` and exactly
 * one `Finished` are emitted per spawn, in that order, even when the child
 * is refused before its first model call (a refusal still brackets, so a
 * consumer folding these never sees an unclosed child).
 */
export type SubAgentPhase = {
  /**
   * Stable id for this child, unique within the parent turn.
   */
  agent_id: string;
  /**
   * The USD ceiling actually carved for this child, after clamping
   * the request against the parent's remaining headroom. `None` when
   * no axis anywhere bounds it.
   */
  budget_usd?: number | null;
  /**
   * Nesting depth: `1` for a child of the top-level turn.
   */
  depth: number;
  /**
   * The child's task, truncated for display. Never the full prompt —
   * the prompt can be large and the event stream is journaled.
   */
  instruction_preview: string;
  phase: "started";
  /**
   * Whether the child may mutate the workspace. `false` (the
   * default) means it ran behind a read-only view of the parent's
   * tools and could not have changed anything.
   */
  write_access: boolean;
} | {
  /**
   * How many messages the child's private transcript grew to — the
   * context the parent did NOT have to carry. This is the primitive's
   * whole value proposition, reported rather than asserted.
   */
  absorbed_messages: number;
  /**
   * Matches the `Started` this closes.
   */
  agent_id: string;
  /**
   * The child's total spend, already settled into the parent's guard
   * by the time this is emitted.
   */
  cost_usd: number;
  phase: "finished";
  /**
   * Present only when `status` is not `Completed`: why it ended.
   */
  reason?: string | null;
  status: SubAgentStatus;
  /**
   * Model calls the child made.
   */
  steps: number;
  /**
   * The report handed back to the parent — the ONLY thing that may
   * enter the parent's transcript, and already clamped to the spec's
   * character cap.
   */
  summary: string;
  /**
   * Whether the report was cut to fit that cap. Never silent: a
   * consumer can tell an exhaustive answer from a clipped one.
   */
  truncated: boolean;
};

/**
 * How a sub-agent's turn ended. The parent reasons about this as data — a
 * failed child is never an error that kills the parent turn.
 */
export type SubAgentStatus = "completed" | "incomplete" | "refused";

/**
 * What a task means by done (SPEC 7.1).
 */
export type TaskContract = {
  kind: "read_only";
} | {
  checks: DefinitionOfDone;
  kind: "definition_of_done";
};

/**
 * One entry on the turn's task board (the `task_*` tools). The board is
 * session-scoped working state — what the agent has planned, is doing,
 * and has finished — mirrored to the store for cross-session findability.
 */
export interface TaskItem {
  /**
   * What this task means by done (SPEC 7.1).
   *
   * `None` is *nobody has said yet*, and is deliberately not the same fact
   * as [`TaskContract::ReadOnly`], which is *somebody looked and there is
   * nothing to prove*. A board that collapsed the two would let an
   * undeclared task close on the same terms as one declared harmless —
   * which is the self-report [`TaskContract`] exists to end.
   *
   * Optional because the board predates contracts and a session may still
   * create a task without one; `stella_core::tasks` refuses the *close*, not
   * the creation, so an undeclared task is visible on the board rather than
   * rejected at the door.
   */
  contract?: TaskContract | null;
  /**
   * What needs to be done, if the creator elaborated beyond the subject.
   */
  description?: string | null;
  /**
   * Stable per-session ordinal id ("1", "2", …) — what `task_complete`
   * / `task_cancel` / `task_assign` reference.
   */
  id: string;
  /**
   * Which agent lane owns the task: `None` until claimed, `Some("lead")`
   * for the lead, or the sub-agent lane id once `task_assign` spawned a
   * dedicated worker for it.
   */
  owner?: string | null;
  /**
   * Where the task is in its lifecycle ([`TaskStatus`]).
   */
  status: TaskStatus;
  /**
   * Imperative title ("Fix the auth redirect loop").
   */
  subject: string;
}

/**
 * Lifecycle of a `TaskItem`. Terminal states are `Completed` and
 * `Cancelled`; a cancelled task keeps its row (the board is an audit
 * surface, not just a scheduler).
 */
export type TaskStatus = "pending" | "in_progress" | "completed" | "cancelled";

/**
 * One tool invocation the model requested.
 */
export interface ToolCall {
  /**
   * Stable id correlating this call to its eventual `ToolResult`.
   */
  call_id: string;
  /**
   * The arguments, as the model produced them. Runtime data: never trust
   * the shape. `stella-tools` validates this against
   * [`ToolSchema::input_schema`] at dispatch (`registry/validate.rs`,
   * #3144) — required fields, declared types, enums, item types, and
   * `additionalProperties: false` where a schema advertises it — and
   * refuses a contradicting call before the tool runs. Tools still read
   * fields defensively: a direct caller may bypass the registry.
   */
  input: unknown;
  /**
   * Which tool to run — matches the [`ToolSchema::name`] it was chosen
   * from.
   */
  name: string;
}

/**
 * The output of running a tool — success or a typed, named failure. Never a
 * bare string: every tool result is inspectable without string-sniffing.
 */
export type ToolOutput = {
  ok: {
    /**
     * What the tool produced, as the model will read it.
     */
    content: string;
    /**
     * The structured half of the result, when the tool has one (#3285).
     * `content` is prose for the model; `data` is the same facts as a
     * value a contract's `output_schema` can check — "references, not
     * payloads" (#2694 §4) is unenforceable over prose. `None` means
     * the tool produces no structured output, which is every tool
     * written before this field existed. Optional and absent-when-`None`
     * so every payload written before the field round-trips
     * byte-identically (invariant #4), and so the content bytes the
     * model sees are never perturbed by structure.
     */
    data?: unknown;
  };
} | {
  error: {
    /**
     * Which [`ErrorClass`] this failure falls in (#3145). `None` is a
     * declared default meaning "unclassified" — the site that built
     * this error has not been audited into a class yet, which is
     * distinct from any class it could be assigned. Optional and
     * absent-when-`None` so every payload written before the field
     * existed round-trips byte-identically (invariant #4), and so the
     * message bytes the model sees are never perturbed by
     * classification.
     */
    class?: ErrorClass | null;
    /**
     * Why it failed, phrased so the model can act on it — the model
     * sees this text and retries against it.
     */
    message: string;
  };
};

/**
 * A tool result reported back to the model, correlated to its call.
 */
export interface ToolResult {
  /**
   * The [`ToolCall::call_id`] this answers.
   */
  call_id: string;
  /**
   * What running the call produced — success or a named failure.
   */
  output: ToolOutput;
}

/**
 * A tool schema advertised to the model: name, description, and a JSON
 * Schema for its input. Kept as `serde_json::Value` rather than a typed
 * schema struct so any tool (built-in or MCP-supplied) can describe itself
 * without a second schema language.
 */
export interface ToolSchema {
  /**
   * What the tool does, written for the model rather than for a human
   * reader — this text is the whole basis on which it decides to call.
   */
  description: string;
  /**
   * JSON Schema for the object the model must send as
   * [`ToolCall::input`].
   */
  input_schema: unknown;
  /**
   * The tool's identifier, as the model must spell it in a
   * [`ToolCall::name`]. Unique within one registry.
   */
  name: string;
  /**
   * True when the tool cannot mutate any state (filesystem, processes,
   * environment) — the engine may run consecutive read-only calls from
   * one step concurrently. Defaults to false so unknown/external tools
   * are treated as mutating, the safe direction.
   */
  read_only?: boolean;
  /**
   * True when one announced call may safely EXECUTE TWICE — the claim
   * speculative execution needs (#923). A stream attempt that fails
   * after announcing its read-only prefix re-announces it on retry, so
   * every speculated call must tolerate a duplicate run. That is a
   * stronger claim than [`read_only`](Self::read_only): a web search
   * mutates no workspace state yet burns a metered API call each run,
   * and a graph query writes catch-up state to its own database on the
   * way to answering. Only a tool that is BOTH `read_only` and
   * `speculation_safe` is ever run before its step commits. Defaults to
   * false so external tools (MCP servers foremost) are never speculated
   * unless they opt in — the failure mode of the opposite default is
   * invisible and lands on the user's bill.
   */
  speculation_safe?: boolean;
}

/**
 * Serializable projection of [`TurnOutcome`] for the wire. `TurnOutcome` lives
 * in `stella-core` and is not itself `Serialize`, so the boundary owns the
 * mapping.
 */
export type TurnOutcomeWire = {
  cost_usd: number;
  status: "completed";
  text: string;
} | {
  cost_usd?: number;
  reason: string;
  status: "aborted";
};

/**
 * Content-free reason a provider attempt cannot contribute a truthful usage
 * envelope. Error bodies and prompts are deliberately unrepresentable.
 */
export type UsageIncompleteReason = "provider_error" | "timeout" | "cancelled";

/**
 * Response-detail level for providers with a verbosity parameter (OpenAI's
 * `text.verbosity`). Adapters whose wire has no equivalent ignore it — the
 * same never-fail contract as [`ReasoningEffort`].
 */
export type Verbosity = "low" | "medium" | "high";

/**
 * Evidence backing a `Verdict`. `deterministic` distinguishes the
 * flip-oracle/tests ladder from a model verifier's opinion — the two are
 * never conflated (L-E11).
 */
export interface VerdictEvidence {
  /**
   * `true` when the verdict came from the deterministic ladder (a
   * fail→pass flip of the same normalized test command, touched-tests
   * green, diff budget) rather than a model verifier.
   */
  deterministic: boolean;
  /**
   * Pointers to the underlying artifacts (`trace:t1#verify`, a test
   * command, a diff), so a reader can go check the summary rather than
   * take it on faith.
   */
  evidence_refs?: string[];
  /**
   * The full ladder input snapshot this verdict was decided from (#865).
   * `replay` answers "why did this run fast-submit / revise / verifier?"
   * from here without re-deriving, and a verifier escalation renders it into
   * the prompt (#864) so the verifier sees *why* the ladder was inconclusive
   * rather than a diff cold. Absent on events recorded before it existed.
   */
  ladder?: LadderSnapshot | null;
  /**
   * One line naming what was checked and what it showed.
   */
  summary: string;
}

/**
 * One frame emitted by the engine toward the host over the outbound stream.
 *
 * Not `Clone`: every frame is produced once and moved onto the channel, so no
 * consumer ever needs a second copy. The variants own their payloads outright
 * rather than borrowing — the port adapters in `remote.rs` pay whatever copy
 * that costs once, at construction (a completion request arrives borrowed and
 * is materialized with `CompletionRequestRef::into_owned`; a tool's `input`
 * arrives as `&Value` and is cloned there) — which is what lets a frame cross
 * from the session thread to the server runtime.
 */
export type ServerFrame = {
  event: AgentEvent;
  type: "event";
} | {
  input: unknown;
  name: string;
  request_id: string;
  type: "tool_request";
} | {
  /**
   * The provider the caller asked to serve THIS call: the turn's own
   * `provider_id`, or the override on its goal/sub-agent block.
   */
  provider_id: string;
  request: CompletionRequest;
  request_id: string;
  /**
   * What the call is for, so a host can route by role rather than by
   * string-matching a provider id.
   */
  role: ModelCallRole;
  type: "provider_request";
} | {
  reason?: string | null;
  type: "turn_held";
} | {
  type: "turn_released";
} | {
  outcome: TurnOutcomeWire;
  type: "turn_complete";
};

/**
 * Every `"type"` tag this build emits. A value outside this union is an
 * event from a newer stella — keep it, do not fail on it.
 */
export type KnownTypeTag =
  | "event"
  | "tool_request"
  | "provider_request"
  | "turn_held"
  | "turn_released"
  | "turn_complete";

// ── the envelope ────────────────────────────────────────────────────────────
//
// `seq` is added by the transport at delivery time, not by the engine, so it
// is not a field on any ServerFrame variant above. On the wire it sits
// alongside them: `{"seq":12,"type":"event","event":{…}}`.
//
// It is monotonic and gapless in DELIVERY order, starting at 1, and is also
// emitted as the SSE `id:` line — which is what lets a browser EventSource
// resume automatically via `Last-Event-ID`.

/** One frame as it appears on the wire: the engine's frame, plus its seq. */
export type StellaWireFrame = ServerFrame & { seq: number };

/**
 * Sent instead of a replay when the requested resume point has already been
 * evicted from the server's retained ring.
 *
 * Deliberately has no `seq`: it describes what the transport can no longer
 * supply, not something that happened in the turn. Receiving it means the
 * frames between `requested_after` and `oldest_retained` are unrecoverable —
 * reconnect with `?after=` one less than `oldest_retained` to replay what is
 * still held (an `?after=0` resume just re-answers `replay_truncated` unless
 * the ring still holds seq 1), or abandon the turn.
 */
export interface ReplayTruncated {
  type: "replay_truncated";
  /** The seq the client asked to resume after. */
  requested_after: number;
  /** The oldest seq the server still holds. */
  oldest_retained: number;
}

/** Anything a `data:` line can carry. */
export type StellaSseFrame = StellaWireFrame | ReplayTruncated;

// ── holds ARE re-learned by replay ──────────────────────────────────────────
//
// `turn_held` / `turn_released` are ordinary numbered frames, so a resumed
// stream replays them like any other and a client reconnecting mid-hold
// rediscovers that the turn is waiting on it. This is the OPPOSITE of the
// reverse-request rule stated below, and the difference is deliberate: an
// obligation is not re-announced because `?after=N` asserts you already have
// it, whereas a hold is a *state* the turn is still in. Read the tail from a
// `turn_held` with no matching `turn_released` after it and the turn is still
// held; post /resume to release it.

// ── inbound: answering a reverse request ────────────────────────────────────
//
// The engine never runs a model or a tool itself. When it needs one it emits a
// `provider_request` / `tool_request` frame carrying a `request_id`, and parks
// that step until the host POSTs the result back under the same id.
//
// An outstanding reverse request is NOT re-announced on resume: asking for
// `?after=N` asserts you received everything through N, obligations included.
// A client that persisted its seq but not its in-flight request ids must
// replay from the start — `?after=0`, or after a `replay_truncated`, one less
// than `oldest_retained` — to rediscover what it owes; obligations announced
// in frames the ring has already evicted cannot be re-learned this way.

/**
 * Why a tool call failed, as a closed machine-readable set. A tool result's `message` is prose written for the model to retry against; this is the axis a measurement needs, because a per-tool error rate cannot mean anything while a tool defect, model misuse and a policy refusal all count as the same failure. The values partition failures by whose problem they are: the model's (`invalid_input`, `not_found`), the policy plane's (`permission_denied`, `refused_by_policy`), the world's (`timeout`, `environment`), or the agent's own (`internal`). There is deliberately no `abandoned` class: a call whose turn ended before it returned produced no tool result at all. An unrecognized token reads as `other`, and re-serializing writes `other` rather than the original.
 */
export type ErrorClass = "invalid_input" | "not_found" | "permission_denied" | "refused_by_policy" | "timeout" | "environment" | "internal" | "other";

/**
 * The output of running a tool — success or a typed, named failure. Never a
 * bare string: every tool result is inspectable without string-sniffing.
 */
export type ToolOutput = {
  ok: {
    /**
     * What the tool produced, as the model will read it.
     */
    content: string;
    /**
     * The structured half of the result, when the tool has one (#3285).
     * `content` is prose for the model; `data` is the same facts as a
     * value a contract's `output_schema` can check — "references, not
     * payloads" (#2694 §4) is unenforceable over prose. `None` means
     * the tool produces no structured output, which is every tool
     * written before this field existed. Optional and absent-when-`None`
     * so every payload written before the field round-trips
     * byte-identically (invariant #4), and so the content bytes the
     * model sees are never perturbed by structure.
     */
    data?: unknown;
  };
} | {
  error: {
    /**
     * Which [`ErrorClass`] this failure falls in (#3145). `None` is a
     * declared default meaning "unclassified" — the site that built
     * this error has not been audited into a class yet, which is
     * distinct from any class it could be assigned. Optional and
     * absent-when-`None` so every payload written before the field
     * existed round-trips byte-identically (invariant #4), and so the
     * message bytes the model sees are never perturbed by
     * classification.
     */
    class?: ErrorClass | null;
    /**
     * Why it failed, phrased so the model can act on it — the model
     * sees this text and retries against it.
     */
    message: string;
  };
};

/**
 * Host → engine: the result of a [`ServerFrame::ToolRequest`].
 */
export interface ToolResultIn {
{
  output: ToolOutput;
  request_id: string;
}}

/**
 * The result of a completion.
 */
export interface CompletionResult {
  /**
   * Estimated provider cost in USD (0 for on-device/local).
   */
  cost_usd: number;
  /**
   * Why generation stopped, when the adapter can determine it. `None` when
   * the provider doesn't report it. `serde(default)` so envelopes
   * serialized before this field existed still parse.
   */
  finish_reason?: FinishReason | null;
  /**
   * Concrete model id/slug that produced the result, resolved from the
   * catalog — never a literal at the call site.
   */
  model: string;
  /**
   * The answer text, assembled from the stream. Empty when the model
   * only made tool calls.
   */
  text?: string;
  /**
   * Tool calls the model requested, in the order it made them.
   */
  tool_calls?: ToolCall[];
  /**
   * The upstream that actually served this call, when the endpoint is a
   * *gateway* that routes to somebody else's silicon and names it in the
   * response (OpenRouter's top-level `provider`).
   *
   * `None` on every direct endpoint, where the provider id already answers
   * "who served this?" — Anthropic-direct is served by Anthropic. Only a
   * gateway can make that question unanswerable, and one did: a probe
   * carrying Stella's own attribution asked OpenRouter for
   * `anthropic/claude-sonnet-5` and was served by Amazon Bedrock, which no
   * trace could show because the adapter recorded the gateway and threw the
   * upstream away. A head-to-head is only controlled if the model
   * *provider* is held fixed, so an unrecorded upstream is an uncontrolled
   * variable hiding inside a field that reads as though it were pinned.
   *
   * It rides here rather than in [`CompletionUsage`] because usage is a
   * `Copy` envelope of counters; this is call metadata, like `model`.
   */
  upstream_provider?: string | null;
  /**
   * Token accounting for this call ([`CompletionUsage`]).
   */
  usage: CompletionUsage;
}

/**
 * Token accounting for a single completion, normalized across providers
 * into one envelope: normalization lives in the adapter, not the caller.
 */
export interface CompletionUsage {
  /**
   * Tokens WRITTEN to the provider's prompt cache by this call
   * (Anthropic `cache_creation_input_tokens`, Bedrock
   * `cacheWriteInputTokens`). Unlike `cached_input_tokens` this is NOT a
   * subset of `input_tokens` — providers report writes separately, and
   * folding them into `input_tokens` would change cost accounting
   * (`Pricing::cost_usd` bills them on their own line at the catalog's
   * `cache_write_usd_per_mtok`, so folding would double-charge). 0 for providers
   * that never report cache writes (the OpenAI-compatible dialects).
   * `serde(default)` so envelopes serialized before this field existed
   * still parse.
   */
  cache_write_tokens?: number;
  /**
   * The subset of `input_tokens` served from the provider's prompt cache
   * — billed at the cache-read rate, not the input rate. 0 for providers
   * that never report a cache hit.
   */
  cached_input_tokens?: number;
  /**
   * Tokens the prompt cost, cache hits included.
   */
  input_tokens: number;
  /**
   * Tokens the model generated.
   */
  output_tokens: number;
  /**
   * The subset of `output_tokens` the model spent on reasoning, when the
   * provider breaks it out (`completion_tokens_details.reasoning_tokens`
   * on the OpenAI-compatible dialects, `output_tokens_details` on the
   * Responses API).
   *
   * `None` means NOT REPORTED, and is not the same fact as `Some(0)`.
   * Anthropic's Messages API folds thinking into `output_tokens` with no
   * breakdown at all, so every anthropic.rs call records `None` — while a
   * reasoning-capable model that genuinely did no thinking on a call
   * records `Some(0)`. Collapsing the two would report "this model never
   * thinks" for the entire Anthropic-direct route, which is the same class
   * of error as reading an unfilled placeholder column as a measured zero.
   *
   * Already inside `output_tokens` for billing on every provider that
   * reports it, so it is a diagnostic breakdown and never its own cost
   * line.
   */
  reasoning_tokens?: number | null;
  /**
   * The adapter observed the provider's authoritative usage-bearing
   * terminal response. This is explicit because a legitimate call can
   * report all zero counters, while a missing usage frame can accompany
   * non-empty streamed text. Legacy envelopes fail closed.
   */
  reported?: boolean;
}

/**
 * Why the model stopped generating, normalized across providers. Lets the
 * engine tell a natural stop from a truncation (`Length`) so an empty or
 * cut-off turn is surfaced to the user instead of being recorded as a clean
 * completion (the "turn ends with no feedback" defect).
 */
export type FinishReason = "stop" | "length" | "tool_calls" | "content_filter";

/**
 * The accounting an adapter had already observed when an attempt died before its terminal usage frame arrived. A mid-stream disconnect is not a total loss of accounting: dialects that report the prompt's cost up front have already delivered exact input, cache-read and cache-write counts by the time generation is cut. Every field is a LOWER BOUND on real spend and never a substitute for a provider-attested total, which is why such a record can never be mistaken for settled accounting.
 */
export interface PartialUsage {
  /**
   * `usage` priced at the serving model's catalog rates, or `0.0` when the
   * adapter had no pricing row for the model. Never provider-attested.
   */
  cost_usd: number;
  /**
   * Whether the input-side counts came from the provider's own frame
   * rather than a local estimate. `true` is the common case for
   * Anthropic-shaped streams and `false` for the OpenAI-shaped ones, which
   * send usage only at the end — the distinction a reader needs before
   * treating `usage.input_tokens` as fact.
   */
  input_reported?: boolean;
  /**
   * Counts observed before the failure. Input-side figures are the
   * provider's own when the dialect front-loads them; `output_tokens` is
   * whatever the last usage frame stated, or an estimate over the text
   * that actually arrived when no such frame did.
   */
  usage: CompletionUsage;
}

/**
 * Serializable mirror of [`ProviderError`]'s taxonomy. The host classifies the
 * failure at its adapter (never re-derived here) and sends the class; the
 * engine reconstructs a real [`ProviderError`] so its retry logic behaves
 * exactly as it would with a local provider.
 */
export type ProviderErrorWire = {
  kind: "transport";
  message: string;
  /**
   * Accounting a host's dying stream had already observed. Carried
   * across the wire so a remote provider loses no more usage than a
   * local one does; `serde(default)` keeps hosts that predate the
   * field (and the many failures with nothing to report) valid.
   */
  partial?: PartialUsage | null;
} | {
  kind: "rate_limited";
  message: string;
  retry_after_ms?: number | null;
} | {
  kind: "overloaded";
  message: string;
  retry_after_ms?: number | null;
} | {
  kind: "auth";
  message: string;
} | {
  kind: "unknown_model";
  slug: string;
} | {
  kind: "malformed";
  message: string;
} | {
  kind: "cancelled";
} | {
  kind: "context_overflow";
  message: string;
} | {
  affordable_output_tokens?: number | null;
  kind: "output_budget_exceeded";
  message: string;
} | {
  kind: "terminal";
  message: string;
};

/**
 * One tool invocation the model requested.
 */
export interface ToolCall {
  /**
   * Stable id correlating this call to its eventual `ToolResult`.
   */
  call_id: string;
  /**
   * The arguments, as the model produced them. Runtime data: never trust
   * the shape. `stella-tools` validates this against
   * [`ToolSchema::input_schema`] at dispatch (`registry/validate.rs`,
   * #3144) — required fields, declared types, enums, item types, and
   * `additionalProperties: false` where a schema advertises it — and
   * refuses a contradicting call before the tool runs. Tools still read
   * fields defensively: a direct caller may bypass the registry.
   */
  input: unknown;
  /**
   * Which tool to run — matches the [`ToolSchema::name`] it was chosen
   * from.
   */
  name: string;
}

/**
 * Host → engine: the result of a [`ServerFrame::ProviderRequest`] — either a
 * completed model response or a classified provider error.
 */
export interface ProviderResultIn {
{
  request_id: string;
}}

// ── inbound, optional: streaming a provider answer ──────────────────────────
//
// A host that streams its model call MAY POST batches of fragments to
// `POST /v1/turns/{id}/provider-delta` while the provider_request is in
// flight, keyed by the same request_id its eventual ProviderResultIn answers.
// Fragments surface on /events as text_delta / reasoning frames (so second
// subscribers and resuming clients see them) and each batch resets the
// reverse-request deadline. Strictly advisory: the definitive text is the
// CompletionResult on the terminating provider-result POST — a retried call
// re-streams from the start with no reset marker. A host that cannot stream
// simply never uses this route.

/**
 * One streamed fragment of an in-flight model completion.
 *
 * Text and thinking are distinct variants rather than one string because the
 * two must never be confused downstream: thinking renders as collapsible,
 * visibly-secondary content while answer text is the reply — the same
 * separation `ToolCallObserver` keeps between `text_delta` and
 * `reasoning_delta`, carried across the wire.
 */
export type ProviderDelta = {
  kind: "text";
  text: string;
} | {
  kind: "reasoning";
  text: string;
};

/**
 * Host → engine: a batch of streamed fragments for an in-flight
 * [`ServerFrame::ProviderRequest`] — the incremental half of a provider
 * answer (#1165), POSTed to `POST /v1/turns/{id}/provider-delta` and keyed by
 * the same `request_id` the terminating [`ProviderResultIn`] answers.
 *
 * Strictly optional: a host that cannot stream never POSTs one and keeps
 * exactly its old behavior. Strictly advisory, with the same contract as
 * `ToolCallObserver::text_delta`: the definitive text is the
 * `CompletionResult` on the eventual provider result — a retried model call
 * re-streams from the start with no reset marker, and consumers replace the
 * preview with the authoritative `Text` event when it lands.
 *
 * A batch rather than one fragment per POST, because a per-token HTTP
 * request would cost more than the latency it buys: the host accumulates
 * whatever chunking its own stream hands it and flushes on its own cadence.
 */
export interface ProviderDeltaIn {
{
  /**
   * The fragments, in stream order. Must not be empty — an empty batch
   * carries no information and is refused at the route.
   */
  deltas: ProviderDelta[];
  request_id: string;
}}

// ── request-side: the optional `engine` object on POST /v1/turns ────────────
//
// Per-turn engine knobs (#1167), also accepted on
// POST /v1/sessions/{id}/turns. Lowered onto the server's defaults: an
// omitted field keeps the default, an empty object is a no-op. Unusable
// values (a zero cap, a NaN temperature) are refused with a 400 naming the
// knob; values past an operator ceiling are clamped, and every clamp is
// reported in the create response's `clamped` array as
// {knob, requested, effective} — a request is never silently honored at a
// value it did not get. retry_policy and loop_detection are operator policy
// and are deliberately not on this object.

/**
 * Optional sampling/routing parameter overrides riding a
 * [`CompletionRequest`]. Every field is independently optional —
 * "include" semantics: `None` leaves the provider's own default in place,
 * `Some` puts the value on the wire. Each adapter forwards the subset its
 * dialect supports and silently drops the rest (a param the provider
 * can't express must never fail the request).
 */
export interface GenerationParams {
  /**
   * Penalize tokens by their frequency in the text so far.
   */
  frequency_penalty?: number | null;
  /**
   * Penalize tokens that have appeared at all in the text so far.
   */
  presence_penalty?: number | null;
  /**
   * Multiplicative repetition penalty (>1 discourages, <1 encourages).
   */
  repetition_penalty?: number | null;
  /**
   * Random seed for deterministic outputs, where supported.
   */
  seed?: number | null;
  /**
   * Which capacity tier to route to ([`ServiceTier`]).
   */
  service_tier?: ServiceTier | null;
  /**
   * Limit sampling to the k highest-probability tokens.
   */
  top_k?: number | null;
  /**
   * Nucleus sampling: cumulative-probability cutoff.
   */
  top_p?: number | null;
  /**
   * How much detail to ask for ([`Verbosity`]).
   */
  verbosity?: Verbosity | null;
}

/**
 * Reasoning effort forwarded to models with a thinking/extended-reasoning
 * mode. One enum, mapped per-adapter to the provider's own parameter name
 * ("reasoning_param").
 */
export type ReasoningEffort = "low" | "medium" | "high" | "xhigh" | "max";

/**
 * Provider service tier: `Priority` routes to faster paid-tier capacity,
 * `Flex` to cheaper capacity with slower response times. Only applied by
 * providers that support tiered service; others use their default tier.
 */
export type ServiceTier = "auto" | "default" | "flex" | "priority";

/**
 * Response-detail level for providers with a verbosity parameter (OpenAI's
 * `text.verbosity`). Adapters whose wire has no equivalent ignore it — the
 * same never-fail contract as [`ReasoningEffort`].
 */
export type Verbosity = "low" | "medium" | "high";

/**
 * The caller-policy slice of `EngineConfig`, settable per turn (#1167) as
 * the optional `engine` object on `POST /v1/turns` and
 * `POST /v1/sessions/{id}/turns`.
 *
 * Every field is independently optional with "lower onto defaults" semantics:
 * `None` keeps the server's configured default, `Some` overrides it for this
 * turn only. What is deliberately **not** here: `retry_policy` and
 * `loop_detection` are operator policy — they bound what a single request can
 * cost this process, and a caller who could widen them could make a turn
 * effectively unbounded — and `max_steps` / `reverse_request_timeout_ms`
 * already ride the request top-level.
 *
 * Unknown fields are refused rather than ignored: a typoed knob that parses
 * is a knob silently *not* honored, the same illegibility the ceilings exist
 * to avoid on the other side.
 *
 * Session note: none of these knobs touch the message transcript, so a
 * per-turn override on `POST /v1/sessions/{id}/turns` cannot perturb the
 * byte-stable prompt prefix the session's cache contract depends on.
 */
export interface EngineOverrides {
{
  /**
   * Compaction trigger, in estimated tokens. Rejected at 0; clamped to
   * [`MAX_COMPACTION_BUDGET_TOKENS`].
   */
  compaction_budget_tokens?: number | null;
  /**
   * Reasoning-effort tier (`CompletionRequest::effort` semantics).
   */
  effort?: ReasoningEffort | null;
  /**
   * Output-token cap forwarded on every completion of this turn. Rejected
   * at 0; clamped to [`MAX_OUTPUT_TOKENS_CEILING`]. Effort tier and this
   * cap are one budget — raising effort against a small cap is how a model
   * spends its tokens thinking and gets truncated before its tool call.
   */
  max_output_tokens?: number | null;
  /**
   * Seconds of provider silence that end a single generation
   * (`EngineConfig::model_timeout`). Clamped to
   * [`MAX_MODEL_TIMEOUT_SECS`]; `0` disables the backstop.
   *
   * The partner of `max_output_tokens`, and unusable without it: the two are
   * one budget, so a host that raises the cap and leaves this at the default
   * has moved where its steps die rather than stopped them dying. Exposed
   * for that reason — before this, a host could pin the output cap over the
   * wire but not the timeout that has to scale with it (#1211 §6.2).
   */
  model_timeout_secs?: number | null;
  /**
   * Sampling/routing overrides (`CompletionRequest::params` semantics —
   * the host's adapter forwards the subset its dialect supports).
   */
  params?: GenerationParams | null;
  /**
   * Thinking-mode enable/disable (`CompletionRequest::reasoning`
   * semantics; `None` = provider default).
   */
  reasoning?: boolean | null;
  /**
   * Messages at the tail the summarizer never touches. Clamped to
   * [`MAX_SUMMARIZE_KEEP_RECENT`].
   */
  summarize_keep_recent?: number | null;
  /**
   * Whether overflow beyond the compaction budget may be summarized away
   * by a model call (`EngineConfig::summarize_overflow`).
   */
  summarize_overflow?: boolean | null;
  /**
   * Sampling temperature. Rejected unless finite and non-negative; clamped
   * to [`MAX_TEMPERATURE`].
   */
  temperature?: number | null;
  /**
   * Age-based tool-result retention horizon, in tool-bearing steps
   * (`EngineConfig::tool_result_horizon_steps`): results older than this
   * many steps are middle-out aged on every step, independent of the
   * compaction budget (#1285). Clamped to
   * [`MAX_TOOL_RESULT_HORIZON_STEPS`]; `0` disables the pass, restoring
   * pure budget-triggered compaction.
   */
  tool_result_horizon_steps?: number | null;
}}
