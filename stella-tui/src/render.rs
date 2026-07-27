//! Pure rendering: `(model, ui) -> frame`. Every panel is drawn by a function
//! that reads only `&SessionModel` / small `Copy` view values, so the whole
//! surface is a deterministic function of the event log plus the ephemeral
//! scroll/compose state (L-T1) — the replay-determinism proptest at the bottom
//! renders two independently-folded models and asserts identical backing cell
//! buffers.
//!
//! # Panel panic boundary (L-T7)
//!
//! Each panel is drawn through `guarded_panel`, which renders it into its
//! **own** throwaway [`Buffer`] inside `catch_unwind`. If a panel panics
//! mid-write, that local buffer is discarded and an error card is drawn in its
//! place; the app keeps running with input alive. This is sound because the
//! draw closures capture only immutable references (`&SessionModel` and
//! `Copy` values — no interior mutability) and the sole mutable state they
//! touch is the freshly-created local buffer, which is thrown away on panic.
//! The frame's real buffer is only ever written by the infallible `blit`
//! *after* the panel has finished, so a half-written panel can never reach the
//! screen. Hence the `AssertUnwindSafe` wrapper is justified rather than
//! papered over.

use std::ops::Range;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use stella_protocol::{CiStatus, FileChangeKind, PrStatus};

use crate::composer::{ComposerLayout, SlashMenu, layout as composer_layout, split_row_at};
use crate::model::{AskUserPrompt, FileState, Hud, InlineDiffRef, SessionModel, TranscriptEntry};
use crate::textline::{
    self, budget_mode_label, ci_status_label, media_kind_label, media_state_label, pr_status_label,
    stage_label,
};
use crate::ui::{PanelFocus, UiState, ViewportMetrics};

mod row;
use crate::{diff, theme};
pub(crate) use row::*;

/// Draw the whole TUI for one frame. Records the panels' viewport sizes back
/// into `ui.metrics` so the pure key handler can clamp scrolling on the next
/// keypress (the only reason this takes `&mut UiState`).
pub fn render(model: &SessionModel, ui: &mut UiState, frame: &mut Frame) {
    let root = frame.area();
    let has_scope = model.pending_scope_review.is_some();
    let has_ask = model.pending_ask_user.is_some();

    // Vertical bands: HUD, main, [scope], [ask], composer. The slash menu is
    // no longer a band — it floats above the composer as a popup.
    let mut constraints = vec![Constraint::Length(3), Constraint::Min(1)];
    if has_scope {
        constraints.push(Constraint::Length(6));
    }
    if let Some(prompt) = model.pending_ask_user.as_ref() {
        // question + one row per option + a free-text hint, within a border.
        constraints.push(Constraint::Length(
            (prompt.options.len() as u16 + 4).min(10),
        ));
    }
    // The composer band grows with its soft-wrapped content (textarea
    // semantics) up to a cap, then scrolls to keep the cursor row visible.
    // Text width: the band spans the root minus 2 border columns and the
    // 2-column `› ` prompt prefix.
    let c_layout = composer_layout(&ui.composer, root.width.saturating_sub(4).max(1) as usize);
    let composer_rows = c_layout.rows.len().clamp(1, COMPOSER_MAX_ROWS) as u16;
    constraints.push(Constraint::Length(composer_rows + 2));
    let bands = Layout::vertical(constraints).split(root);

    let hud_area = bands[0];
    let main_area = bands[1];
    let mut idx = 2;
    let scope_area = if has_scope {
        let a = bands[idx];
        idx += 1;
        Some(a)
    } else {
        None
    };
    let ask_area = if has_ask {
        let a = bands[idx];
        idx += 1;
        Some(a)
    } else {
        None
    };
    let composer_area = bands[idx];

    // HUD.
    guarded_panel(frame, hud_area, "hud", |buf| {
        render_hud(&model.hud, hud_area, buf)
    });

    // Main: transcript (left) + files/diff (right).
    let cols = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(main_area);
    let transcript_area = cols[0];
    let right_area = cols[1];

    let expand_thinking = ui.thinking_expanded;
    let t_width = inner_width(transcript_area);
    ui.ensure_transcript_lines(model, expand_thinking, t_width);
    let t_lines = ui.transcript_lines();
    let t_total = t_lines.len();
    let t_inner_h = inner_height(transcript_area);
    let t_window = ui.scroll.window(t_total, t_inner_h);
    let following = ui.scroll.follow;
    guarded_panel(frame, transcript_area, "transcript", |buf| {
        render_transcript(t_lines, t_window.clone(), following, transcript_area, buf)
    });

    // Right pane: diff viewer when open, else the files-touched panel.
    let (diff_total, diff_inner_h) = if ui.diff_open {
        let file = model.files.get(ui.selected_file);
        let diff_text = file.and_then(|f| f.latest_diff.as_deref());
        let d_lines = diff_text
            .map(|d| diff::body_lines(d, file.map(|f| f.path.as_str())))
            .unwrap_or_default();
        let (added, removed) = diff_text.map(diff::count_diff_lines).unwrap_or((0, 0));
        let d_total = d_lines.len();
        let d_inner_h = inner_height(right_area);
        let d_window = ui.diff_scroll.window(d_total, d_inner_h);
        let title = file
            .map(|f| f.path.clone())
            .unwrap_or_else(|| "diff".to_string());
        guarded_panel(frame, right_area, "diff", |buf| {
            render_diff(
                &d_lines,
                d_window.clone(),
                &title,
                (added, removed),
                right_area,
                buf,
            )
        });
        (d_total, d_inner_h)
    } else {
        let selected = ui.selected_file;
        let focus = ui.focus;
        guarded_panel(frame, right_area, "files", |buf| {
            render_files(&model.files, selected, focus, right_area, buf)
        });
        (0, 0)
    };

    // Scope-review card (when a gate is pending).
    if let (Some(area), Some(proposal)) = (scope_area, model.pending_scope_review.as_ref()) {
        let answered = ui.scope_answered;
        guarded_panel(frame, area, "scope-review", |buf| {
            render_scope_review(proposal, answered, area, buf)
        });
    }

    // Ask-user card (when a question is pending).
    if let (Some(area), Some(prompt)) = (ask_area, model.pending_ask_user.as_ref()) {
        let answered = ui.ask_answered;
        guarded_panel(frame, area, "ask-user", |buf| {
            render_ask_user(prompt, answered, area, buf)
        });
    }

    // Composer.
    let composer_focused = ui.focus == PanelFocus::Composer;
    let composer_blank = ui.composer.is_blank();
    let enter_submits = ui.enter_submits;
    guarded_panel(frame, composer_area, "composer", |buf| {
        render_composer(
            &c_layout,
            composer_blank,
            composer_focused,
            enter_submits,
            composer_area,
            buf,
        )
    });

    // Slash-command popup, floating just above the composer (drawn last
    // so it sits over the transcript, Crush-style, instead of reflowing it).
    let slash = ui.composer.slash_menu(&ui.slash_commands);
    if let Some(menu) = slash.filter(|m| !m.is_empty()) {
        let selected = ui.slash_selected.min(menu.matches.len().saturating_sub(1));
        let area = slash_popup_area(root, composer_area, menu.matches.len());
        guarded_panel(frame, area, "slash-menu", |buf| {
            render_slash_popup(&menu, selected, area, buf)
        });
    }

    // Cache viewport sizes for the next keypress's scroll clamping.
    ui.metrics = ViewportMetrics {
        transcript_height: t_inner_h,
        transcript_total: t_total,
        diff_height: diff_inner_h,
        diff_total,
    };
}

