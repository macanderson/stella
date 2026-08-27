// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! SPEC 7.3's expanded plan panel, with every producer supplied — the
//! economics column, the running-task card, a drift row, and the drift footer.
//!
//! The card the workspace fixture draws (`card_plan`) is the *live* state:
//! nothing writes `Plan::ledger` or `Plan::lanes` yet (#5286, #5270), so every
//! economics cell there reads `—` and the footer's two lane counts do too.
//! That golden is what a user sees today, and it stays.
//!
//! This one is the other half: the same card with the producers standing in,
//! so the layout those issues will land into is pinned now rather than
//! discovered later.
//!
//! A submodule for the reason [`super::gate_board`] is one: the parent is
//! within twenty lines of its ceiling. Every helper comes from the parent, and
//! one command blesses them all:
//! `BLESS=1 cargo test -p stella-tui --test deck_render_snapshots`.
//!
//! Colour is stripped by `render_frame`, so the gold/`GOLD_BRIGHT` claims stay
//! where they can be asserted on spans — `views::plan_card::step_style`'s own
//! tests for the drift row's metal, and
//! `views::plan_card::economics::tests` for the rest.

use super::*;

use stella_protocol::{TaskItem, TaskStatus};
use stella_tui::deck::ActiveTaskStamp;
use stella_tui::deck_ui::cards::Card;
use stella_tui::plan::{ActualStep, EvidenceKind, EvidenceRow, PlanLanes, StepLedger, StepSpend};

fn task(id: &str, subject: &str, status: TaskStatus) -> TaskItem {
    TaskItem {
        id: id.into(),
        subject: subject.into(),
        description: None,
        status,
        owner: None,
        contract: None,
    }
}

fn spend(usd: f64, tokens: u64) -> StepSpend {
    StepSpend {
        usd,
        tokens,
        cache_read_pct: 62,
        model_calls: 3,
        est_remaining_usd: None,
    }
}

/// The plan panel with every SPEC 7.3 element on screen at once.
#[test]
fn deck_render_snapshots_pin_the_plan_panel_economics() {
    let mut model = fixture_model();
    let agent = model.agents.first_mut().expect("the lead lane");

    // A plan of three approved steps, plus one the plan did not contain.
    agent.model.plan.apply_board(&[
        task("1", "extract types", TaskStatus::Completed),
        task("2", "move hooks", TaskStatus::InProgress),
        task("3", "update 9 imports", TaskStatus::Pending),
        task("4", "repair the failing tests gate", TaskStatus::Pending),
    ]);
    // What each step spent — SPEC 7.3's right-aligned `9k tok`.
    for (id, usd, tokens) in [("1", 0.31, 9_100u64), ("2", 0.12, 4_200)] {
        agent.model.plan.ledger.insert(
            id.into(),
            StepLedger {
                evidence: vec![EvidenceRow {
                    kind: EvidenceKind::Edit,
                    subject: "packages/automations/src/types.ts".into(),
                    outcome: "+41 -6".into(),
                }],
                spend: Some(spend(usd, tokens)),
            },
        );
    }
    // The two lanes, disagreeing by one step — which is what makes task 4 a
    // drift row rather than an ordinary one.
    agent.model.plan.lanes = Some(PlanLanes {
        planned: vec![
            "extract types".into(),
            "move hooks".into(),
            "update 9 imports".into(),
        ],
        actual: vec![
            ActualStep {
                title: "extract types".into(),
                cause: None,
            },
            ActualStep {
                title: "move hooks".into(),
                cause: None,
            },
            ActualStep {
                title: "repair the failing tests gate".into(),
                cause: Some("E0432: unresolved import".into()),
            },
        ],
    });
    // The board's in-progress task, with the anchors the running card's cost
    // and clock are measured against.
    agent.active_task = Some(ActiveTaskStamp {
        id: "2".into(),
        started_ms: model.now_ms.saturating_sub(184_000),
        cost_at_start_usd: agent.cost_usd - 0.12,
    });

    let mut ui = ui_for(DeckTab::Session);
    ui.cards.raise(Card::Plan);
    let frame = render_frame(&model, &mut ui, W, H);

    // SPEC 7.2, 7.3 and 8.1 all write `⌥`, `stella_tui_theme::glyph::DRIFT` is
    // `⌥`, and `glyph::ALL` is gate-enforced — so the `⑂` this issue wrote
    // would be a drift mark no other surface draws. An absence assertion,
    // because a blessed golden cannot be relied on to notice a glyph swap.
    assert!(frame.contains('⌥'), "{frame}");
    assert!(!frame.contains('⑂'), "{frame}");
    // SPEC 5 forbids a `det` ratio anywhere, and this issue drops it from the
    // economics line by name.
    assert!(!frame.contains("det est"), "{frame}");
    assert!(!frame.contains("det %"), "{frame}");

    assert_golden(
        "card_plan_economics",
        "SPEC 7.3: per-task economics, the running-task card, a drift row, the drift footer",
        W,
        H,
        &frame,
    );
}
