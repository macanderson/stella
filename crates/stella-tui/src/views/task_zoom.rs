// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The task zoom (`↵` on a plan step) — SPEC 7.5, rendering `03-task-zoom`:
//!
//! ```text
//! task 3 · wire the triggers API                    ● working · lead · esc back
//!
//! done means · the contract, checked not vibed
//!   ✓ no inbound refs to the removed symbol                      graph · det
//!   ○ the automations suite is green                              unit · det
//!   2 checks · 2 deterministic · 0 model-judged · 1 pending, 0 failed
//! evidence · every event tagged task:3
//! planned vs actual · [:NEXT] is the plan · [:THEN] is what happened
//! spend · $0.12 · 41.0k tok · cache rd 71% · 2 model calls · est remain $0.05
//!
//! r re-run checks · s split task · b hand to worker · i promote to issue · ⌥ diff plan
//! a task closes when its checks pass, not when the model says so.
//! ```
//!
//! A projection and nothing else. Each block reads [`crate::plan`] and elides
//! itself **by name** when its source has not been built yet — the lanes until
//! #5037 records the plan graph, the evidence and the spend strip until #5039
//! tags events with a task id. Borrowing the session's untagged edits to fill
//! the ledger would put work on screen that this task did not do.
//!
//! The action row's five verbs are drawn and inert; [`crate::deck_ui::cards`]
//! names the issue that wires each one.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};
use stella_tui_theme::{glyph, token};
use unicode_width::UnicodeWidthStr;

use stella_protocol::{Check, CheckOutcome, Closure, Judge, TaskContract};

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::plan::{PlanLanes, PlanStep, PlanStepState, StepLedger, StepSpend};
use crate::views::cards;
use crate::views::plan_card::step_style;

/// The four lettered verbs of SPEC 7.5's action row, in its order.
///
/// The fifth, `⌥ diff plan`, is appended at render time from [`glyph::DRIFT`]
/// rather than spelled here, so the drift mark on this row is the same
/// character the lanes below draw. [`crate::deck_ui::cards`] matches these
/// same four letters, and names the issue that wires each verb.
pub const ACTION_VERBS: [(&str, &str); 4] = [
    ("r", "re-run checks"),
    ("s", "split task"),
    ("b", "hand to worker"),
    ("i", "promote to issue"),
];

/// SPEC 7.5's closing line, verbatim. It is `stella_protocol::task_contract`'s
/// whole thesis in one sentence, which is why it is the last row on the
/// surface rather than a caption inside it.
pub const CLOSING_LINE: &str = "a task closes when its checks pass, not when the model says so.";

/// The left column every block indents its rows into.
const ROW_INDENT: &str = "  ";

/// Narrowest column a wrapped tail is worth keeping; below it the row drops
/// its right-hand metadata instead of pushing it past the edge.
const MIN_TEXT_COLS: usize = 8;

/// The zoom's bands: the header, the body, and the two-row foot (the action
/// row, then [`CLOSING_LINE`]). A frame too short to hold them gets
/// zero-height bands, which every drawer below guards on — the same grammar
/// [`crate::views::installed`] uses, so moving between deck surfaces moves the
/// content and nothing else.
fn bands(area: Rect) -> (Rect, Rect, Rect) {
    let split = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(area);
    (split[0], split[2], split[3])
}

fn muted() -> Style {
    Style::new().fg(token::MUTED)
}

/// Display width of a row of spans.
fn row_cols(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum()
}

/// `left`, then `tail` right-aligned at `width`.
///
/// The tail is **dropped** rather than pushed past the edge when the two
/// cannot both fit: it is metadata, and a row whose last cells fall off the
/// terminal loses whichever half happened to be last rather than the half the
/// layout judged least important.
fn with_tail(left: Vec<Span<'static>>, tail: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let (left_w, tail_w) = (row_cols(&left), row_cols(&tail));
    if left_w + tail_w + 1 > width {
        return Line::from(left);
    }
    let mut row = left;
    row.push(Span::raw(" ".repeat(width - left_w - tail_w)));
    row.extend(tail);
    Line::from(row)
}

