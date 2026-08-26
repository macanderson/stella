// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **`design/tui-v2/SPEC.md` §6.4 describes the highlighter that ships.**
//!
//! §6.4 named the syntax palette in one sentence — *"keywords gold, types
//! silver_type, identifiers text, comments comment, strings silver, primitives
//! silver_type"* — and every clause of it was false. Keywords ride the bright
//! neutral, strings are a green at the scheme's own chroma, numbers are violet,
//! and the grammar-backed lexer (#4283) added a type and a function position
//! the sentence predates entirely. Comments were the clause that got noticed
//! (#4946): the `comment` token the sentence named was painted by nothing
//! anywhere in the tree, and the highlighter has always used the caption tier.
//!
//! A specification that disagrees with the code in six of six roles is worse
//! than none, because it is read as the answer. So the sentence is now a table
//! between two markers, and this test is what makes the table true: it reads
//! §6.4 and holds every row to the constant it names.
//!
//! It reads the file as text rather than generating it. Generation would make
//! the spec a projection of the code, and the point of a spec is that it can
//! disagree — a row that no longer matches is a decision someone has to make,
//! which is the conversation this file exists to force rather than to
//! foreclose.

use std::path::{Path, PathBuf};

use ratatui::style::Color;
use stella_tui::theme;

const BEGIN: &str = "<!-- BEGIN syntax palette -->";
const END: &str = "<!-- END syntax palette -->";

fn spec() -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../design/tui-v2/SPEC.md")
        .canonicalize()
        .expect("design/tui-v2/SPEC.md resolves from CARGO_MANIFEST_DIR");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The constants §6.4 is allowed to name, and their shipped values.
///
/// Spelled here rather than looked up, because Rust has no reflection over
/// module constants — and spelling them is what makes the table's *coverage*
/// checkable: a `SYNTAX_*` constant that grows without a row is caught by
/// `every_syntax_constant_has_a_row` below, which is the half a value
/// comparison alone would miss.
fn shipped() -> Vec<(&'static str, Color)> {
    vec![
        ("SYNTAX_KEYWORD", theme::SYNTAX_KEYWORD),
        ("SYNTAX_STRING", theme::SYNTAX_STRING),
        ("SYNTAX_NUMBER", theme::SYNTAX_NUMBER),
        ("SYNTAX_COMMENT", theme::SYNTAX_COMMENT),
        ("SYNTAX_TYPE", theme::SYNTAX_TYPE),
        ("SYNTAX_FUNCTION", theme::SYNTAX_FUNCTION),
    ]
}

fn hex(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02X}{g:02X}{b:02X}"),
        other => panic!("a syntax role must be a truecolor value, not {other:?}"),
    }
}

/// `(constant, stated hex)` for every row of §6.4's table.
fn rows(markdown: &str) -> Vec<(String, String)> {
    let from = markdown
        .find(BEGIN)
        .unwrap_or_else(|| panic!("design/tui-v2/SPEC.md carries no `{BEGIN}`"))
        + BEGIN.len();
    let len = markdown[from..]
        .find(END)
        .unwrap_or_else(|| panic!("no `{END}` after `{BEGIN}`"));
    markdown[from..from + len]
        .lines()
        .filter_map(|line| {
            let cells: Vec<&str> = line.trim().trim_matches('|').split('|').collect();
            if cells.len() != 3 {
                return None;
            }
            let constant = cells[1].trim().trim_matches('`');
            let value = cells[2].trim().trim_matches('`');
            if !constant.starts_with("SYNTAX_") || !value.starts_with('#') {
                return None;
            }
            Some((constant.to_string(), value.to_ascii_uppercase()))
        })
        .collect()
}

/// Every value §6.4 states is the value that constant carries.
#[test]
fn the_spec_states_the_syntax_palette_that_ships() {
    let table = rows(&spec());
    assert!(
        !table.is_empty(),
        "§6.4's syntax table parsed to no rows. An empty table passes every \
         comparison below, which is the one way this test can be green and \
         mean nothing."
    );

    let mut drift = Vec::new();
    for (constant, stated) in &table {
        match shipped().iter().find(|(name, _)| name == constant) {
            None => drift.push(format!(
                "§6.4 names `theme::{constant}`, which does not exist"
            )),
            Some((_, color)) => {
                let real = hex(*color);
                if &real != stated {
                    drift.push(format!(
                        "§6.4 says `theme::{constant}` is {stated}; it is {real}"
                    ));
                }
            }
        }
    }

    assert!(
        drift.is_empty(),
        "design/tui-v2/SPEC.md §6.4 disagrees with the highlighter in {} \
         role(s):\n  {}\n\nMove the code or move the spec — but a reader has \
         to be able to trust one of them.",
        drift.len(),
        drift.join("\n  ")
    );
}

/// Every syntax role the highlighter paints has a row.
///
/// The value check above cannot see an *absence*, and absence is how §6.4 went
/// stale in the first place: `SYNTAX_TYPE` and `SYNTAX_FUNCTION` arrived with
/// #4283 and the spec's sentence was never widened to admit they existed.
#[test]
fn every_syntax_constant_has_a_row() {
    let table = rows(&spec());
    let missing: Vec<&str> = shipped()
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !table.iter().any(|(stated, _)| stated == name))
        .collect();

    assert!(
        missing.is_empty(),
        "the highlighter paints {} role(s) design/tui-v2/SPEC.md §6.4 does not \
         mention: {}",
        missing.len(),
        missing.join(", ")
    );
}
