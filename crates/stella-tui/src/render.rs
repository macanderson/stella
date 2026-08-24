//! Pure rendering: `(model, ui) -> frame`. Every panel is drawn by a function
//! that reads only `&SessionModel` / small `Copy` view values, so the whole
//! surface is a deterministic function of the event log plus the ephemeral
//! scroll/compose state (L-T1).
//!
//! # Panel panic boundary (L-T7)
//!
//! Nothing here takes the boundary itself. These are leaf panels the deck
//! draws *inside* its own guarded bands (`deck_render` → `panel_guard`), so a
//! panic in one is caught by the band that called it and painted as an error
//! card over that band's rectangle, with input alive.
//!
//! That is also why no recoverability argument is owed on this path: every
//! function below takes `&`-only inputs (`&SessionModel` and `Copy` values —
//! no interior mutability) plus the scratch [`Buffer`] the guard throws away
//! on panic. The deck's own closures do capture `&mut DeckUi`, and the
//! argument for those lives with the boundary in `panel_guard`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};

use crate::composer::SlashMenu;
use crate::model::{AskUserPrompt, FileState, InlineDiffRef};

mod entry;
// `pub(crate)` for `wrap_one_indent` alone: the startup-notice dialog
// (`crate::notice`) wraps its detail clauses with the same hanging indent the
// transcript uses, rather than growing a second wrapper beside it.
pub(crate) mod row;
use crate::theme;
// The transcript content builders moved to `entry` when this file crossed the
// 1500-line guard; re-exported so `crate::render::transcript_lines` and
// `::entry_lines` still resolve for `ui.rs` and `deck_ui.rs`.
pub(crate) use entry::{EntryView, entry_lines, reasoning_is_live, streaming_lines};
pub(crate) use row::*;

/// The usable interior width of a single-border panel.
pub(crate) fn inner_width(area: Rect) -> usize {
    area.width.saturating_sub(2) as usize
}

/// The usable interior height of a single-border panel.
pub(crate) fn inner_height(area: Rect) -> usize {
    area.height.saturating_sub(2) as usize
}

// Word-aware line wrapping (pre-wrap so scroll math stays line-exact, L-T4)

// Panels

// The scope gate's renderer is the modal plan-review dialog
// (`crate::views::scope_dialog`) — the band it replaced lived here.

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

/// Where the command palette floats: anchored to the composer's left edge,
/// opening upward, tall enough for its interior rows (capped at
/// [`SLASH_POPUP_MAX_ROWS`]) plus its own hint row, and clamped to the frame
/// on small terminals. The `+3` reserves the two border rows and the hint row
/// under the matches — the key hints ride the top border, so no interior row
/// but the hint is chrome (SPEC 10).
///
/// `rows` counts group headings as well as matches ([`display_rows`]): a
/// sectioned browse list that sized itself on matches alone would clip its
/// last commands behind their own captions.
pub(crate) fn slash_popup_area(root: Rect, composer: Rect, rows: usize) -> Rect {
    let h = ((rows.min(SLASH_POPUP_MAX_ROWS) as u16) + 3).min(root.height);
    let w = root.width.saturating_sub(2).min(96);
    Rect {
        x: composer.x,
        y: composer.y.saturating_sub(h),
        width: w,
        height: h,
    }
}

/// One interior row of the command palette: a group heading, or the match at
/// that index in [`SlashMenu::matches`].
///
/// The selection is an index into the *matches*, and the scroll window is
/// over the *rows*, so the two are different coordinates and the popup keeps
/// them apart by name rather than by arithmetic (#4338).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PopupRow {
    Heading(String),
    Command(usize),
}

/// The palette's interior rows: every match, with its section heading above
/// it. `sections` is ascending and every index is in range (both guaranteed
/// by [`SlashMenu::filter_with`]), so a heading lands exactly once.
pub(crate) fn display_rows(menu: &SlashMenu) -> Vec<PopupRow> {
    let mut rows = Vec::with_capacity(menu.matches.len() + menu.sections.len());
    for index in 0..menu.matches.len() {
        if let Some((_, heading)) = menu.sections.iter().find(|(at, _)| *at == index) {
            rows.push(PopupRow::Heading(heading.clone()));
        }
        rows.push(PopupRow::Command(index));
    }
    rows
}

