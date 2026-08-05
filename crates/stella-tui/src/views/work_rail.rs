// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The work rail: scope, tasks and proof, rendered at whatever size the frame
//! can afford.
//!
//! # The problem this solves
//!
//! These three surfaces are the deck's answer to *what did I agree to, what is
//! it doing, and is any of it proven*. Each used to own a fixed band above or
//! below the transcript, and each declared its height before the transcript got
//! its `Min(1)` leftovers. Added up on an 80×24 terminal — 11 rows of deck
//! chrome, 1 header, 3 HUD, up to 10 TASKS, 7 PROOF — the fixed bands asked for
//! more rows than the content area has. The transcript, the one panel that is
//! always worth reading, was squeezed to nothing by two panels that on a
//! typical turn had nothing to say.
//!
//! Worse, the information was simultaneously *over*- and *under*-exposed. The
//! proof rail was up from triage's first event whether or not it had news,
//! while the approved scope — five steps the user had just consented to —
//! was destroyed the instant its gate closed and could not be recalled at all.
//!
//! # The shape
//!
//! One idea at three sizes, chosen by how much room there is and how much there
//! is to say:
//!
//! | Size | When | Cost |
//! |---|---|---|
//! | [`strip_line`] | always | **1 row** |
//! | [`render_rail`] | frame ≥ [`RAIL_MIN_COLS`] wide | 0 rows, [`RAIL_W`] columns |
//! | [`render_overlay`] | `⌃S` | 0 rows (floats) |
//!
//! plus the proof rail's own promotion to a band when it turns notable
//! ([`crate::views::proof::band_height`]).
//!
//! The strip is the load-bearing one: it is never absent, so the reader always
//! knows the scope exists, how the board is going and where the proof stands,
//! and it costs a fifteenth of what the two cards cost. Detail is a keystroke
//! away rather than permanently on screen, which is the right default for
//! information you check occasionally and read continuously only when it turns
//! bad.
//!
//! Pure line-builders where it is possible to be pure ([`strip_line`],
//! [`task_card_rows`]) so the collapse policy is unit-testable without a
//! terminal, matching the discipline in [`crate::proof`] and
//! [`crate::cache_panel`].

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use unicode_width::UnicodeWidthStr;

use stella_protocol::{TaskItem, TaskStatus};

use crate::model::SessionModel;
use crate::render::truncate_spans;
use crate::theme;
use crate::views::proof;

/// Narrowest frame that gets the right-hand rail. Below this the rail would
/// cost the transcript more columns than the cards are worth, so the strip
/// carries everything instead.
///
/// Set so a rail'd transcript still clears 78 columns — the width the deck's
/// own tool rows and diff headers are laid out for.
pub const RAIL_MIN_COLS: u16 = 110;

/// Columns the right-hand rail claims when it is up.
pub const RAIL_W: u16 = 32;

/// Most checklist rows the TASKS surface shows before collapsing the tail.
const TASK_CARD_MAX_ROWS: usize = 8;

/// Whether this frame is wide enough for the right-hand rail (Option B), and
/// there is anything to put in it.
pub fn rail_visible(sm: &SessionModel, width: u16) -> bool {
    width >= RAIL_MIN_COLS && (sm.approved_scope.is_some() || !sm.tasks.is_empty())
}

// ---------------------------------------------------------------------------
// The strip
// ---------------------------------------------------------------------------

/// Smallest task subject worth showing in the strip. Below this the cell is
/// dropped whole rather than elided to `co…`, which costs the same columns as
/// the counts it would crowd and says nothing.
const MIN_SUBJECT_COLS: usize = 12;

