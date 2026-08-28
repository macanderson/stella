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
//! (`stella_plugin::WrapperPoint` has two cases), so a plugin cannot
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
    UndecidedReason, UndecidedRequirement, UnmetBecause, UnmetRequirement, Verdict, VerdictRule,
    VolatileContext,
};
use stella_protocol::{GateBoard, GateRow, GateState};

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
            // Rule-wide: decided before any requirement is looked at, so
            // there is no per-requirement list to carry and the board paints
            // every row undecided (see `gate_state`).
            undecided: Vec::new(),
        };
    };

    let flip = flip_credit(oracle, evidence);
    let mut unmet: Vec<UnmetRequirement> = Vec::new();
    // Every undecidable clause, in requirement order (#5267). `requirements`
    // is a `BTreeMap`, so that order is a fact about the manifest rather than
    // about a hash seed — two runs over the same evidence report the same
    // list, and the FIRST entry is the single reason a report prints.
    //
    // Collected even when a determinate failure is also found. The verdict is
    // still `Unmet` — a failure outranks an abstention — but the board needs
    // to know that a clause nobody could decide is not a clause that held.
    let mut undecided: Vec<UndecidedRequirement> = Vec::new();

    for (name, statement) in &rule.requirements {
        // Every check naming this requirement conjoins with every other one
        // (`stella_plugin::OracleCheck`), and that whole conjunction then
        // conjoins with the flip below — the loop falls through
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
                    abstain(
                        &mut undecided,
                        name,
                        statement,
                        UndecidedReason::MeasurementMissing {
                            requirement: name.clone(),
                            measurement,
                        },
                    );
                }
                // Rejected at load (`ManifestError::UnparsableCheck`), so this
                // is reachable only for a rule that did not come from a
                // validated manifest — which must still not read as "the
                // budget held".
                CheckOutcome::Unreadable { reason } => {
                    abstain(
                        &mut undecided,
                        name,
                        statement,
                        UndecidedReason::UnreadableCheck {
                            requirement: name.clone(),
                            reason,
                        },
                    );
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
            FlipCredit::Undecided(reason) => {
                abstain(&mut undecided, name, statement, reason.clone());
            }
            // `flip = "not-applicable"` contributes no conjunct at all — it is
            // the declaration that this oracle's evidence is not a flip, so a
            // check is the only thing that can decide. A requirement no check
            // decides is then `ManifestError::UndecidableRequirement` at load;
            // a hand-built rule reaches it here and abstains rather than
            // crediting.
            FlipCredit::NoFlipPolicy if !decided_by_check => {
                abstain(
                    &mut undecided,
                    name,
                    statement,
                    UndecidedReason::Undecidable {
                        requirement: name.clone(),
                    },
                );
            }
            FlipCredit::NoFlipPolicy => {}
        }
    }

    if !unmet.is_empty() {
        return Verdict::Unmet { unmet, undecided };
    }
    match undecided.first().map(|u| u.reason.clone()) {
        Some(reason) => Verdict::Undecided { reason, undecided },
        // Provenance is carried out of the evidence, not consulted on the way
        // in: which arm we reach was decided above, on the flip, the tamper
        // finding and the measurements alone (#3513).
        None => Verdict::Met {
            evidence: evidence.provenance,
        },
    }
}

/// Record one requirement's abstention, at most once per requirement.
///
/// At most once because the clauses of a requirement conjoin: two checks that
/// both fail to decide it are one undecided row, and a board that drew the
/// requirement twice would be counting checks where SPEC 8.1 counts
/// requirements. The FIRST reason is kept for the same reason `judge` reports
/// the first one overall — it is the one a report prints, and keeping the
/// first makes that choice a fact about requirement order rather than about
/// which check ran last.
fn abstain(
    undecided: &mut Vec<UndecidedRequirement>,
    name: &str,
    statement: &str,
    reason: UndecidedReason,
) {
    if undecided.iter().any(|u| u.requirement == name) {
        return;
    }
    undecided.push(UndecidedRequirement {
        requirement: name.to_string(),
        statement: statement.to_string(),
        reason,
    });
}

