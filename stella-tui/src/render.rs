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

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use crate::composer::SlashMenu;
use crate::model::{AskUserPrompt, FileState, Hud, InlineDiffRef};
use crate::textline::{self, budget_mode_label, stage_label};

mod entry;
// `pub(crate)` for `wrap_one_indent` alone: the startup-notice dialog
// (`crate::notice`) wraps its detail clauses with the same hanging indent the
// transcript uses, rather than growing a second wrapper beside it.
pub(crate) mod row;
use crate::theme;
// The transcript content builders moved to `entry` when this file crossed the
// 1500-line guard; re-exported so `crate::render::transcript_lines` and
// `::entry_lines` still resolve for `ui.rs` and `deck_ui.rs`.
pub(crate) use entry::{entry_lines, reasoning_is_live, streaming_lines};
pub(crate) use row::*;

/// The usable interior height of a single-border panel.
pub(crate) fn inner_height(area: Rect) -> usize {
    area.height.saturating_sub(2) as usize
}

/// The usable interior width of a single-border panel.
pub(crate) fn inner_width(area: Rect) -> usize {
    area.width.saturating_sub(2) as usize
}

// Word-aware line wrapping (pre-wrap so scroll math stays line-exact, L-T4)

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

/// Rows one hunk claims on the card: its header line plus a capped slice of
/// its diff body. Capped because a card that scrolls off the frame hides the
/// hunk a reviewer is about to accept, and the Files tab is where a full diff
/// is read — this band exists to decide, not to browse.
pub(crate) const HUNK_CARD_DIFF_ROWS: usize = 6;

/// How tall the hunk-review band needs to be: one row per hunk header, up to
/// [`HUNK_CARD_DIFF_ROWS`] of body for the hunk under the cursor, plus borders,
/// title and the footer legend. Capped so a fifty-hunk proposal cannot eat the
/// transcript.
#[must_use]
pub(crate) fn hunk_review_height(hunks: usize) -> u16 {
    let rows = hunks + HUNK_CARD_DIFF_ROWS + 4;
    (rows as u16).min(20)
}

