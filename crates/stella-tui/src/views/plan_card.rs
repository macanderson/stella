// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The plan card (`⌃S`, `/plan`): the whole plan, **readable**.
//!
//! # What this is for
//!
//! The tab row's breadcrumb ([`crate::views::frame`]) spends one row on the plan:
//! the running step's title and the fraction, and nothing else. That is the
//! right density for something on screen the whole time, and the wrong one for
//! the moment a user actually asks *what is step 3*.
//!
//! This card is that moment, and it is what the breadcrumb expands into
//! (SPEC 7.3). Every step at full width with its elaboration underneath, then
//! the plan's operating envelope — where it may write, what it may spend,
//! which models are routed where, and what will count as done.
//!
//! It supersedes three cards that each showed a slice of this and none of which
//! showed a step's text: `/scope` (the envelope, no steps), `/tasks` (a board
//! nothing populated), and `/witness` (the verification records).
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

pub mod economics;
pub(crate) mod step_style;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use stella_tui_theme::token;

use stella_protocol::ScopeProposal;

use crate::deck::{AgentEntry, PipelineRole, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::plan::{Plan, PlanState};
use crate::views::cards;

/// Width of the dimmed left label column.
const LABEL_W: usize = 10;

/// One labeled grid row: dim fixed-width label, then the value spans.
fn grid_row(label: &str, value: Vec<Span<'static>>, accessible: bool) -> Line<'static> {
    let dim = Style::new().fg(token::MUTED);
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
    let dim = Style::new().fg(token::MUTED);
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
            Style::new().fg(token::GOLD),
        ),
        Span::styled(" · ", dim),
        Span::styled(format!("verify {}", slot(PipelineRole::Verifier)), dim),
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
///
/// [`step_row`] draws one row; this walks the plan and adds the detail rows
/// under it.
pub fn step_rows(
    plan: &Plan,
    width: usize,
    selected: Option<usize>,
    now_ms: u64,
    animate: bool,
    running: Option<&economics::RunningTask>,
) -> (Vec<Line<'static>>, Option<usize>) {
    let dim = Style::new().fg(token::MUTED);
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
    let num_w = ordinal_width(&steps);
    for (i, step) in steps.iter().enumerate() {
        if Some(i) == selected {
            selected_row = Some(rows.len());
        }
        let spend = plan
            .ledger
            .get(&step.id)
            .and_then(|entry| entry.spend.as_ref());
        rows.push(priced_step_row(
            step,
            Some(i) == selected,
            now_ms,
            animate,
            spend,
            width,
            num_w,
        ));
        // The elaboration, wrapped under the title at the title's indent.
        if let Some(detail) = &step.detail {
            let indent = 7usize;
            for chunk in cards::wrap(detail, width.saturating_sub(indent).max(8)) {
                rows.push(Line::from(vec![
                    Span::raw(" ".repeat(indent)),
                    Span::styled(chunk, dim),
                ]));
            }
        }
        // SPEC 7.3's running-task card, under the step it belongs to rather
        // than at the top of the list: the reader's eye is already on the row
        // that is moving, and a card pinned elsewhere would make them find it
        // twice.
        rows.extend(economics::running_card(step, running, plan, width));
    }
    (rows, selected_row)
}

/// The cells `<n>.` occupies, so the ordinal column can be sized from the
/// widest id in a plan rather than from a constant.
///
/// Display width rather than `len()`, on the rule the rest of this renderer
/// follows (`cards::pad_right`): an id is ASCII today, and a column measured
/// in bytes is wrong the first time one is not.
fn display_w(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// The ordinal column's width for a whole plan: the widest `<id>.` in it, plus
/// the one cell that separates it from the title.
///
/// Computed over the plan rather than fixed at three cells (#5271). `{:<3}`
/// spends its whole budget on `"10."` or a revision's `"3b."`, so the pad adds
/// nothing and the title butts against the dot — and every plan hits that once
/// it reaches ten steps or a revision renumbers one. Sizing from the widest id
/// also lines the titles up, which three cells could only do by accident.
fn ordinal_width(steps: &[crate::plan::PlanStep]) -> usize {
    steps
        .iter()
        .map(|s| display_w(&format!("{}.", s.id)) + 1)
        .max()
        .unwrap_or(3)
}

/// One step's own row: selection marker, state glyph, ordinal, title, and the
/// trailing `(note)` / `(owner)` tag.
///
/// `num_w` is the ordinal column's width, from [`ordinal_width`] over the whole
/// plan so the titles line up. It is a layout parameter, not plan state — the
/// row stays goldenable one state at a time, which is what the note below is
/// about. The separator survives whatever is passed: the field is never
/// narrower than this id plus one cell.
///
/// Split out of [`step_rows`] so a state can be goldened as the row it draws
/// rather than as the styles behind it: five of the six come off a board
/// status and the sixth, [`crate::plan::PlanStepState::DriftInserted`], is
/// derived from the plan's two lanes disagreeing — so no single fixture puts
/// the whole vocabulary on screen at once.
pub fn step_row(
    step: &crate::plan::PlanStep,
    selected: bool,
    now_ms: u64,
    animate: bool,
    num_w: usize,
) -> Line<'static> {
    let v = step_style::step_visual(step.state, now_ms, animate);
    let ordinal = format!("{}.", step.id);
    let mut spans = vec![
        cards::marker(selected),
        Span::styled(v.glyph.to_string(), v.ring),
        Span::styled(" ", v.gap),
        Span::styled(
            format!(
                "{ordinal:<width$}",
                width = num_w.max(display_w(&ordinal) + 1)
            ),
            v.num,
        ),
        Span::styled(step.title.clone(), v.text),
    ];
    if let Some(note) = &step.note {
        spans.push(Span::styled(format!("  ({note})"), v.meta));
    } else if let Some(owner) = &step.owner {
        spans.push(Span::styled(format!("  ({owner})"), v.meta));
    }
    Line::from(spans)
}

/// [`step_row`] with SPEC 7.3's economics column right-aligned onto it.
///
/// A wrapper rather than two more parameters on `step_row`, because that
/// function's job is to be goldenable one *state* at a time (see its doc), and
/// a trailing measurement is the same on every state — folding it in would
/// make every state's golden carry a column that is not about the state.
///
/// Right-aligned against the card's interior rather than the frame, through
/// `cards::pad_right`, which measures display width: a title carrying a wide
/// glyph still lines the column up. Muted, because a token count is a
/// measurement beside a title and money is the only number that takes the
/// accent (SPEC 5).
fn priced_step_row(
    step: &crate::plan::PlanStep,
    selected: bool,
    now_ms: u64,
    animate: bool,
    spend: Option<&crate::plan::StepSpend>,
    inner_w: usize,
    num_w: usize,
) -> Line<'static> {
    let row = step_row(step, selected, now_ms, animate, num_w);
    Line::from(cards::pad_right(
        row.spans,
        Span::styled(economics::token_cell(spend), Style::new().fg(token::MUTED)),
        inner_w,
    ))
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
    let dim = Style::new().fg(token::MUTED);
    let primary = Style::new().fg(token::TEXT);
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

    // Budget: spent in gold — money renders gold and its meter is gold on
    // the border gray (SPEC 5); spend is a fact, not a pass, so it takes no
    // verdict ink — `of $cap` dim, plus a mini fraction bar. The cap is the
    // agent's own metered limit.
    let mut budget: Vec<Span<'static>> = vec![Span::styled(
        format!("${:.2}", agent.cost_usd),
        Style::new().fg(token::GOLD),
    )];
    if let Some(cap) = agent.model.hud.limit_usd.filter(|cap| *cap > 0.0) {
        budget.push(Span::styled(format!(" of ${cap:.2} "), dim));
        if !accessible {
            let pct = ((agent.cost_usd / cap).clamp(0.0, 1.0) * 100.0).round() as usize;
            budget.extend(cards::mini_fraction_bar(pct, 100, 7, token::GOLD));
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
    rows
}

/// What the in-flight task has cost and how long it has been going, as the
/// fold knows it — the subtraction [`economics::RunningTask`] describes.
///
/// `AgentEntry::active_task` is the only source with a real number in it
/// today: the board says which task is in progress, and the fold stamps the
/// session's spend and the deck clock at the moment it flipped. That makes
/// *this* task's cost the difference, which is a fact about one task rather
/// than the session total wearing a task's name.
///
/// `None` when no task is active, which is every idle session and every turn
/// whose board has nothing in progress.
fn running_task(agent: &AgentEntry, now_ms: u64) -> Option<economics::RunningTask> {
    let stamp = agent.active_task.as_ref()?;
    Some(economics::RunningTask {
        id: stamp.id.clone(),
        elapsed_ms: now_ms.saturating_sub(stamp.started_ms),
        cost_usd: (agent.cost_usd - stamp.cost_at_start_usd).max(0.0),
    })
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
            Style::new().fg(token::TEXT).add_modifier(Modifier::BOLD),
        )));
        rows.push(Line::default());
    }
    let step_count = plan.steps().len();
    let selected = (step_count > 0).then(|| ui.cards.plan_sel.min(step_count - 1));
    let offset = rows.len();
    let running = running_task(agent, model.now_ms);
    let (step_lines, selected_row) = step_rows(
        plan,
        inner_w,
        selected,
        model.now_ms,
        !ui.no_anim,
        running.as_ref(),
    );
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

    // SPEC 7.3's footer, last: it is about the plan as a whole, so it closes
    // the card rather than sitting between the steps and their envelope.
    if !plan.is_empty() {
        rows.push(Line::default());
        rows.extend(economics::footer_rows(plan));
    }

    let (done, total) = plan.progress();
    // The revision breadcrumb (#4333): silent on the first revision and on a
    // plan whose producer does not number them, so the marker appears exactly
    // when a plan has been re-proposed and a reader needs to know they are
    // looking at a different one.
    let revision = match plan.revision {
        Some(r) if r > 1 => format!(" · r{r}"),
        _ => String::new(),
    };
    let context = vec![Span::styled(
        match plan.state {
            PlanState::PendingApproval => {
                format!("{} · {total} steps{revision}", plan.state.label())
            }
            PlanState::Draft => plan.state.label().to_string(),
            _ => format!("{} {done}/{total}{revision}", plan.state.label()),
        },
        Style::new().fg(token::MUTED),
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
    use crate::plan::{Plan, PlanStep, PlanStepState};
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
            contract: None,
        }]);
        let text: String = step_rows(&plan, 52, None, 0, false, None)
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
            contract: None,
        }]);
        let (rows, _) = step_rows(&plan, 40, None, 0, false, None);
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

    /// **The golden row per state (SPEC 7.2).** Six states, six rows, spelled
    /// out as the text a reader sees rather than as the styles behind it.
    ///
    /// A `contains` assertion would not have caught what this is for: `✗` used
    /// to be the whole story for a step that ended badly, and `●` stood where
    /// SPEC 4 asks for `◐`. Both read fine one row at a time and wrong side by
    /// side.
    #[test]
    fn every_state_draws_its_specced_row() {
        let step = |id: &str, title: &str, state: PlanStepState, note: Option<&str>| PlanStep {
            id: id.into(),
            title: title.into(),
            detail: None,
            state,
            owner: None,
            note: note.map(str::to_string),
            contract: None,
        };
        let rows = [
            (
                step("1", "extract types", PlanStepState::Complete, None),
                "  ✓ 1. extract types",
            ),
            (
                step("2", "move hooks", PlanStepState::Started, None),
                "  ◐ 2. move hooks",
            ),
            (
                step("3", "update imports", PlanStepState::Planned, None),
                "  ○ 3. update imports",
            ),
            (
                step("4", "run the gates", PlanStepState::Verify, None),
                "  ◇ 4. run the gates",
            ),
            (
                step("5", "ship it", PlanStepState::Blocked, None),
                "  ✗ 5. ship it",
            ),
            (
                step(
                    "6",
                    "fix borrow err",
                    PlanStepState::DriftInserted,
                    Some("inserted"),
                ),
                "  ⌥ 6. fix borrow err  (inserted)",
            ),
        ];
        for (step, expected) in rows {
            assert_eq!(text_of(&step_row(&step, false, 0, false, 3)), expected);
        }
        // And the one row the glyph cannot disambiguate says it in words.
        assert_eq!(
            text_of(&step_row(
                &step("7", "drop it", PlanStepState::Blocked, Some("cancelled")),
                false,
                0,
                false,
                3
            )),
            "  ✗ 7. drop it  (cancelled)"
        );

        // **The witness (#5271).** A two-character id keeps its separator.
        //
        // `{:<3}` spent its whole budget on `"10."`, so the pad added nothing
        // and the title butted against the dot — `10.update imports`. Every
        // plan reaches this at ten steps, and any plan does the moment a
        // revision renumbers a step `3b`. The width passed here is the one
        // `ordinal_width` computes for such a plan.
        assert_eq!(
            text_of(&step_row(
                &step("10", "update imports", PlanStepState::Planned, None),
                false,
                0,
                false,
                4
            )),
            "  ○ 10. update imports"
        );
        assert_eq!(
            text_of(&step_row(
                &step("3b", "fix the borrow", PlanStepState::Planned, None),
                false,
                0,
                false,
                4
            )),
            "  ○ 3b. fix the borrow"
        );
        // And the separator does not depend on the caller passing the right
        // width: too narrow a column still leaves one cell.
        assert_eq!(
            text_of(&step_row(
                &step("10", "update imports", PlanStepState::Planned, None),
                false,
                0,
                false,
                1
            )),
            "  ○ 10. update imports"
        );
    }

    /// The ordinal column is sized from the widest id, so the titles line up.
    ///
    /// The other half of #5271: a separator alone would leave a ten-step plan
    /// ragged, with `9.` one cell narrower than `10.` and every title after it
    /// stepping left.
    #[test]
    fn a_plan_past_nine_steps_lines_its_titles_up() {
        let mut plan = Plan::default();
        let titles: Vec<String> = (1..=11).map(|n| format!("step {n}")).collect();
        plan.propose(&proposal(
            &titles.iter().map(String::as_str).collect::<Vec<_>>(),
        ));
        plan.approve();

        let rows = step_rows(&plan, 60, None, 0, false, None).0;
        let starts: Vec<usize> = rows
            .iter()
            .map(text_of)
            .filter(|t| t.contains("step "))
            .map(|t| t.find("step ").expect("a title"))
            .collect();
        assert_eq!(starts.len(), 11, "one row per step");
        assert!(
            starts.windows(2).all(|w| w[0] == w[1]),
            "titles do not share a column: {starts:?}"
        );
    }

    #[test]
    fn an_unproposed_plan_says_so_rather_than_showing_an_empty_list() {
        let text = text_of(&step_rows(&Plan::default(), 52, None, 0, false, None).0[0]);
        assert!(text.contains("no plan has been proposed"), "{text}");
    }

    /// D6: this surface's words are plan words. The other tools' vocabulary and
    /// the internal role names never reach it.
    #[test]
    fn the_card_never_speaks_another_tools_vocabulary() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one"]));
        plan.approve();
        let text: String = step_rows(&plan, 52, None, 0, false, None)
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
