// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Painting the engine panel into the SETTINGS tab's content area.
//!
//! Five bands, no box: the tab strip with the modified marker on the right
//! edge, a row of air, the config rows, the driver's status line, and the
//! legend. The panel used to draw a full-height bordered block with its title
//! and legend on the border itself; on the SPEC 5 frame the tab strip above it
//! and the hint row below it already delimit the body, so a second frame
//! inside the first spent two columns and two rows saying what the reader can
//! already see.
//!
//! The one box left is the model picker, which floats over the rows and has to
//! say where it ends.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Widget};
use stella_tui_theme::{glyph, token};

use super::tabs::{EngineTab, GLOBAL_ROWS, GlobalRow};
use super::{AgentField, EngineOverlay, NO_SNAPSHOT_HINT, picker_matches};
use crate::deck_ui::DeckUi;
use crate::envelope::{EngineConfigState, EngineRole};
use crate::render::scroll_window_start;

/// Label column width — fits the longest key (`repetition_penalty`, 18).
const LABEL_W: usize = 19;

/// Draw the panel into `area`, the SETTINGS tab's content area below the pane
/// nav. The model picker, when open, floats over the result.
pub fn render(ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    let e = &ui.engine;
    if area.width < 8 || area.height < 4 {
        return; // no readable panel fits — draw nothing rather than garbage
    }

    let bands = Layout::vertical([
        Constraint::Length(1), // tab strip · modified
        Constraint::Length(1), // air
        Constraint::Min(1),    // the config rows
        Constraint::Length(1), // driver status / busy
        Constraint::Length(1), // legend
    ])
    .split(area);

    render_strip(e, bands[0], buf);
    render_rows(e, bands[2], buf);
    render_status(e, bands[3], buf);
    render_legend(e, bands[4], buf);

    if e.picker.is_some() {
        render_model_picker(e, area, buf);
    }
}

/// `GLOBAL   default` with the active page in gold, and `modified` holding the
/// right edge while the working copy differs from the driver's last snapshot.
fn render_strip(e: &EngineOverlay, area: Rect, buf: &mut Buffer) {
    let mut spans = vec![Span::raw(" ")];
    for (i, tab) in EngineTab::ALL.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(" "));
        }
        if *tab == e.tab {
            spans.push(Span::styled(
                format!("  {}  ", tab.label()),
                Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                tab.label().to_string(),
                Style::new().fg(token::MUTED),
            ));
        }
    }
    if e.dirty() {
        let used: usize = spans.iter().map(Span::width).sum();
        let marker = "modified ";
        let width = area.width as usize;
        if used + marker.len() < width {
            spans.push(Span::raw(" ".repeat(width - used - marker.len())));
            spans.push(Span::styled(marker, Style::new().fg(token::GOLD_BRIGHT)));
        }
    }
    Paragraph::new(Line::from(spans)).render(area, buf);
}

/// The windowed config rows, or the wait hint when no snapshot has landed.
fn render_rows(e: &EngineOverlay, area: Rect, buf: &mut Buffer) {
    let Some(state) = &e.state else {
        Paragraph::new(Line::from(Span::styled(
            format!("  {NO_SNAPSHOT_HINT}"),
            Style::new().fg(token::MUTED),
        )))
        .render(area, buf);
        return;
    };
    let count = e.row_count();
    let sel = e.row.min(count.saturating_sub(1));
    let visible = (area.height as usize).max(1);
    let first = scroll_window_start(count, sel, visible);
    let last = (first + visible).min(count);
    let lines: Vec<Line<'static>> = (first..last)
        .map(|i| row(e, state, i, i == sel, area.width as usize))
        .collect();
    Paragraph::new(lines).render(area, buf);
}

