//! The law at the completion gate when more than one thing gets a say.
//!
//! `judge` reads one plugin's evidence. `again` decides whether that one
//! plugin holds the turn open. Both assume a single arbiter. A workspace
//! witness plugin beside an org policy plugin is the obvious pair. When it
//! arrives, "what does the host do when they disagree?" must already have a
//! written answer. This module is that answer.
//!
//! # Pure, total, free of I/O
//!
//! `fold_stamps` takes owned claims and a budget. It hands back a decision.
//! No clock, no process, no model. `judge` and `again` carry the same rule
//! one layer down, for the same reason. A gate that can ask someone is a
//! gate that can be talked round.
//!
//! # The five rules
//!
//! 1. **A red test is never talked round.** No count of `done` claims moves
//!    a rung that rests on one. `refutes_done` still answers `true`.
//! 2. **One live `not_done` holds the turn.** A veto never waits for a
//!    second. A check that needs a quorum to report a bug is not a check.
//! 3. **A `done` claim never lifts a rung.** `Arbitration::rung` is the rung
//!    the fold was handed. That is `VerdictStamp`'s rule, kept here too.
//! 4. **`inconclusive` and `not_applicable` are kept, and count for
//!    nothing.** Each gets a row, so a gap in the checking is plain to see.
//!    Neither holds the turn. Neither carries an unmet clause.
//! 5. **Each arbiter draws holds from one turn total.** Its own ask is
//!    clamped by the host ceiling. The turn total goes up by **one** per
//!    held round, however many said no. So two arbiters that take turns
//!    saying no spend what one would spend.
//!
//! # Fail open, and say so
//!
//! An arbiter that dies, runs out of time, or answers junk never blocks.
//! Failing shut is wrong here in a way it is not at a tool gate. There a
//! denial means nothing runs. Here it means the model burns more steps on
//! feedback it cannot act on. "The plugin crashed" is not a fix list.
//!
//! Failing open in silence is wrong too. That is the half this module adds.
//! Without it, a run whose arbiter died reads like a run whose arbiter was
//! happy. `did_not_answer` turns the failure into an `inconclusive` claim.
//! It carries the wrapper's own error text. The gap then sits beside the
//! verdict, where a reader will find it.

use stella_plugin::{LoopGrant, Participation, UnmetRequirement, Verdict};
use stella_protocol::{LadderRung, StampAssessment, VerdictStamp};

use super::error::WrapperError;

/// One claim at the completion gate, and what the host needs to price it.
///
/// A claim holds what a [`VerdictStamp`] holds. It adds two facts the wire
/// record leaves out, because they belong to the host. The first is whether
/// this grade may hold a turn open. The second is what it has spent doing
/// so. A plugin can write neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArbiterClaim {
    /// Who claimed it. The host fills this in from the manifest it loaded.
    /// So no plugin can speak in another's name.
    pub author: String,
    /// The author's own version string, copied word for word. A reader may
    /// show it. No code reads it.
    pub author_version: Option<String>,
    /// What this check concluded.
    pub assessment: StampAssessment,
    /// One line: what was checked, and what it showed. Words for a person.
    /// Nothing parses it.
    pub summary: String,
    /// What this claim says is still unmet.
    ///
    /// Read only for [`StampAssessment::NotDone`]. On every other value the
    /// fold drops it. That is rule 4, and the fold applies it rather than
    /// trust the caller. A check that could not tell has found no bug, and
    /// its clauses would read as one.
    pub unmet: Vec<UnmetRequirement>,
    /// Whether this grade may hold a turn open.
    ///
    /// `arbiter` grade, and nothing less (`doc:pipeline-as-plugins` §4). A
    /// `steering` wrapper that reports unmet clauses gets them printed. It
    /// does not get another turn.
    pub may_hold: bool,
    /// How many holds this claim asks for. `None` takes the host ceiling. It
    /// does not mean zero.
    pub max_holds: Option<u32>,
    /// Holds this claim has spent in this turn.
    pub holds_spent: u32,
    /// The check ran out of time. Its answer is then what it had when the
    /// clock stopped.
    pub timed_out: bool,
    /// Whether this check answered at all.
    ///
    /// `false` only for [`ArbiterClaim::did_not_answer`]. This is not the
    /// same question as the answer itself. A check that returns
    /// [`StampAssessment::Inconclusive`] looked and could not tell. A check
    /// that never answered did not look. Both stand aside. A line that said
    /// "did not answer" for the first would report a crash that never
    /// happened.
    pub answered: bool,
    /// How long this check took, in milliseconds.
    pub duration_ms: u64,
}

