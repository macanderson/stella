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
//! │  [x] bench-rig-access         learned       Reach the rig and read a reward.    │
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
//! uninstall, edit, pin — with the skills stella learned from its own traces in
//! their own section) and the **registry** (`npx skills find` → install). ←/→
//! move the keyboard between the two. The driver owns the skills on disk (both
//! scopes), their enabled/version/pin state, and the npx registry; this module
//! renders the [`crate::envelope::SkillsView`] read-model it pushes, and
//! [`overlays`] draws the scope / create / edit / pin dialogs and the `ctrl+o`
//! preview over the top. The content is a deterministic function of
//! `(ui.skills)`, so buffer tests stay byte-stable.
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

/// Draw the tab into `area`, then float whichever dialog is open above it.
pub fn render(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    let hits = ui.skills.hits.len();
    let registry_h = if ui.skills.focus == SkillsFocus::Search || hits > 0 || ui.skills.searching {
        (hits as u16 + 4).clamp(4, area.height / 2)
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
/// the whole tab — [`crate::v2::frame`] already carved the content area out.
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
    if used + right.chars().count() < inner_w {
        spans.push(Span::raw(
            " ".repeat(inner_w - used - right.chars().count()),
        ));
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
    let row_line = |(i, row): &(usize, &SkillRow)| -> Line<'static> {
        let is_sel = *i == sel && focused;
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
        let meta = if row.origin == "auto" {
            " learned ".to_string()
        } else {
            format!(" {ver} · {} ", row.scope.label())
        };
        let name = format!("{:<24}", row.name);
        let used = marker.chars().count()
            + boxed.chars().count()
            + name.chars().count()
            + meta.chars().count();
        let desc_room = width.saturating_sub(used + 3);
        let desc = if desc_room >= 6 && !row.description.is_empty() {
            format!("  {}", truncate(&row.description, desc_room))
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
    };

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
    for entry in authored.iter().skip(start).take(authored_budget) {
        lines.push(row_line(entry));
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
        for entry in learned.iter().skip(lstart).take(room) {
            lines.push(row_line(entry));
        }
    }
    Paragraph::new(lines).render(inner, buf);
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
        let right_w = bar.chars().count()
            + usize::from(!bar.is_empty())
            + metric.chars().count()
            + usize::from(!metric.is_empty())
            + install.chars().count()
            + 2;
        let name = truncate_skill_id(&hit.id, width.saturating_sub(marker.len() + right_w).max(4));
        let pad = width
            .saturating_sub(marker.len() + name.chars().count() + right_w)
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
    let keys: &[(&str, &str)] = match ui.skills.focus {
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
    if id.chars().count() <= max || max == 0 {
        return truncate(id, max);
    }
    if let Some(at) = id.rfind('@') {
        let skill = &id[at..]; // "@skill"
        let skill_w = skill.chars().count();
        // Keep the whole @skill tail plus an ellipsis, filling the rest with the
        // head of owner/repo — but only when that leaves real owner context.
        if skill_w + 2 <= max {
            let owner_room = max - skill_w - 1; // room minus the ellipsis
            let owner_head: String = id[..at].chars().take(owner_room).collect();
            return format!("{owner_head}…{skill}");
        }
    }
    truncate(id, max)
}

/// Truncate to `max` chars with a trailing ellipsis, char-safe.
fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deck_ui::{SkillPreview, SkillPrompt};
    use crate::envelope::{SkillRow, SkillScope, SkillSearchHit, SkillsView};
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
            enabled,
            version,
            latest,
            removable: true,
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
}
