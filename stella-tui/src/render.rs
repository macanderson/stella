//! Pure rendering: `(model, ui) -> frame`. Every panel is drawn by a function
//! that reads only `&SessionModel` / small `Copy` view values, so the whole
//! surface is a deterministic function of the event log plus the ephemeral
//! scroll/compose state (L-T1) — the replay-determinism proptest at the bottom
//! renders two independently-folded models and asserts identical backing cell
//! buffers.
//!
//! # Panel panic boundary (L-T7)
//!
//! Each panel is drawn through `guarded_panel`, a thin `Frame`-shaped
//! wrapper over `panel_guard::guarded_band` — the crate's single
//! boundary, shared with the deck. The panel renders into its own scratch
//! [`Buffer`]; a panic mid-write discards it and paints an error card in its
//! place, and the app keeps running with input alive.
//!
//! On *this* path the `AssertUnwindSafe` needs no recoverability argument at
//! all: the draw closures below capture only immutable references
//! (`&SessionModel` and `Copy` values — no interior mutability) and the sole
//! mutable state they touch is the scratch buffer, which is thrown away on
//! panic. `ui.metrics` is written by `render` itself, outside every guard. The
//! deck's closures do capture `&mut DeckUi`, and the argument for those lives
//! with the boundary in `panel_guard`.

use std::ops::Range;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use stella_protocol::FileChangeKind;

use crate::composer::{ComposerLayout, SlashMenu, layout as composer_layout, split_row_at};
use crate::model::{AskUserPrompt, FileState, Hud, InlineDiffRef, SessionModel};
use crate::textline::{self, budget_mode_label, stage_label};
use crate::ui::{PanelFocus, UiState, ViewportMetrics};

