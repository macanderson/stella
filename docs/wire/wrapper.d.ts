// GENERATED FILE — DO NOT EDIT.
//
// Regenerate with:  bash scripts/export-agentevent-schema.sh
// Source of truth:  stella-plugin/src/wire.rs
// Guarded by:       scripts/check-wire-schema.sh (`make wire-schema`)
//
// The wrapper socket: the two messages an out-of-process plugin exchanges with
// its host at a point. The host writes a WrapperRequest to the plugin's stdin,
// one JSON object per line; the plugin writes a WrapperResponse back to stdout
// to end the point.
//
// A request and a response are NOT alternatives. `point` is a legal tag on
// both, and says which point a message belongs to — never which direction it
// travels. Which direction you are reading is decided by which pipe it came
// off, and a plugin only ever writes responses.
//
// The host-call and driver channels cross the same pipes and are not described
// here: HostCallOk is an untagged union whose contract is that its arms are
// discriminable by their required keys, which JSON Schema can state and cannot
// check. docs/wire/wrapper.wire.json shows every one of their messages as the
// exact bytes a parser meets.
//
// Every message carries protocol_version, and the contract is additive-only.

/**
 * Everything a wrapper is given once the turn's completion lands.
 *
 * It receives the outcome; it does **not** hold a channel into the turn. That
 * is #3379's one-directional connection stated as a socket rule — the
 * pipeline no longer edits the engine's stream, and no plugin ever gets to.
 */
export interface AfterTurnRequest {
  /**
   * The candidate workspace the turn ran against, when there was one —
   * see [`CandidateGrant`].
   */
  candidate?: CandidateGrant | null;
  /**
   * The goal, as the user stated it.
   */
  goal: string;
  /**
   * The version this message is written at.
   */
  protocol_version: number;
  /**
   * Which round of the wrapper's loop just ran.
   */
  round: number;
  /**
   * Which declared stage this evidence is about, mirroring the stage the same round's before_turn named. Absent means this host runs no stage program at all — never a default stage.
   */
  stage?: StageName | null;
  /**
   * What the turn did.
   */
  turn: TurnOutcome;
  /**
   * The variant id of the wrapper being asked.
   */
  wrapper: string;
}

/**
 * The evidence a wrapper gathered.
 */
export interface AfterTurnResponse {
  /**
   * What the wrapper observed — its own half of the evidence, never the
   * host's ([`ObservedEvidence`], and #3499 for why the difference is a
   * type).
   */
  evidence: ObservedEvidence;
  /**
   * The version this message is written at.
   */
  protocol_version: number;
}

/**
 * Everything a wrapper is given before a turn runs.
 *
 * Every capability arrives *in the request* — there is no ambient authority
 * to reach for, which is the property that lets the same plugin run under
 * `stella-cli`, `stella-serve` and an embedded host without change
 * (`doc:wrapper-socket` §6).
 */
export interface BeforeTurnRequest {
  /**
   * The candidate workspace this turn will run against, when the host has
   * created one — see [`CandidateGrant`].
   */
  candidate?: CandidateGrant | null;
  /**
   * The goal, as the user stated it.
   */
  goal: string;
  /**
   * The version this message is written at.
   */
  protocol_version: number;
  /**
   * What earlier stages of this same turn published, in publication order.
   */
  published?: PublishedSignal[];
  /**
   * Which round of the wrapper's loop this is; `0` for the first turn.
   */
  round: number;
  /**
   * Which declared stage this call is for. before_turn runs once per stage the host resolved this wrapper's declared order down to.
   */
  stage: StageName;
  /**
   * The variant id of the wrapper being asked — `StageProgram::variant`,
   * and the join key of every per-variant comparison (#3388).
   */
  wrapper: string;
}

/**
 * What a wrapper contributes to the turn about to run.
 *
 * Everything here is an *offer*: the host applies it, bounded by what the
 * manifest declared and what the user consented to. A wrapper cannot run the
 * loop itself, and nothing in this type is a channel into it.
 */
