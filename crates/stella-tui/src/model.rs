//! The pure render model: a deterministic fold of the `AgentEvent` log into
//! the derived state every panel draws from ( L-T1).
//!
//! [`SessionModel`] owns **only** state that is reconstructible by replaying
//! the event log from seq 1 — transcript lines, the files-touched map, HUD
//! numbers, and the pending scope-review. It has exactly one mutator,
//! [`SessionModel::apply`]; there is no other way to change it. Ephemeral
//! interaction state (scroll offset, composer buffer, panel focus) that is
//! *not* derived from events lives in [`crate::deck_ui::DeckUi`], never here —
//! that boundary is what makes replay-from-seq-1 a supported debug mode and
//! what makes the panel panic boundary sound (render is a pure function over
//! `&SessionModel`, so a panicking panel can be caught and discarded without
//! leaving torn state — L-T7).
//!
//! Styling is deliberately *not* stored here: entries are semantic records,
//! and [`mod@crate::render`] converts them to styled `ratatui` lines as a pure
//! function of the model. Determinism therefore extends all the way to the
//! backing cell buffer (the replay-determinism test in [`mod@crate::render`]).

use crate::ansi::strip_ansi;
use stella_protocol::{
    AgentEvent, FileChangeKind, HunkProposal, ScopeProposal, StageKind, StageScope, SubAgentPhase,
    TaskItem, TaskStatus, ToolOutput,
};

use std::collections::VecDeque;

mod diff_budget;
pub mod entry;
mod error_rows;
pub mod file_state;
mod inline_diff;
pub mod recall;
mod summarize;
mod turn;

pub use diff_budget::DIFF_TEXT_BUDGET;
// Re-exported flat, so `crate::model::TranscriptEntry` still resolves and the
// split moved no call site — same discipline as `file_state` and `turn` below
// (#4217). `entry`'s module doc carries why the seam is declarations-vs-logic.
pub use entry::{
    AskUserPrompt, InlineDiffRef, OpenPark, ReadSize, SubAgentSummary, TranscriptEntry,
};
pub use file_state::{FileState, MAX_TRACKED_FILES, RememberedDiff};
// The renderer's, not the fold's: the count a multi-path row states is decided
// where the claims are defined, so the row cannot arrive at a different one
// (#4214).
pub(crate) use inline_diff::distinct_paths as distinct_diff_paths;
pub use recall::{RecallBudget, RecalledFrameRow};
pub use turn::{Hud, TurnCounters, TurnOpening, TurnReceipt};
// The role predicate, for the AGENTS-tab fold in `super::deck`, which folds
// `StepUsage` — a record with a role and no `call_seq` (#4307).
pub(crate) use turn::role_supplies_the_turns_model;
// Re-imported rather than left qualified, so the split was a pure move: every
// call site in the fold reads exactly as it did before (#2958). See
// `summarize`'s module doc for why the seam is there and not elsewhere.
use summarize::{
    INPUT_BUDGET, OUTPUT_BUDGET, cap_input_json, cap_middle, format_tool_input, is_file_mutation,
    summarize, tool_input_path,
};

/// How many characters of a tool input / output summary we retain on a
/// transcript line before eliding — the full payload is never needed on the
/// one-line card (the diff panel and detail views carry the rest).
const SUMMARY_BUDGET: usize = 200;

/// Retention cap on transcript entries. The per-entry char budgets
/// ([`INPUT_BUDGET`], [`OUTPUT_BUDGET`]) bound one entry to ~20 KiB, but
/// without an entry-count cap a long-running session grows without bound;
/// 4 000 entries bounds the worst case to low tens of MiB while staying far
/// deeper than any scrollback a user actually walks. Below the cap the fold
/// is unchanged.
pub(crate) const MAX_TRANSCRIPT_ENTRIES: usize = 4_000;

/// Entries dropped per eviction pass — 10% of the cap, so the O(chunk) drain
/// and the deck fold-cache rebuild amortize over hundreds of events instead
/// of firing on every push once the cap is reached.
pub(crate) const TRANSCRIPT_EVICTION_CHUNK: usize = MAX_TRANSCRIPT_ENTRIES / 10;

// A pass must drop more than the one marker it inserts (or the transcript
// never shrinks) and must never drain the live tail.
const _: () =
    assert!(TRANSCRIPT_EVICTION_CHUNK >= 2 && TRANSCRIPT_EVICTION_CHUNK < MAX_TRANSCRIPT_ENTRIES);