/// The first visible row of a scrolling list of `len` rows that shows
/// `visible` at a time, chosen so `selected` stays on screen — the window
/// only moves once the selection would fall off an edge. Mirrors the
/// composer's cursor-row windowing (`render_composer` in [`crate::deck_render`]) so the slash popup
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

/// The command palette (SPEC 10, rendering `08-command-palette`):
///
/// ```text
/// ╭ / commands  6 of 129 · fuzzy ──────────────── ↑↓ move · ↵ run ╮
/// │ /gates          show the gate board                ◐ 2/5 green │
/// │ /gate rerun     rerun one gate                         ⇥ <name> │
/// │ ⚡ /fix-bug      fix a bug end to end                            │
/// │ ▲1 ▼3                                                           │
/// ╰────────────────────────────────────────────────────────────────╯
/// ```
///
/// Each row is the command in gold — the typed prefix lit bright, the rest
/// gold — its one-line effect dim, and a live value on the right when the
/// model has one (`/inbox · 3 unread`). The selected row carries the `▸`
/// marker *plus* the highlight ground: the golden suite strips style, so a
/// style-only selection would be invisible to it. User-authored commands keep
/// their `SlashKind` glyph.
///
/// `live` overrides descriptions with values read from the model at render
/// time, keyed by command name — computed by the caller each frame, never
/// cached in view state.
///
/// When more commands match than fit, the rows window around `selected` so
/// arrow-key navigation always keeps the highlight visible, and the hint row
/// says how many are hidden above (`▲`) and below (`▼`).
///
/// With no query typed, the list is sectioned: [`SlashMenu::sections`] puts
/// `relevant now · <why>` over the commands the session's own state makes
/// worth reaching for, then a heading per [`crate::composer::SlashDomain`]
/// group (#4338). A typed query drops the headings — grouping a three-row
/// result buries the rows under their own captions — and keeps the flat
/// ranking, in which a relevant command still leads its rank.
///
/// The renderings' `recent` section is still absent: it needs per-workspace
/// persistence, which the deck has no store for (#4338).
pub(crate) fn render_slash_popup(
    menu: &SlashMenu,
    selected: usize,
    live: &[(String, String)],
    area: Rect,
    buf: &mut Buffer,
) {
    use stella_tui_theme::token;
    ratatui::widgets::Clear.render(area, buf);
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let gold = Style::new().fg(token::GOLD);
    let lit = Style::new()
        .fg(token::GOLD_BRIGHT)
        .add_modifier(Modifier::BOLD);

    let total = menu.matches.len();
    let selected = selected.min(total.saturating_sub(1));
    // Headings occupy rows of their own, so the window is over the display
    // list rather than over the matches — a section boundary must not push
    // the highlight off the bottom edge.
    let rows = display_rows(menu);
    let selected_row = rows
        .iter()
        .position(|row| *row == PopupRow::Command(selected))
        .unwrap_or(0);
    // The hint row keeps the last interior row.
    let visible = inner_height(area).saturating_sub(1).max(1);
    let first = scroll_window_start(rows.len(), selected_row, visible);
    let last = (first + visible).min(rows.len());
    let inner_w = inner_width(area);
    let query = menu.query.trim_start_matches('/').to_ascii_lowercase();

    let mut lines: Vec<Line<'static>> = rows[first..last]
        .iter()
        .map(|row| {
            let index = match row {
                // A heading is chrome: dim, indented under the border, and
                // never selectable — the selection walks commands only.
                PopupRow::Heading(text) => {
                    return Line::from(Span::styled(format!(" {text}"), muted));
                }
                PopupRow::Command(index) => *index,
            };
            let c = menu.matches[index];
            let is_sel = index == selected;
            let marker = if is_sel { "▸ " } else { "  " };
            let live_value = live
                .iter()
                .find(|(name, _)| *name == c.name)
                .map(|(_, v)| v.clone());
            let description = c.description.clone();
            // The typed prefix lights up inside the name — `/ga` → `/ga`tes.
            let name = c.name.clone();
            let bare = name.trim_start_matches('/').to_ascii_lowercase();
            let (head, tail) = if !query.is_empty() && bare.starts_with(&query) {
                let cut = 1 + query.len();
                (name[..cut].to_string(), name[cut..].to_string())
            } else {
                (String::new(), name.clone())
            };
            let mut spans = vec![Span::styled(marker.to_string(), gold)];
            if c.kind != crate::composer::SlashKind::Builtin {
                spans.push(Span::styled(format!("{} ", c.kind.glyph()), muted));
            }
            if !head.is_empty() {
                spans.push(Span::styled(head.clone(), lit));
            }
            let pad = 16usize.saturating_sub(head.chars().count() + tail.chars().count());
            spans.push(Span::styled(format!("{tail}{}", " ".repeat(pad)), gold));
            spans.push(Span::styled(format!(" {description}"), dim));
            if let Some(value) = live_value {
                let used: usize = spans.iter().map(Span::width).sum();
                if used + value.chars().count() + 1 < inner_w {
                    spans.push(Span::raw(
                        " ".repeat(inner_w - used - value.chars().count() - 1),
                    ));
                    spans.push(Span::styled(value, muted));
                }
            }
            let mut line = Line::from(spans);
            if is_sel {
                line.style = Style::new().bg(token::HL);
            }
            line
        })
        .collect();
    let hidden_above = rows[..first]
        .iter()
        .filter(|row| matches!(row, PopupRow::Command(_)))
        .count();
    let hidden_below = rows[last..]
        .iter()
        .filter(|row| matches!(row, PopupRow::Command(_)))
        .count();
    let shown = total - hidden_above - hidden_below;
    let mut hint = vec![Span::raw(" ")];
    if hidden_above > 0 || hidden_below > 0 {
        hint.push(Span::styled(
            format!("▲{hidden_above} ▼{hidden_below}"),
            dim,
        ));
    } else {
        hint.push(Span::styled("⇥ completes · esc closes", dim));
    }
    lines.push(Line::from(hint));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::new().fg(token::RULE))
        .title(Line::from(vec![
            Span::styled(" / commands", gold),
            Span::styled(format!("  {shown} of {total} · fuzzy "), muted),
        ]))
        .title(Line::from(Span::styled(" ↑↓ move · ↵ run · esc ", dim)).right_aligned());
    Paragraph::new(Text::from(lines))
        .block(block)
        .render(area, buf);
}

