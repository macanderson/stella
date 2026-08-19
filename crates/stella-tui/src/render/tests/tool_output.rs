// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A tool result's *output* in the collapsed fold.
//!
//! The fold used to show a successful call one line of output and a size chip,
//! which for every tool whose output is the answer — `search`, `read_file`,
//! `get_state` — left the answer behind a keystroke. These pin the two halves
//! of the fix: a bounded preview is shown, and a JSON body is colored.

use super::*;
use crate::syntax;
use crate::theme;

/// A successful, non-mutating tool result carrying `body`.
fn result(body: &str) -> TranscriptEntry {
    TranscriptEntry::ToolResult {
        call_id: "c1".into(),
        name: "search".into(),
        ok: true,
        summary: "ok".into(),
        full: body.into(),
        duration_ms: 7,
        speculated: false,
        diff: None,
    }
}

fn collapsed(entry: &TranscriptEntry) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    entry_lines(entry, &[], false, false, false, 120, &mut out);
    out
}

fn text_of(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_successful_result_previews_its_output_rather_than_one_line() {
    let body = "alpha\nbravo\ncharlie\ndelta\necho\nfoxtrot\ngolf\nhotel";
    let rendered = text_of(&collapsed(&result(body)));

    // The witness: on the old one-line budget only the first payload line
    // reached the fold. A preview shows the lines after it too.
    for line in ["bravo", "charlie", "delta"] {
        assert!(
            rendered.contains(line),
            "collapsed preview dropped {line:?}:\n{rendered}"
        );
    }
}

#[test]
fn a_truncated_success_says_how_much_is_hidden_and_which_key_shows_it() {
    let body = (1..=40).map(|i| format!("line {i}\n")).collect::<String>();
    let rendered = text_of(&collapsed(&result(&body)));

    // Previously failure-only: a success stated its size in the metric column
    // with no affordance beside it.
    assert!(
        rendered.contains("ctrl+o"),
        "no reveal affordance on a truncated success:\n{rendered}"
    );
    assert!(
        rendered.contains("lines · ctrl+o"),
        "hidden count not stated with the affordance:\n{rendered}"
    );
}

#[test]
fn a_short_success_claims_nothing_is_hidden() {
    let rendered = text_of(&collapsed(&result("only line")));
    assert!(
        !rendered.contains("ctrl+o"),
        "offered to reveal output that is already fully shown:\n{rendered}"
    );
}

/// Colors are asserted through [`syntax::tok_style`] rather than literal theme
/// constants, so a palette change moves the test with the theme instead of
/// breaking it.
fn colors(lines: &[Line<'static>]) -> Vec<(String, Option<ratatui::style::Color>)> {
    lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| (s.content.to_string(), s.style.fg))
        .collect()
}

#[test]
fn a_json_result_body_is_syntax_coloured() {
    let body = "{\n  \"query\": \"needle\",\n  \"hits\": 3\n}";
    let spans = colors(&collapsed(&result(body)));

    let key = syntax::tok_style(syntax::Tok::Keyword).fg;
    let string = syntax::tok_style(syntax::Tok::Str).fg;
    let number = syntax::tok_style(syntax::Tok::Number).fg;

    assert!(
        spans.iter().any(|(t, c)| t == "\"query\"" && *c == key),
        "object key not coloured as structure: {spans:?}"
    );
    assert!(
        spans.iter().any(|(t, c)| t == "\"needle\"" && *c == string),
        "string value not coloured as a string: {spans:?}"
    );
    assert!(
        spans.iter().any(|(t, c)| t == "3" && *c == number),
        "number value not coloured as a number: {spans:?}"
    );
    // A key and its value must not share a hue, or the object has no shape.
    assert_ne!(key, string, "key and string value are the same colour");
}

#[test]
fn a_plain_text_result_is_not_coloured_as_json() {
    let spans = colors(&collapsed(&result("error: no such file\nat line 3")));
    let key = syntax::tok_style(syntax::Tok::Keyword).fg;
    assert!(
        !spans.iter().any(|(_, c)| *c == key),
        "shell output picked up JSON structure colouring: {spans:?}"
    );
    // It still renders in the body's own tone rather than losing its style.
    assert!(spans.iter().any(|(_, c)| *c == Some(theme::MUTED)));
}

#[test]
fn a_json_preview_starts_at_the_opening_brace() {
    let body = "{\n  \"a\": 1,\n  \"b\": 2\n}";
    let rendered = text_of(&collapsed(&result(body)));
    // `salient_line` skips a preamble to the interesting line; for an object
    // that would cut the shape away from its own contents.
    assert!(
        rendered.contains('{'),
        "JSON preview dropped its opening delimiter:\n{rendered}"
    );
}

/// The deck and the export/Observatory surfaces show the **same** amount of a
/// successful tool result.
///
/// This is the cross-surface half of #3644, and it is a genuine round trip
/// rather than a restatement of a shared constant: the deck folds by taking
/// `OK_PREVIEW` lines from a salient offset in one string, while
/// `stella_transcript::digest::fold_output` folds a `Vec<String>` into a head
/// and possibly a tail. Two implementations, one policy — so sharing
/// `PREVIEW_LINES` makes them agree by construction only if both actually
/// honour it, which is what this counts.
///
/// The bug it pins: the deck previewed six lines while the export previewed
/// three, for the same run. A reader comparing a mailed-around transcript
/// against the live deck saw two different amounts of the same tool's output
/// and had no way to know which was the truncation.
#[test]
fn the_deck_and_the_export_show_the_same_number_of_result_lines() {
    for total in 1..24usize {
        let lines: Vec<String> = (0..total).map(|i| format!("line-{i:03}")).collect();
        let body = lines.join("\n");

        // What the deck draws: body rows are every rendered line that carries
        // one of the payload's own lines. Counting by content rather than by
        // row index keeps this independent of the chrome around them (the
        // metric row, the `⋯ N · ctrl+o` affordance, the trailing gap).
        let rendered = text_of(&collapsed(&result(&body)));
        let deck_shown = lines
            .iter()
            .filter(|l| rendered.contains(l.as_str()))
            .count();

        // What the export and the Observatory draw, through the shared policy.
        let fold = stella_transcript::digest::fold_output(
            &stella_transcript::Output {
                lines: lines.clone(),
                clipped: 0,
            },
            "search",
        );
        let export_shown = fold.head.len() + fold.tail.len();

        assert_eq!(
            deck_shown, export_shown,
            "a {total}-line successful result: the deck shows {deck_shown} lines, \
             the export shows {export_shown} — the surfaces have diverged again"
        );
        assert_eq!(
            export_shown,
            total.min(stella_transcript::digest::PREVIEW_LINES),
            "a {total}-line result must show the shared preview budget"
        );
    }
}

/// The JSON lexer the deck colours with is the one the export surfaces use.
///
/// Not a re-test of the lexer — `stella-transcript` owns that — but of the
/// wiring: the deck's `Lang::Json` arm must still resolve to the shared
/// function, because the whole point of moving it down was that one lexer
/// serves all three surfaces. A second copy reappearing here would pass every
/// existing colouring test while restoring exactly the drift #3644 closed.
#[test]
fn the_decks_json_colouring_comes_from_the_shared_lexer() {
    let line = r#"  "path": "src/main.rs", "count": 12, "ok": true"#;
    let deck: Vec<(String, Option<syntax::Tok>)> = syntax::tokenize(line, syntax::Lang::Json);
    assert_eq!(
        deck,
        stella_transcript::syntax::json_runs(line),
        "the deck is no longer lexing JSON with the shared lexer"
    );
    // And the contract that makes sharing safe in the first place.
    assert_eq!(
        deck.iter().map(|(t, _)| t.as_str()).collect::<String>(),
        line,
        "the lexer stopped being lossless"
    );
}

/// The parity above survives a *salient* line, wherever it sits.
///
/// The deck does not simply take the first `PREVIEW_LINES` lines: it anchors on
/// `salient_line`, the first line worth reading. That offset is the second way
/// the two surfaces can disagree about "how much do I see", and the first
/// version of this fix closed only the first: with the salient line near the
/// end of the output there were fewer than `PREVIEW_LINES` lines left to take,
/// so the deck showed one line where the export showed six.
///
/// Sweeping the marker across every position is what makes this a test rather
/// than an anecdote — the failure only appears in the last few.
#[test]
fn a_salient_line_near_the_end_still_shows_the_full_preview() {
    let total = 12usize;
    for marker_at in 0..total {
        let lines: Vec<String> = (0..total)
            .map(|i| {
                if i == marker_at {
                    "error: the thing failed".to_string()
                } else {
                    format!("line-{i:03}")
                }
            })
            .collect();
        let rendered = text_of(&collapsed(&result(&lines.join("\n"))));
        let deck_shown = lines
            .iter()
            .filter(|l| rendered.contains(l.as_str()))
            .count();

        assert_eq!(
            deck_shown,
            stella_transcript::digest::PREVIEW_LINES,
            "a salient line at index {marker_at} of {total} starved the preview \
             to {deck_shown} lines"
        );
        assert!(
            rendered.contains("error: the thing failed"),
            "the salient line at index {marker_at} fell outside the window it anchors"
        );
    }
}
