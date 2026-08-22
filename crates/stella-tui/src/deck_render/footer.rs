// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The quiet keybind + line-counter row directly under the composer and above
//! the statline.
//!
//! Its own module for the same reason `parked` is: `deck_render.rs` is a god
//! file closed to growth (`scripts/file-size-baseline.txt`).
//!
//! The row is two groups sharing one line — a right-aligned readout (turn
//! cost · line counter · queue status) whose width is decided by its own
//! contents, and a left-hand run of keybind hints that has to live inside
//! whatever the readout leaves over. Fitting the hints is therefore a *budget*
//! problem, and the budget is spent by [`compose`]: hints are admitted in
//! **priority** order and drawn in **display** order, so a narrow row loses
//! the least useful affordance rather than the rightmost one, and never
//! renders half a label against the readout (#3591).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use super::display_width;
use crate::composer::ComposerLayout;
use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::{textline, theme};

/// Drawn between two admitted hints, and nowhere else — the witness test
/// splits the finished row on its `·` to recover the hints it holds.
const SEP: &str = "  ·  ";

/// A blank column ahead of the first glyph, so it never sits on the frame edge.
const LEAD: usize = 1;

/// One keybind affordance on the footer's left-hand run.
struct Hint {
    /// The key or chord, drawn in the accent colour.
    glyph: &'static str,
    /// What the key does, drawn dim. Carries its own leading space.
    label: &'static str,
    /// Rank in the width budget — higher wins a contested row, and ties keep
    /// display order. It is deliberately independent of where the hint is
    /// drawn: value to a user watching a turn and left-to-right position are
    /// different questions.
    priority: u8,
}

impl Hint {
    /// Columns this hint occupies, its separator excluded.
    fn width(&self) -> usize {
        display_width(self.glyph) + display_width(self.label)
    }
}

/// Pick the hints that fit in `budget` columns and lay them out.
///
/// Greedy by priority: the most valuable hint is offered the budget first, and
/// one that does not fit is skipped rather than ending the run — a cheap
/// low-priority hint still gets the columns an expensive high-priority one
/// could not use.
///
/// The returned spans are never wider than `budget`, which is the whole point:
/// the caller clips them to a `Rect`, and a run that already fits cannot be
/// cut mid-word against the readout beside it (#3591).
fn compose(
    hints: &[Hint],
    budget: usize,
    key: Style,
    dim: Style,
    sep: Style,
) -> Vec<Span<'static>> {
    let mut admitted = vec![false; hints.len()];
    let mut used = LEAD;
    if used <= budget {
        let mut by_value: Vec<usize> = (0..hints.len()).collect();
        // Stable, so equal priorities fall back to display order.
        by_value.sort_by_key(|&i| std::cmp::Reverse(hints[i].priority));
        for i in by_value {
            let sep_cost = if used > LEAD { display_width(SEP) } else { 0 };
            let cost = sep_cost + hints[i].width();
            if used + cost <= budget {
                used += cost;
                admitted[i] = true;
            }
        }
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, hint) in hints.iter().enumerate() {
        if !admitted[i] {
            continue;
        }
        spans.push(if spans.is_empty() {
            Span::raw(" ")
        } else {
            Span::styled(SEP, sep)
        });
        spans.push(Span::styled(hint.glyph, key));
        spans.push(Span::styled(hint.label, dim));
    }
    spans
}

/// Total display width of a span run, in terminal columns.
fn span_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| display_width(&s.content)).sum()
}

/// Render `spans` right-aligned at the end of `area` without clearing the rest
/// of the row — draws only its own sub-rect, so a left-aligned line drawn first
/// survives everywhere it doesn't overlap.
fn render_right(spans: Vec<Span<'static>>, area: Rect, buf: &mut Buffer) {
    let w = span_width(&spans).min(area.width as usize) as u16;
    let rect = Rect {
        x: area.x + area.width.saturating_sub(w),
        y: area.y,
        width: w,
        height: 1,
    };
    Paragraph::new(Line::from(spans)).render(rect, buf);
}

