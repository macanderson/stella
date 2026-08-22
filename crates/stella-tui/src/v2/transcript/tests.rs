//! P1 acceptance (IMPLEMENTATION-PLAN §3): a scripted turn renders begin,
//! skill, read, run and receipt; fold and unfold differ; every event row shows
//! its rail in the correct metal; a healthy frame contains no red.

use super::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::widgets::{Paragraph, Widget};

const W: usize = 88;

/// The scripted turn the plan names: begin, skill, read, run, receipt.
fn scripted_turn() -> (TurnHead, Vec<Event>, Receipt) {
    let head = TurnHead {
        number: 14,
        stage: "execute".into(),
        model: Some("kimi-k3".into()),
        budget_usd: Some(0.60),
        queued_steer: None,
    };

    let mut skill = Event::new(
        EventKind::Skill {
            trigger: "auto".into(),
            tokens: 1200,
        },
        "oxagen-feature",
    );
    skill.footer = Some("  injected 10-layer feature contract · used 42× this repo".into());

    let mut read = Event::new(EventKind::Read, "…/lifecycle.rs");
    read.duration_ms = 3;

    let mut edit = Event::new(
        EventKind::Edit {
            extent: Extent::delta(3, 1),
        },
        "…/self_driving_cmd.rs",
    );
    edit.duration_ms = 2;
    edit.task = Some(3);
    edit.body = vec![Line::raw("  122 - digest: Vec<String>,")];

    let mut run = Event::new(EventKind::Run, "cargo test -p stella-core");
    run.duration_ms = 1840;

    let receipt = Receipt {
        spend_usd: 0.11,
        tokens: Some(18_000),
        det_pct: Some(86),
        tests_passed: 4,
        tests_total: 4,
        files: 2,
        memories: 1,
    };
    (head, vec![skill, read, edit, run], receipt)
}

