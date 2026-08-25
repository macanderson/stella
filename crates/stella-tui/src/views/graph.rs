//! The GRAPH tab's coupling ranking — SPEC 9.1.
//!
//! ```text
//! coupling
//!   driver.rs        ████████████  24
//!   bus.rs           ██████         12
//!   settlement.rs    ███             6
//!   high coupling = blast radius if you edit this file
//! ```
//!
//! ## What this replaced, and why
//!
//! A dot-matrix sketch: the neighborhood drawn as a ring of nodes with lines
//! between them, on a `ratatui` `Canvas` with `Marker::Dot`. SPEC 9.1 retires it
//! by name, and two independent rules already condemned it.
//!
//! It did not respect the cell grid (SPEC 2). `Marker::Dot` paints braille
//! sub-cells, which is the one thing this design says a terminal surface may
//! not assume — the eighth-block bar glyphs are the whole sub-cell budget, and
//! they are spent here.
//!
//! And it answered no question. A ring of dots shows *that* a node has
//! neighbours, at a fixed radius, in an order the layout chose; it cannot show
//! *which* neighbour matters, because every node sits equidistant from the
//! focus by construction. The one thing a reader wants from a code graph before
//! editing a file is what breaks if they do, and that is a ranking — so the
//! panel ranks, and says what the ranking is for.
//!
//! ## Deterministic, and priced
//!
//! Every number here is a count of edges the index already holds. No model is
//! consulted, which is why the tab's footer prices the whole view at `$0.00`
//! (SPEC 9.1) — the first of SPEC 1's four theses, stated where it happens to
//! be literally true rather than as a slogan.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use stella_tui_theme::{glyph, token};

use crate::graph::GraphSnapshot;

/// One neighbour of the focused node, with the weight of its connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Neighbor {
    /// The neighbour's human label — never a raw id (L-C4).
    pub label: String,
    /// Edges running between it and the focus, in either direction.
    ///
    /// Undirected on purpose: the question is blast radius, and an edge that
    /// points *at* you breaks you just as surely as one you point out with.
    pub edges: usize,
}

/// Neighbours of `cursor`, heaviest first (SPEC 9.1).
///
/// Pure over the snapshot, so the ranking is testable without a buffer and the
/// panel below is a projection of it.
///
/// Ties break on label rather than on index. Index order is the order the graph
/// query happened to return rows in, which is stable for one query and not
/// across two — a ranking that reshuffles equal-weight neighbours when the user
/// re-roots the view reads as movement that means something, and it would mean
/// nothing.
#[must_use]
pub fn coupling(snapshot: &GraphSnapshot, cursor: usize) -> Vec<Neighbor> {
    let mut counts: Vec<usize> = vec![0; snapshot.nodes.len()];
    for edge in &snapshot.edges {
        let other = match (edge.from, edge.to) {
            (from, to) if from == cursor && to != cursor => to,
            (from, to) if to == cursor && from != cursor => from,
            // A self-edge is not a neighbour, and counting it would rank a node
            // against itself.
            _ => continue,
        };
        if let Some(slot) = counts.get_mut(other) {
            *slot += 1;
        }
    }
    let mut out: Vec<Neighbor> = counts
        .into_iter()
        .enumerate()
        .filter(|&(_, edges)| edges > 0)
        .filter_map(|(idx, edges)| {
            snapshot.nodes.get(idx).map(|node| Neighbor {
                label: node.label.clone(),
                edges,
            })
        })
        .collect();
    out.sort_by(|a, b| b.edges.cmp(&a.edges).then_with(|| a.label.cmp(&b.label)));
    out
}

/// Grouped relation counts for the focused node, **including the reverse
/// direction** (SPEC 9.1).
///
/// Returned as `(kind, outgoing, incoming)` in first-seen order. The reverse
/// half is the point: `imports 24` alone tells a reader what this file needs,
/// and says nothing about who needs *it* — which is the direction that decides
/// whether a change is safe.
#[must_use]
pub fn relations(snapshot: &GraphSnapshot, cursor: usize) -> Vec<(String, usize, usize)> {
    let mut out: Vec<(String, usize, usize)> = Vec::new();
    for edge in &snapshot.edges {
        let (outgoing, incoming) = (edge.from == cursor, edge.to == cursor);
        if !outgoing && !incoming {
            continue;
        }
        let slot = match out.iter_mut().find(|(kind, _, _)| *kind == edge.kind) {
            Some(slot) => slot,
            None => {
                out.push((edge.kind.clone(), 0, 0));
                out.last_mut().expect("just pushed")
            }
        };
        // A self-edge counts once in each direction, which is what it is.
        if outgoing {
            slot.1 += 1;
        }
        if incoming {
            slot.2 += 1;
        }
    }
    out
}

