// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The SKILLS tab — SPEC 9.2, the filesystem-first skills manager:
//!
//! ```text
//! ╭ ⌕ pdf▌                                     installed + registry · web · 3 hits ╮
//! installed · 4 · 2 match
//! ╭────────────────────────────────────────────────────────────────────────────────╮
//! │▸ [x] rust-review              v2/4 · user   Review Rust diffs for ownership.    │
//! │  [ ] sql-tuning               v1 · user     Read a query plan and propose …     │
//! │ learned from traces · 1   stella wrote these after repeated wins                │
//! │  [x] bench-rig-access         learned   from 4 traces · turn 37 · was a1b2c3d4  │
//! │ 1 rejected   press ! to review · reverse one                                    │
//! ╰────────────────────────────────────────────────────────────────────────────────╯
//! registry · web · 3 results
//! ╭────────────────────────────────────────────────────────────────────────────────╮
//! │▸ wshobson/agents@pdf-extract                       ▰▰▰▱ 15.8K installs ↵ install│
//! ╰────────────────────────────────────────────────────────────────────────────────╯
//!  space on/off · ctrl+o preview · e edit · p pin · n new from prompt · → search
//!  4 installed · 1 learned · 3 enabled
//! ```
//!
//! One search box over two sources: the **installed** list (activate, disable,
//! uninstall, edit, pin — plus the skills stella learned from its own traces,
//! in a section of their own showing `from N traces · turn M · was <hash>` and
//! answering `r` rename, `ctrl+o` source traces, `x` reject) and the
//! **registry** (`npx skills find` → install). ←/→ move the keyboard between
//! the two. Below the learned section, a collapsed `N rejected` line is the
//! reader/undo half `x` never had: `!` opens a review of every rejection
//! this workspace has recorded — name and date — and reverses one, after
//! which the miner is free to propose it again on its very next pass.
//! The driver owns the skills on disk, their state, and the registry;
//! this module renders the [`crate::envelope::SkillsView`] read-model it
//! pushes, and [`overlays`] draws the dialogs and the `ctrl+o` preview over the
//! top. Content is a function of `(ui.skills)`, so buffer tests stay stable.
//!
//! The two source boxes stack on every frame rather than sitting side by side,
//! so a read-aloud row carries one source and never a slice of each — accessible
//! mode needs no second layout here (#1258).
//!
//! The renderings' per-skill economics (`18× · 0.9k` tokens per inject) and the
//! registry's signature status have no producer yet (#4337); both are elided
//! rather than drawn with a stand-in.

pub mod overlays;

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};
use stella_tui_theme::token;

use crate::deck::WorkspaceModel;
use crate::deck_ui::{DeckUi, SkillsFocus};
use crate::envelope::SkillRow;
use crate::render::columns;

/// Draw the tab into `area`, then float whichever dialog is open above it.
pub fn render(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    let hits = ui.skills.hits.len();
    let registry_h = if ui.skills.focus == SkillsFocus::Search || hits > 0 || ui.skills.searching {
        // Half the tab at most, and no floor: `hits + 4` is already 4 or more,
        // so a `clamp(4, ..)` bought nothing and panicked whenever the tab was
        // shorter than eight rows, because `clamp` asserts `min <= max`.
        u16::try_from(hits)
            .unwrap_or(u16::MAX)
            .saturating_add(4)
            .min(area.height / 2)
    } else {
        2
    };
    let bands = Layout::vertical([
        Constraint::Length(3),          // the search box
        Constraint::Min(4),             // installed (+ learned)
        Constraint::Length(registry_h), // registry
        Constraint::Length(2),          // keys · counts
    ])
    .split(area);

    render_search_box(ui, bands[0], buf);
    render_installed(ui, bands[1], buf);
    render_search(ui, bands[2], buf);
    render_status(ui, bands[3], buf);

    // The prompts float above the boxes; the creating dialog animates off the
    // deck clock. The `ctrl+o` preview is topmost — mutually exclusive with the
    // prompts at the key layer, drawn last anyway.
    if ui.skills.prompt.is_some() {
        overlays::render_prompt(ui, model.now_ms, area, buf);
    }
    if ui.skills.preview.is_some() {
        overlays::render_preview(ui, area, buf);
    }
}

/// The section boxes each source sits in. A box per source, never one around
/// the whole tab — [`crate::views::frame`] already carved the content area out.
fn rounded() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(token::BORDER))
}

/// Installed rows matching the live query — every row when it is empty.
fn matching(ui: &DeckUi) -> Vec<(usize, &SkillRow)> {
    let needle = ui.skills.query.trim().to_lowercase();
    ui.skills
        .view
        .rows
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            needle.is_empty()
                || row.name.to_lowercase().contains(&needle)
                || row.description.to_lowercase().contains(&needle)
        })
        .collect()
}

/// The one search box: its query hits the installed list and the registry
/// together. The caret draws while the box has the keyboard.
fn render_search_box(ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    let focused = ui.skills.focus == SkillsFocus::Search;
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let mut spans = vec![
        Span::styled(" ⌕ ", Style::new().fg(token::GOLD)),
        Span::styled(ui.skills.query.clone(), Style::new().fg(token::TEXT)),
    ];
    if focused {
        spans.push(Span::styled("▌", Style::new().fg(token::TEXT)));
    }
    let right = match (ui.skills.searching, ui.skills.hits.len()) {
        (true, _) => "installed + registry · searching… ".to_string(),
        (false, 0) => "installed + registry ".to_string(),
        (false, n) => format!("installed + registry · web · {n} hits "),
    };
    let used: usize = spans.iter().map(Span::width).sum();
    let inner_w = area.width.saturating_sub(2) as usize;
    // `right` is fixed words plus a decimal count. Every glyph in it
    // (`·`, `…`) is one column, so `chars().count()` is already its width.
    let right_w = right.chars().count();
    if used + right_w < inner_w {
        spans.push(Span::raw(" ".repeat(inner_w - used - right_w)));
        spans.push(Span::styled(right, if focused { muted } else { dim }));
    }
    Paragraph::new(Line::from(spans))
        .block(rounded())
        .render(area, buf);
}