/// The whole derived state of a session, folded from its `AgentEvent` log.
///
/// Every field is a pure function of the sequence of events applied so far;
/// two `SessionModel`s that have seen the same event vector are identical
/// (the L-T1 replay-determinism guarantee, exercised by tests here and in
/// [`mod@crate::render`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionModel {
    /// Turns this session has completed — the ordinal stamped on each
    /// [`TranscriptEntry::Complete`] so a closing rule can name its turn
    /// (SPEC 6.1).
    ///
    /// A counter rather than a derivation, because the two obvious derivations
    /// both lie: counting `Complete` entries in the transcript renumbers
    /// everything once front-eviction drops one, and `AgentEvent` carries a
    /// `turn_instance` that is per-*run*, so a wrapped run restarts it while
    /// the scrollback does not.
    pub turns_completed: u32,
    /// Whether the turn in flight has already drawn its SPEC 6.1 opening rule.
    ///
    /// The turn's *first* stage boundary opens it and every later one is a
    /// plain section rule ([`TurnOpening`]). Kept as a latch rather than
    /// derived from the trailing transcript entries because the fold is the
    /// only place that can see the boundary: front-eviction can drop the
    /// opening rule itself, and a derivation that looked backwards for one
    /// would re-open the turn the moment the retention cap bit.
    ///
    /// Cleared by whatever ends a turn — a `TurnComplete`, the `RunComplete`
    /// that ends the run, a terminal `Error`, and `/clear`.
    turn_head_stamped: bool,
    /// Where in [`Self::transcript`] the turn in flight stamped its opening
    /// rule, so the first answering call can fill in the model it was opened
    /// without ([`TurnOpening::model`]).
    ///
    /// A back-patch rather than a deferred push: the boundary has to appear at
    /// the position the stage arrived in, and the model is not knowable until a
    /// call commits some way into the turn. Holding the entry back until then
    /// would file the turn's own events above its rule.
    ///
    /// An index, and therefore something front-eviction has to maintain —
    /// [`Self::evict_transcript_overflow`] rebases it and drops it outright
    /// when the rule itself is the thing evicted. Cleared by everything that
    /// clears `turn_head_stamped`, so a call landing between turns patches
    /// nothing: the rule the previous turn stamped is settled.
    turn_head_idx: Option<usize>,
    /// Per-turn counters SPEC 6.1's receipt is stamped from, reset at every
    /// turn boundary ([`TurnCounters`]).
    pub turn_counters: TurnCounters,
    /// The scrollback transcript, oldest first. Streaming `Text`/`Reasoning`
    /// deltas are accumulated into the trailing entry rather than producing
    /// one line per token.
    pub transcript: Vec<TranscriptEntry>,
    /// Files the agent touched, in first-touched order, each retaining every
    /// diff that rode its `FileChange` events (L-T5 — there is no second data
    /// path for diffs). Capped at [`MAX_TRACKED_FILES`] rows; the
    /// least-recently-touched path is evicted to admit a new one. The diff
    /// *text* is bounded separately and in bytes by [`DIFF_TEXT_BUDGET`],
    /// because a count of paths never bounded a count of bytes (#4365).
    ///
    /// Outlives the conversation: [`Self::reset_conversation`] keeps this and
    /// the two fields below, because a `/clear` does not un-write the bytes on
    /// disk — and `deck::WorkspaceModel::ledger`, the Files tab's rows, is not
    /// reset either.
    pub files: Vec<FileState>,
    /// How many paths [`MAX_TRACKED_FILES`] eviction has dropped — surfaced
    /// in the files panel title so a capped ledger never reads as complete.
    pub files_evicted: u32,
    /// Monotonic touch counter stamping [`FileState::touched_seq`].
    file_touch_seq: u64,
    /// The bound on remembered diff text, in bytes — see [`mod@diff_budget`].
    diff_budget: diff_budget::DiffBudget,
    /// Measured changes no transcript row has claimed yet, and the rule for
    /// which row may claim one — see [`mod@inline_diff`].
    claims: inline_diff::ClaimWindow,
    /// For each in-flight call, whatever its tool, the position in `claims` at
    /// the moment it dispatched.
    ///
    /// This is what lets a `ToolResult` tell *which producer owes it a change*:
    /// a measurement recorded above this mark folded between the call's start
    /// and its result, which only the registry's per-call measurement does
    /// (#4175), so the row claims it; nothing above the mark means the call
    /// moved the tree not at all, and any change the turn boundary sweeps up
    /// afterwards belongs to somebody else. Entered on the `ToolStart` and
    /// taken on the matching result, so it holds only calls actually in flight
    /// — a turn abandoned mid-call leaves at most its own entry, cleared with
    /// the rest of the conversation.
    claim_baselines: std::collections::HashMap<String, u64>,
    /// Whether the registry's **per-call** measurement has ever been observed
    /// answering in this session (#4227).
    ///
    /// Which producer measures is a property of the *session*, not of a call:
    /// `SessionDurability::snapshot_worktree` either has a work journal or it
    /// does not, so a session with one measures every solo mutating call and a
    /// session without one measures none. One observed per-call answer settles
    /// which of those a session is, and that is what makes a row's *empty* own
    /// measurement readable: under a live per-call producer it means the call
    /// genuinely moved nothing, and the change the turn boundary sweeps up next
    /// belongs to somebody else.
    ///
    /// Latched, never cleared: a producer that answered once has demonstrated
    /// it exists, and a later call it did not measure (a concurrently
    /// dispatched group, which stays on the boundary sweep by #4175's own
    /// scope note) is evidence about that call, not about the journal.
    ///
    /// Survives [`Self::reset_conversation`] with the file ledger it describes
    /// — `/clear` un-writes no bytes and takes away no journal.
    per_call_producer_seen: bool,
    /// Live HUD numbers: spend/limit/mode, current stage, model.
    pub hud: Hud,
    /// **The plan** — the one surface for what stella said it would do and how
    /// far through it is ([`crate::plan`]).
    ///
    /// Folded from both streams that used to render as separate panels: the
    /// gate's proposal supplies the steps, `TaskUpdate` supplies their states.
    /// The two fields below remain because the *gate* and the scope's
    /// non-step detail (globs, budget, routing) still need them; nothing
    /// renders a step list from them any more.
    pub plan: crate::plan::Plan,
    /// A scope-review gate awaiting the user's decision (L-E5). Set by a
    /// `ScopeReview` event and cleared by the engine's follow-on event
    /// (a non-scope-review `Stage`, `Complete`, or `Error`) — so the pending
    /// state is itself purely event-derived and reconstructs on replay.
    pub pending_scope_review: Option<ScopeProposal>,
    /// The plan the engine actually went on to execute, kept for the rest of
    /// the turn once its gate has closed.
    ///
    /// # Why this is retained rather than dropped
    ///
    /// [`Self::pending_scope_review`] is the *gate*, and it correctly clears
    /// the moment the engine moves past it. But clearing it used to destroy the
    /// only copy of the plan: the scrollback record
    /// ([`TranscriptEntry::ScopeReview`]) keeps a summary and two counts, never
    /// `ScopeProposal::steps`. So a user who approved five steps could not, one
    /// minute later, recall what the third one was — the thing they had just
    /// consented to was the one thing the session could no longer show them.
    ///
    /// Set only on the *approval* path (a non-`ScopeReview` stage, meaning the
    /// gate was answered and work proceeded). A turn that died at the gate
    /// leaves this `None`, because an abandoned proposal was never a plan.
    /// Cleared when a new turn opens, alongside the plan.
    pub approved_scope: Option<ScopeProposal>,
    /// An `ask_user` question awaiting the user's answer. Set by an `AskUser`
    /// event; cleared purely by events — the answer returns as the tool call's
    /// ordinary `ToolResult` (matched by `id`), so a `ToolResult` with the
    /// question's `call_id` clears it (also cleared on `Complete`/`Error`).
    pub pending_ask_user: Option<AskUserPrompt>,
    /// A per-hunk approval gate awaiting the user's decision (#1265). Set by a
    /// `HunkReview` event; cleared purely by events — the host echoes a
    /// `ToolResult` carrying the proposal's `id` once the decision is in, the
    /// same event-pure clear `pending_ask_user` uses (also cleared on
    /// `Complete`/`Error`).
    ///
    /// Only the *proposal* lives here. Which hunks the reviewer has marked is
    /// view state, not session state, and belongs to `DeckUi` beside the
    /// composer — a mark is not something the session did, and folding it in
    /// here would make replay reconstruct a half-finished opinion as fact.
    pub pending_hunk_review: Option<HunkProposal>,
    /// The latest task-board snapshot (the `task_*` tools). Each
    /// `TaskUpdate` event replaces the whole board — snapshot semantics keep
    /// the fold pure and make a dead session's board reconstruct on replay.
    pub tasks: Vec<TaskItem>,
    /// The in-progress answer preview, accumulated from `TextDelta` events
    /// while a model call streams and rendered as a live trailing entry.
    /// Best-effort by protocol contract: REPLACED (never merged) when the
    /// step's authoritative `Text` event lands — which also folds retries
    /// away, since a retried attempt re-streams its deltas from the start —
    /// and dropped on `Error`/`Complete`/a new prompt. Middle-out capped at
    /// `OUTPUT_BUDGET` so an unbounded stream can't grow per-frame render
    /// cost; the authoritative `Text` entry is never capped by this.
    pub streaming_text: String,
    /// The parked wait currently open (#1471, #2007), or `None` when the turn
    /// is not parked. Set by `TurnParked`, cleared by `TurnWoken` and by the
    /// turn ending — a park that is cancelled or soft-stopped out of never
    /// gets its wake, so `Complete`/`Error` have to close the span too.
    ///
    /// The *what* of a live park; deliberately not the *when*. A park lasts up
    /// to its deadline with the engine emitting nothing at all, so the deck's
    /// countdown needs a clock — and reading one here would break the property
    /// this whole fold rests on (`replay(&log) == replay(&log)`, L-T1). The
    /// timestamp is stamped outside, from the deck's injected clock
    /// (`deck::AgentEntry::parked_since_ms`), exactly as `turn_started_ms` is.
    pub parked: Option<OpenPark>,
    /// Where the live progress counter [`Self::set_progress_line`] last wrote
    /// begins: `(transcript index, byte offset into that entry's text)`.
    ///
    /// Positional rather than a bare flag so it invalidates itself: the counter
    /// is rewritable only while it is still the very end of the transcript, and
    /// every entry pushed after it makes the recorded index stale. That is what
    /// makes the last tick of a pass permanent — nothing has to remember to
    /// clear this, so no new transcript-pushing branch can forget to.
    progress: Option<(usize, usize)>,
}

