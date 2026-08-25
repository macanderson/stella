// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The GRAPH tab — SPEC 9.1, rendering `04-graph`:
//!
//! ```text
//! ╭ ⌕ file:crates/stella-protocol/src/lib.rs        / files   438 nodes · det ╮
//! nodes · 27 in file
//! ╭──────────────────────╮ ╭ ▤ crates/stella-protocol/src/lib.rs   ● hot ───╮
//! │ ▤ lib.rs       ● hot │ │ imports 24 → · ← imported-by 12 · tests 6      │
//! │ ▢ Attachment      18 │ │ → attachment::Attachment                       │
//! │ ƒ human_bytes      3 │ │ ← stella-cli::self_driving_cmd                 │
//! │ ⋯ 16 more            │ ╰────────────────────────────────────────────────╯
//! │ ▤ file · ▢ type · ƒ fn│ ╭ coupling · neighbors by edge count ───────────╮
//! │ right column = edges │ │ attachment::Attachment ████████  18            │
//! ╰──────────────────────╯ ╰────────────────────────────────────────────────╯
//! ↵ open file · / files · ⇥ panes
//! every answer here is deterministic · $0.00
//! ```
//!
//! Renders from [`crate::deck_ui::DeckUi::graph`], the out-of-band
//! [`GraphSnapshot`], plus the focused lane's file ledger for the `● hot`
//! mark — a node is hot when this session changed the file it lives in.
//! Every number is a count of edges the index already holds; no model is
//! consulted, which is what the footer's `$0.00` states.
//!
//! The rendering's `12ms` is [`GraphSnapshot::query_ms`], measured by the
//! driver and drawn only when it is there — a demo snapshot nobody timed
//! carries `None` and the bar simply omits it.
//!
//! The renderings' `q free-form query` is the query bar's second mode. `q`
//! opens a modal box on this tab; `⏎` sends the text as a
//! [`crate::envelope::WorkspaceInput::GraphQuery`], and the driver answers
//! with a snapshot whose [`GraphSnapshot::query`] echoes what was asked. The
//! bar reads `q:<text>` in that mode and `file:<focus>` otherwise, so it
//! always names which of the two produced the neighborhood on screen
//! (#4335).

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};
use stella_tui_theme::{glyph, token};

use crate::graph::{GraphNode, GraphSnapshot};

/// The SPEC 4 glyph for a node kind: file, type, fn — anything else a plain
/// bullet.
#[must_use]
pub fn kind_glyph(kind: &str) -> char {
    match kind {
        "file" | "module" => glyph::NODE_FILE,
        "struct" | "enum" | "trait" | "type" => glyph::NODE_TYPE,
        "function" | "method" => glyph::NODE_FN,
        _ => '•',
    }
}

/// Whether `node` lives in a file this session changed.
fn is_hot(node: &GraphNode, changed: &[String]) -> bool {
    changed.iter().any(|path| {
        node.label == *path
            || path.ends_with(&format!("/{}", node.label))
            || node
                .location
                .as_deref()
                .is_some_and(|loc| loc.starts_with(path.as_str()))
    })
}

/// Passive form of an edge kind for an incoming relation — `imports` →
/// `imported by`, `calls` → `called by`.
#[must_use]
pub fn passive(kind: &str) -> String {
    let stem = kind.strip_suffix('s').unwrap_or(kind);
    let past = if stem.ends_with('e') {
        format!("{stem}d")
    } else {
        format!("{stem}ed")
    };
    format!("{past} by")
}

fn rounded(title: Line<'static>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(token::BORDER))
        .title(title)
}