/// The one-row state strip: scope · tasks · proof, in that order, inside
/// `width` columns.
///
/// `None` when all three are empty — a fresh session with no plan, no board and
/// no proof has nothing to summarize, and a row of dashes is worse than no row.
///
/// Cells are omitted rather than dashed when their surface is absent, so the
/// strip's width tracks what is actually happening. The task cell carries the
/// *in-progress* item because "what is it doing right now" is the question a
/// glance at a running agent is asking; the counts answer "how far in".
///
/// # Fitting
///
/// The strip is one row and must never be cut off at the frame edge: a clipped
/// cell does not read as a truncated cell, it reads as a *different* cell —
/// `⚖ waived · nothing` is a phrase, and not the phrase the rail meant. So the
/// budget is spent deliberately rather than left to the terminal:
///
/// 1. the scope and task **counts** are near-fixed width and always fit;
/// 2. the proof cell is capped at `PROOF_CELL_COLS` and elided if longer
///    (its reasons are free text and can run to a sentence);
/// 3. the task **subject** takes whatever is left, and is dropped entirely
///    below `MIN_SUBJECT_COLS`.
///
/// A final `truncate_spans` pass backstops all of it, so no arithmetic slip
/// here can put a glyph past the frame edge.
pub fn strip_line(sm: &SessionModel, width: u16) -> Option<Line<'static>> {
    let width = width as usize;
    let label = Style::new().fg(theme::TEXT_TERTIARY);
    let mut spans: Vec<Span<'static>> = Vec::new();

    if let Some(scope) = &sm.approved_scope {
        spans.push(Span::styled(" ⌾ scope ", label));
        spans.push(Span::styled(
            format!("✓{}", scope.steps.len()),
            Style::new().fg(theme::OK),
        ));
    }

    let done = sm.tasks.iter().filter(|t| !t.status.is_open()).count();
    if !sm.tasks.is_empty() {
        if !spans.is_empty() {
            spans.push(Span::styled("   ", label));
        }
        spans.push(Span::styled(" ☑ ", label));
        spans.push(Span::styled(
            format!("{done}/{}", sm.tasks.len()),
            Style::new().fg(theme::TEXT_PRIMARY),
        ));
    }

    // Built before the subject so the subject can be given what is left over —
    // proof is the cell a reader cannot reconstruct from anywhere else on the
    // frame, and the task subject is the one that scrolls past in the
    // transcript anyway.
    let mut proof_cell: Vec<Span<'static>> = Vec::new();
    if !sm.proof.is_empty() {
        let summary = sm.proof.summary();
        proof_cell.push(Span::styled("   ", label));
        proof_cell.push(Span::styled(" ⚖ ", label));
        proof_cell.push(Span::styled(summary.value, proof::style_for(summary.tone)));
        proof_cell = truncate_spans(proof_cell, PROOF_CELL_COLS);
    }

    // The current task, if one is claimed. A board with nothing in progress is
    // a real state (all done, or nothing started) and says so rather than
    // borrowing a neighbouring row's subject.
    let active = sm.tasks.iter().find(|t| t.status == TaskStatus::InProgress);
    if let Some(active) = active {
        let spent = cols(&spans) + cols(&proof_cell);
        let room = width.saturating_sub(spent + 3); // 3 = the " ▸ " marker
        if room >= MIN_SUBJECT_COLS {
            spans.push(Span::styled(" ▸ ", theme::accent()));
            spans.extend(truncate_spans(
                vec![Span::styled(
                    active.subject.clone(),
                    Style::new().fg(theme::TEXT_PRIMARY),
                )],
                room,
            ));
        }
    } else if !sm.tasks.is_empty() && done == sm.tasks.len() {
        spans.push(Span::styled(" done", Style::new().fg(theme::OK)));
    }

    spans.extend(proof_cell);
    if spans.is_empty() {
        return None;
    }
    Some(Line::from(truncate_spans(spans, width)))
}

/// Widest the proof cell may grow before it is elided. Its reasons are free
/// text from the pipeline and can run to a sentence; the cell has to stay a
/// cell.
const PROOF_CELL_COLS: usize = 34;

/// Display columns a styled run occupies.
fn cols(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .map(|s| UnicodeWidthStr::width(&*s.content))
        .sum()
}

// ---------------------------------------------------------------------------
// The right-hand rail (Option B)
// ---------------------------------------------------------------------------

/// Draw the right-hand rail: the approved scope over the task board.
///
/// Vertical rows are the scarce resource on a terminal and columns usually are
/// not — a 140-column frame has 60 columns of slack and zero rows of it. So on
/// a wide frame the cards move sideways, where they cost the transcript some
/// clipping at the right margin instead of costing it whole rows.
pub fn render_rail(sm: &SessionModel, area: Rect, buf: &mut Buffer) {
    if area.height == 0 {
        return;
    }
    // Scope first: it is fixed-size and it is the thing being measured against.
    // The board grows, so it takes the remainder and does its own collapsing.
    let scope_h: u16 = if sm.approved_scope.is_some() { 4 } else { 0 };
    let bands = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Length(scope_h),
        ratatui::layout::Constraint::Min(0),
    ])
    .split(area);

    if let Some(scope) = &sm.approved_scope
        && bands[0].height >= 3
    {
        let lines = vec![
            Line::from(Span::styled(
                format!(
                    " {} steps · ~{} files",
                    scope.steps.len(),
                    scope.estimated_files
                ),
                Style::new().fg(theme::TEXT_SECONDARY),
            )),
            Line::from(Span::styled(
                match scope.estimated_cost_usd {
                    Some(c) => format!(" est ~${c:.2} · ⌃S opens"),
                    None => " ⌃S opens the plan".to_string(),
                },
                theme::muted(),
            )),
        ];
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::rule())
                    .title(" SCOPE ✓ "),
            )
            .render(bands[0], buf);
    }

    if sm.tasks.is_empty() || bands[1].height < 3 {
        return;
    }
    let done = sm.tasks.iter().filter(|t| !t.status.is_open()).count();
    // The rail is short as well as narrow, so the board is capped by the space
    // it was actually given — not by `TASK_CARD_MAX_ROWS`, which sizes a
    // full-width card.
    let capacity = (bands[1].height as usize).saturating_sub(2);
    // …and elided to the width it was given: `Paragraph` clips at the border,
    // and a subject cut mid-word with no ellipsis reads as the whole subject.
    let rows: Vec<Line<'static>> = task_card_rows_capped(&sm.tasks, capacity)
        .into_iter()
        .map(|line| {
            Line::from(truncate_spans(
                line.spans,
                (bands[1].width as usize).saturating_sub(2),
            ))
        })
        .collect();
    Paragraph::new(rows)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::rule())
                .title(format!(" TASKS {done}/{} ", sm.tasks.len())),
        )
        .render(bands[1], buf);
}

