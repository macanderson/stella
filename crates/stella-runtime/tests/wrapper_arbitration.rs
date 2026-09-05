//! The law at the completion gate, proved rather than described.
//!
//! `fold_stamps` is written before the second arbiter exists. So nothing
//! here can lean on shipped behaviour to say what the rules are. The tests
//! are the rules.
//!
//! Four run as properties, over every claim shape the words allow. One pins
//! the fold to the single-arbiter rule `again` ships today. Two more drive
//! the real dispatch loop.
//!
//! Every test names a type that `main` does not have. So the whole file
//! fails to build there. The law is absent before the change, and that is
//! the witness.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use proptest::prelude::*;
use stella_plugin::{
    Continuation, HookEvent, LoopGrant, Outcome, Participation, PluginManifest, RoundState,
    SignalValues, StopReason, TamperFinding, TurnOutcome, UnmetBecause, UnmetRequirement, Verdict,
    WrapperPoint,
};
use stella_protocol::{LadderRung, StampAssessment};
use stella_runtime::wrapper::{
    ArbiterClaim, DEFAULT_WRAPPER_TIMEOUT, DrivenTurn, HoldStop, RoundInput, SubprocessWrapper,
    TurnDriver, TurnHoldBudget, TurnPrelude, WrapperDispatch, WrapperError, again, fold_stamps,
};

/// The seven rungs. A property runs over the whole ladder, not just the one
/// rung the rule names.
const RUNGS: [LadderRung; 7] = [
    LadderRung::SubmitFast,
    LadderRung::Revise,
    LadderRung::NothingAttempted,
    LadderRung::Unverifiable,
    LadderRung::Unverified,
    LadderRung::WitnessUnsatisfiable,
    LadderRung::Waived,
];

const ASSESSMENTS: [StampAssessment; 4] = [
    StampAssessment::Done,
    StampAssessment::NotDone,
    StampAssessment::Inconclusive,
    StampAssessment::NotApplicable,
];

fn grant(participation: Participation, max_holds: Option<u32>) -> LoopGrant {
    LoopGrant {
        participation,
        hooks: vec![HookEvent::Stop],
        points: vec![WrapperPoint::AfterTurn],
        max_holds,
        ..LoopGrant::default()
    }
}

fn clause(requirement: &str) -> UnmetRequirement {
    UnmetRequirement {
        requirement: requirement.to_string(),
        statement: format!("{requirement} holds"),
        because: UnmetBecause::Budget {
            check: "p50 <= 105".to_string(),
            reported: 118,
        },
        detail: None,
    }
}

/// One claim, field by field. A property then varies just the one field it
/// is about.
fn claim(
    author: &str,
    assessment: StampAssessment,
    may_hold: bool,
    max_holds: Option<u32>,
    holds_spent: u32,
) -> ArbiterClaim {
    ArbiterClaim {
        author: author.to_string(),
        author_version: None,
        assessment,
        summary: format!("{author} says {}", assessment.as_str()),
        unmet: vec![clause(author)],
        may_hold,
        max_holds,
        holds_spent,
        timed_out: false,
        answered: true,
        duration_ms: 0,
    }
}

fn budget(turn_holds_spent: u32, host_max_holds: u32) -> TurnHoldBudget {
    TurnHoldBudget {
        turn_holds_spent,
        host_max_holds,
    }
}

/// An index into a fixed table. That is how a strategy runs over a closed
/// set of words with no `Arbitrary` to write.
fn assessments(len: usize) -> impl Strategy<Value = Vec<StampAssessment>> {
    proptest::collection::vec(0usize..ASSESSMENTS.len(), 1..=len)
        .prop_map(|picks| picks.into_iter().map(|pick| ASSESSMENTS[pick]).collect())
}