export interface BeforeTurnResponse {
  /**
   * Context to put in front of the model — as volatile messages *after* the
   * byte-stable prefix, which [`VolatileContext`] makes the only option.
   */
  context?: VolatileContext[];
  /**
   * The version this message is written at.
   */
  protocol_version: number;
  /**
   * Signals this stage publishes for later stages of the same turn to read. The host validates each value's type and carries it forward into the next stage's request. It does not change which stages run: that is resolved once, before the first stage.
   */
  publish?: PublishedSignal[];
  /**
   * A role *intent* for this turn: the name of a `[roles.<name>]` entry the
   * same manifest declares. Never a model id, a provider, a URL or a
   * credential — the host resolves the intent against the user's BYOK
   * providers, and refuses an intent the manifest never declared.
   */
  role?: string | null;
  /**
   * Workspace-relative paths the wrapper believes the turn should stay
   * within. Advisory input to the host's own scoping — a plugin-supplied
   * path is never itself a permission (`stella_protocol::candidate`).
   */
  scope?: string[];
  /**
   * Workspace-relative paths this wrapper will judge its flip against. The host snapshots their identity before the turn and re-checks after it; the plugin never vouches for its own witness. A path outside the granted root is dropped from the watch, and declaring nothing leaves the tamper finding unchecked, which is not a pass.
   */
  witness?: string[];
}

/**
 * The candidate workspace, as a plugin receives it.
 *
 * **A capability the host resolves and bounds, never a path a plugin is
 * trusted to stay inside.** The distinction is the whole design and it is
 * worth being exact about, because the grant does hand out an absolute path:
 *
 * - [`Self::root`] is where the plugin's *own* reads and its *own* test run
 *   happen. A plugin that runs a test suite needs a directory, and pretending
 *   otherwise is what pushed three reference plugins onto `[runtime] env` for
 *   their test command (#3498).
 * - [`Self::handle`] is the capability. Every path the plugin names on the
 *   way *back* — a scope, a withheld adoption path, a witness artifact — is
 *   resolved against this handle's root by the host and refused if it lands
 *   anywhere else, after symlinks, on the host's own filesystem
 *   (`CandidateDenial`, and this crate's `candidate_grant::fence` for the
 *   implementation — it was the staged pipeline's
 *   `ports::CandidateHandles` until #3865 deleted that crate). The refusal is
 *   the host's; nothing here is a promise
 *   the plugin was asked to keep.
 *
 * So a plugin that ignores the root and lies about where it went has told the
 * host nothing the host will act on, which is the property that lets the same
 * grant cross to a process in any language.
 */
export interface CandidateGrant {
  /**
   * The name the host minted for this workspace, and the only thing that
   * re-addresses it. Opaque: see [`CandidateHandle`].
   */
  handle: CandidateHandle;
  /**
   * The workspace's absolute root on the host's filesystem, canonical — the
   * host resolves symlinks *before* minting the grant, so the path a plugin
   * is told and the path the host fences against are the same one.
   */
  root: string;
  /**
   * The test the host would run in this workspace, when it has one to
   * give. `None` is "the host has no test invocation", never "run whatever
   * you like": a plugin with no plan reports
   * [`FlipObservation::Unobservable`] rather than guessing.
   */
  test?: TestPlan | null;
}

/**
 * A serializable name for one live candidate workspace.
 *
 * Opaque by construction: the string is the host's, and nothing but the host
 * that minted it can turn one back into a workspace. It carries no path, no
 * file descriptor and no authority of its own — resolving it is a lookup in
 * the minting registry, so a handle for a workspace that was never created,
 * or one already removed, is [`CandidateDenial::UnknownHandle`].
 *
 * A newtype rather than a bare `String` for the reason AGENTS.md's glossary
 * gives: this workspace already has six identifiers that read alike, and an
 * unwrapped `String` is how a seventh joins them.
 *
 * **It is a name, not a bearer token.** Ids are minted in order, so a plugin
 * that holds one handle can spell a sibling candidate's. Binding a handle to
 * the principal it was issued to belongs with plugin identity (#3380 A1) —
 * until that exists (#3484), a host must not hand handles to two principals
 * it means to keep apart.
 */
export type CandidateHandle = string;

/**
 * What a wrapper saw of the fail→pass flip. Closed: a host refuses a value it does not know rather than reading it as a weaker one.
 */
export type FlipObservation = "not-attempted" | "achieved" | "not-achieved" | "unsatisfiable" | "unobservable";

/**
 * A stage boundary **this host itself** emits.
 *
 * Closed by design, and the claim it makes is deliberately smaller than it
 * used to be: this is not the stage vocabulary any more (that is
 * [`StageName`], which is open), it is the set of boundaries the host knows
 * how to name on its own. The names and their order were taken from
 * `stage_rank` in the staged pipeline's `replay.rs` (`crates/stella-pipeline`,
 * deleted in #3865), which was the canonical ordering then — and
 * [`HostStage::kind`] makes the correspondence mechanical rather than a
 * claim, since it is one-to-one onto [`StageKind`]'s twelve. With that crate
 * gone, this enum *is* the ordering of the host's own stages.
 *
 * **A name here is not a promise that every host runs it.** The vocabulary
 * mirrors [`StageKind`] because a wrapper that cannot spell a boundary
 * cannot describe the run it wraps; which hosts emit which boundary today
 * differs per stage, and each variant below says so rather than leaving a
 * manifest author to discover it from a run that quietly did nothing.
 */
