// GENERATED FILE — DO NOT EDIT.
//
// Regenerate with:  bash scripts/export-agentevent-schema.sh
// Source of truth:  stella-plugin/src/driver.rs
// Guarded by:       scripts/check-wire-schema.sh (`make wire-schema`)
//
// The driver channel: the session a plugin opens when it drives turns,
// instead of sitting inside one. The host opens the session with a
// DriveRequest. The driver asks for a capability with a DriverCallRequest,
// and the host answers with a DriverCallResponse. The driver ends the
// session with a DriveResponse naming what should happen next.
//
// The host-call channel crosses the same pipes and is not described here.
// One of its answers is an untagged union. Its arms are told apart only by
// their required keys, a rule JSON Schema can state and cannot check.
// docs/wire/wrapper.wire.json shows every one of its messages as the exact
// bytes a parser meets.
//
// Every capability ask names a call. The host's grant either permits or
// refuses it. A refusal is a value the driver branches on, never a death.

/**
 * Why a unit is being released.
 *
 * A sentence rather than a code, because the audience is a human reading the
 * loop's ledger afterwards. A release with no reason is the silence
 * `doc:backlog-self-driving` §3.7 refuses at the session's own terminal, one
 * level down: a loop that gives an issue back without saying why teaches
 * nobody anything.
 */
export interface AbandonArgs {
  /**
   * What a human reads in the ledger.
   */
  reason: string;
}

/**
 * One issue, as the driver channel carries it.
 *
 * Four fields, and not `stella_protocol::Issue`. A plugin holds
 * no forge token, so what it needs is the identity to name in its next ask,
 * the line a human reads in its log, and the labels its own policy branches
 * on. The body is the field this must not carry: issue text is written by
 * whoever can file an issue, and pushing it through a plugin's process would
 * make untrusted prose the plugin's problem to handle rather than the host's
 * (`doc:agent-native-delivery` §10.2).
 */
export interface BacklogEntry {
  /**
   * The tracker's own identifier, as the tracker spells it — `"1234"`,
   * `"STELLA-42"`. Opaque: a driver hands it back and never parses it.
   */
  key: string;
  /**
   * The labels the tracker carries, verbatim. Priority and area live here.
   */
  labels?: string[];
  /**
   * The issue's one-line title.
   */
  title: string;
  /**
   * Where a human would go to read it, or empty when the tracker offers no
   * address.
   */
  url?: string;
}

/**
 * The ranked queue one [`DriverCall::BacklogNext`] read produced.
 *
 * A page rather than a single issue, because a loop that skips a unit it
 * cannot take needs the next one without spending a second ask, and because a
 * driver seeing only the top of the ranking could not tell one unit of work
 * from a backlog with one issue in it.
 */
export interface BacklogPage {
  /**
   * The open issues the host read, most-ready first. Empty means the
   * tracker answered and had nothing ready — a failed read is a
   * [`HostCallFailure`], never an empty page.
   */
  issues?: BacklogEntry[];
}

/**
 * Who holds a unit of backlog, after [`DriverCall::BacklogClaim`] asked.
 *
 * Three answers, and they are different facts a loop branches on: the ask
 * took the lease, a live peer already holds it, or the ledger could not say.
 * The third is not the second — an unopenable ledger is not a peer, and a
 * loop that read it as one would stop working on its own crash — so
 * [`Self::holder`] is empty exactly when nobody was named.
 */
export interface ClaimReport {
  /**
   * Whether this session now holds the lease.
   */
  held: boolean;
  /**
   * Who holds it instead, or empty when nobody was named — either because
   * this session took it, or because the ledger could not answer.
   */
  holder?: string;
  /**
   * The unit the ask named, echoed so a pipelined driver can join it.
   */
  issue: string;
}

/**
 * What `deliver_next` decides over: one reading, and what this loop has spent
 * on this pull request so far.
 *
 * The counts are the driver's own. The host keeps no memory of a driver's
 * earlier cycles. A session is one conversation, and a loop that pushed two
 * fixes across two days is the only party that knows it.
 */
export interface DecideArgs {
  /**
   * Fix pushes already made against this pull request's own red build.
   */
  fixes?: number;
  /**
   * What the driver read.
   */
  observation: DeliverObservation;
  /**
   * Rebases already made against a moved base.
   */
  rebases?: number;
}

/**
 * The single next thing to do.
 */
export type DeliverAction = "wait" | "push_fix" | "rebase" | "mark_ready" | "merge" | "escalate";

/**
 * What one read of the forge said about a pull request's checks.
 *
 * Three, not a forge's whole vocabulary. A cancelled run, a skipped run and a
 * run that never started all mean "no answer yet" here.
 */