/// The installed section: a heading with the match count, then one row per
/// skill — enabled box, name, version, scope, description. Skills stella wrote
/// itself (origin `auto`) list under their own `learned from traces` heading.
fn render_installed(ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    let focused = ui.skills.focus == SkillsFocus::Installed;
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let rows = &ui.skills.view.rows;
    let shown = matching(ui);
    let learned: Vec<&(usize, &SkillRow)> =
        shown.iter().filter(|(_, r)| r.origin == "auto").collect();
    let authored: Vec<&(usize, &SkillRow)> =
        shown.iter().filter(|(_, r)| r.origin != "auto").collect();

    let mut head = vec![
        Span::styled(" installed", text),
        Span::styled(format!(" · {}", rows.len()), muted),
    ];
    if !ui.skills.query.trim().is_empty() {
        head.push(Span::styled(format!(" · {} match", shown.len()), muted));
    }
    let bands = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(area);
    Paragraph::new(Line::from(head)).render(bands[0], buf);
    let block = rounded();
    let inner = block.inner(bands[1]);
    block.render(bands[1], buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    if rows.is_empty() {
        let hint = if ui.skills.view.busy {
            "loading…"
        } else {
            "no skills installed — type to search the registry, ↵ installs"
        };
        Paragraph::new(hint)
            .style(muted)
            .alignment(Alignment::Center)
            .render(centered_row(inner), buf);
        return;
    }
    if shown.is_empty() {
        Paragraph::new("nothing installed matches")
            .style(muted)
            .alignment(Alignment::Center)
            .render(centered_row(inner), buf);
        return;
    }

    let sel = ui.skills.sel.min(rows.len() - 1);
    let width = inner.width as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();
    let budget = inner.height as usize;
    // Window the authored rows around the selection; the learned section
    // follows, and is itself windowed to what is left.
    let learned_rows = if learned.is_empty() {
        0
    } else {
        (learned.len() + 1).min(budget / 2)
    };
    let authored_budget = budget.saturating_sub(learned_rows).max(1);
    let sel_pos = authored.iter().position(|(i, _)| *i == sel).unwrap_or(0);
    let start = window_start(authored.len(), sel_pos, authored_budget);
    for (i, row) in authored.iter().skip(start).take(authored_budget) {
        lines.push(installed_row_line(row, *i == sel && focused, width));
    }
    if !learned.is_empty() && lines.len() < budget {
        lines.push(Line::from(vec![
            Span::styled(" learned from traces", text),
            Span::styled(format!(" · {}", learned.len()), muted),
            Span::styled("   stella wrote these after repeated wins", dim),
        ]));
        let room = budget.saturating_sub(lines.len());
        let lsel = learned.iter().position(|(i, _)| *i == sel).unwrap_or(0);
        let lstart = window_start(learned.len(), lsel, room.max(1));
        for (i, row) in learned.iter().skip(lstart).take(room) {
            lines.push(installed_row_line(row, *i == sel && focused, width));
        }
    }
    // The collapsed half of the reader/undo lifecycle: every rejection this
    // workspace has recorded is invisible otherwise, so this line is the
    // whole answer to "what have I rejected here?" until `!` expands it into
    // the names-and-dates review.
    let rejected = ui.skills.view.rejections.len();
    if rejected > 0 && lines.len() < budget {
        lines.push(Line::from(vec![
            Span::styled(format!(" {rejected} rejected"), text),
            Span::styled("   press ! to review · reverse one", dim),
        ]));
    }
    Paragraph::new(lines).render(inner, buf);
}

/// One row of the installed list: enabled box, name, version/scope or
/// provenance, description — fitted to `width` display columns.
///
/// A named function rather than a closure over `render_installed`'s locals,
/// so a test can compose one row and measure [`Line::width`] directly against
/// `width`, which is exactly what a CJK or emoji name, plugin id, or
/// description needs checked: none of them are text this crate authored.
fn installed_row_line(row: &SkillRow, is_sel: bool, width: usize) -> Line<'static> {
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let marker = if is_sel { "▸ " } else { "  " };
    let boxed = if row.enabled { "[x] " } else { "[ ] " };
    let box_style = if row.enabled {
        Style::new().fg(token::GOLD)
    } else {
        dim
    };
    let ver = if row.latest > row.version {
        format!("v{}/{}", row.version, row.latest)
    } else {
        format!("v{}", row.version)
    };
    // A contributed skill names its package instead of a version and a
    // scope: neither is a thing it has (`skill_manager::contributed_rows`),
    // and whose it is is the column a reader of this row actually needs.
    //
    // A learned row names the grade of the evidence that promoted it
    // (#4871) when the ledger still has one — the same `snake_case` token
    // `stella proposals` prints, so a reader learns one vocabulary rather
    // than two. Absent for a skill mined before the ledger existed, or
    // under the shipped lexical loop, which records no proposal at all —
    // there is no grade to name, not a missing feature.
    let meta = match (&row.contributed_by, row.origin.as_str()) {
        (Some(plugin), _) => format!(" via {plugin} "),
        (None, "auto") => match &row.evidence_grade {
            Some(grade) => format!(" learned · {grade} "),
            None => " learned ".to_string(),
        },
        (None, _) => format!(" {ver} · {} ", row.scope.label()),
    };
    // `columns::pad`, not `{:<24}`. `row.name` is a skill's own name,
    // chosen by a user or by `stella` itself. Rust's own pad fills by
    // char, so a CJK or emoji name overshoots the 24-column target.
    let name = columns::pad(&row.name, 24);
    let used = columns::width(marker)
        + columns::width(boxed)
        + columns::width(&name)
        + columns::width(&meta);
    let desc_room = width.saturating_sub(used + 3);
    // A learned row spends its last column on **provenance** rather than
    // on a description (SPEC 9.2), because provenance carries the turn and
    // the identity `r rename` has to keep, and the description does not.
    //
    // That trade used to be free: the description a mined skill carried
    // was `Learned from N observations.`, the trace count said less
    // precisely. Since #5335 it is the lesson's own first sentence, so
    // there is now a real thing on the other side of the choice — #5425
    // is where whether this column should prefer it gets decided. The
    // full prose is in the body either way, one `ctrl+o` away. *This*
    // column and not a right-aligned one: the right edge of an installed
    // row is where per-skill economics land (#4337).
    let tail = provenance(row).unwrap_or_else(|| row.description.clone());
    let desc = if desc_room >= 6 && !tail.is_empty() {
        format!("  {}", columns::head(&tail, desc_room))
    } else {
        String::new()
    };
    let mut line = Line::from(vec![
        Span::styled(marker, Style::new().fg(token::GOLD)),
        Span::styled(boxed, box_style),
        Span::styled(name, text),
        Span::styled(
            meta,
            if row.origin == "auto" {
                Style::new().fg(token::GOLD)
            } else {
                muted
            },
        ),
        Span::styled(desc, muted),
    ]);
    if is_sel {
        line.style = Style::new().bg(token::HL);
    }
    line
}

