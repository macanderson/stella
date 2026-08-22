//! The **turn in flight** and the boundaries that fence it — the half of
//! [`super::SessionModel`] that answers "what is happening now", as opposed
//! to the scrollback that records what happened.
//!
//! Two types, and they are the two ends of the same thing. [`Hud`] is live
//! state, read by the statline while the turn runs and overwritten by the
//! next one. [`TurnOpening`] is the settled stamp that state leaves on the
//! transcript when a turn opens, so the boundary can still say what the turn
//! was long after the HUD has moved on.
//!
//! Split out of `model.rs` when it crossed the 1500-line guard, on the
//! sibling-submodule remedy AGENTS.md names rather than a raised baseline —
//! the same cut `file_state` and `recall` already make, and along the same
//! seam: a `TranscriptEntry`'s supporting types live beside it, not in it.

use stella_protocol::{BudgetMode, ModelCallRole, StageKind, StageName};

/// Whether a call of this role is the one **answering the turn** — the model
/// SPEC 6.1's opening rule names, and the one the statline means by "model".
///
/// `StepUsage` reports every committed model call, and most of them are not
/// the answer: an overflow summarizer, a reflection pass, or a wrapper
/// plugin's verdict call each name a model that has no business labelling the
/// turn. An unfiltered fold would make a long turn's rule read as though the
/// summarizer had done the work.
///
/// One named predicate rather than a `matches!` at the fold site, so the
/// roleless-core churn (#3903) has exactly one place to land: the `match` is
/// exhaustive, so a role added to the vocabulary is an `E0004` here and a
/// maintainer has to answer "does this one answer a turn?" rather than
/// inheriting a wildcard's guess.
///
/// [`ModelCallRole::Unknown`] counts, and deliberately: it is the
/// `serde(default)` for an *absent* role on a stream recorded before call-role
/// attribution existed, never a catch-all for a role this build does not know
/// (an unrecognized token fails the whole event). On such a stream every call
/// is `Unknown`, and the first one is the worker's — so admitting it is what
/// keeps an old recording's opening rule readable, and it cannot admit a
/// future auxiliary role.
pub(super) fn answers_the_turn(role: ModelCallRole) -> bool {
    match role {
        ModelCallRole::Worker | ModelCallRole::Unknown => true,
        ModelCallRole::Triage
        | ModelCallRole::Research
        | ModelCallRole::Plan
        | ModelCallRole::PlanRepair
        | ModelCallRole::WitnessAuthor
        | ModelCallRole::WitnessRepair
        | ModelCallRole::DistressGuidance
        | ModelCallRole::Verdict
        | ModelCallRole::AgentAuthor
        | ModelCallRole::SkillAuthor
        | ModelCallRole::DomainInference
        | ModelCallRole::Reflection
        | ModelCallRole::Summarization => false,
    }
}

/// What SPEC 6.1's opening rule says out loud, stamped onto the stage boundary
/// that opens a turn.
///
/// # Why the facts ride the entry
///
/// The same reason [`super::TranscriptEntry::Complete`]'s `turn` does: `render::entry`
/// renders one entry at a time and holds no session state, so a fact that is not
/// on the entry is a fact the rule cannot say.
///
/// # Why one rule per turn and not one per stage
///
/// SPEC 6.1 draws a single labelled boundary and SPEC 2 makes the turn the
/// transcript's unit. A wrapped run has four or five stages inside one turn, so
/// a rule per stage would announce `turn 14` five times and the number would
/// stop reading as the turn's identity. Which boundary is the opening one is
/// decided at fold time (`SessionModel::turn_head_stamped`) rather than at
/// render time, because the renderer cannot see the entry before this one.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnOpening {
    /// This turn's 1-based ordinal — [`super::SessionModel::turns_completed`] plus the
    /// turn in flight, so it is the number the turn's own closing rule will
    /// carry.
    ///
    /// It counts turns that *completed*. A turn that died without one is
    /// therefore not counted, and the next attempt reopens under the same
    /// ordinal — which is the honest reading of a counter that means "turns
    /// this session has finished", not a renumbering.
    pub turn: u32,
    /// The model answering this turn — the **first** committed call of the
    /// turn whose role `answers_the_turn`, as reported by
    /// `AgentEvent::StepUsage`.
    ///
    /// # Why `StepUsage` and not `StepManifest`
    ///
    /// The manifest arrives earlier — before the call, rather than after it —
    /// and would spare the back-patch below. It names the model the engine
    /// *asked for*, and this repo settles routing mid-turn, so the two can
    /// disagree. A rule that stated what was asked and never what answered
    /// would be asserting a routing decision that did not survive. `StepUsage`
    /// reports a call that **committed**, so this only ever names a model that
    /// actually ran.
    ///
    /// # Why it is back-patched
    ///
    /// The boundary has to appear where the stage arrived, which is before any
    /// call can have reported. So it is stamped with whatever the last turn ran
    /// on — `None` on the session's first turn — and
    /// `super::SessionModel::turn_head_idx` holds the slot until this turn's
    /// own first answering call overwrites it. That provisional label is a
    /// sticky route's best guess for the few hundred milliseconds it stands,
    /// never the rule's final word.
    ///
    /// Only the **first** such call settles it. A later re-route in the same
    /// turn moves [`Hud::model`] and leaves this alone: a boundary the reader
    /// has already scrolled past changing its mind silently is worse than two
    /// rules that differ, and the correction is already visible on the closing
    /// rule, whose `TurnComplete` names the model the turn ended on.
    ///
    /// `None` therefore means **no answering call has reported** — a turn still
    /// waiting on its first commit, one that died before it, or a stream
    /// carrying no `StepUsage` at all. Not substituted with the configured
    /// default even then: that is not evidence of what a router picked
    /// (#4183, #4124).
    pub model: Option<String>,
    /// This turn's spend ceiling, from [`Hud::limit_usd`].
    ///
    /// The **turn's**, not the session's, which is what makes it the right
    /// number for a rule that opens one turn: `AgentEvent::BudgetTick`'s own
    /// doc states that `spent_usd`/`limit_usd` are turn-scoped and that the
    /// session axis rides separately on `session_spent_usd`/
    /// `session_limit_usd`, which the deck does not fold.
    ///
    /// `None` means **no budget is armed**, which is not a budget of `$0.00` —
    /// the same distinction [`Hud::deadline_remaining_ms`] draws, and for the
    /// same reason: a rule printing `budget $0.00` over an uncapped run would
    /// state a cap nobody set.
    pub budget_usd: Option<f64>,
}