export type DeliverCi = "pending" | "green" | "red";

/**
 * What the machine decided, given one reading.
 */
export interface DeliverDecision {
  /**
   * The one next action.
   */
  action: DeliverAction;
  /**
   * What ran out, when the action is [`DeliverAction::Escalate`].
   *
   * Absent otherwise. A reason on a `wait` would be a sentence about an
   * escalation that never happened.
   */
  escalation?: DeliverEscalation | null;
  /**
   * The pull request this is about.
   */
  pr: string;
  /**
   * Where it now stands.
   */
  state: DeliverState;
}

/**
 * Why the machine gave up.
 *
 * Typed, because a human reading an escalation needs to know what to do next.
 * Fix something, wait for something, or raise a ceiling are three different
 * moves.
 */
export type DeliverEscalation = "fix_ceiling_reached" | "rebase_ceiling_reached" | "review_needs_human";

/**
 * Whether the branch merges as it stands.
 */
export type DeliverMergeability = "clean" | "conflicted" | "unknown";

/**
 * One read of the forge. Facts only. The decision is [`DeliverDecision`].
 *
 * Every fact a decision reads is required on the wire. Leave one out and the
 * decision runs over a default nobody observed. The default that matters most
 * is an unknown mergeability, and it reads as "wait".
 */
export interface DeliverObservation {
  /**
   * What the *same* checks said on the base branch.
   *
   * The whole comparison rests on this. Without it a red build looks the
   * same as an inherited one. A loop then spends its budget fixing somebody
   * else's breakage.
   */
  base_ci: DeliverCi;
  /**
   * What this pull request's checks said.
   */
  ci: DeliverCi;
  /**
   * Whether it is still a draft.
   */
  draft?: boolean;
  /**
   * Whether the branch still merges cleanly.
   */
  mergeable: DeliverMergeability;
  /**
   * The pull request this is about, echoed back so a driver can join it.
   */
  pr: string;
  /**
   * What review has said.
   */
  review: DeliverReview;
  /**
   * Whether it has already merged or closed.
   *
   * Beside the facts rather than among them. The machine does not decide
   * over it. A merged pull request reports an unknown mergeability, which
   * reads as "wait" forever.
   */
  settled?: boolean;
}

/**
 * What human review has said, if anything.
 */
export type DeliverReview = "none" | "changes_requested" | "approved";

/**
 * Where a pull request the loop opened now stands.
 */
export type DeliverState = "draft" | "ci_pending" | "ci_red" | "base_broken" | "conflicted" | "ready_for_review" | "review_changes_requested" | "approved" | "merged" | "escalated";

/**
 * What a driver asks to happen after this session.
 *
 * Two answers, both of them terminal for the session and neither of them a
 * silence. `Sleep` is `doc:backlog-self-driving` §3.7's `Watch`, reduced to the
 * only wake condition B0 can honestly serve — a duration. The condition-shaped
 * wakes (`WakeCondition`) arrive with the supply model in B4.
 */
export type DriveNext = {
  sleep: {
    /**
     * How long to wait. The host clamps it; a driver cannot buy an
     * unbounded absence by asking for one.
     */
    secs: number;
  };
} | {
  halt: {
    /**
     * The sentence a human reads in the loop's ledger.
     */
    reason: string;
  };
};

/**
 * The one point a driver session dispatches.
 *
 * A closed single-case enum rather than a bare string, so `"point":
 * "before_turn"` written into a driver session is a decode error rather than
 * something a reader shrugs at. A second driver point would be a reviewable
 * addition here.
 */
export type DrivePoint = "drive";

/**
 * The host's opening of a driver session: `{"point": "drive", "body": {…}}`.
 *
 * The body carries the session identity and nothing else at B0. The loop state
 * `doc:backlog-self-driving` §3.0 sketches beside it is `LoopState`, which is
 * B2's — pure, in `stella-autonomy`, and not yet written. Naming a field for
 * it here would be pinning a wire shape to a type that does not exist.
 */
export interface DriveRequest {
  /**
   * The session body.
   */
  body: DriveSession;
  /**
   * Which point is open. Always [`DrivePoint::Drive`]; present so the
   * request is self-describing on the wire.
   */
  point: DrivePoint;
}

/**
 * The response that ends a driver session: which point is open — always "drive" — and what the driver wants to happen next.
 */
export interface DriveResponse {
  body: {
    next: DriveNext;
  };
  point: DrivePoint;
}

/**
 * What the host tells a driver when it opens the session.
 */
