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
//! │ ƒ human_bytes      3 │ │ ← self_driving_cmd · edited turn 14            │
//! │ ⋯ 16 more            │ ╰────────────────────────────────────────────────╯
//! │ ▤ file · ▢ type · ƒ fn│ ╭ coupling · neighbors by edge count ───────────╮
//! │ right column = edges │ │ attachment::Attachment ████████  18            │
//! ╰──────────────────────╯ ╰────────────────────────────────────────────────╯
//! ↵ open file · / files · ⇥ panes
//! every answer here is deterministic · $0.00 · 12ms
//! ```
//!
//! Renders from [`crate::deck_ui::DeckUi::graph`], the out-of-band
//! [`GraphSnapshot`], plus the focused lane's file ledger, which supplies the
//! `● hot` mark and the `edited turn 14` tag beside an edge: a node is hot when
//! this session touched the file it lives in, and the tag names the turn that
//! did it. [`GraphSnapshot::stamp_session_touches`] writes both from that one
//! ledger every frame, so one cannot go stale while the other has not.
//! Every number is a count of edges the index already holds; no model is
//! consulted, which is what the footer's `$0.00` states.
//!
//! The rendering's `12ms` is [`GraphSnapshot::query_ms`], measured by the
//! driver and drawn in the query bar and the footer — and only when it is
//! there: a demo snapshot nobody timed carries `None` and both omit it.
//!
//! The renderings' `q free-form query` is the query bar's second mode. `q`
//! opens a modal box on this tab; `⏎` sends the text as a
//! [`crate::envelope::WorkspaceInput::GraphQuery`], and the driver answers
//! with a snapshot whose [`GraphSnapshot::query`] echoes what was asked. The
//! bar reads `q:<text>` in that mode and `file:<focus>` otherwise, so it
//! always names which of the two produced the neighborhood on screen
//! (#4335).

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};
use stella_tui_theme::{glyph, token};

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::graph::{FileTouch, GraphSnapshot, SessionTouch};
use crate::render::columns;

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

