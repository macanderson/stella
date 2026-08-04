//! Accessible mode's layout contract (#1258, phase 3): **no rendered row
//! carries content from two logical panes.**
//!
//! A side-by-side split is a visual affordance. Read aloud, a row that holds
//! the node list on its left and the detail panel on its right is one
//! interleaved line, and the two panes — which are separate documents — are
//! spliced token by token. Linear layout is what makes reading order match
//! document order.
//!
//! Only three splits exist in the whole TUI, and this file pins all three:
//! GRAPH, SKILLS, and the Session tab's work rail. Each test also asserts the
//! *default* frame still splits, so the pair reads as "this is the change" and
//! a regression that linearised everything for everyone would fail too.
//!
//! The probe is a pair of needles that only one pane can produce. Both must
//! appear, and no single row may hold both.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use stella_protocol::{AgentEvent, ScopeProposal, StageKind};
use stella_tui::scenario::demo_graph;
use stella_tui::{
    AgentMeta, DeckTab, DeckUi, FlushBlock, Inbound, SkillRow, SkillScope, SkillsView,
    WorkspaceModel, render_deck,
};

/// Wide enough that every split this file cares about is above its own
/// threshold — the work rail in particular only appears past `RAIL_MIN_COLS`.
const W: u16 = 160;
const H: u16 = 44;

fn rows(model: &WorkspaceModel, ui: &mut DeckUi, w: u16, h: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("TestBackend");
    terminal
        .draw(|f| render_deck(model, ui, f))
        .expect("render_deck");
    let buf = terminal.backend().buffer();
    let area = *buf.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
        })
        .collect()
}

/// Whether any single row carries both needles — i.e. whether the two panes
/// are side by side.
fn shares_a_row(rows: &[String], left: &str, right: &str) -> bool {
    rows.iter().any(|r| r.contains(left) && r.contains(right))
}

fn assert_both_present(rows: &[String], left: &str, right: &str) {
    let joined = rows.join("\n");
    assert!(
        rows.iter().any(|r| r.contains(left)),
        "the fixture must actually render {left:?}:\n{joined}"
    );
    assert!(
        rows.iter().any(|r| r.contains(right)),
        "the fixture must actually render {right:?}:\n{joined}"
    );
}

/// Both panes render, and never on the same row.
fn assert_linear(rows: &[String], left: &str, right: &str) {
    assert_both_present(rows, left, right);
    assert!(
        !shares_a_row(rows, left, right),
        "a rendered row carries both panes — read aloud that is one interleaved line:\n{}",
        rows.join("\n")
    );
}

fn deck_ui(tab: DeckTab, accessible: bool) -> DeckUi {
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ui.notice.dismiss();
    ui.tab = tab;
    ui.accessible = accessible;
    ui
}

fn one_agent() -> WorkspaceModel {
    let mut model = WorkspaceModel::new();
    model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
    model
}

// ───────────────────────────────── GRAPH ─────────────────────────────────

/// The node list's block title, and the detail panel's — one per pane, and
/// both on their pane's top border row, so a two-column layout puts them on
/// the same terminal row and a stacked one cannot.
const GRAPH_LIST: &str = " nodes · ";
const GRAPH_DETAIL: &str = "engine step driver";

fn graph_ui(accessible: bool) -> DeckUi {
    let mut ui = deck_ui(DeckTab::Graph, accessible);
    ui.graph = Some(demo_graph());
    ui
}

#[test]
fn the_graph_tab_stacks_its_panes_in_accessible_mode() {
    let model = one_agent();
    let mut ui = graph_ui(true);
    let frame = rows(&model, &mut ui, W, H);
    assert_linear(&frame, GRAPH_LIST, GRAPH_DETAIL);
}

#[test]
fn the_graph_tab_still_splits_side_by_side_by_default() {
    let model = one_agent();
    let mut ui = graph_ui(false);
    let frame = rows(&model, &mut ui, W, H);
    assert_both_present(&frame, GRAPH_LIST, GRAPH_DETAIL);
    assert!(
        shares_a_row(&frame, GRAPH_LIST, GRAPH_DETAIL),
        "an ordinary session keeps the two-column graph:\n{}",
        frame.join("\n")
    );
}

// ───────────────────────────────── SKILLS ────────────────────────────────

const SKILLS_INSTALLED: &str = " Installed — ";
const SKILLS_SEARCH: &str = " Registry search ";

fn skills_ui(accessible: bool) -> DeckUi {
    let mut ui = deck_ui(DeckTab::Skills, accessible);
    ui.skills.view = SkillsView {
        rows: vec![SkillRow {
            scope: SkillScope::Project,
            name: "pdf".into(),
            description: "read and write PDFs".into(),
            body: String::new(),
            origin: "workspace".into(),
            enabled: true,
            version: 1,
            latest: 1,
            removable: true,
        }],
        ..Default::default()
    };
    ui
}