impl SessionModel {
    /// A fresh, empty model — the seq-0 state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Rewind the **conversation** to seq-0 while keeping the record of what
    /// this session did to the tree — what `/clear` means
    /// (`Inbound::SessionReset`; `command_deck::session_clear`).
    ///
    /// Everything conversational resets: transcript, HUD, plan, pending
    /// gates, streaming preview. The file-touch half
    /// ([`Self::files`], [`Self::files_evicted`], `file_touch_seq`) survives,
    /// because those bytes are still on the user's disk after a clear and
    /// `/clear` changes no identity — the session, its store row, its sidecar
    /// dir and its worker lanes all continue.
    ///
    /// # Why this is not `*self = Self::new()`
    ///
    /// It was, and that made the Files tab lie. The tab's ROWS come from
    /// `deck::WorkspaceModel::ledger`, which `/clear` deliberately leaves
    /// alone; its diff TEXT is looked up here (L-T5 — there is no second data
    /// path for diffs). Wholesale replacement cut one of those two and not the
    /// other, so every row survived with its counts intact and every diff pane
    /// went to `(no diff captured)`: an accurate `+64 -6` beside a claim that
    /// nothing was captured.
    ///
    /// Carrying `file_touch_seq` across is required, not tidiness. It
    /// stamps [`FileState::touched_seq`], the recency key
    /// [`MAX_TRACKED_FILES`] eviction orders by; restarting it at 0 under
    /// retained files would rank every surviving path above every new one and
    /// evict newest-first.
    ///
    /// `diff_budget` crosses for the same reason and is the sharper case: it
    /// is the accounting of the text `files` still holds, so resetting it
    /// under a retained ledger would leave the session believing it holds
    /// nothing while holding everything — a bound that reads as satisfied
    /// because it forgot what it was bounding.
    ///
    /// Written as a destructure-and-restore so the default for a field added
    /// later is to RESET — new conversation state is the common case, and it
    /// then needs no edit here; a new *file-ledger* field is what has to be
    /// named.
    pub fn reset_conversation(&mut self) {
        let Self {
            files,
            files_evicted,
            file_touch_seq,
            diff_budget,
            per_call_producer_seen,
            ..
        } = std::mem::take(self);
        self.files = files;
        self.files_evicted = files_evicted;
        self.file_touch_seq = file_touch_seq;
        self.diff_budget = diff_budget;
        self.per_call_producer_seen = per_call_producer_seen;
    }

