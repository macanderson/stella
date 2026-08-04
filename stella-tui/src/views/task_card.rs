//! The task-board card (`/tasks`): the focused agent's `task_*` board as a
//! floating checklist — done rows struck through, the active row accented
//! with its live elapsed/cost, queued rows dim. Rendered purely from the
//! `TaskUpdate` fold (this is the *agent task board*, never the fleet
//! ledger's same-named unit); `x`/skip leaves as a `WorkspaceInput` and the
//! row only changes when the next snapshot folds back.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use stella_protocol::{TaskItem, TaskStatus};

use crate::deck::{ActiveTaskStamp, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::theme;
use crate::views::cards;

/// Cells the title's mini done-fraction bar spans.
const TITLE_BAR_W: usize = 7;

/// One task row (marker + status glyph + subject, active rows carrying the
/// right-aligned `elapsed · $cost`), or the labeled-record form in
/// accessible mode.
fn task_row(
    task: &TaskItem,
    selected: bool,
    stamp: Option<&ActiveTaskStamp>,
    agent_cost: f64,
    now_ms: u64,
    inner_w: usize,
    accessible: bool,
) -> Line<'static> {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let (glyph, style) = match task.status {
        TaskStatus::Completed => (
            Span::styled("✓ ", Style::new().fg(theme::OK)),
            dim.add_modifier(Modifier::CROSSED_OUT),
        ),
        TaskStatus::InProgress => (
            Span::styled("▸ ", theme::accent().add_modifier(Modifier::BOLD)),
            Style::new().fg(theme::TEXT_PRIMARY),
        ),
        TaskStatus::Pending => (Span::styled("○ ", dim), dim),
        TaskStatus::Cancelled => (
            Span::styled("✕ ", dim),
            dim.add_modifier(Modifier::CROSSED_OUT),
        ),
    };
    // The active task's live readout: elapsed and spend since the board
    // flipped it active (the stamp the deck fold keeps).
    let live = (task.status == TaskStatus::InProgress)
        .then(|| stamp.filter(|s| s.id == task.id))
        .flatten()
        .map(|s| {
            (
                cards::fmt_mss(now_ms.saturating_sub(s.started_ms)),
                (agent_cost - s.cost_at_start_usd).max(0.0),
            )
        });
    if accessible {
        let status_word = match task.status {
            TaskStatus::Completed => "done",
            TaskStatus::InProgress => "active",
            TaskStatus::Pending => "queued",
            TaskStatus::Cancelled => "cancelled",
        };
        let mut text = format!(
            "· task {} · status {status_word} · {}",
            task.id, task.subject
        );
        if let Some((elapsed, cost)) = &live {
            text.push_str(&format!(" · elapsed {elapsed} · cost ${cost:.2}"));
        }
        return Line::from(Span::styled(text, style));
    }
    let mut row = vec![
        cards::marker(selected),
        glyph,
        Span::styled(
            cards::truncate_cols(&task.subject, inner_w.saturating_sub(18)),
            style,
        ),
    ];
    if let Some((elapsed, cost)) = live {
        row = cards::pad_right(
            row,
            Span::styled(format!("{elapsed} · ${cost:.2}"), dim),
            inner_w,
        );
    }
    Line::from(row)
}

/// Render the task card over `frame` for the focused agent.
pub fn render(model: &WorkspaceModel, ui: &mut DeckUi, frame: Rect, buf: &mut Buffer) {
    let Some(agent) = model.agents.get(ui.focused) else {
        return;
    };
    let tasks = &agent.model.tasks;
    let inner_w = (cards::CARD_MAX_W - 2) as usize;
    // Clamp the selection against the current board — the same idempotent
    // clamp-at-render every scrolling surface uses (panel_guard class 2).
    ui.cards.tasks_sel = ui.cards.tasks_sel.min(tasks.len().saturating_sub(1));

    let done = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Completed)
        .count();
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut selected_row: Option<usize> = None;
    if tasks.is_empty() {
        rows.push(Line::from(Span::styled(
            if ui.accessible {
                "· no tasks on the board yet"
            } else {
                "no tasks on the board yet"
            },
            Style::new().fg(theme::TEXT_TERTIARY),
        )));
    }
    for (i, task) in tasks.iter().enumerate() {
        let selected = i == ui.cards.tasks_sel;
        if selected {
            selected_row = Some(rows.len());
        }
        rows.push(task_row(
            task,
            selected,
            agent.active_task.as_ref(),
            agent.cost_usd,
            model.now_ms,
            inner_w,
            ui.accessible,
        ));
        // ⏎ expands the selected row into its long-form description.
        if selected && ui.cards.tasks_expanded {
            if let Some(description) = &task.description {
                let text = if ui.accessible {
                    format!("· detail {description}")
                } else {
                    format!("     {}", cards::truncate_cols(description, inner_w - 6))
                };
                rows.push(Line::from(Span::styled(
                    text,
                    Style::new().fg(theme::TEXT_TERTIARY),
                )));
            }
        }
    }

    let area = cards::card_area(frame, rows.len() as u16, cards::CARD_MAX_W, ui.accessible);
    let mut context = vec![Span::styled(
        format!("{done} of {} done", tasks.len()),
        Style::new().fg(theme::TEXT_TERTIARY),
    )];
    if !ui.accessible {
        // The mini done-fraction bar rides the title beside the counts. A
        // distinct ▰▱ widget, deliberately not the brand-gradient `█` bar.
        context.push(Span::raw(" "));
        context.extend(cards::mini_fraction_bar(
            done,
            tasks.len().max(1),
            TITLE_BAR_W,
            theme::OK,
        ));
    }
    let hints = if crate::deck_ui::cards::selected_task_skippable(model, ui) {
        "↑↓ select · ⏎ expand · x skip"
    } else {
        "↑↓ select · ⏎ expand"
    };
    let inner = cards::card_frame(area, "tasks", context, hints, buf);
    cards::render_body(rows, selected_row, inner, buf);
}
