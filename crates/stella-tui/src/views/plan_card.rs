// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The plan card (`/plan`): the whole plan, **readable**.
//!
//! # What this is for
//!
//! The rail ([`crate::views::plan_rail`]) is a quarter of the frame wide, so a
//! step there is a headline: a ring, an ordinal, and as much of the title as
//! fits. That is the right density for something on screen the whole time, and
//! the wrong one for the moment a user actually asks *what is step 3*.
//!
//! This card is that moment. Every step at full width with its elaboration
//! underneath, then the plan's operating envelope — where it may write, what it
//! may spend, which models are routed where, and what will count as done.
//!
//! It supersedes three cards that each showed a slice of this and none of which
//! showed a step's text: `/scope` (the envelope, no steps), `/tasks` (a board
//! nothing populated), and `/witness` (the verification records, now the rail's
//! bottom panel).
//!
//! Post-approval the envelope is read-only: the title says `locked · e to
//! edit`, and `e` *proposes* a change as a `WorkspaceInput` out to the driver —
//! the card never edits locally, so what it shows is always the plan actually
//! in force.
//!
//! # Copy law (D6)
//!
//! Plan, plan step, done verification. Model routing renders as the three
//! labeled slots `think` / `work` / `verify`; the internal pipeline role
//! identifiers never reach a rendered string.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use stella_protocol::ScopeProposal;

use crate::deck::{AgentEntry, PipelineRole, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::plan::{Plan, PlanState, PlanStepState};
use crate::theme;
use crate::views::cards;

/// The literal done-when contract — product copy, verbatim (D5), split at its
/// natural break because the whole line is wider than the card.
const DONE_WHEN_FLIP: &str = "oracle flips red → green";
/// The contract's second half, on the continuation row.
const DONE_WHEN_PROOF: &str = "(confirmed from evidence, not self-report)";

/// Width of the dimmed left label column.
const LABEL_W: usize = 10;

/// One labeled grid row: dim fixed-width label, then the value spans.
fn grid_row(label: &str, value: Vec<Span<'static>>, accessible: bool) -> Line<'static> {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    if accessible {
        // Labeled record, no column alignment: `· label value`.
        let mut spans = vec![Span::styled(format!("· {label} "), dim)];
        spans.extend(value);
        return Line::from(spans);
    }
    let mut spans = vec![Span::styled(format!("{label:<LABEL_W$}"), dim)];
    spans.extend(value);
    Line::from(spans)
}

/// The value spans for the `models` row: `think <id>` dim · `work <id>` accent
/// · `verify <id>` dim.
fn models_value(model: &WorkspaceModel) -> Vec<Span<'static>> {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let slot = |role: PipelineRole| -> String {
        model
            .role_pins
            .get(&role)
            .map(|pin| pin.model.clone())
            .unwrap_or_else(|| "—".to_string())
    };
    vec![
        Span::styled(format!("think {}", slot(PipelineRole::Triage)), dim),
        Span::styled(" · ", dim),
        Span::styled(
            format!("work {}", slot(PipelineRole::Worker)),
            theme::accent(),
        ),
        Span::styled(" · ", dim),
        Span::styled(format!("verify {}", slot(PipelineRole::Judge)), dim),
    ]
}

