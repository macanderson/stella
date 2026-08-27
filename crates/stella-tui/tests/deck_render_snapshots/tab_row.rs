// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The tab row as the frame narrows (#5072).
//!
//! A submodule of `deck_render_snapshots` rather than more lines in it: the
//! parent is the file that reaches the 1500-line ceiling first, so a new
//! golden goes here. Every helper comes from the parent, and the goldens are
//! blessed by the same command:
//! `BLESS=1 cargo test -p stella-tui --test deck_render_snapshots`.
//!
//! Four widths, because the row is a ladder and a golden at one width pins one
//! rung. 120 and 100 are the widths #5072 was filed about; 72 is the first
//! width at which the full list and the wordmark cannot both be drawn; 56 is
//! where the full list cannot be drawn at all.

use super::*;

/// The frame height these goldens render at. Short on purpose: the subject is
/// row 0, and a 40-row frame at four widths is 160 lines of committed body to
/// review for a change that can only move one of them. The bands below still
/// render, so a narrow-frame break in the composer or the status bar shows up
/// here too.
const LADDER_H: u16 = 12;

/// The widths, and what the tab row is expected to look like at each.
///
/// `SETTINGS` is the needle throughout because it is the last tab in
/// `DeckTab::ALL` and therefore the first casualty of a row drawn at full
/// length into a frame that cannot hold it — which is exactly what it was
/// before the ladder: at 72 columns the list fit and the *wordmark* was
/// dropped, and at 56 the list itself ran off the edge as `…ISSUES SE`.
const LADDER: [(u16, &str, &str); 4] = [
    (120, "tab_row_120", "SETTINGS"),
    (100, "tab_row_100", "SETTINGS"),
    (72, "tab_row_72", "SET"),
    (56, "tab_row_56", "SET"),
];

/// The whole frame at each rung of the ladder.
#[test]
fn deck_render_snapshots_cover_the_tab_row_width_ladder() {
    let model = fixture_model();
    for (w, name, _) in LADDER {
        let mut ui = ui_for(DeckTab::Files);
        let frame = render_frame(&model, &mut ui, w, LADDER_H);
        assert_golden(
            name,
            &format!("the tab row's width ladder at {w} columns (#5072)"),
            w,
            LADDER_H,
            &frame,
        );
    }
}

/// The witness for #5072: at every width on the ladder the row names all nine
/// tabs and the wordmark holds the right edge.
///
/// Before the ladder both failed, at different widths and in different ways.
/// At 72 columns `views::frame::render_chrome_row` found no room for the list
/// and the mark together and dropped the mark, per its own rule — correct for
/// a frame that cannot shorten its left side, wrong for the tab row, which
/// can. At 56 the list was drawn at full length into a frame nine columns too
/// narrow, so `SETTINGS` was clipped mid-word by the terminal.
///
/// A named assertion beside the goldens for the reason
/// `the_help_overlay_carries_the_metrics_spec_5_sends_behind_it` gives: a
/// golden pins the whole frame, so a row that stops rendering moves it and the
/// reviewer's job becomes noticing an absence inside a hundred-line diff.
#[test]
fn every_tab_survives_the_width_ladder_and_so_does_the_wordmark() {
    let model = fixture_model();
    for (w, _, settings) in LADDER {
        let mut ui = ui_for(DeckTab::Files);
        let frame = render_frame(&model, &mut ui, w, LADDER_H);
        let row = frame.lines().next().unwrap_or_default().to_string();

        assert!(
            row.contains(settings),
            "at {w} columns SETTINGS is not named as {settings:?}: {row}"
        );
        // Uncut, not merely present: a clipped `SETTINGS` still contains
        // `SET`, so the row has to end with the mark rather than with the
        // ragged tail of a name.
        assert!(
            row.trim_end().ends_with("stella*"),
            "at {w} columns the wordmark was dropped: {row}"
        );
        // …and every other tab is identifiable too — in full above the
        // abbreviating rung, by its three-letter form below it.
        for tab in DeckTab::ALL {
            let needle = if settings.len() == 3 {
                &tab.title()[..3]
            } else {
                tab.title()
            };
            assert!(
                row.contains(needle),
                "at {w} columns {} is not named as {needle:?}: {row}",
                tab.title()
            );
        }
    }
}
