// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `doc:brand-hue` says what gold may mean on each surface.
//!
//! Each row of its table names the files that state a rule for gold. Every one
//! of them must cite the doc, so a person who edits one rule finds the others.
//! The row for interactive mode also names the states that take gold, and this
//! test holds that list to `status_color`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use ratatui::style::Color;
use stella_tui::envelope::AgentStatus;
use stella_tui::theme;

const DOC: &str = "docs/spec/brand-hue.md";
const CITE: &str = "doc:brand-hue";
const BEGIN: &str = "<!-- BEGIN brand-hue-surfaces -->";
const END: &str = "<!-- END brand-hue-surfaces -->";

/// The files that state a rule for gold today. The table may name more, but
/// it may not drop one of these, or that file could stop citing the doc and
/// nothing would see it.
const KNOWN: [&str; 5] = [
    "crates/stella-tui/src/theme.rs",
    "crates/stella-tui/src/palette.rs",
    "design/tui-v2/SPEC.md",
    "crates/stella-observatory/src/assets/index.html",
    "crates/stella-cli/src/export.rs",
];

/// The row whose states this test checks against the code.
const TERMINAL: &str = "crates/stella-tui/src/theme.rs";

/// The table's columns, counted from zero.
const SOURCE: usize = 1;
const STATUSES: usize = 3;

fn repo(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn read(rel: &str) -> String {
    let path = repo(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Each span in backticks in one cell of a row.
fn ticked(row: &[String], column: usize) -> Vec<String> {
    let cell = row.get(column).map(String::as_str).unwrap_or("");
    cell.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// The body rows of the table between the markers, split into cells.
fn rows(doc: &str) -> Vec<Vec<String>> {
    let start = doc
        .find(BEGIN)
        .unwrap_or_else(|| panic!("{DOC} has no {BEGIN}"));
    let end = doc
        .find(END)
        .unwrap_or_else(|| panic!("{DOC} has no {END}"));
    doc[start + BEGIN.len()..end]
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('|'))
        .skip(2)
        .map(|line| {
            line.trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_string())
                .collect()
        })
        .collect()
}

/// Every state an agent can be in. The match stops the build when a state is
/// added, so this list cannot fall behind the enum.
fn every_status() -> [AgentStatus; 7] {
    use AgentStatus::*;
    let all = [Queued, Running, Paused, WaitingInput, Done, Failed, Killed];
    for status in all {
        match status {
            Queued | Running | Paused | WaitingInput | Done | Failed | Killed => {}
        }
    }
    all
}

fn is_gold(color: Color) -> bool {
    [
        theme::ACCENT,
        theme::ACCENT_FILL,
        theme::ACCENT_LIVE,
        theme::GOLD,
        theme::GOLD_LIVE,
    ]
    .contains(&color)
}

#[test]
fn the_doc_carries_the_id_the_surfaces_cite() {
    let doc = read(DOC);
    assert!(
        doc.starts_with("---\nid: brand-hue\n"),
        "{DOC} must open with frontmatter naming id `brand-hue`"
    );
}

#[test]
fn every_file_that_states_a_rule_for_gold_cites_the_doc() {
    let rows = rows(&read(DOC));
    assert!(!rows.is_empty(), "{DOC} lists no surfaces");
    let mut named = BTreeSet::new();
    for row in &rows {
        let sources = ticked(row, SOURCE);
        assert!(!sources.is_empty(), "row {row:?} names no source file");
        for source in sources {
            // The only check on the HTML citation: the doc-link guard reads
            // .rs, .md and .toml files, and never .html.
            assert!(
                read(&source).contains(CITE),
                "{source} states a rule for gold but does not cite {CITE}"
            );
            named.insert(source);
        }
    }
    for known in KNOWN {
        assert!(
            named.contains(known),
            "{DOC} dropped {known} from its table"
        );
    }
}

#[test]
fn the_states_that_take_gold_in_interactive_mode_match_the_code() {
    let rows = rows(&read(DOC));
    let row = rows
        .iter()
        .find(|row| ticked(row, SOURCE).iter().any(|s| s == TERMINAL))
        .unwrap_or_else(|| panic!("{DOC} has no row for {TERMINAL}"));
    let stated: BTreeSet<String> = ticked(row, STATUSES).into_iter().collect();
    let actual: BTreeSet<String> = every_status()
        .into_iter()
        .filter(|status| is_gold(theme::status_color(*status)))
        .map(|status| format!("{status:?}"))
        .collect();
    assert_eq!(
        stated, actual,
        "{DOC} names these states as gold in interactive mode, and status_color disagrees"
    );
}
