// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A JSON body that arrived on one line is re-laid so it can be read.
//!
//! The bug: an API response — `gh api`, an MCP server, any REST tool — comes
//! back as a single line. Every surface counted that as a one-line result, hid
//! nothing, offered no reveal affordance, and handed the pane several thousand
//! unbroken columns to wrap. The content was all there and none of it was
//! legible.

use crate::digest;
use crate::model::Output;
use crate::syntax;

/// Every non-whitespace character outside a string literal, in order — the
/// invariant [`syntax::reindent_json`] promises, and the only one that makes it
/// safe to run over a body nobody has parsed.
fn skeleton(text: &str) -> String {
    let mut out = String::new();
    let mut in_string = false;
    let mut escaped = false;
    for ch in text.chars() {
        if in_string {
            out.push(ch);
            match ch {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => {
                in_string = true;
                escaped = false;
                out.push(ch);
            }
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    out
}

/// The witness. One line in, an object with a shape out — and nothing but
/// whitespace changed on the way.
#[test]
fn a_one_line_json_body_is_re_laid_one_member_to_a_line() {
    let body = r#"{"id":42,"name":"stella","tags":["a","b"],"nested":{"ok":true}}"#;
    let out = syntax::reindent_json(body);
    assert!(
        out.lines().count() > 6,
        "the body is still one line: {out:?}"
    );
    assert_eq!(
        skeleton(&out),
        skeleton(body),
        "re-indenting changed something other than whitespace"
    );
    assert!(
        out.lines().any(|l| l == r#"  "id":42,"#),
        "a top-level member did not land on its own line:\n{out}"
    );
}

/// Applying it twice is applying it once — which is what lets a surface run it
/// unconditionally on anything that reads as JSON, with no "long enough"
/// threshold for two surfaces to disagree about.
#[test]
fn re_indenting_an_indented_body_is_a_no_op() {
    let pretty = "{\n  \"a\": 1,\n  \"b\": [\n    2,\n    3\n  ],\n  \"c\": {}\n}";
    assert_eq!(syntax::reindent_json(pretty), pretty);
}

/// The bodies these surfaces hold are **middle-elided to a char budget** and
/// are therefore not valid JSON. A `serde_json` round trip refuses exactly this
/// input and hands the reader the wall of text back; a scan does not.
#[test]
fn a_truncated_body_is_still_re_laid() {
    let body = "{\"a\":1,\"b\":2,\"c\":3\n[… truncated …]\n\"x\":9,\"y\":10}";
    assert!(
        serde_json::from_str::<serde_json::Value>(body).is_err(),
        "the fixture parses, so it does not test the truncated case"
    );
    let out = syntax::reindent_json(body);
    assert!(
        out.lines().any(|l| l.trim() == r#""a":1,"#),
        "the head half was not re-laid:\n{out}"
    );
    assert!(out.contains(r#""y":10"#), "the tail half was lost:\n{out}");
    assert!(
        out.contains("truncated"),
        "the elision marker was destroyed:\n{out}"
    );
}

/// Whitespace inside a string literal is content, not layout — and a brace
/// inside one is not structure.
#[test]
fn string_contents_survive_re_indenting() {
    let body = r#"{"msg":"a  b\n c","re":"{\"x\": 1}"}"#;
    let out = syntax::reindent_json(body);
    assert!(
        out.contains(r#""a  b\n c""#),
        "a double space inside a string was collapsed:\n{out}"
    );
    assert!(
        out.contains(r#""{\"x\": 1}""#),
        "an escaped brace inside a string was read as structure:\n{out}"
    );
}

/// An empty container keeps its two glyphs together rather than being broken
/// across three lines to say nothing.
#[test]
fn an_empty_container_stays_on_one_line() {
    assert_eq!(syntax::reindent_json("{}"), "{}");
    assert_eq!(syntax::reindent_json("[]"), "[]");
    assert_eq!(syntax::reindent_json(r#"{"a":[]}"#), "{\n  \"a\":[]\n}");
}

/// The sniff and the transform are asked together, so a `read_file` listing —
/// whose line-number gutter makes its first character a digit — is never
/// re-laid as if it were an object.
#[test]
fn only_a_body_that_reads_as_json_is_re_indented() {
    assert!(syntax::reindent_json_body(r#"{"a":1}"#).is_some());
    assert!(syntax::reindent_json_body("     1\t{\n     2\t  \"a\": 1\n").is_none());
    assert!(syntax::reindent_json_body("error: no such file").is_none());
}

/// The agent's own task board is not JSON, and neither is anything else whose
/// `[` opens something an array cannot hold.
///
/// `task_list` returns one row per line as `[x] #1 subject`
/// (`stella_tools::tasks::render_line`), so a `starts_with('[')` sniff sent
/// every board down the re-indent and the JSON colourer (#4344). All four
/// status glyphs are covered, `-` included: it is the one that also opens a
/// JSON number.
#[test]
fn a_task_board_is_not_json_and_a_real_array_still_is() {
    for row in ["[ ] #1 a", "[x] #1 a", "[~] #1 a", "[-] #1 a"] {
        assert!(!syntax::reads_as_json(row), "board row read as JSON: {row}");
    }
    assert!(!syntax::body_reads_as_json(&[
        "[ ] #1 a".to_string(),
        "[x] #2 b".to_string(),
    ]));
    assert!(syntax::reindent_json_body("[ ] #1 a\n[x] #2 b").is_none());

    for array in [
        r#"[{"a":1}]"#,
        "[]",
        "[ ]",
        r#"["a"]"#,
        "[1,2]",
        "[-1]",
        "[true]",
        "[null]",
        "[[0]]",
        // A pretty-printed array's opening line, which is all
        // `body_reads_as_json` ever sees of it.
        "[",
    ] {
        assert!(
            syntax::reads_as_json(array),
            "array not read as JSON: {array}"
        );
    }
    assert!(syntax::body_reads_as_json(&[
        "[".to_string(),
        r#"  {"a": 1}"#.to_string(),
        "]".to_string(),
    ]));
}

/// The fold measures the re-laid body, so a one-line API response stops
/// claiming it is fully shown.
///
/// This is the half a reader feels. Before it, `fold_output` counted one line,
/// hid nothing, and every surface reported the blob as complete. The deck
/// normalises through the same function, which is what keeps "how much of this
/// result do I see" one answer across the surfaces (#3644).
#[test]
fn the_fold_measures_a_re_laid_json_body() {
    let members: String = (1..=40).map(|i| format!("\"k{i}\":{i},")).collect();
    let one_line = format!("{{{members}\"end\":0}}");
    let fold = digest::fold_output(
        &Output {
            lines: vec![one_line],
            clipped: 0,
        },
        "gh api /repos/oxagen/stella",
    );
    assert!(
        fold.has_more(),
        "a 41-member object still reports nothing hidden: {fold:?}"
    );
    assert_eq!(
        fold.head.len() + fold.tail.len(),
        digest::PREVIEW_LINES,
        "the fold did not spend its preview budget on the re-laid body"
    );
}
