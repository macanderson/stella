// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The ISSUES tab — SPEC 9.4, the tracker-backed issue panel: browse/search
//! the connected tracker's issues, create one through a form, comment, move
//! status, and start work — all without leaving the deck.
//!
//! ```text
//!  backlog · 3 listed
//!   ⇢ #981 open · CI … running
//!
//!     ○ #874   Command-deck render golden tests   open · dev · enhancement
//! ▸   ○ #826   run_deck event-loop pty harness     open · enhancement
//!
//!  ○ #826  run_deck event-loop pty harness
//!    open · enhancement · updated 2026-01-14T09:30:00Z
//!    ↵ open · c comment · s status · p to prompt
//! ```
//!
//! Renders from [`crate::deck_ui::IssuesPanel`] and the session's own PR — a
//! pure fold over both, taking no `DeckUi` and mutating nothing, so a mode,
//! a form field or a type-ahead window can be pinned in a test without a
//! terminal. The driver services the [`crate::envelope::WorkspaceInput`]
//! requests the key handlers emit and answers with out-of-band
//! [`crate::envelope::Inbound::IssuesList`] / `IssueActDone` / `EntityHits`
//! snapshots.
//!
//! The tab draws no frame of its own: the tab row, hint row, pulse row and
//! status bar are [`super::frame`]'s, and the content fills the band they
//! leave. The two popups below — the create form's type-ahead and the
//! send-to-prompt confirmation — are floating cards over that band, not
//! chrome around it.
//!
//! ## State glyphs
//!
//! SPEC 9.4 gives the tracker its own four-state alphabet — `▶` in progress,
//! `○` open or triage, `✓` done, `◇` blocked — kept here as literals rather
//! than mapped onto [`stella_tui_theme::glyph`]. Three of the four coincide
//! with an agent-status glyph by shape alone: taking `glyph::GATE` for
//! "blocked" would assert that a tracker state and an engine gate must move
//! together, which is not true of either.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use stella_tui_theme::token;

use crate::deck::PrInfo;
use crate::deck_ui::{IssueField, IssuesMode, IssuesPanel};
use crate::envelope::{EntityHit, IssueRow};
use crate::render::scroll_window_start;

/// Most hit rows the type-ahead popup shows before it scrolls.
const TYPEAHEAD_MAX_ROWS: usize = 8;
/// Most body lines the create form previews before eliding.
const FORM_BODY_MAX_LINES: usize = 6;

/// Gold with bold — headings, focused labels, and the border of a floating
/// card. One helper rather than a repeated pair of calls, because the weight
/// is the half that is easy to drop by hand and it is what separates a
/// heading from an ordinary gold key hint.
fn gold_bold() -> Style {
    Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD)
}

