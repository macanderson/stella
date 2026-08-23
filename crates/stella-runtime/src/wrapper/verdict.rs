//! `judge` and `again` — the two points a plugin **cannot** implement.
//!
//! `doc:wrapper-socket` §4 and `doc:pipeline-as-plugins` §6 settle this and
//! this module does not re-argue it: plugins declare the verdict rule as data,
//! and the host evaluates it. What that resolution implies for the socket is a
//! shape, and the shape is here —
//!
//! ```text
//! judge(rule: &VerdictRule, evidence: &EvidenceSet)          -> Verdict
//! again(verdict: &Verdict, round: &RoundState, grant: &LoopGrant) -> Continuation
//! ```
//!
//! — two free functions, synchronous, total, I/O-free, over owned data. They
//! are not trait methods and there is no wire message that addresses them
//! (`stella_plugin::WrapperPoint` has two variants), so a plugin cannot
//! implement either one in Rust, in Python, or in anything else. That is the
//! property that keeps "a verification plugin quietly calls a model to decide
//! done" impossible **by construction** rather than by policy: the failure it
//! forecloses is the worker grading its own work, and a passing verdict looks
//! identical either way.
//!
//! Being synchronous is what enforces it. There is no `.await` to hang a model
//! call on, no `Command` to spawn one with, and no `Result` to smuggle an
//! escalation through — [`judge`] is total, so every shape of evidence already
//! has an answer and none of them is "ask someone".
//!
//! # What totality costs, and why it is paid here
//!
//! Total means every arm is decided, including the ones a `Result` would let a
//! caller ignore: a measurement the oracle failed to report, a check that does
//! not parse, a tamper policy with nothing to compare against. Each of those
//! is a [`UndecidedReason`], not a panic and not a `false`. A missing number
//! is never read as a satisfied budget.
//!
//! That discipline is not restated here — it is *the same code*.
//! [`stella_plugin::CheckOutcome`] is the grammar's own per-check evaluator,
//! total by construction, and both this function and
//! `stella_plugin::Oracle::unmet` fold it; they differ only in how they
//! report, which is exactly the difference that has to exist (`unmet` has a
//! `Result` to return an error through and this does not). Until #3515 they
//! were two hand-written walks over `oracle.checks` in two crates, and the one
//! the plugin crate documented as canonical had no production caller at all —
//! so a change to either was invisible to the other, and #3510 was already a
//! defect in how *this* copy composed checks with the flip policy.
//!
//! The staged pipeline's `ladder_decision` (`crates/stella-pipeline`, deleted
//! in #3865) was the same shape, which was the reason porting that pipeline
//! onto this socket would have been a re-home rather than a rewrite: every arm
//! of that ladder was terminal, and so is every arm here.

use std::fmt::Write as _;

use stella_plugin::{
    CheckOutcome, Continuation, Correction, EvidenceSet, FlipObservation, FlipPolicy, LoopGrant,
    Oracle, Outcome, Participation, RoundState, StopReason, TamperFinding, TamperPolicy,
    UndecidedReason, UnmetBecause, UnmetRequirement, Verdict, VerdictRule, VolatileContext,
};