/// The `/model` argument menu ([`crate::composer::args`]): the palette's
/// floating shape over candidate model specs. The typed fragment lights up
/// inside each candidate the way the typed prefix does in the command
/// palette; the window scrolls around the selection identically.
pub(crate) fn render_arg_popup(
    command: &str,
    matches: &[String],
    fragment: &str,
    selected: usize,
    area: Rect,
    buf: &mut Buffer,
) {
    use stella_tui_theme::token;
    ratatui::widgets::Clear.render(area, buf);
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let gold = Style::new().fg(token::GOLD);
    let lit = Style::new()
        .fg(token::GOLD_BRIGHT)
        .add_modifier(Modifier::BOLD);

    let total = matches.len();
    let selected = selected.min(total.saturating_sub(1));
    let visible = inner_height(area).saturating_sub(1).max(1);
    let first = scroll_window_start(total, selected, visible);
    let last = (first + visible).min(total);
    let needle = fragment.to_ascii_lowercase();

    let mut lines: Vec<Line<'static>> = matches[first..last]
        .iter()
        .enumerate()
        .map(|(offset, candidate)| {
            let index = first + offset;
            let is_sel = index == selected;
            let marker = if is_sel { "▸ " } else { "  " };
            let mut spans = vec![Span::styled(marker.to_string(), gold)];
            // Light the matched fragment wherever it sits in the spec.
            match (!needle.is_empty())
                .then(|| candidate.to_ascii_lowercase().find(&needle))
                .flatten()
            {
                Some(at) => {
                    let end = at + needle.len();
                    spans.push(Span::styled(candidate[..at].to_string(), gold));
                    spans.push(Span::styled(candidate[at..end].to_string(), lit));
                    spans.push(Span::styled(candidate[end..].to_string(), gold));
                }
                None => spans.push(Span::styled(candidate.clone(), gold)),
            }
            let mut line = Line::from(spans);
            if is_sel {
                line.style = Style::new().bg(token::HL);
            }
            line
        })
        .collect();
    let (hidden_above, hidden_below) = (first, total - last);
    let mut hint = vec![Span::raw(" ")];
    if hidden_above > 0 || hidden_below > 0 {
        hint.push(Span::styled(
            format!("▲{hidden_above} ▼{hidden_below}"),
            dim,
        ));
    } else {
        hint.push(Span::styled("⇥ completes · esc closes", dim));
    }
    lines.push(Line::from(hint));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::new().fg(token::RULE))
        .title(Line::from(vec![
            Span::styled(format!(" {command}"), gold),
            Span::styled(format!("  {} of {total} ", last - first), muted),
        ]))
        .title(Line::from(Span::styled(" ↑↓ move · ↵ run · esc ", dim)).right_aligned());
    Paragraph::new(Text::from(lines))
        .block(block)
        .render(area, buf);
}

