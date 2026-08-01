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
 * compat fallback would mean hand-writing a visitor for all 34 variants.
 */
export type AgentEvent = {
  name: StageKind;
  type: "stage";
} | {
  delta: string;
  type: "text";
} | {
  text: string;
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
  text: string;
  type: "steered";
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
   * `"exact_repeat"` | `"short_cycle"` — mirrors
   * `stella-core::loop_detect::LoopVerdict` (kept as a string here so
   * `stella-protocol` never depends on `stella-core`).
   */
  kind: string;
  /**
   * Tool names of the repeated signature, in cycle order (one entry
   * for an exact repeat).
   */
  pattern: string[];
  /**
   * Consecutive identical calls (exact repeat) or full cycles (short
   * cycle) observed.
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
   * sent — paired with `input_tokens` this is one drift sample, the
   * feedback that calibrates future estimates per model
   * (`stella-core::estimator::Calibration`). Raw by contract:
   * consumers rebuild the correction from these pairs, and a
   * corrected estimate here would compound the correction on every
   * round trip. `0` means no estimate was taken (pre-drift emitters —
   * hence `serde(default)`, so old streams still parse).
   */
  estimated_input_tokens?: number;
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
   */
  provider?: string;
  retries: number;
  /**
   * Exact call purpose. Missing legacy values deserialize as
   * [`ModelCallRole::Unknown`].
   */
  role?: ModelCallRole;
  step: number;
  tool_calls: number;
  type: "step_usage";
} | {
  duration_ms: number;
  model: string;
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
   * The CGP usage report for this recall (`docs/context-reuse.md` §2):
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
   * and the pipeline's triage/judge/plan/guidance roles — take 1, 2, …
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
  evidence: JudgeEvidence;
  passed: boolean;
  type: "judge_verdict";
} | {
  proposal: ScopeProposal;
  type: "scope_review";
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
  message: string;
  retryable: boolean;
  type: "error";
} | {
  cost_usd: number;
  model: string;
  type: "complete";
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
 * spec (`docs/design/session-telemetry-receipts-spec.md`, §4). Forward-compat:
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
 * Aggregate CI verdict for a PR's head commit, as observed by the
 * fleet monitor (`gh pr checks`). Reconciled against the live source
 * before rendering, never served from cache alone (L-V3).
 */
export type CiStatus = "pending" | "running" | "passing" | "failing";

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
 * defines it (`docs/context-reuse.md` §2 `ProviderUsage`).
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
 * The per-request roll-up of what one context recall cost
 * (`docs/context-reuse.md` §2 `UsageReport`) — the envelope a metering
 * pipeline bills from, and the answer to "what did this turn's context cost,
 * and which sources drove it?".
 *
 * Deliberately **content-free**: budget scalars, an accounting timestamp, and
 * per-provider counts and costs. The spec's `served_frames` drill-down is
 * *not* duplicated here — the sibling `frames: Vec<ContextFrameRef>` on the
 * same [`crate::event::AgentEvent::ContextRecall`] already records the frame-granular
 * identities locally, so an auditor still walks from a total to its frames
 * without this type ever carrying one.
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
 * What happened to a file in a `FileChange` event.
 */
export type FileChangeKind = "read" | "created" | "modified" | "deleted";

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
 * Evidence backing a `JudgeVerdict`. `deterministic` distinguishes the
 * flip-oracle/tests ladder from a model judge's opinion — the two are
 * never conflated (L-E11).
 */
export interface JudgeEvidence {
  /**
   * `true` when the verdict came from the deterministic ladder (a
   * fail→pass flip of the same normalized test command, touched-tests
   * green, diff budget) rather than a model judge.
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
   * `replay` answers "why did this run fast-submit / revise / judge?"
   * from here without re-deriving, and a judge escalation renders it into
   * the prompt (#864) so the judge sees *why* the ladder was inconclusive
   * rather than a diff cold. Absent on events recorded before it existed.
   */
  ladder?: LadderSnapshot | null;
  /**
   * One line naming what was checked and what it showed.
   */
  summary: string;
}

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
   * Lines changed, and the budget they were judged against.
   */
  diff_lines: number;
  /**
   * Mutating file touches the recorder observed.
   */
  file_change_events: number;
  /**
   * Whether the oracle's flip was achieved — after the confirmation run,
   * so an unconfirmed flip reads `false` here with `unstable_flip: true`.
   */
  flip_achieved: boolean;
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
   * The oracle's observations in order (baseline, candidate runs, the
   * pre-submit confirmation). Infra runs are absent by construction.
   */
  oracle_trace?: OracleObservation[];
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
export type ModelCallRole = "unknown" | "triage" | "plan" | "plan_repair" | "witness_author" | "witness_repair" | "worker" | "distress_guidance" | "judge" | "agent_author" | "skill_author" | "domain_inference" | "reflection" | "summarization";

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
 * One step of the proof a turn builds for its own work, in the order the
 * pipeline makes the observation. Carried by [`AgentEvent::Proof`].
 *
 * Additive to the wire contract in both directions: an older reader sees the
 * whole event as [`AgentEvent::Unknown`], and a reader that knows `Proof` but
 * not a future step tags it `Unknown` at the step level rather than guessing.
 */
