//! Witness: the SESSION tab's nested subagent rows (D5) — under the lead's
//! header, each registered subagent gets a `└─ ◆` block with its vitals and
//! its `returns: … → lead inbox` contract — and in accessible mode those
//! rows are labeled records that never share a rendered row with the
//! transcript (#1258's layout contract, applied to the new surface).
//!
//! Before D5 subagents appeared only on the AGENTS dashboard: the SESSION
//! tab said nothing about the lanes working under the lead, so there was no
//! row to pin. The needles here are glyphs and words, not styles, because
//! the golden suite is style-blind and this file follows the same rule.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use stella_protocol::AgentEvent;
use stella_tui::{AgentMeta, DeckUi, Inbound, WorkspaceModel, render_deck};

fn model_with_subagent() -> WorkspaceModel {
    let mut m = WorkspaceModel::new();
    m.now_ms = 10_000;
    m.apply_inbound(&Inbound::Register(
        AgentMeta::new("lead", "the goal", 0).with_role("lead"),
    ));
    m.apply_inbound(&Inbound::Register(
        AgentMeta::new("sub:auth", "wire the API", 0).with_role("subagent"),
    ));
    m.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Text {
            text: "the transcript body".into(),
        },
    });
    m
}

fn rows(model: &WorkspaceModel, accessible: bool) -> Vec<String> {
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ui.accessible = accessible;
    let mut terminal = Terminal::new(TestBackend::new(160, 44)).expect("TestBackend");
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
        .collect()
}

#[test]
fn a_registered_subagent_gets_its_nested_block_under_the_lead() {
    let model = model_with_subagent();
    let frame = rows(&model, false).join("\n");
    for needle in [
        "└─ ◆ sub:auth",
        "subagent",
        "returns: wire the API → lead inbox",
    ] {
        assert!(frame.contains(needle), "missing {needle:?}:\n{frame}");
    }
}

#[test]
fn subagent_rows_never_share_a_rendered_row_with_the_transcript_in_accessible_mode() {
    let model = model_with_subagent();
    let frame = rows(&model, true);
    let joined = frame.join("\n");
    assert!(
        joined.contains("the transcript body"),
        "the transcript still renders:\n{joined}"
    );
    assert!(
        joined.contains("· subagent sub:auth"),
        "the subagent block renders as a labeled record:\n{joined}"
    );
    for row in &frame {
        assert!(
            !(row.contains("subagent sub:auth") && row.contains("the transcript body")),
            "a rendered row carries both the subagent block and the transcript — read aloud \
             that is one interleaved line: {row:?}"
        );
    }
}

#[test]
fn accessible_vitals_read_as_labeled_records() {
    let model = model_with_subagent();
    let frame = rows(&model, true);
    let row = frame
        .iter()
        .find(|r| r.contains("· model "))
        .unwrap_or_else(|| panic!("no vitals row:\n{}", frame.join("\n")))
        .trim_matches(|c| c == '│' || c == ' ');
    for label in ["model", "ctx", "spend"] {
        assert!(
            row.contains(&format!("· {label} ")),
            "vitals must label each value; {label:?} is unlabelled in {row:?}"
        );
    }
    assert!(
        !row.contains("  "),
        "no column alignment in accessible mode: {row:?}"
    );
}
