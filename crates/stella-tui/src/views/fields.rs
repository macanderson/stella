// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The deck's ratatui adapter over the shared field projection.
//!
//! The projection itself — a JSON object read as fields rather than shown as
//! syntax — lives in [`stella_transcript::fields`], which is where the export
//! and the Observatory can reach it (#4340). What is left here is the half
//! that genuinely needs ratatui and the terminal theme: turning a
//! [`FieldTone`] into a `Style`.
//!
//! That is the same line `stella-transcript::syntax` draws for the lexers —
//! `tok_style` stayed in the deck while `tokenize` went down — and it is drawn
//! for the same reason. The deck read a `task_create` result as fields while
//! `stella export` printed the same bytes as re-indented JSON: one change, two
//! readings, which is the drift `stella-transcript` exists to end.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;
use stella_transcript::fields::{FieldRow, FieldTone};
use stella_tui_theme::token;

pub use stella_transcript::fields::{headline, parse_document};

/// The deck's palette for one span of a field row.
///
/// `Plain` keeps the terminal's own foreground rather than taking a token:
/// indentation and the gap between a key and its value are structure, and
/// giving them a colour would let a run of spaces read as content.
fn style(tone: FieldTone) -> Option<Style> {
    match tone {
        FieldTone::Plain => None,
        FieldTone::Key => Some(Style::new().fg(token::MUTED)),
        FieldTone::Value => Some(Style::new().fg(token::TEXT)),
        FieldTone::Note => Some(Style::new().fg(token::DIM)),
    }
}

/// One shared row as a ratatui line.
fn line(row: &FieldRow) -> Line<'static> {
    Line::from(
        row.spans
            .iter()
            .map(|span| match style(span.tone) {
                Some(style) => Span::styled(span.text.clone(), style),
                None => Span::raw(span.text.clone()),
            })
            .collect::<Vec<_>>(),
    )
}

/// The whole object as a field table, painted for the deck.
///
/// Row for row the same table the export folds and the Observatory emits —
/// [`stella_transcript::fields::rows`] is the one producer, and this only
/// decides the hue.
#[must_use]
pub fn rows(value: &Value) -> Vec<Line<'static>> {
    stella_transcript::fields::rows(value)
        .iter()
        .map(line)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The adapter is row-for-row the shared projection. A deck that grew its
    /// own row rule would put back exactly the divergence #4340 closed, and it
    /// would pass every colouring test on the way.
    #[test]
    fn the_decks_rows_are_the_shared_projections_rows() {
        let value = json!({
            "tasks": [{"subject": "wire the digest", "status": "pending"}],
            "cost": {"usd": 0.04},
            "note": "first\nsecond",
        });
        let painted = rows(&value);
        let shared = stella_transcript::fields::text_rows(&value);
        assert_eq!(painted.len(), shared.len());
        for (drawn, expected) in painted.iter().zip(&shared) {
            let text: String = drawn.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(&text, expected);
        }
    }

    /// A key is muted, a value takes the text tone, and a container's count is
    /// dim — the one thing this module decides, pinned so a palette change is
    /// a deliberate one.
    #[test]
    fn a_key_a_value_and_a_count_take_their_own_tones() {
        let painted = rows(&json!({"path": "a.rs", "steps": ["read"]}));
        let styled: Vec<Vec<(String, Style)>> = painted
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| (s.content.to_string(), s.style))
                    .collect()
            })
            .collect();
        let find = |needle: &str| {
            styled
                .iter()
                .flatten()
                .find(|(text, _)| text == needle)
                .map(|(_, style)| style.fg)
                .unwrap_or_else(|| panic!("no span reads {needle:?}: {styled:?}"))
        };
        assert_eq!(find("path"), Some(token::MUTED));
        assert_eq!(find("a.rs"), Some(token::TEXT));
        assert_eq!(find(" · 1 item"), Some(token::DIM));
    }
}