/// Turn evidence into a verdict, by the rule the plugin declared.
///
/// # The order of the three answers
///
/// A determinate failure outranks an abstention: if any requirement is
/// **unmet** by evidence that actually decided it, the verdict is
/// [`Verdict::Unmet`] even when some *other* requirement could not be decided.
/// A failure is actionable and an abstention is not, and reporting
/// [`Verdict::Met`] because one clause went unmeasured is the false claim this
/// whole apparatus exists to prevent. Only when nothing is determinately unmet
/// does an undecidable clause become [`Verdict::Undecided`] — the ladder's
/// "not a pass, and explicitly not a failure".
///
/// A rule with no requirements is [`Verdict::Met`]: a steering-grade wrapper
/// that contributes context and gathers nothing has nothing to hold open, and
/// inventing a requirement for it would be the host deciding done.
///
/// # A check narrows done; it never widens it
///
/// The flip policy and the `[[oracle.checks]]` entries on a requirement are
/// **conjuncts**, never alternatives. Under [`FlipPolicy::Required`] every
/// requirement must clear the flip *and* the tamper exclusion *and* every
/// check that names it — so declaring a check can only ever remove a
/// requirement from the met set, never add one.
///
/// This is not a stylistic preference. Treating a check as a *replacement* for
/// the flip is the whole failure this socket exists to foreclose, reached
/// without a model call: a plugin declares one trivial budget, rewrites the
/// witness, never goes red→green, and is credited done — the worker grading
/// its own work, which `doc:pipeline-as-plugins` §6 names and #3510 witnessed.
/// [`judge`] being synchronous and I/O-free guarantees no model is asked; it
/// is this conjunction that keeps the oracle *binding*.
///
/// [`FlipPolicy::NotApplicable`] contributes no conjunct, because it is the
/// declaration that this oracle's evidence is not a flip at all. It is
/// strictly more demanding rather than an escape: with nothing else to decide
/// a requirement, a manifest whose requirement no check decides is refused at
/// load (`ManifestError::UndecidableRequirement`), and a hand-built rule that
/// skipped validation abstains here.
#[must_use]
pub fn judge(rule: &VerdictRule, evidence: &EvidenceSet) -> Verdict {
    if rule.requirements.is_empty() {
        return Verdict::Met {
            evidence: evidence.provenance,
        };
    }
    let Some(oracle) = &rule.oracle else {
        return Verdict::Undecided {
            reason: UndecidedReason::NoOracle,
        };
    };

    let flip = flip_credit(oracle, evidence);
    let mut unmet: Vec<UnmetRequirement> = Vec::new();
    // The first undecidable clause in requirement order. `requirements` is a
    // `BTreeMap`, so "first" is a fact about the manifest rather than about a
    // hash seed — two runs over the same evidence report the same reason.
    let mut undecided: Option<UndecidedReason> = None;
    let mut abstain = |reason: UndecidedReason| {
        if undecided.is_none() {
            undecided = Some(reason);
        }
    };

    for (name, statement) in &rule.requirements {
        // Every check naming this requirement conjoins with every other one
        // (`stella_plugin::OracleCheck`), and that whole conjunction then
        // conjoins with the flip below — the loop deliberately falls through
        // to it rather than `continue`ing past it (#3510).
        let mut decided_by_check = false;
        for check in oracle.checks.iter().filter(|c| &c.requirement == name) {
            decided_by_check = true;
            // The grammar's own per-check semantics, evaluated once, in the
            // crate that owns the grammar (#3515). What is left here is the
            // half that is genuinely this function's: reporting an outcome as
            // a verdict rather than as a `Result`. `CheckOutcome` is total, so
            // every arm below is decided and none of them is "ask someone".
            match check.outcome(&evidence.measurements) {
                CheckOutcome::Held => {}
                CheckOutcome::Failed { reported, .. } => unmet.push(UnmetRequirement {
                    requirement: name.clone(),
                    statement: statement.clone(),
                    because: UnmetBecause::Budget {
                        check: check.rule.clone(),
                        reported,
                    },
                    // `judge` decides from `EvidenceSet` alone; the plugin's
                    // advisory note is attached afterwards by
                    // `Verdict::with_detail` (#3840).
                    detail: None,
                }),
                CheckOutcome::MeasurementMissing { measurement } => {
                    abstain(UndecidedReason::MeasurementMissing {
                        requirement: name.clone(),
                        measurement,
                    });
                }
                // Rejected at load (`ManifestError::UnparsableCheck`), so this
                // is reachable only for a rule that did not come from a
                // validated manifest — which must still not read as "the
                // budget held".
                CheckOutcome::Unreadable { reason } => {
                    abstain(UndecidedReason::UnreadableCheck {
                        requirement: name.clone(),
                        reason,
                    });
                }
            }
        }

        match &flip {
            FlipCredit::Credited => {}
            FlipCredit::Denied(because) => unmet.push(UnmetRequirement {
                requirement: name.clone(),
                statement: statement.clone(),
                because: because.clone(),
                detail: None,
            }),
            FlipCredit::Undecided(reason) => abstain(reason.clone()),
            // `flip = "not-applicable"` contributes no conjunct at all — it is
            // the declaration that this oracle's evidence is not a flip, so a
            // check is the only thing that can decide. A requirement no check
            // decides is then `ManifestError::UndecidableRequirement` at load;
            // a hand-built rule reaches it here and abstains rather than
            // crediting.
            FlipCredit::NoFlipPolicy if !decided_by_check => {
                abstain(UndecidedReason::Undecidable {
                    requirement: name.clone(),
                });
            }
            FlipCredit::NoFlipPolicy => {}
        }
    }

    if !unmet.is_empty() {
        return Verdict::Unmet { unmet };
    }
    match undecided {
        Some(reason) => Verdict::Undecided { reason },
        // Provenance is carried out of the evidence, not consulted on the way
        // in: which arm we reach was decided above, on the flip, the tamper
        // finding and the measurements alone (#3513).
        None => Verdict::Met {
            evidence: evidence.provenance,
        },
    }
}

/// What the declared flip policy says about the evidence.
///
/// It is a fact about the *run*, not about one requirement, so it is computed
/// once and conjoined with every requirement's checks — a check cannot excuse
/// a requirement from it (#3510).
enum FlipCredit {
    /// The flip was observed and the artifacts are the ones that were
    /// authored.
    Credited,
    /// The evidence determinately refuses the flip.
    Denied(UnmetBecause),
    /// Nothing available settles it.
    Undecided(UndecidedReason),
    /// This oracle has no flip to decide anything with.
    NoFlipPolicy,
}

