// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The plan-review dialog: the scope gate as a **modal**, centered over the
//! deck, answered with a single keypress.
//!
//! The gate used to render as a band above the transcript whose answer was
//! *typed into the composer* and submitted with `⏎` — deliberate, at the time,
//! because the composer was an always-live text field competing for every
//! letter (see the history in `deck_ui/gates.rs`). The band bought safety by
//! costing attention: reviewers missed it, and the extra type-then-submit
//! round taught people the card was broken.
//!
//! This dialog resolves the same collision the opposite way: while it is up it
//! **owns the keyboard** (its handler claims every key ahead of the composer),
//! so a bare letter cannot leak into a prompt and a prompt cannot swallow a
//! decision. That modality is what makes one-keypress answers safe here when
//! they were not safe on the band — the exact trade `git add -p` and every
//! permission dialog make.
//!
//! The body is the plan itself: the summary, then **every step, numbered**, in
//! the muted pending tone ([`crate::views::plan_style`] — nothing is committed
//! until the user answers), then the estimates, then the four answers:
//!
//! - `a` — approve the plan as proposed
//! - `t` — approve, trimmed to the scope thresholds
//! - `r` — send it back: type a refinement note (`⌥⏎` for a new line, `⏎`
//!   sends it to the planner)
//! - `x` / `Esc` — abort the plan
//! - `!` — run a shell line without leaving the dialog, and without answering
//!   it: reading back (`!git status`) is part of deciding
//!
//! The last two inputs exist because owning the keyboard also took the
//! composer away, and with it two things the typed-answer flow had — a
//! multi-line note and the `!` escape. Each came back as its own field
//! *inside* the dialog ([`crate::deck_ui::ScopeInput`]) rather than as a hole
//! in the modality, so no key ever leaks to the composer behind it.
//!
//! Once the decision is sent the pending gate clears on the engine's follow-on
//! event; until then the answered latch keeps [`pending`] `None`, so the
//! dialog drops the moment the keypress lands rather than advertising keys
//! that would double-send.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use stella_protocol::ScopeProposal;

use crate::deck::{DeckTab, WorkspaceModel};
use crate::deck_ui::{DeckUi, ScopeInput};
use crate::plan::PlanStepState;
use crate::theme;
use crate::views::cards::truncate_cols;
use crate::views::plan_style::step_visual;

/// Widest the dialog grows — wider than a floating card, because its body is
/// the full text of every plan step.
const DIALOG_MAX_W: u16 = 74;

/// The focused agent's unanswered scope proposal, if any — the single
/// predicate for "the dialog is up". Shared by the renderer, the key handler's
/// caller and the cursor-suppression check so they can never disagree.
///
/// Scoped to the Session tab, like every other tab-owned modal in
/// `deck_render`'s cursor-suppression list (`DeckTab::Issues`'s sub-modes,
/// `DeckTab::Settings`'s config editors). The gate belongs to the focused
/// agent's REPL surface — it was a band above that transcript before it became
/// a dialog, and its reach did not widen when its shape changed. Without this
/// the frame-sized overlay paints over AGENTS, TRACES, GRAPH, FILES and MCP,
/// blanking tabs that answer questions the dialog cannot.
pub(crate) fn pending<'m>(model: &'m WorkspaceModel, ui: &DeckUi) -> Option<&'m ScopeProposal> {
    if ui.tab != DeckTab::Session {
        return None;
    }
    let entry = model.agents.get(ui.focused)?;
    if ui.scope_answered.contains(&entry.meta.id) {
        return None;
    }
    entry.model.pending_scope_review.as_ref()
}

/// How many rows of a long refine note stay on screen. The note is the one
/// part of this dialog the user is authoring, so it gets a real budget — but
/// the plan is what the decision is *about*, so the note never crowds it out.
const NOTE_MAX_ROWS: usize = 6;

/// The rows for an open input: the typed line(s) behind a labelled marker,
/// with the cursor on the last one.
///
/// The refine note is soft-wrapped **and** honours the newlines a modified `⏎`
/// inserted, because a note that renders as one tail-truncated line is
/// indistinguishable from a note that was silently truncated on the way in
/// (#1630). When it outgrows [`NOTE_MAX_ROWS`] the *tail* is what survives:
/// the text under the cursor is the text being typed.
fn input_lines(input: &ScopeInput, inner_w: usize) -> Vec<Line<'static>> {
    let (label, color, body) = match input {
        ScopeInput::Refine(note) => ("refine ▸ ", theme::VIOLET, note.as_str()),
        ScopeInput::Shell(cmd) => ("!", theme::TEXT_SECONDARY, cmd.as_str()),
    };
    let width = inner_w.saturating_sub(label.chars().count() + 1).max(8);

    // Explicit newlines first, then soft-wrap each of those on width.
    let mut rows: Vec<String> = Vec::new();
    for paragraph in body.split('\n') {
        let mut row = String::new();
        for c in paragraph.chars() {
            if row.chars().count() == width {
                rows.push(std::mem::take(&mut row));
            }
            row.push(c);
        }
        rows.push(row);
    }
    // A single-line input keeps its tail visible rather than wrapping into the
    // plan: one command is one row.
    let cap = match input {
        ScopeInput::Refine(_) => NOTE_MAX_ROWS,
        ScopeInput::Shell(_) => 1,
    };
    let folded = rows.len().saturating_sub(cap);
    let rows = rows.split_off(folded);

    let indent = " ".repeat(label.chars().count());
    let last = rows.len().saturating_sub(1);
    rows.into_iter()
        .enumerate()
        .map(|(i, row)| {
            let mut spans = vec![Span::styled(
                if i == 0 {
                    label.to_string()
                } else {
                    indent.clone()
                },
                Style::new().fg(color).add_modifier(Modifier::BOLD),
            )];
            spans.push(Span::styled(row, Style::new().fg(theme::TEXT_PRIMARY)));
            if i == last {
                spans.push(Span::styled("▎", Style::new().fg(color)));
            }
            Line::from(spans)
        })
        .collect()
}