export type HostStage = "triage" | "recall" | "research" | "plan" | "scope" | "execute" | "witness" | "verify" | "verdict" | "reflect" | "contextwrite" | "complete";

/**
 * The evidence a wrapper gathered — the plugin-owned half of an
 * [`EvidenceSet`].
 *
 * Every field here is something a plugin's own process can honestly observe:
 * it ran the witness and watched it flip, or it ran a benchmark and counted.
 * Nothing here is a verdict, and nothing here is a fact only the host holds.
 */
export interface ObservedEvidence {
  /**
   * What this wrapper's own process wants the next round told, in its own words. Advisory: it never decides a verdict, it only rides onto the correction a held-open round renders. Absent when there is nothing to add.
   */
  detail?: string | null;
  /**
   * What the wrapper saw of the fail→pass flip.
   */
  flip: FlipObservation;
  /**
   * The numbers the oracle reported, by declared measurement name. A name
   * absent here is *missing*, never a satisfied budget — see
   * `stella_runtime::wrapper::judge`.
   */
  measurements?: Record<string, unknown>;
}

/**
 * One signal value a stage published.
 *
 * The signal set is the manifest's closed [`Signal`] vocabulary, so a stage
 * cannot invent a fact for a later condition to read — the load-time rule
 * ("a condition naming a signal the host does not publish is a load error")
 * stated for the socket that actually dispatches.
 */
export interface PublishedSignal {
  /**
   * Which signal.
   */
  signal: Signal;
  /**
   * Its value this turn.
   */
  value: SignalValue;
}

/**
 * A fact a condition may read.
 *
 * The **published set** — naming anything outside it is a load error. Every
 * entry transcribes a branch the pipeline takes today, at a named line; a
 * fact with no live branch behind it is not added, because the grammar's
 * whole defence is that a new name is a reviewable decision rather than a
 * dial someone might find a use for. Growing this set is how a wrapper gains
 * expressiveness — never a richer condition syntax.
 *
 * Host facts, readable by any stage:
 *
 * - `test-command` — `Pipeline::config.test_command`, the fact gating
 *   witness authoring (`pipeline.rs`, the `authored_witness` conjunction).
 * - `candidates` — `PipelineConfig::candidate_count()`. `n == 1` is the
 *   single-shot/best-of-N split in `pipeline.rs::run`.
 * - `budget-metered` — `budget.mode() != BudgetMode::Off`, the first thing
 *   `pipeline/repair_gate.rs::repair_headroom` asks before a repair round
 *   may be bought. Metered, not the amount: dollars are a float, the grammar
 *   compares against whole numbers only, and a threshold silently coarser
 *   than the guard's would be worse than no threshold.
 *
 * Triage's assessment, which decides the conversational fast path, whether
 * research runs, and what the class is owed:
 *
 * - `conversational`, `questions`, `plans`, `verifies` — as before.
 * - `wants-witness` — `assessment.wants_witness()`, a conjunct of the
 *   authored-witness decision in `pipeline.rs::run`.
 * - `wants-verifier` — `assessment.wants_verifier()`, read by the
 *   `LadderDecision::Unverified` arm of `pipeline.rs::verify_candidate`.
 *
 * What execution produced, read by the ladder in `verify.rs`:
 *
 * - `mutating-actions` — `mutating_actions == 0`, a conjunct of
 *   `LadderInputs::nothing_was_attempted`.
 * - `diff-lines` — `diff_lines <= diff_budget`, the diff-budget conjunct of
 *   the `SubmitFast` rung.
 *
 * What the witness stage produced:
 *
 * - `witness-authored` — `witness.is_some()`, which decides the effective
 *   test command, the tamper sweep, and the mutation audit in
 *   `pipeline.rs::verify_candidate`.
 *
 * What verification observed. The two test signals are **both** false when no
 * test ran, and that is deliberate: `touched_tests_passed` is an
 * `Option<bool>`, and one `tests-passed` boolean would report a suite that
 * never ran identically to one that went red. Two total predicates keep the
 * third state visible, the way [`FlipOutcome::is_achieved`] and
 * [`FlipOutcome::was_observed`] do for the flip:
 *
 * - `flip-achieved` — `flip.is_achieved()`, the receipt half of the
 *   `SubmitFast` rung.
 * - `tests-red` — `touched_tests_passed == Some(false)`, the ladder's
 *   deterministic-failure rung.
 * - `tests-green` — `touched_tests_passed == Some(true)`, the corroboration
 *   the pipeline's own flip needs beside it.
 *
 * [`FlipOutcome::is_achieved`]: stella_protocol::FlipOutcome::is_achieved
 * [`FlipOutcome::was_observed`]: stella_protocol::FlipOutcome::was_observed
 */