    /// Fold one event into the model. This is the **only** mutator; every
    /// panel's state is a pure function of the sequence of `apply` calls, so
    /// replaying the same log yields an identical model (L-T1).
    pub fn apply(&mut self, event: &AgentEvent) {
        match event {
            // An event from a newer stella. The fold stays a pure function of
            // events it understands: guessing at state from an undecodable
            // payload would make the model disagree with the transcript that
            // rendered it. The transcript still shows the event (see
            // `textline::unknown_event`), so nothing is hidden — the model
            // just declines to invent state for it.
            AgentEvent::Unknown { .. } => {}
            AgentEvent::Stage { name, scope } => {
                // A stage after a Complete means a new turn has started —
                // clear the completion flag so the progress bar and HUD read
                // fresh (otherwise the bar stays frozen at full-green and
                // `final_cost_usd` is stale). Within a single turn, complete
                // is never set until the very end, so this is a no-op there.
                if self.hud.complete {
                    // The approved plan belongs to the turn that ran it. A new
                    // turn starting under the previous turn's scope would be
                    // the deck asserting consent that was never given for this
                    // work.
                    self.approved_scope = None;
                    self.plan = crate::plan::Plan::default();
                }
                self.hud.complete = false;
                self.hud.final_cost_usd = None;
                self.hud.stage = Some(name.clone());
                // Only a host boundary moves the progress bar — see
                // `Hud::host_stage` for why a contributed stage must leave it
                // where it stands rather than resetting it.
                if let Some(kind) = name.kind() {
                    self.hud.host_stage = Some(kind);
                }
                // Any stage that isn't the scope-review gate itself means the
                // engine has moved past a pending gate (approved → execute,
                // or a later plan/verify stage) — retire it. Kept event-driven
                // so the pending state reconstructs on replay.
                //
                // Retire, not discard: this transition IS the approval signal,
                // so the proposal graduates to `approved_scope` and stays
                // readable for the rest of the turn (`⌃S`).
                //
                // Only a WRAPPER's stage counts (#3398). The engine emits its
                // own turn phases, and one of those arriving while a gate is
                // open would graduate the proposal into `approved_scope` —
                // the deck asserting the human's consent on the strength of an
                // event the human never saw. A turn-scoped stage is never an
                // approval signal, because nobody was asked.
                if *scope == StageScope::Run
                    && name.kind() != Some(StageKind::ScopeReview)
                    && let Some(approved) = self.pending_scope_review.take()
                {
                    self.approved_scope = Some(approved);
                    // The same transition is the plan's approval: the rail
                    // stops saying "pending approval" the moment work starts,
                    // and does so from the event, so replay reconstructs it.
                    self.plan.approve();
                }
                // The first boundary of a turn carries SPEC 6.1's rule; the
                // rest of the turn's stages are plain section rules.
                let opens = if self.turn_head_stamped {
                    None
                } else {
                    self.turn_head_stamped = true;
                    // Where the entry below is about to land, so the turn's
                    // first answering call can name the model this rule opened
                    // without — see `turn_head_idx`.
                    self.turn_head_idx = Some(self.transcript.len());
                    Some(TurnOpening {
                        turn: self.turns_completed.saturating_add(1),
                        // Deliberately not `self.hud.model`: that field is
                        // written only by `TurnComplete`/`RunComplete`, so at
                        // this instant it holds either nothing (turn 1) or the
                        // *previous* turn's model. The turn's own first worker
                        // call back-fills it — see `TurnOpening::model`.
                        model: None,
                        budget_usd: self.hud.limit_usd,
                        // Same shape, same reason: a steer is consumed mid-turn,
                        // after this rule is drawn. The turn's own first
                        // `SteerCause::User` steer back-fills it — see
                        // `TurnOpening::queued_steer`.
                        queued_steer: None,
                    })
                };
                self.transcript.push(TranscriptEntry::Stage {
                    name: name.clone(),
                    opens,
                });
            }
            AgentEvent::Text { text } => {
                // The authoritative step text replaces any streamed preview
                // outright — merging would duplicate (or, after a retry,
                // garble) what the deltas already showed.
                self.streaming_text.clear();
                self.push_text(text)
            }
            AgentEvent::TextDelta { delta } => self.push_streaming_delta(delta),
            AgentEvent::Reasoning { delta } => self.push_reasoning(delta),
            AgentEvent::ToolStart { call, .. } => {
                let path = tool_input_path(&call.input);
                // Mark where the claim window stands *before* the call runs, so
                // its result can tell a change of its own from one already
                // recorded — see `claim_baselines`. Every call, whatever its
                // tool: which calls are measured is decided from the schema by
                // `ToolRegistry::measures_alone`, and a name list here could
                // only disagree with it (#4213).
                self.claim_baselines
                    .insert(call.call_id.clone(), self.claims.open());
                self.transcript.push(TranscriptEntry::ToolStart {
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    input: format_tool_input(&call.input),
                    raw: cap_input_json(&call.input, INPUT_BUDGET),
                    path,
                });
            }
            AgentEvent::ToolResult {
                call_id,
                output,
                duration_ms,
                speculated, .. } => {
                // ANSI escapes are stripped here, at fold time, and nowhere
                // else: the fold cache would retain them if the renderer
                // stripped per frame, and ratatui renders everything after
                // the ESC byte literally (#934).
                // The tool's own line-coverage report, off the structured
                // `data` half of the wire (#4297). Read here, before the
                // prose is capped: `full` is bounded at OUTPUT_BUDGET, so a
                // consumer recounting the rendered body would measure the
                // cap, not the read — the substitution #2290 established as
                // the defect for mutation counts.
                let read_size = match output {
                    ToolOutput::Ok {
                        data: Some(data), ..
                    } => entry::ReadSize::from_data(data),
                    _ => None,
                };
                let (ok, summary, full) = match output {
                    ToolOutput::Ok { content , .. } => {
                        let content = strip_ansi(content);
                        (
                            true,
                            summarize(&content),
                            cap_middle(&content, OUTPUT_BUDGET),
                        )
                    }
                    ToolOutput::Error { message, .. } => {
                        let message = strip_ansi(message);
                        (
                            false,
                            summarize(&message),
                            cap_middle(&message, OUTPUT_BUDGET),
                        )
                    }
                };
                // Resolve the tool's name and target path from its start entry
                // (results only carry the call id on the wire). One lookup for
                // both: they answer to the same entry, and two reverse scans
                // could only differ by finding different `ToolStart`s for one
                // call id, which would be worse than either answer.
                let (name, path) = self
                    .transcript
                    .iter()
                    .rev()
                    .find_map(|e| match e {
                        TranscriptEntry::ToolStart {
                            call_id: cid,
                            name,
                            path,
                            ..
                        } if cid == call_id => Some((name.clone(), path.clone())),
                        _ => None,
                    })
                    .unwrap_or_else(|| ("tool".to_string(), None));
                // Only a *successful* mutation gets an inline-diff reference —
                // a failed call produced no `FileChange`, and rendering the
                // path's previous diff under its ✗ would attribute a change
                // the call never made.
                //
                // The seq names the change **this call** produced, and there are
                // now two producers publishing it at two different moments, so
                // the stamp asks which one is answering rather than assuming
                // (#4175 against #4155/#4176):
                //
                // - The **registry** measures the work tree the moment a solo
                //   mutating call returns and publishes on the channel the
                //   engine then sends this `ToolResult` on, so the `FileChange`
                //   arrives *first*: `touch_file` has already bumped `changes`
                //   and `remember_diff` recorded at the value read here
                //   (`stella_tools::call_measure`).
                // - The **turn boundary** reports whatever the per-call
                //   readings did not claim — a concurrent `delegate`'s writes,
                //   a tool that mutated while advertising `read_only`, a human
                //   editing in another window — and that emit lands *after*
                //   every `ToolResult` of the turn has folded, so the change is
                //   still to come and will be recorded one past `changes`.
                //
                // The **claim window** is what distinguishes them, and it asks
                // *where* a measurement folded rather than what the tool is
                // called ([`mod@inline_diff`]). Anything recorded above this
                // call's baseline landed between its start and its result,
                // which only the per-call producer does; nothing there means
                // this call moved the tree not at all. Guessing either way is a
                // live defect and both have been shipped — reading `changes`
                // under a boundary-only producer left `diff_at` short by
                // exactly one and **every** mutating row rendered diffless
                // (#4155); adding `+1` under a per-call producer points one
                // change into the future instead.
                //
                // A row states what it can attribute and predicts nothing
                // (#4227). Stamping `recorded + 1` under a live per-call
                // producer names a change that has not happened and that
                // nothing binds to this call — a successful `write_file` of
                // the bytes already on disk moves the tree not at all
                // (`stella_tools::write` has no identical-content
                // short-circuit), and the next writer's change then rendered
                // under its row. So the prediction survives only while
                // `per_call_producer_seen` is still false, which is exactly
                // the session where the boundary is the *only* producer and
                // the change it owes really is this call's — and *there* the
                // deck has nothing but the tool's name to go on, which is the
                // one job `is_file_mutation` still holds.
                let baseline = self.claim_baselines.remove(call_id);
                let mut diff: Vec<InlineDiffRef> = Vec::new();
                if ok {
                    if let Some(since) = baseline {
                        diff = self.claims.claim(since, path.as_deref());
                    }
                    if !diff.is_empty() {
                        self.per_call_producer_seen = true;
                    } else if !self.per_call_producer_seen
                        && let Some(path) = self.mutated_path_for(call_id)
                    {
                        let recorded = self
                            .files
                            .iter()
                            .find(|f| f.path == path)
                            .map_or(0, |f| f.changes);
                        // One reference, because the name list this arm falls
                        // back on names one path and the boundary producer
                        // behind it emits one aggregate change per path. A
                        // multi-path row is what the *claim window* answers
                        // (#4214); predicting a second path here would be
                        // predicting a change nothing measured.
                        diff.push(InlineDiffRef {
                            path,
                            seq: recorded + 1,
                        });
                    }
                }
                // Two calls can still land on one seq — several mutations to a
                // path in a turn whose measurement only the boundary takes, so
                // they all point at the single aggregate change it will emit.
                // Exactly one row may claim it: the last, whose post-state the
                // diff actually describes. Earlier rows give up their reference
                // and degrade to naming their change, the same degradation
                // `MAX_TRACKED_FILES` eviction already has. Per-call measurement makes this the rarer path rather
                // than the only one — two measured calls hold two distinct
                // seqs and neither supersedes the other.
                self.supersede_inline_diffs(&diff);
                self.transcript.push(TranscriptEntry::ToolResult {
                    call_id: call_id.clone(),
                    name,
                    path,
                    ok,
                    summary,
                    full,
                    duration_ms: *duration_ms,
                    speculated: *speculated,
                    diff,
                    read_size,
                });
                // The answer to an `ask_user` question comes back as this very
                // tool result (correlated by id) — there is no separate answer
                // event — so a matching result clears the pending question.
                if self
                    .pending_ask_user
                    .as_ref()
                    .is_some_and(|p| p.id == *call_id)
                {
                    self.pending_ask_user = None;
                }
                // A hunk-review card clears the same event-pure way: the host
                // echoes a result carrying the proposal's id once the decision
                // has been taken.
                if self
                    .pending_hunk_review
                    .as_ref()
                    .is_some_and(|p| p.id == *call_id)
                {
                    self.pending_hunk_review = None;
                }
            }
            AgentEvent::Retry { attempt, reason } => {
                self.transcript.push(TranscriptEntry::Retry {
                    attempt: *attempt,
                    reason: reason.clone(),
                });
            }
            AgentEvent::Steered { text, cause } => {
                // A steered message IS a user message — it entered the
                // conversation mid-turn; the prefix is what tells the
                // reader (and a replay) it landed at a step boundary.
                self.transcript
                    .push(TranscriptEntry::User(format!("(steered mid-turn) {text}")));
                // …and only a person's steer is the payoff SPEC 6.1's rule
                // promises. The engine's loop and stall nudges keep their row
                // and get no label (#4185).
                if cause.is_from_a_person() {
                    self.name_the_open_turns_steer(text);
                }
            }
            AgentEvent::TurnParked {
                description,
                poll_interval_secs,
                deadline_secs,
            } => {
                self.transcript.push(TranscriptEntry::Parked {
                    description: description.clone(),
                    poll_interval_secs: *poll_interval_secs,
                    deadline_secs: *deadline_secs,
                });
                // …and the live state the row cannot carry: the transcript is
                // a log, and a countdown is not a thing that happened (#2007).
                self.parked = Some(OpenPark {
                    description: description.clone(),
                    poll_interval_secs: *poll_interval_secs,
                    deadline_secs: *deadline_secs,
                });
            }
            AgentEvent::TurnWoken { reason, polls_used } => {
                self.transcript.push(TranscriptEntry::Woken {
                    reason: reason.clone(),
                    polls_used: *polls_used,
                });
                self.parked = None;
            }
            AgentEvent::Compaction {
                before_tokens,
                after_tokens,
                evicted,
                deduped,
                ..
            } => {
                self.transcript.push(TranscriptEntry::Compaction {
                    before_tokens: *before_tokens,
                    after_tokens: *after_tokens,
                    evicted: *evicted,
                    deduped: *deduped,
                });
            }
            AgentEvent::BudgetTick {
                spent_usd,
                limit_usd,
                mode,
                deadline_remaining_ms,
                ..
            } => {
                // Gauge only — deliberately *not* pushed to the transcript.
                //
                // A tick fires after every model call that spends money, which
                // for one ordinary turn means four or five of them, each
                // printing a number that differs from the last by a fraction of
                // a cent and none of which is the turn's cost (see
                // `Hud::spent_usd` — this gauge is cumulative). The transcript
                // is the record of what *happened*; a budget gauge moving is
                // not an event, it is a reading. It belongs where a reading
                // belongs — live next to the composer, updating in place — and
                // the transcript gets the one line that is actually news: the
                // settled cost, once, at `Complete`.
                //
                // The `TranscriptEntry::BudgetTick` variant is kept: a log
                // replayed from an older stella may still carry the rows, and
                // dropping the variant would silently reshape that history.
                self.hud.spent_usd = *spent_usd;
                self.hud.limit_usd = *limit_usd;
                self.hud.budget_mode = Some(*mode);
                // Assigned, never merged with what was there: an unarmed run
                // must be able to go back to reporting nothing, and `or`-ing
                // the old value would latch a stale clock onto it forever.
                self.hud.deadline_remaining_ms = *deadline_remaining_ms;
            }
            AgentEvent::ProviderFallback { from, to, reason } => {
                self.transcript.push(TranscriptEntry::ProviderFallback {
                    from: from.clone(),
                    to: to.clone(),
                    reason: reason.clone(),
                });
            }
            // `added`/`removed` are the ledger's business (`deck::FileLedger`);
            // this read-model only keeps the diff text for the panel.
            AgentEvent::FileChange {
                path,
                kind,
                added,
                removed,
                diff,
            } => {
                self.turn_counters.touch(path);
                self.touch_file(path, *kind, *added, *removed, diff);
            }
            // Every field is carried through. The old fold projected the
            // frames down to their labels here, which is where the deck's
            // recall row lost the ability to be anything but a paragraph —
            // and it is also how `latency_ms` and `used_ann_index`, both added
            // to the wire precisely because recall sits on the first-token
            // path (#875), never reached a surface at all.
            AgentEvent::ContextRecall {
                frames,
                provider_mix,
                tokens,
                usage,
                latency_ms,
                used_ann_index,
            } => {
                let (frames, providers, budget) =
                    recall::project(frames, provider_mix, usage.as_ref());
                self.transcript.push(TranscriptEntry::ContextRecall {
                    frames,
                    tokens: *tokens,
                    latency_ms: *latency_ms,
                    used_ann_index: *used_ann_index,
                    providers,
                    budget,
                });
            }
            AgentEvent::ContextWrite {
                provider,
                upserts,
                superseded,
            } => {
                self.turn_counters.memories =
                    self.turn_counters.memories.saturating_add(*upserts);
                self.transcript.push(TranscriptEntry::ContextWrite {
                    provider: provider.clone(),
                    upserts: *upserts,
                    superseded: *superseded,
                });
            }
            AgentEvent::MediaProgress {
                artifact_id,
                kind,
                state,
            } => {
                self.transcript.push(TranscriptEntry::MediaProgress {
                    artifact_id: artifact_id.clone(),
                    kind: *kind,
                    state: state.clone(),
                });
            }
            AgentEvent::MediaComplete { artifact } => {
                self.transcript.push(TranscriptEntry::MediaComplete {
                    label: artifact.label.clone(),
                    path: artifact.path.clone(),
                    kind: artifact.kind,
                });
            }
            AgentEvent::Verdict { passed, evidence } => {
                self.transcript.push(TranscriptEntry::Verdict {
                    passed: *passed,
                    summary: evidence.summary.clone(),
                    deterministic: evidence.deterministic,
                });
            }
            AgentEvent::ScopeReview { proposal } => {
                self.transcript.push(TranscriptEntry::ScopeReview {
                    summary: proposal.summary.clone(),
                    steps: proposal.steps.len(),
                    estimated_files: proposal.estimated_files,
                });
                self.pending_scope_review = Some(proposal.clone());
                // The gate's proposal IS the plan — the rail shows its steps
                // from this moment, marked pending approval, so what the user
                // is being asked to consent to is legible before they answer.
                self.plan.propose(proposal);
            }
            AgentEvent::HunkReview { proposal } => {
                let mut files: Vec<&str> =
                    proposal.hunks.iter().map(|h| h.path.as_str()).collect();
                files.sort_unstable();
                files.dedup();
                self.transcript.push(TranscriptEntry::HunkReview {
                    tool: proposal.tool.clone(),
                    hunks: proposal.hunks.len(),
                    files: files.len(),
                });
                self.pending_hunk_review = Some(proposal.clone());
            }
            AgentEvent::AskUser {
                id,
                question,
                options,
            } => {
                self.transcript.push(TranscriptEntry::AskUser {
                    question: question.clone(),
                    options: options.len(),
                });
                self.pending_ask_user = Some(AskUserPrompt {
                    id: id.clone(),
                    question: question.clone(),
                    options: options.clone(),
                });
            }
            AgentEvent::Commit { sha, message } => {
                self.transcript.push(TranscriptEntry::Commit {
                    sha: sha.clone(),
                    message: message.clone(),
                });
            }
            AgentEvent::Pr {
                url,
                status,
                number,
                ci,
            } => {
                self.transcript.push(TranscriptEntry::Pr {
                    url: url.clone(),
                    status: *status,
                    number: *number,
                    ci: *ci,
                });
            }
            AgentEvent::TaskUpdate { tasks } => {
                // The board is snapshot state (rendered as a pinned
                // checklist card); the transcript gets a one-line digest so
                // scrollback shows *when* the board moved.
                self.tasks = tasks.clone();
                self.plan.apply_board(tasks);
                self.transcript.push(TranscriptEntry::TaskUpdate {
                    done: tasks
                        .iter()
                        .filter(|t| t.status == TaskStatus::Completed)
                        .count(),
                    total: tasks.len(),
                    active: tasks
                        .iter()
                        .find(|t| t.status == TaskStatus::InProgress)
                        .map(|t| t.subject.clone()),
                });
            }
            AgentEvent::GoalVerdict {
                met, round, reasoning, ..
            } => {
                // Symmetric to `Verdict` above — a scrollback row. The
                // event's own `cost_usd` is already accounted against the
                // budget when it fires, so it is dropped here (folding it would
                // double-count the HUD spend, which `BudgetTick` drives).
                self.transcript.push(TranscriptEntry::GoalVerdict {
                    met: *met,
                    round: *round,
                    reasoning: reasoning.clone(),
                });
            }
            AgentEvent::SubAgent { phase } => {
                // Both phases are scrollback rows: a child can run for
                // minutes with only its forwarded tool calls visible, so
                // without the start row a reader cannot tell whose calls
                // those are. Spend is deliberately not folded into the HUD
                // here — the engine re-ticks the parent's own post-settlement
                // numbers, and folding this too would double-count.
                let entry = match phase {
                    SubAgentPhase::Started {
                        agent_id,
                        instruction_preview,
                        write_access,
                        ..
                    } => TranscriptEntry::SubAgent {
                        agent_id: agent_id.clone(),
                        finished: None,
                        instruction_preview: instruction_preview.clone(),
                        write_access: *write_access,
                    },
                    SubAgentPhase::Finished {
                        agent_id,
                        status,
                        cost_usd,
                        steps,
                        absorbed_messages,
                        reason,
                        ..
                    } => TranscriptEntry::SubAgent {
                        agent_id: agent_id.clone(),
                        finished: Some(SubAgentSummary {
                            status: *status,
                            cost_usd: *cost_usd,
                            steps: *steps,
                            absorbed_messages: *absorbed_messages,
                            reason: reason.clone(),
                        }),
                        instruction_preview: String::new(),
                        write_access: false,
                    },
                };
                self.transcript.push(entry);
            }
            // Two facts come off a metering record, and they have different
            // scopes — which is why this is one arm and not two.
            //
            // The **tokens** are folded for every call, whatever its role: the
            // receipt accounts for the whole turn, auxiliary work included
            // (#4184). The **model name** is folded only for a call that
            // answers the turn (#4183). Splitting them into a guarded arm and a
            // fallthrough would silently stop counting an answering call's
            // tokens, because the first matching arm wins.
            //
            // The cost deliberately is not folded either way. The HUD's live
            // spend comes from `BudgetTick`, so folding `StepUsage::cost_usd`
            // here would double-count it — that hazard is why this event was
            // ignored outright, and the token half was collateral.
            AgentEvent::StepUsage {
                input_tokens,
                output_tokens,
                ..
            } => {
                self.turn_counters.add_tokens(*input_tokens, *output_tokens);
            }
            AgentEvent::UsageIncomplete { .. }
            // Context receipts (spec §4/§5) are consumed by the store/inspector,
            // not folded into TUI panel state — the model stays a pure function
            // of the user-visible event sequence.
            | AgentEvent::BlockRegistered { .. }
            // `CandidateDelivery` too: its files arrive as `FileChange`.
            | AgentEvent::CandidateDelivery { .. }
            // `Proof` folded the PROOF rail, whose only emitter was the staged
            // pipeline; the rail went ahead of that crate's extraction
            // (#3511), and the crate itself was deleted in #3865, so nothing
            // emits `Proof` today. The step survives in the traces tab and the transcript
            // export, which read the raw stream.
            | AgentEvent::Proof { .. } => {}
            // The one field of a manifest the deck folds: **which model is
            // answering this turn**. Its token, cost and block fields are
            // pointedly still ignored, for the reason the group above states —
            // the live spend is `BudgetTick`'s and folding a second source
            // would double-count it.
            //
            // The manifest is emitted immediately *before* its call commits,
            // which is what makes it early enough to label a turn that has only
            // just started — the whole point, since `Hud::model`'s existing
            // writers are both terminal (#4183).
            AgentEvent::StepManifest {
                role,
                model,
                call_seq,
                ..
            } => {
                if turn::supplies_the_turns_model(*role, *call_seq) {
                    self.hud.model = Some(model.clone());
                    self.name_the_open_turns_model(model);
                }
            }
            AgentEvent::Error { message, retryable } => {
                // A terminal error ends the turn without a `Complete`, so the
                // plan has to close here too: a step left `working` on a turn
                // that died reads as in-flight forever. A retryable error is a
                // warning mid-flight; the turn goes on.
                if !*retryable {
                    self.plan.finish();
                    // The turn died without a `TurnComplete`, so nothing else
                    // will clear the latch and the next turn would open with
                    // no rule at all.
                    self.turn_head_stamped = false;
                    self.turn_head_idx = None;
                }
                self.pending_scope_review = None;
                self.pending_ask_user = None;
                self.pending_hunk_review = None;
                // A park that the turn was cancelled or soft-stopped out of
                // never gets its `TurnWoken`, so the span has to close here or
                // the ⏳ chip counts up forever on a dead turn (#2007).
                if !*retryable {
                    self.parked = None;
                }
                // An aborted model call never commits its text — without
                // this the un-committed preview would linger indefinitely.
                self.streaming_text.clear();
                // One failure, one row — see `error_rows` for why the same
                // error can arrive twice.
                if !error_rows::repeats_the_last_row(&self.transcript, message, *retryable) {
                    self.transcript.push(TranscriptEntry::Error {
                        message: message.clone(),
                        retryable: *retryable,
                    });
                }
            }
            // ONE turn ended (#3379). A wrapped run has several, so this must
            // not settle anything terminal: the cost and model are this turn's
            // and are worth showing, but dropping a pending prompt here would
            // discard a live approval gate the run is still waiting on.
            // `RunComplete` below is the terminal one.
            AgentEvent::TurnComplete { model, cost_usd } => {
                self.hud.model = Some(model.clone());
                self.streaming_text.clear();
                self.turns_completed += 1;
                // The next stage boundary opens a new turn, and its rule. This
                // turn's rule is settled: a call arriving after the turn closed
                // (a reflection pass, a stray retry) patches nothing.
                self.turn_head_stamped = false;
                self.turn_head_idx = None;
                self.transcript.push(TranscriptEntry::Complete {
                    model: model.clone(),
                    cost_usd: *cost_usd,
                    turn: self.turns_completed,
                    receipt: self.turn_counters.settle(),
                });
                self.turn_counters = TurnCounters::default();
            }
            // The RUN ended — the only event that means nothing more is
            // coming, and so the only one that may settle terminal state.
            AgentEvent::RunComplete { model, cost_usd } => {
                self.hud.stage = Some(StageKind::Complete.into());
                self.hud.host_stage = Some(StageKind::Complete);
                self.hud.model = Some(model.clone());
                self.hud.final_cost_usd = Some(*cost_usd);
                self.hud.complete = true;
                // A run that ended between turns leaves no `TurnComplete` to
                // clear the latch, and the next run's first stage must still
                // open a rule (#4124).
                self.turn_head_stamped = false;
                self.turn_head_idx = None;
                self.plan.finish();
                self.pending_scope_review = None;
                self.pending_ask_user = None;
                self.pending_hunk_review = None;
                // The run is over; a span still open here was one it never
                // woke from (#2007).
                self.parked = None;
                self.streaming_text.clear();
            }
            // Internal accounting for read-only speculation that never
            // committed — no visible model state to update.
            AgentEvent::SpeculationDiscarded { .. } => {}
            // Typed decision twins (receipts spec §6.3/§6.4) — the prose
            // events they mirror already updated the visible state.
            AgentEvent::LoopDetected { .. }
            | AgentEvent::BudgetDenied { .. }
            | AgentEvent::RetriesExhausted { .. }
            | AgentEvent::PolicyDecision { .. } => {}
            // A session-level fact established before the turn opened, not a
            // step of it — but the transcript is the only place the deck can
            // say it, since stderr is swallowed under the alternate screen
            // (#4463).
            AgentEvent::SteeringWithheld {
                withheld_by,
                memories,
                records,
                skills,
                commands,
                agents,
            } => self.transcript.push(TranscriptEntry::SteeringWithheld {
                withheld_by: *withheld_by,
                memories: *memories,
                records: *records,
                skills: *skills,
                commands: *commands,
                agents: *agents,
            }),
        }
        self.evict_transcript_overflow();
    }