proptest! {
    /// **Rule 1.** A red test is never talked round.
    ///
    /// Run over every rung and every list of claims. So the property is
    /// about the whole ladder, not one rung. On `revise` the fold still says
    /// not done, however many claims say done. On no other rung does a list
    /// of nothing but done say not done.
    #[test]
    fn a_deterministic_failure_survives_every_done_claim(
        picks in proptest::collection::vec(0usize..RUNGS.len(), 1..4),
        holds in proptest::collection::vec(0u32..4, 1..4),
    ) {
        let agreeing: Vec<ArbiterClaim> = holds
            .iter()
            .enumerate()
            .map(|(index, spent)| {
                claim(&format!("a{index}"), StampAssessment::Done, true, None, *spent)
            })
            .collect();

        for pick in picks {
            let rung = RUNGS[pick];
            let folded = fold_stamps(Some(rung), &agreeing, budget(0, 2));
            if rung == LadderRung::Revise {
                prop_assert!(
                    folded.refutes_done(),
                    "a red test outranks every agreeing observer"
                );
            } else {
                prop_assert!(
                    !folded.refutes_done(),
                    "{rung:?} is not a determinate failure, so agreement is not contradicted"
                );
            }
            // Rule 3, on the same inputs: the rung the fold hands back is the
            // rung it was handed.
            prop_assert_eq!(folded.rung, Some(rung));
        }
    }

    /// **Rule 2.** One live `not_done` holds the turn.
    ///
    /// All must agree to pass. A majority is not enough. One no among any
    /// number of yes claims still holds, and the fold never counts heads.
    /// The turn total is large here, so the no is what decides it. Rule 5
    /// covers the budget on its own.
    #[test]
    fn one_objection_holds_the_turn_however_many_agree(
        agreeing in 0usize..5,
        position in 0usize..5,
    ) {
        let mut claims: Vec<ArbiterClaim> = (0..agreeing)
            .map(|index| claim(&format!("agree{index}"), StampAssessment::Done, true, None, 0))
            .collect();
        let at = position.min(claims.len());
        claims.insert(at, claim("objector", StampAssessment::NotDone, true, None, 0));

        let folded = fold_stamps(None, &claims, budget(0, 8));
        prop_assert!(folded.held(), "one live objection is enough");
        prop_assert!(folded.refutes_done());
        let holders: Vec<&str> = folded.holders().map(|row| row.author.as_str()).collect();
        prop_assert_eq!(holders, vec!["objector"], "and it is the objector holding");
        prop_assert_eq!(
            folded.turn_spent,
            1,
            "one round is bought, however many observers were in the room"
        );
    }

    /// **Rule 4.** `inconclusive` and `not_applicable` are kept, and count
    /// for nothing.
    ///
    /// Kept: every claim gets a row, in arrival order, whatever it said.
    /// Counts for nothing: only a `not_done` row holds, and only a
    /// `not_done` row carries clauses. Every claim here arrives with a
    /// clause attached. So dropping it is the fold's work, not the
    /// caller's.
    #[test]
    fn an_abstention_is_recorded_and_never_decides(
        said in assessments(6),
    ) {
        let claims: Vec<ArbiterClaim> = said
            .iter()
            .enumerate()
            .map(|(index, assessment)| {
                claim(&format!("a{index}"), *assessment, true, None, 0)
            })
            .collect();
        let folded = fold_stamps(None, &claims, budget(0, 8));

        prop_assert_eq!(folded.rows.len(), claims.len(), "every claim is on the record");
        for (row, claim) in folded.rows.iter().zip(&claims) {
            prop_assert_eq!(&row.author, &claim.author, "in arrival order");
            prop_assert_eq!(row.assessment, claim.assessment);
            let objects = claim.assessment == StampAssessment::NotDone;
            prop_assert_eq!(
                row.unmet.is_empty(),
                !objects,
                "only a determinate finding carries clauses: {:?}",
                row
            );
            prop_assert!(
                objects || !row.holding,
                "an abstention or an agreement never holds: {row:?}"
            );
        }
        prop_assert_eq!(
            folded.held(),
            said.contains(&StampAssessment::NotDone),
            "the hold follows the objections and nothing else"
        );
    }

    /// **The fold is the shipped rule, widened.**
    ///
    /// `again` decides one arbiter's hold today. This fold decides any
    /// number. So the two must agree wherever both apply. The test runs over
    /// the grade, the ask, the spend and the ceiling. That is the whole
    /// input space the clamp is defined on. A match at one point would say
    /// nothing about the edges, and edges are where a clamp goes wrong.
    #[test]
    fn the_fold_agrees_with_again_on_a_single_arbiter(
        arbiter in proptest::bool::ANY,
        ask in proptest::option::of(0u32..5),
        spent in 0u32..5,
        ceiling in 0u32..5,
    ) {
        let unmet = vec![clause("within-budget")];
        let verdict = Verdict::Unmet {
            unmet: unmet.clone(),
            undecided: Vec::new(),
        };
        let participation = if arbiter {
            Participation::Arbiter
        } else {
            Participation::Steering
        };
        let grant = grant(participation, ask);
        let round = RoundState {
            holds_spent: spent,
            host_max_holds: ceiling,
        };

        let shipped = again(&verdict, &round, &grant);
        let folded = fold_stamps(
            None,
            &[ArbiterClaim::from_verdict("only", &verdict, &grant, spent)],
            budget(spent, ceiling),
        );

        match shipped {
            Continuation::Again { correction } => {
                prop_assert!(folded.held(), "both hold, or neither does");
                prop_assert_eq!(correction.unmet, unmet.clone());
                let held: Vec<UnmetRequirement> =
                    folded.unmet().map(|(_, clause)| clause.clone()).collect();
                prop_assert_eq!(held, unmet.clone(), "and both report the same clauses");
            }
            Continuation::Stop { outcome } => {
                prop_assert!(!folded.held());
                let stopped = match outcome {
                    Outcome::Unmet { unmet: reported, stopped } => {
                        prop_assert_eq!(reported, unmet.clone());
                        stopped
                    }
                    other => return Err(TestCaseError::fail(format!(
                        "an unmet verdict stops as unmet: {other:?}"
                    ))),
                };
                let row = &folded.rows[0];
                let mirrored = match row.stopped {
                    Some(HoldStop::NotAnArbiter) => StopReason::NotAnArbiter,
                    Some(HoldStop::TurnAllowanceSpent { spent, allowed })
                    | Some(HoldStop::ArbiterAllowanceSpent { spent, allowed }) => {
                        StopReason::AllowanceSpent { spent, allowed }
                    }
                    None => return Err(TestCaseError::fail(
                        "a row that is not holding says why".to_string(),
                    )),
                };
                prop_assert_eq!(mirrored, stopped, "and for the same reason");
            }
        }
    }
}

