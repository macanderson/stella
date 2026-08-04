// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The right-hand rail: **PLAN** over **DONE VERIFICATION**, always on.
//!
//! # What this replaces
//!
//! Four surfaces used to describe two things, and between them they managed to
//! show neither:
//!
//! - a one-row `⌾ scope ✓7 · ☑ 2/4 · ⚖ waived` strip, which compressed three
//!   panels so far that the `✓7` (a *step count*, not a progress count) read as
//!   seven passing checks;
//! - a `TASKS` card fed by a board nothing ever populated, so its list was
//!   permanently empty;
//! - a `PROOF` band **gated on bad news** — `band_height` returned 0 unless a
//!   row had gone wrong — so on a healthy turn the verification lights were not
//!   dim, they were absent;
//! - a `⌃S` overlay holding the readable version of all of it, behind a
//!   keystroke, which is where information goes to not be read.
//!
//! # The shape now
//!
//! One column, a quarter of the frame, up for the whole session:
//!
//! ```text
//! ┌ PLAN · working 2/5 ─────────┐
//! │ collapse the rail surfaces  │
//! │ ● 1 read the band layout    │  ← green ring, struck through
//! │ ● 2 fold plan and board     │  ← violet ring, bright text
//! │ ○ 3 pin the approved plan   │  ← default text, hollow ring
//! │ ○ 4 update the goldens      │
//! └──────────── /plan reads it ─┘
//! ┌ DONE VERIFICATION ──────────┐
//! │ ● warrant  required · 41 ln │
//! │ ● witness  authored         │
//! │ ○ oracle   pending          │
//! │ ○ tamper   —                │
//! │ ○ verdict  pending          │
//! └─────────────────────────────┘
//! ```
//!
//! Both panels are **unconditional**. That is the whole point: a rail that
//! appears only when something is wrong teaches the reader that its absence
//! means nothing is happening, when in fact absence and healthy look identical.
//! All five verification rows are always drawn, so a row moving from `pending`
//! to `authored` is a light changing colour in a fixed place — which is what
//! makes it readable out of the corner of an eye.
//!
//! # Copy law (D6)
//!
//! `PLAN`, `DONE VERIFICATION`, plan step. Never task, scope, issue, witness,
//! or proof — those are other tools' words (GitHub's, Jira's) or internal role
//! names, and neither belongs on a user-facing surface.
//!
//! Pure line-builders ([`plan_rows`], [`verification_rows`]) so the state→colour
//! mapping is unit-testable without a terminal.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::model::SessionModel;
use crate::plan::{Plan, PlanState, PlanStepState};
use crate::proof::ProofState;
use crate::render::truncate_spans;
use crate::textline::Tone;
use crate::theme;
use crate::views::proof;

/// Narrowest frame that can afford a rail at all.
///
/// Set low deliberately. The panels are meant to be up *always*, and the
/// alternative on a narrow frame is worse than a cramped transcript: stacked,
/// the same two panels cost thirteen of a 24-row terminal's rows, where as a
/// column they cost none. So the column wins down to the point where the
/// transcript would stop being a transcript.
///
/// Below this, [`crate::views::session`] stacks them instead — the same
/// fallback accessible mode always takes.
pub const MIN_FRAME_COLS: u16 = 72;

/// Narrowest useful rail: `● 12 ` plus a word or two.
const RAIL_MIN_W: u16 = 24;

/// Widest the rail grows. Past this it is stealing columns from the transcript
/// to render whitespace.
const RAIL_MAX_W: u16 = 44;

/// Rows the verification panel always claims: five lights plus its border.
const VERIFY_H: u16 = proof::ROWS + 2;

/// Rows the PLAN panel claims when it is *stacked* above the transcript rather
/// than in the rail (a narrow frame, or accessible mode).
///
/// Fixed, unlike the rail's version, which takes whatever the column has left.
/// Stacked, the panel is spending rows the transcript would otherwise have, so
/// it takes a stated few and folds the rest — three steps plus the border.
/// `/plan` reads the whole thing either way.
pub const STACKED_PLAN_H: u16 = 5;

