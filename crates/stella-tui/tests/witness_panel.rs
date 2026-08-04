//! Witness: the witness panel (D6) renders the deterministic proof as three
//! records in strict order — author → execute → result, exactly one active —
//! and the color red carries exactly one meaning in the deck: the oracle's
//! pre-flip state, in its own `theme::ORACLE_PRE_FLIP` role.
//!
//! Before D6 the proof fold reached the screen only as the rail's five rows
//! and the strip's one cell: no surface told the author → execute → result
//! story, run counts and seeds had no home, and "red" as a healthy expected
//! state did not exist. These tests pin the phase walk, the replay facts
//! (`run 2 of 3`, `deterministic seed 7741`), the tamper-exclusion line, and
//! the red-token rule — with the expected color derived from the theme role,
//! never a hex or hue predicate (the same discipline as
//! `tests/progress_brand_fill.rs`, whose header explains why).

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

use stella_protocol::{AgentEvent, ProofStep, ProofTree};
use stella_tui::deck_ui::cards::Card;
use stella_tui::theme;
use stella_tui::{AgentMeta, DeckUi, Inbound, WorkspaceModel, render_deck};

/// The demo-shaped proof state: witness authored, confirmed red pre-patch,
/// two of three candidate replays green.
fn mid_execute_model() -> WorkspaceModel {
    let mut m = WorkspaceModel::new();
    m.now_ms = 10_000;
    m.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
    let ev = |event: AgentEvent| Inbound::Event {
        agent: "lead".into(),
        event,
    };
    m.apply_inbound(&ev(AgentEvent::Proof {
        step: ProofStep::WitnessAuthored {
            path: "tests/triggers.rs".into(),
            command: "cargo test -p x triggers".into(),
            fingerprint: "sha256:9f3c".into(),
        },
    }));
    let oracle = |passed: bool, tree: ProofTree, run: Option<u32>| {
        ev(AgentEvent::Proof {
            step: ProofStep::Oracle {
                command: "cargo test -p x triggers".into(),
                passed,
                tree,
                run,
                runs_required: Some(3),
                seed: Some(7741),
            },
        })
    };
    m.apply_inbound(&oracle(false, ProofTree::Baseline, None));
    m.apply_inbound(&oracle(true, ProofTree::Candidate, Some(1)));
    m.apply_inbound(&oracle(true, ProofTree::Candidate, Some(2)));
    m
}

/// Render the full deck with the witness panel up; return the raw buffer.
fn witness_frame(model: &WorkspaceModel, accessible: bool) -> ratatui::buffer::Buffer {
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ui.accessible = accessible;
    ui.cards.raise(Card::Witness);
    let mut terminal = Terminal::new(TestBackend::new(140, 40)).expect("TestBackend");
    terminal
        .draw(|f| render_deck(model, &mut ui, f))
        .expect("render_deck");
    terminal.backend().buffer().clone()
}

fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
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
fn the_three_records_render_in_order_with_exactly_one_active() {
    let buf = witness_frame(&mid_execute_model(), false);
    let text = buffer_text(&buf);
    let pos = |needle: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("missing {needle:?}:\n{text}"))
    };
    // Strict order, and the phase glyphs say which is which: author done,
    // execute active, result pending.
    assert!(pos("author") < pos("execute"), "author before execute");
    assert!(pos("execute") < pos("result"), "execute before result");
    assert!(text.contains("✓  author"), "author is done:\n{text}");
    assert!(text.contains("▸  execute"), "execute is active:\n{text}");
    assert!(text.contains("○  result"), "result is pending:\n{text}");
    // The replay facts and the evidence clause.
    for needle in [
        "test built before the patch",
        "confirmed",
        "red",
        "pre-patch",
        "replaying oracle against patched worktree",
        "run 2 of 3",
        "deterministic seed 7741",
        "evidence → witness only, no self-report",
        "awaiting flip",
        "red ──▸ green",
        "3/3 required",
        "run ends only on oracle flip",
        "shadow sandbox",
    ] {
        assert!(text.contains(needle), "missing {needle:?}:\n{text}");
    }
}