/// Draw the tab into `area`. `pr` is the session's own pull request, if the
/// monitor has seen one.
pub fn render(
    pr: Option<&PrInfo>,
    issues: &IssuesPanel,
    accessible: bool,
    area: Rect,
    buf: &mut Buffer,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);

    // `loaded_page`, not `page`: the header describes the rows on screen, and
    // `page` has already moved to whatever the last `]` asked for — which is a
    // different number whenever that fetch failed or is still in flight.
    let mut head = vec![
        Span::styled(" backlog", text),
        Span::styled(format!(" · {} listed", issues.rows.len()), muted),
    ];
    if issues.loaded_page > 0 {
        head.push(Span::styled(
            format!(" · page {}", issues.loaded_page + 1),
            muted,
        ));
    }
    let mut lines: Vec<Line<'static>> = vec![Line::from(head)];
    // The session's own PR, above whatever mode the tab is in — it is a fact
    // about this session, not about the list, so a search or a half-filled
    // create form must not hide it.
    if let Some(pr) = pr {
        lines.push(pr_strip(pr));
        lines.push(Line::default());
    }
    // The line index (within `lines`) of the create form's active field —
    // the type-ahead popup anchors right under it.
    let mut active_field_line = 0usize;

    match issues.mode {
        IssuesMode::Create => {
            active_field_line = render_form(issues, area.width as usize, &mut lines);
        }
        IssuesMode::SearchTracker => {
            lines.push(Line::from(vec![
                Span::styled("  search tracker ", gold_bold()),
                Span::styled(issues.search_query.clone(), text),
                Span::styled("▏", Style::new().fg(token::GOLD)),
            ]));
            lines.push(Line::default());
            render_list(issues, accessible, area, &mut lines);
        }
        IssuesMode::Comment | IssuesMode::SetStatus => {
            let (label, target) = (
                if issues.mode == IssuesMode::Comment {
                    "  comment on "
                } else {
                    "  set status of "
                },
                issues.selected().map(|r| r.key.clone()).unwrap_or_default(),
            );
            lines.push(Line::from(vec![
                Span::styled(label, gold_bold()),
                Span::styled(target, text),
                Span::styled(": ", muted),
                Span::styled(issues.input.clone(), text),
                Span::styled("▏", Style::new().fg(token::GOLD)),
            ]));
            lines.push(Line::default());
            render_list(issues, accessible, area, &mut lines);
        }
        IssuesMode::Browse => {
            render_list(issues, accessible, area, &mut lines);
            render_detail(issues, area.width as usize, &mut lines);
        }
        // The confirmation is a floating popup rendered after the base view —
        // the browse list stays visible behind it.
        IssuesMode::ConfirmSend => {
            render_list(issues, accessible, area, &mut lines);
        }
    }

    // Notice line (op outcomes, errors, the no-tracker hint) + key footer.
    lines.push(Line::default());
    if let Some(notice) = &issues.notice {
        lines.push(Line::from(Span::styled(
            format!("  {}{notice}", if issues.busy { "◌ " } else { "" }),
            Style::new().fg(token::GOLD),
        )));
    }
    if issues.mode == IssuesMode::Browse {
        lines.push(Line::from(vec![
            Span::styled(" w", Style::new().fg(token::GOLD)),
            Span::styled(" start work", Style::new().fg(token::GOLD)),
            Span::styled(
                "   on the selected issue → stella drafts the plan from the issue and the graph, \
                 and waits for your approval before touching code",
                dim,
            ),
        ]));
    }
    lines.push(footer(issues.mode));

    Paragraph::new(lines).render(area, buf);

    // The type-ahead popup floats above the form, anchored to its field.
    if issues.mode == IssuesMode::Create && issues.typeahead.open() {
        render_typeahead(issues, area, active_field_line, buf);
    }

    // The send-to-prompt confirmation floats above the browse list.
    if issues.mode == IssuesMode::ConfirmSend {
        render_confirm_send(issues, area, buf);
    }
}

/// The `p` confirmation popup: names every issue about to be submitted so the
/// human can verify the batch before ⏎ sends it. Esc cancels.
fn render_confirm_send(issues: &IssuesPanel, area: Rect, buf: &mut Buffer) {
    let rows = issues.picked_rows();
    // +4: title, blank, footer hint, and the border's own two rows are
    // accounted by the block; the content is title + one line per issue.
    let height = (rows.len() as u16 + 4).min(area.height.saturating_sub(2));
    let width = (area.width * 2 / 3)
        .max(40)
        .min(area.width.saturating_sub(4));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    Clear.render(popup, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(gold_bold())
        .title(Span::styled(" send to prompt ", gold_bold()));
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "about to submit {} issue{} as a prompt:",
            rows.len(),
            if rows.len() == 1 { "" } else { "s" }
        ),
        Style::new().fg(token::TEXT),
    )));
    lines.push(Line::default());
    for row in rows {
        lines.push(Line::from(vec![
            Span::styled(format!("  {}", row.key), gold_bold()),
            Span::styled(format!("  {}", row.title), Style::new().fg(token::TEXT)),
        ]));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(" ↵", Style::new().fg(token::GOLD)),
        Span::styled(" send & go to transcript", Style::new().fg(token::MUTED)),
        Span::styled("   esc", Style::new().fg(token::GOLD)),
        Span::styled(" cancel", Style::new().fg(token::MUTED)),
    ]));
    Paragraph::new(lines).block(block).render(popup, buf);
}

