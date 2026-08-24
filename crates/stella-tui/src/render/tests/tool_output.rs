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
    result_for(None, body)
}

/// [`result`] for a call that targeted `path` — a file read, whose body is
/// that file's source and whose extension is the only evidence of its
/// language.
fn read_result(path: &str, body: &str) -> TranscriptEntry {
    result_for(Some(path), body)
}

/// A result for an arbitrary tool, so a test can name the one it means.
fn tool_result(name: &str, ok: bool, path: Option<&str>, body: &str) -> TranscriptEntry {
    TranscriptEntry::ToolResult {
        call_id: "c1".into(),
        name: name.into(),
        path: path.map(str::to_owned),
        ok,
        summary: "ok".into(),
        full: body.into(),
        duration_ms: 7,
        speculated: false,
        diff: Vec::new(),
        read_size: None,
    }
}

fn result_for(path: Option<&str>, body: &str) -> TranscriptEntry {
    TranscriptEntry::ToolResult {
        call_id: "c1".into(),
        name: if path.is_some() {
            "read_file"
        } else {
            "search"
        }
        .into(),
        path: path.map(str::to_owned),
        ok: true,
        summary: "ok".into(),
        full: body.into(),
        duration_ms: 7,
        speculated: false,
        diff: Vec::new(),
        read_size: None,
    }
}

/// One line of `read_file`'s output: a right-aligned line number, a tab, the
/// source. Built from the emitter's own format string
/// (`stella-tools`' `{line_num:>6}\t{line}`) so a test cannot drift from the
/// shape it is meant to be reading.
fn numbered(first: usize, source: &[&str]) -> String {
    source
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:>6}\t{l}\n", first + i))
        .collect()
}

/// The same rows a reader gets after `ctrl+o`.
///
/// Three of the tests below moved here from [`collapsed`]. A `read_file`
/// result no longer renders a body while collapsed — SPEC 2 folds what did not
/// change, and a read changed nothing, so the head's `N lines` is the whole
/// report and the file itself lives one keystroke away. The colouring those
/// tests cover (#4019, #4020, #4036) is untouched and still worth pinning; it
/// simply lives on the other side of that keystroke now, which is where they
/// assert it.
fn expanded(entry: &TranscriptEntry) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    entry_lines(
        entry,
        EntryView::default(),
        false,
        true,
        false,
        120,
        &mut out,
    );
    out
}

fn collapsed(entry: &TranscriptEntry) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    entry_lines(
        entry,
        EntryView::default(),
        false,
        false,
        false,
        120,
        &mut out,
    );
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

