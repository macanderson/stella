//! Witness: the plan card renders the plan's operating envelope as a labeled
//! grid — repo (with `⎇ branch`), write/read globs, budget with its cap, the
//! `think` / `work` / `verify` model slots, the shell policy, and the literal
//! done-when contract — over the readable step list, and flips to `locked`
//! once the gate has approved.
//!
//! These rows used to be the `/scope` card's whole content, on a surface that
//! showed the envelope and never the steps. The card now carries both; these
//! tests pin the grid's labels, the lock line, the step list, and the
//! accessible labeled-record form.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use stella_protocol::{AgentEvent, ScopeProposal, StageKind};
use stella_tui::deck_ui::cards::Card;
use stella_tui::{AgentMeta, DeckUi, Inbound, WorkspaceModel, render_deck};

fn scoped_model(approved: bool) -> WorkspaceModel {
    let mut m = WorkspaceModel::new();
    m.now_ms = 10_000;
    m.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
    m.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::ScopeReview {
            proposal: ScopeProposal {
                summary: "wire the automations API".into(),
                steps: vec!["route".into(), "guard".into()],
                estimated_files: 3,
                estimated_cost_usd: Some(0.15),
                repo: Some("macanderson/web-app".into()),
                branch: Some("feat/triggers".into()),
                write_globs: vec!["apps/api/**".into()],
                read_globs: vec!["apps/**".into()],
                shell_policy: Some("allowlisted".into()),
            },
        },
    });
    if approved {
        // Approval = the first non-ScopeReview stage.
        m.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Stage {
                name: StageKind::Execute,
            },
        });
    }
    m
}

fn frame(model: &WorkspaceModel, accessible: bool) -> String {
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ui.accessible = accessible;
    ui.cards.raise(Card::Plan);
    let mut terminal = Terminal::new(TestBackend::new(120, 32)).expect("TestBackend");
    terminal
        .draw(|f| render_deck(model, &mut ui, f))
        .expect("render_deck");
    let buf = terminal.backend().buffer();
    let area = *buf.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_grid_carries_every_labeled_row() {
    let text = frame(&scoped_model(false), false);
    for needle in [
        "repo",
        "macanderson/web-app",
        "⎇ feat/triggers",
        "write",
        "apps/api/**",
        "read",
        "budget",
        "models",
        "think",
        "work",
        "verify",
        "shell",
        "allowlisted",
        "done when",
        "oracle flips red → green",
        "(confirmed from evidence, not self-report)",
    ] {
        assert!(text.contains(needle), "grid missing {needle:?}:\n{text}");
    }
}

#[test]
fn post_approval_the_card_reads_locked_with_the_edit_affordance() {
    let pending = frame(&scoped_model(false), false);
    assert!(
        pending.contains("pending approval"),
        "a waiting gate reads pending:\n{pending}"
    );
    let locked = frame(&scoped_model(true), false);
    assert!(
        locked.contains("locked"),
        "an approved plan reads locked:\n{locked}"
    );
    assert!(
        locked.contains("e edit"),
        "…and still offers the edit affordance:\n{locked}"
    );
}

/// The card's other half, and the one the old `/scope` never had: the steps
/// themselves, readable, with their status rings.
#[test]
fn the_card_lists_the_plan_steps_over_the_envelope() {
    let text = frame(&scoped_model(true), false);
    for needle in ["route", "guard", "wire the automations API", "approved 0/2"] {
        assert!(
            text.contains(needle),
            "step list missing {needle:?}:\n{text}"
        );
    }
}

#[test]
fn accessible_mode_reads_the_grid_as_labeled_records() {
    let text = frame(&scoped_model(true), true);
    let row = text
        .lines()
        .find(|l| l.contains("macanderson/web-app"))
        .unwrap_or_else(|| panic!("no repo row:\n{text}"))
        .trim_matches(|c| c == '│' || c == ' ');
    assert!(
        row.contains("· repo "),
        "the repo value is labeled inline: {row:?}"
    );
    assert!(
        !row.contains("  "),
        "no column alignment in accessible mode: {row:?}"
    );
}