/// The readable step list: one row per step, plus an indented detail row
/// wherever the planner or the worker wrote one.
///
/// This is the half the old cards never had. `width` is the card's interior, so
/// a detail longer than the card wraps onto continuation rows rather than being
/// elided — the point of opening this card is to read the words.
///
/// Returns the rows and the *row index* of `selected` (a step index), which the
/// caller needs for the selection tint — a step can occupy several rows once
/// its detail wraps, so the two indices are not the same number.
pub fn step_rows(
    plan: &Plan,
    width: usize,
    selected: Option<usize>,
) -> (Vec<Line<'static>>, Option<usize>) {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let steps = plan.steps();
    if steps.is_empty() {
        return (
            vec![Line::from(Span::styled(
                match plan.state {
                    PlanState::Cancelled => "this plan was cancelled",
                    _ => "no plan has been proposed for this turn",
                },
                dim,
            ))],
            None,
        );
    }
    let mut rows = Vec::new();
    let mut selected_row = None;
    for (i, step) in steps.iter().enumerate() {
        if Some(i) == selected {
            selected_row = Some(rows.len());
        }
        let (glyph, ring, text) = match step.state {
            PlanStepState::Planned => {
                let grey = matches!(plan.state, PlanState::Draft | PlanState::PendingApproval);
                let fg = if grey {
                    theme::TEXT_TERTIARY
                } else {
                    theme::TEXT_PRIMARY
                };
                ("○", Style::new().fg(fg), Style::new().fg(fg))
            }
            PlanStepState::Started => (
                "●",
                Style::new().fg(theme::VIOLET).add_modifier(Modifier::BOLD),
                Style::new()
                    .fg(theme::TEXT_PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ),
            PlanStepState::Complete => (
                "●",
                Style::new().fg(theme::OK),
                Style::new()
                    .fg(theme::TEXT_TERTIARY)
                    .add_modifier(Modifier::CROSSED_OUT),
            ),
            PlanStepState::Error => (
                "●",
                Style::new().fg(theme::DANGER).add_modifier(Modifier::BOLD),
                Style::new().fg(theme::TEXT_SECONDARY),
            ),
        };
        let mut spans = vec![
            cards::marker(Some(i) == selected),
            Span::styled(format!("{glyph} "), ring),
            Span::styled(format!("{:<3}", step.id), dim),
            Span::styled(step.title.clone(), text),
        ];
        if let Some(note) = &step.note {
            spans.push(Span::styled(format!("  ({note})"), dim));
        } else if let Some(owner) = &step.owner {
            spans.push(Span::styled(format!("  ({owner})"), dim));
        }
        rows.push(Line::from(spans));
        // The elaboration, wrapped under the title at the title's indent.
        if let Some(detail) = &step.detail {
            let indent = 7usize;
            for chunk in wrap(detail, width.saturating_sub(indent).max(8)) {
                rows.push(Line::from(vec![
                    Span::raw(" ".repeat(indent)),
                    Span::styled(chunk, dim),
                ]));
            }
        }
    }
    (rows, selected_row)
}

/// Greedy word wrap. The card is fixed-width and the details are free text from
/// the model, so a mid-word cut would be the common case, not the edge one.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if line.is_empty() {
            line.push_str(word);
        } else if line.chars().count() + 1 + word.chars().count() <= width {
            line.push(' ');
            line.push_str(word);
        } else {
            out.push(std::mem::take(&mut line));
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// The operating-envelope grid: where the plan may write, what it may spend,
/// how it is routed, and what will count as done. Pure over the fold, so the
/// labels and copy are unit-testable without a buffer.
pub fn grid_rows(
    model: &WorkspaceModel,
    agent: &AgentEntry,
    proposal: &ScopeProposal,
    accessible: bool,
) -> Vec<Line<'static>> {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let primary = Style::new().fg(theme::TEXT_PRIMARY);
    let val = |text: String| vec![Span::styled(text, primary)];
    let dash = "—".to_string();
    let mut rows = Vec::new();

    let mut repo = proposal.repo.clone().unwrap_or_else(|| dash.clone());
    if let Some(branch) = &proposal.branch {
        repo.push_str(&format!(" ⎇ {branch}"));
    }
    rows.push(grid_row("repo", val(repo), accessible));
    let globs = |globs: &[String]| -> String {
        if globs.is_empty() {
            dash.clone()
        } else {
            format!("{}  ({} globs)", globs.join(" · "), globs.len())
        }
    };
    rows.push(grid_row(
        "write",
        val(globs(&proposal.write_globs)),
        accessible,
    ));
    rows.push(grid_row(
        "read",
        val(globs(&proposal.read_globs)),
        accessible,
    ));

    // Budget: spent (success — money spent is a fact) `of $cap` dim, plus a
    // mini fraction bar. The cap is the agent's own metered limit.
    let mut budget: Vec<Span<'static>> = vec![Span::styled(
        format!("${:.2}", agent.cost_usd),
        Style::new().fg(theme::SUCCESS_BRIGHT),
    )];
    if let Some(cap) = agent.model.hud.limit_usd.filter(|cap| *cap > 0.0) {
        budget.push(Span::styled(format!(" of ${cap:.2} "), dim));
        if !accessible {
            let pct = ((agent.cost_usd / cap).clamp(0.0, 1.0) * 100.0).round() as usize;
            budget.extend(cards::mini_fraction_bar(pct, 100, 7, theme::SUCCESS_BRIGHT));
        }
    } else if let Some(estimate) = proposal.estimated_cost_usd {
        budget.push(Span::styled(format!(" · est ${estimate:.2}"), dim));
    }
    rows.push(grid_row("budget", budget, accessible));

    rows.push(grid_row("models", models_value(model), accessible));
    rows.push(grid_row(
        "shell",
        val(proposal.shell_policy.clone().unwrap_or(dash)),
        accessible,
    ));
    // The done-when contract is wider than the card, so it wraps at its natural
    // break onto a labeled row plus an indented continuation, never a mid-token
    // elision.
    rows.push(grid_row(
        "done when",
        vec![Span::styled(DONE_WHEN_FLIP, dim)],
        accessible,
    ));
    rows.push(if accessible {
        Line::from(Span::styled(format!("· {DONE_WHEN_PROOF}"), dim))
    } else {
        Line::from(vec![
            Span::raw(" ".repeat(LABEL_W)),
            Span::styled(DONE_WHEN_PROOF, dim),
        ])
    });
    rows
}