// ---------------------------------------------------------------------------
// The ⌃S overlay
// ---------------------------------------------------------------------------

/// Draw the STATE overlay: the approved plan, the whole board, and the full
/// five-row proof rail — everything the strip compressed, at once.
///
/// This is the answer to "I approved a scope and can never see it again". It
/// floats, so it costs no permanent rows and no other panel reflows when it
/// opens; and it always shows all five proof rows, including the muted ones the
/// promoted band filters out, because a reader who asked for the rail asked for
/// the whole rail.
///
/// `scroll` is the caller's offset in content rows, clamped here and returned
/// as the row count so the key handler can clamp the next keypress against what
/// was actually drawn — the same measure-during-render contract the deck's
/// other scrolling overlays use. A twelve-task board does not fit a 24-row
/// terminal, and an overlay that silently drops the rows past the fold would
/// reintroduce, inside the fix, exactly the defect it was built to remove.
pub fn render_overlay(sm: &SessionModel, scroll: usize, area: Rect, buf: &mut Buffer) -> usize {
    let mut lines: Vec<Line<'static>> = Vec::new();

    match &sm.approved_scope {
        Some(scope) => {
            lines.push(section(" SCOPE"));
            lines.push(Line::from(Span::styled(
                format!("  {}", scope.summary),
                Style::new()
                    .fg(theme::TEXT_PRIMARY)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                match scope.estimated_cost_usd {
                    Some(c) => format!(
                        "  {} steps · ~{} files · est ~${c:.2}",
                        scope.steps.len(),
                        scope.estimated_files
                    ),
                    None => format!(
                        "  {} steps · ~{} files",
                        scope.steps.len(),
                        scope.estimated_files
                    ),
                },
                theme::muted(),
            )));
            for (i, step) in scope.steps.iter().enumerate() {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {:>2}  ", i + 1), theme::muted()),
                    Span::styled(step.clone(), Style::new().fg(theme::TEXT_SECONDARY)),
                ]));
            }
        }
        None => {
            lines.push(section(" SCOPE"));
            lines.push(Line::from(Span::styled(
                // Distinguish "no gate was raised" from "a gate is waiting" —
                // the second is on screen as its own card, and saying nothing
                // here would imply this turn ran unapproved.
                if sm.pending_scope_review.is_some() {
                    "  awaiting your decision — see the card above"
                } else {
                    "  no scope gate was raised for this turn"
                },
                theme::muted(),
            )));
        }
    }

    lines.push(Line::default());
    let done = sm.tasks.iter().filter(|t| !t.status.is_open()).count();
    lines.push(section(&format!(" TASKS  {done}/{}", sm.tasks.len())));
    if sm.tasks.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no task board for this turn",
            theme::muted(),
        )));
    } else {
        // Every task, uncollapsed: the overlay is where the full board lives,
        // so the `… +K done` tail would be hiding the very thing it was opened
        // to show.
        lines.extend(sm.tasks.iter().map(task_row));
    }

    lines.push(Line::default());
    lines.push(section(" PROOF"));
    if sm.proof.is_empty() {
        lines.push(Line::from(Span::styled(
            "  nothing has been established yet",
            theme::muted(),
        )));
    } else {
        lines.extend(proof::rail_lines(&sm.proof.rows()));
    }

    let total = lines.len();
    let w = area.width.saturating_sub(8).clamp(20, 78);
    let h = (total as u16 + 2).min(area.height.saturating_sub(2)).max(3);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    let viewport = h.saturating_sub(2) as usize;
    let max_scroll = total.saturating_sub(viewport);
    let scroll = scroll.min(max_scroll);
    // Elide to the popup's interior for the same reason the rail does: a
    // clipped step is a different step.
    let visible: Vec<Line<'static>> = lines
        .into_iter()
        .skip(scroll)
        .take(viewport)
        .map(|line| Line::from(truncate_spans(line.spans, w.saturating_sub(2) as usize)))
        .collect();
    // The footer states the scroll position whenever there is one, so content
    // below the fold announces itself rather than simply not being there.
    let hint = if max_scroll > 0 {
        format!(
            " {}–{} of {total} · ↑↓ scroll · ⌃S or Esc closes ",
            scroll + 1,
            (scroll + viewport).min(total)
        )
    } else {
        " ⌃S or Esc closes ".to_string()
    };
    Clear.render(popup, buf);
    Paragraph::new(visible)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(theme::accent())
                .title(" STATE — scope · tasks · proof ")
                .title_bottom(
                    Line::from(Span::styled(hint, Style::new().fg(theme::TEXT_TERTIARY)))
                        .right_aligned(),
                ),
        )
        // No wrap: one logical line per row is what keeps the scroll math
        // line-exact (L-T4). Overflow is elided above, not reflowed here.
        .render(popup, buf);
    total
}

