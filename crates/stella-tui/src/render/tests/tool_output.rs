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
    let body = (1..=40)
        .map(|i| format!("line {i}\n"))
        .collect::<String>();
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