export interface DriveSession {
  /**
   * The host's identifier for this session, echoed into its telemetry.
   */
  session: string;
}

/**
 * What a capability ask is **about** — one member per verb that reads one.
 *
 * The mirror of [`DriverOk`], on the way in. Every member is omitted when
 * absent, so a verb that reads nothing sends no table at all and the bytes an
 * ask written against B0 sends are unchanged. The table denies unknown
 * fields, so an argument this contract does not have is a decode error on the
 * host's side rather than a value silently dropped.
 *
 * **A table names exactly the verb the ask names.** [`Self::tables`] is what
 * a host checks that with: an ask carrying `work_start` arguments under a
 * `work_status` call is two claims about what the driver wants, and believing
 * one of them quietly is the failure [`DriverMessage`]'s own envelope refuses
 * one layer out.
 *
 * `work_status` and `deliver_open` carry no member here, because they act on
 * the unit the session already holds. A key on either could only name a
 * different unit from the one it would report on.
 */
export interface DriverArgs {
  /**
   * Which unit [`DriverCall::BacklogClaim`] should take the lease on.
   */
  backlog_claim?: UnitArgs | null;
  /**
   * Which pull request [`DriverCall::DeliverMerge`] should merge.
   */
  deliver_merge?: PullRequestArgs | null;
  /**
   * What [`DriverCall::DeliverNext`] should decide over.
   */
  deliver_next?: DecideArgs | null;
  /**
   * Which pull request [`DriverCall::DeliverObserve`] should read.
   */
  deliver_observe?: PullRequestArgs | null;
  /**
   * Which pull request [`DriverCall::DeliverReady`] should take out of
   * draft.
   */
  deliver_ready?: PullRequestArgs | null;
  /**
   * Why [`DriverCall::WorkAbandon`] is releasing the session's unit.
   */
  work_abandon?: AbandonArgs | null;
  /**
   * Which unit [`DriverCall::WorkStart`] should work.
   */
  work_start?: UnitArgs | null;
}

/**
 * The capabilities a **driver** may ask the host for.
 *
 * Closed, and [`DriverGrant::calls`] declares which of them a given driver may
 * use. The entries in [`DriverCall::all`] are exactly the verbs
 * `doc:backlog-self-driving` §3.1–§3.5 tabulates, in five families:
 *
 * - **`backlog`** (§3.1) — the provider-agnostic issue port that replaces
 *   three hardcoded `gh` call sites. Every other family depends on it: `work`
 *   needs a unit, `sweep` needs somewhere to file, `deliver` needs something
 *   to close.
 * - **`work`** (§3.2) — one unit of backlog through Stella's own staged
 *   pipeline, in `candidate_ws.rs`'s shadow worktree, with the pipeline's
 *   verdict as the definition of done.
 * - **`deliver`** (§3.3) — branch, PR, CI, draft, review, merge, driven by a
 *   pure state machine that buys no model call.
 * - **`sweep`** (§3.4) — where the next question comes from.
 * - **`curate`** (§3.5) — proposals for tools, context records and skills,
 *   which the loop may *propose* and never grant itself (§7).
 *
 * Two things are absent, and both are `doc:backlog-self-driving`
 * §3.6's own rules rather than an oversight: there is **no `release` verb** at
 * B0–B6, and there is **no verb that edits a driver's own grant** — a
 * capability that could widen `calls` would make the grant advisory.
 *
 * Every verb is its own case rather than a family with a mode parameter,
 * because AGENTS.md #9 governs a capability the same way it governs a tool: a
 * parameter may scope an operation, never select one. `curate accept` and
 * `curate reject` would be two calls, not one with a boolean — which is why
 * the latter is simply not here yet.
 */
export type DriverCall = "backlog_next" | "backlog_claim" | "backlog_file" | "backlog_close" | "backlog_link" | "work_start" | "work_status" | "work_abandon" | "deliver_open" | "deliver_observe" | "deliver_next" | "deliver_ready" | "deliver_merge" | "sweep_audit" | "sweep_regress" | "sweep_meta" | "curate_propose" | "curate_list" | "curate_accept";

/**
 * One capability ask a driver writes mid-session. `id` is the driver's own correlation number, echoed back on the answer, so a driver that pipelines two asks can tell the answers apart. `args` is the named verb's own table, omitted for a verb that reads none.
 */
export interface DriverCallRequest {
  /**
   * What the ask is about, for a verb that reads one.
   *
   * Omitted for a verb that reads none, which is what every ask written
   * against B0 sends. A table naming a verb other than [`Self::call`] is a
   * refusal rather than a value the host quietly drops — see
   * [`DriverArgs::tables`].
   */
  args?: DriverArgs | null;
  /**
   * Which capability.
   */
  call: DriverCall;
  /**
   * What the driver calls this ask, echoed on the answer.
   */
  id: number;
}