export type ProofStep = {
  /**
   * Whether a model judge was called for on inconclusive evidence.
   */
  judge: boolean;
  kind: "assurance";
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
  command: string;
  kind: "oracle";
  passed: boolean;
  tree: ProofTree;
};

/**
 * Which code state a [`ProofStep::Oracle`] observation was made against.
 *
 * The distinction is the whole content of a flip: the same command failing in
 * `Baseline` and passing in `Candidate` is proof, while either result twice
 * against one tree is a tree observed twice.
 */
export type ProofTree = "baseline" | "candidate";

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
 */
export interface ScopeProposal {
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
   * The plan's steps, in the order the worker will attempt them.
   */
  steps: string[];
  /**
   * One line describing the work, for the approval prompt's headline.
   */
  summary: string;
}

/**
 * Provider service tier: `Priority` routes to faster paid-tier capacity,
 * `Flex` to cheaper capacity with slower response times. Only applied by
 * providers that support tiered service; others use their default tier.
 */
export type ServiceTier = "auto" | "default" | "flex" | "priority";

/**
 * A named point in the turn's data flow. Exactly one stage vocabulary
 * exists in this workspace — never duplicated per-crate (the TS-era
 * `StageKind` duplication this structurally forbids, L-E1).
 */
export type StageKind = "triage" | "context_recall" | "plan" | "scope_review" | "witness" | "execute" | "verify" | "judge" | "reflect" | "context_write" | "complete";

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
 * One entry on the turn's task board (the `task_*` tools). The board is
 * session-scoped working state — what the agent has planned, is doing,
 * and has finished — mirrored to the store for cross-session findability.
 */
export interface TaskItem {
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
   * The arguments, as the model produced them. Runtime data: validate
   * against [`ToolSchema::input_schema`] rather than trusting the shape.
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
  };
} | {
  error: {
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
  request: CompletionRequest;
  request_id: string;
  type: "provider_request";
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
 * reconnect with `?after=0` to replay what is still held, or abandon the turn.
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

// ── inbound: answering a reverse request ────────────────────────────────────
//
// The engine never runs a model or a tool itself. When it needs one it emits a
// `provider_request` / `tool_request` frame carrying a `request_id`, and parks
// that step until the host POSTs the result back under the same id.
//
// An outstanding reverse request is NOT re-announced on resume: asking for
// `?after=N` asserts you received everything through N, obligations included.
// A client that persisted its seq but not its in-flight request ids must
// resume from `?after=0` and replay to rediscover what it owes.

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
  };
} | {
  error: {
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
   * (`Pricing::cost_usd` carries no cache-write rate). 0 for providers
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
 * Serializable mirror of [`ProviderError`]'s taxonomy. The host classifies the
 * failure at its adapter (never re-derived here) and sends the class; the
 * engine reconstructs a real [`ProviderError`] so its retry logic behaves
 * exactly as it would with a local provider.
 */
export type ProviderErrorWire = {
  kind: "transport";
  message: string;
} | {
  kind: "rate_limited";
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
   * The arguments, as the model produced them. Runtime data: validate
   * against [`ToolSchema::input_schema`] rather than trusting the shape.
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