/// SPEC 9.2's learned-skill provenance line — `from N traces · turn M ·
/// was <hash>` — or `None` for a row that is not a learned skill.
///
/// Every segment is dropped when the fact behind it is missing, rather than
/// printed with a placeholder: a skill mined before the turn was recorded has
/// no turn, and inventing one would make the line a worse answer than the
/// shorter true one. The order never changes, so the eye learns one shape.
pub(crate) fn provenance(row: &SkillRow) -> Option<String> {
    let learned = row.learned.as_ref()?;
    let mut parts: Vec<String> = Vec::with_capacity(3);
    if learned.traces > 0 {
        let plural = if learned.traces == 1 { "" } else { "s" };
        parts.push(format!("from {} trace{plural}", learned.traces));
    }
    if let Some(turn) = learned.turn {
        parts.push(format!("turn {turn}"));
    }
    if !learned.was.is_empty() {
        parts.push(format!("was {}", learned.was));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The registry section: the last search's hits, each with its install count,
/// and the install affordance on the selected one.
fn render_search(ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    let focused = ui.skills.focus == SkillsFocus::Search;
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let bands = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
    let mut head = vec![
        Span::styled(" registry", text),
        Span::styled(" · web", muted),
    ];
    if ui.skills.searching {
        head.push(Span::styled(" · searching…", muted));
    } else if !ui.skills.hits.is_empty() {
        head.push(Span::styled(
            format!(" · {} results", ui.skills.hits.len()),
            muted,
        ));
    } else {
        head.push(Span::styled(" · type a term, ↵ searches", dim));
    }
    Paragraph::new(Line::from(head)).render(bands[0], buf);
    if bands[1].height == 0 || ui.skills.hits.is_empty() {
        return;
    }
    let block = rounded();
    let inner = block.inner(bands[1]);
    block.render(bands[1], buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let visible = (inner.height as usize).saturating_sub(1).max(1);
    let sel = ui
        .skills
        .search_sel
        .min(ui.skills.hits.len().saturating_sub(1));
    let start = window_start(ui.skills.hits.len(), sel, visible);
    // The most-installed hit anchors the popularity bar's full width.
    let peak = ui
        .skills
        .hits
        .iter()
        .map(|h| h.installs_rank)
        .max()
        .unwrap_or(0);
    let width = inner.width as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, hit) in ui.skills.hits.iter().enumerate().skip(start).take(visible) {
        let is_sel = i == sel && focused;
        let marker = if is_sel { "▸ " } else { "  " };
        let bar = popularity_bar(hit.installs_rank, peak);
        let metric = hit.installs.clone();
        let install = if is_sel { "↵ install" } else { "" };
        // `metric` and the id below come from a registry, not this crate.
        // A foreign registry can format an install count or an owner/repo
        // id however it likes. So every width here is a display column,
        // never `marker.len()`'s byte count or a plain `.chars().count()`.
        let right_w = columns::width(&bar)
            + usize::from(!bar.is_empty())
            + columns::width(&metric)
            + usize::from(!metric.is_empty())
            + columns::width(install)
            + 2;
        let marker_w = columns::width(marker);
        let name = truncate_skill_id(&hit.id, width.saturating_sub(marker_w + right_w).max(4));
        let pad = width
            .saturating_sub(marker_w + columns::width(&name) + right_w)
            .max(1);
        let mut spans = vec![
            Span::styled(marker, Style::new().fg(token::GOLD)),
            Span::styled(name, text),
            Span::raw(" ".repeat(pad)),
        ];
        if !bar.is_empty() {
            spans.push(Span::styled(
                format!("{bar} "),
                Style::new().fg(token::GOLD),
            ));
        }
        if !metric.is_empty() {
            spans.push(Span::styled(format!("{metric} "), muted));
        }
        spans.push(Span::styled(install, Style::new().fg(token::GOLD)));
        let mut line = Line::from(spans);
        if is_sel {
            line.style = Style::new().bg(token::HL);
        }
        lines.push(line);
    }
    lines.push(Line::from(Span::styled(
        " installs land disabled until you preview and enable",
        dim,
    )));
    Paragraph::new(lines).render(inner, buf);
}

/// The `▰▱`-style micro-bar for a hit's install count relative to the
/// most-installed hit in the result set. Empty when there is no signal.
fn popularity_bar(rank: u64, peak: u64) -> String {
    if peak == 0 || rank == 0 {
        return String::new();
    }
    // 1..=4 filled blocks, scaled against the peak.
    let filled = (((rank as f64 / peak as f64) * 4.0).round() as usize).clamp(1, 4);
    let mut s = String::with_capacity(4 * 3);
    for _ in 0..filled {
        s.push('▰');
    }
    for _ in filled..4 {
        s.push('▱');
    }
    s
}

/// The bottom two rows: the keys (or the transient status), then the counts.
fn render_status(ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    // The learned lifecycle's verbs (SPEC 9.2) belong to learned rows and to
    // nothing else, so the hint line follows the selection rather than listing
    // every key the tab has ever had. That also keeps it on one line: `r` and
    // `x` are two more keys, and the installed line was already full.
    //
    // Keyed on the provenance and not on `origin == "auto"`, because that is
    // what the verbs themselves are keyed on: a hand-written `origin: auto`
    // file with no mined identity and no evidence gets no provenance, and `r`
    // and `x` refuse it — so advertising them here would be a hint that lies.
    let on_learned = ui
        .skills
        .view
        .rows
        .get(ui.skills.sel)
        .is_some_and(|row| row.learned.is_some());
    let keys: &[(&str, &str)] = match ui.skills.focus {
        SkillsFocus::Installed if on_learned => &[
            ("space", "on/off"),
            ("ctrl+o", "source traces"),
            ("r", "rename"),
            ("x", "reject"),
            ("e", "edit"),
            ("→", "search"),
        ],
        SkillsFocus::Installed => &[
            ("space", "on/off"),
            ("ctrl+o", "preview"),
            ("e", "edit"),
            ("p", "pin"),
            ("n", "new from prompt"),
            ("ctrl+x ctrl+x", "delete"),
            ("→", "search"),
        ],
        SkillsFocus::Search => &[
            ("type", "query"),
            ("↵", "search / install"),
            ("↑↓", "pick"),
            ("ctrl+o", "preview"),
            ("←", "installed"),
        ],
    };
    let first = match &ui.skills.status {
        Some(status) => Line::from(Span::styled(
            format!(" {status}"),
            Style::new().fg(token::GOLD),
        )),
        None => {
            let mut spans = vec![Span::raw(" ")];
            for (i, (k, desc)) in keys.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" · ", dim));
                }
                spans.push(Span::styled((*k).to_string(), muted));
                spans.push(Span::styled(format!(" {desc}"), dim));
            }
            Line::from(spans)
        }
    };
    let rows = &ui.skills.view.rows;
    let learned = rows.iter().filter(|r| r.origin == "auto").count();
    let enabled = rows.iter().filter(|r| r.enabled).count();
    let counts = Line::from(vec![
        Span::styled(format!(" {} installed", rows.len()), muted),
        Span::styled(format!(" · {learned} learned"), muted),
        Span::styled(format!(" · {enabled} enabled"), muted),
    ]);
    Paragraph::new(vec![first, counts]).render(area, buf);
}