/// Cells the heaviest neighbour's bar occupies. Everything else scales against
/// it, so the panel always uses its width and never implies an absolute scale
/// the graph does not have.
const BAR_CELLS: usize = 12;

/// The bar for `edges` against the ranking's maximum, in eighth-blocks.
///
/// Two regimes, and the switch between them is where the bar earns its
/// reading. While the
/// heaviest neighbour still fits the bar, one cell means one edge and the bar
/// is *absolute* — three neighbours tied at one edge each draw three one-cell
/// bars, which is what a tie at one edge looks like. Only once the maximum
/// outgrows the bar does it become *relative*, scaling everything against the
/// heaviest.
///
/// Normalizing unconditionally is what the first cut did, and it made a tie at
/// the bottom indistinguishable from a tie at the top: every neighbour sat at
/// its own maximum, so three files sharing one edge apiece each drew a full
/// twelve-cell bar under a caption reading "high coupling = blast radius". The
/// number beside it said `1`. A bar chart where every bar is full has stopped
/// discriminating, and this one would have shouted precisely when there was
/// nothing to say.
fn bar(edges: usize, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if max <= BAR_CELLS {
        return glyph::BLOCK_EIGHTHS[8]
            .to_string()
            .repeat(edges.min(BAR_CELLS));
    }
    let eighths = (edges * BAR_CELLS * 8).div_ceil(max);
    let full = eighths / 8;
    let rest = eighths % 8;
    let mut s = glyph::BLOCK_EIGHTHS[8].to_string().repeat(full);
    if rest > 0 {
        s.push(glyph::BLOCK_EIGHTHS[rest]);
    }
    s
}