/// The rail's width for a frame — a quarter, within reason.
pub fn rail_width(frame_w: u16) -> u16 {
    (frame_w / 4).clamp(RAIL_MIN_W, RAIL_MAX_W)
}

/// Rows the transcript keeps whatever else wants them. Stacked panels are the
/// only thing that can take rows from it, and a transcript below this is not a
/// transcript.
const TRANSCRIPT_FLOOR: u16 = 6;

/// `(plan_h, verify_h)` for the stacked fallback, given the rows left after
/// the header, HUD and any raised gates.
///
/// Verification is served first: it is fixed-height and it is the panel a
/// reader cannot reconstruct from anywhere else on the frame. The plan takes
/// what is left, and both drop to zero rather than render a border with one row
/// inside it.
///
/// On a frame too short for either — a 12-row terminal with a gate up — they
/// are simply absent. "Always visible" is a promise about the frames people
/// actually work in, not one a 12-row window can keep, and starving the
/// transcript to keep it would trade the readable surface for the glanceable
/// one.
pub fn stacked_heights(rows_available: u16) -> (u16, u16) {
    let spare = rows_available.saturating_sub(TRANSCRIPT_FLOOR);
    if spare < 3 {
        return (0, 0);
    }
    let verify = VERIFY_H.min(spare);
    let plan = STACKED_PLAN_H.min(spare - verify);
    (if plan < 3 { 0 } else { plan }, verify)
}

/// Whether this frame gets the side-by-side rail.
///
/// Accessible mode always says no: the rail is a *column beside* the
/// transcript, so every terminal row it occupies carries two logical panes at
/// once, and read aloud that is one interleaved line (#1258). Nothing is lost
/// — the caller stacks the same two panels instead.
pub fn rail_visible(accessible: bool, frame_w: u16) -> bool {
    !accessible && frame_w >= MIN_FRAME_COLS
}

// ---------------------------------------------------------------------------
// State → colour
// ---------------------------------------------------------------------------

/// The status ring for one plan step, and how its title is styled.
///
/// The mapping is the product spec, and it is deliberately redundant: state is
/// carried by *ring shape*, *ring colour*, and *text treatment* at once, so it
/// survives a reader who is colour-blind, a terminal with a poor palette, and a
/// glance too quick to read words.
fn step_style(state: PlanStepState, plan: PlanState) -> (&'static str, Style, Style) {
    match state {
        // A step nobody has reached yet. Grey while the plan is still waiting
        // on a decision — nothing is committed — and plain terminal text once
        // it is approved, because then it genuinely is queued work.
        PlanStepState::Planned => {
            let pending_gate = matches!(plan, PlanState::Draft | PlanState::PendingApproval);
            let fg = if pending_gate {
                theme::TEXT_TERTIARY
            } else {
                theme::TEXT_PRIMARY
            };
            ("○", Style::new().fg(fg), Style::new().fg(fg))
        }
        // Working: a filled violet ring, bright text.
        PlanStepState::Started => (
            "●",
            Style::new().fg(theme::VIOLET).add_modifier(Modifier::BOLD),
            Style::new()
                .fg(theme::TEXT_PRIMARY)
                .add_modifier(Modifier::BOLD),
        ),
        // Done: green ring, and the title muted and struck through — the row
        // stays legible as history without competing with the live one.
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
    }
}