/// A block heading: the block's name, then a dim caption saying what its rows
/// are. One helper, so the four blocks cannot drift apart.
///
/// The caption drops to its own wrapped rows on a terminal too narrow to hold
/// both — it says what the block *is*, which is the half a reader who has
/// never seen this surface needs most.
fn heading(name: &str, caption: &str, width: usize) -> Vec<Line<'static>> {
    let bold = Style::new().fg(token::TEXT).add_modifier(Modifier::BOLD);
    let one_row = vec![
        Span::styled(name.to_string(), bold),
        Span::styled(format!(" · {caption}"), muted()),
    ];
    if row_cols(&one_row) <= width {
        return vec![Line::from(one_row)];
    }
    let mut rows = vec![Line::from(Span::styled(name.to_string(), bold))];
    rows.extend(wrapped_rows(caption, width));
    rows
}

/// Dim, indented rows carrying `text` — how every block says why it has
/// nothing to show, and how it prints its own tally.
fn wrapped_rows(text: &str, width: usize) -> Vec<Line<'static>> {
    let budget = width.saturating_sub(ROW_INDENT.len()).max(MIN_TEXT_COLS);
    cards::wrap(text, budget)
        .into_iter()
        .map(|chunk| Line::from(vec![Span::raw(ROW_INDENT), Span::styled(chunk, muted())]))
        .collect()
}

/// The `det` / `model` tag: who settles a check.
///
/// Gold on the model side because gold is money on this deck (SPEC 5) and a
/// model-judged check is the one that buys a call; a deterministic check is
/// silver and costs `$0.00`.
fn judge_tag(judge: Judge) -> Span<'static> {
    match judge {
        Judge::Deterministic => Span::styled("det", Style::new().fg(token::SILVER)),
        Judge::Model => Span::styled("model", Style::new().fg(token::GOLD)),
    }
}

/// One clause of the contract: its glyph, its statement, and the mechanism
/// that settles it right-aligned — plus, on an indented dim row, whatever the
/// judge actually saw.
///
/// The evidence row is not decoration. `CheckOutcome` carries it on both
/// settled arms precisely so "it passed" can be read as "42 tests, 0 failures"
/// by someone who was not there, and a surface that drops it hands the reader
/// back the self-report the type was built to replace.
fn check_rows(check: &Check, width: usize) -> Vec<Line<'static>> {
    let (mark, ink) = match check.outcome {
        CheckOutcome::Passed { .. } => (glyph::DONE, token::GREEN),
        CheckOutcome::Failed { .. } => (glyph::FAILED, token::RED),
        CheckOutcome::Pending => (glyph::QUEUED, token::MUTED),
    };
    let tail = vec![
        Span::styled(format!("{} · ", check.mechanism.as_str()), muted()),
        judge_tag(check.mechanism.judge()),
    ];
    let lead_w = ROW_INDENT.len() + 2;
    let statement_w = width
        .saturating_sub(lead_w + row_cols(&tail) + 1)
        .max(MIN_TEXT_COLS);
    let left = vec![
        Span::raw(ROW_INDENT),
        Span::styled(format!("{mark} "), Style::new().fg(ink)),
        Span::styled(
            cards::truncate_cols(&check.statement, statement_w),
            Style::new().fg(token::TEXT),
        ),
    ];
    let mut rows = vec![with_tail(left, tail, width)];
    let seen = match &check.outcome {
        CheckOutcome::Passed { evidence } | CheckOutcome::Failed { evidence } => Some(evidence),
        CheckOutcome::Pending => None,
    };
    if let Some(seen) = seen {
        let budget = width.saturating_sub(lead_w + 2).max(MIN_TEXT_COLS);
        for chunk in cards::wrap(seen, budget) {
            rows.push(Line::from(vec![
                Span::raw(" ".repeat(lead_w + 2)),
                Span::styled(chunk, muted()),
            ]));
        }
    }
    rows
}

