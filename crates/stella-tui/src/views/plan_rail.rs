// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The right-hand rail: **PLAN** over **PROOF**, always on.
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
//! ┌ PROOF ✓ proved ─────────────┐
//! │ a test fails without this   │
//! │ change and passes with it   │
//! │ tests/clear_reset.rs        │
//! └─────────────────────────────┘
//! ```
//!
//! Both panels are **unconditional**. That is the whole point: a rail that
//! appears only when something is wrong teaches the reader that its absence
//! means nothing is happening, when in fact absence and healthy look identical.
//! PROOF is always drawn at the same height, so a change of standing is a light
//! changing colour in a fixed place — which is what makes it readable out of the
//! corner of an eye.
//!
//! # Why the two panels are shaped alike
//!
//! Each carries its state **in its own title** — `PLAN ● working 2/5`,
//! `PROOF ✓ proved` — and spends its body on the detail behind it. One idiom,
//! learned once, read twice. PROOF used to be a title that said only what the
//! panel *was* (`DONE VERIFICATION`) over five rows of pipeline stage names, so
//! the answer was somewhere in the body if it was anywhere; the state-in-title
//! shape puts it where the eye already goes for PLAN.
//!
//! # Copy law (D6)
//!
//! `PLAN`, `PROOF`, plan step. Never task, scope or issue — those are other
//! tools' words (GitHub's, Jira's) — and never `warrant`, `witness`, `oracle`,
//! `tamper` or `verdict`, which name `stella-pipeline` stages. `PROOF` is not
//! among them: it is the plain-English claim the panel makes, and the earlier
//! ban on it here is what produced a title nobody could read.
//!
//! Pure line-builders ([`plan_rows`], [`proof_body`]) so the state→colour
//! mapping is unit-testable without a terminal.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::model::SessionModel;
use crate::plan::{Plan, PlanState};
use crate::proof::{ProofState, Standing};
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

/// Rows the PROOF panel claims when it can show its whole explanation.
const VERIFY_H: u16 = proof::BODY_ROWS + 2;

/// Rows PLAN keeps before verification may take any: its border plus two
/// steps. Below this the panel is a bordered box with nothing readable in it.
const PLAN_MIN_H: u16 = 4;

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

// The per-step visual (ring, number, title, row treatment) lives in
// [`crate::views::plan_style`], shared with the `/plan` card and the
// plan-review dialog so the three surfaces cannot drift apart.
use crate::views::plan_style::step_visual;

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

