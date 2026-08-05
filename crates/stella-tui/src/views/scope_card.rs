//! The scope card v2 (`/scope`): the run's scope as a labeled grid — repo,
//! write/read globs, budget, model routing, shell policy, and the done-when
//! contract — rendered from the folded [`ScopeProposal`] (the pending gate's
//! proposal while one waits, else the approved scope the model retains for
//! the rest of the turn).
//!
//! Post-approval the card is read-only: the title says `locked at plan · e
//! to edit`, and `e` *proposes* a scope change as a `WorkspaceInput` out to
//! the driver — the card never edits locally, so what it shows is always
//! the scope actually in force.
//!
//! Model routing renders as the three labeled slots `think` / `work` /
//! `verify` (the statline's MODELS row is gone; this card and `/models` are
//! where routing lives now). The labels are this surface's own vocabulary —
//! the internal pipeline role identifiers never reach a rendered string.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use stella_protocol::ScopeProposal;

use crate::deck::{AgentEntry, PipelineRole, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::theme;
use crate::views::cards;

/// The literal done-when contract — product copy, verbatim (D5), split at
/// its natural break because the whole line is wider than the card.
const DONE_WHEN_FLIP: &str = "oracle flips red → green";
/// The contract's second half, on the continuation row.
const DONE_WHEN_PROOF: &str = "(witness confirms from evidence)";

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

/// The value spans for the `models` row: `think <id>` dim · `work <id>`
/// accent · `verify <id>` dim.
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
        Span::styled(format!("verify {}", slot(PipelineRole::Verifier)), dim),
    ]
}

/// The grid rows for one agent's scope. Pure over the fold, so the labels
/// and copy are unit-testable without a buffer.
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
    // The done-when contract is 58 columns of product copy — wider than the
    // card's interior — so it wraps at its natural break onto a labeled row
    // plus an indented continuation, never a mid-token elision.
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

/// Render the scope card over `frame` for the focused agent.
pub fn render(model: &WorkspaceModel, ui: &DeckUi, frame: Rect, buf: &mut Buffer) {
    let Some(agent) = model.agents.get(ui.focused) else {
        return;
    };
    let pending = agent.model.pending_scope_review.as_ref();
    let Some(proposal) = pending.or(agent.model.approved_scope.as_ref()) else {
        // No scope yet: an honest one-row card beats an empty grid.
        let area = cards::card_area(frame, 1, cards::CARD_MAX_W, ui.accessible);
        let inner = cards::card_frame(area, "scope", Vec::new(), "esc close", buf);
        cards::render_body(
            vec![Line::from(Span::styled(
                "no scope proposed yet",
                Style::new().fg(theme::TEXT_TERTIARY),
            ))],
            None,
            inner,
            buf,
        );
        return;
    };
    let locked = pending.is_none();
    let mut rows = grid_rows(model, agent, proposal, ui.accessible);
    // The headline under the grid: what the scope is for.
    rows.insert(
        0,
        Line::from(Span::styled(
            cards::truncate_cols(&proposal.summary, (cards::CARD_MAX_W - 4) as usize),
            Style::new().fg(theme::TEXT_PRIMARY),
        )),
    );
    let area = cards::card_area(frame, rows.len() as u16, cards::CARD_MAX_W, ui.accessible);
    let context = if locked {
        vec![Span::styled(
            "locked at plan · e to edit",
            Style::new().fg(theme::TEXT_TERTIARY),
        )]
    } else {
        vec![Span::styled(
            format!("pending approval · {} steps", proposal.steps.len()),
            Style::new().fg(theme::TEXT_TERTIARY),
        )]
    };
    let inner = cards::card_frame(area, "scope", context, "esc close", buf);
    cards::render_body(rows, None, inner, buf);
}