/// The contract block (SPEC 7.1): what this task means by done, per check,
/// with the mechanism and the det/model tag each one carries.
///
/// The three shapes a contract can be in are three different facts and read as
/// three different blocks. No contract at all is *the board never stated one*;
/// [`TaskContract::ReadOnly`] is *this task produces no diff, so it has
/// nothing to prove*; a definition of done is the list. Collapsing the first
/// two into one empty state is how a surface ends up claiming a read was
/// verified.
pub fn contract_rows(contract: Option<&TaskContract>, width: usize) -> Vec<Line<'static>> {
    let mut rows = heading("done means", "the contract, checked not vibed", width);
    let Some(contract) = contract else {
        rows.extend(wrapped_rows(
            "the board stated no contract for this task — nothing here says what done means",
            width,
        ));
        return rows;
    };
    if matches!(contract, TaskContract::ReadOnly) {
        rows.extend(wrapped_rows(
            "read only · no contract — it produces no diff, so it closes on its own events",
            width,
        ));
        return rows;
    }
    let checks: Vec<&Check> = contract.checks().collect();
    for check in &checks {
        rows.extend(check_rows(check, width));
    }
    let deterministic = checks
        .iter()
        .filter(|c| c.mechanism.judge().is_deterministic())
        .count();
    let mut tally = format!(
        "{} checks · {deterministic} deterministic · {} model-judged",
        checks.len(),
        checks.len() - deterministic,
    );
    // Closure is derived from the checks on every read
    // (`TaskContract::closure`) — there is no field anywhere a caller can set
    // to mean done — and this row is that guarantee said out loud.
    match contract.closure() {
        Closure::Earned => tally.push_str(" · every check passed · it may close"),
        Closure::Outstanding { pending, failed } => tally.push_str(&format!(
            " · {pending} pending, {failed} failed · it may not close"
        )),
        Closure::NotContracted => {}
    }
    rows.extend(wrapped_rows(&tally, width));
    rows
}

/// The evidence block (SPEC 7.1): every event tagged with this task's id.
///
/// The empty state names its reason rather than printing a blank list, because
/// "nothing happened" and "nothing is tagged" are opposite claims and today
/// only the second one is true.
pub fn evidence_rows(id: &str, ledger: Option<&StepLedger>, width: usize) -> Vec<Line<'static>> {
    let mut rows = heading("evidence", &format!("every event tagged task:{id}"), width);
    let evidence = ledger.map_or(&[][..], |l| l.evidence.as_slice());
    if evidence.is_empty() {
        rows.extend(wrapped_rows(
            "no event carries a task id yet, so this ledger is empty",
            width,
        ));
        return rows;
    }
    for row in evidence {
        let tail = vec![Span::styled(
            row.outcome.clone(),
            Style::new().fg(token::SILVER),
        )];
        let kind_w = 7;
        let subject_w = width
            .saturating_sub(ROW_INDENT.len() + kind_w + row_cols(&tail) + 1)
            .max(MIN_TEXT_COLS);
        let left = vec![
            Span::raw(ROW_INDENT),
            Span::styled(format!("{:<kind_w$}", row.kind.label()), muted()),
            Span::styled(
                cards::truncate_cols(&row.subject, subject_w),
                Style::new().fg(token::TEXT),
            ),
        ];
        rows.push(with_tail(left, tail, width));
    }
    rows
}

/// One lane of the planned-vs-actual block: the label, then the path, wrapped
/// under itself rather than elided — a path with its tail cut off reads as
/// shorter than the path was.
fn lane_rows(label: &str, path: &str, width: usize) -> Vec<Line<'static>> {
    let indent = ROW_INDENT.len() + 9;
    let budget = width.saturating_sub(indent).max(MIN_TEXT_COLS);
    cards::wrap(path, budget)
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let lead = if i == 0 {
                format!("{ROW_INDENT}{label:<9}")
            } else {
                " ".repeat(indent)
            };
            Line::from(vec![
                Span::styled(lead, muted()),
                Span::styled(chunk, Style::new().fg(token::TEXT)),
            ])
        })
        .collect()
}