/// What SPEC 6.1's receipt says, stamped onto the entry that closes a turn.
///
/// Rides the entry for the same reason [`TurnOpening`] does: `render::entry`
/// renders one entry and holds no session state, so a fact that is not on the
/// entry is a fact the receipt cannot say.
///
/// Every field here was counted. A field with no source is **absent from this
/// struct** rather than present and zero — see [`TurnCounters`] for what is
/// counted and `crate::v2::transcript::Receipt` for what is still missing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnReceipt {
    /// Tokens this turn spent, summed from its `StepUsage` events.
    ///
    /// `None` when no usage event arrived — a turn answered from cache, or one
    /// whose provider reported none. Never `Some(0)` from an absence.
    pub tokens: Option<u64>,
    /// Distinct paths the turn changed, from its `FileChange` events.
    ///
    /// A genuine `0` now — every mutation emits a `FileChange`, so a turn that
    /// changed nothing counted zero rather than failing to count.
    pub files: u32,
    /// Memories written, summed over the turn's `ContextWrite` upserts.
    pub memories: u32,
    /// Wall clock the turn took, in milliseconds.
    ///
    /// `None` inside the fold, always: [`super::SessionModel`] reads no clock
    /// by contract (L-T1, `replay(&log) == replay(&log)`). The deck stamps it
    /// from `deck::AgentEntry::turn_clock_ms` on the way past, which is the
    /// same shape `parked_since_ms` already uses.
    pub elapsed_ms: Option<u64>,
}

/// The per-turn counters [`TurnReceipt`] is stamped from, reset at each turn
/// boundary.
///
/// # What is not here
///
/// **Tests.** SPEC 6.1's `4/4 tests` has no source: nothing in the event
/// stream reports a test tally. `AgentEvent::Verdict` carries a
/// pass/fail plus prose, not counts, and a `bash` call running `cargo test` is
/// opaque to the fold — its output is a `ToolResult` string nobody parses, and
/// parsing one would be a scraper guessing at a harness rather than a
/// measurement. Feeding this field needs an event that states the counts:
/// either a verification plugin reporting its `EvidenceSet` per check, or a
/// test-runner tool that returns structured results instead of text.
///
/// **`det %`.** Removed from the design; see `crate::v2::transcript::Receipt`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnCounters {
    /// Summed from `AgentEvent::StepUsage`'s **token** fields only.
    ///
    /// Never its `cost_usd`: the deck's spend is driven by `BudgetTick`, and
    /// folding usage cost as well would double-count it. That hazard is why
    /// the deck ignored `StepUsage` outright, and the token half was collateral.
    pub tokens: Option<u64>,
    /// Distinct paths this turn's `FileChange` events named, in first-touched
    /// order.
    ///
    /// Distinct, not a call count: the receipt says how much of the tree moved,
    /// not how many edits it took. Bounded by the turn, so unlike
    /// [`super::SessionModel::files`] it does not outlive a `/clear`.
    pub files: Vec<String>,
    /// Summed over `AgentEvent::ContextWrite::upserts`.
    pub memories: u32,
}