fn section(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        title.to_string(),
        Style::new()
            .fg(theme::TEXT_TERTIARY)
            .add_modifier(Modifier::BOLD),
    ))
}

// ---------------------------------------------------------------------------
// The task checklist (moved here from `views::session`)
// ---------------------------------------------------------------------------

/// Build the TASKS checklist rows from the latest board snapshot — pure, so
/// the collapse policy is unit-testable, and total on any board contents (no
/// indexing beyond what is counted, no unwraps).
///
/// - Empty board → no rows.
/// - A board that fits (`TASK_CARD_MAX_ROWS` or fewer) renders every task in
///   board order.
/// - A larger board prefers the open work (pending + in-progress, still in
///   board order) and folds everything hidden into one dim tail row —
///   `… +K done` for the finished/cancelled tasks, plus `+J more` if even the
///   open set overflows.
pub fn task_card_rows(tasks: &[TaskItem]) -> Vec<Line<'static>> {
    task_card_rows_capped(tasks, TASK_CARD_MAX_ROWS)
}

/// [`task_card_rows`] against an arbitrary row budget, so a surface that knows
/// its own height (the right-hand rail) collapses to fit it rather than to a
/// constant sized for somewhere else.
fn task_card_rows_capped(tasks: &[TaskItem], cap: usize) -> Vec<Line<'static>> {
    if tasks.is_empty() || cap == 0 {
        return Vec::new();
    }
    if tasks.len() <= cap {
        return tasks.iter().map(task_row).collect();
    }
    let open: Vec<&TaskItem> = tasks.iter().filter(|t| t.status.is_open()).collect();
    let done = tasks.len() - open.len();
    let shown = open.len().min(cap - 1);
    let mut rows: Vec<Line<'static>> = open.iter().take(shown).map(|t| task_row(t)).collect();
    let hidden_open = open.len() - shown;
    let mut parts: Vec<String> = Vec::new();
    if hidden_open > 0 {
        parts.push(format!("+{hidden_open} more"));
    }
    if done > 0 {
        parts.push(format!("+{done} done"));
    }
    if !parts.is_empty() {
        rows.push(Line::from(Span::styled(
            format!("… {}", parts.join(" · ")),
            theme::muted(),
        )));
    }
    rows
}