/// The verdict as a board a reader can look at — SPEC 8.1's `gate board`.
///
/// One row per requirement the rule declares, which is why this takes the rule
/// and not only the verdict: a [`Verdict::Unmet`] lists what failed and says
/// nothing about what held, so a board built from the verdict alone would have
/// as many rows as there were failures and would report `0/0 green` on a
/// perfect run.
///
/// Pure and total, like [`judge`] above and for the same reason: nothing about
/// drawing a board may reach for a model, a process or a clock. It re-decides
/// nothing either — every row is read out of the verdict [`judge`] already
/// reached, so the board and the verdict cannot disagree.
///
/// # Which rows are red
///
/// A requirement named in [`Verdict::Unmet`]'s `unmet` failed; one named in
/// its `undecided` could not be decided; everything else held.
///
/// The middle case is #5267. `judge` still reports a determinate failure in
/// preference to an abstention — that is what the verdict concludes — but it
/// now carries the abstentions beside the failures, so a clause nobody could
/// decide no longer renders green on an `Unmet` verdict. A row that quietly
/// says "green" for a clause nobody could decide is the flattering claim this
/// whole path exists to prevent.
///
/// A requirement in **both** lists is red, and that is the conjunction rather
/// than a tie-break: a determinate failure on one check makes the requirement
/// unmet whatever a second check could not decide.
///
/// An undecided verdict paints the requirements its own list names. When that
/// list is empty it paints **every** row undecided, which is the old behaviour
/// and still the right one for the two cases that produce it:
/// [`UndecidedReason::NoOracle`], decided before any requirement is looked at,
/// and a verdict deserialized from a payload written before #5267.
///
/// `deterministic` is `true` on every row: [`judge`] is synchronous and
/// I/O-free, so no model was asked and the decision cost nothing.
#[must_use]
pub fn gate_board(rule: &VerdictRule, verdict: &Verdict, patch: Option<String>) -> GateBoard {
    let gates = rule
        .requirements
        .iter()
        .map(|(name, statement)| GateRow {
            name: name.clone(),
            state: gate_state(name, statement, verdict),
            deterministic: true,
        })
        .collect();
    GateBoard { patch, gates }
}