impl ArbiterClaim {
    /// The claim a check that did not answer leaves behind.
    ///
    /// [`StampAssessment::Inconclusive`], never
    /// [`StampAssessment::NotDone`]. The check broke. Reading that as a
    /// finding would blame a worker for a crash. It holds nothing, whatever
    /// the grade. There is no fix list to write from a non-answer, and
    /// another turn would buy the same non-answer.
    ///
    /// `timed_out` is set for [`WrapperError::Timeout`] and nothing else. A
    /// check that stood aside and one the clock cut short are two facts. The
    /// stamp keeps them apart.
    ///
    /// The grant comes in so the row can say what grade fell silent. A
    /// surface prints "arbiter X did not answer" only for a check that had a
    /// say to lose.
    #[must_use]
    pub fn did_not_answer(
        author: impl Into<String>,
        failure: &WrapperError,
        grant: &LoopGrant,
    ) -> Self {
        let author = author.into();
        let summary = format!("arbiter {author} did not answer: {failure}");
        Self {
            author,
            author_version: None,
            assessment: StampAssessment::Inconclusive,
            summary,
            unmet: Vec::new(),
            may_hold: grant.participation.includes(Participation::Arbiter),
            max_holds: grant.max_holds,
            holds_spent: 0,
            timed_out: matches!(failure, WrapperError::Timeout { .. }),
            answered: false,
            duration_ms: 0,
        }
    }

    /// The claim a host makes from the verdict a check's evidence produced.
    ///
    /// Three verdicts, three answers, and no fourth reading. Met is
    /// [`StampAssessment::Done`]. A hard failure is
    /// [`StampAssessment::NotDone`]. Standing aside is
    /// [`StampAssessment::Inconclusive`]. The third is not a finding that
    /// the work fell short. `Verdict::Undecided` exists to keep those two
    /// apart one layer down.
    #[must_use]
    pub fn from_verdict(
        author: impl Into<String>,
        verdict: &Verdict,
        grant: &LoopGrant,
        holds_spent: u32,
    ) -> Self {
        let (assessment, summary, unmet) = match verdict {
            Verdict::Met { .. } => (
                StampAssessment::Done,
                "every declared requirement is met".to_string(),
                Vec::new(),
            ),
            Verdict::Unmet { unmet, .. } => (
                StampAssessment::NotDone,
                format!("{} declared requirement(s) not met", unmet.len()),
                unmet.clone(),
            ),
            Verdict::Undecided { reason, .. } => (
                StampAssessment::Inconclusive,
                format!("nothing decided it either way: {reason}"),
                Vec::new(),
            ),
        };
        Self {
            author: author.into(),
            author_version: None,
            assessment,
            summary,
            unmet,
            may_hold: grant.participation.includes(Participation::Arbiter),
            max_holds: grant.max_holds,
            holds_spent,
            timed_out: false,
            answered: true,
            duration_ms: 0,
        }
    }

    /// This claim as the wire record, tied to the evidence `preimage_hash`
    /// names.
    ///
    /// The hash comes from the caller. It is taken over a `LadderSnapshot`
    /// the host owns. `LadderSnapshot::stamp_preimage` builds the object,
    /// and the record hash of ADR 0004 digests it. This module holds
    /// neither. That is what keeps it free of I/O and of a clock.
    #[must_use]
    pub fn into_stamp(self, preimage_hash: impl Into<String>, decided_at_ms: u64) -> VerdictStamp {
        VerdictStamp {
            author: self.author,
            author_version: self.author_version,
            assessment: self.assessment,
            summary: self.summary,
            preimage_hash: preimage_hash.into(),
            evidence_refs: Vec::new(),
            decided_at_ms,
            duration_ms: self.duration_ms,
            timed_out: self.timed_out,
        }
    }
}

/// What a turn has spent holding itself open, and what the host allows.
///
/// The turn's numbers, not one arbiter's. Rule 5 rations model calls. A
/// model call is bought by a *round*, so this counts rounds. Each arbiter's
/// own allowance narrows that from inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnHoldBudget {
    /// Holds spent in this turn, by every arbiter together.
    pub turn_holds_spent: u32,
    /// The host ceiling for the whole turn, whatever a manifest asked for.
    pub host_max_holds: u32,
}

