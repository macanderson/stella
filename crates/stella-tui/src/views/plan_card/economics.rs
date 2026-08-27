// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What each task costs, what the running one is doing, and how far the plan
//! has drifted — SPEC 7.3's three additions to the plan panel.
//!
//! ```text
//!  ✓ 1.  read the band layout                            9.0k tok
//!  ◐ 2.  fold the rail                                          —
//!      │ contract  the rail renders at one height · unit · det
//!      │ evidence  —
//!      │ cost      $0.12 · 3:04
//!  ⌥ 4.  repair the tests gate  (inserted)                      —
//!
//!  planned 3 · actual — · ⌥ — drift
//!  drift is recorded, not hidden. it trains your model.
//! ```
//!
//! # An em dash is an answer
//!
//! Two of the three producers do not exist yet. `Plan::ledger` has a reader
//! and no writer — its own doc says why the session's untagged edits and runs
//! must not be borrowed to fill it — and `Plan::lanes` is in the same state
//! (`Plan::propose` sets it to `None` and nothing else writes it; #5286 and
//! #5270 are where that wiring is tracked). So the economics column, the actual count
//! and the drift count each render `—`.
//!
//! That is the point rather than a placeholder. A `0 tok` would be a
//! measurement nobody took, and `⌥ 0 drift` would say the plan held when
//! nothing checked. The shape is here so the producers land into a surface
//! that already reads them, and the dash says exactly which half is missing.
//!
//! # No `det` ratio, anywhere
//!
//! SPEC 5 forbids one and this issue drops it from the economics line by name.
//! A check either reaches a model or it does not — the running card's contract
//! line carries that as the `det` word, off `CheckMechanism::judge`, never as
//! a percentage.
//!
//! # Pure
//!
//! Projections onto `Line<'static>` over owned data, like every other view.
//! The running card's cost is a subtraction the *caller* performs against
//! `AgentEntry::active_task` (see [`RunningTask`]), so nothing here reads a
//! clock or a session.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use stella_protocol::TaskContract;
use stella_tui_theme::{glyph, token};

use crate::plan::{Plan, PlanStep, StepSpend};
use crate::views::cards;

/// What a cell renders when its producer does not exist yet — see the module
/// docs. One spelling, so no surface invents a second way to say "unmeasured".
pub const ELIDED: &str = "—";

/// SPEC 7.3's closing sentence, under the counts.
const DRIFT_CLOSER: &str = "drift is recorded, not hidden. it trains your model.";

/// Cells the running card's sub-lines are indented, matching the detail rows
/// [`super::step_rows`] already draws under a step title.
const SUB_INDENT: &str = "     ";

/// The in-flight task's live numbers, as the caller measured them.
///
/// The cost is a *subtraction* — `AgentEntry::cost_usd` now, less
/// `ActiveTaskStamp::cost_at_start_usd` when the board flipped this task
/// active — and the elapsed likewise. Both belong to the deck fold, which owns
/// the clock and the stamp; this module renders what it is handed and measures
/// nothing, which is what keeps the plan card a pure projection.
///
/// It is not read out of [`Plan::ledger`]: that map is the *attributed*
/// per-step spend and has no producer, while this is one number about one task
/// that the fold genuinely knows.
#[derive(Clone, Debug, PartialEq)]
pub struct RunningTask {
    /// The `PlanStep::id` the board has in progress.
    pub id: String,
    /// How long it has been running, from the deck clock.
    pub elapsed_ms: u64,
    /// What the session has spent since it went active.
    pub cost_usd: f64,
}

/// SPEC 7.3's right-aligned economics cell for one step — `9.0k tok`, or
/// [`ELIDED`] where nothing has attributed spend to it.
#[must_use]
pub fn token_cell(spend: Option<&StepSpend>) -> String {
    spend.map_or_else(
        || ELIDED.to_owned(),
        |spend| format!("{} tok", crate::textline::fmt_tokens(spend.tokens)),
    )
}

