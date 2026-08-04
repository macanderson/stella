// GENERATED FILE — DO NOT EDIT.
//
// Regenerate with:  bash scripts/export-agentevent-schema.sh
// Source of truth:  stella-protocol/src/event.rs (`AgentEvent`)
// Guarded by:       scripts/check-wire-schema.sh (`make wire-schema`)
//
// The event stream emitted by `stella --output-format stream-json` (one JSON
// object per line), by `stella-serve` over SSE, and folded by the TUI.
//
// A `"type"` value not listed here is NOT an error: it is an event from a
// newer stella. Forward-compatible consumers keep such a line intact and move
// on, exactly as `AgentEvent::Unknown` does on the Rust side.

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
 * defines it (`docs/design/adaptive-context/context-reuse.md` §2 `ProviderUsage`).
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
 * (`docs/design/adaptive-context/context-reuse.md` §2 `UsageReport`) — the envelope a metering
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
 * Why the model stopped generating, normalized across providers. Lets the
 * engine tell a natural stop from a truncation (`Length`) so an empty or
 * cut-off turn is surfaced to the user instead of being recorded as a clean
 * completion (the "turn ends with no feedback" defect).
 */
export type FinishReason = "stop" | "length" | "tool_calls" | "content_filter";

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
 * Which rung of the evidence ladder a `JudgeVerdict` actually came to rest
 * on (#1043).
 *
 * # Why the rung is on the wire instead of inferred
 *
 * `JudgeEvidence` already carries `passed` and `deterministic`, and for a
 * while that looked like enough. It is not, and the gap is not academic:
 *
 * - `deterministic: true, passed: false` is emitted by **two** rungs — a red
 *   touched test ([`LadderRung::Revise`], a genuine failure) and a turn that
 *   dispatched nothing ([`LadderRung::NothingAttempted`], where the honest
 *   label is "no evidence", not "wrong").
 * - `deterministic: true, passed: true` is emitted both by the full
 *   deterministic pass ([`LadderRung::SubmitFast`]) and by a *waived* review
 *   where the warrant found nothing to prove ([`LadderRung::Waived`]) — so a
 *   reader inferring "deterministic pass" from those two flags would label a
 *   turn nothing checked as the ladder's strongest possible result.
 * - `deterministic: false` covers a real model judge, a heuristic verdict
 *   after the judge call *failed*, and the abstain rung. The first is soft
 *   signal, the second is not signal at all, and the only thing separating
 *   them in the old shape was the wording of a summary string.
 *
 * Re-deriving the rung by re-running the ladder over
 * [`LadderSnapshot`] cannot close any of that: it reproduces the *decision*
 * but not which of the three ways the judge arm resolved, and it silently
 * disagrees with reality whenever the run's `veto_warnings` setting differed
 * from the reader's assumption. So the pipeline states the rung it took.
 *
 * Wire-compatible in the additive direction only, like every nested
 * vocabulary in this crate: a reader that meets a rung it does not know fails
 * the event rather than laundering it (see the [`crate::event`] docs).
 */
export type LadderRung = "submit_fast" | "revise" | "nothing_attempted" | "unverifiable" | "model_judge" | "heuristic_fallback" | "waived";

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
 * Additive in one direction only: an older reader that does not know the
 * `proof` type tag preserves the whole event via [`AgentEvent::Unknown`],
 * but a reader that knows `Proof` and meets a future `kind` fails the whole
 * event — this nested enum is closed, with no `Unknown` step (see the
 * module docs on nested vocabularies).
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
 * Content-free reason a provider attempt cannot contribute a truthful usage
 * envelope. Error bodies and prompts are deliberately unrepresentable.
 */
export type UsageIncompleteReason = "provider_error" | "timeout" | "cancelled";

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
   * Why generation stopped, as the provider reported it
   * ([`crate::completion::FinishReason`]). `Length` is the
   * only *ground truth* a consumer has that this step was cut off at
   * the output ceiling — the "we stopped first" event.
   *
   * It is here because it was previously nowhere: the engine knew the
   * reason and dropped it at this boundary, so every downstream reader
   * had to *infer* truncation from step shape. The benchmark harness
   * inferred it as "≥16384 output tokens and no tool call", which was
   * right when the output ceiling was 16K and became a false positive
   * the moment the ceiling moved to 64K — the reading behind the
   * unexplained `cap_hits: 106` in the GLM-5.2 head-to-head. An
   * inferred cap hit cannot be told from a long answer; a reported one
   * can.
   *
   * `None` means the provider did not report a reason (or the stream
   * predates this field — hence `serde(default)`), and must never be
   * read as "not truncated": absence of the signal is not evidence of
   * a clean stop.
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
   * The CGP usage report for this recall (`docs/design/adaptive-context/context-reuse.md` §2):
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
 * Every `"type"` tag this build emits. A value outside this union is an
 * event from a newer stella — keep it, do not fail on it.
 */
export type KnownTypeTag =
  | "stage"
  | "text"
  | "text_delta"
  | "reasoning"
  | "tool_start"
  | "tool_result"
  | "speculation_discarded"
  | "retry"
  | "steered"
  | "loop_detected"
  | "budget_denied"
  | "retries_exhausted"
  | "policy_decision"
  | "compaction"
  | "budget_tick"
  | "step_usage"
  | "usage_incomplete"
  | "goal_verdict"
  | "provider_fallback"
  | "file_change"
  | "context_recall"
  | "context_write"
  | "block_registered"
  | "step_manifest"
  | "proof"
  | "judge_verdict"
  | "scope_review"
  | "ask_user"
  | "media_progress"
  | "media_complete"
  | "commit"
  | "pr"
  | "task_update"
  | "sub_agent"
  | "error"
  | "complete";