    /// Fold an entire log at once — the replay entry point.
    pub fn replay(events: &[AgentEvent]) -> Self {
        let mut model = Self::new();
        for event in events {
            model.apply(event);
        }
        model
    }

    /// Append a streaming text delta, coalescing into the trailing `Text`
    /// entry when the last thing emitted was also assistant text.
    /// Write one live progress counter into the transcript **in place**: the
    /// previous counter this wrote is replaced, not appended to.
    ///
    /// # Why the fold owns this rather than the emitter
    ///
    /// A long pass (`/init`'s code-graph walk and its two embedding passes)
    /// narrates once a second so it cannot be mistaken for a wedge. Sent as
    /// ordinary `Text`, a two-hundred-tick pass leaves two hundred near-identical
    /// lines in the scrollback and buries the ✓ summaries that are the actual
    /// record of what init did. Only the fold can rewrite what it already wrote,
    /// so the replacement lives here and the emitter just says the number.
    ///
    /// Still a pure fold: the result is a function of the sequence of calls, and
    /// the last tick of a pass survives as an ordinary line the moment anything
    /// else is pushed — so the counter reads as history afterwards, not as a
    /// spinner that erased itself.
    pub fn set_progress_line(&mut self, line: &str) {
        // Rewritable only while the counter is still the tail of the last
        // entry; anything appended since makes it ordinary scrollback.
        let last = self.transcript.len().wrapping_sub(1);
        let open_at = match (self.progress, self.transcript.last()) {
            (Some((idx, at)), Some(TranscriptEntry::Text(buf)))
                if idx == last && at <= buf.len() =>
            {
                Some(at)
            }
            _ => None,
        };
        match self.transcript.last_mut() {
            Some(TranscriptEntry::Text(buf)) => {
                let at = open_at.unwrap_or(buf.len());
                buf.truncate(at);
                buf.push_str(line);
                // The counter terminates its own line: emitters send lines
                // without one, and the next milestone appends straight onto
                // this buffer — so without it the ✓ summary lands ON the final
                // count rather than under it. Inside the rewritten region, so a
                // later tick replaces it along with the count.
                buf.push('\n');
                self.progress = Some((last, at));
            }
            _ => {
                self.transcript
                    .push(TranscriptEntry::Text(format!("{line}\n")));
                self.progress = Some((self.transcript.len() - 1, 0));
            }
        }
    }