/// The running task's card: its contract, its evidence, and what it has cost.
///
/// Three sub-lines under the step's own row rather than a second framed box,
/// because the plan card is already a frame and a box inside a box costs two
/// columns of rail for no extra meaning. What marks it as a card is the gold
/// rail on each line — the same device the transcript uses to bind a block to
/// the row above it.
///
/// Empty when `step` is not the running task, so a caller can hand every step
/// the same `running` and get the card exactly once.
#[must_use]
pub fn running_card(
    step: &PlanStep,
    running: Option<&RunningTask>,
    ledger: &Plan,
    width: usize,
) -> Vec<Line<'static>> {
    let Some(running) = running.filter(|r| r.id == step.id) else {
        return Vec::new();
    };
    let evidence = ledger
        .ledger
        .get(&step.id)
        .map(|entry| entry.evidence.len())
        .filter(|rows| *rows > 0)
        .map_or_else(
            || ELIDED.to_owned(),
            |rows| format!("{rows} recorded this task"),
        );
    let cost = format!(
        "${:.2} · {}",
        running.cost_usd,
        cards::fmt_mss(running.elapsed_ms)
    );
    let budget = width.saturating_sub(GUTTER_W).max(MIN_VALUE_COLS);
    [
        ("contract", contract_line(step.contract.as_ref(), budget)),
        ("evidence", evidence),
        ("cost", cost),
    ]
    .into_iter()
    .map(|(label, value)| sub_line(label, &value, budget))
    .collect()
}

/// The contract in one line, inside `budget` columns: what done means, and the
/// mechanism that settles it with its `det` tag.
///
/// The first check only. SPEC 7.3 asks the running card for *a* contract line,
/// and a task with four checks belongs in the task zoom (SPEC 7.5), which
/// renders every one of them — a card that grew a row per check would push the
/// plan off the screen exactly when the reader wants to see it.
///
/// **The statement is what gets cut, never the tail.** The mechanism and the
/// `det` word say whether a model gets to decide, and a naive truncation drops
/// exactly them because they come last. So the tail is measured first and the
/// author's sentence is fitted into what remains.
fn contract_line(contract: Option<&TaskContract>, budget: usize) -> String {
    let Some(contract) = contract else {
        return format!("{ELIDED} · the board stated no contract");
    };
    if matches!(contract, TaskContract::ReadOnly) {
        return "read only · no contract".to_owned();
    }
    let Some(check) = contract.checks().next() else {
        return format!("{ELIDED} · the contract carries no checks");
    };
    let judge = if check.mechanism.judge().is_deterministic() {
        "det"
    } else {
        "model"
    };
    let more = contract.checks().count().saturating_sub(1);
    let mut tail = format!(" · {} · {judge}", check.mechanism.as_str());
    if more > 0 {
        tail.push_str(&format!(" · +{more} more"));
    }
    let room = budget.saturating_sub(unicode_width::UnicodeWidthStr::width(tail.as_str()));
    format!("{}{tail}", cards::truncate_cols(&check.statement, room))
}

/// Columns the rail, the gap and the label column take before a value starts.
const GUTTER_W: usize = SUB_INDENT.len() + 2 + LABEL_W;

/// The label column's width — `contract` is the longest of the three.
const LABEL_W: usize = 10;

/// The narrowest value column worth rendering, so a hostile terminal width
/// leaves a cut sentence rather than nothing at all.
const MIN_VALUE_COLS: usize = 8;

/// One `label  value` sub-line on the card's gold rail.
fn sub_line(label: &str, value: &str, budget: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{SUB_INDENT}│ "),
            Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{label:<LABEL_W$}"), Style::new().fg(token::MUTED)),
        Span::styled(
            cards::truncate_cols(value, budget),
            Style::new().fg(token::TEXT),
        ),
    ])
}

/// SPEC 7.3's footer: `planned 6 · actual 7 · ⌥ 1 drift`, then the closing
/// sentence.
///
/// `planned` is the approved plan's own step count, which the fold has.
/// `actual` and the drift count come from [`Plan::lanes`] and elide to
/// [`ELIDED`] while nothing writes it (module docs, #5270).
///
/// The counts row is muted and the `⌥` takes drift gold, so the one cell that
/// is a *finding* is the one that is not grey. The closing sentence is dimmer
/// still: it is the card's argument, and it should not compete with the
/// numbers it is about.
#[must_use]
pub fn footer_rows(plan: &Plan) -> Vec<Line<'static>> {
    let muted = Style::new().fg(token::MUTED);
    let (actual, drift) = plan.lanes.as_ref().map_or_else(
        || (ELIDED.to_owned(), ELIDED.to_owned()),
        |lanes| {
            (
                lanes.actual.len().to_string(),
                lanes.divergences().to_string(),
            )
        },
    );
    let planned = plan
        .planned_count()
        .map_or_else(|| ELIDED.to_owned(), |n| n.to_string());
    vec![
        Line::from(vec![
            Span::styled(format!("planned {planned} · actual {actual} · "), muted),
            Span::styled(
                format!("{} {drift} drift", glyph::DRIFT),
                Style::new().fg(token::GOLD_BRIGHT),
            ),
        ]),
        Line::from(Span::styled(DRIFT_CLOSER, Style::new().fg(token::DIM))),
    ]
}

#[cfg(test)]
mod tests;