/// The usable interior height of a single-border panel.
pub(crate) fn inner_height(area: Rect) -> usize {
    area.height.saturating_sub(2) as usize
}

/// The usable interior width of a single-border panel.
pub(crate) fn inner_width(area: Rect) -> usize {
    area.width.saturating_sub(2) as usize
}

// Word-aware line wrapping (pre-wrap so scroll math stays line-exact, L-T4)

// Panel panic boundary (L-T7)

/// Render one panel into a throwaway buffer under `catch_unwind`; on panic,
/// substitute a visible error card. See the module docs for the soundness
/// argument behind `AssertUnwindSafe`.
///
/// The [`crate::term::PanelBoundary`] marker tells the panic hook this panic
/// is caught here (in unwind builds), so it must not restore the terminal
/// mid-session; in abort builds the catch is inert and the hook restores
/// unconditionally — the process is about to die either way.
pub(crate) fn guarded_panel<F>(frame: &mut Frame, area: Rect, label: &str, draw: F)
where
    F: Fn(&mut Buffer),
{
    if area.width == 0 || area.height == 0 {
        return;
    }
    let drawn = {
        let _boundary = crate::term::PanelBoundary::enter();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut buf = Buffer::empty(area);
            draw(&mut buf);
            buf
        }))
    };
    let buf = match drawn {
        Ok(buf) => buf,
        Err(payload) => error_card(area, label, &panic_message(&*payload)),
    };
    blit(frame.buffer_mut(), &buf, area);
}

/// Copy every cell of `src` in `area` into `dst`. Infallible — the only write
/// to the real frame buffer, always after a panel has fully drawn or failed.
fn blit(dst: &mut Buffer, src: &Buffer, area: Rect) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = src.cell((x, y)).cloned();
            if let (Some(cell), Some(slot)) = (cell, dst.cell_mut((x, y))) {
                *slot = cell;
            }
        }
    }
}

/// Extract a human message from a panic payload.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panicked with a non-string payload".to_string()
    }
}

/// A visible red error card standing in for a panel that panicked.
fn error_card(area: Rect, label: &str, message: &str) -> Buffer {
    let mut buf = Buffer::empty(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::DANGER))
        .title(format!(" ⚠ panel '{label}' panicked "));
    let body = format!("{message}\n\nthe rest of the TUI is still running");
    Paragraph::new(body)
        .block(block)
        .wrap(Wrap { trim: true })
        .style(Style::new().fg(theme::DANGER).add_modifier(Modifier::BOLD))
        .render(area, &mut buf);
    buf
}

// Panels