/// The planned-vs-actual block (SPEC 7.4).
///
/// Both lanes come from the plan graph — planned from `[:NEXT]`, actual from
/// `[:THEN]`. Neither is recoverable from the task board, which holds only the
/// path that survived, so a plan with no graph behind it gets the elision
/// instead of the board drawn twice under two headings.
pub fn lane_block_rows(lanes: Option<&PlanLanes>, width: usize) -> Vec<Line<'static>> {
    let mut rows = heading(
        "planned vs actual",
        "[:NEXT] is the plan · [:THEN] is what happened",
        width,
    );
    let Some(lanes) = lanes else {
        rows.extend(wrapped_rows(
            "no plan graph yet — the planned and actual paths are not recorded",
            width,
        ));
        return rows;
    };
    rows.extend(lane_rows("planned", &lanes.planned.join(" → "), width));
    let actual = lanes
        .actual
        .iter()
        .map(|step| {
            if step.is_drift() {
                format!("{} {}", glyph::DRIFT, step.title)
            } else {
                step.title.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" → ");
    rows.extend(lane_rows("actual", &actual, width));

    let divergences = lanes.divergences();
    let footer = if divergences == 0 {
        "no divergence · the plan held".to_string()
    } else {
        let causes: Vec<&str> = lanes
            .actual
            .iter()
            .filter_map(|s| s.cause.as_deref())
            .collect();
        format!(
            "{divergences} divergence · cause {} · recorded as a trace outcome",
            causes.join(" · "),
        )
    };
    rows.extend(wrapped_rows(&footer, width));
    rows
}

/// The spend strip (SPEC 7.1's third part): `$ · tok · cache rd% · model calls
/// · est remain`.
///
/// Never the session's own figures wearing a task's heading. Cost is
/// attributed per session and per turn today (`receipts.rs`, `budget.rs`) and
/// per task nowhere, so a strip filled from the session would price this task
/// at everything every other task spent.
pub fn spend_rows(spend: Option<&StepSpend>, width: usize) -> Vec<Line<'static>> {
    let mut rows = heading("spend", "what this task has cost so far", width);
    let Some(spend) = spend else {
        rows.extend(wrapped_rows(
            "cost is not attributed by task yet — the session's total is not this task's",
            width,
        ));
        return rows;
    };
    let mut parts = vec![
        format!("${:.2}", spend.usd),
        format!("{} tok", crate::textline::fmt_tokens(spend.tokens)),
        format!("cache rd {}%", spend.cache_read_pct),
        format!("{} model calls", spend.model_calls),
    ];
    if let Some(remaining) = spend.est_remaining_usd {
        parts.push(format!("est remain ${remaining:.2}"));
    }
    // Money renders gold and everything beside it is a measurement, so the
    // dollar figure is the one span that takes the accent (SPEC 5).
    let mut spans = vec![
        Span::raw(ROW_INDENT),
        Span::styled(parts.remove(0), Style::new().fg(token::GOLD)),
    ];
    let joined = parts.join(" · ");
    let budget = width
        .saturating_sub(row_cols(&spans) + 3)
        .max(MIN_TEXT_COLS);
    for (i, chunk) in cards::wrap(&joined, budget).into_iter().enumerate() {
        if i == 0 {
            spans.push(Span::styled(" · ", muted()));
            spans.push(Span::styled(chunk, Style::new().fg(token::SILVER)));
            rows.push(Line::from(std::mem::take(&mut spans)));
        } else {
            rows.push(Line::from(vec![
                Span::raw(ROW_INDENT),
                Span::styled(chunk, Style::new().fg(token::SILVER)),
            ]));
        }
    }
    rows
}

/// The whole body, in SPEC 7.5's order: contract, evidence, lanes, spend.
///
/// Pure over the fold, so every block above is testable without a terminal.
/// What each elision claims is a sentence rather than a pixel, and a sentence
/// is only checkable where it can be read back as a string.
pub fn body_rows(
    step: &PlanStep,
    ledger: Option<&StepLedger>,
    lanes: Option<&PlanLanes>,
    width: usize,
) -> Vec<Line<'static>> {
    let mut rows = contract_rows(step.contract.as_ref(), width);
    for block in [
        evidence_rows(&step.id, ledger, width),
        lane_block_rows(lanes, width),
        spend_rows(ledger.and_then(|l| l.spend.as_ref()), width),
    ] {
        rows.push(Line::default());
        rows.extend(block);
    }
    rows
}

/// The word beside the state glyph — never the glyph alone (SPEC 2, SPEC 13).
fn state_word(step: &PlanStep) -> String {
    match step.state {
        PlanStepState::Planned => "queued".to_string(),
        PlanStepState::Started => "working".to_string(),
        PlanStepState::Verify => "verify".to_string(),
        PlanStepState::Complete => "done".to_string(),
        // A cancelled step carries its reason in the note, and "blocked" would
        // read as a gate stopping it when it was a decision.
        PlanStepState::Blocked => step.note.clone().unwrap_or_else(|| "blocked".to_string()),
        PlanStepState::DriftInserted => "inserted".to_string(),
    }
}