export type Signal = "test-command" | "candidates" | "budget-metered" | "conversational" | "questions" | "plans" | "verifies" | "wants-witness" | "wants-verifier" | "mutating-actions" | "diff-lines" | "witness-authored" | "flip-achieved" | "tests-red" | "tests-green";

/**
 * A published signal's value — the two shapes [`SignalKind`] enumerates.
 */
export type SignalValue = {
  boolean: boolean;
} | {
  count: number;
};

/**
 * The name of a stage a wrapper declares: one of the host's own, or a word
 * this manifest contributed (#3963).
 *
 * # Why this is open, and what still refuses to load
 *
 * A closed vocabulary capped the set of turn shapes a plugin could express at
 * the set the host anticipated, which is the exact shape `doc:roleless-core`
 * exists to remove. What it bought — that a manifest cannot declare a stage
 * nothing will dispatch — is kept, and by a stronger route than a closed enum:
 * the dispatcher iterates whatever the resolved program holds, so a
 * contributed stage dispatches *because* it was declared, and
 * loading still refuses the names that could not be dispatched, rendered, or
 * told apart from a host boundary.
 *
 * # Normalization is what keeps one word one stage
 *
 * [`StageName::new`] resolves a name [`HostStage`] knows into
 * [`StageName::Host`], so [`StageName::Contributed`] never holds a word the
 * host already answers to — the same discipline, and the same reason, as
 * `stella_protocol::StageName`: a value that does not survive its own round
 * trip is what invariant 4 forbids.
 */
export type StageName = {
  Host: HostStage;
} | {
  Contributed: string;
};

/**
 * What a [`TestPlan`]'s invocation reported before the turn ran.
 *
 * Four answers rather than an exit code, and the fourth is why: a run that
 * timed out or could not find its toolchain never observed an assertion, and
 * scoring its non-zero exit as "red" is exactly the bug `CmdOutcome`'s
 * `assertion_result` exists to close (#860) — an infra failure would satisfy
 * a flip's precondition and the next clean run would be credited as a fix.
 */
export type TestBaseline = "not-run" | "passed" | "failed" | "unobserved";

/**
 * One test invocation, as the host already parsed it.
 *
 * **argv, never a shell string** — the #1400 rule every spawned thing in this
 * workspace follows, and the reason the host's own strict test-command parser
 * runs before a grant is minted rather than inside the plugin. A plugin
 * receives a program and its arguments; it never receives a line to hand to a
 * shell, and a command the host's parser refuses produces no grant to carry
 * one.
 */
export interface TestPlan {
  /**
   * Its exact argument vector.
   */
  args?: string[];
  /**
   * What this same invocation reported *before* the turn ran — the red half
   * of a fail→pass flip, handed over so a plugin does not have to
   * reconstruct it (or take it from an environment variable, which is what
   * Track C had to do).
   */
  baseline?: TestBaseline;
  /**
   * The test runner executable, from the host's closed runner vocabulary.
   */
  program: string;
}

/**
 * The read-only report of a turn that finished.
 *
 * The engine always finishes its own turn and always says so; `completed` is
 * that statement, and a wrapper's "the whole job is over" is a separate,
 * separately named thing ([`Continuation`]) that cannot fake it.
 *
 * # Why two of the four fields are optional
 *
 * `tools` and `changed_files` are facts a host **may not have**, and the two
 * answers a plugin needs to tell apart are "the turn dispatched no tools /
 * changed no files" and "this host does not report them". They were plain
 * `Vec`s until #3552, so every host that could not measure them sent `[]` —
 * which reads as the first answer and *is* the second. A wrapper that gates
 * its evidence on "did the turn touch anything" then graded every run as
 * untouched, and nothing in the message let it notice.
 *
 * So the absent case is spelled `None` (the key is omitted on the wire) and
 * the empty case is spelled `Some(vec![])` (`[]`). Additive:
 * [`PROTOCOL_VERSION`] is unchanged, a plugin written against the old shape
 * reads `[]` exactly where it always did, and a host that omits the key sends
 * bytes the old readers already accepted as "no entries" — the difference is
 * that a reader who *cares* can now ask.
 */