/**
 * The host's answer to one capability ask: `result`, the id the ask carried, plus exactly one of `ok` (what the host did) or `err` (why it would not or could not).
 */
export type DriverCallResponse = {
  ok: DriverOk;
  result: number;
} | {
  err: HostCallFailure;
  result: number;
};

/**
 * What a successful capability ask returned.
 *
 * One member per verb the host performs, and nothing for the verbs it does
 * not. That is the rule this module's header sets, and [`BacklogPage`] is the
 * first thing to land under it: a result shape is written when the verb
 * reporting it is served, so no field here describes behaviour no host code
 * can produce.
 *
 * Every member is omitted when absent, so an answer to a verb that reports
 * nothing is `{}` on the wire — the bytes every driver written against B0
 * already reads. The table denies unknown fields, so a host answering with a
 * payload the contract does not have is a decode error on the driver's side
 * rather than a value silently dropped.
 */
export interface DriverOk {
  /**
   * What [`DriverCall::BacklogNext`] answered with: the tracker's open
   * queue, in the order the loop should take it.
   *
   * Absent for every other verb. An `Option` rather than an empty page
   * because "no queue was asked for" and "the queue is empty" are different
   * facts, and a loop branches on both.
   */
  backlog?: BacklogPage | null;
  /**
   * What [`DriverCall::BacklogClaim`] answered with: who holds the unit
   * now.
   *
   * Absent for every other verb, on [`Self::backlog`]'s terms.
   */
  claim?: ClaimReport | null;
  /**
   * What [`DriverCall::DeliverNext`] answered with: the one next action.
   */
  decision?: DeliverDecision | null;
  /**
   * What [`DriverCall::DeliverMerge`] answered with: the pull request that
   * landed.
   */
  merge?: MergeReport | null;
  /**
   * What [`DriverCall::DeliverObserve`] answered with: one read of the
   * forge.
   */
  observation?: DeliverObservation | null;
  /**
   * What [`DriverCall::DeliverOpen`] answered with: the pull request it
   * pushed the session's unit into.
   */
  pull_request?: OpenReport | null;
  /**
   * What [`DriverCall::DeliverReady`] answered with: the pull request that
   * is now out of draft.
   */
  ready?: ReadyReport | null;
  /**
   * What [`DriverCall::SweepRegress`] and [`DriverCall::SweepMeta`]
   * answered with: one draw from one supply.
   *
   * One member for both verbs, and [`SweepReport::supply`] says which one
   * answered. The two report the same counts over different records, so a
   * member each would be one shape wearing two names.
   */
  sweep?: SweepReport | null;
  /**
   * What the three `work` verbs answered with: the session's unit, and what
   * the tree holds for it.
   */
  work?: WorkReport | null;
}

/**
 * Why a host call did not return a result.
 *
 * The code is closed so a plugin can branch on it; the detail is the sentence a
 * human reads when the plugin reports what it could not get.
 */
export interface HostCallFailure {
  /**
   * Why, in words — for the plugin's log and the host's report. Never the
   * thing a plugin branches on.
   */
  detail: string;
  /**
   * Which refusal, as the plugin's own `match` reads it.
   */
  refusal: HostCallRefusal;
}

/**
 * The closed set of reasons a host call comes back empty.
 *
 * Each one is a different thing for a plugin to do next, which is why they are
 * not one code with a message: an [`Undeclared`](Self::Undeclared) call is the
 * plugin author's manifest to fix, a [`Forbidden`](Self::Forbidden) one is an
 * ask no manifest can buy, an [`Unsupported`](Self::Unsupported) one is
 * this host's gap, an [`AllowanceSpent`](Self::AllowanceSpent) one means stop
 * asking and answer, [`Unavailable`](Self::Unavailable) means the capability
 * exists but this host has no plane behind it, and [`Failed`](Self::Failed)
 * means the host tried.
 */
export type HostCallRefusal = "undeclared" | "forbidden" | "unsupported" | "allowance-spent" | "unavailable" | "failed";

/**
 * The pull request `deliver_merge` merged.
 *
 * One field. The ask succeeding is the whole answer. A merge that did not
 * happen comes back as a refusal naming what the host saw, not as a report
 * with a false flag in it. The number is here so a driver that sent two asks
 * can tell which one landed.
 */
export interface MergeReport {
  /**
   * The pull request that merged.
   */
  pr: string;
}