/// The PROOF light. Genuine three-channel redundancy — **shape**, colour, and
/// the standing's own word — so the panel survives a colour-blind reader and a
/// monochrome terminal alike.
///
/// The shape channel used to carry only two values (`●` established, `○` not),
/// which meant colour alone separated a proof from a failure. It now borrows
/// the plan's own settled glyphs, so `✓` and `✗` mean the same thing in both
/// panels of one rail.
fn verify_ring(tone: Tone) -> (&'static str, Style) {
    match tone {
        Tone::Success => ("✓", Style::new().fg(theme::OK)),
        Tone::Warn => ("⚠", Style::new().fg(theme::WARN)),
        Tone::Error => (
            "✗",
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
pub fn plan_rows(plan: &Plan, cap: usize, now_ms: u64, animate: bool) -> Vec<Line<'static>> {
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
        let v = step_visual(step.state, now_ms, animate);
        let mut spans = vec![
            Span::styled(v.glyph, v.ring),
            Span::styled(" ", v.gap),
            Span::styled(format!("{}. ", step.id), v.num),
            Span::styled(step.title.clone(), v.text),
        ];
        if let Some(note) = &step.note {
            spans.push(Span::styled(format!(" ({note})"), v.meta));
        } else if let Some(owner) = &step.owner {
            spans.push(Span::styled(format!(" ({owner})"), v.meta));
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

/// The PROOF panel's body: the plain sentences behind the standing its title
/// carries, wrapped to `width`.
///
/// No light and no label per row — the panel makes **one** claim, so it gets one
/// light, in the title, where the eye already goes for PLAN's. Five lights over
/// five pipeline stage names is what this replaced, and the reason is in
/// [`crate::proof`]'s module docs.
///
/// A body that is quiet news reads in the ordinary secondary text; one that is
/// bad news is tinted, so a reader who has looked away from the title still sees
/// the panel as a problem.
pub fn proof_body(state: &ProofState, width: usize) -> Vec<Line<'static>> {
    let tone = state.standing().tone();
    let style = match tone {
        Tone::Warn | Tone::Error => proof::style_for(tone),
        _ => Style::new().fg(theme::TEXT_SECONDARY),
    };
    state
        .explain()
        .iter()
        .flat_map(|sentence| proof::wrap(sentence, width))
        .map(|line| Line::from(Span::styled(line, style)))
        .collect()
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Draw the rail into `area`: PLAN on top, PROOF pinned to the bottom.
///
/// PROOF takes its fixed rows first and the plan takes the rest, so the light
/// never moves as the plan grows — a status light that changes position is a
/// status light nobody learns to find.
pub fn render(sm: &SessionModel, now_ms: u64, animate: bool, area: Rect, buf: &mut Buffer) {
    if area.height < 3 || area.width < 4 {
        return;
    }
    let verify_h = verification_height(area.height);
    let bands = Layout::vertical([Constraint::Min(0), Constraint::Length(verify_h)]).split(area);
    render_plan(&sm.plan, now_ms, animate, bands[0], buf);
    render_verification(&sm.proof, bands[1], buf);
}

/// Rows to give the PROOF panel out of a rail `height` rows tall.
///
/// PROOF is a fixed-height panel by design — a light that moves is a light
/// nobody learns to find — but taking those rows unconditionally meant that on
/// a frame with a gate card up, it claimed everything and **PLAN disappeared
/// entirely**. Both panels are supposed to be up always, so the fixed height
/// yields before the plan does:
///
/// | Rail height | PROOF |
/// |---|---|
/// | ≥ `PLAN_MIN_H + VERIFY_H` | the standing and its whole explanation |
/// | ≥ `PLAN_MIN_H + 3` | the standing and the first line of it |
/// | below that | nothing — the plan takes the rail |
///
/// The middle rung costs less than it looks: the standing rides in the title,
/// so a squeezed panel is still a complete answer with a shorter reason. Before
/// the standing moved into the title this rung had to swap the whole body for a
/// hand-written summary line, because any prefix of five stage rows could drop
/// the verdict and still look finished.
fn verification_height(height: u16) -> u16 {
    if height >= PLAN_MIN_H + VERIFY_H {
        VERIFY_H
    } else if height >= PLAN_MIN_H + 3 {
        3
    } else {
        0
    }
}

/// The PLAN panel. Public so the narrow/accessible path can stack it.
pub fn render_plan(plan: &Plan, now_ms: u64, animate: bool, area: Rect, buf: &mut Buffer) {
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
    let rows: Vec<Line<'static>> = plan_rows(
        plan,
        (area.height as usize).saturating_sub(2),
        now_ms,
        animate,
    )
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

/// The PROOF panel. Public for the same reason.
///
/// The title carries the whole answer, so a squeezed panel simply explains
/// itself in fewer words — there is no rung at which it stops saying where the
/// proof stands. That is the structural win over the five-row version, which had
/// to special-case a one-line summary because an arbitrary prefix of its rows
/// could drop the verdict and still look complete.
pub fn render_verification(state: &ProofState, area: Rect, buf: &mut Buffer) {
    if area.height < 3 {
        return;
    }
    let inner_w = area.width.saturating_sub(2) as usize;
    let capacity = (area.height as usize).saturating_sub(2);
    let standing = state.standing();
    let (glyph, ring) = verify_ring(standing.tone());
    let title = Line::from(vec![
        Span::styled(" PROOF ", Style::new().fg(theme::TEXT_SECONDARY)),
        Span::styled(format!("{glyph} "), ring),
        Span::styled(
            format!("{} ", standing.word()),
            proof::style_for(standing.tone()),
        ),
    ]);
    let rows: Vec<Line<'static>> = proof_body(state, inner_w)
        .into_iter()
        .take(capacity)
        .map(|line| Line::from(truncate_spans(line.spans, inner_w)))
        .collect();
    Paragraph::new(rows)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style(standing))
                .title(title),
        )
        .render(area, buf);
}

/// The PROOF border tracks the standing, so an unproven turn is legible from the
/// frame before a word is read.
fn border_style(standing: Standing) -> Style {
    match standing.tone() {
        Tone::Error => Style::new().fg(theme::DANGER),
        Tone::Warn => Style::new().fg(theme::WARN),
        Tone::Success => Style::new().fg(theme::SUCCESS),
        _ => theme::rule(),
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
        let rows = plan_rows(&plan, 12, 0, false);
        let text: Vec<String> = rows.iter().map(text_of).collect();
        assert!(
            text.iter().any(|r| r.contains("read the band layout")),
            "{text:?}"
        );
        assert!(text.iter().any(|r| r.contains("fold the rail")), "{text:?}");
    }

    /// The colour contract, asserted on the spans rather than a screenshot:
    /// done is a check on the green indicator with bright struck-through text,
    /// working carries the bright pulse tone, queued is muted grey — and each
    /// row is numbered.
    #[test]
    fn each_step_state_carries_its_own_ring_and_text_treatment() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one", "two", "three"]));
        plan.approve();
        plan.apply_board(&[
            item("1", "one", TaskStatus::Completed),
            item("2", "two", TaskStatus::InProgress),
        ]);
        let rows = plan_rows(&plan, 12, 0, false);
        // rows[0] is the summary line. Spans: glyph · gap · number · title.
        let done = &rows[1];
        assert_eq!(done.spans[0].content.as_ref(), "✓");
        assert_eq!(done.spans[0].style.bg, Some(theme::OK));
        assert_eq!(done.spans[2].content.as_ref(), "1. ");
        assert_eq!(done.spans[3].style.fg, Some(theme::TEXT_PRIMARY));
        assert!(
            done.spans[3]
                .style
                .add_modifier
                .contains(Modifier::CROSSED_OUT),
            "a finished step is struck through"
        );

        let working = &rows[2];
        assert_eq!(working.spans[0].content.as_ref(), "●");
        assert_eq!(working.spans[0].style.fg, Some(theme::TEXT_PRIMARY));
        assert_eq!(
            working.spans[2].style.fg, working.spans[3].style.fg,
            "the number carries the working tone with the title"
        );

        let queued = &rows[3];
        assert_eq!(queued.spans[0].content.as_ref(), "○");
        assert_eq!(queued.spans[0].style.fg, Some(theme::TEXT_TERTIARY));
    }

    /// Un-started work is muted grey even AFTER approval — approval commits
    /// the plan, it does not light up rows nobody is working on.
    #[test]
    fn a_planned_step_stays_muted_grey_after_approval() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one", "two"]));
        plan.approve();
        let rows = plan_rows(&plan, 12, 0, false);
        assert_eq!(rows[1].spans[0].style.fg, Some(theme::TEXT_TERTIARY));
        assert_eq!(rows[1].spans[3].style.fg, Some(theme::TEXT_TERTIARY));
    }

    /// Before the user answers the gate, nothing is committed — every ring is
    /// grey, including the plan's own.
    #[test]
    fn a_plan_awaiting_approval_is_grey_throughout() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one", "two"]));
        assert_eq!(plan.state, PlanState::PendingApproval);
        assert_eq!(plan_ring(plan.state).1.fg, Some(theme::TEXT_TERTIARY));
        let rows = plan_rows(&plan, 12, 0, false);
        assert_eq!(rows[1].spans[0].style.fg, Some(theme::TEXT_TERTIARY));
    }

    /// A failed/cancelled step paints its ENTIRE row red — glyph, number,
    /// title and the reason — with the ✗ cut out of the indicator.
    #[test]
    fn a_failed_step_paints_the_whole_row_red_and_names_why() {
        let mut plan = Plan::default();
        plan.propose(&proposal(&["one"]));
        plan.approve();
        plan.apply_board(&[item("1", "one", TaskStatus::Cancelled)]);
        let rows = plan_rows(&plan, 12, 0, false);
        assert_eq!(rows[1].spans[0].content.as_ref(), "✗");
        for span in &rows[1].spans {
            assert_eq!(
                span.style.bg,
                Some(theme::DANGER),
                "{:?} must ride the red row",
                span.content
            );
        }
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
        let rows = plan_rows(&plan, 6, 0, false);
        assert!(rows.len() <= 6, "{} rows for a cap of 6", rows.len());
        assert!(
            text_of(rows.last().unwrap()).contains("more"),
            "{:?}",
            text_of(rows.last().unwrap())
        );
    }

    #[test]
    fn an_empty_plan_says_so_rather_than_rendering_nothing() {
        let text: Vec<String> = plan_rows(&Plan::default(), 8, 0, false)
            .iter()
            .map(text_of)
            .collect();
        assert!(text.iter().any(|r| r.contains("no plan yet")), "{text:?}");
    }

    // ---- the PROOF panel ----

    /// **The guard on the copy.** Every standing's title must fit the narrowest
    /// rail the layout will ever draw.
    ///
    /// A ratatui block title clips with **no ellipsis at all** — unlike the body
    /// rows, which go through [`truncate_spans`] and at least admit the cut. So
    /// a word a few characters too long does not wrap, elide, or fail: it
    /// silently becomes a different word, in the one place on the frame that
    /// carries the whole answer. `nothing to prove` was exactly that word, and
    /// this test is why the copy reads `not needed`.
    #[test]
    fn every_standing_fits_the_narrowest_rail() {
        // The title lives inside the block's borders: two columns of corner,
        // then " PROOF " + a ring glyph and its space + the word + one trailing
        // space.
        let budget = RAIL_MIN_W as usize - 2;
        for standing in Standing::ALL {
            let cols = " PROOF ".chars().count() + 2 + standing.word().chars().count() + 1;
            assert!(
                cols <= budget,
                "`{}` needs {cols} columns of a {budget}-column title and would \
                 be clipped without an ellipsis",
                standing.word()
            );
        }
    }

    /// The shape channel has to carry the meaning on its own, for a
    /// colour-blind reader and for a monochrome terminal. Every tone therefore
    /// gets a distinct glyph — it used to be `●` for four of the five, which
    /// left colour as the only thing separating a proof from a failure.
    #[test]
    fn every_tone_has_its_own_shape_not_just_its_own_colour() {
        let glyphs: Vec<&str> = [
            Tone::Success,
            Tone::Warn,
            Tone::Error,
            Tone::Info,
            Tone::Muted,
        ]
        .into_iter()
        .map(|t| verify_ring(t).0)
        .collect();
        let mut distinct = glyphs.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), glyphs.len(), "two tones share a shape");
    }

    /// And the guard is load-bearing at the real narrow width, not just in
    /// arithmetic: the whole word survives a rendered minimum-width rail.
    #[test]
    fn the_standing_survives_a_minimum_width_rail() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Assurance {
            witness: false,
            verifier: false,
        });
        let area = Rect::new(0, 0, RAIL_MIN_W, VERIFY_H);
        let text = rendered(area, |b| render_verification(&state, area, b));
        assert!(text.contains("not needed"), "{text}");
    }

    /// **The regression this panel's redesign exists for.** A reader must be
    /// able to answer "is this proven?" from the title alone, in words they use
    /// — never from five rows of `stella-pipeline` stage names.
    #[test]
    fn the_title_carries_the_answer_in_the_readers_own_words() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::WitnessAuthored {
            path: "tests/clear_reset.rs".into(),
            command: "cargo test clear_reset".into(),
            fingerprint: "sha256:9f3c1d2e".into(),
        });
        state.apply(&ProofStep::Oracle {
            command: "cargo test clear_reset".into(),
            passed: false,
            tree: stella_protocol::ProofTree::Baseline,
            run: None,
            runs_required: None,
            seed: None,
        });
        state.apply(&ProofStep::Oracle {
            command: "cargo test clear_reset".into(),
            passed: true,
            tree: stella_protocol::ProofTree::Candidate,
            run: None,
            runs_required: None,
            seed: None,
        });

        let area = Rect::new(0, 0, 38, 5);
        let text = rendered(area, |b| render_verification(&state, area, b));
        assert!(text.contains("PROOF"), "{text}");
        assert!(text.contains("proved"), "{text}");
        for stage in ["warrant", "witness", "oracle", "tamper", "verdict"] {
            assert!(
                !text.contains(stage),
                "the pipeline's `{stage}` reached the panel:\n{text}"
            );
        }
    }

    /// The panel explains itself in plain sentences under that title.
    #[test]
    fn the_body_explains_the_standing_in_plain_sentences() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Warrant {
            required: false,
            reason: Some("documentation only; prose has no runtime behavior to flip".into()),
            diff_lines: 4,
        });
        let text: Vec<String> = proof_body(&state, 30).iter().map(text_of).collect();
        assert_eq!(
            text,
            vec![
                "documentation only; prose has",
                "no runtime behavior to flip"
            ],
            "the warrant's own reason, wrapped to the rail"
        );
    }

    /// The light changes colour where it stands, and the panel never changes
    /// height as it does — that fixed position is what makes it glanceable.
    #[test]
    fn the_light_changes_colour_without_the_panel_changing_height() {
        let area = Rect::new(0, 0, 38, VERIFY_H);
        let height_of = |state: &ProofState| -> usize {
            rendered(area, |b| render_verification(state, area, b))
                .lines()
                .count()
        };
        let mut state = ProofState::default();
        assert_eq!(state.standing().tone(), Tone::Muted);
        let before = height_of(&state);

        state.apply(&ProofStep::Warrant {
            required: true,
            reason: None,
            diff_lines: 41,
        });
        assert_eq!(state.standing().tone(), Tone::Info, "in flight");
        assert_eq!(height_of(&state), before);

        state.apply(&ProofStep::WitnessUnavailable {
            reason: "no author independent of the worker".into(),
        });
        assert_eq!(state.standing().tone(), Tone::Warn, "and then a problem");
        assert_eq!(height_of(&state), before);
    }

    /// A waived turn is a report, not an absence: the panel is up, and it says
    /// what was decided rather than going blank.
    #[test]
    fn a_turn_with_nothing_to_prove_still_says_so() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Assurance {
            witness: false,
            verifier: false,
        });
        state.finish();
        let area = Rect::new(0, 0, 38, VERIFY_H);
        let text = rendered(area, |b| render_verification(&state, area, b));
        assert!(text.contains("not needed"), "{text}");
    }

    // ---- rendering ----

    #[test]
    fn the_rail_draws_both_panels_with_the_plan_on_top() {
        let mut sm = SessionModel::default();
        sm.plan
            .propose(&proposal(&["read the layout", "fold the rail"]));
        sm.plan.approve();
        let area = Rect::new(0, 0, 34, 20);
        let text = rendered(area, |b| render(&sm, 0, false, area, b));
        let plan_at = text.find("PLAN").expect("a PLAN panel");
        let verify_at = text.find("PROOF").expect("a PROOF panel");
        assert!(plan_at < verify_at, "PLAN sits above PROOF");
        assert!(text.contains("read the layout"), "{text}");
        assert!(text.contains("checking"), "{text}");
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
        let text = rendered(area, |b| render(&sm, 0, false, area, b));
        assert!(text.contains('…'), "no ellipsis marks the cut:\n{text}");
    }

    /// **The regression the merge with the two-row statline exposed.** A gate
    /// card up top leaves the rail about nine rows; verification's fixed seven
    /// took nearly all of them and PLAN — the panel this rail is named for —
    /// rendered nothing at all.
    #[test]
    fn a_squeezed_rail_keeps_the_plan_and_shrinks_the_verification() {
        let mut sm = SessionModel::default();
        sm.plan.propose(&proposal(&["one", "two", "three"]));
        sm.plan.approve();
        let area = Rect::new(0, 0, 34, 9);
        let text = rendered(area, |b| render(&sm, 0, false, area, b));
        assert!(
            text.contains("PLAN"),
            "the plan survives the squeeze:\n{text}"
        );
        assert!(text.contains("one"), "…with its steps:\n{text}");
        assert!(
            text.contains("PROOF"),
            "…and PROOF is still present, just compact:\n{text}"
        );
    }

    /// The three rungs of the yield, as heights.
    #[test]
    fn verification_yields_its_rows_before_the_plan_loses_its() {
        assert_eq!(verification_height(PLAN_MIN_H + VERIFY_H), VERIFY_H);
        assert_eq!(verification_height(PLAN_MIN_H + 3), 3, "the summary rung");
        assert_eq!(verification_height(PLAN_MIN_H + 2), 0, "the plan takes it");
    }

    #[test]
    fn a_rail_too_short_for_both_panels_is_a_no_op_not_a_panic() {
        let sm = SessionModel::default();
        for h in 0..8u16 {
            let area = Rect::new(0, 0, 30, h);
            rendered(area, |b| render(&sm, 0, false, area, b));
        }
    }

    #[test]
    fn a_rail_too_narrow_to_draw_is_a_no_op_not_a_panic() {
        let sm = SessionModel::default();
        for w in 0..6u16 {
            let area = Rect::new(0, 0, w, 20);
            rendered(area, |b| render(&sm, 0, false, area, b));
        }
    }

    /// PROOF is pinned to the bottom at a fixed height, so its light does not
    /// move as the plan above it grows.
    #[test]
    fn the_light_holds_its_position_as_the_plan_grows() {
        let area = Rect::new(0, 0, 34, 24);
        let row_of = |steps: &[&str]| -> usize {
            let mut sm = SessionModel::default();
            sm.plan.propose(&proposal(steps));
            sm.plan.approve();
            let text = rendered(area, |b| render(&sm, 0, false, area, b));
            text.lines()
                .position(|l| l.contains("PROOF"))
                .expect("a PROOF title")
        };
        assert_eq!(row_of(&["one"]), row_of(&["one", "two", "three", "four"]));
    }
}