/// One checklist row: status glyph · `#id subject` · dim ` (owner)`.
fn task_row(task: &TaskItem) -> Line<'static> {
    let (glyph, glyph_style, text_style) = match task.status {
        TaskStatus::Pending => ("☐", theme::muted(), Style::new().fg(theme::TEXT_SECONDARY)),
        TaskStatus::InProgress => (
            "▸",
            theme::accent().add_modifier(Modifier::BOLD),
            Style::new()
                .fg(theme::TEXT_PRIMARY)
                .add_modifier(Modifier::BOLD),
        ),
        TaskStatus::Completed => (
            "✓",
            Style::new().fg(theme::SUCCESS),
            Style::new().fg(theme::TEXT_TERTIARY),
        ),
        TaskStatus::Cancelled => (
            "✗",
            Style::new().fg(theme::DANGER).add_modifier(Modifier::DIM),
            Style::new().fg(theme::TEXT_TERTIARY),
        ),
    };
    let mut spans = vec![
        Span::styled(format!(" {glyph} "), glyph_style),
        Span::styled(format!("#{} {}", task.id, task.subject), text_style),
    ];
    if let Some(owner) = &task.owner {
        spans.push(Span::styled(format!(" ({owner})"), theme::muted()));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests_support {
    use stella_protocol::{ScopeProposal, TaskItem, TaskStatus};

    /// A scope proposal for tests.
    pub(super) fn proposal(steps: &[&str]) -> ScopeProposal {
        ScopeProposal {
            summary: "collapse the tasks and proof panels".into(),
            steps: steps.iter().map(|s| (*s).to_string()).collect(),
            estimated_files: 9,
            estimated_cost_usd: Some(1.40),
            ..Default::default()
        }
    }

    pub(super) fn task(id: &str, subject: &str, status: TaskStatus) -> TaskItem {
        TaskItem {
            id: id.into(),
            subject: subject.into(),
            description: None,
            status,
            owner: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::*;
    use super::*;
    use crate::proof::ProofState;
    use stella_protocol::{ProofStep, ProofTree, VerdictEvidence};

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

    // ---- the strip ----

    #[test]
    fn a_session_with_nothing_to_say_builds_no_strip() {
        assert!(strip_line(&SessionModel::default(), 120).is_none());
    }

    /// The whole point of the strip: everything the two cards used to say, in
    /// one row. A golden, because its value IS that it reads at a glance — a
    /// change that makes it wordy is a regression no per-cell assertion sees.
    #[test]
    fn the_strip_carries_scope_tasks_and_proof_in_one_row() {
        let mut sm = SessionModel::default();
        sm.approved_scope = Some(proposal(&["a", "b", "c", "d", "e"]));
        sm.tasks = vec![
            task("1", "read the band layout", TaskStatus::Completed),
            task("2", "gate on relevance", TaskStatus::Completed),
            task("3", "collapse the proof rail", TaskStatus::InProgress),
            task("4", "pin the approved scope", TaskStatus::Pending),
        ];
        sm.proof.apply(&ProofStep::Assurance {
            witness: false,
            verifier: false,
        });
        let line = strip_line(&sm, 120).expect("a strip");
        assert_eq!(
            text_of(&line),
            " ⌾ scope ✓5    ☑ 2/4 ▸ collapse the proof rail    ⚖ waived · nothing to prove"
        );
    }

    /// A cell whose surface does not exist is omitted, not dashed — the strip's
    /// width should track what is actually happening.
    #[test]
    fn absent_surfaces_claim_no_cells() {
        let mut sm = SessionModel::default();
        sm.tasks = vec![task("1", "only a board", TaskStatus::InProgress)];
        let line = strip_line(&sm, 120).expect("a strip");
        let text = text_of(&line);
        assert!(!text.contains("scope"), "{text}");
        assert!(!text.contains("⚖"), "{text}");
        assert!(text.contains("☑ 0/1 ▸ only a board"), "{text}");
    }

    #[test]
    fn a_finished_board_says_done_rather_than_naming_a_stale_task() {
        let mut sm = SessionModel::default();
        sm.tasks = vec![
            task("1", "one", TaskStatus::Completed),
            task("2", "two", TaskStatus::Completed),
        ];
        assert!(text_of(&strip_line(&sm, 120).unwrap()).contains("☑ 2/2 done"));
    }

    /// The regression that motivated the strip's proof cell: a turn triage
    /// waived must read as settled, not as five rows of pending.
    #[test]
    fn a_waived_turn_reads_as_settled_in_one_cell() {
        let mut sm = SessionModel::default();
        sm.proof.apply(&ProofStep::Assurance {
            witness: false,
            verifier: false,
        });
        assert!(
            text_of(&strip_line(&sm, 120).unwrap()).contains("waived · nothing to prove"),
            "a waived turn must say so in the strip"
        );
    }

    #[test]
    fn an_unproven_turn_shouts_in_the_strip() {
        let mut sm = SessionModel::default();
        sm.proof.apply(&ProofStep::Warrant {
            required: true,
            reason: None,
            diff_lines: 41,
        });
        sm.proof.apply(&ProofStep::WitnessUnavailable {
            reason: "no independent author".into(),
        });
        let line = strip_line(&sm, 120).unwrap();
        assert!(text_of(&line).contains("⚠ warranted, NOT proven"));
        assert!(
            line.spans.iter().any(|s| s.style.fg == Some(theme::WARN)),
            "the cell must carry its tone, not just its words"
        );
    }

    // ---- the right rail (Option B) ----

    #[test]
    fn the_rail_stays_down_on_a_narrow_frame() {
        let mut sm = SessionModel::default();
        sm.tasks = vec![task("1", "something", TaskStatus::Pending)];
        assert!(!rail_visible(&sm, RAIL_MIN_COLS - 1));
        assert!(rail_visible(&sm, RAIL_MIN_COLS));
    }

    /// A wide frame with nothing to put in the rail must not draw an empty
    /// column — the transcript keeps the width.
    #[test]
    fn an_empty_session_never_raises_the_rail() {
        assert!(!rail_visible(&SessionModel::default(), 200));
    }

    #[test]
    fn the_rail_draws_the_scope_over_the_board() {
        let mut sm = SessionModel::default();
        sm.approved_scope = Some(proposal(&["a", "b", "c", "d", "e"]));
        sm.tasks = vec![
            task("1", "read the band layout", TaskStatus::Completed),
            task("2", "collapse the rail", TaskStatus::InProgress),
        ];
        let text = rendered(Rect::new(0, 0, RAIL_W, 12), |b| {
            render_rail(&sm, Rect::new(0, 0, RAIL_W, 12), b)
        });
        for expected in [
            "SCOPE ✓",
            "5 steps · ~9 files",
            "TASKS 1/2",
            "collapse the rail",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }

    /// The rail is short as well as narrow: a board bigger than the space it
    /// was given must collapse to fit rather than overflow the card.
    #[test]
    fn the_rail_collapses_the_board_to_the_height_it_was_given() {
        let tasks: Vec<TaskItem> = (1..=20)
            .map(|i| task(&i.to_string(), "work", TaskStatus::Pending))
            .collect();
        let rows = task_card_rows_capped(&tasks, 4);
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert!(text_of(&rows[3]).contains("+17 more"));
    }

    #[test]
    fn a_zero_height_rail_is_a_no_op() {
        let mut sm = SessionModel::default();
        sm.tasks = vec![task("1", "x", TaskStatus::Pending)];
        // Must not panic or index outside the buffer.
        rendered(Rect::new(0, 0, RAIL_W, 1), |b| {
            render_rail(&sm, Rect::new(0, 0, RAIL_W, 1), b)
        });
    }

    // ---- the ⌃S overlay ----

    /// The defect this whole change was reported for: an approved scope had to
    /// remain recallable. The steps are what was consented to, so the steps are
    /// what the overlay must show.
    #[test]
    fn the_overlay_shows_every_step_of_the_approved_scope() {
        let mut sm = SessionModel::default();
        sm.approved_scope = Some(proposal(&[
            "gate the rail on relevance",
            "pin the approved scope",
            "update the golden tests",
        ]));
        let text = rendered(Rect::new(0, 0, 80, 24), |b| {
            render_overlay(&sm, 0, Rect::new(0, 0, 80, 24), b);
        });
        for expected in [
            "STATE — scope · tasks · proof",
            "collapse the tasks and proof panels",
            "3 steps · ~9 files · est ~$1.40",
            "gate the rail on relevance",
            "pin the approved scope",
            "update the golden tests",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }

    /// The overlay is where the *whole* rail lives: a reader who asked for it
    /// gets the muted rows too, not the promoted band's filtered subset.
    #[test]
    fn the_overlay_shows_all_five_proof_rows_including_the_quiet_ones() {
        let mut sm = SessionModel::default();
        sm.proof.apply(&ProofStep::Assurance {
            witness: false,
            verifier: false,
        });
        let text = rendered(Rect::new(0, 0, 80, 24), |b| {
            render_overlay(&sm, 0, Rect::new(0, 0, 80, 24), b);
        });
        for label in ["warrant", "witness", "oracle", "tamper", "verdict"] {
            assert!(text.contains(label), "missing {label:?} in:\n{text}");
        }
    }

    /// An unapproved turn must not read as though it ran unapproved *silently*
    /// — the two "no scope" cases are different and say so.
    #[test]
    fn the_overlay_distinguishes_no_gate_from_a_waiting_gate() {
        let sm = SessionModel::default();
        let text = rendered(Rect::new(0, 0, 80, 24), |b| {
            render_overlay(&sm, 0, Rect::new(0, 0, 80, 24), b);
        });
        assert!(text.contains("no scope gate was raised"), "{text}");

        let mut waiting = SessionModel::default();
        waiting.pending_scope_review = Some(proposal(&["a"]));
        let text = rendered(Rect::new(0, 0, 80, 24), |b| {
            render_overlay(&waiting, 0, Rect::new(0, 0, 80, 24), b);
        });
        assert!(text.contains("awaiting your decision"), "{text}");
    }

    /// The board is shown whole here — the `… +K done` collapse is a
    /// small-card policy, and applying it in the overlay would hide exactly
    /// what the overlay was opened to show.
    #[test]
    fn the_overlay_never_collapses_the_board() {
        let mut sm = SessionModel::default();
        sm.tasks = (1..=12)
            .map(|i| {
                task(
                    &i.to_string(),
                    &format!("task number {i}"),
                    TaskStatus::Pending,
                )
            })
            .collect();
        let text = rendered(Rect::new(0, 0, 80, 40), |b| {
            render_overlay(&sm, 0, Rect::new(0, 0, 80, 40), b);
        });
        assert!(text.contains("task number 12"), "{text}");
        assert!(
            !text.contains("more"),
            "the overlay collapsed the board:\n{text}"
        );
    }

    #[test]
    fn the_overlay_survives_a_frame_too_small_to_hold_it() {
        let mut sm = SessionModel::default();
        sm.tasks = (1..=30)
            .map(|i| task(&i.to_string(), "work", TaskStatus::Pending))
            .collect();
        rendered(Rect::new(0, 0, 24, 5), |b| {
            render_overlay(&sm, 0, Rect::new(0, 0, 24, 5), b);
        });
    }

    // ---- the checklist ----

    #[test]
    fn task_card_rows_empty_board_builds_no_card() {
        assert!(task_card_rows(&[]).is_empty());
    }

    #[test]
    fn task_card_rows_small_board_shows_every_task_in_order_with_glyphs() {
        let tasks = vec![
            task("1", "one", TaskStatus::Completed),
            task("2", "two", TaskStatus::InProgress),
            task("3", "three", TaskStatus::Pending),
            task("4", "four", TaskStatus::Cancelled),
        ];
        let rows = task_card_rows(&tasks);
        assert_eq!(rows.len(), 4);
        assert!(text_of(&rows[0]).starts_with(" ✓ #1 one"));
        assert!(text_of(&rows[1]).starts_with(" ▸ #2 two"));
        assert!(text_of(&rows[2]).starts_with(" ☐ #3 three"));
        assert!(text_of(&rows[3]).starts_with(" ✗ #4 four"));
    }

    #[test]
    fn task_card_rows_large_board_prefers_open_work_and_collapses_the_done_tail() {
        let mut tasks: Vec<TaskItem> = (1..=6)
            .map(|i| task(&i.to_string(), "done", TaskStatus::Completed))
            .collect();
        tasks.extend((7..=10).map(|i| task(&i.to_string(), "open", TaskStatus::Pending)));
        let rows = task_card_rows(&tasks);
        assert!(rows.len() <= TASK_CARD_MAX_ROWS);
        assert!(text_of(&rows[0]).contains("#7 open"));
        assert!(text_of(rows.last().unwrap()).contains("+6 done"));
    }

    #[test]
    fn task_card_rows_overflowing_open_work_reports_the_hidden_count() {
        let tasks: Vec<TaskItem> = (1..=20)
            .map(|i| task(&i.to_string(), "open", TaskStatus::Pending))
            .collect();
        let rows = task_card_rows(&tasks);
        assert_eq!(rows.len(), TASK_CARD_MAX_ROWS);
        assert!(text_of(rows.last().unwrap()).contains("+13 more"));
    }

    #[test]
    fn a_verdict_summarizes_to_one_cell() {
        let mut state = ProofState::default();
        state.apply_verdict(
            true,
            &VerdictEvidence {
                summary: "ok".into(),
                deterministic: true,
                evidence_refs: vec![],
                ladder: None,
            },
        );
        assert_eq!(state.summary().value, "✓ passed · deterministic");
    }

    #[test]
    fn an_achieved_flip_outranks_the_verdict_in_the_summary() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Oracle {
            command: "cargo test w".into(),
            passed: false,
            tree: ProofTree::Baseline,
            run: None,
            runs_required: None,
            seed: None,
        });
        state.apply(&ProofStep::Oracle {
            command: "cargo test w".into(),
            passed: true,
            tree: ProofTree::Candidate,
            run: None,
            runs_required: None,
            seed: None,
        });
        assert_eq!(state.summary().value, "flip ✓ red→green");
    }
}

#[cfg(test)]
mod fitting_tests {
    use super::tests_support::*;
    use super::*;
    use stella_protocol::{ProofStep, TaskStatus};

    fn width_of(line: &Line<'static>) -> usize {
        line.spans
            .iter()
            .map(|s| UnicodeWidthStr::width(&*s.content))
            .sum()
    }

    /// The strip is one row and it is at the frame's full width, so anything
    /// that overflows is *cut off*, not wrapped. A cut cell reads as a shorter
    /// phrase rather than as a truncated one — `⚖ waived · nothing` looks like
    /// a complete, and wrong, statement.
    #[test]
    fn the_strip_never_overflows_the_frame() {
        let mut sm = SessionModel::default();
        sm.approved_scope = Some(proposal(&["a", "b", "c", "d", "e"]));
        sm.tasks = vec![
            task("1", "done", TaskStatus::Completed),
            task(
                "2",
                "collapse the tasks and proof panels into one relevance-gated state strip",
                TaskStatus::InProgress,
            ),
        ];
        sm.proof.apply(&ProofStep::WitnessUnavailable {
            reason: "no author independent of the worker was configured for this run".into(),
        });
        for width in [20u16, 40, 60, 80, 100, 120, 200] {
            let line = strip_line(&sm, width).expect("a strip");
            assert!(
                width_of(&line) <= width as usize,
                "strip overflowed {width} columns by {} — `{}`",
                width_of(&line) - width as usize,
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            );
        }
    }

    /// What survives the squeeze matters as much as that something does: the
    /// proof cell is the one a reader cannot get from anywhere else on the
    /// frame, so it outlives the task subject.
    #[test]
    fn a_narrow_strip_keeps_the_proof_and_drops_the_subject() {
        let mut sm = SessionModel::default();
        sm.tasks = vec![task(
            "1",
            "collapse the tasks and proof panels into one strip",
            TaskStatus::InProgress,
        )];
        sm.proof.apply(&ProofStep::WitnessUnavailable {
            reason: "no independent author".into(),
        });
        let text: String = strip_line(&sm, 40)
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("⚠ warranted"), "proof survives: {text}");
        assert!(!text.contains("collapse the"), "subject dropped: {text}");
        assert!(text.contains("☑ 0/1"), "counts survive: {text}");
    }

    /// The rail is 32 columns and `Paragraph` clips at its border with no
    /// ellipsis, so a long subject silently became a different subject.
    #[test]
    fn rail_rows_are_elided_rather_than_clipped() {
        let mut sm = SessionModel::default();
        sm.tasks = vec![task(
            "1",
            "collapse the tasks and proof panels into one relevance-gated strip",
            TaskStatus::InProgress,
        )];
        let area = Rect::new(0, 0, RAIL_W, 10);
        let mut buf = Buffer::empty(area);
        render_rail(&sm, area, &mut buf);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains('…'), "no ellipsis marks the cut:\n{text}");
    }

    /// **The defect the visual check caught.** The overlay's whole job is
    /// showing what the strip compressed; dropping the rows past the fold
    /// would rebuild the original complaint inside its own fix.
    #[test]
    fn a_board_too_tall_for_the_frame_scrolls_instead_of_vanishing() {
        let mut sm = SessionModel::default();
        sm.approved_scope = Some(proposal(&["a", "b", "c", "d", "e"]));
        sm.tasks = (1..=20)
            .map(|i| {
                task(
                    &i.to_string(),
                    &format!("task number {i}"),
                    TaskStatus::Pending,
                )
            })
            .collect();
        sm.proof.apply(&ProofStep::Assurance {
            witness: false,
            verifier: false,
        });
        let area = Rect::new(0, 0, 80, 24);

        let draw = |scroll: usize| {
            let mut buf = Buffer::empty(area);
            let total = render_overlay(&sm, scroll, area, &mut buf);
            let text: String = (0..area.height)
                .map(|y| {
                    (0..area.width)
                        .map(|x| buf[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            (total, text)
        };

        let (total, top) = draw(0);
        assert!(
            total > 22,
            "this board must not fit, or the test proves nothing"
        );
        assert!(top.contains("SCOPE"), "the top starts at the top:\n{top}");
        assert!(
            top.contains("of 3") || top.contains(&format!("of {total}")),
            "a clipped overlay must state its position:\n{top}"
        );

        // Everything below the fold is reachable, including the last proof row.
        let (_, bottom) = draw(total);
        assert!(
            bottom.contains("verdict"),
            "the last row must be reachable by scrolling:\n{bottom}"
        );
    }

    /// Scrolling past the end clamps rather than emptying the popup.
    #[test]
    fn scrolling_past_the_end_still_shows_content() {
        let mut sm = SessionModel::default();
        sm.tasks = vec![task("1", "one", TaskStatus::Pending)];
        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        render_overlay(&sm, 9_999, area, &mut buf);
        let text: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("STATE"), "the popup is still drawn:\n{text}");
        assert!(text.contains("#1 one"), "…with content in it:\n{text}");
    }
}