/// The session's own PR, as one strip above the list.
///
/// SPEC 5 sends the status wall's PR cell here (§9.4) rather than onto the
/// one-row status bar: a pull request is the tracker-side artifact of the work
/// this tab is already about, and the bar is six values that fit on one row
/// (#4126). What is kept from that cell is the part that had teeth — failing
/// CI is red and bold, because a failing PR carried an elevated drop priority
/// precisely so a narrow row could not hide it.
///
/// The CI verdict carries its **word** as well as its glyph. The status wall
/// printed the glyph alone, which its width made defensible and a full-width
/// tab does not: `✓`/`✗` differing only in colour and shape is the failure
/// mode SPEC 2's "never colour alone" names. `None` prints nothing at all —
/// the monitor has not polled yet, which is not the same claim as "passing".
///
/// A draft is muted rather than amber. The amber that marked it is the one
/// tone in this deck's palette with no token behind it and a hue that fails
/// the gold clamp ([`crate::palette`]'s own note on `WARNING`), and a draft is
/// in any case the state where nothing has been claimed yet.
fn pr_strip(pr: &PrInfo) -> Line<'static> {
    use stella_protocol::{CiStatus, PrStatus};

    let muted = Style::new().fg(token::MUTED);
    let status_color = match pr.status {
        PrStatus::Draft => token::MUTED,
        PrStatus::Open | PrStatus::Merged => token::GOLD,
        PrStatus::Closed => token::RED,
    };
    let status_style = Style::new().fg(status_color);
    let ident = match pr.number {
        Some(n) => format!("#{n}"),
        // No number parsed out of the URL — its tail still identifies the PR.
        None => pr
            .url
            .rsplit('/')
            .find(|s| !s.is_empty())
            .unwrap_or("pr")
            .to_string(),
    };
    let mut spans = vec![
        Span::styled("  ⇢ ", muted),
        Span::styled(ident, status_style.add_modifier(Modifier::BOLD)),
        Span::styled(
            format!(" {}", crate::textline::pr_status_label(pr.status)),
            status_style,
        ),
    ];
    if let Some(ci) = pr.ci {
        let (glyph, style) = match ci {
            CiStatus::Passing => ("✓", Style::new().fg(token::GREEN)),
            CiStatus::Failing => (
                "✗",
                Style::new().fg(token::RED).add_modifier(Modifier::BOLD),
            ),
            CiStatus::Pending => ("◌", muted),
            CiStatus::Running => ("…", muted),
        };
        spans.push(Span::styled(" · CI ", muted));
        spans.push(Span::styled(glyph, style));
        spans.push(Span::styled(
            format!(" {}", crate::textline::ci_status_label(ci)),
            style,
        ));
    }
    Line::from(spans)
}

/// The browse list, windowed on the selection so long lists keep it in view.
fn render_list(issues: &IssuesPanel, accessible: bool, area: Rect, lines: &mut Vec<Line<'static>>) {
    let muted = Style::new().fg(token::MUTED);
    if issues.rows.is_empty() {
        if !issues.busy {
            lines.push(Line::from(Span::styled(
                if issues.loaded {
                    "  No issues matched."
                } else {
                    "  No issues loaded yet — press r to fetch the tracker's list."
                },
                muted,
            )));
        }
        return;
    }
    // Header lines already pushed, the detail pane, the notice and the two
    // footer rows.
    let reserved = lines.len() + DETAIL_ROWS + 4;
    let visible = (area.height as usize).saturating_sub(reserved).max(1);
    let selected = issues.sel.min(issues.rows.len() - 1);
    let first = scroll_window_start(issues.rows.len(), selected, visible);
    let last = (first + visible).min(issues.rows.len());
    for (i, row) in issues.rows.iter().enumerate().take(last).skip(first) {
        let width = area.width as usize;
        let picked = issues.picked.contains(&row.key);
        lines.push(if accessible {
            issue_record(row, i == selected, width)
        } else {
            issue_line(row, i == selected, picked, width)
        });
    }
    if last < issues.rows.len() {
        lines.push(Line::from(Span::styled(
            format!("  … {} more", issues.rows.len() - last),
            muted,
        )));
    }
}