/// Draw the tab. `cursor` is already clamped by the caller; `changed` is the
/// focused lane's changed paths.
///
/// `typing` is the query box's live buffer while it is open — deck state, not
/// snapshot state, because it is text nobody has answered yet. Once the
/// driver answers, the same text arrives back as
/// [`GraphSnapshot::query`] and the bar reads it from there instead, which is
/// what keeps the bar from claiming a neighborhood answers a query it does
/// not (#4335).
pub fn render(
    snapshot: &GraphSnapshot,
    cursor: usize,
    changed: &[String],
    accessible: bool,
    typing: Option<&str>,
    area: Rect,
    buf: &mut Buffer,
) {
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let gold = Style::new().fg(token::GOLD);

    let bands = Layout::vertical([
        Constraint::Length(3), // query bar
        Constraint::Length(1), // nodes · n
        Constraint::Min(4),    // panes
        Constraint::Length(2), // keys + price
    ])
    .split(area);

    // The query bar: the selector the view is rooted on, the keys that change
    // it, and the index's size. Three states, in priority order — typing a
    // query beats showing the one already answered, which beats the file the
    // neighborhood is rooted on.
    let mut query = vec![Span::styled(" ⌕ ", gold)];
    match (typing, snapshot.query.as_deref()) {
        (Some(text_so_far), _) => {
            query.push(Span::styled("q:", gold));
            query.push(Span::styled(format!("{text_so_far}▏"), text));
        }
        (None, Some(answered)) => {
            query.push(Span::styled("q:", muted));
            query.push(Span::styled(answered.to_string(), text));
        }
        (None, None) => {
            query.push(Span::styled("file:", muted));
            query.push(Span::styled(snapshot.focus.clone(), text));
        }
    }
    if !snapshot.files.is_empty() && typing.is_none() {
        query.push(Span::styled("   / files · q query", dim));
    }
    // `438 nodes · 12ms · det`. The timing is drawn only when the caller
    // measured one: a snapshot nobody timed (a demo, a scenario fixture)
    // says nothing rather than claiming `0ms` (#4335).
    let right = match snapshot.query_ms {
        Some(ms) => format!("{} nodes · {ms}ms · det ", snapshot.nodes.len()),
        None => format!("{} nodes · det ", snapshot.nodes.len()),
    };
    let used: usize = query.iter().map(Span::width).sum();
    let inner_w = bands[0].width.saturating_sub(2) as usize;
    if used + right.chars().count() < inner_w {
        query.push(Span::styled(
            " ".repeat(inner_w - used - right.chars().count()),
            dim,
        ));
        query.push(Span::styled(right, muted));
    }
    Paragraph::new(Line::from(query))
        .block(rounded(Line::default()))
        .render(bands[0], buf);

    Paragraph::new(Line::from(vec![
        Span::styled(" nodes", muted),
        Span::styled(format!(" · {} in file", snapshot.nodes.len()), dim),
    ]))
    .render(bands[1], buf);

    // Side by side normally; stacked in accessible mode, so a read-aloud row
    // carries one pane, never a slice of each (#1258).
    let panes = if accessible {
        Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).split(bands[2])
    } else {
        Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(64)]).split(bands[2])
    };
    render_list(snapshot, cursor, changed, panes[0], buf);

    let right = Layout::vertical([Constraint::Min(5), Constraint::Length(8)]).split(panes[1]);
    render_card(snapshot, cursor, changed, right[0], buf);
    let coupling_area = if right[1].height >= 4 {
        right[1]
    } else {
        Rect::default()
    };
    if coupling_area.height > 0 {
        super::graph::render(snapshot, cursor, coupling_area, buf);
    }

    // While the query box is open its own keys are the only ones that do
    // anything, so the footer says those instead of the tab's.
    let mut keys = if typing.is_some() {
        vec![
            Span::styled(" ↵", muted),
            Span::styled(" run query", dim),
            Span::styled(" · ", dim),
            Span::styled("esc", muted),
            Span::styled(" cancel", dim),
        ]
    } else {
        vec![Span::styled(" ↵", muted), Span::styled(" open file", dim)]
    };
    if !snapshot.files.is_empty() && typing.is_none() {
        keys.push(Span::styled(" · ", dim));
        keys.push(Span::styled("/", muted));
        keys.push(Span::styled(" files", dim));
        keys.push(Span::styled(" · ", dim));
        keys.push(Span::styled("q", muted));
        keys.push(Span::styled(" query", dim));
    }
    keys.push(Span::styled(" · ", dim));
    keys.push(Span::styled("↑↓", muted));
    keys.push(Span::styled(" walk", dim));
    Paragraph::new(vec![
        Line::from(keys),
        Line::from(vec![
            Span::styled(" every answer here is deterministic", dim),
            Span::styled(" · ", dim),
            Span::styled("$0.00", gold),
        ]),
    ])
    .render(bands[3], buf);
}

/// The node list: glyph, label, then `● hot` or the edge count on the right.
fn render_list(
    snapshot: &GraphSnapshot,
    cursor: usize,
    changed: &[String],
    area: Rect,
    buf: &mut Buffer,
) {
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let block = rounded(Line::default());
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    // Two legend rows at the bottom when there is room for them and a list.
    let legend_rows = if inner.height >= 6 { 2 } else { 0 };
    let list_rows = (inner.height as usize).saturating_sub(legend_rows);
    let total = snapshot.nodes.len();
    // One row is the `⋯ n more` tail when the list overflows.
    let visible = if total > list_rows {
        list_rows.saturating_sub(1).max(1)
    } else {
        list_rows
    };
    let start = if total <= visible {
        0
    } else {
        cursor
            .saturating_sub(visible.saturating_sub(1) / 2)
            .min(total - visible)
    };
    let end = (start + visible).min(total);
    let width = inner.width as usize;

    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, node) in snapshot.nodes.iter().enumerate().take(end).skip(start) {
        let selected = i == cursor;
        let hot = is_hot(node, changed);
        let right = if hot {
            "● hot".to_string()
        } else {
            snapshot.degree(i).to_string()
        };
        let label_w = width.saturating_sub(right.chars().count() + 4);
        let mut label = node.label.clone();
        if label.chars().count() > label_w {
            label = label.chars().take(label_w.saturating_sub(1)).collect();
            label.push('…');
        }
        let mut spans = vec![
            Span::styled(
                format!(" {} ", kind_glyph(&node.kind)),
                Style::new().fg(token::SILVER),
            ),
            Span::styled(label.clone(), Style::new().fg(token::TEXT)),
        ];
        let used = 3 + label.chars().count();
        if used + right.chars().count() < width {
            spans.push(Span::raw(" ".repeat(width - used - right.chars().count())));
        }
        spans.push(Span::styled(
            right,
            if hot {
                Style::new().fg(token::GOLD_BRIGHT)
            } else {
                muted
            },
        ));
        let mut line = Line::from(spans);
        if selected {
            line.style = Style::new().bg(token::HL).add_modifier(Modifier::BOLD);
        }
        lines.push(line);
    }
    if total > visible {
        lines.push(Line::from(Span::styled(
            format!(" ⋯ {} more", total - visible),
            dim,
        )));
    }
    if legend_rows > 0 {
        while lines.len() < list_rows {
            lines.push(Line::default());
        }
        lines.push(Line::from(vec![
            Span::styled(format!(" {} file", glyph::NODE_FILE), dim),
            Span::styled(format!(" · {} type", glyph::NODE_TYPE), dim),
            Span::styled(format!(" · {} fn", glyph::NODE_FN), dim),
        ]));
        lines.push(Line::from(Span::styled(" right column = edge count", dim)));
    }
    Paragraph::new(lines).render(inner, buf);
}