/// Why a claim that said no is not holding the turn open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldStop {
    /// The turn's shared total is spent, while this claim still had room.
    ///
    /// Only two arbiters can reach this. With one claim its allowance is
    /// already clamped by the turn ceiling, so a claim under its own
    /// allowance is under the turn's too.
    TurnAllowanceSpent {
        /// Holds the turn has spent.
        spent: u32,
        /// The host ceiling for the turn.
        allowed: u32,
    },
    /// This claim's own allowance is spent.
    ///
    /// Asked before the turn's total, because this is what stopped it. Its
    /// clauses read against the turn's numbers would name the wrong ceiling.
    ArbiterAllowanceSpent {
        /// Holds this claim has spent.
        spent: u32,
        /// Its ask, clamped to the host ceiling.
        allowed: u32,
    },
    /// The grade may not hold a turn open. Only an `arbiter` may.
    NotAnArbiter,
}

/// One claim's row in the fold, in the order it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArbiterRow {
    /// Who claimed it.
    pub author: String,
    /// What it concluded.
    pub assessment: StampAssessment,
    /// Its own words.
    pub summary: String,
    /// What it says is unmet. Empty on every answer but
    /// [`StampAssessment::NotDone`]. That is rule 4, and the fold applies
    /// it.
    pub unmet: Vec<UnmetRequirement>,
    /// It ran out of time.
    pub timed_out: bool,
    /// Whether it answered at all — see [`ArbiterClaim::answered`].
    pub answered: bool,
    /// Whether its grade may hold a turn open at all.
    pub may_hold: bool,
    /// Whether it is holding this turn open.
    pub holding: bool,
    /// Why it is not, when it said no and could not hold. `None` for a row
    /// that is holding, and for one that never said no.
    pub stopped: Option<HoldStop>,
    /// Holds this claim had spent when the fold ran.
    pub arbiter_spent: u32,
    /// Its ask, clamped to the host ceiling.
    pub arbiter_allowed: u32,
}

/// What the completion gate made of every claim it was handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arbitration {
    /// The rung the fold was handed, unchanged.
    ///
    /// Rule 3: a claim is a record, not a vote. Nothing here moves it. The
    /// field is here so rule 1 has something to read. A red test is a fact
    /// about the evidence, and the fold must not paper over it.
    pub rung: Option<LadderRung>,
    /// One row per claim, in arrival order. Every claim gets one, even the
    /// ones that count for nothing.
    pub rows: Vec<ArbiterRow>,
    /// Holds the turn has spent once this fold is counted. That includes the
    /// round this fold buys, when it buys one.
    pub turn_spent: u32,
    /// The host ceiling for the turn.
    pub turn_allowed: u32,
}

impl Arbitration {
    /// Whether the turn is held open at all.
    #[must_use]
    pub fn held(&self) -> bool {
        self.rows.iter().any(|row| row.holding)
    }

    /// The rows holding the turn open, in arrival order.
    pub fn holders(&self) -> impl Iterator<Item = &ArbiterRow> {
        self.rows.iter().filter(|row| row.holding)
    }

    /// The rows that stood aside, in arrival order. Some looked and could
    /// not tell. Some never answered.
    pub fn abstentions(&self) -> impl Iterator<Item = &ArbiterRow> {
        self.rows
            .iter()
            .filter(|row| row.assessment == StampAssessment::Inconclusive)
    }

    /// The rows that never answered at all, in arrival order.
    ///
    /// A surface draws these so a run whose arbiter died does not read like
    /// a run whose arbiter was happy. Narrower than [`Self::abstentions`],
    /// which also holds the check that answered and could not tell. That is
    /// a real answer, not a crash.
    pub fn unanswered(&self) -> impl Iterator<Item = &ArbiterRow> {
        self.rows.iter().filter(|row| !row.answered)
    }

    /// Every unmet clause, in arrival order, each with the row that said it.
    ///
    /// Reported whether or not the turn is held. That is rule 5: a spent
    /// allowance ends the turn with the unmet clauses named. Drop them and a
    /// turn ends with the work unfinished and nothing saying so.
    pub fn unmet(&self) -> impl Iterator<Item = (&str, &UnmetRequirement)> {
        self.rows.iter().flat_map(|row| {
            row.unmet
                .iter()
                .map(move |clause| (row.author.as_str(), clause))
        })
    }

