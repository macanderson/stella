//! Witness: the run progress bar's fill must be the brand hue.
//!
//! The bar is the deck's largest continuous run of colour, so it is the first
//! thing to fall out of sync with a recolour — twice now it has been the last
//! surface still wearing a retired palette. This test renders a live (Running)
//! bar through the public `progress::render` entry point and asserts every
//! painted fill cell carries the brand accent, which pins the fill to whatever
//! `theme::BRAND_STOPS` currently is rather than to a hue this file names.
//!
//! Deliberately *not* a hue predicate. The previous version asserted `r > b`
//! ("gold is warm"), and inverting that by hand on every recolour is exactly
//! how an assertion drifts away from the palette it is meant to guard. The
//! gradient interpolates between the two brand stops, so a fill cell is
//! correct iff it lies on the segment between them.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

use stella_protocol::{AgentEvent, StageKind};
use stella_tui::theme::{self, ColorMode};
use stella_tui::{AgentMeta, DeckUi, Inbound, WorkspaceModel};

/// A workspace with one lead agent mid-run (Execute stage → ~50% fill), so the
/// bar is in its `Running` state and paints a substantial run of fill cells.
fn running_model() -> WorkspaceModel {
    let mut m = WorkspaceModel::new();
    m.now_ms = 10_000;
    m.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
    m.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Stage {
            name: StageKind::Execute,
        },
    });
    m
}

#[test]
fn progress_bar_fill_rides_the_brand_gradient() {
    let model = running_model();

    // Truecolor is the one mode where the per-cell fill color is emitted
    // verbatim (lesser terminals collapse to a single solid token); freeze
    // motion so the frame is deterministic.
    let ui = DeckUi {
        focused: 0,
        color_mode: ColorMode::Truecolor,
        no_anim: true,
        ..DeckUi::default()
    };

    let width: u16 = 60;
    let area = Rect::new(0, 0, width, 1);
    let mut buf = Buffer::empty(area);

    stella_tui::progress::render(&model, &ui, area, &mut buf);

    // Scan the row for the fill glyph. Only the determinate track paints `█`;
    // labels/telemetry/notches use other glyphs, so this isolates the fill.
    let mut fill_cells = 0usize;
    for x in 0..width {
        let Some(cell) = buf.cell((x, 0)) else {
            continue;
        };
        if cell.symbol() != "█" {
            continue;
        }
        fill_cells += 1;
        let Color::Rgb(r, g, b) = cell.fg else {
            panic!("fill cell at x={x} is not an RGB brand color: {:?}", cell.fg);
        };
        let (Color::Rgb(dr, dg, db), Color::Rgb(br, bg, bb)) =
            (theme::BRAND_STOPS[0], theme::BRAND_STOPS[1])
        else {
            panic!("the brand stops must be truecolor tokens");
        };

        // A fill cell is `brand_gradient(t)`, optionally lifted toward white by
        // the shimmer band (`theme::lighten`). Both are per-channel linear
        // blends, which gives two invariants that survive either operation:
        //
        //  1. No channel falls below the dimmer stop's value for it. Blending
        //     between the stops stays inside their interval, and lightening
        //     only moves toward 255.
        //  2. The channels keep the order they hold in the stops. Lifting x and
        //     y toward the same target by the same factor scales (x − y) by
        //     (1 − a), which cannot flip its sign — it can only collapse it to
        //     zero at pure white.
        //
        // Both are read off `BRAND_STOPS`, so the next recolour re-derives them
        // instead of needing this file edited. That is the whole point: the
        // version this replaced hardcoded "gold is warm, so r > b", which would
        // have had to be inverted by hand — and an assertion maintained by hand
        // against a palette is one that eventually disagrees with it.
        for (got, lo, chan) in [(r, dr.min(br), "r"), (g, dg.min(bg), "g"), (b, db.min(bb), "b")] {
            assert!(
                i32::from(got) >= i32::from(lo) - 1,
                "fill cell at x={x} is dimmer than the brand gradient: {:?} \
                 ({chan}={got} below {lo})",
                cell.fg
            );
        }
        let rank = |x: u8, y: u8| (i32::from(x) - i32::from(y)).signum();
        for (lhs, rhs, s0, s1, pair) in [
            (r, g, rank(dr, dg), rank(br, bg), "r/g"),
            (g, b, rank(dg, db), rank(bg, bb), "g/b"),
        ] {
            // Only assert where the two stops agree on the ordering; a gradient
            // whose stops disagree has no ordering to preserve.
            if s0 != s1 || s0 == 0 {
                continue;
            }
            let got = rank(lhs, rhs);
            assert!(
                got == s0 || got == 0,
                "fill cell at x={x} inverts the brand gradient's {pair} ordering: {:?}",
                cell.fg
            );
        }
    }

    assert!(
        fill_cells > 0,
        "expected the running progress bar to paint fill cells to inspect"
    );
}