/// The header row: which task this is, what it is doing, and the way out.
fn header_row(step: &PlanStep, model_id: Option<&str>, width: usize) -> Line<'static> {
    // The zoom is a still surface, so the working step takes its bright frame
    // rather than the plan card's pulse: `animate` off, clock at zero.
    let visual = step_style::step_visual(step.state, 0, false);
    let mut right = format!("{} {}", visual.glyph, state_word(step));
    if let Some(owner) = &step.owner {
        right.push_str(&format!(" · {owner}"));
    }
    if let Some(model_id) = model_id {
        right.push_str(&format!(" · {model_id}"));
    }
    right.push_str(" · esc back");
    let tail = vec![Span::styled(right, muted())];
    let id = format!("task {} · ", step.id);
    let title_w = width
        .saturating_sub(id.len() + row_cols(&tail) + 1)
        .max(MIN_TEXT_COLS);
    let left = vec![
        Span::styled(
            id,
            Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            cards::truncate_cols(&step.title, title_w),
            Style::new().fg(token::TEXT),
        ),
    ];
    with_tail(left, tail, width)
}

/// The action row, then [`CLOSING_LINE`].
fn render_foot(area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    let dim = Style::new().fg(token::DIM);
    let mut spans = Vec::new();
    for (i, (key, label)) in ACTION_VERBS
        .iter()
        .map(|(k, l)| ((*k).to_string(), *l))
        .chain(std::iter::once((glyph::DRIFT.to_string(), "diff plan")))
        .enumerate()
    {
        if i > 0 {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled(key, muted()));
        spans.push(Span::styled(format!(" {label}"), dim));
    }
    Paragraph::new(vec![
        Line::from(spans),
        Line::from(Span::styled(CLOSING_LINE, muted())),
    ])
    .render(area, buf);
}

/// Draw the zoom over `area` — the deck's content band, which it owns
/// entirely while it is up.
pub fn render(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    // The zoom replaces the tab body rather than floating above it, so it
    // repaints the ground and clears whatever the tab drew.
    Clear.render(area, buf);
    buf.set_style(area, Style::new().bg(token::BG).fg(token::TEXT));
    let (head_area, body_area, foot_area) = bands(area);
    let width = area.width as usize;

    let steps = model
        .agents
        .get(ui.focused)
        .map(|agent| agent.model.plan.steps())
        .unwrap_or_default();
    let Some(step) = steps
        .get(ui.cards.plan_sel)
        .or_else(|| steps.last())
        .cloned()
    else {
        render_nothing_to_zoom(
            "this turn has no plan yet — there is no task to zoom",
            head_area,
            buf,
        );
        return;
    };
    // Unwrapped only after the step above proved there is an agent to read.
    let agent = &model.agents[ui.focused];
    let plan = &agent.model.plan;

    if head_area.height > 0 {
        Paragraph::new(header_row(&step, agent.meta.model.as_deref(), width))
            .render(head_area, buf);
    }
    if body_area.height > 0 {
        let rows = body_rows(&step, plan.ledger.get(&step.id), plan.lanes.as_ref(), width);
        cards::render_body(rows, None, body_area, buf);
    }
    render_foot(foot_area, buf);
}

