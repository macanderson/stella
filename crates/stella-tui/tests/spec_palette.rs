// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **`design/tui-v2/SPEC.md` §3.1 and §3.5 state the palette that ships.**
//!
//! §3.1 is the normative palette table and `design/tokens/stella-tokens.json`'s
//! own header points at it — *"Normative definition: SPEC-stella-tui-v2.md
//! sections 2-4"* — while the JSON is the file every artifact generates from.
//! Nothing held the two together. §3.1 happens to be correct today, and what
//! makes it correct is that people have been careful, which is the state §6.4
//! was in right up until it was not: its one-sentence syntax palette was false
//! in six of six roles for as long as the grammar-backed lexer existed
//! (#4283, #4946). #4967 added `spec_syntax_palette.rs` for §6.4 and had to
//! delete §3.1's `comment` row by hand — the manual step this file removes.
//!
//! §3.5's fallback list is here for the same reason and one more. #4976 read
//! `token::SILVER | token::SILVER_TYPE => Color::Gray` in
//! `stella_tui_theme::fallback::ansi16` as contradicting §3.5's *"silver to
//! white"*. It does not: ratatui spells ANSI 7 `Color::Gray` and ANSI 15
//! `Color::White`, so that arm **is** silver going to white, with `text`
//! lifting to bright white above it — `crates/stella-tui/src/theme.rs`'s
//! `FALLBACKS` records the same pair as 16-indices 7 and 15. A sentence that
//! can be misread into a bug report is a sentence worth turning into a table
//! a test holds, which is what §3.5 is now.
//!
//! Both tables are **read**, not generated. Generating them would make the
//! spec a projection of the code, and a spec's job is that it can disagree —
//! a row that stops matching should become a decision someone makes, not
//! something that cannot happen.

use std::path::{Path, PathBuf};

use ratatui::style::Color;
use stella_tui_theme::{fallback, token};

const PALETTE_BEGIN: &str = "<!-- BEGIN palette -->";
const PALETTE_END: &str = "<!-- END palette -->";
const DEGRADE_BEGIN: &str = "<!-- BEGIN degradation -->";
const DEGRADE_END: &str = "<!-- END degradation -->";

/// The surface `stella-tokens.json` uses for the deck's own ramp.
const TUI_SURFACE: &str = "tui";

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
        .canonicalize()
        .unwrap_or_else(|e| panic!("{rel} resolves from CARGO_MANIFEST_DIR: {e}"))
}

fn read(rel: &str) -> String {
    let path = repo(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn spec() -> String {
    read("design/tui-v2/SPEC.md")
}

/// One declared token: its name, its value, and whether the deck renders it.
struct Token {
    name: String,
    hex: String,
    on_tui: bool,
}

/// Every token `design/tokens/stella-tokens.json` declares.
fn declared() -> Vec<Token> {
    let doc: serde_json::Value = serde_json::from_str(&read("design/tokens/stella-tokens.json"))
        .expect("design/tokens/stella-tokens.json is valid JSON");
    let rows = doc["tokens"]
        .as_array()
        .expect("`tokens` is an array")
        .iter()
        .filter_map(|tok| {
            // The array carries one `$comment`-only object as a section
            // header; it declares no token and is not one.
            let name = tok.get("name")?.as_str()?.to_string();
            let hex = tok.get("hex")?.as_str()?.to_ascii_uppercase();
            let on_tui = tok
                .get("surfaces")
                .and_then(|s| s.as_array())
                .is_some_and(|s| s.iter().any(|v| v.as_str() == Some(TUI_SURFACE)));
            Some(Token { name, hex, on_tui })
        })
        .collect::<Vec<_>>();
    assert!(
        !rows.is_empty(),
        "no token parsed out of design/tokens/stella-tokens.json — every \
         comparison below would then be vacuous"
    );
    rows
}

/// §3.1 and §3.5 spell a token `gold_bright`; the JSON spells it
/// `gold-bright`. The spec has used the underscore form since it was written
/// and §3.2 and §4 name tokens the same way in running prose, so the two
/// spellings are reconciled here rather than by rewriting the document — a
/// separator is a spelling convention, and this test is about values.
fn canonical(name: &str) -> String {
    name.replace('_', "-")
}

/// `(token, second cell)` for every row of the table between two markers.
fn rows(markdown: &str, begin: &str, end: &str) -> Vec<(String, String)> {
    let from = markdown
        .find(begin)
        .unwrap_or_else(|| panic!("design/tui-v2/SPEC.md carries no `{begin}`"))
        + begin.len();
    let len = markdown[from..]
        .find(end)
        .unwrap_or_else(|| panic!("no `{end}` after `{begin}`"));
    markdown[from..from + len]
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if !line.starts_with('|') {
                return None;
            }
            let cells: Vec<&str> = line.trim_matches('|').split('|').collect();
            if cells.len() < 2 {
                return None;
            }
            let name = cells[0].trim();
            // The header row and its `|---|` separator have no backticked
            // token in the first cell.
            let name = name.strip_prefix('`')?.strip_suffix('`')?;
            Some((
                canonical(name),
                cells[1].trim().trim_matches('`').to_string(),
            ))
        })
        .collect()
}