    fn push_text(&mut self, delta: &str) {
        self.progress = None;
        if let Some(TranscriptEntry::Text(buf)) = self.transcript.last_mut() {
            buf.push_str(delta);
        } else {
            self.transcript
                .push(TranscriptEntry::Text(delta.to_string()));
        }
    }

    /// Append one best-effort answer fragment to the streaming preview,
    /// re-capping middle-out at [`OUTPUT_BUDGET`] once it overflows — the
    /// cap keeps the per-frame re-fold of the live tail bounded no matter
    /// how long the model streams. Still a pure fold: the capped buffer is
    /// a deterministic function of the delta sequence.
    fn push_streaming_delta(&mut self, text: &str) {
        self.streaming_text.push_str(text);
        if self.streaming_text.len() > OUTPUT_BUDGET {
            self.streaming_text = cap_middle(&self.streaming_text, OUTPUT_BUDGET);
        }
    }

    /// Append a streaming reasoning delta, coalescing like [`Self::push_text`].
    fn push_reasoning(&mut self, delta: &str) {
        if let Some(TranscriptEntry::Reasoning(buf)) = self.transcript.last_mut() {
            buf.push_str(delta);
        } else {
            self.transcript
                .push(TranscriptEntry::Reasoning(delta.to_string()));
        }
    }

