//! Label-value rendering for the deck's grid views in accessible mode.
//!
//! A table is a compression scheme that only works on an eye. `$0.05` under a
//! `Cost` header two rows up is a labelled value to a reader who can see the
//! header and the column; read aloud it is the number five, unqualified, in a
//! stream of other unqualified numbers — and the alignment that made it mean
//! "cost" is whitespace, which is exactly what a screen reader collapses.
//!
//! So in accessible mode each grid view renders **one row per record, with
//! every value carrying its own label** ([`record_line`]). Nothing is dropped:
//! the same fields, in the same order, saying what they are.
//!
//! One row per record is not a stylistic preference either — the deck's
//! scrolling is line-exact (L-T4), and every one of these views reports its
//! row count to the key handler through `DeckUi::metrics`. A record that
//! wrapped to two rows would silently break ↑/↓, so a long row is truncated
//! with an ellipsis rather than wrapped, which is what the tables already did
//! at their column boundaries.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme;

/// The separator between one labelled value and the next.
///
/// Spoken, ` · ` is a pause rather than a word, which is what a field boundary
/// should be. It is also what the deck already uses between clauses everywhere
/// else, so a linearised row reads like the rest of the product.
const FIELD_SEP: &str = " · ";

/// One record as a single row: an identity, then `label value` pairs.
///
/// `identity` is the part that names the record — the caret, the status glyph,
/// the id — and keeps whatever styling the view gave it, because that is the
/// part a sighted user of accessible mode still scans. Fields whose value is
/// empty are dropped rather than rendered as `label` with nothing after it: a
/// spoken label with no value is a question, not an answer.
///
/// The row is clipped to `width`, never wrapped (see the module docs).
pub(crate) fn record_line(
    identity: Vec<Span<'static>>,
    fields: &[(&str, String)],
    width: usize,
) -> Line<'static> {
    let mut spans = identity;
    for (label, value) in fields {
        if value.trim().is_empty() {
            continue;
        }
        spans.push(Span::styled(
            format!("{FIELD_SEP}{label} "),
            Style::new().fg(theme::TEXT_TERTIARY),
        ));
        spans.push(Span::styled(value.clone(), theme::body()));
    }
    Line::from(truncate_spans(spans, width))
}

/// The identity half of a [`record_line`]: a selection caret and a name.
///
/// The caret is two columns whether or not the row is selected, so a reader
/// walking the list hears the same shape every time and the rows stay
/// column-aligned for whoever is also looking at them.
pub(crate) fn identity(
    name: impl Into<String>,
    selected: bool,
    color: Color,
) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            if selected { "> " } else { "  " },
            Style::new().fg(theme::ACCENT),
        ),
        Span::styled(
            name.into(),
            Style::new().fg(color).add_modifier(Modifier::BOLD),
        ),
    ]
}

/// Clip a span run to `width` display columns, marking a clip with an ellipsis.
///
/// Char-counted rather than measured with `unicode-width`, matching what the
/// grid views' own truncation already does; every label here is ASCII and the
/// values are the same strings the tables were clipping before.
pub(crate) fn truncate_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if total <= width {
        return spans;
    }
    if width == 0 {
        return Vec::new();
    }
    let budget = width - 1;
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let len = span.content.chars().count();
        if used + len <= budget {
            used += len;
            out.push(span);
            continue;
        }
        let take = budget - used;
        if take > 0 {
            let head: String = span.content.chars().take(take).collect();
            out.push(Span::styled(head, span.style));
        }
        break;
    }
    out.push(Span::styled("…", Style::new().fg(theme::TEXT_TERTIARY)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn every_value_carries_its_own_label() {
        let line = record_line(
            identity("lead", true, theme::ACCENT),
            &[
                ("status", "running".into()),
                ("cost", "$0.05".into()),
                ("cpu", "3%".into()),
            ],
            200,
        );
        assert_eq!(text(&line), "> lead · status running · cost $0.05 · cpu 3%");
    }

    #[test]
    fn an_absent_value_drops_its_label_instead_of_asking_a_question() {
        let line = record_line(
            identity("lead", false, theme::ACCENT),
            &[("status", "running".into()), ("warmth", String::new())],
            200,
        );
        assert_eq!(text(&line), "  lead · status running");
    }

    #[test]
    fn a_long_row_is_clipped_rather_than_wrapped() {
        // Line-exact scroll (L-T4) is one visual row per record; a wrap would
        // silently desynchronise ↑/↓ from the list it is moving through.
        let line = record_line(
            identity("lead", false, theme::ACCENT),
            &[("goal", "a".repeat(200))],
            20,
        );
        let rendered = text(&line);
        assert_eq!(rendered.chars().count(), 20, "{rendered:?}");
        assert!(rendered.ends_with('…'), "{rendered:?}");
    }

    #[test]
    fn a_row_that_fits_is_left_exactly_as_it_was() {
        let spans = vec![Span::raw("abc".to_string())];
        assert_eq!(truncate_spans(spans.clone(), 3), spans);
    }
}