/// Every value §3.1 states is the value that token holds.
#[test]
fn the_spec_states_the_palette_that_ships() {
    let table = rows(&spec(), PALETTE_BEGIN, PALETTE_END);
    assert!(
        !table.is_empty(),
        "§3.1's palette table parsed to no rows. An empty table passes every \
         comparison below, which is the one way this test can be green and \
         mean nothing."
    );

    let tokens = declared();
    let mut drift = Vec::new();
    for (name, stated) in &table {
        match tokens.iter().find(|t| &t.name == name) {
            None => drift.push(format!(
                "§3.1 has a row for `{name}`, which design/tokens/stella-tokens.json \
                 does not declare"
            )),
            Some(token) if token.hex != stated.to_ascii_uppercase() => drift.push(format!(
                "§3.1 says `{name}` is {stated}; the palette holds {}",
                token.hex
            )),
            Some(token) if !token.on_tui => drift.push(format!(
                "§3.1 has a row for `{name}`, which declares no `{TUI_SURFACE}` \
                 surface — §3.1 is the deck's ramp"
            )),
            Some(_) => {}
        }
    }

    assert!(
        drift.is_empty(),
        "design/tui-v2/SPEC.md §3.1 disagrees with \
         design/tokens/stella-tokens.json in {} place(s):\n  {}\n\nMove the \
         palette or move the spec — but a reader has to be able to trust one \
         of them.",
        drift.len(),
        drift.join("\n  ")
    );
}

/// Every token the deck renders has a §3.1 row.
///
/// The value check above cannot see an *absence*, and absence is how §6.4 went
/// stale: `SYNTAX_TYPE` and `SYNTAX_FUNCTION` arrived with #4283 and the
/// sentence was never widened to admit they existed.
#[test]
fn every_tui_token_has_a_palette_row() {
    let table = rows(&spec(), PALETTE_BEGIN, PALETTE_END);
    let missing: Vec<String> = declared()
        .into_iter()
        .filter(|t| t.on_tui)
        .map(|t| t.name)
        .filter(|name| !table.iter().any(|(stated, _)| stated == name))
        .collect();

    assert!(
        missing.is_empty(),
        "the palette declares {} token(s) on the `{TUI_SURFACE}` surface that \
         design/tui-v2/SPEC.md §3.1 does not mention: {}",
        missing.len(),
        missing.join(", ")
    );
}