/// The coupling panel: ranked neighbours, gold bars, and the caption that says
/// what the ranking is for.
pub fn render(snapshot: &GraphSnapshot, cursor: usize, area: Rect, buf: &mut Buffer) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::new().fg(token::BORDER))
        .title(Line::from(vec![
            Span::styled(" coupling", Style::new().fg(token::TEXT)),
            Span::styled(" · neighbors by edge count ", Style::new().fg(token::MUTED)),
        ]));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 || inner.width < 12 {
        return;
    }

    let ranked = coupling(snapshot, cursor);
    let caption = Line::from(Span::styled(
        "high coupling = blast radius if you edit this file",
        Style::new().fg(token::DIM),
    ));
    if ranked.is_empty() {
        Paragraph::new(vec![
            Line::from(Span::styled(
                "no edges touch this node",
                Style::new().fg(token::MUTED),
            )),
            caption,
        ])
        .render(inner, buf);
        return;
    }

    let max = ranked.first().map_or(0, |n| n.edges);
    // The caption keeps the last row; the ranking gets what is left.
    let rows = inner.height.saturating_sub(1) as usize;
    let label_w = ranked
        .iter()
        .take(rows)
        .map(|n| n.label.chars().count())
        .max()
        .unwrap_or(0)
        .min(inner.width.saturating_sub(BAR_CELLS as u16 + 6) as usize);

    let mut lines: Vec<Line<'static>> = ranked
        .iter()
        .take(rows)
        .map(|n| {
            let mut label = n.label.clone();
            if label.chars().count() > label_w {
                label = label.chars().take(label_w.saturating_sub(1)).collect();
                label.push('…');
            }
            Line::from(vec![
                Span::styled(format!("{label:label_w$} "), Style::new().fg(token::TEXT)),
                Span::styled(bar(n.edges, max), Style::new().fg(token::GOLD)),
                Span::styled(format!("  {}", n.edges), Style::new().fg(token::MUTED)),
            ])
        })
        .collect();
    // The caption holds the panel's last row, so the eye finds it in the
    // same place whatever the ranking's length.
    while lines.len() + 1 < inner.height as usize {
        lines.push(Line::default());
    }
    lines.push(caption);
    Paragraph::new(lines).render(inner, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{GraphEdge, GraphNode};

    fn node(label: &str) -> GraphNode {
        GraphNode {
            label: label.into(),
            kind: "file".into(),
            location: None,
        }
    }

    fn edge(from: usize, to: usize, kind: &str) -> GraphEdge {
        GraphEdge {
            from,
            to,
            kind: kind.into(),
        }
    }

    /// focus=0, with 1 tied by three edges, 2 by one, 3 by none.
    fn snapshot() -> GraphSnapshot {
        GraphSnapshot {
            focus: "focus.rs".into(),
            nodes: vec![
                node("focus.rs"),
                node("driver.rs"),
                node("bus.rs"),
                node("lonely.rs"),
            ],
            edges: vec![
                edge(0, 1, "imports"),
                edge(1, 0, "imports"),
                edge(0, 1, "calls"),
                edge(2, 0, "imports"),
            ],
            files: Vec::new(),
            query_ms: None,
        }
    }

    #[test]
    fn neighbours_rank_by_edge_count() {
        let ranked = coupling(&snapshot(), 0);
        assert_eq!(ranked.len(), 2, "a node with no edges is not a neighbour");
        assert_eq!(ranked[0].label, "driver.rs");
        assert_eq!(ranked[0].edges, 3);
        assert_eq!(ranked[1].label, "bus.rs");
        assert_eq!(ranked[1].edges, 1);
    }

    /// The question is blast radius: an edge pointing *at* you breaks you just
    /// as surely as one you point out with, so the count is undirected.
    #[test]
    fn an_inbound_edge_counts_as_coupling() {
        let ranked = coupling(&snapshot(), 0);
        let bus = ranked
            .iter()
            .find(|n| n.label == "bus.rs")
            .expect("bus.rs ranks");
        assert_eq!(bus.edges, 1, "bus.rs only ever points *at* the focus");
    }

    /// Ties break on label, not on index. Index order is whatever the graph
    /// query returned, which is stable for one query and not across two — a
    /// ranking that reshuffles equal-weight rows on a re-root reads as movement
    /// that means something.
    #[test]
    fn equal_weights_break_on_label_so_the_order_is_stable() {
        let mut snap = snapshot();
        snap.nodes = vec![node("focus.rs"), node("zeta.rs"), node("alpha.rs")];
        snap.edges = vec![edge(0, 1, "imports"), edge(0, 2, "imports")];
        let ranked = coupling(&snap, 0);
        assert_eq!(ranked[0].label, "alpha.rs");
        assert_eq!(ranked[1].label, "zeta.rs");
    }

    /// SPEC 9.1 asks for the reverse direction by name: `imports 24 → · ←
    /// imported-by 12`. One number cannot carry both.
    #[test]
    fn relations_carry_both_directions_per_kind() {
        let rel = relations(&snapshot(), 0);
        let imports = rel
            .iter()
            .find(|(k, _, _)| k == "imports")
            .expect("imports group");
        assert_eq!(imports.1, 1, "focus imports one thing");
        assert_eq!(imports.2, 2, "and two things import focus");
        let calls = rel
            .iter()
            .find(|(k, _, _)| k == "calls")
            .expect("calls group");
        assert_eq!((calls.1, calls.2), (1, 0));
    }

    /// A self-edge is not a neighbour; ranking a node against itself would put
    /// the focus at the top of its own blast radius.
    #[test]
    fn a_self_edge_is_not_a_neighbour() {
        let mut snap = snapshot();
        snap.edges.push(edge(0, 0, "calls"));
        assert!(
            coupling(&snap, 0).iter().all(|n| n.label != "focus.rs"),
            "the focus ranked itself"
        );
    }

    /// SPEC 2 allows eighth-blocks and nothing finer. The panel this replaced
    /// painted braille sub-cells through `Marker::Dot`.
    #[test]
    fn bars_use_only_the_eighth_block_ramp() {
        for edges in 0..=24usize {
            for ch in bar(edges, 24).chars() {
                assert!(
                    glyph::BLOCK_EIGHTHS.contains(&ch),
                    "{ch:?} is not on the eighth-block ramp"
                );
            }
        }
    }

    /// A tie at one edge draws one cell each, not three full bars.
    ///
    /// The regression this guards: normalizing unconditionally put every
    /// neighbour at its own maximum, so three files sharing a single edge
    /// apiece each filled the bar under a caption reading "high coupling =
    /// blast radius", with `1` printed beside it.
    #[test]
    fn small_counts_are_absolute_so_a_low_tie_reads_as_low() {
        assert_eq!(bar(1, 1).chars().count(), 1, "a lone edge drew a full bar");
        assert_eq!(bar(3, 3).chars().count(), 3);
        assert_eq!(bar(1, 3).chars().count(), 1);
        // At the switch the bar is full either way, so the two regimes meet
        // without a jump.
        assert_eq!(bar(BAR_CELLS, BAR_CELLS).chars().count(), BAR_CELLS);
        assert_eq!(bar(BAR_CELLS + 1, BAR_CELLS + 1).chars().count(), BAR_CELLS);
    }

    /// The heaviest neighbour fills the bar; anything present draws something.
    /// A neighbour that ranks but renders as blank is a row saying nothing.
    #[test]
    fn the_bar_spans_the_ranking_without_vanishing() {
        assert_eq!(bar(24, 24).chars().count(), BAR_CELLS);
        for edges in 1..24usize {
            assert!(!bar(edges, 24).is_empty(), "{edges} of 24 drew nothing");
        }
        assert!(bar(0, 24).is_empty(), "zero drew a bar");
    }
}