/// One issue row in accessible mode: the same fields, each named.
///
/// `[open]` is a chip — brackets are a visual container, and spoken they are
/// punctuation around a word that could as easily be a label as a state. The
/// bare trailing `· octocat · bug, p1` has the same problem: two lists with no
/// indication of which is which.
fn issue_record(row: &IssueRow, selected: bool, width: usize) -> Line<'static> {
    let fields = [
        ("state", row.state.clone()),
        ("title", row.title.clone()),
        ("assignee", row.assignee.clone().unwrap_or_default()),
        ("labels", row.labels.join(", ")),
    ];
    super::record::record_line(
        super::record::identity(row.key.clone(), selected, token::GOLD),
        &fields,
        width,
    )
}

/// One issue row: `▸ ▶ KEY  title   state   assignee · labels   age` — the
/// state's glyph beside the key, `●` marking a multiselect pick, `▸` the
/// cursor.
fn issue_line(row: &IssueRow, selected: bool, picked: bool, width: usize) -> Line<'static> {
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let marker = if selected { "▸ " } else { "  " };
    let pick = if picked { "● " } else { "  " };
    let (glyph, glyph_style) = state_mark(&row.state);
    let mut key_style = Style::new().fg(token::GOLD);
    let mut body_style = text;
    if selected {
        key_style = key_style.bg(token::HL).add_modifier(Modifier::BOLD);
        body_style = body_style.bg(token::HL);
    }
    let mut tail = String::new();
    if let Some(assignee) = &row.assignee {
        tail.push_str(&format!(" · {assignee}"));
    }
    if !row.labels.is_empty() {
        tail.push_str(&format!(" · {}", row.labels.join(", ")));
    }
    let age = row
        .updated_at
        .as_deref()
        .map(|u| u.get(..10).unwrap_or(u).to_string())
        .unwrap_or_default();
    let head = format!("{marker}{pick}");
    let key = format!("{:<10}", row.key);
    let state = format!("  {}", row.state);
    let budget = width
        .saturating_sub(
            head.chars().count()
                + 2
                + key.chars().count()
                + state.chars().count()
                + tail.chars().count()
                + age.chars().count()
                + 3,
        )
        .max(8);
    let title = truncate(&row.title, budget);
    let mut spans = vec![
        Span::styled(head, Style::new().fg(token::GOLD)),
        Span::styled(format!("{glyph} "), glyph_style),
        Span::styled(key, key_style),
        Span::styled(title.clone(), body_style),
        Span::styled(state, glyph_style),
        Span::styled(tail, muted),
    ];
    if !age.is_empty() {
        let used: usize = spans.iter().map(Span::width).sum();
        if used + age.chars().count() + 1 < width {
            spans.push(Span::raw(
                " ".repeat(width - used - age.chars().count() - 1),
            ));
        } else {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(age, dim));
    }
    Line::from(spans)
}

/// The glyph and tone for a tracker state word (SPEC 9.4; see the module doc
/// on why these four are literals).
fn state_mark(state: &str) -> (char, Style) {
    let lower = state.to_ascii_lowercase();
    if lower.contains("progress") || lower.contains("started") {
        ('▶', Style::new().fg(token::GOLD))
    } else if lower.contains("block") {
        ('◇', Style::new().fg(token::RED))
    } else if lower.contains("done") || lower.contains("closed") || lower.contains("merged") {
        ('✓', Style::new().fg(token::DIM))
    } else {
        ('○', Style::new().fg(token::MUTED))
    }
}

/// Rows the detail pane spends under the list.
const DETAIL_ROWS: usize = 5;

