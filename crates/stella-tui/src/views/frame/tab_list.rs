// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The tab row's width ladder: what the nine-tab list gives up, and in what
//! order, as the frame narrows (#5072).
//!
//! The list at full length costs 65 columns and the wordmark eight more, so
//! under 74 something has to go. The rule is that **the list yields and the
//! mark never does**: [`spans`] is handed the columns left once the mark is
//! paid for and returns the widest rendering that fits inside them, so
//! [`super::render_chrome_row`]'s own drop rule — which other frames still
//! need — is never reached from this row.

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use stella_tui_theme::token;

use crate::deck::DeckTab;

/// One rendering of the tab list, in the order [`spans`] tries them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rung {
    /// Every title in full: 65 columns.
    Full,
    /// Inactive titles to their three-letter form, the active tab in full, so
    /// the one name a reader needs right now is never the abbreviated one: 45
    /// columns at the widest active tab.
    Short,
    /// The active tab alone, with its place in the list: 17 columns at the
    /// widest. `4/9` stays because it is the fact a reader loses when the
    /// other eight names go — this row is the only place the deck says how
    /// many tabs there are, and `tab`/`⇧tab` walk them drawn or not.
    Solo,
}

/// The rungs, widest first. [`spans`] takes the first one that fits.
const RUNGS: [Rung; 3] = [Rung::Full, Rung::Short, Rung::Solo];

/// The tab list rendered into `budget` columns: the widest rung that fits, or
/// [`Rung::Solo`] when nothing does.
///
/// `budget` is what [`super::right_edge_reserve`] leaves — never the whole
/// row, which is what makes the mark survive every width.
pub(super) fn spans(active: DeckTab, budget: usize) -> Vec<Span<'static>> {
    RUNGS
        .iter()
        .map(|rung| rung_spans(active, *rung))
        .find(|spans| width(spans) <= budget)
        .unwrap_or_else(|| rung_spans(active, Rung::Solo))
}

/// The columns a rendering occupies.
fn width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// The active tab in gold with a cell of air either side, the rest muted.
fn rung_spans(active: DeckTab, rung: Rung) -> Vec<Span<'static>> {
    let muted = Style::new().fg(token::MUTED);
    let dim = Style::new().fg(token::DIM);
    let lit = Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::raw(" ")];
    if rung == Rung::Solo {
        spans.push(Span::styled(format!("  {}  ", active.title()), lit));
        spans.push(Span::styled(
            format!("{}/{}", active.index() + 1, DeckTab::ALL.len()),
            dim,
        ));
        return spans;
    }
    for (i, tab) in DeckTab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        if *tab == active {
            spans.push(Span::styled(format!("  {}  ", tab.title()), lit));
        } else if rung == Rung::Short {
            spans.push(Span::styled(short_title(*tab).to_string(), muted));
        } else {
            spans.push(Span::styled(tab.title(), muted));
        }
    }
    spans
}

/// A tab's three-letter form: the first three letters of [`DeckTab::title`].
///
/// Computed rather than tabled. Nine tabs give nine distinct prefixes (`SES`
/// `AGE` `TRA` `GRA` `FIL` `SKI` `MCP` `ISS` `SET`), which a reader works out
/// once, and a table here would be a second list of tab names to keep in step
/// with the first. `three_letter_forms_stay_distinct` holds the computed form
/// to the property [`Rung::Short`] needs.
///
/// Every title is ASCII and at least three letters, so the slice lands on a
/// character boundary; `get` rather than an index anyway, because a title
/// short enough to make that untrue should render whole, not panic.
fn short_title(tab: DeckTab) -> &'static str {
    let title = tab.title();
    title.get(..3).unwrap_or(title)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(active: DeckTab, budget: usize) -> String {
        spans(active, budget)
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    /// The property [`Rung::Short`] rests on: nine tabs, nine distinct
    /// three-letter forms, so an abbreviated row still names nine things.
    #[test]
    fn three_letter_forms_stay_distinct() {
        let mut forms: Vec<&str> = DeckTab::ALL.iter().map(|t| short_title(*t)).collect();
        let all = forms.clone();
        forms.sort_unstable();
        forms.dedup();
        assert_eq!(forms.len(), DeckTab::ALL.len(), "collision in {all:?}");
        for tab in DeckTab::ALL {
            assert!(
                tab.title().starts_with(short_title(tab)),
                "{} is not a prefix of {}",
                short_title(tab),
                tab.title()
            );
        }
    }

    /// Every rung fits the width its doc comment claims, at every active tab —
    /// arithmetic nobody would otherwise re-do after adding a tab.
    #[test]
    fn each_rung_fits_the_width_it_claims() {
        for tab in DeckTab::ALL {
            assert!(width(&rung_spans(tab, Rung::Full)) <= 65);
            assert!(width(&rung_spans(tab, Rung::Short)) <= 45);
            assert!(width(&rung_spans(tab, Rung::Solo)) <= 17);
        }
    }

    /// The ladder descends a rung at a time, and never returns a row wider
    /// than the columns it was given while a narrower rung exists.
    #[test]
    fn the_ladder_takes_the_widest_rung_that_fits() {
        for tab in DeckTab::ALL {
            assert_eq!(spans(tab, 65), rung_spans(tab, Rung::Full));
            assert_eq!(spans(tab, 64), rung_spans(tab, Rung::Short));
            assert_eq!(spans(tab, 45), rung_spans(tab, Rung::Short));
            assert_eq!(spans(tab, 17), rung_spans(tab, Rung::Solo));
            for budget in [17, 45, 64, 65, 200] {
                assert!(
                    width(&spans(tab, budget)) <= budget,
                    "{tab:?} overflows {budget}"
                );
            }
        }
    }

    /// Below the last rung the row is still the active tab and its place —
    /// clipped by the terminal, never by a rung that gave up naming anything.
    #[test]
    fn a_frame_under_the_last_rung_still_names_where_it_is() {
        let row = text(DeckTab::Settings, 4);
        assert!(row.contains("SETTINGS"), "{row}");
        assert!(row.contains("9/9"), "{row}");
    }

    /// The abbreviated rung still names all nine tabs, and the active one in
    /// full: the rung exists to keep the list readable, not to shorten it.
    #[test]
    fn the_short_rung_names_every_tab_and_spells_the_active_one() {
        let row = text(DeckTab::Graph, 50);
        assert!(row.contains("  GRAPH  "), "{row}");
        for tab in DeckTab::ALL.iter().filter(|t| **t != DeckTab::Graph) {
            assert!(row.contains(short_title(*tab)), "{} in {row}", tab.title());
        }
        assert!(!row.contains("SETTINGS"), "still abbreviated: {row}");
    }
}