/// The session HUD strip: stage, model, and the live cost of the turn.
///
/// Cost here reads *per turn*, matching the composer's cell and the `✓ cost`
/// line — it used to print `hud.spent_usd` raw, which on the deck is the
/// session-cumulative gauge, so the "spend" in this box, the SPEND cell in the
/// statline, and the `◇ spend` rows in the transcript were three different
/// renderings of two different quantities under one word.
pub(crate) fn render_hud(hud: &Hud, area: Rect, buf: &mut Buffer) {
    let label = Style::new().fg(theme::TEXT_TERTIARY);
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled("stage ", label),
        Span::styled(
            hud.stage.map(stage_label).unwrap_or("—").to_string(),
            Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("   model ", label),
        Span::styled(
            hud.model.clone().unwrap_or_else(|| "—".to_string()),
            Style::new().fg(theme::INK),
        ),
        Span::styled("   turn ", label),
        Span::styled(
            textline::fmt_cost(hud.turn_spent_usd()),
            Style::new()
                .fg(spend_color(hud))
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(limit) = hud.limit_usd {
        spans.push(Span::styled(format!(" / ${limit:.2}"), label));
    }
    if let Some(mode) = hud.budget_mode {
        spans.push(Span::styled(
            format!("  ·  {}", budget_mode_label(mode)),
            label,
        ));
    }
    if hud.complete {
        spans.push(Span::styled(
            "   ✓ complete",
            Style::new().fg(theme::OK).add_modifier(Modifier::BOLD),
        ));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(" stella ");
    Paragraph::new(Line::from(spans))
        .block(block)
        .render(area, buf);
}

pub(crate) fn render_transcript(
    lines: &[Line<'static>],
    window: Range<usize>,
    following: bool,
    area: Rect,
    buf: &mut Buffer,
) {
    let visible: Vec<Line<'static>> = lines
        .get(window.clone())
        .map(<[Line]>::to_vec)
        .unwrap_or_default();
    render_transcript_window(visible, window, lines.len(), following, None, area, buf);
}

/// [`render_transcript`] for a caller that already materialized just the
/// visible window (the deck's fold cache clones ≤ one viewport of lines per
/// frame instead of the whole history); `total` sizes the title. `hint`, when
/// set, renders as a dim bottom title — the contextual "what can I press
/// here" line the deck varies with the transcript's interaction state.
pub(crate) fn render_transcript_window(
    visible: Vec<Line<'static>>,
    window: Range<usize>,
    total: usize,
    following: bool,
    hint: Option<&str>,
    area: Rect,
    buf: &mut Buffer,
) {
    let title = if following {
        format!(" transcript · {total} lines · following ")
    } else {
        format!(
            " transcript · {}-{} / {total} ",
            window.start.min(total),
            window.end.min(total)
        )
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(title);
    if let Some(hint) = hint {
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(" {hint} "),
                Style::new().fg(theme::TEXT_TERTIARY),
            ))
            .right_aligned(),
        );
    }
    // No wrap: one logical line per row keeps the scroll math line-exact
    // (L-T4); overflow is clipped horizontally, not reflowed.
    Paragraph::new(Text::from(visible))
        .block(block)
        .render(area, buf);
}

fn render_files(
    files: &[FileState],
    selected: usize,
    focus: PanelFocus,
    area: Rect,
    buf: &mut Buffer,
) {
    let title = format!(" files touched · {} ", files.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(title);
    let lines: Vec<Line<'static>> = if files.is_empty() {
        vec![Line::from(Span::styled(
            "no files touched yet",
            Style::new().fg(theme::TEXT_TERTIARY),
        ))]
    } else {
        files
            .iter()
            .enumerate()
            .map(|(i, f)| file_line(f, i == selected && focus == PanelFocus::Files))
            .collect()
    };
    Paragraph::new(Text::from(lines))
        .block(block)
        .render(area, buf);
}

/// The diff viewer, PR-style: the full file path inline in a rule above the
/// body, the numbered/styled body in the middle, and a closing rule below
/// counting the additions/removals (`crate::diff` owns all three parts, so
/// this pane and the deck's Files tab read identically).
fn render_diff(
    lines: &[Line<'static>],
    window: Range<usize>,
    path: &str,
    (added, removed): (u32, u32),
    area: Rect,
    buf: &mut Buffer,
) {
    if area.height < 2 {
        return;
    }
    let bands = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(area);
    let w = area.width as usize;
    Paragraph::new(diff::header_line(path, w)).render(bands[0], buf);
    let visible: Vec<Line<'static>> = lines.get(window).map(<[Line]>::to_vec).unwrap_or_default();
    Paragraph::new(Text::from(visible)).render(bands[1], buf);
    Paragraph::new(diff::footer_line(added, removed, w)).render(bands[2], buf);
}

pub(crate) fn render_scope_review(
    proposal: &stella_protocol::ScopeProposal,
    answered: bool,
    area: Rect,
    buf: &mut Buffer,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        proposal.summary.clone(),
        Style::new().add_modifier(Modifier::BOLD),
    )));
    let cost = proposal
        .estimated_cost_usd
        .map(|c| format!("  ·  est. cost ~${c:.2}"))
        .unwrap_or_default();
    lines.push(Line::from(Span::styled(
        format!(
            "{} steps  ·  ~{} files{cost}",
            proposal.steps.len(),
            proposal.estimated_files
        ),
        Style::new().fg(theme::TEXT_TERTIARY),
    )));
    lines.push(if answered {
        Line::from(Span::styled(
            "decision sent — awaiting engine…",
            Style::new()
                .fg(theme::TEXT_TERTIARY)
                .add_modifier(Modifier::ITALIC),
        ))
    } else {
        Line::from(vec![
            Span::styled(
                "[a]",
                Style::new().fg(theme::OK).add_modifier(Modifier::BOLD),
            ),
            Span::styled("pprove  ", Style::new().fg(theme::INK)),
            Span::styled(
                "[t]",
                Style::new().fg(theme::WARN).add_modifier(Modifier::BOLD),
            ),
            Span::styled("rim  ", Style::new().fg(theme::INK)),
            Span::styled(
                "[x]",
                Style::new().fg(theme::DANGER).add_modifier(Modifier::BOLD),
            ),
            Span::styled("abort", Style::new().fg(theme::INK)),
        ])
    });
    // Warning, not the accent. A scope gate is the deck waiting on *you*, and
    // "needs input" is a warning everywhere else in this UI — including the
    // `⏸ scope` row this card mirrors in the transcript. Bordering it in the
    // brand hue made the one card that halts the session read as decoration,
    // and it was the single largest block of accent on the frame.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::WARNING_BRIGHT))
        .title(" scope review ");
    Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: true })
        .render(area, buf);
}

pub(crate) fn render_ask_user(
    prompt: &AskUserPrompt,
    answered: bool,
    area: Rect,
    buf: &mut Buffer,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        prompt.question.clone(),
        Style::new().add_modifier(Modifier::BOLD),
    )));
    // The structured options, numbered for quick-pick.
    for (i, option) in prompt.options.iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {}. ", i + 1),
                Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(option.clone(), Style::new().fg(theme::INK)),
        ]));
    }
    // BINDING: always exactly one additional free-text affordance, on every
    // question, whether or not the model listed one.
    lines.push(if answered {
        Line::from(Span::styled(
            "answer sent — awaiting engine…",
            Style::new()
                .fg(theme::TEXT_TERTIARY)
                .add_modifier(Modifier::ITALIC),
        ))
    } else {
        Line::from(Span::styled(
            "  or type your own answer, then Enter",
            Style::new().fg(theme::OK).add_modifier(Modifier::ITALIC),
        ))
    });
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::ACCENT))
        .title(" question ");
    Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: true })
        .render(area, buf);
}

/// Most command rows the slash popup shows at once before it scrolls. The
/// list grows to this, then windows around the selection (see
/// [`scroll_window_start`]) so ↑/↓ can walk a long menu without the highlight
/// ever leaving the frame.
pub(crate) const SLASH_POPUP_MAX_ROWS: usize = 8;