/// The selected issue, in full: key and title, then assignee, labels, the
/// tracker URL and when it last moved — the fields the one-line row
/// truncates. SPEC 9.4's linked-plan and evidence lines have no producer
/// yet (#4336).
fn render_detail(issues: &IssuesPanel, width: usize, lines: &mut Vec<Line<'static>>) {
    let Some(row) = issues.selected() else {
        return;
    };
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let (glyph, glyph_style) = state_mark(&row.state);
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(format!(" {glyph} "), glyph_style),
        Span::styled(row.key.clone(), Style::new().fg(token::GOLD)),
        Span::styled(
            format!("  {}", truncate(&row.title, width.saturating_sub(20))),
            text,
        ),
    ]));
    let mut facts: Vec<Span<'static>> = vec![Span::raw("   ")];
    facts.push(Span::styled(row.state.clone(), glyph_style));
    if let Some(assignee) = &row.assignee {
        facts.push(Span::styled(" · assignee ", dim));
        facts.push(Span::styled(assignee.clone(), muted));
    }
    if !row.labels.is_empty() {
        facts.push(Span::styled(" · ", dim));
        facts.push(Span::styled(row.labels.join(", "), muted));
    }
    if let Some(updated) = &row.updated_at {
        facts.push(Span::styled(" · updated ", dim));
        facts.push(Span::styled(updated.clone(), muted));
    }
    lines.push(Line::from(facts));
    if !row.url.is_empty() {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(truncate(&row.url, width.saturating_sub(4)), dim),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("   ↵", muted),
        Span::styled(" open · ", dim),
        Span::styled("c", muted),
        Span::styled(" comment · ", dim),
        Span::styled("s", muted),
        Span::styled(" status · ", dim),
        Span::styled("p", muted),
        Span::styled(" to prompt", dim),
    ]));
}

/// The create form. Returns the line index of the active field (for the
/// type-ahead popup's anchor).
fn render_form(issues: &IssuesPanel, width: usize, lines: &mut Vec<Line<'static>>) -> usize {
    lines.push(Line::from(Span::styled("  new issue", gold_bold())));
    lines.push(Line::default());
    let mut active = 0usize;

    let field_line = |lines: &mut Vec<Line<'static>>, label: &str, value: &str, focused: bool| {
        let label_style = if focused {
            gold_bold()
        } else {
            Style::new().fg(token::MUTED)
        };
        let mut spans = vec![
            Span::styled(format!("  {label:<9} "), label_style),
            Span::styled(
                truncate(value, width.saturating_sub(16)),
                Style::new().fg(token::TEXT),
            ),
        ];
        if focused {
            spans.push(Span::styled("▏", Style::new().fg(token::GOLD)));
        }
        lines.push(Line::from(spans));
    };

    // Title.
    if issues.form_field == IssueField::Title {
        active = lines.len();
    }
    field_line(
        lines,
        "title",
        &issues.form_title,
        issues.form_field == IssueField::Title,
    );

    // Body — the one multi-line field; preview capped, caret on the tail.
    let body_focused = issues.form_field == IssueField::Body;
    if body_focused {
        active = lines.len();
    }
    let body = issues.form_body.buffer().to_string();
    let body_lines: Vec<&str> = body.split('\n').collect();
    let shown = body_lines.len().min(FORM_BODY_MAX_LINES);
    for (i, body_line) in body_lines.iter().take(shown).enumerate() {
        let label = if i == 0 { "body" } else { "" };
        let is_tail = i + 1 == shown && body_lines.len() <= FORM_BODY_MAX_LINES;
        field_line(lines, label, body_line, body_focused && is_tail);
    }
    if body_lines.len() > FORM_BODY_MAX_LINES {
        lines.push(Line::from(Span::styled(
            format!("            … {} more lines", body_lines.len() - shown),
            Style::new().fg(token::MUTED),
        )));
    }

    // Labels + assignee — the type-ahead fields.
    if issues.form_field == IssueField::Labels {
        active = lines.len();
    }
    field_line(
        lines,
        "labels",
        &issues.form_labels,
        issues.form_field == IssueField::Labels,
    );
    if issues.form_field == IssueField::Assignee {
        active = lines.len();
    }
    field_line(
        lines,
        "assignee",
        &issues.form_assignee,
        issues.form_field == IssueField::Assignee,
    );
    active
}