    /// Whether anything here says the work is **not** done.
    ///
    /// `true` on a red test, whatever the claims say. That is rule 1. Also
    /// `true` while any claim says no. That is rule 2. A check that stood
    /// aside makes it neither. It found nothing, and silence is not a
    /// finding.
    ///
    /// Never flip this into "the work is done". A fold of nothing but
    /// abstentions answers `false`. All that says is that nobody said no.
    #[must_use]
    pub fn refutes_done(&self) -> bool {
        self.rung.is_some_and(deterministic_failure)
            || self
                .rows
                .iter()
                .any(|row| row.assessment == StampAssessment::NotDone)
    }
}

/// A rung that says the work is wrong, on a real test run.
///
/// Written as a full match, not as "hard evidence and not the pass". So a
/// rung added later is a build error here. It cannot land on one side of
/// rule 1 in silence.
fn deterministic_failure(rung: LadderRung) -> bool {
    match rung {
        LadderRung::Revise => true,
        LadderRung::SubmitFast
        | LadderRung::NothingAttempted
        | LadderRung::Unverifiable
        | LadderRung::Unverified
        | LadderRung::WitnessUnsatisfiable
        | LadderRung::Waived => false,
    }
}

/// Fold every claim into one decision about the completion gate.
///
/// The five rules in this module's header, in one pass. Claims are read in
/// arrival order. Pure and total: every claim has an answer, and no answer
/// is "ask someone".
///
/// `rung` is what the test evidence found, when the host has any. `None` is
/// a host with no ladder over this turn. That is the wrapper socket today.
/// Read it as "no hard evidence to weigh", never as a pass.
#[must_use]
pub fn fold_stamps(
    rung: Option<LadderRung>,
    claims: &[ArbiterClaim],
    budget: TurnHoldBudget,
) -> Arbitration {
    let mut rows = Vec::with_capacity(claims.len());

    for claim in claims {
        // Rule 4, applied here rather than trusted from the claim. Only a
        // hard finding carries clauses. So a claim that stood aside cannot
        // become a finding by arriving with a list attached.
        let objects = claim.assessment == StampAssessment::NotDone;
        let unmet = if objects {
            claim.unmet.clone()
        } else {
            Vec::new()
        };

        // Rule 5's clamp, the one `again` applies to a single arbiter. A
        // manifest cannot buy a loop with no end. One that asks for nothing
        // takes the ceiling, not zero.
        let allowed = claim
            .max_holds
            .unwrap_or(budget.host_max_holds)
            .min(budget.host_max_holds);

        // Its own ceiling is asked first, because that is what stopped it.
        // The turn arm fires only for a claim that still had room of its
        // own, which is the case a single arbiter can never reach: with one
        // claim, `allowed` is already clamped by the turn ceiling, so a claim
        // under its own allowance is under the turn's too. That is what lets
        // this agree with `again` everywhere.
        let stopped = if !objects {
            None
        } else if !claim.may_hold {
            Some(HoldStop::NotAnArbiter)
        } else if claim.holds_spent >= allowed {
            Some(HoldStop::ArbiterAllowanceSpent {
                spent: claim.holds_spent,
                allowed,
            })
        } else if budget.turn_holds_spent >= budget.host_max_holds {
            Some(HoldStop::TurnAllowanceSpent {
                spent: budget.turn_holds_spent,
                allowed: budget.host_max_holds,
            })
        } else {
            None
        };

        rows.push(ArbiterRow {
            author: claim.author.clone(),
            assessment: claim.assessment,
            summary: claim.summary.clone(),
            unmet,
            timed_out: claim.timed_out,
            answered: claim.answered,
            may_hold: claim.may_hold,
            // Rule 2: one live no is enough. This row holds on its own
            // account. It never waits to see whether a second agrees.
            holding: objects && stopped.is_none(),
            stopped,
            arbiter_spent: claim.holds_spent,
            arbiter_allowed: allowed,
        });
    }

    // Rule 5's other half. The turn spends one hold for the round it buys,
    // however many claims asked for it. Two arbiters that both say no in one
    // round buy one turn between them, not two.
    let held = rows.iter().any(|row| row.holding);
    Arbitration {
        rung,
        rows,
        turn_spent: budget.turn_holds_spent.saturating_add(u32::from(held)),
        turn_allowed: budget.host_max_holds,
    }
}