/// **Rule 5.** Two arbiters share one turn total. Each is priced by its own
/// ask.
///
/// A table can state this sum where a property cannot. Two arbiters that say
/// no in turn must not buy twice the model calls one would buy. The turn
/// goes up by one per held round, however many said no. An arbiter out of
/// its own holds is named against *its* ceiling, not the turn's. A single
/// arbiter could never make that row.
#[test]
fn two_arbiters_draw_from_one_turn_total() {
    // Both object, both have room. One round is bought between them.
    let both = fold_stamps(
        None,
        &[
            claim("witness", StampAssessment::NotDone, true, Some(3), 1),
            claim("policy", StampAssessment::NotDone, true, Some(3), 1),
        ],
        budget(1, 4),
    );
    assert!(both.held());
    assert_eq!(
        both.holders().count(),
        2,
        "both are holding: a veto is not a vote and neither yields to the other"
    );
    assert_eq!(
        both.turn_spent, 2,
        "the turn spent one more round, not one per objector"
    );
    assert_eq!(both.turn_allowed, 4);

    // The witness has spent its own ask; the policy has not, and the turn has
    // room for both. Only the policy holds, and the witness is reported
    // against the ceiling that actually stopped it.
    let one_spent = fold_stamps(
        None,
        &[
            claim("witness", StampAssessment::NotDone, true, Some(2), 2),
            claim("policy", StampAssessment::NotDone, true, Some(3), 0),
        ],
        budget(2, 6),
    );
    assert!(one_spent.held());
    let holders: Vec<&str> = one_spent.holders().map(|row| row.author.as_str()).collect();
    assert_eq!(holders, vec!["policy"]);
    assert_eq!(
        one_spent.rows[0].stopped,
        Some(HoldStop::ArbiterAllowanceSpent {
            spent: 2,
            allowed: 2
        }),
        "its own ask is what ran out, and the row names that number"
    );
    // Rule 5's report half: a clause nobody can still act on is still
    // reported, attributed to the arbiter that asked for it.
    let attributed: Vec<(&str, String)> = one_spent
        .unmet()
        .map(|(author, clause)| (author, clause.requirement.clone()))
        .collect();
    assert_eq!(
        attributed,
        vec![
            ("witness", "witness".to_string()),
            ("policy", "policy".to_string())
        ],
        "the spent arbiter's clauses are reported, never dropped"
    );

    // The turn's total is spent. Neither holds, and both are reported against
    // the turn's ceiling rather than their own untouched asks.
    let turn_spent = fold_stamps(
        None,
        &[
            claim("witness", StampAssessment::NotDone, true, Some(9), 0),
            claim("policy", StampAssessment::NotDone, true, Some(9), 0),
        ],
        budget(3, 3),
    );
    assert!(!turn_spent.held(), "a spent turn completes");
    assert!(
        turn_spent.refutes_done(),
        "completing is not agreeing: the objections stand"
    );
    for row in &turn_spent.rows {
        assert_eq!(
            row.stopped,
            Some(HoldStop::TurnAllowanceSpent {
                spent: 3,
                allowed: 3
            }),
            "{row:?}"
        );
        assert_eq!(
            row.arbiter_allowed, 3,
            "an ask above the ceiling gets the ceiling"
        );
    }
    assert_eq!(turn_spent.turn_spent, 3, "and nothing more is spent");

    // Both ceilings are gone at once. The row names its own, because that is
    // the one it hit. Naming the turn's here would report a number the
    // arbiter never reached, and would put the fold out of step with `again`
    // wherever a manifest asks for less than the host allows.
    let both_gone = fold_stamps(
        None,
        &[claim("witness", StampAssessment::NotDone, true, Some(2), 4)],
        budget(4, 4),
    );
    assert!(!both_gone.held());
    assert_eq!(
        both_gone.rows[0].stopped,
        Some(HoldStop::ArbiterAllowanceSpent {
            spent: 4,
            allowed: 2
        })
    );
}