/// One config row: `▸ label  value`, the whole line lifted onto the highlight
/// background when selected. `None` values render dimmed as "(provider
/// default)"; an active inline edit shows the live buffer with a caret instead
/// of the stored value.
fn row(
    e: &EngineOverlay,
    state: &EngineConfigState,
    i: usize,
    is_sel: bool,
    width: usize,
) -> Line<'static> {
    // Marker (2) + label column + one cell of air bound the value width.
    let value_w = width.saturating_sub(2 + LABEL_W + 1).max(8);
    let (label, value): (&str, Option<String>) = match e.tab {
        EngineTab::Global => {
            let global = GLOBAL_ROWS[i.min(GLOBAL_ROWS.len() - 1)];
            let value = match global {
                // The one list row: an empty list falls through to the dimmed
                // placeholder below.
                GlobalRow::AllowedModels => {
                    (!state.allowed_models.is_empty()).then(|| state.allowed_models.join(", "))
                }
                toggle => toggle
                    .flag(state)
                    .map(|on| (if on { "on" } else { "off" }).to_string()),
            };
            (global.label(), value)
        }
        EngineTab::Agent(role) => {
            let field = AgentField::ALL[i.min(AgentField::ALL.len() - 1)];
            let value = state.agent(role).and_then(|a| field.value(a));
            (field.label(), value)
        }
    };

    let mut spans = vec![
        Span::styled(
            if is_sel {
                format!("{} ", glyph::COLLAPSED)
            } else {
                "  ".to_string()
            },
            Style::new().fg(token::GOLD),
        ),
        Span::styled(format!("{label:<LABEL_W$} "), Style::new().fg(token::MUTED)),
    ];

    if let Some(edit) = e.edit.as_ref().filter(|edit| edit.row == i) {
        // The live buffer, tail-windowed so the caret end stays visible on
        // long values (a prompt), with the gold caret the composer uses.
        spans.push(Span::styled(
            tail_chars(&edit.buffer, value_w.saturating_sub(1)),
            Style::new().fg(token::TEXT),
        ));
        spans.push(Span::styled("▏", Style::new().fg(token::GOLD)));
    } else {
        match value {
            Some(v) => spans.push(Span::styled(
                truncate_chars(&v, value_w),
                Style::new().fg(token::TEXT),
            )),
            None => spans.push(Span::styled(
                match e.tab {
                    EngineTab::Global => "(none — pickers offer the catalog)",
                    EngineTab::Agent(_) => "(provider default)",
                },
                Style::new().fg(token::DIM),
            )),
        }
    }

    let mut line = Line::from(spans);
    if is_sel {
        line.style = Style::new().bg(token::HL).add_modifier(Modifier::BOLD);
    }
    line
}

/// The driver/local status line ("saved", parse errors), or the busy hint.
fn render_status(e: &EngineOverlay, area: Rect, buf: &mut Buffer) {
    let status = e
        .status
        .clone()
        .or_else(|| e.busy.then(|| "working…".to_string()));
    let Some(status) = status else { return };
    Paragraph::new(Line::from(Span::styled(
        format!(" {status}"),
        Style::new().fg(token::GOLD),
    )))
    .render(area, buf);
}

/// The legend tracks focus: while the panel owns the keyboard it teaches its
/// verbs; otherwise it teaches the one key that grants focus. Keys muted,
/// their meanings dim — the hint row's own two tones, so one legend is read
/// the same way everywhere.
fn render_legend(e: &EngineOverlay, area: Rect, buf: &mut Buffer) {
    let key = Style::new().fg(token::MUTED);
    let dim = Style::new().fg(token::DIM);
    let pairs: &[(&str, &str)] = if e.focused {
        &[
            ("⇥", "agent"),
            ("⏎", "edit"),
            ("space", "toggle"),
            ("x", "clear"),
            ("s", "save user"),
            ("S", "save project"),
            ("r", "reload"),
            ("esc", "done"),
        ]
    } else {
        &[("e", "edit agents config")]
    };
    let mut spans = vec![Span::raw(" ")];
    for (i, (chord, does)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled(*chord, key));
        spans.push(Span::styled(format!(" {does}"), dim));
    }
    Paragraph::new(Line::from(spans)).render(area, buf);
}

/// The model-picker sub-overlay, centered over the panel: the graph tab's file
/// picker idiom (filter line, windowed matches, legend), and the one bordered
/// box left here — it floats over the rows, so it has to say where it ends.
fn render_model_picker(e: &EngineOverlay, area: Rect, buf: &mut Buffer) {
    let Some(picker) = &e.picker else { return };
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let w = area.width.min(56);
    let h = area.height.min(16);
    if w < 4 || h < 4 {
        return;
    }
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    Clear.render(popup, buf);

    let role_label = e.role().map(EngineRole::key).unwrap_or("agent");
    let current = e
        .role()
        .and_then(|role| e.state.as_ref().and_then(|s| s.agent(role)))
        .and_then(|a| a.model.clone());
    let matches = e
        .state
        .as_ref()
        .map(|s| picker_matches(s, &picker.query))
        .unwrap_or_default();

    let inner_h = (h as usize).saturating_sub(2);
    let visible = inner_h.saturating_sub(2).max(1);
    let sel = picker.sel.min(matches.len().saturating_sub(1));
    let first = scroll_window_start(matches.len(), sel, visible);
    let last = (first + visible).min(matches.len());

    let mut lines: Vec<Line<'static>> = vec![Line::from(vec![
        Span::styled("filter ", muted),
        Span::styled(picker.query.clone(), Style::new().fg(token::TEXT)),
        Span::styled("▏", Style::new().fg(token::GOLD)),
    ])];

    if e.state.is_none() {
        lines.push(Line::from(Span::styled(
            format!("  {NO_SNAPSHOT_HINT}"),
            muted,
        )));
    } else if matches.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no models match — Backspace to widen",
            muted,
        )));
    }
    for (i, slug) in matches.iter().enumerate().take(last).skip(first) {
        let is_sel = i == sel;
        let mut spans = vec![
            Span::styled(
                if is_sel {
                    format!("{} ", glyph::COLLAPSED)
                } else {
                    "  ".to_string()
                },
                Style::new().fg(token::GOLD),
            ),
            Span::styled(
                truncate_chars(slug, (w as usize).saturating_sub(6)),
                Style::new().fg(token::TEXT),
            ),
        ];
        if current.as_deref() == Some(slug.as_str()) {
            spans.push(Span::styled("  · current", dim));
        }
        let mut line = Line::from(spans);
        if is_sel {
            line.style = Style::new().bg(token::HL).add_modifier(Modifier::BOLD);
        }
        lines.push(line);
    }

    // Pad so the legend sits on the last interior row regardless of matches.
    while lines.len() < inner_h.saturating_sub(1).max(1) {
        lines.push(Line::default());
    }
    lines.push(Line::from(vec![
        Span::styled(" type", muted),
        Span::styled(" to filter · ", dim),
        Span::styled("↑↓", muted),
        Span::styled(" select · ", dim),
        Span::styled("⏎", muted),
        Span::styled(" pick · ", dim),
        Span::styled("esc", muted),
        Span::styled(" back", dim),
    ]));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(token::BORDER))
        .title(Line::from(vec![
            Span::styled(" model ", Style::new().fg(token::GOLD)),
            Span::styled(format!("· {role_label} · "), dim),
            Span::styled(format!("{} available ", matches.len()), muted),
        ]));
    Paragraph::new(lines).block(block).render(popup, buf);
}