/**
 * The pull request `deliver_open` opened.
 */
export interface OpenReport {
  /**
   * The branch it carries.
   */
  branch: string;
  /**
   * The unit it closes.
   */
  issue: string;
  /**
   * The pull request, as the forge numbers it.
   */
  pr: string;
}

/**
 * Which pull request an ask is about: the forge's own number, as a string, opaque to the driver — a second forge numbers differently, and a driver hands back what the host gave it.
 */
export interface PullRequestArgs {
  /**
   * The pull request.
   */
  pr: string;
}

/**
 * The pull request `deliver_ready` took out of draft.
 *
 * One field, on [`MergeReport`]'s terms: the ask succeeding is the whole
 * answer, and a pull request that stayed a draft comes back as a refusal
 * naming what the host saw.
 */
export interface ReadyReport {
  /**
   * The pull request that is now out of draft.
   */
  pr: string;
}

/**
 * How much of the closure ledger one `sweep_regress` draw could re-ask.
 */
export interface SweepReceipts {
  /**
   * Those whose cited change could be looked for on the base.
   */
  checked: number;
  /**
   * Those that could not be, one entry per reason.
   */
  skipped?: SweepSkip[];
  /**
   * Receipts on file.
   */
  total: number;
}

/**
 * What one draw from one supply produced.
 *
 * Three counts and the keys, because a driver decides what to do next from
 * them. A sweep that offered nothing means the supply is dry and the loop
 * should watch. A sweep that offered plenty and filed none means the seen set
 * is holding it all back. Those are different states and call for different
 * answers.
 */
export interface SweepReport {
  /**
   * The tracker keys the sweep filed, as the tracker spells them.
   *
   * Shorter than [`Self::fresh`] when a filing was refused or read as a
   * repeat. Both are ordinary answers, and neither is an error.
   */
  filed?: string[];
  /**
   * How many of those the seen set did not already hold.
   */
  fresh: number;
  /**
   * How many findings the supply offered, before the seen set was read.
   */
  offered: number;
  /**
   * What the closure ledger could answer, for the supply that reads one.
   *
   * Absent for `sweep_meta`, which folds the cycle ledger and reads no
   * receipts.
   */
  receipts?: SweepReceipts | null;
  /**
   * Which supply this is a report about.
   */
  supply: SweptSupply;
}

/**
 * One reason receipts could not be re-checked, and how many carried it.
 */
export interface SweepSkip {
  /**
   * How many receipts this reason accounts for.
   */
  count: number;
  /**
   * Why.
   */
  reason: SweepSkipReason;
}

/**
 * Why one closed issue's receipt could not be re-checked.
 *
 * A receipt says what a closure cited and whether that change was on the base
 * branch at the time. Missing either fact makes *gone* and *never seen* look
 * alike, and filing the wrong one of those costs a person an hour.
 */
export type SweepSkipReason = "no_change_cited" | "unknown_at_close" | "absent_at_close";

/**
 * Which supply a sweep drew from.
 *
 * Echoed on the answer, so a driver that pipelines two asks can read one
 * report without joining it back to the ask that produced it.
 */
export type SweptSupply = "regress" | "meta";

/**
 * The unit of backlog an ask is about.
 *
 * The tracker's own key, as [`BacklogEntry::key`] spelled it. Opaque to the
 * driver, which hands back what the host handed it and never parses it — a
 * second tracker spells its keys `STELLA-42` and the loop's policy must not
 * have an opinion about that.
 */
export interface UnitArgs {
  /**
   * The tracker key.
   */
  issue: string;
}

/**
 * The session's unit of work, as the three `work` verbs report it.
 */
export interface WorkReport {
  /**
   * The branch the work sits on, for `deliver` to push. Empty unless the
   * tree holds a change.
   */
  branch?: string;
  /**
   * What the turn said, or why the unit did not finish. Empty when there is
   * nothing to add.
   */
  detail?: string;
  /**
   * The unit, or empty when the session holds none.
   */
  issue?: string;
  /**
   * The summary line `git` printed for what the turn left behind. Empty
   * unless the tree holds a change.
   */
  stat?: string;
  /**
   * Where the unit stands.
   */
  state: WorkState;
}

/**
 * Where a unit of work stands.
 *
 * [`Self::NoChange`] is **not** a failure and the two are separate cases for
 * that reason: an issue the loop cannot act on is a real answer, and folding
 * it into failure is how a loop learns to report noise
 * (`crates/stella-cli/src/self_driving_cmd/work.rs` argues it at the door
 * that produces it).
 */
export type WorkState = "idle" | "changed" | "no_change" | "failed";