mod entry;
// `pub(crate)` for `wrap_one_indent` alone: the startup-notice dialog
// (`crate::notice`) wraps its detail clauses with the same hanging indent the
// transcript uses, rather than growing a second wrapper beside it.
pub(crate) mod row;
use crate::{diff, theme};
// The transcript content builders moved to `entry` when this file crossed the
// 1500-line guard; re-exported so `crate::render::transcript_lines` and
// `::entry_lines` still resolve for `ui.rs` and `deck_ui.rs`.
pub(crate) use entry::{entry_lines, reasoning_is_live, streaming_lines, transcript_lines};
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

    // Main: transcript (left) + files/diff (right) — stacked instead of split
    // when a screen reader is listening, because a reader walks the grid row
    // by row and a two-column row is two unrelated half-sentences spoken as
    // one (see `UiState::screen_reader`). The panels, their contents, and
    // every key that drives them are identical either way; only the axis
    // changes, so `Tab`-to-files and the diff viewer keep working.
    let cols = if ui.screen_reader {
        Layout::vertical([Constraint::Percentage(70), Constraint::Percentage(30)]).split(main_area)
    } else {
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(main_area)
    };
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
        // The file's measured delta, not a recount of the rendered hunk.
        let (added, removed) = file.map(|f| (f.added, f.removed)).unwrap_or((0, 0));
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
        let evicted = model.files_evicted;
        guarded_panel(frame, right_area, "files", |buf| {
            render_files(&model.files, evicted, selected, focus, right_area, buf)
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
    let mut slash_open = false;
    if let Some(menu) = slash.filter(|m| !m.is_empty()) {
        slash_open = true;
        let selected = ui.slash_selected.min(menu.matches.len().saturating_sub(1));
        let area = slash_popup_area(root, composer_area, menu.matches.len());
        guarded_panel(frame, area, "slash-menu", |buf| {
            render_slash_popup(&menu, selected, area, buf)
        });
    }

    // Put the *hardware* cursor where the caret is (#935). ratatui hides the
    // terminal cursor on any frame that never positions it, and until this
    // surface had a caller the fix landed on the deck alone — so the caret
    // here was a reversed cell and nothing more. A reversed cell is invisible
    // to everything that reads a terminal programmatically: screen readers
    // have no insertion point to follow, and a CJK/IME candidate window
    // anchors to the terminal cursor, so composition appeared in the wrong
    // place or not at all.
    //
    // Suppressed while the slash popup owns the keyboard, mirroring the
    // precedence `handle_key` already applies — the caret must not sit in the
    // composer while keys are steering a menu. The scope and ask-user cards
    // are deliberately NOT suppressors: both are answered *by typing into the
    // composer* (see `render_scope_review`), so the composer still holds the
    // insertion point while they are up.
    if !slash_open
        && ui.focus == PanelFocus::Composer
        && let Some((x, y)) = composer_cursor_position(&c_layout, composer_area)
    {
        frame.set_cursor_position((x, y));
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

/// Render one panel of the single-session shell under the crate's panic
/// boundary; on panic, substitute a visible error card. See
/// `panel_guard` for the mechanism and the `AssertUnwindSafe`
/// argument, and this module's docs for why that argument is trivial here.
pub(crate) fn guarded_panel<F>(frame: &mut Frame, area: Rect, label: &str, draw: F)
where
    F: FnOnce(&mut Buffer),
{
    crate::panel_guard::guarded_band(frame.buffer_mut(), area, label, draw);
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
    evicted: u32,
    selected: usize,
    focus: PanelFocus,
    area: Rect,
    buf: &mut Buffer,
) {
    // The evicted count keeps a capped ledger honest: `MAX_TRACKED_FILES`
    // eviction must never read as "only this many files were touched".
    let title = if evicted > 0 {
        format!(" files touched · {} (+{evicted} evicted) ", files.len())
    } else {
        format!(" files touched · {} ", files.len())
    };
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
        // Every answer is typed and sent, so the legend reads as what to type
        // rather than as keys that fire on their own — `[a]` looked like a
        // one-press command, which is exactly the reading that made a note
        // opening "also…" approve an eight-step plan. The bracket styling stays
        // for scannability; the trailing "then ⏎" is the whole contract.
        //
        // The typed path is named here because it is now the only way to say
        // "not like that — do this", and an affordance nobody is told about is
        // one the next reviewer discovers by having their words routed
        // somewhere they did not expect.
        Line::from(vec![
            Span::styled("type ", Style::new().fg(theme::TEXT_TERTIARY)),
            Span::styled("a", Style::new().fg(theme::OK).add_modifier(Modifier::BOLD)),
            Span::styled("pprove  ", Style::new().fg(theme::INK)),
            Span::styled(
                "t",
                Style::new().fg(theme::WARN).add_modifier(Modifier::BOLD),
            ),
            Span::styled("rim  ", Style::new().fg(theme::INK)),
            Span::styled(
                "x",
                Style::new().fg(theme::DANGER).add_modifier(Modifier::BOLD),
            ),
            Span::styled("abort", Style::new().fg(theme::INK)),
            Span::styled(
                "  ·  or what to change  —  then ⏎",
                Style::new().fg(theme::TEXT_TERTIARY),
            ),
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

/// Columns the `› ` prompt glyph (and the matching continuation indent)
/// occupies before the composer's own text starts.
const PROMPT_PREFIX_W: usize = 2;

/// The first visible composer row for a viewport of `visible` rows. Factored
/// out of [`render_composer`] so [`composer_cursor_position`] windows the
/// buffer exactly the way the draw does, rather than by a second copy of the
/// arithmetic that could drift from it.
fn composer_scroll_first(cursor_row: usize, visible: usize) -> usize {
    (cursor_row + 1).saturating_sub(visible)
}

/// The absolute terminal cell of the composer caret, or `None` when the
/// composer band is too small to have drawn one.
///
/// `area` is the bordered band, so the interior — where [`render_composer`]
/// writes — starts one row down and one column in, after which the `› `
/// prefix takes [`PROMPT_PREFIX_W`] more columns. The blank-composer branch
/// draws its cursor block at exactly that first interior text cell, and
/// `ComposerLayout` reports `(0, 0)` for an empty buffer, so one formula
/// covers both branches.
pub(crate) fn composer_cursor_position(layout: &ComposerLayout, area: Rect) -> Option<(u16, u16)> {
    // Under 3 rows / 3 columns the border leaves no interior and nothing was
    // drawn — there is no caret cell to point at.
    if area.width < 3 || area.height < 3 {
        return None;
    }
    let visible = inner_height(area).max(1);
    let first = composer_scroll_first(layout.cursor_row, visible);
    let row_in_view = layout.cursor_row.checked_sub(first)?;
    let y = area
        .y
        .checked_add(1)?
        .checked_add(u16::try_from(row_in_view).ok()?)?;
    let x = area
        .x
        .checked_add(1)?
        .checked_add(u16::try_from(PROMPT_PREFIX_W + layout.cursor_col).ok()?)?;
    // A caret scrolled past the band's right edge has no cell of its own; the
    // terminal would clamp it onto a neighbouring panel's column, which is a
    // worse lie than leaving the cursor where it was.
    (x < area.right() && y < area.bottom()).then_some((x, y))
}

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
    let cursor_style = Style::new().fg(theme::OK).add_modifier(Modifier::REVERSED);
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
        let first = composer_scroll_first(layout.cursor_row, visible);
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

/// That inline diff's measured `(added, removed)`, from the emitter — the
/// companion to [`resolve_inline_diff`], so a transcript row states the size of
/// the change rather than the size of its rendering.
pub(crate) fn resolve_inline_delta(
    dref: &InlineDiffRef,
    files: &[FileState],
) -> Option<(u32, u32)> {
    files
        .iter()
        .find(|f| f.path == dref.path)
        .and_then(|f| f.delta_at(dref.seq))
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