/// Where the slash popup floats: anchored to the composer's left edge,
/// opening upward, tall enough for the matches (capped at
/// [`SLASH_POPUP_MAX_ROWS`]) and clamped to the frame on small terminals. The
/// `+3` reserves the two borders and the one-line key legend.
pub(crate) fn slash_popup_area(root: Rect, composer: Rect, matches: usize) -> Rect {
    let h = ((matches.min(SLASH_POPUP_MAX_ROWS) as u16) + 3).min(root.height);
    let w = root.width.min(56);
    Rect {
        x: composer.x,
        y: composer.y.saturating_sub(h),
        width: w,
        height: h,
    }
}

/// The first visible row of a scrolling list of `len` rows that shows
/// `visible` at a time, chosen so `selected` stays on screen — the window
/// only moves once the selection would fall off an edge. Mirrors the
/// composer's cursor-row windowing ([`render_composer`]) so the slash popup
/// and the textarea scroll with identical feel.
pub(crate) fn scroll_window_start(len: usize, selected: usize, visible: usize) -> usize {
    if visible == 0 || len <= visible {
        return 0;
    }
    let selected = selected.min(len - 1);
    // Keep `selected` inside [first, first + visible); clamp so the last
    // window never shows blank rows past the end.
    (selected + 1).saturating_sub(visible).min(len - visible)
}

/// The floating slash-command menu: an accent-bordered popup with the
/// selected row highlighted and a one-line key legend. Shared by the
/// single-session REPL and the deck (both anchor it above their composer).
///
/// When more commands match than fit, the rows window around `selected` so
/// arrow-key navigation always keeps the highlight visible, and the legend
/// shows how many rows are hidden above (`▲`) / below (`▼`).
pub(crate) fn render_slash_popup(menu: &SlashMenu, selected: usize, area: Rect, buf: &mut Buffer) {
    ratatui::widgets::Clear.render(area, buf);
    let total = menu.matches.len();
    let selected = selected.min(total.saturating_sub(1));
    // The interior minus the legend line is what the command rows scroll in.
    let visible = inner_height(area).saturating_sub(1).max(1);
    let first = scroll_window_start(total, selected, visible);
    let last = (first + visible).min(total);
    let mut lines: Vec<Line<'static>> = menu.matches[first..last]
        .iter()
        .enumerate()
        .map(|(offset, c)| {
            let is_sel = first + offset == selected;
            let marker = if is_sel { "▸ " } else { "  " };
            let mut name_style = theme::accent();
            let mut desc_style = theme::muted();
            if is_sel {
                name_style = name_style.add_modifier(Modifier::REVERSED);
                desc_style = desc_style.add_modifier(Modifier::REVERSED);
            }
            Line::from(vec![
                Span::styled(marker.to_string(), name_style),
                Span::styled(format!("{} ", c.kind.glyph()), name_style),
                Span::styled(c.name.clone(), name_style),
                Span::styled("  ", desc_style),
                Span::styled(c.description.clone(), desc_style),
            ])
        })
        .collect();
    let hidden_above = first;
    let hidden_below = total.saturating_sub(last);
    let legend = if hidden_above > 0 || hidden_below > 0 {
        // Compact when scrolling so the ▲/▼ affordance still fits the width.
        format!(" ↑↓ choose · tab fill · ⏎ run · esc · ▲{hidden_above} ▼{hidden_below}")
    } else {
        " ↑/↓ choose · tab complete · enter run · esc dismiss".to_string()
    };
    lines.push(Line::from(Span::styled(legend, theme::muted())));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .title(format!(" / commands · {total} "));
    Paragraph::new(Text::from(lines))
        .block(block)
        .render(area, buf);
}

/// Cap on the composer's visible content rows: it grows with the prompt up
/// to this, then scrolls to keep the cursor row in view.
pub(crate) const COMPOSER_MAX_ROWS: usize = 8;

/// The multi-line composer panel. Rows come pre-wrapped from
/// [`crate::composer::layout`]; this draws the capped window that keeps the
/// cursor row visible, with a block cursor at the exact cursor column.
fn render_composer(
    layout: &ComposerLayout,
    blank: bool,
    focused: bool,
    enter_submits: bool,
    area: Rect,
    buf: &mut Buffer,
) {
    let accent = Style::new().fg(if focused {
        theme::OK
    } else {
        theme::TEXT_TERTIARY
    });
    let cursor_style = Style::new()
        .fg(theme::OK)
        .add_modifier(Modifier::REVERSED);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if blank {
        // Empty composer: the cursor block plus a key hint matched to the
        // terminal's Enter semantics.
        let hint = if enter_submits {
            "⏎ send · ⌥⏎ newline"
        } else {
            "⏎ send · ⌘⏎ newline · ⌥[ start · ⌥] end"
        };
        let mut spans = vec![Span::styled("› ", accent)];
        if focused {
            spans.push(Span::styled(" ", cursor_style));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(hint, Style::new().fg(theme::TEXT_TERTIARY)));
        lines.push(Line::from(spans));
    } else {
        let visible = inner_height(area).max(1);
        let first = (layout.cursor_row + 1).saturating_sub(visible);
        for (i, row) in layout.rows.iter().enumerate().skip(first).take(visible) {
            // The prompt glyph marks the first row; continuations align.
            let prefix = if i == 0 { "› " } else { "  " };
            let mut spans = vec![Span::styled(prefix, accent)];
            if focused && i == layout.cursor_row {
                let (before, under, after) = split_row_at(row, layout.cursor_col);
                spans.push(Span::styled(before, Style::new().fg(theme::INK)));
                spans.push(Span::styled(
                    under.map(String::from).unwrap_or_else(|| " ".into()),
                    cursor_style,
                ));
                spans.push(Span::styled(after, Style::new().fg(theme::INK)));
            } else {
                spans.push(Span::styled(row.clone(), Style::new().fg(theme::INK)));
            }
            lines.push(Line::from(spans));
        }
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::rule())
        .title(" prompt ");
    Paragraph::new(Text::from(lines))
        .block(block)
        .render(area, buf);
}