impl TurnCounters {
    /// Fold one usage event's tokens in, leaving its cost alone.
    pub fn add_tokens(&mut self, input: u64, output: u64) {
        let sum = input.saturating_add(output);
        self.tokens = Some(self.tokens.unwrap_or(0).saturating_add(sum));
    }

    /// Record a path this turn changed, once however often it is touched.
    pub fn touch(&mut self, path: &str) {
        if !self.files.iter().any(|p| p == path) {
            self.files.push(path.to_string());
        }
    }

    /// The settled receipt, minus the elapsed the fold may not measure.
    #[must_use]
    pub fn settle(&self) -> TurnReceipt {
        TurnReceipt {
            tokens: self.tokens,
            files: u32::try_from(self.files.len()).unwrap_or(u32::MAX),
            memories: self.memories,
            elapsed_ms: None,
        }
    }
}

/// Live HUD numbers, all folded from the event stream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hud {
    /// Spend as the budget guard reports it on every `BudgetTick`.
    ///
    /// Note this is **cumulative for the life of the guard**, not per turn: the
    /// deck builds one `BudgetGuard` for the whole session and never calls
    /// `begin_turn`, so this only ever rises. Use [`Hud::turn_spent_usd`] for
    /// the number a reader would call "what this turn cost".
    pub spent_usd: f64,
    pub limit_usd: Option<f64>,
    pub budget_mode: Option<BudgetMode>,
    /// Wall clock left before the task deadline, as the last `BudgetTick`
    /// reported it (#2240, #2435). `None` is the load-bearing case and means
    /// **no deadline is armed** — never "no time left", which is
    /// `Some(0)`. The status bar renders the two differently for exactly that
    /// reason: a HUD showing `0s` for an unarmed run would put back into the
    /// UI the confusion #2240 took out of the journal. `None` draws no cell at
    /// all and `Some(0)` draws the word `expired`
    /// ([`crate::v2::status_bar`]).
    ///
    /// Milliseconds rather than a `Duration` because that is the wire shape
    /// (`AgentEvent::BudgetTick::deadline_remaining_ms`), and this struct is a
    /// fold of the stream, not a reinterpretation of it.
    pub deadline_remaining_ms: Option<u64>,
    /// The stage the turn is in. [`StageName`], not [`StageKind`]: a
    /// contributed stage is what the statline must be able to name.
    pub stage: Option<StageName>,
    /// The most recent stage that was one of **this host's own** boundaries.
    ///
    /// Separate from [`Hud::stage`] because the two answer different questions,
    /// and one field cannot answer both once the vocabulary is open. `stage` is
    /// "what is happening right now", which is what the statline says out loud.
    /// This is "how far through its own shape the turn has got", which is what
    /// the three-segment progress bar draws — and the bar has only the host's
    /// three phases to draw with.
    ///
    /// Keeping it is what stops a contributed stage reading as a regression: a
    /// plugin stage arriving after `execute` leaves this at `Execute`, so the
    /// bar holds. Folding the contributed stage into the same field would make
    /// it phase-less, and a phase-less stage falls back to phase 0 — the bar
    /// would snap back to "plan" and claim the run had gone backwards.
    pub host_stage: Option<StageKind>,
    /// The model serving the turn, as the statline names it.
    ///
    /// Fed **during** the turn by every committed call that
    /// `answers_the_turn`, so it tracks a mid-turn re-route as it settles,
    /// and corrected at the end by `AgentEvent::TurnComplete`/`RunComplete`,
    /// which are authoritative for the model the turn finished on (#4183).
    /// Only the model name is taken from `StepUsage` — its token and cost
    /// fields belong to `BudgetTick` and the store, and folding them here
    /// would double-count the spend the gauge already shows.
    pub model: Option<String>,
    /// [`Hud::spent_usd`] as it stood when the current turn began, so live turn
    /// cost is the difference. Snapshotted in [`super::SessionModel::push_user_prompt`]
    /// — the earliest signal a turn has started.
    pub turn_start_spent_usd: f64,
    /// The final turn cost, set once a `Complete` event lands.
    pub final_cost_usd: Option<f64>,
    pub complete: bool,
}

impl Hud {
    /// What the turn in flight has cost so far.
    ///
    /// Once the turn settles this yields to `Complete`'s own `cost_usd`, which
    /// is authoritative — the driver totals it directly rather than differencing
    /// a cumulative gauge, so it also catches spend that never produced a tick.
    pub fn turn_spent_usd(&self) -> f64 {
        self.final_cost_usd
            .unwrap_or_else(|| (self.spent_usd - self.turn_start_spent_usd).max(0.0))
    }
}