/// The plan's own ring, for the panel title.
fn plan_ring(state: PlanState) -> (&'static str, Style) {
    match state {
        PlanState::Draft => ("○", Style::new().fg(theme::TEXT_TERTIARY)),
        // Grey: nothing is committed until the user answers.
        PlanState::PendingApproval => ("○", Style::new().fg(theme::TEXT_TERTIARY)),
        // White: agreed, not yet moving.
        PlanState::Approved => ("●", Style::new().fg(theme::TEXT_PRIMARY)),
        PlanState::Started => (
            "●",
            Style::new().fg(theme::VIOLET).add_modifier(Modifier::BOLD),
        ),
        PlanState::Completed => ("●", Style::new().fg(theme::OK)),
        PlanState::Cancelled => ("○", Style::new().fg(theme::TEXT_TERTIARY)),
        PlanState::Error => (
            "●",
            Style::new().fg(theme::DANGER).add_modifier(Modifier::BOLD),
        ),
    }
}

/// The verification light for a row's tone. Same three-channel redundancy as
/// the plan rings: shape, colour, and the row's own words.
fn verify_ring(tone: Tone) -> (&'static str, Style) {
    match tone {
        Tone::Success => ("●", Style::new().fg(theme::OK)),
        Tone::Warn => ("●", Style::new().fg(theme::WARN)),
        Tone::Error => (
            "●",
            Style::new().fg(theme::DANGER).add_modifier(Modifier::BOLD),
        ),
        // In flight: the same violet the plan uses for "working", so one
        // colour means one thing across the whole rail.
        Tone::Info => ("●", Style::new().fg(theme::VIOLET)),
        // Nothing has been established here yet.
        Tone::Muted => ("○", Style::new().fg(theme::TEXT_TERTIARY)),
    }
}

// ---------------------------------------------------------------------------
// The panels, as pure rows
// ---------------------------------------------------------------------------

/// The PLAN panel's body: the summary, then one row per plan step.
///
/// `cap` is the number of rows available. An over-long plan keeps the open work
/// and folds the finished tail into one dim row rather than dropping steps
/// silently — a rail that shows four of nine steps without saying so is worse
/// than one that shows three and admits it.
pub fn plan_rows(plan: &Plan, cap: usize) -> Vec<Line<'static>> {
    if cap == 0 {
        return Vec::new();
    }
    let steps = plan.steps();
    let mut rows: Vec<Line<'static>> = Vec::new();
    // The summary is a nice-to-have; the steps are the panel. It is included
    // only when doing so still leaves room for two of them — a squeezed panel
    // that spends its one content row on the headline and folds every step
    // into `+3 more` has told the reader nothing they could not read off the
    // gate card above it.
    let summary_fits = !plan.summary.is_empty() && (steps.is_empty() || cap >= 3);
    if summary_fits {
        rows.push(Line::from(Span::styled(
            plan.summary.clone(),
            Style::new().fg(theme::TEXT_SECONDARY),
        )));
    }
    if steps.is_empty() {
        rows.push(Line::from(Span::styled(
            match plan.state {
                PlanState::Cancelled => "plan cancelled",
                _ => "no plan yet — stella plans before it works",
            },
            Style::new().fg(theme::TEXT_TERTIARY),
        )));
        return rows;
    }

    let budget = cap.saturating_sub(rows.len());
    // Which steps to show when they do not all fit: open work first, in plan
    // order, because "what is left" is the question a running plan is asked.
    let (shown, hidden_done, hidden_open) = if steps.len() <= budget {
        (steps.clone(), 0, 0)
    } else {
        // One row for the `… +K more` tail — unless spending it would leave no
        // room for a step at all. A panel showing only `+3 more` has told the
        // reader nothing the title's `0/3` did not already say; one real step
        // is worth more than an accurate count of the ones it is hiding.
        let room = if budget > 1 { budget - 1 } else { budget };
        let open: Vec<_> = steps
            .iter()
            .filter(|s| s.state.is_open())
            .cloned()
            .collect();
        let done = steps.len() - open.len();
        let take = open.len().min(room);
        (open[..take].to_vec(), done, open.len() - take)
    };

    for step in &shown {
        let (glyph, ring, text) = step_style(step.state, plan.state);
        let mut spans = vec![
            Span::styled(format!("{glyph} "), ring),
            Span::styled(
                format!("{} ", step.id),
                Style::new().fg(theme::TEXT_TERTIARY),
            ),
            Span::styled(step.title.clone(), text),
        ];
        if let Some(note) = &step.note {
            spans.push(Span::styled(
                format!(" ({note})"),
                Style::new().fg(theme::TEXT_TERTIARY),
            ));
        } else if let Some(owner) = &step.owner {
            spans.push(Span::styled(
                format!(" ({owner})"),
                Style::new().fg(theme::TEXT_TERTIARY),
            ));
        }
        rows.push(Line::from(spans));
    }
    // The tail row only when a row was actually reserved for it.
    let mut tail: Vec<String> = Vec::new();
    if rows.len() >= cap {
        return rows;
    }
    if hidden_open > 0 {
        tail.push(format!("+{hidden_open} more"));
    }
    if hidden_done > 0 {
        tail.push(format!("+{hidden_done} done"));
    }
    if !tail.is_empty() {
        rows.push(Line::from(Span::styled(
            format!("  {}", tail.join(" · ")),
            Style::new().fg(theme::TEXT_TERTIARY),
        )));
    }
    rows
}