/// Render a turn to a fixed grid so the assertions below read the same thing a
/// terminal would.
fn frame(head: &TurnHead, events: &[Event], r: &Receipt) -> (Buffer, String) {
    let mut lines = vec![turn_begin(head, W)];
    for e in events {
        lines.extend(event_rows(e, W));
    }
    lines.push(turn_end(head.number, Some("0:42"), W));
    lines.push(receipt(r));

    let h = lines.len() as u16;
    let mut term = Terminal::new(TestBackend::new(W as u16, h)).unwrap();
    term.draw(|f| {
        let area = f.area();
        Paragraph::new(lines.clone()).render(area, f.buffer_mut());
    })
    .unwrap();
    let buf = term.backend().buffer().clone();
    let text = (0..buf.area().height)
        .map(|y| {
            (0..buf.area().width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    (buf, text)
}

#[test]
fn a_scripted_turn_renders_begin_events_and_receipt() {
    let (head, events, r) = scripted_turn();
    let (_, text) = frame(&head, &events, &r);
    eprintln!("\n{text}\n");

    for needle in [
        "── turn 14 execute · kimi-k3 · budget $0.60",
        "✦ skill oxagen-feature · auto",
        "injected 10-layer feature contract",
        // No size column: a read has no producer for one, so the head states
        // its path and stops (#4180).
        "▸ read …/lifecycle.rs",
        "● edit …/self_driving_cmd.rs +3 -1",
        "● run cargo test -p stella-core",
        "── turn 14 done · 0:42",
        "receipt $0.11 · 18k tok · det 86% · 4/4 tests · 2 files · 1 memory · ↵ audit",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
    }
}

/// SPEC 6.1: the rule reaches the right edge. A rule that stops short reads as
/// a truncated line rather than as a boundary.
#[test]
fn a_turn_rule_spans_the_full_width() {
    let (head, _, _) = scripted_turn();
    assert_eq!(turn_begin(&head, W).width(), W);
    assert_eq!(turn_end(14, Some("0:42"), W).width(), W);
    // A turn with no measured clock still rules the full width.
    assert_eq!(turn_end(14, None, W).width(), W);
}

/// SPEC 6.2: every event row carries a rail, and the rail is the kind's metal.
#[test]
fn every_event_row_shows_its_rail_in_the_correct_metal() {
    let cases = [
        (EventKind::Read, token::MUTED),
        (
            EventKind::Edit {
                extent: Extent::delta(1, 0),
            },
            token::GOLD,
        ),
        (
            EventKind::Write {
                extent: Extent::added(1),
            },
            token::GOLD,
        ),
        (
            EventKind::Delete {
                extent: Extent::removed(1),
            },
            token::RED,
        ),
        (EventKind::Run, token::GOLD),
        (
            EventKind::Skill {
                trigger: "auto".into(),
                tokens: 1,
            },
            token::SILVER,
        ),
        (EventKind::Memory, token::SILVER),
        (EventKind::Model { tokens_per_sec: 1 }, token::GOLD_BRIGHT),
    ];
    for (kind, expected) in cases {
        let mut event = Event::new(kind.clone(), "x");
        event.collapsed = Some(false);
        event.body = vec![Line::raw("body")];
        event.footer = Some("footer".into());
        for (i, row) in event_rows(&event, W).iter().enumerate() {
            let first = row.spans.first().expect("every row has a rail");
            assert_eq!(
                first.style.fg,
                Some(expected),
                "row {i} of {kind:?} carries the wrong rail metal"
            );
            assert!(
                first.content.contains('│'),
                "row {i} of {kind:?} has no rail glyph"
            );
            assert_eq!(
                first.content.chars().count(),
                RAIL_W,
                "row {i} of {kind:?} must align to the declared rail width, or \
                 the heads of two events sit in different columns"
            );
        }
    }
}

/// SPEC 6.3: reads collapse by default, edits expand.
#[test]
fn reads_collapse_and_edits_expand_by_default() {
    assert!(EventKind::Read.collapses_by_default());
    assert!(
        !EventKind::Edit {
            extent: Extent::delta(1, 1)
        }
        .collapses_by_default()
    );
}

/// The fold toggle changes both the glyph and the row count.
#[test]
fn folding_an_event_hides_its_body_and_flips_the_glyph() {
    let mut event = Event::new(
        EventKind::Edit {
            extent: Extent::delta(1, 0),
        },
        "src/lib.rs",
    );
    event.body = vec![Line::raw("a"), Line::raw("b")];

    let unfolded = event_rows(&event, W);
    assert_eq!(unfolded.len(), 3, "head + two body rows");

    event.collapsed = Some(true);
    let folded = event_rows(&event, W);
    assert_eq!(folded.len(), 1, "head only");

    let head_text = |rows: &[Line<'static>]| {
        rows[0]
            .spans
            .iter()
            .map(|s| s.content.clone())
            .collect::<String>()
    };
    assert!(head_text(&folded).contains(glyph::COLLAPSED));
    assert!(!head_text(&unfolded).contains(glyph::COLLAPSED));
}

/// SPEC 6.1: a consumed steer is visible, so queue-never-blocks has a payoff.
#[test]
fn a_consumed_steer_is_named_on_the_turn_rule() {
    let mut head = scripted_turn().0;
    head.queued_steer = Some("also fix the flake".into());
    let text = turn_begin(&head, W)
        .spans
        .iter()
        .map(|s| s.content.clone())
        .collect::<String>();
    assert!(text.contains("queued: \"also fix the flake\""), "{text}");
}

/// SPEC 6.3: compaction is one dim line and takes no rail.
#[test]
fn compaction_is_one_quiet_line_with_no_rail() {
    let event = Event::new(
        EventKind::Compaction {
            from_tokens: 74_000,
            to_tokens: 69_000,
            evicted: 0,
            deduped: 0,
        },
        String::new(),
    );
    let rows = event_rows(&event, W);
    assert_eq!(rows.len(), 1);
    let text = rows[0]
        .spans
        .iter()
        .map(|s| s.content.clone())
        .collect::<String>();
    assert!(
        text.contains("↓ compacted 74k→69k · 0 evicted · 0 deduped"),
        "{text}"
    );
    assert!(!text.contains('│'), "compaction must not carry a rail");
    assert_eq!(rows[0].spans[0].style.fg, Some(token::DIM));
}

/// prompt.md rule 5 — red scarcity. A healthy turn holds no red cell.
#[test]
fn a_healthy_turn_has_no_red_cells() {
    let (head, events, r) = scripted_turn();
    let (buf, _) = frame(&head, &events, &r);
    let red = (0..buf.area().height)
        .flat_map(|y| (0..buf.area().width).map(move |x| (x, y)))
        .filter(|&(x, y)| buf.cell((x, y)).and_then(|c| c.fg.into()) == Some(token::RED))
        .count();
    // The edit's `-1` removal count is the one legitimate red on a healthy
    // row: a removal is a removal, and SPEC 6.4 makes the sign column carry
    // colour. Nothing else may.
    assert_eq!(red, 2, "only the edit's `-1` may be red");
}

/// A failing suite must not borrow the metal that says "passed".
#[test]
fn a_partial_test_pass_is_not_green() {
    let r = Receipt {
        tests_passed: 3,
        tests_total: 4,
        ..Receipt::default()
    };
    let span = receipt(&r)
        .spans
        .into_iter()
        .find(|s| s.content.contains("3/4 tests"))
        .expect("the tests cell renders");
    assert_eq!(span.style.fg, Some(token::RED));
}

/// prompt.md rule 3: no hex literal outside the theme crate.
#[test]
fn no_hex_literals_in_v2_transcript_code() {
    let src = include_str!("../transcript.rs");
    for (i, line) in src.lines().enumerate() {
        assert!(
            !line.contains("Rgb(") && !line.contains("Color::Rgb"),
            "line {} declares a colour instead of taking a token: {line}",
            i + 1
        );
    }
}