// Transcript rail layout
//
// A transcript is a list, and lists are scanned down their left edge. Every
// row therefore opens with a fixed-width *rail* — a glyph in column 0 (or 2
// for subordinate rows) that names the row's kind — so the eye can run
// straight down the margin and land on "what happened" without reading a
// word. The predecessor layout right-aligned a `[name]:` tag into column 22,
// which put the tag's *left* edge at a different column on every row (`[cmd]`
// at 15, `[✓ read_file]` at 7): the index column jittered, so there was
// nothing to scan. It also spent 22 columns — 19% of a 118-column pane — on
// chrome, which the diff bodies then paid for again in clipped code.

/// Most styled diff lines a collapsed tool result shows inline before folding
/// the rest behind ctrl+o — a mutation stays glanceable in the transcript
/// without a large diff flooding it uninvited.
pub(crate) const INLINE_DIFF_CAP: usize = 20;

/// Resolve a tool result's [`InlineDiffRef`] to the diff text it may render,
/// or `None` when the reference went stale: the diff shown must be the one
/// this call produced, so it only resolves while the path's `changes` counter
/// still matches the seq recorded at fold time (a later mutation of the same
/// path bumps it) and the path still carries a diff.
fn resolve_inline_diff<'a>(dref: &InlineDiffRef, files: &'a [FileState]) -> Option<&'a str> {
    files
        .iter()
        .find(|f| f.path == dref.path)
        .and_then(|f| f.diff_at(dref.seq))
}

// Pure content builders (unit-tested directly)

/// The full visual-line list for the transcript. Each entry is rendered with
/// per-entry wrapping so continuation lines respect the label column. An
/// in-flight streaming preview (`SessionModel::streaming_text`) renders as a
/// live trailing agent entry — it is not a transcript entry, so the
/// authoritative `Text` event replaces it without leaving a duplicate row.
pub(crate) fn transcript_lines(
    model: &SessionModel,
    expand_thinking: bool,
    width: usize,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for entry in &model.transcript {
        entry_lines(
            entry,
            &model.files,
            expand_thinking,
            expand_thinking,
            width,
            &mut out,
        );
    }
    if !model.streaming_text.is_empty() {
        let preview = TranscriptEntry::Text(model.streaming_text.clone());
        entry_lines(
            &preview,
            &model.files,
            expand_thinking,
            expand_thinking,
            width,
            &mut out,
        );
    }
    out
}

/// Whether an entry closes a readable block, and so is followed by a spacer.
///
/// Trailing rather than leading, which is what lets the rhythm stay
/// entry-local: a leading gap would have to know what preceded it, and the
/// deck's incremental fold renders each entry in isolation. Two entries are
/// deliberately *not* block-closing — a [`TranscriptEntry::ToolStart`], whose
/// result belongs directly beneath it, and [`TranscriptEntry::Evicted`], the
/// one-line note that opens the scrollback. A consequence worth keeping: a
/// batch of parallel `ToolStart`s renders as a tight block, which is exactly
/// how a fan-out should read.
fn closes_block(entry: &TranscriptEntry) -> bool {
    !matches!(
        entry,
        TranscriptEntry::ToolStart { .. } | TranscriptEntry::Evicted { .. }
    )
}

pub(crate) fn entry_lines(
    entry: &TranscriptEntry,
    files: &[FileState],
    expand_thinking: bool,
    expanded: bool,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    entry_body(entry, files, expand_thinking, expanded, width, out);
    if closes_block(entry) {
        push_gap(out);
    }
}

/// The label style for a system note that wants to be *found*: errors, held
/// scopes, questions, failed verdicts. Bold and hued, because the whole point
/// of the row is that a scan should stop on it.
fn loud(color: Color) -> Style {
    Style::new().fg(color).add_modifier(Modifier::BOLD)
}

/// The label style for a system note that is context rather than event —
/// recall, memory writes, compaction, fallbacks, media, commits.
///
/// These used to be hued too, and collectively they were most of the colour on
/// screen: a transcript where recall, spend and compaction all shout as loudly
/// as an error reads as a list of problems. They are bookkeeping. They get a
/// dim label, and the reader's eye is left free for the rows that matter.
fn quiet() -> Style {
    Style::new().fg(theme::TEXT_TERTIARY)
}

/// The value half of a system note. Always white.
///
/// This is the rule that keeps the deck legible: **the label is coloured, the
/// value is read.** Before the split, `push_note` was routinely handed one
/// style for both halves, so every note row was a single saturated hue end to
/// end and the accent stopped meaning anything. A colour earns its place by
/// being rare.
fn value() -> Style {
    Style::new().fg(theme::INK)
}

/// `1 frame` / `3 frames` — a count that reads as English. The transcript used
/// to render "1 frames".
fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