/// Most styled diff lines a collapsed tool result shows inline before folding
/// the rest behind ctrl+o — a mutation stays glanceable in the transcript
/// without a large diff flooding it uninvited.
///
/// Public because the plain surface renders the same capped diff (#2421) and
/// a second number would be a second policy: "how much diff is glanceable" is
/// one judgement about reading, not about ratatui.
///
/// It is not about *terminals* either, which is why the number itself now
/// lives in [`stella_diff::view`] beside the policy that spends it: the
/// Observatory and an exported dashboard render the same edit, and a cap that
/// disagreed across them would make one change look like three.
pub const INLINE_DIFF_CAP: usize = stella_diff::view::INLINE_CAP;

/// Re-exported for the same reason as [`INLINE_DIFF_CAP`]: the plain surface
/// previews a chain of thought to the same depth the deck does, and the depth
/// is a reading judgement both surfaces should lose or keep together.
pub use entry::THINKING_ROWS;

/// Resolve a tool result's [`InlineDiffRef`] to the diff text it may render,
/// or `None` when the reference can no longer be honoured.
///
/// The diff shown must be the one *this* call produced, so the lookup is by the
/// `changes` seq recorded at fold time — never "the path's latest diff", which
/// would misattribute an later edit's change to an earlier row.
///
/// It resolves for as long as that mutation's text is still held:
/// [`FileState`] remembers every mutation of a path, so a file edited any
/// number of times keeps every row's diff, and only two things take one away —
/// the byte budget
/// [`DIFF_TEXT_BUDGET`](crate::model::DIFF_TEXT_BUDGET) releasing the oldest
/// text under memory pressure, or the path itself being evicted at
/// [`MAX_TRACKED_FILES`](crate::model::file_state::MAX_TRACKED_FILES). The
/// first leaves the row its measured `+N −M`; the second leaves it naming its
/// change (#4365).
///
/// This used to say the reference went stale the moment a later mutation
/// bumped the counter, which described the behaviour before that history
/// existed and read as though almost every diff were hidden.
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

/// The `(added, removed)` for a whole call: [`resolve_inline_delta`] summed
/// over every change the call claimed (#4214).
///
/// A row states one scope or the other and never both at once. A head reading
/// `3 files · +12 −4` where the counts were the *first* file's would be two
/// numbers disagreeing about what they describe — the defect class #4155 and
/// #4156 are about — so the count and the delta are derived from the same set
/// here rather than from different ends of it.
///
/// A reference that no longer resolves (its path evicted) contributes nothing
/// rather than a zero, and `None` when *none* of
/// them resolve, which is the same "no column at all" the singular resolver
/// already returns. For a one-change call this is [`resolve_inline_delta`]
/// exactly.
pub(crate) fn resolve_inline_delta_total(
    refs: &[InlineDiffRef],
    files: &[FileState],
) -> Option<(u32, u32)> {
    refs.iter()
        .filter_map(|dref| resolve_inline_delta(dref, files))
        .reduce(|(a1, r1), (a2, r2)| (a1 + a2, r1 + r2))
}

#[cfg(test)]
mod tests;