/// **Fail open, and say so.** An arbiter that did not answer stands aside.
/// It never blocks, and it says which failure it was.
///
/// Three shapes a host catches: a plugin that died, one that ran out of
/// time, and one that wrote junk. All three become the same claim, and none
/// of them holds. `timed_out` still splits the clock from the crash. A claim
/// cut short and a claim weighed are two facts, and the stamp carries
/// both.
#[test]
fn an_arbiter_that_did_not_answer_abstains_and_never_blocks() {
    let arbiter = grant(Participation::Arbiter, Some(3));
    let failures = [
        WrapperError::Exit {
            program: "vera".into(),
            status: "7".into(),
            stderr: "panicked".into(),
        },
        WrapperError::Timeout {
            program: "vera".into(),
            timeout: Duration::from_secs(60),
        },
        WrapperError::NoResponse {
            program: "vera".into(),
            stderr: String::new(),
        },
    ];

    for failure in &failures {
        let claim = ArbiterClaim::did_not_answer("vera", failure, &arbiter);
        assert_eq!(claim.assessment, StampAssessment::Inconclusive);
        assert!(!claim.answered);
        assert_eq!(
            claim.timed_out,
            matches!(failure, WrapperError::Timeout { .. }),
            "only the clock sets the clock flag: {failure}"
        );
        assert!(
            claim.summary.starts_with("arbiter vera did not answer"),
            "the summary is the sentence a surface prints: {}",
            claim.summary
        );
        assert!(
            claim.summary.contains(&failure.to_string()),
            "and it carries the failure's own words: {}",
            claim.summary
        );

        let folded = fold_stamps(None, std::slice::from_ref(&claim), budget(0, 3));
        assert!(!folded.held(), "a dead instrument holds nothing: {failure}");
        assert!(
            !folded.refutes_done(),
            "and it is not a finding that the work is wrong either"
        );
        assert_eq!(folded.unanswered().count(), 1, "but it is on the record");

        // The record the host stores is the wire stamp, against the evidence
        // the claim was made about.
        let hash = format!("sha256:{}", "ab".repeat(32));
        let stamp = claim.into_stamp(hash.clone(), 1_767_225_600_000);
        assert_eq!(stamp.author, "vera");
        assert_eq!(stamp.assessment, StampAssessment::Inconclusive);
        assert_eq!(stamp.preimage_hash, hash);
        assert_eq!(
            stamp.timed_out,
            matches!(failure, WrapperError::Timeout { .. })
        );
    }
}

/// A claim that *answered* and could not tell is not a claim that never
/// answered.
///
/// Both stand aside. Only one is a broken check. A line that could not tell
/// them apart would report a crash on every run whose evidence just did not
/// settle the question.
#[test]
fn an_answered_abstention_is_not_a_silence() {
    let arbiter = grant(Participation::Arbiter, None);
    let undecided = Verdict::Undecided {
        reason: stella_plugin::UndecidedReason::NoOracle,
        undecided: Vec::new(),
    };
    let answered = ArbiterClaim::from_verdict("vera", &undecided, &arbiter, 0);
    assert_eq!(answered.assessment, StampAssessment::Inconclusive);
    assert!(answered.answered);

    let folded = fold_stamps(None, &[answered], budget(0, 2));
    assert_eq!(folded.abstentions().count(), 1);
    assert_eq!(
        folded.unanswered().count(),
        0,
        "it answered; it just could not tell"
    );
}

// ---------------------------------------------------------------------------
// The integration witness: a real dispatch, a real plugin, a real failure.
// ---------------------------------------------------------------------------

/// The arbiter manifest, cut down to the one stage this file needs.
const MANIFEST: &str = r#"
name = "reference-wrapper"
description = "keeps the benchmark inside its recorded budget"

[loop]
participation = "arbiter"
hooks = ["Stop"]
points = ["before_turn", "after_turn"]
max_holds = 3

[requirements]
within-budget = "the benchmark p50 stays inside its recorded budget"