fn entry_body(
    entry: &TranscriptEntry,
    files: &[FileState],
    expand_thinking: bool,
    expanded: bool,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    match entry {
        TranscriptEntry::Evicted { count } => out.push(Line::from(Span::styled(
            format!("… {count} earlier entries evicted"),
            Style::new()
                .fg(theme::TEXT_TERTIARY)
                .add_modifier(Modifier::ITALIC),
        ))),
        TranscriptEntry::User(text) => {
            // The one transcript entry rendered in a single color end to end:
            // the `[user]:` tag and every line of the prompt ride the same
            // violet as the composer's keybind glyphs and the
            // "deterministic-first" chip (`deck_render`) — the interactive-
            // chrome accent, never the brand gold. Rendered as plain lines
            // (not markdown) so nothing tints part of the prompt a 2nd color.
            let violet = Style::new().fg(theme::VIOLET);
            let lines: Vec<Line<'static>> = text
                .split('\n')
                .map(|l| Line::from(Span::styled(l.to_owned(), violet)))
                .collect();
            push_row_block(Rail::User, lines, width, out);
        }
        TranscriptEntry::Stage(name) => {
            // A section rule, not a row — see `push_rule`. The word "stage" is
            // dropped with it: the label *is* the stage, and prefixing every
            // one with its own type name was three columns spent restating
            // what the divider already says.
            push_rule(
                stage_label(*name),
                Style::new()
                    .fg(theme::TEXT_SECONDARY)
                    .add_modifier(Modifier::BOLD),
                width,
                out,
            );
        }
        TranscriptEntry::Text(text) => {
            push_row_block(Rail::Agent, crate::markdown::render(text), width, out);
        }
        TranscriptEntry::Reasoning(text) => {
            let total_lines = text.lines().count().max(1);
            let show_all = expand_thinking || expanded;
            let chevron = if show_all { "⏶" } else { "⏵" };
            // Dim, not tinted. Reasoning is the agent talking to itself; it is
            // the *least* load-bearing text on screen, and the glacier blue it
            // used to wear now reads as the brand accent.
            let header_style = quiet();
            let reasoning_style = Style::new()
                .fg(theme::TEXT_TERTIARY)
                .add_modifier(Modifier::ITALIC);
            let mut block = vec![Line::from(Span::styled(
                format!("{total_lines} lines"),
                header_style,
            ))];
            if show_all {
                for l in text.split('\n') {
                    block.push(Line::from(Span::styled(l.to_owned(), reasoning_style)));
                }
            } else {
                let preview_count = 3;
                let mut shown = 0;
                for l in text.lines() {
                    if shown >= preview_count {
                        break;
                    }
                    if !l.trim().is_empty() {
                        block.push(Line::from(Span::styled(l.to_owned(), reasoning_style)));
                        shown += 1;
                    }
                }
                if total_lines > preview_count {
                    block.push(Line::from(Span::styled(
                        "⋯ ctrl+o expands this thought · ctrl+r all",
                        Style::new().fg(theme::TEXT_TERTIARY),
                    )));
                }
            }
            push_note_block(
                &format!("{chevron} thinking"),
                header_style,
                block,
                width,
                out,
            );
        }
        TranscriptEntry::ToolStart {
            name,
            input,
            raw,
            path,
            ..
        } => {
            // `name` then `argument`, the name soft-padded to a common column
            // so arguments line up across a run of calls. Soft, not hard: a
            // long MCP name (`mcp__github__create_pull_request`) overruns the
            // column rather than being truncated, since the tool's identity
            // outranks the alignment it would cost.
            // The tool name is the one thing in the transcript that carries
            // the full brand accent. Everything a session did, it did through a
            // tool call, so the names are the index to the whole scrollback —
            // and they are the only rows a reader scans *for* rather than
            // reads. The argument beside it stays white/dim (`path_spans`), so
            // the accent marks the verb and never the object.
            let mut left = vec![Span::styled(
                pad_name(name),
                Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
            )];
            left.extend(path_spans(input, path.is_some()));
            push_row(Rail::Call, left, width, out);
            if expanded {
                // ctrl+o: the full argument object, pretty-printed and dim.
                // An over-budget argument may not parse (char-capped raw) —
                // show it wrapped rather than clipped at the pane edge.
                let pretty = serde_json::from_str::<serde_json::Value>(raw)
                    .and_then(|v| serde_json::to_string_pretty(&v))
                    .unwrap_or_else(|_| raw.clone());
                for l in pretty.lines() {
                    push_detail_line(l, width, out);
                }
            }
        }
        TranscriptEntry::ToolResult {
            ok,
            full,
            duration_ms,
            speculated,
            diff,
            ..
        } => {
            let rail = if *ok { Rail::Result } else { Rail::Fail };
            let dim = Style::new().fg(theme::MUTED);
            let total = full.lines().count();
            // ⚡ marks a speculated result: the duration overlapped the
            // model's own streaming instead of following it.
            let dur = if *speculated {
                format!("⚡{}", human_duration(*duration_ms))
            } else {
                human_duration(*duration_ms)
            };
            let inline = diff.as_ref().and_then(|d| resolve_inline_diff(d, files));

            // The right-hand metric column. A diff states its own size in
            // added/removed lines, which is the honest unit for an edit —
            // "42 lines of output" would describe the tool's chatter, not the
            // change. Everything else reports output size, and only when
            // there is more than the one line already shown.
            let mut metric: Vec<Span<'static>> = Vec::new();
            if let Some(d) = inline {
                let (added, removed) = diff::count_diff_lines(d);
                metric.push(Span::styled(
                    format!("+{added}"),
                    Style::new().fg(theme::OK),
                ));
                metric.push(Span::styled(" ".to_string(), dim));
                metric.push(Span::styled(
                    format!("−{removed}"),
                    Style::new().fg(theme::BAD),
                ));
                metric.push(Span::styled(" · ".to_string(), dim));
            } else if total > 1 && !expanded {
                // `⋯` is the one glyph this UI uses for "there is more behind
                // this", so it carries the ctrl+o affordance the removed hint
                // row used to spell out — at no extra row.
                metric.push(Span::styled(format!("⋯ {} · ", plural_lines(total)), dim));
            }
            metric.push(Span::styled(dur, dim));

            if expanded {
                push_row(
                    rail,
                    justify(vec![], metric, width, rail.indent()),
                    width,
                    out,
                );
                for l in full.lines() {
                    push_detail_line(l, width, out);
                }
            } else {
                // A failure never collapses to a single line. The whole point
                // of reading a transcript at the moment something breaks is to
                // see *why*, and a one-line preview of a stack trace is a
                // prompt to go hunting rather than an answer.
                // With a diff below, a prose summary ("Applied edit to
                // src/agent.rs") would restate the call row above it and the
                // diff under it in the same breath. The row carries only its
                // metrics and gets out of the way.
                let shown: Vec<&str> = if inline.is_some() {
                    Vec::new()
                } else {
                    // A failure never collapses to a single line. The point of
                    // reading a transcript at the moment something breaks is to
                    // see *why*, and a one-line preview of a stack trace is a
                    // prompt to go hunting rather than an answer.
                    let budget = if *ok { 1 } else { FAIL_PREVIEW };
                    full.lines().skip(salient_line(full)).take(budget).collect()
                };
                let head: Vec<Span<'static>> = match shown.first() {
                    Some(l) => vec![Span::styled(
                        l.trim_end().to_owned(),
                        if *ok {
                            dim
                        } else {
                            Style::new().fg(theme::BAD)
                        },
                    )],
                    None => Vec::new(),
                };
                push_row(
                    rail,
                    justify(head, metric, width, rail.indent()),
                    width,
                    out,
                );
                for l in shown.iter().skip(1) {
                    push_detail_line(l.trim_end(), width, out);
                }
                // Only a failure earns the "there is more" row: a successful
                // result already states its size in the metric column, and
                // saying it twice is how a dense layout turns back into a
                // sparse one. Anchoring mid-output also means the count is
                // "everything but the window", not "everything after it".
                let hidden = total.saturating_sub(shown.len());
                if hidden > 0 && !*ok {
                    push_detail_line(&format!("⋯ {} · ctrl+o", plural_lines(hidden)), width, out);
                }
            }
            // The mutation's diff, inline under the result — GitHub-PR style
            // via `crate::diff` (the one implementation of "how a diff
            // looks"), gated on freshness: a later mutation of the same path
            // bumps `FileState::changes` past the recorded seq and the diff
            // no longer belongs to this call, so it is hidden rather than
            // misattributed. Collapsed shows at most [`INLINE_DIFF_CAP`]
            // styled lines; ctrl+o reveals the whole diff.
            if let (Some(dref), Some(d)) = (diff.as_ref(), inline) {
                // No path header and no counts footer here, unlike the
                // standalone viewer: the call row above already names the file
                // and the metric column already states `+n −m`, so both rules
                // would be the same facts a second time — four rows of chrome
                // around what is often a two-row change.
                let cap = if expanded {
                    usize::MAX
                } else {
                    INLINE_DIFF_CAP
                };
                let (body, hidden) = diff::body_lines_inline(d, Some(&dref.path), cap);
                for line in body {
                    push_diff_line(line, out);
                }
                if hidden > 0 {
                    push_diff_line(
                        Line::from(Span::styled(
                            format!("⋯ {} · ctrl+o", plural_lines(hidden)),
                            Style::new().fg(theme::MUTED),
                        )),
                        out,
                    );
                }
            }
        }
        TranscriptEntry::Retry { attempt, reason } => {
            push_note(
                "↻ retry",
                loud(theme::WARNING_BRIGHT),
                vec![
                    Span::styled(format!("#{attempt} "), quiet()),
                    Span::styled(reason.clone(), value()),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::Compaction {
            before_tokens,
            after_tokens,
            evicted,
            deduped,
        } => {
            push_note(
                "⇣ compacted",
                quiet(),
                vec![
                    Span::styled(format!("{before_tokens}→{after_tokens} tok"), value()),
                    Span::styled(
                        format!("  ·  {evicted} evicted · {deduped} deduped"),
                        quiet(),
                    ),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::BudgetTick {
            spent_usd,
            limit_usd,
            mode,
        } => {
            let limit = limit_usd.map(|l| format!("/${l:.2}")).unwrap_or_default();
            let style = Style::new().fg(theme::WARNING);
            push_note(
                "◇ spend",
                style,
                vec![Span::styled(
                    format!("${spent_usd:.4}{limit} ({})", budget_mode_label(*mode)),
                    style,
                )],
                width,
                out,
            );
        }
        TranscriptEntry::ProviderFallback { from, to, reason } => {
            push_note(
                "⚡ fallback",
                loud(theme::WARNING),
                vec![
                    Span::styled(format!("{from} → {to}"), value()),
                    Span::styled(format!("  ·  {reason}"), quiet()),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::ContextRecall {
            frames,
            tokens,
            labels,
        } => {
            let cited = labels.join(", ");
            push_note(
                "◉ recalled",
                quiet(),
                vec![
                    Span::styled(
                        format!("{} · {tokens} tok", plural(*frames as u64, "frame", "frames")),
                        value(),
                    ),
                    Span::styled(format!("  ·  {cited}"), quiet()),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::ContextWrite {
            provider,
            upserts,
            superseded,
        } => {
            push_note(
                "✎ memory",
                quiet(),
                vec![
                    Span::styled(plural(u64::from(*upserts), "fact", "facts"), value()),
                    Span::styled(
                        format!("  ·  {superseded} superseded → {provider}"),
                        quiet(),
                    ),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::MediaProgress {
            artifact_id,
            kind,
            state,
        } => {
            push_note(
                "🎞 media",
                quiet(),
                vec![
                    Span::styled(
                        format!("{} {}", media_kind_label(*kind), media_state_label(state)),
                        value(),
                    ),
                    Span::styled(format!("  ·  {artifact_id}"), quiet()),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::MediaComplete { label, path, kind } => {
            push_note(
                "🎨 media",
                quiet(),
                vec![
                    Span::styled(format!("{} {label}", media_kind_label(*kind)), value()),
                    Span::styled(format!("  ·  {path}"), quiet()),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::JudgeVerdict {
            passed,
            summary,
            deterministic,
        } => {
            // Passing is [`theme::OK`], not the accent: a verdict is an
            // outcome, and outcomes are status-coloured. The accent means
            // "active", which a settled verdict by definition is not.
            let (glyph, color) = if *passed {
                ("✓", theme::OK)
            } else {
                ("✗", theme::DANGER)
            };
            let tag = if *deterministic {
                "deterministic"
            } else {
                "model-judge"
            };
            push_note(
                &format!("{glyph} verdict"),
                loud(color),
                vec![
                    Span::styled(summary.clone(), value()),
                    Span::styled(format!("  ·  {tag}"), quiet()),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::GoalVerdict {
            met,
            round,
            reasoning,
        } => {
            let (glyph, color) = if *met {
                ("✓", theme::OK)
            } else {
                ("○", theme::WARN)
            };
            push_note(
                &format!("{glyph} goal"),
                loud(color),
                vec![
                    Span::styled(
                        if *met { "met" } else { "not yet met" }.to_string(),
                        loud(color),
                    ),
                    Span::styled(format!("  {reasoning}"), value()),
                    Span::styled(format!("  ·  round {round}"), quiet()),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::ScopeReview {
            summary,
            steps,
            estimated_files,
        } => {
            push_note(
                "⏸ scope",
                loud(theme::WARNING_BRIGHT),
                vec![
                    Span::styled(summary.clone(), value()),
                    Span::styled(
                        format!(
                            "  ·  {} · ~{}",
                            plural(*steps as u64, "step", "steps"),
                            plural(u64::from(*estimated_files), "file", "files")
                        ),
                        quiet(),
                    ),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::AskUser { question, options } => {
            push_note(
                "? ask",
                loud(theme::WARNING_BRIGHT),
                vec![
                    Span::styled(question.clone(), value()),
                    Span::styled(
                        format!(
                            "  ·  {} + free text",
                            plural(*options as u64, "option", "options")
                        ),
                        quiet(),
                    ),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::Commit { sha, message } => {
            let short = sha.chars().take(9).collect::<String>();
            push_note(
                "● commit",
                quiet(),
                vec![
                    Span::styled(format!("{short}  "), quiet()),
                    Span::styled(message.clone(), value()),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::Pr {
            url,
            status,
            number,
            ci,
        } => {
            let style = Style::new()
                .fg(pr_status_color(*status))
                .add_modifier(Modifier::BOLD);
            let mut spans = vec![Span::styled(
                format!("[{}] ", pr_status_label(*status)),
                style,
            )];
            if let Some(n) = number {
                spans.push(Span::styled(format!("#{n} "), style));
            }
            if let Some(ci) = ci {
                spans.push(Span::styled(
                    format!("ci {} ", ci_status_label(*ci)),
                    Style::new().fg(ci_status_color(*ci)),
                ));
            }
            spans.push(Span::styled(url.clone(), Style::new().fg(theme::TEXT_TERTIARY)));
            push_note("⇢ pr", style, spans, width, out);
        }
        TranscriptEntry::TaskUpdate {
            done,
            total,
            active,
        } => {
            let mut spans = vec![Span::styled(format!("{done}/{total}"), value())];
            if let Some(subject) = active {
                spans.push(Span::styled(format!("  ·  {subject}"), quiet()));
            }
            push_note("☰ tasks", loud(theme::VIOLET), spans, width, out);
        }
        TranscriptEntry::Error { message, retryable } => {
            push_note(
                "✗ error",
                loud(theme::DANGER),
                vec![
                    Span::styled(message.clone(), Style::new().fg(theme::DANGER)),
                    Span::styled(
                        if *retryable { "  ·  retryable" } else { "" }.to_string(),
                        quiet(),
                    ),
                ],
                width,
                out,
            );
        }
        TranscriptEntry::Complete { model, cost_usd } => {
            // The turn's receipt, and now the *only* spend line in the
            // transcript — the per-call `BudgetTick` rows that used to print
            // four or five running subtotals per turn are gauge-only (see
            // `SessionModel::apply`). Because it is the one line, it can afford
            // to be the definite one: green for a settled amount, and the model
            // that actually answered spelled out beside it rather than left to
            // the statline.
            push_note(
                "✓ cost",
                loud(theme::SUCCESS_BRIGHT),
                vec![
                    Span::styled(
                        textline::fmt_cost(*cost_usd),
                        Style::new()
                            .fg(theme::SUCCESS_BRIGHT)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  ·  {model}"), quiet()),
                ],
                width,
                out,
            );
        }
    }
}

fn pr_status_color(status: PrStatus) -> Color {
    // A ramp toward the brand accent as the PR matures, so the `[⇢ pr]:`
    // gutter reads with the rest of the transcript: warning-orange draft, deep
    // gold while open, full gold on merge, danger on close. (The "ember"
    // family this comment used to name was retired with the aurora→gold
    // recolour — see `theme`'s palette-law test.)
    match status {
        PrStatus::Draft => theme::WARNING,
        PrStatus::Open => theme::ACCENT_DEEP,
        PrStatus::Merged => theme::ACCENT,
        PrStatus::Closed => theme::DANGER,
    }
}

fn ci_status_color(status: CiStatus) -> Color {
    match status {
        CiStatus::Pending => theme::TEXT_TERTIARY,
        CiStatus::Running => theme::WARNING_BRIGHT,
        CiStatus::Passing => theme::OK,
        CiStatus::Failing => theme::BAD,
    }
}

fn file_line(file: &FileState, selected: bool) -> Line<'static> {
    let (marker, color) = match file.kind {
        FileChangeKind::Read => ("[r]", theme::TEXT_TERTIARY),
        FileChangeKind::Created => ("[+]", theme::OK),
        FileChangeKind::Modified => ("[~]", theme::WARN),
        FileChangeKind::Deleted => ("[-]", theme::BAD),
    };
    let mut count = if file.changes > 1 {
        format!(" ({})", file.changes)
    } else {
        String::new()
    };
    if file.reads > 0 {
        count.push_str(&format!(" ·r{}", file.reads));
    }
    let mut style = Style::new().fg(color);
    if selected {
        style = style.add_modifier(Modifier::REVERSED);
    }
    Line::from(vec![
        Span::styled(format!("{marker} "), Style::new().fg(color)),
        Span::styled(format!("{}{count}", file.path), style),
    ])
}

// Labels — wording in `crate::textline`; only palette mapping lives here

/// Cost tone: green until the turn approaches its budget, then warning, then
/// danger. Compared against the *turn* figure, because `limit_usd` is the
/// guard's per-turn limit — matching it against the session-cumulative gauge
/// (as this did) meant the tone flipped to red partway through a session and
/// stayed there regardless of what the turn in flight actually cost.
fn spend_color(hud: &Hud) -> Color {
    let spent = hud.turn_spent_usd();
    match hud.limit_usd {
        Some(limit) if limit > 0.0 && spent >= limit => theme::BAD,
        Some(limit) if limit > 0.0 && spent >= limit * 0.8 => theme::WARN,
        _ => theme::OK,
    }
}

#[cfg(test)]
// Test fixtures build a default `UiState` and then poke one or two fields to
// set up a scenario; struct-update syntax for each would only obscure intent.
#[allow(clippy::field_reassign_with_default)]
mod tests;