#[test]
fn red_tokens_carry_the_oracle_pre_flip_role_and_nothing_else_on_the_panel_does() {
    // Every cell of the frame whose fg is the ORACLE_PRE_FLIP role must be
    // part of a literal `red` token, and every rendered `red` token must
    // carry that role — the color and the word are one meaning. Derived
    // from the theme role, never from a hex predicate: a recolor of the
    // role's value must not touch this test.
    let buf = witness_frame(&mid_execute_model(), false);
    let area = *buf.area();
    let mut role_cells = 0usize;
    for y in 0..area.height {
        for x in 0..area.width {
            let Some(cell) = buf.cell((x, y)) else {
                continue;
            };
            if cell.fg != theme::ORACLE_PRE_FLIP {
                continue;
            }
            role_cells += 1;
            // The cell belongs to a `red` token: r, e, or d.
            assert!(
                matches!(cell.symbol(), "r" | "e" | "d"),
                "an ORACLE_PRE_FLIP cell at ({x},{y}) is not part of a `red` token: {:?}",
                cell.symbol()
            );
        }
    }
    // The author line's `red` and the result line's `red ──▸ green` both
    // paint: at least two tokens' worth of cells.
    assert!(
        role_cells >= 6,
        "expected both red tokens to carry the role, found {role_cells} cells"
    );

    // And DANGER never stands in for the pre-flip state: the panel's `red`
    // tokens must not be the generic error red. (The roles are distinct
    // values by design — asserted here so a lazy alias can't creep back.)
    assert_ne!(
        theme::ORACLE_PRE_FLIP,
        theme::DANGER,
        "oracle_pre_flip must not alias the generic error red"
    );
    let _: Color = theme::ORACLE_PRE_FLIP;
}

#[test]
fn a_refused_flip_is_a_distinct_result_state_not_a_generic_failure() {
    // The pipeline refused a would-be flip (the pass fixed a different
    // failure): the result record must say so in its own words, and must
    // not render the generic `failed` vocabulary.
    let mut m = mid_execute_model();
    m.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Verdict {
            passed: false,
            evidence: stella_protocol::VerdictEvidence {
                summary: "flip refused".into(),
                deterministic: true,
                evidence_refs: vec![],
                ladder: Some(Box::new(stella_protocol::LadderSnapshot {
                    rung: None,
                    tracked_command: Some("cargo test -p x triggers".into()),
                    oracle_trace: vec![],
                    flip_achieved: false,
                    unstable_flip: false,
                    flip_refused_different_failure: true,
                    touched_tests_passed: None,
                    test_infra: None,
                    diff_lines: 4,
                    diff_budget: 300,
                    diff_available: true,
                    file_change_events: 1,
                    mutating_actions: 1,
                    new_diag_errors: 0,
                    new_diag_warnings: 0,
                    witness_intact: Some(true),
                    witness_mutation: None,
                    diff_coverage: None,
                })),
            },
        },
    });
    let text = buffer_text(&witness_frame(&m, false));
    assert!(text.contains("flip refused"), "the distinct state:\n{text}");
    assert!(
        text.contains("the pass fixed a different failure · no credit"),
        "the refusal names its reason:\n{text}"
    );
    // Tamper exclusion is stated when the pipeline reported it.
    assert!(
        text.contains("tamper check ✓ witness files untouched"),
        "the tamper line renders under execute:\n{text}"
    );
}

#[test]
fn accessible_mode_reads_the_result_as_a_labeled_record() {
    let text = buffer_text(&witness_frame(&mid_execute_model(), true));
    let row = text
        .lines()
        .find(|l| l.contains("· phase result"))
        .unwrap_or_else(|| panic!("no accessible result record:\n{text}"))
        .trim_matches(|c| c == '│' || c == ' ');
    for needle in ["· phase result", "· oracle red", "· required 3 of 3"] {
        assert!(row.contains(needle), "missing {needle:?} in {row:?}");
    }
    assert!(
        !row.contains("  "),
        "no column alignment in accessible mode: {row:?}"
    );
}