/// The ANSI name for each colour ratatui can stand a token down to.
///
/// Spelled out because ratatui's names are not the ANSI ones and the gap is
/// what #4976 tripped on: `Color::Gray` is ANSI 7, the standard white, and
/// `Color::White` is ANSI 15, bright white. Reading the enum variant as the
/// colour's name turns "silver to white" into "silver to gray" and a correct
/// mapping into a bug report.
fn ansi_name(color: Color) -> &'static str {
    match color {
        Color::Black => "black",
        Color::DarkGray => "bright black",
        Color::Red => "red",
        Color::Green => "green",
        Color::Yellow => "yellow",
        Color::Blue => "blue",
        Color::Magenta => "magenta",
        Color::Cyan => "cyan",
        Color::Gray => "white",
        Color::White => "bright white",
        Color::LightRed => "bright red",
        Color::LightGreen => "bright green",
        Color::LightYellow => "bright yellow",
        Color::LightBlue => "bright blue",
        Color::LightMagenta => "bright magenta",
        Color::LightCyan => "bright cyan",
        other => panic!("ansi16 returned {other:?}, which is not one of the sixteen"),
    }
}

/// Every stand-in §3.5 states is the one `ansi16` returns.
#[test]
fn the_spec_states_the_degradation_that_ships() {
    let table = rows(&spec(), DEGRADE_BEGIN, DEGRADE_END);
    assert!(
        !table.is_empty(),
        "§3.5's degradation table parsed to no rows — every comparison below \
         would then be vacuous."
    );

    let mut drift = Vec::new();
    for (name, stated) in &table {
        match token::ALL.iter().find(|(n, ..)| canonical(n) == *name) {
            None => drift.push(format!(
                "§3.5 has a row for `{name}`, which is not an entry in `token::ALL`"
            )),
            Some((_, color, _)) => {
                let real = ansi_name(fallback::ansi16(*color));
                if real != stated {
                    drift.push(format!(
                        "§3.5 says `{name}` stands down to {stated}; `ansi16` \
                         returns {real}"
                    ));
                }
            }
        }
    }

    assert!(
        drift.is_empty(),
        "design/tui-v2/SPEC.md §3.5 disagrees with \
         `stella_tui_theme::fallback::ansi16` in {} place(s):\n  {}",
        drift.len(),
        drift.join("\n  ")
    );
}

/// Every token has a §3.5 row.
///
/// `ansi16` is exhaustive over `token::ALL` by construction — its catch-all
/// arm passes a non-token straight through — so a token added without a row
/// would silently take whatever the catch-all gives it. The spec is where
/// that choice is supposed to be made.
#[test]
fn every_token_has_a_degradation_row() {
    let table = rows(&spec(), DEGRADE_BEGIN, DEGRADE_END);
    let missing: Vec<String> = token::ALL
        .iter()
        .map(|(name, ..)| canonical(name))
        .filter(|name| !table.iter().any(|(stated, _)| stated == name))
        .collect();

    assert!(
        missing.is_empty(),
        "`token::ALL` carries {} token(s) design/tui-v2/SPEC.md §3.5 does not \
         mention: {}",
        missing.len(),
        missing.join(", ")
    );
}

/// The palette table and the degradation table are about the same palette.
///
/// §3.1 is the deck's ramp and §3.5 is every token, so §3.5 is a superset —
/// and it has to stay one, or a `tui` token could hold a hex nobody stood
/// down. Checked here rather than assumed, because the two tables are
/// maintained one section apart and nothing else compares them.
#[test]
fn the_degradation_table_covers_the_palette_table() {
    let markdown = spec();
    let palette = rows(&markdown, PALETTE_BEGIN, PALETTE_END);
    let degradation = rows(&markdown, DEGRADE_BEGIN, DEGRADE_END);
    let missing: Vec<&str> = palette
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| !degradation.iter().any(|(stated, _)| stated == name))
        .collect();

    assert!(
        missing.is_empty(),
        "§3.1 names {} token(s) §3.5 does not stand down: {}",
        missing.len(),
        missing.join(", ")
    );
}