/// Keep `sel` visible in a window of `visible` rows over `len` items.
fn window_start(len: usize, sel: usize, visible: usize) -> usize {
    if len <= visible {
        return 0;
    }
    sel.saturating_sub(visible.saturating_sub(1) / 2)
        .min(len - visible)
}

/// The single centered row of an inner area, for a one-line hint.
fn centered_row(inner: Rect) -> Rect {
    let y = inner.y + inner.height.saturating_sub(1) / 2;
    Rect::new(inner.x, y, inner.width, 1)
}

/// Truncate an `owner/repo@skill` id to `max` columns, preferring to keep the
/// `@skill` segment (the most identifying part) whole — the owner/repo prefix
/// gives way to an ellipsis first. Falls back to a plain tail-ellipsis when
/// even `@skill` cannot fit.
fn truncate_skill_id(id: &str, max: usize) -> String {
    if columns::width(id) <= max || max == 0 {
        return columns::head(id, max);
    }
    if let Some(at) = id.rfind('@') {
        let skill = &id[at..]; // "@skill"
        let skill_w = columns::width(skill);
        // Keep the whole @skill tail plus an ellipsis, filling the rest with the
        // head of owner/repo — but only when that leaves real owner context.
        if skill_w + 2 <= max {
            let owner_room = max - skill_w - 1; // room minus the ellipsis
            let owner_head = columns::take_left(&id[..at], owner_room);
            return format!("{owner_head}…{skill}");
        }
    }
    columns::head(id, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck_ui::{ScopeAction, SkillPreview, SkillPrompt};
    use crate::envelope::{
        LearnedProvenance, LearnedSource, RejectedSkillRow, SkillRow, SkillScope, SkillSearchHit,
        SkillsView,
    };
    use crate::theme;

    fn buffer_text(buf: &Buffer) -> String {
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

    /// The style of the first cell of the first occurrence of `needle`.
    fn style_at(buf: &Buffer, needle: &str) -> Style {
        let area = *buf.area();
        let want: Vec<String> = needle.chars().map(|c| c.to_string()).collect();
        for y in 0..area.height {
            'col: for x in 0..area.width {
                for (k, w) in want.iter().enumerate() {
                    if buf.cell((x + k as u16, y)).map(|c| c.symbol()) != Some(w.as_str()) {
                        continue 'col;
                    }
                }
                return buf.cell((x, y)).expect("cell in area").style();
            }
        }
        panic!("{needle:?} not on screen");
    }

    fn row(name: &str, scope: SkillScope, enabled: bool, version: u32, latest: u32) -> SkillRow {
        SkillRow {
            scope,
            name: name.to_string(),
            description: format!("{name} does a thing"),
            body: format!("body of {name}"),
            origin: "workspace".to_string(),
            evidence_grade: None,
            learned: None,
            enabled,
            version,
            latest,
            removable: true,
            contributed_by: None,
        }
    }

    /// A learned (`origin: auto`) row, optionally carrying the evidence grade
    /// the SKILLS tab looked up for it (#4871).
    fn learned_row(name: &str, evidence_grade: Option<&str>) -> SkillRow {
        SkillRow {
            scope: SkillScope::Project,
            name: name.to_string(),
            description: format!("{name} does a thing"),
            body: format!("body of {name}"),
            origin: "auto".to_string(),
            evidence_grade: evidence_grade.map(str::to_string),
            learned: None,
            enabled: true,
            version: 1,
            latest: 1,
            removable: true,
            contributed_by: None,
        }
    }

    /// The same, carrying the provenance the driver assembles for it (#5046):
    /// `traces` source traces, learned on `turn`, minted as `<slug>-<was>`.
    fn traced_row(name: &str, traces: u32, turn: Option<u64>, was: &str) -> SkillRow {
        SkillRow {
            learned: Some(LearnedProvenance {
                traces,
                turn,
                was: was.to_string(),
                sources: (0..traces)
                    .map(|i| LearnedSource {
                        reference: format!("reflection:{}", 1000 + i),
                        observed_at: u64::from(1000 + i),
                        snippet: format!("the number {i} time this happened"),
                    })
                    .collect(),
            }),
            ..learned_row(name, None)
        }
    }

    #[test]
    fn installed_pane_shows_rows_with_enabled_box_and_version() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        ui.skills.view = SkillsView {
            rows: vec![
                row("sql-style", SkillScope::Project, true, 2, 3),
                row("pdf-extract", SkillScope::User, false, 1, 1),
            ],
            status: None,
            rejections: vec![],
            busy: false,
            created: None,
        };
        let area = Rect::new(0, 0, 120, 12);
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("sql-style"), "{text}");
        assert!(text.contains("[x]"), "enabled box:\n{text}");
        assert!(text.contains("[ ]"), "disabled box:\n{text}");
        assert!(text.contains("v2/3"), "pinned-older version shown:\n{text}");
        assert!(text.contains("pdf-extract"), "{text}");
    }

    /// A learned skill's row names the grade of the evidence that promoted it
    /// (#4871), and two skills promoted from different-strength evidence read
    /// as different rows rather than both saying only "learned". A learned
    /// row with no grade at all (mined before the ledger existed, or under
    /// the shipped lexical loop) still reads plainly as "learned".
    #[test]
    fn learned_rows_name_their_evidence_grade_and_differ_by_it() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        ui.skills.view = SkillsView {
            rows: vec![
                learned_row("from-build-failures", Some("environment_observation")),
                learned_row("from-model-opinion", Some("model_critique")),
            ],
            status: None,
            rejections: vec![],
            busy: false,
            created: None,
        };
        let area = Rect::new(0, 0, 120, 20);
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("learned · environment_observation"), "{text}");
        assert!(text.contains("learned · model_critique"), "{text}");
    }

    /// **Witness (#5046).** SPEC 9.2's provenance reaches the screen whole:
    /// the trace count, the turn it was learned on, and the mined `<hash>` a
    /// rename has to keep — in that order, on the learned row itself.
    #[test]
    fn a_learned_row_shows_its_traces_turn_and_mined_hash() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        ui.skills.view = SkillsView {
            rows: vec![traced_row("money-is-minor-units", 4, Some(37), "a1b2c3d4")],
            status: None,
            rejections: vec![],
            busy: false,
            created: None,
        };
        let area = Rect::new(0, 0, 120, 20);
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(
            text.contains("from 4 traces · turn 37 · was a1b2c3d4"),
            "the whole provenance line, in SPEC 9.2's order:\n{text}"
        );
    }

    /// **The witness.** A workspace with rejections shows the collapsed count
    /// and the `!` hint under the installed section; one with none shows
    /// neither — the line must not read "0 rejected" at every empty
    /// workspace.
    #[test]
    fn the_installed_section_shows_a_collapsed_rejected_count() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        ui.skills.view = SkillsView {
            rows: vec![row("sql-style", SkillScope::Project, true, 1, 1)],
            rejections: vec![
                RejectedSkillRow {
                    scope: SkillScope::Project,
                    name: "bench-rig-access".to_string(),
                    mined_as: "bench-rig-access-a1b2c3d4".to_string(),
                    rejected_at: 1,
                },
                RejectedSkillRow {
                    scope: SkillScope::User,
                    name: "prefer-tables".to_string(),
                    mined_as: "prefer-tables-deadbeef".to_string(),
                    rejected_at: 2,
                },
            ],
            status: None,
            busy: false,
            created: None,
        };
        let area = Rect::new(0, 0, 120, 20);
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("2 rejected"), "{text}");
        assert!(text.contains("press ! to review"), "{text}");

        ui.skills.view.rejections.clear();
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(
            !text.contains("rejected") && !text.contains("press !"),
            "an empty workspace shows no rejected line at all:\n{text}"
        );
    }

    /// Each segment is dropped when the fact behind it is missing rather than
    /// filled with a placeholder — a skill mined before the turn was recorded
    /// has no turn, and `turn 0` would be a turn that never happened.
    #[test]
    fn provenance_omits_the_segments_it_has_no_answer_for() {
        let no_turn = traced_row("mined-before-turns", 2, None, "deadbeef");
        assert_eq!(
            provenance(&no_turn).as_deref(),
            Some("from 2 traces · was deadbeef")
        );
        let one = traced_row("single-trace", 1, Some(3), "0badcafe");
        assert_eq!(
            provenance(&one).as_deref(),
            Some("from 1 trace · turn 3 · was 0badcafe"),
            "one trace is not `1 traces`"
        );
        let no_traces = traced_row("evidence-was-edited-away", 0, Some(9), "12345678");
        assert_eq!(
            provenance(&no_traces).as_deref(),
            Some("turn 9 · was 12345678")
        );
        assert_eq!(
            provenance(&row("hand-written", SkillScope::Project, true, 1, 1)),
            None,
            "a skill nobody mined has no provenance to show"
        );
    }

    /// The provenance takes the description column, and the row keeps
    /// everything else it had — the enabled box, the name, and the `learned`
    /// tag that puts it in this section at all.
    #[test]
    fn provenance_replaces_the_description_only_on_learned_rows() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        ui.skills.view = SkillsView {
            rows: vec![
                row("sql-style", SkillScope::Project, true, 1, 1),
                traced_row("money-is-minor-units", 4, Some(37), "a1b2c3d4"),
            ],
            status: None,
            rejections: vec![],
            busy: false,
            created: None,
        };
        let area = Rect::new(0, 0, 120, 20);
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(
            text.contains("sql-style does a thing"),
            "an authored row keeps its description:\n{text}"
        );
        assert!(
            !text.contains("money-is-minor-units does a thing"),
            "a learned row spends that column on provenance instead:\n{text}"
        );
        assert!(text.contains("learned"), "still tagged learned:\n{text}");
        assert!(text.contains("[x]"), "still has its enabled box:\n{text}");
    }

    /// The hint line follows the selection: a learned row is the only place
    /// `r` and `x` do anything, so it is the only place they are advertised.
    #[test]
    fn the_key_hints_name_the_learned_verbs_only_on_a_learned_row() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        ui.skills.view = SkillsView {
            rows: vec![
                row("sql-style", SkillScope::Project, true, 1, 1),
                traced_row("money-is-minor-units", 4, Some(37), "a1b2c3d4"),
            ],
            status: None,
            rejections: vec![],
            busy: false,
            created: None,
        };
        let area = Rect::new(0, 0, 120, 20);

        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let authored = buffer_text(&buf);
        assert!(
            authored.contains("ctrl+x ctrl+x delete"),
            "an authored row offers delete:\n{authored}"
        );
        assert!(
            !authored.contains("r rename"),
            "and not the learned verbs:\n{authored}"
        );

        ui.skills.sel = 1;
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let learned = buffer_text(&buf);
        assert!(learned.contains("r rename"), "{learned}");
        assert!(learned.contains("x reject"), "{learned}");
        assert!(
            learned.contains("ctrl+o source traces"),
            "ctrl+o is renamed on a learned row, because that is what it \
             opens on:\n{learned}"
        );
    }

    /// The hints key off the provenance, not off `origin == "auto"`. A
    /// hand-written `origin: auto` file with no mined identity and no evidence
    /// is a row `r` and `x` both refuse, so neither may be advertised on it —
    /// the hint line and the verbs have to agree about what a learned row is.
    #[test]
    fn a_row_with_no_provenance_is_not_offered_the_learned_verbs() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        ui.skills.view = SkillsView {
            // `origin: auto`, but nothing ever mined it: no traces, no sidecar.
            rows: vec![learned_row("hand-written-auto", None)],
            status: None,
            rejections: vec![],
            busy: false,
            created: None,
        };
        let area = Rect::new(0, 0, 120, 20);
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert_eq!(ui.skills.view.rows[0].origin, "auto", "the premise");
        assert!(
            !text.contains("r rename") && !text.contains("x reject"),
            "verbs that would refuse this row must not be advertised on \
             it:\n{text}"
        );
    }

    #[test]
    fn rename_overlay_promises_the_provenance_survives() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        ui.skills.prompt = Some(SkillPrompt::Rename {
            scope: SkillScope::Project,
            name: "money-is-minor-units-a1b2c3d4".into(),
            buffer: "money-is-minor-units".into(),
            was: "a1b2c3d4".into(),
        });
        let area = Rect::new(0, 0, 90, 16);
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("rename learned skill"), "{text}");
        assert!(
            text.contains("keeps its provenance · was a1b2c3d4"),
            "the dialog says the hash survives, on screen:\n{text}"
        );
        assert!(
            text.contains("⏎ rename · esc cancel"),
            "the keys ride the bottom rule:\n{text}"
        );
    }

    #[test]
    fn empty_installed_pane_hints_at_search() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 100, 10);
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        assert!(buffer_text(&buf).contains("no skills installed"));
    }

    /// Every box this tab draws is a section box, and none of them is a frame
    /// around the whole tab: the deck's own frame already carved the content
    /// area out, and a second border around it would read as a nested pane.
    #[test]
    fn the_tab_draws_no_border_around_its_whole_area() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 100, 20);
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let last = area.height - 1;
        let bottom: String = (0..area.width)
            .map(|x| buf.cell((x, last)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        assert!(
            !bottom.contains('─') && !bottom.contains('╰'),
            "the last row is content, not a frame: {bottom:?}"
        );
    }

    fn hit(id: &str, installs: &str, rank: u64) -> SkillSearchHit {
        SkillSearchHit {
            id: id.to_string(),
            installs: installs.to_string(),
            installs_rank: rank,
            url: format!("https://skills.sh/{}", id.replace('@', "/")),
        }
    }

    #[test]
    fn search_pane_shows_clean_name_and_installs_no_ansi_leak() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        ui.skills.focus = SkillsFocus::Search;
        ui.skills.query = "rust".into();
        ui.skills.query_dirty = false;
        ui.skills.hits = vec![
            hit(
                "wshobson/agents@rust-async-patterns",
                "15.8K installs",
                15800,
            ),
            hit(
                "apollographql/skills@rust-best-practices",
                "13.9K installs",
                13900,
            ),
        ];
        // A realistic deck width; the search pane is 45% of it.
        let area = Rect::new(0, 0, 120, 14);
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        // The identifying `@skill` segment survives truncation (owner/repo gives
        // way first), and the installs metric shows.
        assert!(
            text.contains("@rust-async-patterns"),
            "skill segment shown:\n{text}"
        );
        assert!(text.contains("15.8K installs"), "installs shown:\n{text}");
        // The whole point: no raw ANSI / SGR codes leak into the rendered list.
        assert!(!text.contains("[38;5"), "no raw ANSI escapes:\n{text}");
        assert!(!text.contains("[0m"), "no raw reset codes:\n{text}");
    }

    #[test]
    fn scope_overlay_lists_both_destinations() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        ui.skills.prompt = Some(SkillPrompt::Scope {
            action: crate::deck_ui::ScopeAction::Install {
                id: "acme/auth".into(),
            },
            user: false,
        });
        let area = Rect::new(0, 0, 90, 16);
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("Project"), "{text}");
        assert!(text.contains("User"), "{text}");
        assert!(text.contains("acme/auth"), "{text}");
        assert!(
            text.contains("⏎ confirm · esc cancel"),
            "the keys ride the dialog's bottom rule:\n{text}"
        );
    }

    #[test]
    fn edit_overlay_highlights_markdown_source() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        ui.skills.prompt = Some(SkillPrompt::Edit {
            scope: SkillScope::Project,
            name: "writer".into(),
            buffer: "---\nname: writer\n---\n# Usage\nplain prose".into(),
        });
        let area = Rect::new(0, 0, 90, 16);
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("# Usage"), "content on screen:\n{text}");
        assert_eq!(
            style_at(&buf, "# Usage").fg,
            Some(theme::SYNTAX_KEYWORD),
            "headings light up in the edit overlay"
        );
        assert_eq!(
            style_at(&buf, "name:").fg,
            Some(theme::SYNTAX_KEYWORD),
            "frontmatter keys light up"
        );
        assert_eq!(
            style_at(&buf, "plain prose").fg,
            Some(token::TEXT),
            "prose keeps the body tone"
        );
    }

    #[test]
    fn creating_overlay_shows_an_animated_spinner_until_completion() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        ui.skills.prompt = Some(SkillPrompt::Creating {
            description: "extract tables from pdfs".into(),
            scope: SkillScope::Project,
        });
        let area = Rect::new(0, 0, 90, 16);

        // Two deck-clock instants one spinner period apart render DIFFERENT
        // glyphs — the spinner genuinely animates off the tick loop.
        let mut model = WorkspaceModel::new();
        model.now_ms = 0;
        let mut buf_a = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf_a);
        let text_a = buffer_text(&buf_a);
        assert!(text_a.contains("Creating the skill"), "{text_a}");
        assert!(
            text_a.contains("extract tables from pdfs"),
            "the description stays visible:\n{text_a}"
        );

        model.now_ms = 80; // one spinner frame later
        let mut buf_b = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf_b);
        assert_ne!(
            buffer_text(&buf_a),
            buffer_text(&buf_b),
            "the spinner must advance with the deck clock"
        );

        // `--no-anim` pins the glyph: consecutive ticks render identically.
        ui.no_anim = true;
        let mut buf_c = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf_c);
        model.now_ms = 160;
        let mut buf_d = Buffer::empty(area);
        render(&model, &mut ui, area, &mut buf_d);
        assert_eq!(buffer_text(&buf_c), buffer_text(&buf_d));
    }

    #[test]
    fn create_failed_overlay_shows_the_error() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        ui.skills.prompt = Some(SkillPrompt::CreateFailed {
            error: "the model did not return a valid SKILL.md — try again".into(),
        });
        let area = Rect::new(0, 0, 90, 16);
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("skill creation failed"), "{text}");
        assert!(
            text.contains("did not return a valid SKILL.md"),
            "the driver's error is on screen:\n{text}"
        );
    }

    /// Both pickers select the way the tab body does: the `▸` glyph **and**
    /// the [`token::HL`] tint. The goldens strip style, so the tint half has
    /// no golden that can see it and is asserted here instead.
    #[test]
    fn the_pickers_select_with_a_glyph_and_a_background_tint() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        let area = Rect::new(0, 0, 100, 20);

        ui.skills.prompt = Some(SkillPrompt::Scope {
            action: ScopeAction::Create {
                description: "extract tables from pdfs".into(),
            },
            user: false,
        });
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("▸ [p] Project"), "scope marker:\n{text}");
        assert_eq!(
            style_at(&buf, "[p] Project").bg,
            Some(token::HL),
            "the chosen scope carries the tint, not the glyph alone:\n{text}"
        );
        assert_ne!(
            style_at(&buf, "[u] User").bg,
            Some(token::HL),
            "the unchosen scope carries neither half:\n{text}"
        );

        ui.skills.prompt = Some(SkillPrompt::Pin {
            scope: SkillScope::Project,
            name: "rust-review".into(),
            latest: 3,
            sel: 2,
        });
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("▸ v2"), "pin marker:\n{text}");
        assert_eq!(
            style_at(&buf, "v2").bg,
            Some(token::HL),
            "the pinned version carries the tint too:\n{text}"
        );
        assert_ne!(
            style_at(&buf, "v1").bg,
            Some(token::HL),
            "the other versions carry neither half:\n{text}"
        );
    }

    #[test]
    fn preview_overlay_renders_markdown_heading_and_body() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        ui.skills.preview = Some(SkillPreview {
            title: "acme/auth@oauth".into(),
            subtitle: "https://skills.sh/acme/auth/oauth".into(),
            pending: None,
            body: Some("# OAuth Guide\n\nAlways use PKCE for public clients.".into()),
            scroll: 0,
        });
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("acme/auth@oauth"), "title in border:\n{text}");
        assert!(text.contains("OAuth Guide"), "rendered heading:\n{text}");
        assert!(text.contains("PKCE"), "rendered body:\n{text}");
    }

    #[test]
    fn preview_overlay_shows_loading_state_when_body_absent() {
        let mut ui = DeckUi {
            tab: crate::deck::DeckTab::Skills,
            ..Default::default()
        };
        ui.skills.preview = Some(SkillPreview {
            title: "x/y@z".into(),
            subtitle: String::new(),
            pending: Some("x/y@z".into()),
            body: None,
            scroll: 0,
        });
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        render(&WorkspaceModel::new(), &mut ui, area, &mut buf);
        assert!(
            buffer_text(&buf).contains("fetching"),
            "loading state shown"
        );
    }

    /// A CJK skill name stays inside the installed row's width budget.
    ///
    /// Old code used `{:<24}`, which fills by char. A name of 12 wide
    /// glyphs is already 24 columns, but old code still added
    /// `24 - 12 = 12` more chars of fill — 36 columns for a 24-column
    /// budget. Checked with `Line::width()`, backed by [`unicode_width`]
    /// directly, never through `render::columns` itself.
    #[test]
    fn a_wide_character_name_keeps_the_installed_row_inside_its_width() {
        use unicode_width::UnicodeWidthStr;

        let wide_name = "圈".repeat(12);
        assert_eq!(
            UnicodeWidthStr::width(wide_name.as_str()),
            24,
            "the fixture: 12 double-width glyphs, 24 columns, 12 chars"
        );
        let row = row(&wide_name, SkillScope::Project, true, 1, 1);
        // Tight enough that the old pad's 36-column name alone overruns
        // it, before the description even joins the row. The fixed part
        // of the new, correct row is only 44 columns.
        let width = 55;
        let line = installed_row_line(&row, false, width);
        assert!(
            line.width() <= width,
            "row overran its {width}-column budget: {line:?}"
        );
    }

    /// The same fixture, but checking what a person would see: an
    /// over-wide name pushes the row's later cells outward, so
    /// `desc_room` comes up short. At this 55-column width, a char-spent
    /// `desc_room` is 0 — below the 6-column floor — so no description
    /// shows at all. Spent in columns, `desc_room` is 8, and it shows.
    #[test]
    fn a_wide_character_name_still_leaves_room_for_the_description() {
        let wide_name = "圈".repeat(12);
        let row = row(&wide_name, SkillScope::Project, true, 1, 1);
        let line = installed_row_line(&row, false, 55);
        let rendered: String = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            rendered.contains("does a thing") || rendered.contains('…'),
            "the description column should still show something at a \
             60-column width: {rendered:?}"
        );
    }

    /// A registry hit's `owner/repo@skill` id can hold non-ASCII text.
    /// This crate does not control a foreign registry. The elide must stay
    /// inside its column budget, not its char budget.
    ///
    /// Old code used `id.chars().count()` and `.chars().take()`. An id can
    /// sit well under its char budget but over its column budget, and old
    /// code would then keep it whole.
    #[test]
    fn truncate_skill_id_spends_its_budget_in_columns() {
        use unicode_width::UnicodeWidthStr;

        let id = format!("acme/{}@extract", "文".repeat(5));
        assert!(
            id.chars().count() < 20,
            "the fixture: well under 20 chars, comfortably over 20 columns"
        );
        assert!(
            UnicodeWidthStr::width(id.as_str()) > 20,
            "the fixture: over 20 columns"
        );
        let cut = truncate_skill_id(&id, 20);
        assert!(
            UnicodeWidthStr::width(cut.as_str()) <= 20,
            "elided id overran its 20-column budget: {cut:?}"
        );
        assert!(
            cut.ends_with("@extract"),
            "the @skill tail is kept whole: {cut:?}"
        );
    }
}
