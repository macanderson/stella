// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A JSON tool result is read as fields on every surface (#4340).
//!
//! The deck had the projection and the export surfaces did not, so the same
//! result was a field table in one place and re-indented, syntax-coloured JSON
//! in the other two. Those two shapes have different line counts, so the
//! divergence was not only how the result read but how much of it a reader was
//! shown.
//!
//! Kept beside `json_reindent`, whose subject is the other half of the same
//! decision: what happens to a body that opens on a brace and does not parse.

use super::{bash, output, rendered};
use crate::digest;
use crate::model::Status;
use crate::syntax;

/// **Witness (#4340).** A JSON tool result reaches the export and the
/// Observatory as fields, the way the Command Deck has read it since the
/// projection was written.
///
/// The two surfaces used to re-indent and syntax-colour the same bytes as
/// JSON, so one result had two readings — and, because a pretty-printed object
/// and its field table have different line counts, two answers to "how much of
/// this am I seeing". Neither renderer needed an arm of its own to fix it:
/// both derive their paint from the *folded* body, and a body of fields no
/// longer opens on a brace.
#[test]
fn a_json_result_reaches_both_export_renderers_as_fields() {
    let call = bash(
        "gh api /repos/oxagen/stella",
        &[r#"{"name": "stella", "stars": 12, "topics": ["rust", "agent"]}"#],
        Status::Ok,
    );
    let fold = digest::fold_output(&call.output, &call.header_object);
    assert_eq!(
        fold.body,
        vec![
            "name stella".to_string(),
            "stars 12".to_string(),
            "topics · 2 items".to_string(),
            "  - rust".to_string(),
            "  - agent".to_string(),
        ],
        "the fold measured JSON rather than fields"
    );
    assert_eq!(
        syntax::lines_body_paint(call.read_path(), &fold.body),
        syntax::BodyPaint::default(),
        "a body of fields must not be lexed as anything — there is no JSON left in it"
    );

    let (grid, html) = rendered(call);
    for (surface, drawn) in [("grid", &grid), ("html", &html)] {
        assert!(
            drawn.contains("topics · 2 items"),
            "{surface} did not draw the field row:\n{drawn}"
        );
        // Asserted as the exact quoted spellings rather than by sweeping for a
        // brace: both surfaces carry punctuation of their own — the grid's box
        // rules and clock, the page's attributes — and a character sweep over
        // the whole render would be answering a different question.
        for forbidden in ["\"topics\"", "{\"name\"", "\"stars\": 12"] {
            assert!(
                !drawn.contains(forbidden),
                "{surface} still shows the wire ({forbidden}):\n{drawn}"
            );
        }
    }
}

/// A body that opens on a brace but does not parse keeps the character
/// re-layout, which needs no grammar. Dropping it there would put the
/// eight-thousand-column single-line blob back for exactly the truncated
/// responses most likely to produce one.
#[test]
fn a_malformed_json_body_still_gets_its_re_layout() {
    let members: String = (1..=40).map(|i| format!("\"k{i}\":{i},")).collect();
    let truncated = format!("{{{members}\"end\":");
    let fold = digest::fold_output(&output(&[truncated.as_str()]), "gh api");
    assert!(
        fold.body.len() > 1,
        "a truncated response is back to one line: {fold:?}"
    );
    assert!(
        fold.body[0].starts_with('{'),
        "the re-layout is a character one and keeps the brace: {:?}",
        fold.body[0]
    );
}