    /// Push a user-submitted prompt into the transcript. This is **not** an
    /// `AgentEvent` fold — the deck driver calls this when `PromptStarted`
    /// arrives so user messages appear inline in the conversational scrollback.
    /// It is also the earliest signal that a new turn has begun, so it clears
    /// any completion state from the prior turn — the progress bar and HUD
    /// reset immediately on prompt submission rather than waiting for the
    /// first `Stage` event of the new turn.
    pub fn push_user_prompt(&mut self, text: &str) {
        self.hud.complete = false;
        self.hud.final_cost_usd = None;
        // Rebase the live turn-cost readout. `spent_usd` is cumulative for the
        // session, so without this the composer's cost cell would open every
        // turn already showing the session total.
        self.hud.turn_start_spent_usd = self.hud.spent_usd;
        // The same rebase for the receipt's counters: a turn that died without
        // a `TurnComplete` never settled them, and its tokens and files must
        // not be billed to the next turn's receipt.
        self.turn_counters = TurnCounters::default();
        // A preview surviving into the next turn could only be stale — the
        // prior turn either committed (cleared on `Text`) or aborted.
        self.streaming_text.clear();
        // Also drop the prior turn's stage, or the progress bar would resume
        // frozen at that stale position (e.g. verify → 83%) instead of restarting
        // at the new turn's beginning. A model turn's first `Stage` event resets
        // this anyway; a driver command (which emits no stages) relies on it.
        self.hud.stage = None;
        // Both stage fields, or the bar would restart at the *previous* turn's
        // phase: `host_stage` is exactly the field that survives a contributed
        // stage on purpose, so it is also the one that has to be cleared
        // deliberately when the turn it described is over.
        self.hud.host_stage = None;
        self.transcript
            .push(TranscriptEntry::User(text.to_string()));
        self.evict_transcript_overflow();
    }

    /// Total transcript entries evicted by the retention cap so far.
    /// Monotonic — a pass absorbs any prior marker and adds at least one —
    /// so it serves as the invalidation generation for caches keyed on the
    /// retained window's front (see the deck's `SessionFold`).
    pub fn evicted_entries(&self) -> usize {
        match self.transcript.first() {
            Some(TranscriptEntry::Evicted { count }) => *count,
            _ => 0,
        }
    }

    /// Enforce [`MAX_TRANSCRIPT_ENTRIES`]: at the cap, drop the oldest
    /// [`TRANSCRIPT_EVICTION_CHUNK`] entries and stand a single
    /// [`TranscriptEntry::Evicted`] marker in their place, absorbing a prior
    /// marker's count so the tally stays total, not per-pass. Runs inside
    /// every transcript-growing mutator — the retained window is part of the
    /// deterministic fold, never a render-time concern. Only the front is
    /// drained, so streaming coalescing into the tail entry is unaffected.
    fn evict_transcript_overflow(&mut self) {
        if self.transcript.len() < MAX_TRANSCRIPT_ENTRIES {
            return;
        }
        let evicted: usize = self
            .transcript
            .drain(..TRANSCRIPT_EVICTION_CHUNK)
            .map(|entry| match entry {
                TranscriptEntry::Evicted { count } => count,
                _ => 1,
            })
            .sum();
        self.transcript
            .insert(0, TranscriptEntry::Evicted { count: evicted });
        // The one index into `transcript` the model holds has to move with it,
        // or a long turn that outlives its own opening rule would back-patch
        // whichever unrelated entry inherited the slot. The drain removes
        // `TRANSCRIPT_EVICTION_CHUNK` entries and the marker puts one back;
        // a rule inside the drained range is gone, and nothing to patch is the
        // right answer, not a nearby row.
        self.turn_head_idx = self
            .turn_head_idx
            .and_then(|idx| idx.checked_sub(TRANSCRIPT_EVICTION_CHUNK))
            .map(|idx| idx + 1);
    }