/// The `Kind: label — description` text of one type-ahead row, split at the
/// kind prefix (styled separately) and char-safe-truncated to `max_chars`
/// across the pair. Pure — the row-format contract lives here.
fn entity_hit_parts(hit: &EntityHit, max_chars: usize) -> (String, String) {
    let kind = format!("{}: ", hit.kind);
    let rest = if hit.description.is_empty() {
        hit.label.clone()
    } else {
        format!("{} — {}", hit.label, hit.description)
    };
    let kind_len = kind.chars().count();
    if kind_len >= max_chars {
        return (truncate(&kind, max_chars), String::new());
    }
    (kind, truncate(&rest, max_chars - kind_len))
}

/// The floating type-ahead popup: gold-bordered, selection windowed, one
/// muted legend line. Anchored right under the active form field (clamped to
/// the tab's band).
fn render_typeahead(issues: &IssuesPanel, area: Rect, field_line: usize, buf: &mut Buffer) {
    let muted = Style::new().fg(token::MUTED);
    let ta = &issues.typeahead;
    let rows = ta.hits.len().clamp(1, TYPEAHEAD_MAX_ROWS);
    let h = (rows as u16 + 3).min(area.height);
    let w = area.width.saturating_sub(4).clamp(20, 56).min(area.width);
    let below = area.y + (field_line as u16).saturating_add(1);
    let y = if below + h <= area.y + area.height {
        below
    } else {
        (area.y + area.height).saturating_sub(h)
    };
    let popup = Rect {
        x: area.x + area.width.saturating_sub(w) / 2,
        y,
        width: w,
        height: h,
    };
    Clear.render(popup, buf);

    let visible = (h as usize).saturating_sub(3).max(1);
    let selected = ta.sel.min(ta.hits.len().saturating_sub(1));
    let first = scroll_window_start(ta.hits.len(), selected, visible);
    let last = (first + visible).min(ta.hits.len());

    let mut lines: Vec<Line<'static>> = Vec::new();
    if ta.hits.is_empty() {
        lines.push(Line::from(Span::styled(
            if ta.loading {
                "  searching…"
            } else {
                "  no matches"
            },
            muted,
        )));
    }
    for (i, hit) in ta.hits.iter().enumerate().take(last).skip(first) {
        let is_sel = i == selected;
        let marker = if is_sel { "▸ " } else { "  " };
        let mut kind_style = gold_bold();
        let mut rest_style = Style::new().fg(token::TEXT);
        if is_sel {
            kind_style = kind_style.add_modifier(Modifier::REVERSED);
            rest_style = rest_style.add_modifier(Modifier::REVERSED);
        }
        let (kind, rest) = entity_hit_parts(hit, (w as usize).saturating_sub(6));
        lines.push(Line::from(vec![
            Span::styled(marker.to_string(), kind_style),
            Span::styled(kind, kind_style),
            Span::styled(rest, rest_style),
        ]));
    }
    // Pad so the legend sits on the last interior row.
    while lines.len() < (h as usize).saturating_sub(3) + 1 {
        lines.push(Line::default());
    }
    lines.push(Line::from(Span::styled(
        " ↑↓ select · enter/tab insert · esc close",
        muted,
    )));

    let field = ta.field.map(|f| f.label().to_string()).unwrap_or_default();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(gold_bold())
        .title(format!(" {field} · {} ", ta.hits.len()));
    Paragraph::new(lines).block(block).render(popup, buf);
}

/// Char-safe prefix truncation with an ellipsis.
fn truncate(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{head}…")
}

/// The keybind footer, per mode — the same muted-key/dim-word convention as
/// the MCP tab.
fn footer(mode: IssuesMode) -> Line<'static> {
    let pairs: &[(&str, &str)] = match mode {
        IssuesMode::Browse => &[
            ("↑↓", "select"),
            ("w", "start work"),
            ("n", "new issue"),
            ("/", "search tracker"),
            ("r", "refresh"),
            ("]/[", "page"),
            ("space", "pick"),
            ("p", "send to prompt"),
            ("x", "close / reopen"),
        ],
        IssuesMode::SearchTracker => &[("type", "query"), ("↵", "search"), ("esc", "back")],
        IssuesMode::Create => &[
            ("tab", "field"),
            ("@", "opens the picker"),
            ("ctrl+s", "create"),
            ("esc", "cancel"),
        ],
        IssuesMode::Comment | IssuesMode::SetStatus => {
            &[("type", "text"), ("↵", "send"), ("esc", "cancel")]
        }
        IssuesMode::ConfirmSend => &[("↵", "send & go to transcript"), ("esc", "cancel")],
    };
    let key = Style::new().fg(token::MUTED);
    let dim = Style::new().fg(token::DIM);
    let mut spans = vec![Span::raw(" ")];
    for (i, (k, desc)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled((*k).to_string(), key));
        spans.push(Span::styled(format!(" {desc}"), dim));
    }
    Line::from(spans)
}