/// The DONE VERIFICATION panel's body: **all five rows, always**, each with its
/// light.
///
/// Unfiltered by design. The old rail hid every row whose tone was `Muted`,
/// which meant a turn that was proving itself cleanly showed fewer rows than
/// one that had gone wrong — so the panel got *shorter* as things went better,
/// and a reader could not tell "nothing to report" from "not reported".
pub fn verification_rows(state: &ProofState) -> Vec<Line<'static>> {
    state
        .rows()
        .into_iter()
        .map(|row| {
            let (glyph, ring) = verify_ring(row.tone);
            Line::from(vec![
                Span::styled(format!("{glyph} "), ring),
                Span::styled(
                    format!("{:<8} ", row.label),
                    Style::new().fg(theme::TEXT_TERTIARY),
                ),
                Span::styled(row.value.clone(), proof::style_for(row.tone)),
            ])
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Draw the rail into `area`: PLAN on top, DONE VERIFICATION pinned to the
/// bottom.
///
/// Verification takes its fixed seven rows first and the plan takes the rest,
/// so the lights never move as the plan grows — a status light that changes
/// position is a status light nobody learns to find.
pub fn render(sm: &SessionModel, area: Rect, buf: &mut Buffer) {
    if area.height < 3 || area.width < 4 {
        return;
    }
    let verify_h = VERIFY_H.min(area.height);
    let bands = Layout::vertical([Constraint::Min(0), Constraint::Length(verify_h)]).split(area);
    render_plan(&sm.plan, bands[0], buf);
    render_verification(&sm.proof, bands[1], buf);
}

/// The PLAN panel. Public so the narrow/accessible path can stack it.
pub fn render_plan(plan: &Plan, area: Rect, buf: &mut Buffer) {
    if area.height < 3 {
        return;
    }
    let inner_w = area.width.saturating_sub(2) as usize;
    let (done, total) = plan.progress();
    let (glyph, ring) = plan_ring(plan.state);
    let title = Line::from(vec![
        Span::styled(" PLAN ", Style::new().fg(theme::TEXT_SECONDARY)),
        Span::styled(format!("{glyph} "), ring),
        Span::styled(
            if total > 0 {
                format!("{} {done}/{total} ", plan.state.label())
            } else {
                format!("{} ", plan.state.label())
            },
            Style::new().fg(theme::TEXT_TERTIARY),
        ),
    ]);
    let rows: Vec<Line<'static>> = plan_rows(plan, (area.height as usize).saturating_sub(2))
        .into_iter()
        .map(|line| Line::from(truncate_spans(line.spans, inner_w)))
        .collect();
    Paragraph::new(rows)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::rule())
                .title(title)
                .title_bottom(
                    Line::from(Span::styled(
                        " /plan reads it ",
                        Style::new().fg(theme::TEXT_TERTIARY),
                    ))
                    .right_aligned(),
                ),
        )
        .render(area, buf);
}