#[test]
fn the_skills_tab_stacks_its_panes_in_accessible_mode() {
    let model = one_agent();
    let mut ui = skills_ui(true);
    let frame = rows(&model, &mut ui, W, H);
    assert_linear(&frame, SKILLS_INSTALLED, SKILLS_SEARCH);
}

#[test]
fn the_skills_tab_still_splits_side_by_side_by_default() {
    let model = one_agent();
    let mut ui = skills_ui(false);
    let frame = rows(&model, &mut ui, W, H);
    assert_both_present(&frame, SKILLS_INSTALLED, SKILLS_SEARCH);
    assert!(
        shares_a_row(&frame, SKILLS_INSTALLED, SKILLS_SEARCH),
        "an ordinary session keeps the two-column skills manager:\n{}",
        frame.join("\n")
    );
}

// ────────────────────────────── SESSION RAIL ─────────────────────────────

/// A lane whose turn has an approved scope, so the work rail has something to
/// stand a column up for. The scope graduates on the first non-`ScopeReview`
/// stage — that transition IS the approval signal.
fn scoped_session(accessible: bool) -> (WorkspaceModel, DeckUi) {
    let mut model = one_agent();
    let event = |event: AgentEvent| Inbound::Event {
        agent: "lead".into(),
        event,
    };
    model.apply_inbound(&event(AgentEvent::ScopeReview {
        proposal: ScopeProposal {
            summary: "linearise the deck".into(),
            steps: vec!["stack the graph panes".into(), "stack the skills".into()],
            estimated_files: 4,
            estimated_cost_usd: Some(0.40),
            ..Default::default()
        },
    }));
    model.apply_inbound(&event(AgentEvent::Stage {
        name: StageKind::Execute,
    }));
    model.apply_inbound(&event(AgentEvent::Text {
        delta: "the transcript body".into(),
    }));
    (model, deck_ui(DeckTab::Session, accessible))
}

/// The rail's own block title. Deliberately not the step counts — those also
/// appear in the one-row strip, which accessible mode keeps.
const RAIL_SCOPE: &str = " SCOPE ✓ ";
/// The transcript pane's block title, which sits on the same row as the rail's
/// when the two are side by side.
const TRANSCRIPT_TITLE: &str = " transcript · ";
const TRANSCRIPT_BODY: &str = "the transcript body";

#[test]
fn the_session_tab_drops_the_side_rail_in_accessible_mode() {
    let (model, mut ui) = scoped_session(true);
    let frame = rows(&model, &mut ui, W, H);
    assert!(
        frame.iter().any(|r| r.contains(TRANSCRIPT_BODY)),
        "the transcript still renders:\n{}",
        frame.join("\n")
    );
    assert!(
        !frame.iter().any(|r| r.contains(RAIL_SCOPE)),
        "the rail is a column beside the transcript, so every row it occupies \
         carries two panes; accessible mode takes the same path a narrow \
         terminal already takes — the one-row strip, plus ⌃S for the whole \
         thing:\n{}",
        frame.join("\n")
    );
}

#[test]
fn the_session_tab_still_raises_the_side_rail_on_a_wide_frame_by_default() {
    let (model, mut ui) = scoped_session(false);
    let frame = rows(&model, &mut ui, W, H);
    assert!(
        shares_a_row(&frame, TRANSCRIPT_TITLE, RAIL_SCOPE),
        "an ordinary wide session keeps the rail beside the transcript:\n{}",
        frame.join("\n")
    );
}

// ─────────────────────────── SCROLLBACK vs THE PANE ──────────────────────

/// The second invariant #1258 names for phase 2: settled content is rendered
/// **once**. An entry that reached the terminal's scrollback must stop being
/// painted by the live pane, or a reader hears the conversation twice — once
/// as it arrived, once as part of a rectangle that repaints.
///
/// Driven through `render_deck` rather than the fold cache directly, because
/// what matters is what lands on the terminal.
#[test]
fn the_live_pane_skips_whatever_is_already_in_scrollback() {
    let mut model = one_agent();
    for line in ["already flushed", "still live"] {
        for event in [
            AgentEvent::Stage {
                name: StageKind::Execute,
            },
            AgentEvent::Text { delta: line.into() },
        ] {
            model.apply_inbound(&Inbound::Event {
                agent: "lead".into(),
                event,
            });
        }
    }

    let mut ui = deck_ui(DeckTab::Session, true);
    let before = rows(&model, &mut ui, W, H).join("\n");
    assert!(
        before.contains("already flushed") && before.contains("still live"),
        "nothing is in scrollback yet, so the pane paints the whole transcript:\n{before}"
    );

    // Two entries — a stage row and its message — moved into scrollback.
    ui.scrollback.set_live(true);
    ui.scrollback.record(&FlushBlock {
        agent: "lead".into(),
        from: 0,
        to: 2,
        header: None,
    });
    let after = rows(&model, &mut ui, W, H).join("\n");
    assert!(
        !after.contains("already flushed"),
        "a flushed entry must leave the live pane:\n{after}"
    );
    assert!(
        after.contains("still live"),
        "everything not yet flushed keeps painting:\n{after}"
    );
}