/// Draw the footer. Keybind glyphs are violet; the right end carries the live
/// line counter and the queue status.
pub(super) fn render_composer_footer(
    model: &WorkspaceModel,
    ui: &DeckUi,
    _layout: &ComposerLayout,
    area: Rect,
    buf: &mut Buffer,
) {
    if area.height == 0 {
        return;
    }
    let key = Style::default().fg(theme::VIOLET);
    let dim = Style::default().fg(theme::TEXT_TERTIARY);
    let sep = Style::default().fg(theme::HAIRLINE);

    // Right: live logical-line counter · queue status. Built first so its width
    // is known and the left affordances can be budgeted against what remains.
    let n_lines = ui.composer.buffer().split('\n').count().max(1);
    let counter = format!("{n_lines} line{}", if n_lines == 1 { "" } else { "s" });
    let pending = model.queue.pending();
    let (q_text, q_style) = if pending > 0 && ui.dispatch_held {
        (
            format!("{pending} held"),
            Style::default().fg(theme::DANGER),
        )
    } else if pending > 0 {
        (
            format!("{pending} queued"),
            Style::default().fg(theme::WARNING_BRIGHT),
        )
    } else {
        ("queue empty".to_string(), dim)
    };
    // Live turn cost, pinned nearest the composer's left edge of the right
    // cluster — the readout that replaced the `◇ spend` rows the transcript
    // used to print once per model call.
    //
    // A gauge, not an event: it updates in place as ticks land and settles on
    // the turn's real total when `Complete` arrives, at which point the
    // transcript prints the same figure in the same green (`✓ cost`) as the
    // turn's permanent receipt. Four decimals, because the whole value of a
    // live cost readout is watching it move, and at two decimals most turns
    // never leave `$0.00`.
    let turn_cost = model
        .agents
        .get(ui.focused)
        .map_or(0.0, |a| a.model.hud.turn_spent_usd());
    let right = vec![
        Span::styled("turn ", dim),
        Span::styled(
            textline::fmt_cost(turn_cost),
            Style::default()
                .fg(theme::SUCCESS_BRIGHT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  ·  ", sep),
        Span::styled(counter, dim),
        Span::styled("  ·  ", sep),
        Span::styled(q_text, q_style),
        Span::raw(" "),
    ];
    let right_w = span_width(&right) as u16;

    // The left run, in display order. Priorities rank the same hints by value:
    //
    // * **steer** is the one thing a user watching a turn go the wrong way
    //   needs to know (`esc` is the shorter reach for the same act), so it
    //   outranks everything.
    //   Shown only while the focused agent is actually running, because that is
    //   the only time it does anything: a `>` typed at an idle agent is just
    //   the first character of the next prompt.
    // * the **newline chord** is the one the terminal can actually report —
    //   `⌘⏎`/`⌃⏎` where the kitty protocol is live, else the universally-safe
    //   `⌥⏎` — and a driver who cannot find it cannot write a second line.
    // * the **queue** hint explains what a prompt typed mid-turn does.
    // * `!` and `/` are discoverable by typing them, so they yield first.
    let newline = if ui.enter_submits { "⌥⏎" } else { "⌘⏎" };
    let steering = model
        .agents
        .get(ui.focused)
        .is_some_and(|a| a.status == crate::AgentStatus::Running);
    let mut hints = vec![
        Hint {
            glyph: newline,
            label: " new line",
            priority: 3,
        },
        Hint {
            glyph: "⏎",
            label: " queue (never blocks)",
            priority: 2,
        },
    ];
    if steering {
        hints.push(Hint {
            glyph: "esc / >",
            label: " steer this turn",
            priority: 4,
        });
    }
    hints.push(Hint {
        glyph: "!",
        label: " shell",
        priority: 1,
    });
    hints.push(Hint {
        glyph: "/",
        label: " commands",
        priority: 0,
    });

    // One column short of the clipping rect, so the run always lands with a
    // blank between it and the readout instead of touching it.
    let left_budget = area.width.saturating_sub(right_w).saturating_sub(1) as usize;
    let left = compose(&hints, left_budget, key, dim, sep);
    let left_area = Rect {
        width: area.width.saturating_sub(right_w),
        ..area
    };
    Paragraph::new(Line::from(left)).render(left_area, buf);
    render_right(right, area, buf);
}