/// One requirement's row, read out of the verdict.
fn gate_state(name: &str, statement: &str, verdict: &Verdict) -> GateState {
    match verdict {
        Verdict::Met { .. } => GateState::Green,
        Verdict::Undecided { reason, undecided } => {
            // An empty list is a rule-wide abstention (`NoOracle`) or a
            // pre-#5267 payload; either way the whole verdict says undecided,
            // so every row does.
            if undecided.is_empty() {
                return GateState::Undecided {
                    reason: reason.to_string(),
                };
            }
            match undecided.iter().find(|clause| clause.requirement == name) {
                Some(clause) => GateState::Undecided {
                    reason: clause.reason.to_string(),
                },
                // Nothing failed on this verdict, and this clause was decided.
                None => GateState::Green,
            }
        }
        Verdict::Unmet { unmet, undecided } => {
            match unmet.iter().find(|clause| clause.requirement == name) {
                // `because` is the failing case — the check the manifest wrote, or
                // the flip that was not observed — and `detail` is the wrapper's
                // own account of it, which is the excerpt the block shows. The
                // statement stands in when the plugin reported no detail: it is the
                // clause the reader is being told went red, and an empty block
                // under a red row reads as evidence nobody kept.
                Some(clause) => GateState::Failed {
                    case: clause.because.to_string(),
                    log: clause
                        .detail
                        .clone()
                        .unwrap_or_else(|| statement.to_owned()),
                },
                // Not a failure. Undecided if `judge` could not settle it, and
                // only then green — the half #5267 added.
                None => match undecided.iter().find(|clause| clause.requirement == name) {
                    Some(clause) => GateState::Undecided {
                        reason: clause.reason.to_string(),
                    },
                    None => GateState::Green,
                },
            }
        }
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
        Verdict::Undecided { reason, .. } => {
            return Continuation::Stop {
                outcome: Outcome::Undecided {
                    reason: reason.clone(),
                },
            };
        }
        Verdict::Unmet { unmet, .. } => unmet.clone(),
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
/// Byte-stable for the same input (AGENTS.md #7's discipline): the clauses
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use stella_plugin::{EvidenceProvenance, Oracle, OracleCheck};

    /// A rule with two requirements: `budget` decided by a check, `flip`
    /// decided by nothing (no check names it, and the policy declares the
    /// evidence is not a flip) — so `flip` is undecidable and `budget` is
    /// whatever its measurement says.
    fn two_clause_rule() -> VerdictRule {
        VerdictRule {
            requirements: BTreeMap::from([
                ("budget".to_string(), "p50 stays under 100ms".to_string()),
                (
                    "flip".to_string(),
                    "the witness goes red then green".to_string(),
                ),
            ]),
            oracle: Some(Oracle {
                flip: FlipPolicy::NotApplicable,
                checks: vec![OracleCheck {
                    requirement: "budget".to_string(),
                    rule: "p50 <= 100".to_string(),
                }],
                command: None,
                tamper: TamperPolicy::ArtifactIdentity,
                measurements: vec!["p50".to_string()],
            }),
        }
    }

    fn evidence(measurements: BTreeMap<String, u64>) -> EvidenceSet {
        EvidenceSet {
            provenance: EvidenceProvenance::PluginReported,
            flip: FlipObservation::NotAttempted,
            tamper: TamperFinding::NotChecked,
            measurements,
        }
    }

    fn state_of(board: &GateBoard, name: &str) -> GateState {
        board
            .gates
            .iter()
            .find(|row| row.name == name)
            .unwrap_or_else(|| panic!("no row for {name}: {board:?}"))
            .state
            .clone()
    }

    /// **The witness (#5267).** One failing clause and one undecidable clause
    /// draw one red row and one undecided row.
    ///
    /// Fails on the old code, which reported the failure and dropped the
    /// abstention: `Verdict::Unmet` carried only `unmet`, so `gate_state`
    /// found no entry for `flip` and painted it **green** — a row saying a
    /// gate held when nobody could tell. That is the direction that costs,
    /// and it is the claim this whole path exists to prevent.
    #[test]
    fn a_failed_clause_and_an_undecidable_one_draw_two_different_rows() {
        let rule = two_clause_rule();
        // p50 over budget: `budget` fails determinately. `flip` is decided by
        // nothing at all.
        let verdict = judge(&rule, &evidence(BTreeMap::from([("p50".into(), 250)])));

        assert!(
            matches!(verdict, Verdict::Unmet { .. }),
            "a determinate failure still outranks an abstention: {verdict:?}"
        );

        let board = gate_board(&rule, &verdict, None);
        assert!(
            matches!(state_of(&board, "budget"), GateState::Failed { .. }),
            "the failing clause is red: {board:?}"
        );
        assert!(
            matches!(state_of(&board, "flip"), GateState::Undecided { .. }),
            "the undecidable clause must NOT read as green: {board:?}"
        );
        assert_eq!(board.green(), 0, "nothing held, so nothing is green");
    }

    /// A clause that genuinely held stays green beside an undecided one.
    ///
    /// The other side of the same change: painting every row undecided was
    /// the old behaviour on an undecided verdict, and it understated a clause
    /// the evidence did decide.
    #[test]
    fn an_undecided_verdict_greens_the_clauses_it_did_decide() {
        let rule = two_clause_rule();
        // p50 within budget: `budget` holds. `flip` is still undecidable, so
        // the verdict abstains rather than passing.
        let verdict = judge(&rule, &evidence(BTreeMap::from([("p50".into(), 40)])));
        assert!(
            matches!(verdict, Verdict::Undecided { .. }),
            "one undecidable clause makes the whole verdict undecided: {verdict:?}"
        );

        let board = gate_board(&rule, &verdict, None);
        assert_eq!(state_of(&board, "budget"), GateState::Green);
        assert!(matches!(
            state_of(&board, "flip"),
            GateState::Undecided { .. }
        ));
    }

    /// A rule-wide abstention still paints every row, because it names no
    /// requirement to paint.
    ///
    /// `NoOracle` is decided before any requirement is looked at, so its
    /// per-requirement list is empty — and an empty list must not be read as
    /// "everything held". A payload written before #5267 deserializes the
    /// same way, which is what makes the field additive on the wire.
    #[test]
    fn a_rule_wide_abstention_paints_every_row_undecided() {
        let rule = VerdictRule {
            requirements: BTreeMap::from([
                ("budget".to_string(), "p50 stays under 100ms".to_string()),
                ("flip".to_string(), "the witness flips".to_string()),
            ]),
            oracle: None,
        };
        let verdict = judge(&rule, &evidence(BTreeMap::new()));
        assert!(matches!(
            verdict,
            Verdict::Undecided {
                reason: UndecidedReason::NoOracle,
                ..
            }
        ));

        let board = gate_board(&rule, &verdict, None);
        for row in &board.gates {
            assert!(
                matches!(row.state, GateState::Undecided { .. }),
                "no oracle decides nothing, so no row may read as green: {row:?}"
            );
        }
    }

    /// One requirement, two checks that both fail to decide it, is ONE row.
    ///
    /// The clauses of a requirement conjoin, so a board that drew it twice
    /// would be counting checks where SPEC 8.1 counts requirements.
    #[test]
    fn two_undecidable_checks_on_one_requirement_are_one_row() {
        let rule = VerdictRule {
            requirements: BTreeMap::from([("budget".to_string(), "two numbers hold".to_string())]),
            oracle: Some(Oracle {
                flip: FlipPolicy::NotApplicable,
                checks: vec![
                    OracleCheck {
                        requirement: "budget".to_string(),
                        rule: "p50 <= 100".to_string(),
                    },
                    OracleCheck {
                        requirement: "budget".to_string(),
                        rule: "p99 <= 400".to_string(),
                    },
                ],
                command: None,
                tamper: TamperPolicy::ArtifactIdentity,
                measurements: vec!["p50".to_string(), "p99".to_string()],
            }),
        };
        // Neither measurement reported: both checks abstain on the same
        // requirement.
        let verdict = judge(&rule, &evidence(BTreeMap::new()));
        let Verdict::Undecided { undecided, .. } = &verdict else {
            panic!("expected an undecided verdict: {verdict:?}");
        };
        assert_eq!(
            undecided.len(),
            1,
            "one requirement, one row: {undecided:?}"
        );
        assert_eq!(undecided[0].requirement, "budget");
        assert_eq!(gate_board(&rule, &verdict, None).gates.len(), 1);
    }
}