/// The per-hunk approval card (#1265).
///
/// One row per hunk — number, mark, path, `@@` position, counts — with the
/// diff of the hunk under the cursor expanded beneath. The footer always names
/// how many hunks `⏎` would apply, which is the safeguard that lets the card
/// arrive pre-marked accepted without becoming a gate that answers itself.
pub(crate) fn render_hunk_review(
    proposal: &stella_protocol::HunkProposal,
    marks: Option<&crate::deck_ui::HunkMarks>,
    answered: bool,
    area: Rect,
    buf: &mut Buffer,
) {
    let cursor = marks.map_or(0, |m| m.cursor);
    let accepted = |i: usize| marks.is_none_or(|m| m.accepted.get(i).copied().unwrap_or(false));

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, hunk) in proposal.hunks.iter().enumerate() {
        let keep = accepted(i);
        let (glyph, tone) = if keep {
            ("✓", theme::OK)
        } else {
            ("✗", theme::DANGER)
        };
        let position = hunk.diff.lines().next().unwrap_or("@@").trim().to_string();
        lines.push(Line::from(vec![
            Span::styled(
                if i == cursor { "▸ " } else { "  " }.to_string(),
                Style::new().fg(theme::ACCENT),
            ),
            Span::styled(
                format!("{}. ", i + 1),
                Style::new().fg(theme::TEXT_TERTIARY),
            ),
            Span::styled(
                format!("{glyph} "),
                Style::new().fg(tone).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                hunk.path.clone(),
                if keep {
                    Style::new().fg(theme::INK)
                } else {
                    // A declined hunk stays legible but visibly out of the set —
                    // dimming is the whole signal that ⏎ will not write it.
                    Style::new().fg(theme::TEXT_TERTIARY)
                },
            ),
            Span::styled(
                format!(
                    "  {position}  +{} −{}",
                    hunk.lines_added, hunk.lines_removed
                ),
                Style::new().fg(theme::TEXT_TERTIARY),
            ),
        ]));
    }
    // The hunk under the cursor, rendered through the one shared diff body so
    // this surface looks like every other diff in the deck.
    if let Some(hunk) = proposal.hunks.get(cursor) {
        lines.extend(
            crate::diff::body_lines(&hunk.diff, Some(&hunk.path))
                .into_iter()
                .take(HUNK_CARD_DIFF_ROWS),
        );
    }
    let keeping = (0..proposal.hunks.len()).filter(|i| accepted(*i)).count();
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
                "↑↓",
                Style::new().fg(theme::INK).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" move  ", Style::new().fg(theme::TEXT_TERTIARY)),
            Span::styled(
                "space",
                Style::new().fg(theme::INK).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" toggle  ", Style::new().fg(theme::TEXT_TERTIARY)),
            Span::styled(
                "esc",
                Style::new().fg(theme::DANGER).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" decline all  ·  ", Style::new().fg(theme::TEXT_TERTIARY)),
            // The count is the safeguard: the reviewer is never asked to press
            // ⏎ without being told exactly what it writes.
            Span::styled(
                format!("⏎ apply {keeping} of {}", proposal.hunks.len()),
                Style::new()
                    .fg(if keeping == 0 {
                        theme::DANGER
                    } else {
                        theme::OK
                    })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  ·  or type 1 3, 2-4, all, none",
                Style::new().fg(theme::TEXT_TERTIARY),
            ),
        ])
    });
    // Warning-bordered like the scope card, and for the same reason: this is
    // the deck waiting on *you*, and it is the gate that decides whether bytes
    // land in the user's files.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::WARNING_BRIGHT))
        .title(format!(" review {} ", proposal.tool));
    Paragraph::new(Text::from(lines))
        .block(block)
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
/// `+2` reserves the two border rows — the key hints ride the top border and
/// the scroll affordance the bottom one, so no interior row is chrome.
pub(crate) fn slash_popup_area(root: Rect, composer: Rect, matches: usize) -> Rect {
    let h = ((matches.min(SLASH_POPUP_MAX_ROWS) as u16) + 2).min(root.height);
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

/// The floating slash-command menu: each row is `/<name>` followed by its
/// one-line effect description, dimmed; user-authored commands keep their
/// `SlashKind` glyph. The selected row carries the mandatory `▸` marker
/// glyph *plus* the background tint — the golden suite strips style, so a
/// style-only selection would be invisible to it. Key hints ride the top
/// border, dim and right-aligned. Shared by the single-session REPL and the
/// deck (both anchor it above their composer).
///
/// `live` overrides descriptions with values read from the model at render
/// time (`/inbox — 29 unread`), keyed by command name — computed by the
/// caller each frame, never cached in view state.
///
/// When more commands match than fit, the rows window around `selected` so
/// arrow-key navigation always keeps the highlight visible, and the bottom
/// border shows how many rows are hidden above (`▲`) / below (`▼`).
pub(crate) fn render_slash_popup(
    menu: &SlashMenu,
    selected: usize,
    live: &[(String, String)],
    area: Rect,
    buf: &mut Buffer,
) {
    ratatui::widgets::Clear.render(area, buf);
    let total = menu.matches.len();
    let selected = selected.min(total.saturating_sub(1));
    let visible = inner_height(area).max(1);
    let first = scroll_window_start(total, selected, visible);
    let last = (first + visible).min(total);
    let lines: Vec<Line<'static>> = menu.matches[first..last]
        .iter()
        .enumerate()
        .map(|(offset, c)| {
            let is_sel = first + offset == selected;
            let marker = if is_sel { "▸ " } else { "  " };
            let description = live
                .iter()
                .find(|(name, _)| *name == c.name)
                .map(|(_, live_desc)| live_desc.clone())
                .unwrap_or_else(|| c.description.clone());
            let mut line = Line::from(vec![
                Span::styled(marker.to_string(), theme::accent()),
                Span::styled(format!("{} ", c.kind.glyph()), theme::accent()),
                Span::styled(format!("{:<12}", c.name), theme::accent()),
                Span::styled(description, Style::new().fg(theme::TEXT_TERTIARY)),
            ]);
            if is_sel {
                line.style = line.style.bg(theme::SELECT_BG);
            }
            line
        })
        .collect();
    let hidden_above = first;
    let hidden_below = total.saturating_sub(last);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::accent())
        .title(format!(" / commands · {total} "))
        .title(
            Line::from(Span::styled(
                " ↑↓ move · ↵ run · esc close ",
                theme::muted(),
            ))
            .alignment(ratatui::layout::Alignment::Right),
        );
    if hidden_above > 0 || hidden_below > 0 {
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(" ▲{hidden_above} ▼{hidden_below} "),
                theme::muted(),
            ))
            .alignment(ratatui::layout::Alignment::Right),
        );
    }
    Paragraph::new(Text::from(lines))
        .block(block)
        .render(area, buf);
}

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
mod tests;
