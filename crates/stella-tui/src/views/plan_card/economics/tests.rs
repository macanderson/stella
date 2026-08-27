// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What the economics column, the running card and the drift footer say — and
//! what they say when nothing has measured them.

use super::*;
use crate::plan::{EvidenceKind, EvidenceRow, PlanStepState, StepLedger};
use stella_protocol::{
    Check, CheckMechanism, DefinitionOfDone, Judge, ScopeProposal, TaskItem, TaskStatus,
};

fn text(line: &Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

fn rows_text(rows: &[Line<'static>]) -> Vec<String> {
    rows.iter().map(text).collect()
}

fn step(id: &str, title: &str) -> PlanStep {
    PlanStep {
        id: id.into(),
        title: title.into(),
        detail: None,
        state: PlanStepState::Started,
        owner: None,
        note: None,
        contract: None,
    }
}

fn spend(tokens: u64) -> StepSpend {
    StepSpend {
        usd: 0.12,
        tokens,
        cache_read_pct: 40,
        model_calls: 3,
        est_remaining_usd: None,
    }
}

fn running(id: &str) -> RunningTask {
    RunningTask {
        id: id.into(),
        elapsed_ms: 184_000,
        cost_usd: 0.12,
    }
}

fn contract(statement: &str, mechanism: &str, judge: Judge) -> TaskContract {
    TaskContract::DefinitionOfDone(
        DefinitionOfDone::from_vec(vec![Check::new(
            statement,
            CheckMechanism::new(mechanism, judge),
        )])
        .expect("one check is a definition of done"),
    )
}

// ── the economics column ───────────────────────────────────────────────────

/// SPEC 7.3's `9k tok`, from the ledger — and the em dash where nothing has
/// attributed spend to the step.
#[test]
fn the_token_cell_states_a_measurement_or_says_there_is_none() {
    assert_eq!(token_cell(Some(&spend(9_100))), "9.1k tok");
    assert_eq!(token_cell(Some(&spend(420))), "420 tok");
    assert_eq!(token_cell(None), ELIDED);
}

/// **The elision is the claim.** `Plan::ledger` has no producer, so a real
/// plan's every row reads `—` — never `0 tok`, which would be a measurement
/// nobody took.
#[test]
fn a_plan_with_no_ledger_prices_every_step_at_the_elision() {
    let mut plan = Plan::default();
    plan.propose(&ScopeProposal {
        summary: "collapse the rail surfaces".into(),
        steps: vec!["read the band layout".into(), "fold the rail".into()],
        ..Default::default()
    });
    plan.approve();

    let (rows, _) = crate::views::plan_card::step_rows(&plan, 52, None, 0, false, None);
    let text = rows_text(&rows).join("\n");
    assert_eq!(
        text.matches(ELIDED).count(),
        2,
        "one elided cell per step:\n{text}"
    );
    assert!(!text.contains("0 tok"), "{text}");
}

// ── the running card ───────────────────────────────────────────────────────

/// SPEC 7.3's three sub-lines, on the one task the board has in progress.
#[test]
fn the_running_card_states_the_contract_the_evidence_and_the_cost() {
    let mut step = step("2", "fold the rail");
    step.contract = Some(contract(
        "the rail renders once",
        "unit",
        Judge::Deterministic,
    ));
    let plan = Plan::default();

    let rows = rows_text(&running_card(&step, Some(&running("2")), &plan, 52));
    assert_eq!(rows.len(), 3);
    assert!(
        rows[0].contains("contract") && rows[0].contains("the rail renders once"),
        "{rows:#?}"
    );
    assert!(
        rows[0].contains("unit") && rows[0].contains("det"),
        "{rows:#?}"
    );
    assert!(
        rows[1].contains("evidence") && rows[1].ends_with(ELIDED),
        "{rows:#?}"
    );
    assert!(
        rows[2].contains("$0.12") && rows[2].contains("3:04"),
        "{rows:#?}"
    );
}

/// The card belongs to the running task and nothing else, so a caller can hand
/// every step the same `running` and get it exactly once.
#[test]
fn only_the_running_task_draws_a_card() {
    let plan = Plan::default();
    assert!(running_card(&step("3", "verify"), Some(&running("2")), &plan, 52).is_empty());
    assert!(running_card(&step("2", "fold the rail"), None, &plan, 52).is_empty());
}

/// A model-judged check says `model`, never a ratio: SPEC 5 forbids a `det`
/// percentage anywhere, and a check either reaches a model or it does not.
#[test]
fn the_contract_line_names_the_judge_as_a_word_and_never_as_a_ratio() {
    let mut step = step("2", "fold the rail");
    step.contract = Some(contract(
        "the panel reads as one surface",
        "review",
        Judge::Model,
    ));
    let plan = Plan::default();
    let rows = rows_text(&running_card(&step, Some(&running("2")), &plan, 52));
    assert!(rows[0].contains("model"), "{rows:#?}");
    assert!(!rows[0].contains('%'), "{rows:#?}");

    // A read-only task produces no diff, so nothing can settle it and it says
    // so rather than showing a check it does not have.
    let mut read_only = step.clone();
    read_only.contract = Some(TaskContract::ReadOnly);
    let rows = rows_text(&running_card(&read_only, Some(&running("2")), &plan, 52));
    assert!(rows[0].contains("read only · no contract"), "{rows:#?}");

    // And a task the board gave no contract at all names the gap.
    let mut bare = step;
    bare.contract = None;
    let rows = rows_text(&running_card(&bare, Some(&running("2")), &plan, 52));
    assert!(rows[0].contains(ELIDED), "{rows:#?}");
}

/// A contract too wide for the card loses the *sentence*, never the mechanism
/// and its judge — those are what say whether a model gets to decide, and a
/// naive cut drops exactly them because they come last.
#[test]
fn a_wide_contract_line_cuts_the_statement_and_keeps_the_judge() {
    let mut step = step("2", "fold the rail");
    step.contract = Some(contract(
        "every panel in the deck renders at the height the layout gave it and no other",
        "unit",
        Judge::Deterministic,
    ));
    let plan = Plan::default();

    let rows = rows_text(&running_card(&step, Some(&running("2")), &plan, 52));
    assert!(rows[0].ends_with("· unit · det"), "{rows:#?}");
    assert!(rows[0].contains('…'), "the statement was cut: {rows:#?}");
    assert!(
        rows[0].chars().count() <= 52,
        "and the row still fits the card: {rows:#?}"
    );
}

/// The evidence line counts this task's own rows once a ledger holds them.
#[test]
fn the_evidence_line_counts_the_rows_the_ledger_holds_for_this_task() {
    let mut plan = Plan::default();
    plan.ledger.insert(
        "2".into(),
        StepLedger {
            evidence: vec![EvidenceRow {
                kind: EvidenceKind::Edit,
                subject: "crates/stella-tui/src/views/frame.rs".into(),
                outcome: "+41 -6".into(),
            }],
            spend: None,
        },
    );
    let rows = rows_text(&running_card(
        &step("2", "fold the rail"),
        Some(&running("2")),
        &plan,
        52,
    ));
    assert!(rows[1].contains('1'), "{rows:#?}");
    assert!(!rows[1].contains(ELIDED), "{rows:#?}");
}

// ── the drift footer ───────────────────────────────────────────────────────

/// SPEC 7.3's footer, with lanes: `planned n · actual m · ⌥ k drift`, then the
/// closing sentence.
#[test]
fn the_footer_counts_both_lanes_and_the_drift_between_them() {
    use crate::plan::{ActualStep, PlanLanes};

    let mut plan = Plan::default();
    plan.propose(&ScopeProposal {
        summary: "collapse the rail surfaces".into(),
        steps: vec!["a".into(), "b".into(), "c".into()],
        ..Default::default()
    });
    plan.approve();
    plan.lanes = Some(PlanLanes {
        planned: vec!["a".into(), "b".into(), "c".into()],
        actual: vec![
            ActualStep {
                title: "a".into(),
                cause: None,
            },
            ActualStep {
                title: "b".into(),
                cause: None,
            },
            ActualStep {
                title: "c".into(),
                cause: None,
            },
            ActualStep {
                title: "repair the tests gate".into(),
                cause: Some("E0432".into()),
            },
        ],
    });

    let rows = rows_text(&footer_rows(&plan));
    assert_eq!(rows[0], "planned 3 · actual 4 · ⌥ 1 drift");
    assert_eq!(
        rows[1],
        "drift is recorded, not hidden. it trains your model."
    );
    assert!(!rows[0].contains('⑂'), "the drift mark is the theme's ⌥");
}

/// **The elision, again.** `Plan::lanes` has no producer, so the two counts it
/// owns read `—`. `planned` is the approved plan's own step count and is
/// stated, because the fold genuinely has it.
#[test]
fn the_footer_elides_the_counts_that_have_no_producer() {
    let mut plan = Plan::default();
    plan.propose(&ScopeProposal {
        summary: "collapse the rail surfaces".into(),
        steps: vec!["a".into(), "b".into(), "c".into()],
        ..Default::default()
    });
    plan.approve();
    assert_eq!(plan.lanes, None, "nothing writes lanes today (#5270)");

    let rows = rows_text(&footer_rows(&plan));
    assert_eq!(rows[0], "planned 3 · actual — · ⌥ — drift");
    assert!(!rows[0].contains(" 0 drift"), "{rows:#?}");
}

/// A board-only plan was never approved as a list, so `planned` has nothing to
/// count and says so rather than reporting the board's growing length.
#[test]
fn a_board_only_plan_elides_the_planned_count_too() {
    let mut plan = Plan::default();
    plan.apply_board(&[TaskItem {
        id: "1".into(),
        subject: "read the routes".into(),
        description: None,
        status: TaskStatus::InProgress,
        owner: None,
        contract: None,
    }]);
    assert_eq!(plan.planned_count(), None);
    assert_eq!(
        rows_text(&footer_rows(&plan))[0],
        "planned — · actual — · ⌥ — drift"
    );
}