/// The node card: grouped relation counts with the reverse direction, then a
/// sample of the edges themselves.
fn render_card(
    snapshot: &GraphSnapshot,
    cursor: usize,
    changed: &[String],
    area: Rect,
    buf: &mut Buffer,
) {
    let dim = Style::new().fg(token::DIM);
    let muted = Style::new().fg(token::MUTED);
    let text = Style::new().fg(token::TEXT);
    let node = &snapshot.nodes[cursor];
    let mut title = vec![
        Span::styled(
            format!(" {} ", kind_glyph(&node.kind)),
            Style::new().fg(token::GOLD),
        ),
        Span::styled(node.label.clone(), Style::new().fg(token::GOLD)),
        Span::raw(" "),
    ];
    if is_hot(node, changed) {
        title.push(Span::styled("● hot ", Style::new().fg(token::GOLD_BRIGHT)));
    }
    let block = rounded(Line::from(title));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut meta = vec![Span::styled(format!(" {}", node.kind), muted)];
    if let Some(loc) = &node.location {
        meta.push(Span::styled(" · ", dim));
        meta.push(Span::styled(loc.clone(), muted));
    }
    lines.push(Line::from(meta));

    let relations = super::graph::relations(snapshot, cursor);
    if relations.is_empty() {
        lines.push(Line::from(Span::styled(" no known relations", muted)));
    } else {
        let mut spans = vec![Span::raw(" ")];
        for (i, (kind, out, inc)) in relations.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", dim));
            }
            if *out > 0 {
                spans.push(Span::styled(format!("{kind} {out} →"), text));
            }
            if *inc > 0 {
                if *out > 0 {
                    spans.push(Span::styled(" · ", dim));
                }
                spans.push(Span::styled(format!("← {} {inc}", passive(kind)), text));
            }
        }
        lines.push(Line::from(spans));
        let budget = (inner.height as usize).saturating_sub(lines.len() + 1);
        let mut shown = 0usize;
        let mut total = 0usize;
        for edge in &snapshot.edges {
            let outgoing = edge.from == cursor;
            let incoming = edge.to == cursor;
            if !outgoing && !incoming {
                continue;
            }
            total += 1;
            if shown >= budget {
                continue;
            }
            let other = if outgoing { edge.to } else { edge.from };
            let Some(other) = snapshot.nodes.get(other) else {
                continue;
            };
            lines.push(if outgoing {
                Line::from(vec![
                    Span::styled(format!(" {} → ", edge.kind), dim),
                    Span::styled(other.label.clone(), text),
                ])
            } else {
                Line::from(vec![
                    Span::styled(format!(" {} ← ", passive(&edge.kind)), dim),
                    Span::styled(other.label.clone(), text),
                ])
            });
            shown += 1;
        }
        if total > shown {
            lines.push(Line::from(Span::styled(
                format!(" ⋯ {} more edges", total - shown),
                dim,
            )));
        }
    }
    Paragraph::new(lines).render(inner, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_is_hot_when_its_file_changed_this_session() {
        let file = GraphNode {
            label: "lib.rs".into(),
            kind: "file".into(),
            location: None,
        };
        let sym = GraphNode {
            label: "Attachment".into(),
            kind: "struct".into(),
            location: Some("crates/stella-protocol/src/lib.rs:48".into()),
        };
        let changed = vec!["crates/stella-protocol/src/lib.rs".to_string()];
        assert!(is_hot(&file, &changed));
        assert!(is_hot(&sym, &changed));
        assert!(!is_hot(&file, &["crates/other.rs".to_string()]));
    }

    #[test]
    fn kinds_map_onto_the_three_spec_glyphs() {
        assert_eq!(kind_glyph("file"), glyph::NODE_FILE);
        assert_eq!(kind_glyph("struct"), glyph::NODE_TYPE);
        assert_eq!(kind_glyph("function"), glyph::NODE_FN);
        assert_eq!(kind_glyph("widget"), '•');
    }
}