/// The zoom with nothing to zoom on: one centred sentence, rather than four
/// empty headings pretending there is a task behind them.
fn render_nothing_to_zoom(reason: &str, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    Paragraph::new(reason)
        .style(muted())
        .alignment(Alignment::Center)
        .render(Rect { height: 1, ..area }, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{ActualStep, EvidenceKind, EvidenceRow};
    use stella_protocol::{CheckKind, CheckMechanism, DefinitionOfDone};

    fn text_of(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn joined(rows: &[Line<'static>]) -> String {
        rows.iter().map(text_of).collect::<Vec<_>>().join("\n")
    }

    fn step() -> PlanStep {
        PlanStep {
            id: "3".into(),
            title: "wire the triggers API".into(),
            detail: None,
            state: PlanStepState::Started,
            owner: Some("lead".into()),
            note: None,
            contract: None,
        }
    }

    fn contract() -> TaskContract {
        let mut passed = Check::new(
            "no inbound refs to the removed symbol",
            CheckMechanism::Known(CheckKind::Graph),
        );
        passed.outcome = CheckOutcome::Passed {
            evidence: "0 inbound edges".into(),
        };
        TaskContract::DefinitionOfDone(DefinitionOfDone::new(
            passed,
            vec![
                Check::new(
                    "the automations suite is green",
                    CheckMechanism::Known(CheckKind::Unit),
                ),
                Check::new(
                    "the migration reads as reversible",
                    CheckMechanism::new("vera:reversibility", Judge::Model),
                ),
            ],
        ))
    }

    fn lanes() -> PlanLanes {
        PlanLanes {
            planned: vec!["read sources".into(), "implement".into(), "test".into()],
            actual: vec![
                ActualStep {
                    title: "read sources".into(),
                    cause: None,
                },
                ActualStep {
                    title: "implement".into(),
                    cause: None,
                },
                ActualStep {
                    title: "fix borrow err".into(),
                    cause: Some("E0502 borrow".into()),
                },
                ActualStep {
                    title: "test".into(),
                    cause: None,
                },
            ],
        }
    }

    /// **The claim this surface exists to make.** Every clause of the contract
    /// is on screen with the mechanism that settles it and whether a machine
    /// or a model decides — the split SPEC 1's first thesis prices.
    #[test]
    fn every_check_carries_its_mechanism_and_its_det_or_model_tag() {
        let text = joined(&contract_rows(Some(&contract()), 90));
        assert!(
            text.contains("no inbound refs to the removed symbol"),
            "{text}"
        );
        assert!(text.contains("graph · det"), "{text}");
        assert!(text.contains("unit · det"), "{text}");
        assert!(
            text.contains("vera:reversibility · model"),
            "a contributed mechanism keeps its own name and its declared judge:\n{text}"
        );
        assert!(
            text.contains("3 checks · 2 deterministic · 1 model-judged"),
            "{text}"
        );
    }

    /// A settled check shows what the judge saw. "It passed" is a claim;
    /// "0 inbound edges" is the thing somebody else can re-check.
    #[test]
    fn a_settled_check_shows_the_evidence_the_judge_saw() {
        let text = joined(&contract_rows(Some(&contract()), 90));
        assert!(text.contains("0 inbound edges"), "{text}");
    }

    /// Closure is derived, so this row can only say "it may close" once the
    /// checks actually pass.
    #[test]
    fn the_contract_says_whether_the_task_may_close() {
        let outstanding = joined(&contract_rows(Some(&contract()), 90));
        assert!(outstanding.contains("2 pending, 0 failed"), "{outstanding}");
        assert!(outstanding.contains("it may not close"), "{outstanding}");

        let mut earned = contract();
        if let TaskContract::DefinitionOfDone(dod) = &mut earned {
            for check in dod.iter_mut() {
                check.outcome = CheckOutcome::Passed {
                    evidence: "seen".into(),
                };
            }
        }
        let text = joined(&contract_rows(Some(&earned), 90));
        assert!(text.contains("every check passed · it may close"), "{text}");
    }

    /// The two ways a task can have no check list are different facts and read
    /// differently: a read-only task has nothing to prove, and a task whose
    /// board stated no contract has nothing saying what done means.
    #[test]
    fn no_contract_and_read_only_are_not_the_same_empty_state() {
        let none = joined(&contract_rows(None, 90));
        assert!(none.contains("the board stated no contract"), "{none}");
        let read_only = joined(&contract_rows(Some(&TaskContract::ReadOnly), 90));
        assert!(read_only.contains("read only · no contract"), "{read_only}");
    }

    /// **The elision.** No plan graph means the lane block says so; it never
    /// redraws the board and calls one copy the plan (#5037).
    #[test]
    fn the_lanes_are_elided_by_name_when_no_plan_graph_recorded_them() {
        let text = joined(&lane_block_rows(None, 90));
        assert!(text.contains("no plan graph yet"), "{text}");
        assert!(text.contains("not recorded"), "{text}");
    }

    /// With a graph behind it, both lanes render and every divergence carries
    /// its cause (SPEC 7.4).
    #[test]
    fn a_recorded_plan_graph_renders_both_lanes_and_names_the_cause() {
        let text = joined(&lane_block_rows(Some(&lanes()), 90));
        assert!(text.contains("planned"), "{text}");
        assert!(text.contains("read sources → implement → test"), "{text}");
        assert!(
            text.contains(&format!("{} fix borrow err", glyph::DRIFT)),
            "the inserted step wears the drift mark:\n{text}"
        );
        assert!(text.contains("1 divergence · cause E0502 borrow"), "{text}");
    }

    /// A plan that ran as written says so, rather than leaving the reader to
    /// compare two lanes by eye.
    #[test]
    fn a_plan_that_held_reports_no_divergence() {
        let mut lanes = lanes();
        lanes.actual.retain(|s| !s.is_drift());
        let text = joined(&lane_block_rows(Some(&lanes), 90));
        assert!(text.contains("no divergence · the plan held"), "{text}");
    }

    /// The evidence block never borrows the session's untagged edits: with no
    /// ledger it says why it is empty (#5039).
    #[test]
    fn the_evidence_block_says_why_it_is_empty() {
        let text = joined(&evidence_rows("3", None, 90));
        assert!(text.contains("every event tagged task:3"), "{text}");
        assert!(text.contains("no event carries a task id yet"), "{text}");
    }

    #[test]
    fn a_populated_ledger_lists_each_event_with_its_outcome() {
        let ledger = StepLedger {
            evidence: vec![
                EvidenceRow {
                    kind: EvidenceKind::Edit,
                    subject: "crates/stella-cli/src/self_driving_cmd.rs".into(),
                    outcome: "+41 -6".into(),
                },
                EvidenceRow {
                    kind: EvidenceKind::Run,
                    subject: "cargo test digest".into(),
                    outcome: "2/4".into(),
                },
            ],
            spend: None,
        };
        let text = joined(&evidence_rows("3", Some(&ledger), 90));
        assert!(text.contains("edit   crates/stella-cli"), "{text}");
        assert!(text.contains("+41 -6"), "{text}");
        assert!(text.contains("run    cargo test digest"), "{text}");
    }

    /// The spend strip prices the task or admits it cannot — it never shows
    /// the session's total under a task's heading.
    #[test]
    fn the_spend_strip_prices_the_task_or_admits_it_cannot() {
        let empty = joined(&spend_rows(None, 90));
        assert!(empty.contains("not attributed by task yet"), "{empty}");
        let spend = StepSpend {
            usd: 0.12,
            tokens: 41_000,
            cache_read_pct: 71,
            model_calls: 2,
            est_remaining_usd: Some(0.05),
        };
        let text = joined(&spend_rows(Some(&spend), 90));
        assert!(
            text.contains("$0.12 · 41.0k tok · cache rd 71% · 2 model calls · est remain $0.05"),
            "{text}"
        );
    }

    /// SPEC 7.5's order, in one body: contract, evidence, lanes, spend.
    #[test]
    fn the_body_carries_all_four_blocks_in_spec_order() {
        let mut step = step();
        step.contract = Some(contract());
        let text = joined(&body_rows(&step, None, None, 90));
        let mut at = 0usize;
        for name in ["done means", "evidence", "planned vs actual", "spend"] {
            let found = text[at..]
                .find(name)
                .unwrap_or_else(|| panic!("{name:?} is missing or out of order in:\n{text}"));
            at += found + name.len();
        }
    }

    /// Every row fits the width it was given. The zoom is a full-width surface
    /// with right-aligned tails on three of its blocks, and a row that
    /// overflows loses whichever half happened to be last.
    #[test]
    fn no_row_overflows_the_width_it_was_given() {
        let mut step = step();
        step.contract = Some(contract());
        let ledger = StepLedger {
            evidence: vec![EvidenceRow {
                kind: EvidenceKind::GraphWrite,
                subject: "3 nodes · Seen, dedup key".into(),
                outcome: "wr".into(),
            }],
            spend: Some(StepSpend {
                usd: 0.12,
                tokens: 41_000,
                cache_read_pct: 71,
                model_calls: 2,
                est_remaining_usd: Some(0.05),
            }),
        };
        for width in [30usize, 40, 60, 80, 160] {
            let mut rows = body_rows(&step, Some(&ledger), Some(&lanes()), width);
            rows.push(header_row(&step, Some("kimi-k3"), width));
            for row in rows {
                let w = UnicodeWidthStr::width(text_of(&row).as_str());
                assert!(w <= width, "{w} > {width}: {:?}", text_of(&row));
            }
        }
    }

    /// The state word rides beside the glyph, always — colour and shape are
    /// never the only carriers (SPEC 2, SPEC 13).
    #[test]
    fn the_header_names_the_state_in_words() {
        let text = text_of(&header_row(&step(), Some("kimi-k3"), 100));
        assert!(text.contains("task 3"), "{text}");
        assert!(text.contains("working"), "{text}");
        assert!(text.contains("kimi-k3"), "{text}");
        assert!(text.contains("esc back"), "{text}");
    }
}
