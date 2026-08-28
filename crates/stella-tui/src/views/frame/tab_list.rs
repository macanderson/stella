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
    rung_spans(active, chosen_rung(active, budget))
}

/// The rung [`spans`] renders into `budget`: the widest that fits, or
/// [`Rung::Solo`] when nothing does.
fn chosen_rung(active: DeckTab, budget: usize) -> Rung {
    RUNGS
        .iter()
        .copied()
        .find(|rung| width(&rung_spans(active, *rung)) <= budget)
        .unwrap_or(Rung::Solo)
}

/// The tab under `column` of the row [`spans`] renders into `budget`, or
/// `None` for a column on the air between titles or past the list — the hit
/// test for a click on the tab row. Derived from the same cells and the same
/// rung choice as [`spans`], so the two cannot disagree about where a title
/// sits.
pub(super) fn hit(active: DeckTab, budget: usize, column: usize) -> Option<DeckTab> {
    let mut x = 0;
    for (tab, span) in rung_cells(active, chosen_rung(active, budget)) {
        let w = span.width();
        if (x..x + w).contains(&column) {
            return tab;
        }
        x += w;
    }
    None
}

/// The columns a rendering occupies.
fn width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// The active tab in gold with a cell of air either side, the rest muted.
fn rung_spans(active: DeckTab, rung: Rung) -> Vec<Span<'static>> {
    rung_cells(active, rung)
        .into_iter()
        .map(|(_, span)| span)
        .collect()
}

/// One rendering as cells: each span with the tab a click on it lands on —
/// `None` for the air between titles and for [`Rung::Solo`]'s `4/9` note.
/// [`rung_spans`] and [`hit`] both derive from this, which is what keeps the
/// pixels drawn and the columns hit-tested the same list.
fn rung_cells(active: DeckTab, rung: Rung) -> Vec<(Option<DeckTab>, Span<'static>)> {
    let muted = Style::new().fg(token::MUTED);
    let dim = Style::new().fg(token::DIM);
    let lit = Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD);
    let mut cells = vec![(None, Span::raw(" "))];
    if rung == Rung::Solo {
        cells.push((
            Some(active),
            Span::styled(format!("  {}  ", active.title()), lit),
        ));
        cells.push((
            None,
            Span::styled(
                format!("{}/{}", active.index() + 1, DeckTab::ALL.len()),
                dim,
            ),
        ));
        return cells;
    }
    for (i, tab) in DeckTab::ALL.iter().enumerate() {
        if i > 0 {
            cells.push((None, Span::raw(" ")));
        }
        if *tab == active {
            cells.push((
                Some(*tab),
                Span::styled(format!("  {}  ", tab.title()), lit),
            ));
        } else if rung == Rung::Short {
            cells.push((
                Some(*tab),
                Span::styled(short_title(*tab).to_string(), muted),
            ));
        } else {
            cells.push((Some(*tab), Span::styled(tab.title(), muted)));
        }
    }
    cells
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

    /// [`Rung::Solo`] is reachable from a real frame width, not only by
    /// handing [`spans`] a budget by hand.
    ///
    /// The goldens stop at 56 columns, where [`Rung::Short`] still fits, so
    /// without this the narrowest rung would be exercised only by a caller
    /// that does not exist — and a rung nothing reaches is a rung that rots.
    /// The budget comes from `right_edge_reserve`, the shipped arithmetic, so
    /// this is the frame width a terminal would actually have to be.
    #[test]
    fn the_narrowest_rung_is_reachable_from_a_real_frame_width() {
        let reserve = crate::views::frame::right_edge_reserve(&[]);
        // One column under the *narrowest* `Short` rendering, so every tab is
        // past its own threshold rather than only the widest-titled one.
        let narrowest_short = DeckTab::ALL
            .iter()
            .map(|t| width(&rung_spans(*t, Rung::Short)))
            .min()
            .expect("nine tabs");
        let frame = narrowest_short + reserve - 1;
        assert!(
            frame < 56,
            "Solo sits at {frame} columns, which the 56-column golden would already cover"
        );

        for tab in DeckTab::ALL {
            let row: String = spans(tab, frame - reserve)
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert_eq!(
                spans(tab, frame - reserve),
                rung_spans(tab, Rung::Solo),
                "{tab:?} is not on the last rung at {frame} columns: {row}"
            );
            assert!(row.contains(tab.title()), "{row}");
            assert!(row.contains(&format!("/{}", DeckTab::ALL.len())), "{row}");
        }
    }

    /// A click on a full title lands on that tab, and the air between titles
    /// lands on nothing. The expected column comes from the rendered text, so
    /// this holds [`hit`] to what a reader actually sees, not to the cells it
    /// shares with the renderer.
    #[test]
    fn a_click_on_a_full_title_lands_on_its_tab() {
        for active in DeckTab::ALL {
            let row = text(active, 65);
            for tab in DeckTab::ALL {
                let col = row.find(tab.title()).expect("title on the row");
                assert_eq!(hit(active, 65, col), Some(tab), "{tab:?} in {row}");
            }
        }
        // The leading column of air, and any column past the list.
        assert_eq!(hit(DeckTab::Session, 65, 0), None);
        assert_eq!(hit(DeckTab::Session, 65, 200), None);
    }

    /// The abbreviated rung is clickable on the same terms: a three-letter
    /// form selects its tab.
    #[test]
    fn a_click_on_a_short_title_lands_on_its_tab() {
        let row = text(DeckTab::Graph, 50);
        for tab in DeckTab::ALL {
            let needle = if tab == DeckTab::Graph {
                tab.title()
            } else {
                short_title(tab)
            };
            let col = row.find(needle).expect("form on the row");
            assert_eq!(hit(DeckTab::Graph, 50, col), Some(tab), "{tab:?} in {row}");
        }
    }

    /// [`Rung::Solo`] names one tab, so a click selects the tab already
    /// selected — and the `4/9` position note selects nothing.
    #[test]
    fn the_solo_rung_hits_only_the_active_tab() {
        let row = text(DeckTab::Settings, 17);
        let title = row.find("SETTINGS").expect("title on the row");
        assert_eq!(hit(DeckTab::Settings, 17, title), Some(DeckTab::Settings));
        let note = row.find("9/9").expect("note on the row");
        assert_eq!(hit(DeckTab::Settings, 17, note), None);
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