/// The DONE VERIFICATION panel. Public for the same reason.
pub fn render_verification(state: &ProofState, area: Rect, buf: &mut Buffer) {
    if area.height < 3 {
        return;
    }
    let inner_w = area.width.saturating_sub(2) as usize;
    let rows: Vec<Line<'static>> = verification_rows(state)
        .into_iter()
        .take((area.height as usize).saturating_sub(2))
        .map(|line| Line::from(truncate_spans(line.spans, inner_w)))
        .collect();
    Paragraph::new(rows)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style(state))
                .title(Span::styled(
                    " DONE VERIFICATION ",
                    Style::new().fg(theme::TEXT_SECONDARY),
                )),
        )
        .render(area, buf);
}

/// The verification border tracks its worst light, so an unproven turn is
/// legible from the frame before a word is read.
fn border_style(state: &ProofState) -> Style {
    let tones: Vec<Tone> = state.rows().into_iter().map(|r| r.tone).collect();
    if tones.contains(&Tone::Error) {
        Style::new().fg(theme::DANGER)
    } else if tones.contains(&Tone::Warn) {
        Style::new().fg(theme::WARN)
    } else if state.flip.achieved() {
        Style::new().fg(theme::SUCCESS)
    } else {
        theme::rule()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::{ProofStep, ScopeProposal, TaskItem, TaskStatus};

    fn proposal(steps: &[&str]) -> ScopeProposal {
        ScopeProposal {
            summary: "collapse the rail surfaces".into(),
            steps: steps.iter().map(|s| (*s).to_string()).collect(),
            estimated_files: 9,
            estimated_cost_usd: Some(1.40),
            ..Default::default()
        }
    }

    fn item(id: &str, subject: &str, status: TaskStatus) -> TaskItem {
        TaskItem {
            id: id.into(),
            subject: subject.into(),
            description: None,
            status,
            owner: None,
        }
    }

    fn text_of(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn rendered(area: Rect, draw: impl FnOnce(&mut Buffer)) -> String {
        let mut buf = Buffer::empty(area);
        draw(&mut buf);
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    // ---- width ----

    #[test]
    fn the_rail_is_a_quarter_of_the_frame_within_reason() {
        assert_eq!(rail_width(160), 40);
        assert_eq!(rail_width(120), 30);
        assert_eq!(rail_width(80), RAIL_MIN_W, "clamped up off a narrow frame");
        assert_eq!(rail_width(400), RAIL_MAX_W, "clamped down off a huge one");
    }

    #[test]
    fn accessible_mode_never_takes_the_side_by_side_rail() {
        assert!(!rail_visible(true, 200));
        assert!(rail_visible(false, MIN_FRAME_COLS));
        assert!(!rail_visible(false, MIN_FRAME_COLS - 1));
    }

    // ---- the plan panel ----

    /// **The regression this whole change exists for.** A plan the user has
    /// approved must show its steps, with no board, on the very first frame.
    /// The old TASKS panel rendered an empty list here.
    #[test]
    fn an_approved_plan_lists_its_steps_before_any_board_arrives() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["read the band layout", "fold the rail"]));
        plan.approve();
        let rows = plan_rows(&plan, 12);
        let text: Vec<String> = rows.iter().map(text_of).collect();
        assert!(
            text.iter().any(|r| r.contains("read the band layout")),
            "{text:?}"
        );
        assert!(text.iter().any(|r| r.contains("fold the rail")), "{text:?}");
    }

    /// The colour contract, asserted on the spans rather than a screenshot:
    /// done is a green ring with struck-through text, working is a violet ring
    /// with bright text, queued is plain.
    #[test]
    fn each_step_state_carries_its_own_ring_and_text_treatment() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one", "two", "three"]));
        plan.approve();
        plan.apply_board(&[
            item("1", "one", TaskStatus::Completed),
            item("2", "two", TaskStatus::InProgress),
        ]);
        let rows = plan_rows(&plan, 12);
        // rows[0] is the summary line.
        let done = &rows[1];
        assert_eq!(done.spans[0].content.as_ref(), "● ");
        assert_eq!(done.spans[0].style.fg, Some(theme::OK));
        assert!(
            done.spans[2]
                .style
                .add_modifier
                .contains(Modifier::CROSSED_OUT),
            "a finished step is struck through"
        );

        let working = &rows[2];
        assert_eq!(working.spans[0].content.as_ref(), "● ");
        assert_eq!(working.spans[0].style.fg, Some(theme::VIOLET));
        assert_eq!(working.spans[2].style.fg, Some(theme::TEXT_PRIMARY));

        let queued = &rows[3];
        assert_eq!(queued.spans[0].content.as_ref(), "○ ");
        assert_eq!(queued.spans[0].style.fg, Some(theme::TEXT_PRIMARY));
    }

    /// Before the user answers the gate, nothing is committed — every ring is
    /// grey, including the plan's own.
    #[test]
    fn a_plan_awaiting_approval_is_grey_throughout() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one", "two"]));
        assert_eq!(plan.state, PlanState::PendingApproval);
        assert_eq!(plan_ring(plan.state).1.fg, Some(theme::TEXT_TERTIARY));
        let rows = plan_rows(&plan, 12);
        assert_eq!(rows[1].spans[0].style.fg, Some(theme::TEXT_TERTIARY));
    }

    #[test]
    fn a_failed_step_rings_red_and_names_why() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one"]));
        plan.approve();
        plan.apply_board(&[item("1", "one", TaskStatus::Cancelled)]);
        let rows = plan_rows(&plan, 12);
        assert_eq!(rows[1].spans[0].style.fg, Some(theme::DANGER));
        assert!(
            text_of(&rows[1]).contains("(cancelled)"),
            "{:?}",
            text_of(&rows[1])
        );
    }

    /// A plan too tall for the rail keeps the open work and *says* what it
    /// folded — silently showing a prefix would misreport how much is left.
    #[test]
    fn an_overlong_plan_folds_its_tail_and_admits_it() {
        let mut plan = Plan::default();
        let steps: Vec<String> = (1..=20).map(|i| format!("step {i}")).collect();
        let refs: Vec<&str> = steps.iter().map(String::as_str).collect();
        plan.propose(&proposal(&refs));
        plan.approve();
        let rows = plan_rows(&plan, 6);
        assert!(rows.len() <= 6, "{} rows for a cap of 6", rows.len());
        assert!(
            text_of(rows.last().unwrap()).contains("more"),
            "{:?}",
            text_of(rows.last().unwrap())
        );
    }

    #[test]
    fn an_empty_plan_says_so_rather_than_rendering_nothing() {
        let text: Vec<String> = plan_rows(&Plan::default(), 8).iter().map(text_of).collect();
        assert!(text.iter().any(|r| r.contains("no plan yet")), "{text:?}");
    }

    // ---- the verification panel ----

    /// **The other half of the report.** All five lights, on a turn where
    /// nothing has happened yet — the old band rendered zero rows here, which
    /// is why the indicators "never updated": there was nothing on screen to
    /// update.
    #[test]
    fn all_five_lights_are_up_before_anything_has_been_established() {
        let rows = verification_rows(&ProofState::default());
        assert_eq!(rows.len(), 5);
        for label in ["warrant", "witness", "oracle", "tamper", "verdict"] {
            assert!(
                rows.iter().any(|r| text_of(r).contains(label)),
                "missing {label}"
            );
        }
        assert!(
            rows.iter().all(|r| r.spans[0].content.as_ref() == "○ "),
            "an unestablished light is a hollow grey ring"
        );
    }

    /// And they change colour in place as the pipeline reports.
    #[test]
    fn a_light_changes_colour_where_it_stands() {
        let mut state = ProofState::default();
        let before = verification_rows(&state);
        assert_eq!(before[1].spans[0].style.fg, Some(theme::TEXT_TERTIARY));

        state.apply(&ProofStep::Warrant {
            required: true,
            reason: None,
            diff_lines: 41,
        });
        state.apply(&ProofStep::WitnessAuthored {
            path: "tests/clear_reset.rs".into(),
            command: "cargo test clear_reset".into(),
            fingerprint: "sha256:9f3c1d2e".into(),
        });
        let after = verification_rows(&state);
        assert_eq!(after.len(), before.len(), "the panel never changes height");
        assert_eq!(
            after[1].spans[0].style.fg,
            Some(theme::OK),
            "the witness light went green"
        );
        assert!(text_of(&after[1]).contains("authored"));
    }

    #[test]
    fn a_clean_turn_still_shows_every_light() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Assurance {
            witness: false,
            judge: false,
        });
        state.finish();
        assert_eq!(
            verification_rows(&state).len(),
            5,
            "a waived turn is a report, not an absence"
        );
    }

    // ---- rendering ----

    #[test]
    fn the_rail_draws_both_panels_with_the_plan_on_top() {
        let mut sm = SessionModel::default();
        sm.plan
            .propose(&proposal(&["read the layout", "fold the rail"]));
        sm.plan.approve();
        let area = Rect::new(0, 0, 34, 20);
        let text = rendered(area, |b| render(&sm, area, b));
        let plan_at = text.find("PLAN").expect("a PLAN panel");
        let verify_at = text
            .find("DONE VERIFICATION")
            .expect("a verification panel");
        assert!(plan_at < verify_at, "PLAN sits above DONE VERIFICATION");
        assert!(text.contains("read the layout"), "{text}");
        assert!(text.contains("warrant"), "{text}");
    }

    /// The rail is narrow and `Paragraph` clips at its border with no ellipsis,
    /// so a long step silently became a *different* step.
    #[test]
    fn long_rows_are_elided_rather_than_clipped() {
        let mut sm = SessionModel::default();
        sm.plan.propose(&proposal(&[
            "collapse the plan and verification surfaces into one always-on rail",
        ]));
        sm.plan.approve();
        let area = Rect::new(0, 0, 30, 20);
        let text = rendered(area, |b| render(&sm, area, b));
        assert!(text.contains('…'), "no ellipsis marks the cut:\n{text}");
    }

    #[test]
    fn a_rail_too_short_for_both_panels_is_a_no_op_not_a_panic() {
        let sm = SessionModel::default();
        for h in 0..8u16 {
            let area = Rect::new(0, 0, 30, h);
            rendered(area, |b| render(&sm, area, b));
        }
    }

    #[test]
    fn a_rail_too_narrow_to_draw_is_a_no_op_not_a_panic() {
        let sm = SessionModel::default();
        for w in 0..6u16 {
            let area = Rect::new(0, 0, w, 20);
            rendered(area, |b| render(&sm, area, b));
        }
    }

    /// The verification panel is pinned to the bottom at a fixed height, so the
    /// lights do not move as the plan above them grows.
    #[test]
    fn the_lights_hold_their_position_as_the_plan_grows() {
        let area = Rect::new(0, 0, 34, 24);
        let row_of = |steps: &[&str]| -> usize {
            let mut sm = SessionModel::default();
            sm.plan.propose(&proposal(steps));
            sm.plan.approve();
            let text = rendered(area, |b| render(&sm, area, b));
            text.lines()
                .position(|l| l.contains("warrant"))
                .expect("a warrant row")
        };
        assert_eq!(row_of(&["one"]), row_of(&["one", "two", "three", "four"]));
    }
}