#[cfg(test)]
// The lint wants a struct literal, and `IssuesPanel` keeps a private request
// counter — so `Default` followed by assignment is the only way to build one
// from outside `deck_ui`.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    fn hit(kind: &str, label: &str, description: &str) -> EntityHit {
        EntityHit {
            kind: kind.into(),
            label: label.into(),
            description: description.into(),
            insert: label.into(),
        }
    }

    fn text(line: Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.clone()).collect()
    }

    #[test]
    fn entity_hit_rows_read_kind_label_description() {
        let (kind, rest) = entity_hit_parts(&hit("Person", "octocat", "Octo Cat"), 80);
        assert_eq!(format!("{kind}{rest}"), "Person: octocat — Octo Cat");

        // No description: the dash separator is dropped, never dangling.
        let (kind, rest) = entity_hit_parts(&hit("Label", "bug", ""), 80);
        assert_eq!(format!("{kind}{rest}"), "Label: bug");
    }

    #[test]
    fn entity_hit_rows_truncate_char_safely() {
        // A multi-byte description must truncate at a char boundary with an
        // ellipsis, never mid-codepoint.
        let (kind, rest) = entity_hit_parts(&hit("Memory", "naming", "prefer déjà-vu naming"), 20);
        let combined = format!("{kind}{rest}");
        assert!(combined.chars().count() <= 20, "{combined:?}");
        assert!(combined.ends_with('…'), "{combined:?}");

        // A kind wider than the budget still never panics.
        let (kind, rest) = entity_hit_parts(&hit("Symbol", "x", "y"), 4);
        assert!(kind.chars().count() <= 4, "{kind:?}");
        assert!(rest.is_empty());
    }

    fn eng_42() -> IssueRow {
        IssueRow {
            key: "ENG-42".into(),
            title: "Fix flaky test".into(),
            state: "In Progress".into(),
            labels: vec!["bug".into(), "ci".into()],
            assignee: Some("mona@example.com".into()),
            url: String::new(),
            updated_at: None,
        }
    }

    #[test]
    fn issue_lines_mark_the_selection_and_carry_assignee_and_labels() {
        let row = eng_42();
        let selected = text(issue_line(&row, true, false, 120));
        assert!(selected.starts_with("▸   ▶ ENG-42"), "{selected}");
        assert!(selected.contains("  In Progress"), "{selected}");
        assert!(selected.contains("mona@example.com"), "{selected}");
        assert!(selected.contains("bug, ci"), "{selected}");
        let plain = text(issue_line(&row, false, false, 120));
        assert!(plain.starts_with("    ▶ ENG-42"), "{plain}");
        // A multiselect pick shows its marker whether or not it is the cursor.
        let picked = text(issue_line(&row, false, true, 120));
        assert!(picked.starts_with("  ● ▶ ENG-42"), "{picked}");
    }

    /// The tab is a fold over the panel and the PR, so a browse list draws
    /// from a `Rect` and a `Buffer` alone — no `DeckUi`, no terminal, and no
    /// mutation the caller has to undo.
    #[test]
    fn the_tab_draws_from_the_panel_alone() {
        let mut issues = IssuesPanel::default();
        issues.rows = vec![eng_42()];
        issues.loaded = true;
        let area = Rect::new(0, 0, 120, 24);
        let mut buf = Buffer::empty(area);
        render(None, &issues, false, area, &mut buf);
        let drawn: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(drawn.contains("backlog · 1 listed"), "{drawn}");
        assert!(drawn.contains("ENG-42"), "{drawn}");
        assert!(drawn.contains("start work"), "{drawn}");
    }
}
