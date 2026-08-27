// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The plan revision a failing gate puts up — SPEC 8.1 items 3 and 4.
//!
//! ```text
//!  │ ⌥ propose r4: add task "repair a_short_cycle_is_detected"
//!  │     cause  tests · assertion `left == right` failed
//!  │     issue  #151
//!  │     a approve r4 · e edit · x dismiss
//!  │     merge blocked · unblocks on green
//! ```
//!
//! ## Gold, because it is drift and not an alarm
//!
//! The headline takes [`glyph::DRIFT`] in [`token::GOLD_BRIGHT`], the same
//! metal `plan_card::step_style` gives an inserted step — a proposal and the
//! drift row it becomes are one fact seen twice, and painting them differently
//! would make the reader work out that they are the same thing. Red is spent
//! on the failing gate row above (SPEC 2's scarcity rule, and
//! [`super::gate_board`]'s module docs); a proposal is what stella is doing
//! about that failure, not a second alarm about it.
//!
//! ## Nothing here runs anything
//!
//! A projection of owned data onto `Line<'static>`, like every other view in
//! this module. The keys named in the action row are
//! `deck_ui::row_keys`'s, and the withholding they release is
//! `stella_core::plan_graph::RevisionGate::admits`'s — this file draws the
//! sentence and holds none of the state behind it.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use stella_protocol::RevisionProposal;
use stella_tui_theme::{glyph, token};

use super::transcript::rail_span;

/// SPEC 8.1 item 4's banner. Stated as the proposal's closing line because
/// that is where the reader is looking when they decide: the merge is blocked
/// by the gate that failed, and a green board is what lifts it — never this
/// approval, which changes the plan and settles nothing.
const MERGE_BLOCKED: &str = "merge blocked · unblocks on green";

/// Cells the proposal's detail lines are indented under its headline.
const DETAIL_INDENT: &str = "     ";

/// Every row the proposal owns.
#[must_use]
pub fn proposal_rows(proposal: &RevisionProposal) -> Vec<Line<'static>> {
    let mut rows = vec![headline(proposal), field("cause", &cause_text(proposal))];
    if let Some(issue) = &proposal.issue {
        rows.push(field("issue", issue));
    }
    rows.push(detail(
        &format!("a approve {} · e edit · x dismiss", proposal.revision),
        token::GOLD,
    ));
    rows.push(detail(MERGE_BLOCKED, token::MUTED));
    rows
}

/// `⌥ propose r4: add task "<title>"`.
///
/// The subject is quoted because it is the task's own words and can contain
/// the separators around it — an unquoted title with a `·` in it would read as
/// two cells.
fn headline(proposal: &RevisionProposal) -> Line<'static> {
    let gold = Style::new().fg(token::GOLD_BRIGHT);
    Line::from(vec![
        rail_span(token::GOLD_BRIGHT),
        Span::styled(format!(" {} ", glyph::DRIFT), gold),
        Span::styled(
            format!("propose {}", proposal.revision),
            gold.add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(": add task \"{}\"", proposal.subject),
            Style::new().fg(token::TEXT),
        ),
    ])
}

/// `cause  tests · assertion …` — the gate that failed and what its evidence
/// said, joined so the reader never has to look up which gate a cause belongs
/// to.
fn cause_text(proposal: &RevisionProposal) -> String {
    format!("{} · {}", proposal.gate, proposal.cause.as_str())
}

/// One labelled detail line: a dim label, then the value in plain text.
fn field(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        rail_span(token::GOLD_BRIGHT),
        Span::styled(
            format!("{DETAIL_INDENT}{label}  "),
            Style::new().fg(token::DIM),
        ),
        Span::styled(value.to_owned(), Style::new().fg(token::TEXT)),
    ])
}

/// One unlabelled detail line in `color`.
fn detail(text: &str, color: ratatui::style::Color) -> Line<'static> {
    Line::from(vec![
        rail_span(token::GOLD_BRIGHT),
        Span::styled(format!("{DETAIL_INDENT}{text}"), Style::new().fg(color)),
    ])
}

#[cfg(test)]
mod tests;