/// Draw the dialog centered over `area`. `input` is the dialog's open text
/// field: `None` renders the one-keypress answers, `Some` renders the line
/// being typed (`r` or `!` was pressed) with its own legend.
pub(crate) fn render(
    proposal: &ScopeProposal,
    input: Option<&ScopeInput>,
    area: Rect,
    buf: &mut Buffer,
) {
    let w = area.width.saturating_sub(4).clamp(20, DIALOG_MAX_W);
    let inner_w = (w as usize).saturating_sub(4);

    // The step list yields to the frame: fixed chrome is summary + estimates +
    // spacers (5 rows) plus the footer inside the borders (2). The footer is
    // one row of answer keys, or however many rows the open input needs — a
    // growing refine note eats into the step list rather than off the frame.
    let footer = input.map_or(1, |i| input_lines(i, inner_w).len());
    let fixed: u16 = 7 + footer as u16;
    let max_steps = area
        .height
        .saturating_sub(fixed + 2)
        .max(1)
        .min(proposal.steps.len() as u16) as usize;
    let folded = proposal.steps.len() - max_steps;

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        truncate_cols(&proposal.summary, inner_w),
        Style::new()
            .fg(theme::TEXT_PRIMARY)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::default());
    // Every step numbered, in the shared pending-step visual: nothing is
    // committed yet, so the whole list reads in the muted tone.
    let v = step_visual(PlanStepState::Planned, 0, false);
    for (i, step) in proposal.steps.iter().take(max_steps).enumerate() {
        lines.push(Line::from(vec![
            Span::styled(format!(" {glyph} ", glyph = v.glyph), v.ring),
            Span::styled(format!("{:>2}. ", i + 1), v.num),
            Span::styled(truncate_cols(step, inner_w.saturating_sub(8)), v.text),
        ]));
    }
    if folded > 0 {
        lines.push(Line::from(Span::styled(
            format!("    … +{folded} more"),
            Style::new().fg(theme::TEXT_TERTIARY),
        )));
    }
    lines.push(Line::default());
    let cost = proposal
        .estimated_cost_usd
        .map(|c| format!("  ·  est. ~${c:.2}"))
        .unwrap_or_default();
    lines.push(Line::from(Span::styled(
        format!(
            "{} steps  ·  ~{} files{cost}",
            proposal.steps.len(),
            proposal.estimated_files
        ),
        Style::new().fg(theme::TEXT_TERTIARY),
    )));
    lines.push(Line::default());
    match input {
        None => {
            let key = |k: &'static str, color| {
                Span::styled(k, Style::new().fg(color).add_modifier(Modifier::BOLD))
            };
            let word = |t: &'static str| Span::styled(t, Style::new().fg(theme::TEXT_SECONDARY));
            lines.push(Line::from(vec![
                key("a", theme::OK),
                word(" approve   "),
                key("t", theme::WARN),
                word(" trim   "),
                key("r", theme::VIOLET),
                word(" refine   "),
                key("x", theme::DANGER),
                word(" abort   "),
                key("!", theme::TEXT_SECONDARY),
                word(" shell"),
            ]));
        }
        Some(input) => lines.extend(input_lines(input, inner_w)),
    }

    let h = ((lines.len() as u16) + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    Clear.render(popup, buf);
    // Warning, not the accent: this dialog is the deck waiting on *you*, and
    // "needs input" is a warning everywhere else in this UI.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::WARNING_BRIGHT))
        .title(Span::styled(
            " plan review ",
            Style::new()
                .fg(theme::WARNING_BRIGHT)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(Span::styled(
                match input {
                    None => " one keypress decides · esc aborts ",
                    // `⌥⏎` rather than the composer footer's terminal-aware
                    // `⌘⏎`: it is the chord that reports correctly without the
                    // kitty protocol, and this dialog does not carry the
                    // `enter_submits` flag that footer switches on.
                    Some(ScopeInput::Refine(_)) => " ⏎ send · ⌥⏎ newline · esc back ",
                    Some(ScopeInput::Shell(_)) => " ⏎ run · esc back ",
                },
                Style::new().fg(theme::TEXT_TERTIARY),
            ))
            .right_aligned(),
        );
    Paragraph::new(lines).block(block).render(popup, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{AgentMeta, Inbound};
    use stella_protocol::AgentEvent;

    fn proposal() -> ScopeProposal {
        ScopeProposal {
            summary: "wire the dialog".into(),
            steps: vec!["read the layout".into(), "fold the rail".into()],
            estimated_files: 3,
            estimated_cost_usd: Some(0.42),
            ..Default::default()
        }
    }

    fn model_with_review() -> WorkspaceModel {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::ScopeReview {
                proposal: proposal(),
            },
        });
        model
    }

    fn flat(area: Rect, draw: impl FnOnce(&mut Buffer)) -> String {
        let mut buf = Buffer::empty(area);
        draw(&mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The single predicate: pending while unanswered, gone the moment the
    /// answered latch is set — the dialog must not keep advertising keys that
    /// would double-send the decision.
    #[test]
    fn pending_honors_the_answered_latch() {
        let model = model_with_review();
        let mut ui = DeckUi::default();
        assert!(pending(&model, &ui).is_some());
        ui.scope_answered.insert("lead".into());
        assert!(pending(&model, &ui).is_none());
    }

    /// The body numbers every step and names the one-keypress answers.
    #[test]
    fn the_dialog_numbers_the_steps_and_offers_the_four_answers() {
        let frame = flat(Rect::new(0, 0, 80, 24), |buf| {
            render(&proposal(), None, Rect::new(0, 0, 80, 24), buf);
        });
        assert!(frame.contains("plan review"), "{frame}");
        assert!(frame.contains("1. read the layout"), "{frame}");
        assert!(frame.contains("2. fold the rail"), "{frame}");
        assert!(frame.contains("approve"), "{frame}");
        assert!(frame.contains("trim"), "{frame}");
        assert!(frame.contains("refine"), "{frame}");
        assert!(frame.contains("abort"), "{frame}");
        assert!(frame.contains("one keypress decides"), "{frame}");
    }

    /// `r` swaps the legend for the refine input, which echoes the note.
    #[test]
    fn the_refine_mode_echoes_the_typed_note() {
        let note = ScopeInput::Refine("only the dialog".into());
        let frame = flat(Rect::new(0, 0, 80, 24), |buf| {
            render(&proposal(), Some(&note), Rect::new(0, 0, 80, 24), buf);
        });
        assert!(frame.contains("refine ▸ only the dialog"), "{frame}");
        assert!(frame.contains("⏎ send"), "{frame}");
        assert!(!frame.contains("one keypress decides"), "{frame}");
    }

    /// A multi-line note renders as multiple rows (#1630). The single-line
    /// input tail-truncated, which is indistinguishable from a note that was
    /// truncated on the way *in* — the reviewer could not tell whether the
    /// planner would receive what they wrote.
    #[test]
    fn a_multi_line_refine_note_renders_every_line() {
        let note = ScopeInput::Refine("drop step 2\nand re-scope 3".into());
        let frame = flat(Rect::new(0, 0, 80, 24), |buf| {
            render(&proposal(), Some(&note), Rect::new(0, 0, 80, 24), buf);
        });
        assert!(frame.contains("refine ▸ drop step 2"), "{frame}");
        assert!(frame.contains("and re-scope 3"), "{frame}");
        assert!(frame.contains("⌥⏎ newline"), "{frame}");
    }

    /// The shell field names itself and offers "run", not "send" — it does not
    /// answer the gate, and the legend must not imply that it does (#1629).
    #[test]
    fn the_shell_field_offers_run_rather_than_send() {
        let cmd = ScopeInput::Shell("git status".into());
        let frame = flat(Rect::new(0, 0, 80, 24), |buf| {
            render(&proposal(), Some(&cmd), Rect::new(0, 0, 80, 24), buf);
        });
        assert!(frame.contains("!git status"), "{frame}");
        assert!(frame.contains("⏎ run"), "{frame}");
        assert!(!frame.contains("⏎ send"), "{frame}");
    }

    /// The options row advertises the shell escape — an escape nobody can see
    /// is an escape nobody uses.
    #[test]
    fn the_options_row_advertises_the_shell_escape() {
        let frame = flat(Rect::new(0, 0, 80, 24), |buf| {
            render(&proposal(), None, Rect::new(0, 0, 80, 24), buf);
        });
        assert!(frame.contains("shell"), "{frame}");
    }

    /// A long plan folds its tail and says so rather than overflowing the
    /// frame — same honesty rule as the rail.
    #[test]
    fn an_overlong_plan_folds_its_tail_inside_a_short_frame() {
        let long = ScopeProposal {
            summary: "many steps".into(),
            steps: (1..=30).map(|i| format!("step {i}")).collect(),
            estimated_files: 30,
            estimated_cost_usd: None,
            ..Default::default()
        };
        let frame = flat(Rect::new(0, 0, 80, 16), |buf| {
            render(&long, None, Rect::new(0, 0, 80, 16), buf);
        });
        assert!(frame.contains("more"), "{frame}");
    }
}