/// Render the plan card over `frame` for the focused agent.
pub fn render(model: &WorkspaceModel, ui: &DeckUi, frame: Rect, buf: &mut Buffer) {
    let Some(agent) = model.agents.get(ui.focused) else {
        return;
    };
    let sm = &agent.model;
    let plan = &sm.plan;
    let inner_w = (cards::CARD_MAX_W - 4) as usize;

    let mut rows: Vec<Line<'static>> = Vec::new();
    if !plan.summary.is_empty() {
        rows.push(Line::from(Span::styled(
            cards::truncate_cols(&plan.summary, inner_w),
            Style::new()
                .fg(theme::TEXT_PRIMARY)
                .add_modifier(Modifier::BOLD),
        )));
        rows.push(Line::default());
    }
    let step_count = plan.steps().len();
    let selected = (step_count > 0).then(|| ui.cards.plan_sel.min(step_count - 1));
    let offset = rows.len();
    let (step_lines, selected_row) = step_rows(plan, inner_w, selected);
    rows.extend(step_lines);
    let selected_row = selected_row.map(|r| r + offset);

    // The envelope, when a proposal supplied one. A board-only plan (the worker
    // planned as it went) has no globs or routing to state, and inventing an
    // empty grid for it would read as "unrestricted".
    let envelope = sm
        .pending_scope_review
        .as_ref()
        .or(sm.approved_scope.as_ref());
    if let Some(proposal) = envelope {
        rows.push(Line::default());
        rows.extend(grid_rows(model, agent, proposal, ui.accessible));
    }

    let (done, total) = plan.progress();
    let context = vec![Span::styled(
        match plan.state {
            PlanState::PendingApproval => {
                format!("{} · {total} steps", plan.state.label())
            }
            PlanState::Draft => plan.state.label().to_string(),
            _ => format!("{} {done}/{total}", plan.state.label()),
        },
        Style::new().fg(theme::TEXT_TERTIARY),
    )];
    let mut verbs: Vec<&str> = Vec::new();
    if crate::deck_ui::cards::selected_step_skippable(model, ui) {
        verbs.push("x skip");
    }
    if envelope.is_some() && plan.state != PlanState::PendingApproval {
        verbs.push("e edit · locked");
    }
    verbs.push("esc close");
    let hints = verbs.join(" · ");
    let area = cards::card_area(frame, rows.len() as u16, cards::CARD_MAX_W, ui.accessible);
    let inner = cards::card_frame(area, "plan", context, &hints, buf);
    cards::render_body(rows, selected_row, inner, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Plan;
    use stella_protocol::{TaskItem, TaskStatus};

    fn proposal(steps: &[&str]) -> ScopeProposal {
        ScopeProposal {
            summary: "collapse the rail surfaces".into(),
            steps: steps.iter().map(|s| (*s).to_string()).collect(),
            estimated_files: 9,
            estimated_cost_usd: Some(1.40),
            ..Default::default()
        }
    }

    fn text_of(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// **The complaint this card answers.** "Today I can't read what you are
    /// proposing at all" — every step's text, at full width, plus its detail.
    #[test]
    fn every_step_is_readable_with_its_elaboration() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["read the band layout", "fold the rail"]));
        plan.approve();
        plan.apply_board(&[TaskItem {
            id: "1".into(),
            subject: "read the band layout".into(),
            description: Some(
                "each panel declared a fixed height before the transcript got its leftovers".into(),
            ),
            status: TaskStatus::InProgress,
            owner: None,
        }]);
        let text: String = step_rows(&plan, 52, None)
            .0
            .iter()
            .map(text_of)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("read the band layout"), "{text}");
        assert!(text.contains("fold the rail"), "{text}");
        assert!(
            text.contains("declared a fixed height"),
            "the detail must be readable:\n{text}"
        );
    }

    /// A detail longer than the card wraps rather than eliding: the whole point
    /// of opening the card is to read the words.
    #[test]
    fn a_long_detail_wraps_instead_of_being_cut() {
        let long = "the rail was gated on bad news so a healthy turn rendered zero rows \
                    which is why the indicators never appeared to update at all";
        let mut plan = Plan::default();
        plan.apply_board(&[TaskItem {
            id: "1".into(),
            subject: "explain the gate".into(),
            description: Some(long.into()),
            status: TaskStatus::Pending,
            owner: None,
        }]);
        let (rows, _) = step_rows(&plan, 40, None);
        assert!(rows.len() > 2, "the detail wrapped onto its own rows");
        // Re-joined on single spaces: the wrap indents every continuation row,
        // so a phrase that survived the break is only recognizable once the
        // layout whitespace is normalized back out.
        let text = rows
            .iter()
            .flat_map(|r| {
                text_of(r)
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!text.contains('…'), "nothing was elided:\n{text}");
        assert!(text.contains("never appeared to update"), "{text}");
        for row in &rows {
            assert!(
                text_of(row).chars().count() <= 40,
                "a wrapped row overflowed: {:?}",
                text_of(row)
            );
        }
    }

    #[test]
    fn an_unproposed_plan_says_so_rather_than_showing_an_empty_list() {
        let text = text_of(&step_rows(&Plan::default(), 52, None).0[0]);
        assert!(text.contains("no plan has been proposed"), "{text}");
    }

    /// D6: this surface's words are plan words. The other tools' vocabulary and
    /// the internal role names never reach it.
    #[test]
    fn the_card_never_speaks_another_tools_vocabulary() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one"]));
        plan.approve();
        let text: String = step_rows(&plan, 52, None)
            .0
            .iter()
            .map(text_of)
            .collect::<Vec<_>>()
            .join("\n")
            .to_lowercase();
        for banned in ["task", "scope", "issue", "witness", "judge"] {
            assert!(!text.contains(banned), "leaked {banned:?} in:\n{text}");
        }
    }
}
