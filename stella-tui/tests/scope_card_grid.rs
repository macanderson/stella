//! Witness: the scope card v2 renders the run's scope as a labeled grid —
//! repo (with `⎇ branch`), write/read globs, budget with its cap, the
//! `think` / `work` / `verify` model slots, the shell policy, and the
//! literal done-when contract — and flips to `locked at plan · e to edit`
//! once the gate has approved.
//!
//! Before D5 the scope surface was the approval card's summary + step list
//! plus the SCOPE rail's counts: repo, globs, budget, routing and the shell
//! policy arrived nowhere, and post-approval the scope could only be
//! recalled through `⌃S`. These tests pin the grid's labels, the lock line,
//! and the accessible labeled-record form.

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
    ui.cards.raise(Card::Scope);
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
        "(witness confirms from evidence)",
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
        locked.contains("locked at plan · e to edit"),
        "an approved scope reads locked:\n{locked}"
    );
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