/// Classify the flip half of the evidence.
///
/// The flip is read **first** and the tamper finding only qualifies a flip
/// that would otherwise be credited. That ordering is deliberate: a witness
/// that could not run leaves nothing to tamper with, and reporting "no tamper
/// snapshot" for a run whose witness never executed names the wrong problem.
fn flip_credit(oracle: &Oracle, evidence: &EvidenceSet) -> FlipCredit {
    match oracle.flip {
        FlipPolicy::NotApplicable => FlipCredit::NoFlipPolicy,
        FlipPolicy::Required => match evidence.flip {
            FlipObservation::Achieved => tamper_credit(oracle.tamper, &evidence.tamper),
            FlipObservation::NotAchieved => FlipCredit::Denied(UnmetBecause::NoFlip {
                observed: FlipObservation::NotAchieved,
            }),
            FlipObservation::Unsatisfiable => {
                FlipCredit::Undecided(UndecidedReason::WitnessUnsatisfiable)
            }
            FlipObservation::NotAttempted | FlipObservation::Unobservable => {
                FlipCredit::Undecided(UndecidedReason::FlipUnobservable)
            }
        },
    }
}

/// Whether an observed flip may be credited under the declared tamper policy.
///
/// `NotChecked` is deliberately **not** a pass: the policy asks the host to
/// snapshot artifact identity at authoring time, and a flip whose artifacts
/// were never compared is one whose "before" half is unverified. Crediting it
/// would let a worker that rewrote the witness win the flip — the exact
/// exclusion `TamperPolicy::ArtifactIdentity` exists to make.
fn tamper_credit(policy: TamperPolicy, finding: &TamperFinding) -> FlipCredit {
    match policy {
        TamperPolicy::ArtifactIdentity => match finding {
            TamperFinding::Clean => FlipCredit::Credited,
            TamperFinding::Tampered { artifact } => FlipCredit::Denied(UnmetBecause::Tampered {
                artifact: artifact.clone(),
            }),
            TamperFinding::NotChecked => FlipCredit::Undecided(UndecidedReason::TamperUnchecked),
        },
    }
}

/// Decide whether the loop asks for another turn, and on whose authority.
///
/// Three rules, all of them the host's:
///
/// 1. **Only an `arbiter` may hold a completion open.** The grade is the
///    grant; a `steering` wrapper that returns unmet requirements gets them
///    *reported*, not re-run.
/// 2. **The allowance is the plugin's ask clamped to the host's ceiling.**
///    `LoopGrant::max_holds` is a request (`doc:wrapper-socket` §5); a
///    manifest cannot buy an unbounded loop, and a manifest that asks for
///    nothing inherits the ceiling rather than zero.
/// 3. **A spent allowance completes with the unmet requirements reported** —
///    never silently dropped, which is the failure mode #2661 paid for four
///    times on one certification panel.
///
/// An abstention is terminal. There is no correction to author from evidence
/// that decided nothing, and re-running a turn against a broken instrument
/// spends a model call to learn the same non-answer.
#[must_use]
pub fn again(verdict: &Verdict, round: &RoundState, grant: &LoopGrant) -> Continuation {
    let unmet = match verdict {
        Verdict::Met { evidence } => {
            return Continuation::Stop {
                outcome: Outcome::Met {
                    evidence: *evidence,
                },
            };
        }
        Verdict::Undecided { reason } => {
            return Continuation::Stop {
                outcome: Outcome::Undecided {
                    reason: reason.clone(),
                },
            };
        }
        Verdict::Unmet { unmet } => unmet.clone(),
    };

    if !grant.participation.includes(Participation::Arbiter) {
        return Continuation::Stop {
            outcome: Outcome::Unmet {
                unmet,
                stopped: StopReason::NotAnArbiter,
            },
        };
    }

    let allowed = grant
        .max_holds
        .unwrap_or(round.host_max_holds)
        .min(round.host_max_holds);
    if round.holds_spent >= allowed {
        return Continuation::Stop {
            outcome: Outcome::Unmet {
                unmet,
                stopped: StopReason::AllowanceSpent {
                    spent: round.holds_spent,
                    allowed,
                },
            },
        };
    }

    Continuation::Again {
        correction: Correction {
            guidance: VolatileContext::new("wrapper-verdict", correction_text(&unmet)),
            unmet,
        },
    }
}

/// The correction a held-open turn is told, rendered deterministically from
/// the unmet clauses.
///
/// Byte-stable for the same input (invariant 7's discipline): the clauses
/// arrive in `[requirements]` order because [`judge`] walks a `BTreeMap`, so
/// two runs over the same evidence produce the same bytes.
///
/// The wrapper's own words follow the clauses when it had any
/// ([`UnmetRequirement::detail`], #3840). Distinct notes only, in first-seen
/// order: `Verdict::with_detail` puts one observation on every clause, and
/// repeating it once per requirement would be the same sentence three times.
fn correction_text(unmet: &[UnmetRequirement]) -> String {
    let mut text = String::from(
        "The turn completed, but these declared requirements are not met yet. \
         Address them and stop:\n",
    );
    for clause in unmet {
        let _ = writeln!(text, "- {clause}");
    }

    let mut notes: Vec<&str> = Vec::new();
    for detail in unmet.iter().filter_map(|clause| clause.detail.as_deref()) {
        if !notes.contains(&detail) {
            notes.push(detail);
        }
    }
    if !notes.is_empty() {
        text.push_str("\nWhat checked it said:\n");
        for note in notes {
            let _ = writeln!(text, "{note}");
        }
    }
    text
}