export interface TurnOutcome {
  /**
   * The final assistant text.
   */
  answer?: string;
  /**
   * Workspace-relative paths the turn changed, or `None` when this host does
   * not measure them. `Some(vec![])` is "the turn changed nothing".
   */
  changed_files?: string[] | null;
  /**
   * Whether the engine reported the turn complete, as opposed to aborted.
   */
  completed: boolean;
  /**
   * The tools the turn dispatched, in call order, by name — or `None` when
   * this host does not observe them. See the type docs: `Some(vec![])` is
   * "the turn dispatched none", which is a different claim.
   */
  tools?: string[] | null;
}

/**
 * Context a wrapper contributes to a turn.
 *
 * **The invariant-7 constraint, encoded in the type.** Contributed context
 * rides as a volatile message *after* the byte-stable system-prompt prefix,
 * never inside it: prompt-cache hits are a feature, and a wrapper that could
 * inject into the stable prefix would make every installed plugin a per-turn
 * cost regression for every user who installed it. That is the same
 * discipline `crates/stella-cli/src/agent/prompt.rs::build_system_prompt` and
 * `crates/stella-cli/src/memory.rs` already hold for recalled context.
 *
 * So this type carries no placement field. There is no `Placement::System`
 * to pick, no `stable: bool` to set, and **the body is not reachable as a
 * field**: the one way out of the value is
 * [`VolatileContext::into_message`], which builds a
 * [`CompletionMessage::user`] — a message that by construction is not index
 * 0's system message.
 *
 * The privacy is the enforcement, and it was prose until #3524. While `text`
 * was public, a host could write `prompt.push_str(&ctx.text)` into the stable
 * prefix, change no type, and stay green — the unit test below constructs a
 * value and calls `into_message`, so it can say nothing about a caller that
 * never does. Every installed plugin then became a per-turn prompt-cache
 * miss, which is exactly the cost regression this type exists to make
 * unrepresentable. Now that line does not compile out of crate, which the
 * `compile_fail` doctest below pins:
 *
 * ```compile_fail,E0616
 * let ctx = stella_plugin::VolatileContext::new("recall", "the last run failed on I/O");
 * let mut prompt = String::from("<system prefix>");
 * prompt.push_str(&ctx.text);
 * ```
 *
 * The sanctioned reads, and all of them: the provenance label a journal
 * prints, and spending the value as the message it is.
 *
 * ```
 * use stella_plugin::VolatileContext;
 * let ctx = VolatileContext::new("recall", "the last run failed on I/O");
 * assert_eq!(ctx.label(), "recall");
 * assert_eq!(ctx.into_message().content, "the last run failed on I/O");
 * ```
 *
 * What this does **not** claim: that a host cannot splice the body in
 * anywhere at all. A caller that spends the contribution and then reaches
 * into the returned [`CompletionMessage`] has unwrapped a *user* message on
 * purpose, in a line that says so; no type can stop that, and pretending
 * otherwise would be the same overclaim the field made. What is closed is the
 * cheap path — the one a contributor takes without noticing they took it.
 */
export interface VolatileContext {
  /**
   * A short human-readable name for where this came from, for the journal
   * and the deck. Not shown to the model as a header — the host decides
   * presentation. Read through [`VolatileContext::label`].
   */
  label: string;
  /**
   * The context itself. Private on purpose — see the type docs; the only
   * exit is [`VolatileContext::into_message`].
   */
  text: string;
}

/**
 * One request on the wire: `{"point": …, "body": {…}}`.
 *
 * Adjacently framed rather than internally tagged so the body keeps
 * `deny_unknown_fields` — an internally tagged enum hands the tag field down
 * into the variant, where a denying struct rejects it. `Serialize` is derived
 * from that framing; `Deserialize` is written out by hand over a two-key
 * envelope that denies unknown fields, because the derived reader for the
 * same framing accepts any number of keys beside `point` and `body` and drops
 * them silently (#3500 — see this module's `Envelope`).
 */
export type WrapperRequest = {
  body: BeforeTurnRequest;
  point: "before_turn";
} | {
  body: AfterTurnRequest;
  point: "after_turn";
};

/**
 * One response on the wire, in the same framing as [`WrapperRequest`] — and
 * refused the same way when it carries a key this host does not know.
 */
export type WrapperResponse = {
  body: BeforeTurnResponse;
  point: "before_turn";
} | {
  body: AfterTurnResponse;
  point: "after_turn";
};

/**
 * Every point this build's socket speaks. Both directions are tagged with it
 * and both carry the same two values: the tag says which point a message
 * belongs to, never whether it is a request or a response.
 */
export type WrapperPoint =
  | "before_turn"
  | "after_turn";