    /// If tool call `call_id` was a *conventionally named* file mutation, the
    /// path it touched — recovered by correlating back to its `ToolStart`
    /// (which is already on the transcript by the time the result folds).
    ///
    /// This answers for the **turn-boundary producer only**: that change folds
    /// after every result of the turn, so no claim window can hold it and the
    /// tool's name is the only evidence the deck has that the call mutated
    /// anything. Under a live per-call producer the claim window has already
    /// answered and this is never reached, which is what lets a `bash` or MCP
    /// call — measured, but on no name list — render its own change (#4213).
    ///
    /// The diff itself is *not* looked up here — the renderer reads it from
    /// [`SessionModel::files`] at draw time (L-T5).
    fn mutated_path_for(&self, call_id: &str) -> Option<String> {
        self.transcript
            .iter()
            .rev()
            .find_map(|entry| match entry {
                TranscriptEntry::ToolStart {
                    call_id: cid,
                    name,
                    path,
                    ..
                } if cid == call_id => Some((name.clone(), path.clone())),
                _ => None,
            })
            .and_then(|(name, path)| is_file_mutation(&name).then_some(path).flatten())
    }

    /// Name `model` on the opening rule of the turn now in flight, if that rule
    /// has not been given one yet.
    ///
    /// Walks back to the **nearest** stage boundary that opened a turn and stops
    /// there, whether or not it fills anything. That single stop is what makes
    /// this first-write-wins per turn, and it is doing two jobs:
    ///
    /// * an earlier turn's rule is already settled and must not be rewritten by
    ///   a later turn's call;
    /// * a sub-agent's calls also carry
    ///   [`stella_protocol::ModelCallRole::Worker`], and a child may well run on
    ///   a different model. The lead has to call the model before it can decide
    ///   to delegate, so the lead's own first worker call always precedes any
    ///   child's and has already claimed the rule by the time one arrives. Depth
    ///   is not on the wire here, so the ordering is the guarantee rather than a
    ///   filter.
    ///
    /// A turn whose rule was never stamped (`turn_head_stamped` false — no stage
    /// boundary yet) simply finds nothing and leaves `hud.model` to carry it.
    fn name_the_open_turns_model(&mut self, model: &str) {
        if let Some(TranscriptEntry::Stage {
            opens: Some(opening),
            ..
        }) = self
            .transcript
            .iter_mut()
            .rev()
            .find(|e| matches!(e, TranscriptEntry::Stage { opens: Some(_), .. }))
            && opening.model.is_none()
        {
            opening.model = Some(model.to_owned());
        }
    }

    /// Name the steer a person made on the opening rule of the turn now in
    /// flight, if that rule has not been given one yet.
    ///
    /// The same walk as [`Self::name_the_open_turns_model`], and for the same
    /// reason: the nearest stage boundary that opened a turn, stop there
    /// whether or not it fills anything. That single stop is what makes this
    /// first-write-wins per turn — an earlier turn's rule is settled and a
    /// second steer in this turn does not rewrite what the turn opened by
    /// consuming.
    ///
    /// A turn whose rule was never stamped simply finds nothing, and the steer
    /// keeps the `(steered mid-turn)` transcript row it always had.
    fn name_the_open_turns_steer(&mut self, text: &str) {
        if let Some(TranscriptEntry::Stage {
            opens: Some(opening),
            ..
        }) = self
            .transcript
            .iter_mut()
            .rev()
            .find(|e| matches!(e, TranscriptEntry::Stage { opens: Some(_), .. }))
            && opening.queued_steer.is_none()
        {
            opening.queued_steer = Some(text.to_owned());
        }
    }

    /// Drop from every earlier result the inline-diff references that point at
    /// the same changes as `fresh`, so one change is claimed by one row.
    ///
    /// Reachable whenever a path's mutations in a turn are measured only by the
    /// turn boundary, which emits **one** aggregate `FileChange` per path: every
    /// such call stamps an identical `(path, seq)`, and left alone they would
    /// each render the turn's whole change to that file as their own. The last
    /// call keeps it because the measured post-state is the one its edit left
    /// behind; the earlier rows degrade to naming their change rather than
    /// showing it. Per-call measurement (#4175) gives each call its own seq and
    /// so never reaches here.
    ///
    /// Called before the new result is pushed, so it never clears its own refs.
    /// A no-op for the overwhelmingly common empty `fresh` (a read, a failure).
    fn supersede_inline_diffs(&mut self, fresh: &[InlineDiffRef]) {
        if fresh.is_empty() {
            return;
        }
        for entry in &mut self.transcript {
            if let TranscriptEntry::ToolResult { diff, .. } = entry {
                diff.retain(|d| !fresh.contains(d));
            }
        }
    }

    /// Record a file touch, retaining the latest diff for the path (L-T5).
    /// A read on an already-tracked path only grows its read count — the
    /// mutation kind, diff, and `changes` (the inline-diff freshness tag)
    /// stay exactly as the last mutation left them.
    ///
    /// A *mutation* also enters the claim window, so whichever call is in
    /// flight can render it ([`mod@inline_diff`]). Both producers pass through
    /// here and neither is distinguishable at this point; which row may claim
    /// the entry is decided from where it landed, on the result.
    fn touch_file(
        &mut self,
        path: &str,
        kind: FileChangeKind,
        added: u32,
        removed: u32,
        diff: &Option<String>,
    ) {
        self.file_touch_seq += 1;
        let touched_seq = self.file_touch_seq;
        let seq = if let Some(existing) = self.files.iter_mut().find(|f| f.path == path) {
            existing.touched_seq = touched_seq;
            if !kind.is_mutation() {
                existing.reads += 1;
                return;
            }
            existing.kind = kind;
            existing.changes += 1;
            existing.added += added;
            existing.removed += removed;
            existing.remember_diff(diff, added, removed);
            existing.changes
        } else {
            if self.files.len() >= MAX_TRACKED_FILES
                && let Some(evicted) = file_state::evict_lru(&mut self.files)
            {
                // The victim's text leaves with it, so the budget must stop
                // counting bytes nothing holds any more — otherwise a session
                // that sweeps a tree spends its whole budget on paths the
                // ledger has already dropped.
                self.diff_budget.forget(&evicted);
                self.files_evicted += 1;
            }
            let mutation = kind.is_mutation();
            let mut state = FileState {
                path: path.to_string(),
                kind,
                added: if mutation { added } else { 0 },
                removed: if mutation { removed } else { 0 },
                recent_diffs: VecDeque::new(),
                changes: mutation as u32,
                reads: !mutation as u32,
                touched_seq,
            };
            if mutation {
                state.remember_diff(diff, added, removed);
            }
            self.files.push(state);
            if !mutation {
                return;
            }
            1
        };
        self.claims.record(path, seq);
        // Take the new text into the byte budget, and release whatever oldest
        // text that pushes out. A released row keeps its measured `+N −M` and
        // loses only the diff (`FileState::release_text`).
        let bytes = diff.as_ref().map_or(0, String::len);
        for (path, seq) in self.diff_budget.record(path, seq, bytes) {
            if let Some(file) = self.files.iter_mut().find(|f| f.path == path) {
                file.release_text(seq);
            }
        }
    }
}

#[cfg(test)]
mod tests;