/// A JSON result is read as fields — `query needle`, `hits 3` — and not one
/// brace, quote or comma of the wire reaches the pane. The key wears the
/// muted tone and the value the text tone, so the object still has a shape
/// without having syntax.
#[test]
fn a_json_result_body_renders_as_fields_not_as_json() {
    let body = "{\n  \"query\": \"needle\",\n  \"hits\": 3\n}";
    let lines = collapsed(&result(body));
    let rendered = text_of(&lines);
    assert!(rendered.contains("query needle"), "{rendered}");
    assert!(rendered.contains("hits 3"), "{rendered}");
    for forbidden in ['{', '}', '"', ','] {
        assert!(
            !rendered.contains(forbidden),
            "JSON punctuation {forbidden:?} reached the pane:\n{rendered}"
        );
    }
    let spans = colors(&lines);
    assert!(
        spans
            .iter()
            .any(|(t, c)| t == "query" && *c == Some(stella_tui_theme::token::MUTED)),
        "the key is muted: {spans:?}"
    );
    assert!(
        spans
            .iter()
            .any(|(t, c)| t == "needle" && *c == Some(stella_tui_theme::token::TEXT)),
        "the value is text: {spans:?}"
    );
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

/// A folded JSON result previews its first fields and states how many are
/// behind `ctrl+o` — in fields, which is the unit the reader sees.
#[test]
fn a_json_preview_shows_the_first_fields_and_counts_the_rest() {
    let body = "{\"a\": 1, \"b\": 2, \"c\": 3, \"d\": 4, \"e\": 5, \"f\": 6, \"g\": 7, \"h\": 8}";
    let rendered = text_of(&collapsed(&result(body)));
    assert!(rendered.contains("a 1"), "{rendered}");
    assert!(rendered.contains("2 more fields · ctrl+o"), "{rendered}");
    assert!(!rendered.contains("h 8"), "the tail is folded:\n{rendered}");
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

// ── A file read is source, and now reads as source (#4019, #4020) ───────────

/// A `read_file` result is coloured in the language of the file it read.
///
/// The witness: colouring used to be gated on `reads_as_json(full)` — does the
/// body's first non-blank character open a JSON document — and a numbered
/// listing's first character is a *digit*, so a file read fell through to one
/// flat muted tone for every language. The lexer was present and wired to
/// diffs, markdown fences and the definition editors the whole time; nothing
/// reached it from a tool result.
#[test]
fn a_read_result_is_coloured_in_its_files_language() {
    let body = numbered(
        780,
        &["#[test]", "fn a_verb_is_named() {", "    let x = 1;", "}"],
    );
    let spans = colors(&expanded(&read_result(
        "crates/stella-autonomy/src/tests.rs",
        &body,
    )));

    let keyword = syntax::tok_style(syntax::Tok::Keyword).fg;
    assert!(
        spans.iter().any(|(t, c)| t == "fn" && *c == keyword),
        "a Rust keyword in a read_file body was not coloured: {spans:?}"
    );
    assert!(
        spans.iter().any(|(t, c)| t == "let" && *c == keyword),
        "only the first line was coloured: {spans:?}"
    );
}

/// The same read, of a JSON file, is coloured too.
///
/// Kept apart from the case above because it is the sharper half of the bug:
/// the deck already *had* a JSON path, and the gutter defeated it just as
/// thoroughly as it defeated everything else. A reader could reasonably have
/// concluded the deck simply did not highlight source; it did not highlight
/// the one format it was built to highlight either.
#[test]
fn a_read_of_a_json_file_is_coloured_despite_its_gutter() {
    let body = numbered(1, &["{", "  \"name\": \"stella\",", "  \"count\": 12", "}"]);
    let spans = colors(&expanded(&read_result(".stella/settings.json", &body)));

    assert!(
        spans
            .iter()
            .any(|(t, c)| t == "\"name\"" && *c == syntax::tok_style(syntax::Tok::Keyword).fg),
        "a JSON key read through read_file was not coloured: {spans:?}"
    );
    assert!(
        spans
            .iter()
            .any(|(t, c)| t == "\"stella\"" && *c == syntax::tok_style(syntax::Tok::Str).fg),
        "a JSON string read through read_file was not coloured: {spans:?}"
    );
}

/// The line-number gutter is chrome, and is never lexed with the source.
///
/// Without the split it is the leading run of every line, so the lexer would
/// take each line number for a numeric literal and paint the gutter in the hue
/// that means "a value in this code" — the brightest column on screen, saying
/// nothing.
#[test]
fn the_line_number_gutter_is_not_lexed_as_source() {
    let body = numbered(780, &["fn f() {}"]);
    let spans = colors(&expanded(&read_result("src/lib.rs", &body)));

    let number = syntax::tok_style(syntax::Tok::Number).fg;
    assert!(
        !spans.iter().any(|(t, c)| t.contains("780") && *c == number),
        "the line number was coloured as a numeric literal: {spans:?}"
    );
    assert!(
        spans
            .iter()
            .any(|(t, c)| t.contains("780") && *c == Some(theme::TEXT_TERTIARY)),
        "the gutter lost its subordinate tone: {spans:?}"
    );
}

/// A body line with no gutter — `read_file`'s own footer, a cap notice — is not
/// source and is not lexed as any.
#[test]
fn a_body_line_without_a_gutter_stays_plain() {
    let body = format!("{}(read 1/1 lines)", numbered(1, &["const X: u8 = 1;"]));
    let spans = colors(&expanded(&read_result("src/lib.rs", &body)));

    // The source line still colours...
    assert!(
        spans
            .iter()
            .any(|(t, c)| t == "const" && *c == syntax::tok_style(syntax::Tok::Keyword).fg),
        "the numbered line stopped colouring: {spans:?}"
    );
    // ...and the footer beside it carries no token hue at all.
    let footer: Vec<_> = spans
        .iter()
        .filter(|(t, _)| t.contains("read 1/1 lines"))
        .collect();
    assert!(!footer.is_empty(), "the footer vanished: {spans:?}");
    assert!(
        footer.iter().all(|(_, c)| *c == Some(theme::MUTED)),
        "the footer was lexed as source: {footer:?}"
    );
}

/// An extension the deck has no lexer for renders exactly as it did before —
/// plain, never wrong. [`crate::syntax`]'s contract is that an unknown language
/// degrades to no colouring rather than to the wrong one.
#[test]
fn a_read_of_an_unknown_extension_stays_plain() {
    let body = numbered(1, &["fn this is not any language we lex"]);
    let spans = colors(&collapsed(&read_result("notes/scratch.xyz", &body)));
    assert!(
        !spans
            .iter()
            .any(|(_, c)| *c == syntax::tok_style(syntax::Tok::Keyword).fg),
        "an unlexed extension picked up keyword colouring: {spans:?}"
    );
}

/// No tab reaches a drawn span.
///
/// ratatui's `Span::styled_graphemes` filters control characters out of the
/// grapheme stream, so a tab is not narrowed and not drawn zero-width — it is
/// **deleted**. `read_file` separates its gutter from the source with one, so
/// an unindented source line arrived on screen as `780#[test]`, while an
/// indented one looked correct by accident: its own leading spaces stood in for
/// the missing separator (#4020).
///
/// Asserted at the span layer rather than against a rendered buffer because
/// that is also where the deck's own wrap arithmetic reads the width — a tab is
/// width 0 to `UnicodeWidthChar` as well, so leaving it in place would have the
/// deck measuring one thing and ratatui drawing another.
#[test]
fn no_tab_survives_into_a_drawn_span() {
    let body = numbered(780, &["#[test]", "\tfn tab_indented() {}"]);
    let lines = expanded(&read_result("src/tests.rs", &body));

    for line in &lines {
        for span in &line.spans {
            assert!(
                !span.content.contains('\t'),
                "a tab survived into a drawn span ({:?}); ratatui will delete it",
                span.content
            );
        }
    }

    let rendered = text_of(&lines);
    let row = rendered
        .lines()
        .find(|l| l.contains("#[test]"))
        .unwrap_or_else(|| panic!("the source line was not rendered:\n{rendered}"));
    let (_, after) = row
        .split_once("780")
        .unwrap_or_else(|| panic!("the gutter was not rendered: {row:?}"));
    assert!(
        after.starts_with(' '),
        "the line number is glued to the source it numbers: {row:?}"
    );
}

/// SPEC 2 folds what did not change, and a read changed nothing.
///
/// The witness for the collapsed read. Before this the row printed five lines
/// of a file nobody asked to see plus `⋯ 56 lines · ctrl+o`, which is the
/// transcript spending its scarcest resource — rows — on the one event with
/// nothing to report.
#[test]
fn a_collapsed_read_shows_no_body_at_all() {
    let body = numbered(
        20,
        &[
            "use crate::deck_ui::DeckUi;",
            "use crate::theme;",
            "",
            "fn go() {}",
        ],
    );
    let rows = collapsed(&read_result("crates/stella-tui/src/views/issues.rs", &body));
    let text: String = rows
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert!(
        !text.contains("use crate::deck_ui"),
        "a collapsed read leaked its file into the transcript:\n{text}"
    );
    // Expanding is where the file lives, and it still carries its language.
    let expanded_text: String =
        expanded(&read_result("crates/stella-tui/src/views/issues.rs", &body))
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
    assert!(
        expanded_text.contains("use crate::deck_ui"),
        "ctrl+o must still show the file:\n{expanded_text}"
    );
}

/// A mutation whose diff has not arrived stays quiet rather than printing the
/// tool's sentence.
///
/// This is the row that made the same edit look informative on one turn and
/// useless on the next: the diff lands with the `FileChange`, which can be a
/// beat behind the result, and until it did the row fell back to
/// `edit_file`'s own prose — the path the head already names, plus a byte
/// offset and a truncated hash.
#[test]
fn an_edit_with_no_diff_yet_prints_no_prose() {
    let rows = collapsed(&tool_result(
        "edit_file",
        true,
        Some("crates/stella-tui/src/views/issues.rs"),
        "replaced 1 occurrence(s) in crates/stella-tui/src/views/issues.rs \
         at byte 1286 (file sha256/8 e951e674)",
    ));
    let text: String = rows
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    for noise in ["replaced 1 occurrence", "byte 1286", "sha256"] {
        assert!(
            !text.contains(noise),
            "{noise:?} reached the transcript:\n{text}"
        );
    }
}

/// A failure still shows its body, whatever the tool.
///
/// The point of reading a transcript at the moment something breaks is to see
/// why, and that argument does not care which tool broke — so the rule above
/// is scoped to successes.
#[test]
fn a_failed_edit_still_shows_why() {
    let rows = collapsed(&tool_result(
        "edit_file",
        false,
        Some("src/lib.rs"),
        "no occurrence of `foo` in src/lib.rs",
    ));
    let text: String = rows
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect();
    assert!(
        text.contains("no occurrence"),
        "a failure must still say why:\n{text}"
    );
}