/// The focused lane's file ledger, as [`GraphSnapshot::stamp_session_touches`]
/// reads it: every path the session has touched, with the turn that touched it
/// and what it did.
///
/// The same `files` vector the FILES tab lists and the transcript resolves its
/// diffs from. Reading it here rather than keeping a second record of "touched
/// this session" is what lets the `● hot` mark and the `edited turn N` tag
/// beside it never disagree.
fn session_ledger(model: &WorkspaceModel, focused: usize) -> Vec<FileTouch> {
    model
        .agents
        .get(focused)
        .map(|agent| {
            agent
                .model
                .files
                .iter()
                .map(|file| FileTouch {
                    path: file.path.clone(),
                    touch: SessionTouch {
                        turn: file.touched_turn,
                        kind: file.kind,
                    },
                })
                .collect()
        })
        .unwrap_or_default()
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

/// Draw the GRAPH tab, in the deck's tab signature.
///
/// The empty state and the cursor clamp live here with the drawing they guard.
/// They were a separate module for as long as this tab had two renderers and
/// the older one still owned the deck's entry point; it has one now.
pub fn render(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    // The session's own touches, re-read from the ledger on every frame: the
    // neighborhood is queried once and then sits here for the rest of a
    // session, so a tag written at query time would name turn 3 long after
    // turn 9 edited the same file.
    let ledger = session_ledger(model, ui.focused);
    if let Some(snapshot) = ui.graph.as_mut() {
        snapshot.stamp_session_touches(&ledger);
    }

    let Some(snapshot) = ui.graph.as_ref().filter(|g| !g.is_empty()) else {
        // An empty snapshot that carries a query is a query that matched
        // nothing, which is a different fact from "no index here" and gets
        // its own sentence — otherwise a search for a misspelt symbol reads
        // as advice to run `stella init` (#4335).
        let hint = match ui.graph.as_ref().and_then(|g| g.query.as_deref()) {
            Some(query) => format!("nothing in the index matches `{query}`"),
            None => "no neighborhood loaded — the code graph appears here".to_string(),
        };
        render_empty(&hint, area, buf);
        return;
    };

    // Defensive clamp: the deck's key handler (`deck_ui::graph`) already
    // keeps `graph_cursor` in range on every keypress, but this view must
    // never index out of bounds regardless of how the cursor got here (a
    // fresh `DeckUi`, a test, a snapshot swapped out from under a stale
    // cursor).
    let cursor = ui.graph_cursor.min(snapshot.nodes.len() - 1);
    ui.graph_cursor = cursor;

    paint(
        snapshot,
        cursor,
        ui.accessible,
        ui.graph_query.as_deref(),
        area,
        buf,
    );
}

/// The "nothing to draw" state: one centered muted `hint`, no border chrome
/// beyond the tab's own frame.
fn render_empty(hint: &str, area: Rect, buf: &mut Buffer) {
    let block = Block::default().borders(Borders::ALL).title(" Graph ");
    let inner = block.inner(area);
    block.render(area, buf);
    // A 1–2 row tab body leaves no interior at all, and `inner.y` is then one
    // past the block — drawing the hint there would target a row outside the
    // buffer. Same guard the Files tab's empty state carries.
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let line = Line::from(Span::styled(
        hint.to_string(),
        crate::theme::text_secondary(),
    ))
    .alignment(Alignment::Center);

    // Vertically center the single line (mirrors the splash's centering idiom
    // — this crate doesn't carry a generic `centered_rect` helper).
    let mid = inner.height / 2;
    let row = Rect {
        x: inner.x,
        y: inner.y + mid,
        width: inner.width,
        height: inner.height.saturating_sub(mid).max(1),
    };
    Paragraph::new(line).render(row, buf);
}

/// Draw the loaded tab. `cursor` is already clamped by the caller, and
/// `snapshot` already carries this session's touches
/// ([`GraphSnapshot::stamp_session_touches`]), which is what the `● hot` marks
/// and the `edited turn N` tags are drawn from.
///
/// `typing` is the query box's live buffer while it is open — deck state, not
/// snapshot state, because it is text nobody has answered yet. Once the
/// driver answers, the same text arrives back as
/// [`GraphSnapshot::query`] and the bar reads it from there instead, which is
/// what keeps the bar from claiming a neighborhood answers a query it does
/// not (#4335).
pub fn paint(
    snapshot: &GraphSnapshot,
    cursor: usize,
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
    // `right` is a node count, a timing, and static ASCII words joined by
    // `·`. Every glyph is one column, so `chars().count()` is the width.
    let right_w = right.chars().count();
    if used + right_w < inner_w {
        query.push(Span::styled(" ".repeat(inner_w - used - right_w), dim));
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
    render_list(snapshot, cursor, panes[0], buf);

    let right = Layout::vertical([Constraint::Min(5), Constraint::Length(8)]).split(panes[1]);
    render_card(snapshot, cursor, right[0], buf);
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
    // SPEC 9.1's footer prices the view AND states what it cost to answer:
    // `every answer here is deterministic · $0.00 · 12ms`. The timing is the
    // same [`GraphSnapshot::query_ms`] the query bar reports, drawn under the
    // same rule — a snapshot nobody timed says nothing rather than `0ms`,
    // because "free" and "not measured" are different claims (#4335).
    let mut price = vec![
        Span::styled(" every answer here is deterministic", dim),
        Span::styled(" · ", dim),
        Span::styled("$0.00", gold),
    ];
    if let Some(ms) = snapshot.query_ms {
        price.push(Span::styled(" · ", dim));
        price.push(Span::styled(format!("{ms}ms"), muted));
    }
    Paragraph::new(vec![Line::from(keys), Line::from(price)]).render(bands[3], buf);
}

/// The narrowest label this list will render before the right-hand column has
/// to give ground.
///
/// Below this a row stops being a node list and becomes a column of ellipses:
/// the label is the only cell that says *which* node, and the turn is a
/// refinement of a mark that is already visible.
const MIN_LABEL_COLS: usize = 14;

/// A hot node's right-hand column: the longest form that still leaves the
/// label [`MIN_LABEL_COLS`] (SPEC 9.1, #5220).
///
/// The mark degrades rather than the label truncating, which is the rule this
/// renderer already follows for every other optional column — an unmeasured
/// extent draws no column instead of a narrow wrong one. Three rungs:
///
/// - `● hot · turn 14` — SPEC 9.1's sentence in full;
/// - `● hot 14` — the same fact, no separator, for a pane that cannot spare 15
///   columns;
/// - `● hot` — the turn dropped, which is what the list said before this and
///   still true.
///
/// A touch that cannot name a turn stays on the last rung whatever the width.
/// That is the card's own rule for its edge tag ([`SessionTouch::tag`] returns
/// `None`), and a path the ledger kept across `/clear` is exactly the case:
/// still touched this session, no turn to attribute it to. Inventing one here
/// would be the second reading of the ledger ADR 0019 exists to prevent.
fn hot_mark(touch: &SessionTouch, width: usize) -> String {
    const MARK: &str = "● hot";
    let Some(turn) = touch.turn else {
        return MARK.to_string();
    };
    // Longest first; `width` is the pane, and a rung is affordable when the
    // label keeps its floor. The `+ 4` matches `label_w`'s own arithmetic
    // below — the glyph cell, its spaces, and the gap before this column.
    // `rung` is `MARK` (one bullet, ASCII words) plus a turn number.
    // Every glyph is one column wide, so `chars().count()` is the width.
    for rung in [format!("{MARK} · turn {turn}"), format!("{MARK} {turn}")] {
        if width.saturating_sub(rung.chars().count() + 4) >= MIN_LABEL_COLS {
            return rung;
        }
    }
    MARK.to_string()
}

/// The node list: glyph, label, then `● hot` or the edge count on the right.
fn render_list(snapshot: &GraphSnapshot, cursor: usize, area: Rect, buf: &mut Buffer) {
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
        let hot = node.touch.is_some();
        let right = match &node.touch {
            Some(touch) => hot_mark(touch, width),
            None => snapshot.degree(i).to_string(),
        };
        // `right` is `hot_mark`'s output or a decimal count — both one
        // column per glyph. `node.label` is a code-graph symbol or path,
        // and it can be any language, so its elide and this fill both
        // spend their budget in display columns.
        let right_w = columns::width(&right);
        let label_w = width.saturating_sub(right_w + 4);
        let label = columns::head(&node.label, label_w);
        let mut spans = vec![
            Span::styled(
                format!(" {} ", kind_glyph(&node.kind)),
                Style::new().fg(token::SILVER),
            ),
            Span::styled(label.clone(), Style::new().fg(token::TEXT)),
        ];
        let used = 3 + columns::width(&label);
        if used + right_w < width {
            spans.push(Span::raw(" ".repeat(width - used - right_w)));
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
fn render_card(snapshot: &GraphSnapshot, cursor: usize, area: Rect, buf: &mut Buffer) {
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
    if node.touch.is_some() {
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
            let mut spans = if outgoing {
                vec![
                    Span::styled(format!(" {} → ", edge.kind), dim),
                    Span::styled(other.label.clone(), text),
                ]
            } else {
                vec![
                    Span::styled(format!(" {} ← ", passive(&edge.kind)), dim),
                    Span::styled(other.label.clone(), text),
                ]
            };
            // SPEC 9.1's session tag: the edge names a file this session
            // touched, so it says when — `← self_driving_cmd · edited turn 14`.
            // It hangs off the node at the FAR end, which is the one the line
            // cites; the node under the cursor already wears `● hot` in the
            // card's own title, so repeating its turn on every one of its edges
            // would say the same thing twenty-four times.
            if let Some(tag) = other.touch.and_then(|touch| touch.tag()) {
                spans.push(Span::styled(" · ", dim));
                spans.push(Span::styled(tag, Style::new().fg(token::GOLD_BRIGHT)));
            }
            lines.push(Line::from(spans));
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
mod tab;

#[cfg(test)]
mod tests {
    use super::*;

    use crate::envelope::{AgentMeta, Inbound};
    use crate::graph::GraphNode;
    use stella_protocol::{AgentEvent, FileChangeKind};

    fn touched(turn: Option<u32>) -> SessionTouch {
        SessionTouch {
            turn,
            kind: FileChangeKind::Modified,
        }
    }

    /// **The witness (#5220).** A hot node names the turn that touched it, and
    /// the mark — not the label — is what gives ground as the pane narrows.
    ///
    /// SPEC 9.1 asks the node list to tag `● hot` with the turn, and the full
    /// form is 15 columns against the bare mark's 5. In the 36%-wide left pane
    /// of an 80-column terminal that would eat most of the label, and the label
    /// is the only cell saying *which* node — so the ladder degrades the tag
    /// through three rungs rather than truncating the row's identity.
    ///
    /// Pinned at the width each rung takes over rather than at "some narrow
    /// width", because the boundary is the behavior: a rung that silently
    /// shifted a column either way would still pass a test that only asked for
    /// "something shorter".
    #[test]
    fn the_hot_mark_gives_ground_before_the_label_does() {
        let touch = touched(Some(14));

        // Wide: SPEC 9.1's sentence in full.
        assert_eq!(hot_mark(&touch, 60), "● hot · turn 14");
        // 15 + 4 + 14 is exactly 33, so 33 is the last width that affords it.
        assert_eq!(hot_mark(&touch, 33), "● hot · turn 14");

        // One column narrower, the separator goes and the turn stays.
        assert_eq!(hot_mark(&touch, 32), "● hot 14");
        // 8 + 4 + 14 is 26, the last width that affords the compact rung.
        assert_eq!(hot_mark(&touch, 26), "● hot 14");

        // Below that the turn goes too, and the row says what it said before
        // #5220 — which is still true, just less.
        assert_eq!(hot_mark(&touch, 25), "● hot");
        assert_eq!(hot_mark(&touch, 0), "● hot");
    }

    /// A touch that cannot name a turn stays on the bottom rung at any width.
    ///
    /// [`SessionTouch::tag`] already returns `None` there and the card draws a
    /// bare relation; the list agrees rather than inventing a turn. The case is
    /// real — a ledger row kept across `/clear` is touched this session with no
    /// turn to attribute it to — and deriving one here would be the second
    /// reading of the ledger that ADR 0019 chose a per-frame projection to
    /// prevent.
    #[test]
    fn a_touch_with_no_turn_names_none_however_wide_the_pane() {
        assert_eq!(hot_mark(&touched(None), 200), "● hot");
        assert_eq!(hot_mark(&touched(None), 20), "● hot");
    }

    /// A CJK node label keeps the edge count on screen, not past the pane.
    ///
    /// Old code checked `label.chars().count() > label_w`. `label_w` is a
    /// **column** budget, but the check read it as a char count. A 20-glyph
    /// CJK label is 40 columns but only 20 chars. Against a 30-column
    /// `label_w`, `20 > 30` was false, so the old check kept the label
    /// whole — 10 columns over budget. That is what pushes the edge count
    /// off screen.
    #[test]
    fn a_wide_character_label_keeps_the_edge_count_on_screen() {
        let snapshot = GraphSnapshot {
            nodes: vec![GraphNode {
                label: "圈".repeat(20),
                kind: "function".into(),
                location: None,
                touch: None,
            }],
            ..GraphSnapshot::default()
        };
        // `inner.width` 35: label_w = 35 - 1 (the "0" edge count) - 4 = 30.
        let area = Rect::new(0, 0, 37, 5);
        let mut buf = Buffer::empty(area);
        render_list(&snapshot, 0, area, &mut buf);
        let last_col = area.width - 2; // one column inside the right border
        let row_y = area.y + 1; // one row inside the top border
        let row: String = (0..area.width)
            .map(|x| buf.cell((x, row_y)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        assert_eq!(
            buf.cell((last_col, row_y)).map(|c| c.symbol()),
            Some("0"),
            "the edge count for a node with no edges should still land on \
             the last content column, not be clipped past the pane:\n{row:?}"
        );
    }

    /// The ledger the tab reads is the lane's own `files` vector, carrying the
    /// turn each path was last touched in — which is what turns a `● hot` mark
    /// into `edited turn N`.
    #[test]
    fn the_ledger_carries_each_paths_turn_and_the_verb_that_reached_it() {
        let mut model = WorkspaceModel::new();
        model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        let change = |path: &str, kind| Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::FileChange {
                path: path.into(),
                kind,
                added: 1,
                removed: 0,
                diff: None,
                minimal: true,
                task_id: None,
            },
        };
        let end_turn = Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::TurnComplete {
                model: "test".into(),
                cost_usd: 0.0,
            },
        };

        model.apply_inbound(&change("src/a.rs", FileChangeKind::Modified));
        model.apply_inbound(&change("src/read_only.rs", FileChangeKind::Read));
        model.apply_inbound(&end_turn);
        model.apply_inbound(&change("src/b.rs", FileChangeKind::Created));
        // Turn 2 re-reads the file turn 1 edited. The turn belongs to the
        // edit, so it must not follow the read.
        model.apply_inbound(&change("src/a.rs", FileChangeKind::Read));
        model.apply_inbound(&end_turn);
        // Turn 3 edits it again, and that one does move the tag.
        model.apply_inbound(&change("src/b.rs", FileChangeKind::Modified));

        let ledger = session_ledger(&model, 0);
        let row = |path: &str| {
            ledger
                .iter()
                .find(|row| row.path == path)
                .unwrap_or_else(|| panic!("{path} in the ledger"))
                .touch
        };
        assert_eq!(
            row("src/a.rs").tag().as_deref(),
            Some("edited turn 1"),
            "a later read must not carry the turn of the earlier edit forward"
        );
        assert_eq!(
            row("src/b.rs").tag().as_deref(),
            Some("edited turn 3"),
            "a second mutation moves both the verb and the turn"
        );
        assert_eq!(
            row("src/read_only.rs").tag(),
            None,
            "a path only read names no turn rather than inventing one"
        );

        // A lane index nothing registered reads as an empty ledger rather than
        // panicking — the Graph tab renders before any lane exists.
        assert!(session_ledger(&model, 7).is_empty());
    }

    #[test]
    fn kinds_map_onto_the_three_spec_glyphs() {
        assert_eq!(kind_glyph("file"), glyph::NODE_FILE);
        assert_eq!(kind_glyph("struct"), glyph::NODE_TYPE);
        assert_eq!(kind_glyph("function"), glyph::NODE_FN);
        assert_eq!(kind_glyph("widget"), '•');
    }
}