[oracle]
command = { argv = ["bench"], timeout_secs = 60 }
flip = "not-applicable"
measurements = ["p50"]

[[oracle.checks]]
requirement = "within-budget"
check = "p50 <= 105"

[wrapper]
id = "reference-v1"

[[wrapper.stages]]
name = "execute"
"#;

const FIXTURE: &str = env!("CARGO_BIN_EXE_wrapper-plugin-fixture");

/// A plugin that exits non-zero at every point it is asked. The arbiter
/// that dies mid-turn, over the real subprocess transport.
fn dying_plugin() -> Arc<SubprocessWrapper> {
    let argv = vec![
        FIXTURE.to_string(),
        "exit".to_string(),
        "7".to_string(),
        String::new(),
    ];
    Arc::new(
        SubprocessWrapper::declare(argv, Vec::new(), DEFAULT_WRAPPER_TIMEOUT)
            .expect("the transport is declared with a program and a budget")
            .wrapper,
    )
}

/// The host's pre-turn snapshot. The one stage in this manifest has no
/// condition on it. So none of these values matter, and the program is that
/// stage.
fn signals() -> SignalValues {
    SignalValues {
        test_command: false,
        candidates: 1,
        budget_metered: false,
        conversational: false,
        questions: 0,
        plans: true,
        verifies: true,
        wants_witness: false,
        wants_verifier: false,
        mutating_actions: 0,
        diff_lines: 0,
        witness_authored: false,
        flip_achieved: false,
        tests_red: false,
        tests_green: false,
    }
}

/// A host that answers one turn and counts how many it was asked for.
struct Counting {
    turns: u32,
}

#[async_trait(?Send)]
impl TurnDriver for Counting {
    async fn run_turn(&mut self, _prelude: TurnPrelude) -> DrivenTurn {
        self.turns += 1;
        DrivenTurn {
            outcome: TurnOutcome {
                completed: true,
                answer: "did the work anyway".into(),
                tools: Some(vec!["edit_file".into()]),
                changed_files: Some(vec!["crates/stella-core/src/driver.rs".into()]),
            },
            tamper: TamperFinding::NotChecked,
        }
    }
}

/// **The whole-loop witness.** An arbiter dies mid-turn. The turn still
/// ends, and the record says the arbiter never answered.
///
/// The first half already held. A failed point has always meant the plugin
/// stood aside. The record is what was missing. `faults` said what broke and
/// nothing tied it to the verdict. So this run's trace read like a run whose
/// arbiter was happy. Driven through `WrapperDispatch::run`, not the fold
/// alone, because the claim is about what a real run leaves behind.
#[tokio::test]
async fn a_dead_arbiter_completes_the_turn_and_lands_on_the_record() {
    let manifest = PluginManifest::from_toml_str(MANIFEST).expect("the manifest loads");
    let dispatch = WrapperDispatch::bind(manifest, dying_plugin()).expect("[wrapper] is declared");
    let mut host = Counting { turns: 0 };

    let report = dispatch
        .run(
            RoundInput {
                goal: "make the parser faster".into(),
                signals: signals(),
                candidate: None,
            },
            &mut host,
        )
        .await
        .expect("a validated wrapper resolves");

    assert_eq!(host.turns, 1, "the turn ran, and it ran once");
    assert!(!report.faults.is_empty(), "the failures are still reported");

    let silent: Vec<&str> = report
        .arbitration
        .unanswered()
        .map(|row| row.author.as_str())
        .collect();
    assert_eq!(
        silent.len(),
        report.faults.len(),
        "one abstention per failed point: {:?}",
        report.faults
    );
    assert!(
        silent.iter().all(|author| *author == "reference-v1"),
        "each one attributed to the plugin that fell silent: {silent:?}"
    );
    assert!(
        report
            .arbitration
            .unanswered()
            .all(|row| row.assessment == StampAssessment::Inconclusive && row.may_hold),
        "an arbiter's silence is an abstention by an observer that had a say"
    );
    assert!(
        !report.arbitration.held(),
        "and it held nothing open — fail open, loudly"
    );
    assert!(
        !report.arbitration.refutes_done(),
        "a crashed instrument is not a finding that the work is wrong"
    );

    // The arbiter's own claim rides last, from the verdict the loop stopped
    // on: unobserved evidence abstains rather than blaming the worker.
    let last = report
        .arbitration
        .rows
        .last()
        .expect("a claim per round plus the verdict's");
    assert_eq!(last.author, "reference-v1");
    assert_eq!(last.assessment, StampAssessment::Inconclusive);
    assert!(
        last.answered,
        "the host made this claim from a verdict it reached"
    );
}