/// Char-safe prefix truncation with an ellipsis (long prompts, long model
/// lists must never wrap the row).
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}…")
}

/// The last `max_chars` of a buffer (edit rendering keeps the caret end in
/// view), with a leading ellipsis when the head is cut.
fn tail_chars(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let tail: String = s
        .chars()
        .skip(count - max_chars.saturating_sub(1))
        .collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::open_ui;
    use super::super::{EngineTab, ModelPicker};
    use super::*;

    fn text(buf: &Buffer) -> String {
        let area = *buf.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn rows_and_the_picker_draw() {
        let (_model, mut ui) = open_ui();
        ui.engine.tab = EngineTab::Agent(EngineRole::Default);
        let area = Rect::new(0, 0, 80, 30);
        let mut buf = Buffer::empty(area);
        render(&ui, area, &mut buf);
        let frame = text(&buf);
        assert!(frame.contains("default"), "the agent page is lit:\n{frame}");
        assert!(frame.contains("temperature"), "agent rows drawn:\n{frame}");
        assert!(
            frame.contains("(provider default)"),
            "unset renders dimmed:\n{frame}"
        );

        // The picker draws over the rows.
        ui.engine.picker = Some(ModelPicker::default());
        let mut buf = Buffer::empty(area);
        render(&ui, area, &mut buf);
        let frame = text(&buf);
        assert!(
            frame.contains("filter"),
            "picker filter line drawn:\n{frame}"
        );
        assert!(
            frame.contains("claude-fable-5"),
            "allowed models listed:\n{frame}"
        );
    }

    /// SPEC 5's point: the panel draws no frame of its own. The tab strip
    /// above it and the hint row below it already bound the body, and the
    /// bordered block this replaced spent two columns and two rows repeating
    /// that.
    #[test]
    fn the_panel_draws_no_box_around_itself() {
        let (_model, mut ui) = open_ui();
        ui.engine.tab = EngineTab::Global;
        let area = Rect::new(0, 0, 80, 30);
        let mut buf = Buffer::empty(area);
        render(&ui, area, &mut buf);
        let frame = text(&buf);
        for edge in ['┌', '┐', '└', '┘', '│', '─'] {
            assert!(
                !frame.contains(edge),
                "the panel drew a border cell {edge:?}:\n{frame}"
            );
        }
    }

    /// The modified marker rides the strip's right edge — it was the block
    /// title's `· modified` before there was a block to title.
    #[test]
    fn an_edited_working_copy_is_marked_on_the_strip() {
        let (_model, mut ui) = open_ui();
        let area = Rect::new(0, 0, 80, 30);

        let mut buf = Buffer::empty(area);
        render(&ui, area, &mut buf);
        let clean = text(&buf);
        assert!(
            !clean.contains("modified"),
            "in sync says nothing:\n{clean}"
        );

        ui.engine.state.as_mut().unwrap().auto_mode = true;
        let mut buf = Buffer::empty(area);
        render(&ui, area, &mut buf);
        let dirty = text(&buf);
        let strip = dirty.lines().next().unwrap_or_default();
        assert!(strip.trim_end().ends_with("modified"), "{strip:?}");
    }
}
