// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The Graph tab's read side: `codegraph.db` → [`stella_tui::GraphSnapshot`].
//!
//! Separate from `super::graph`, which *builds* the index. This module only
//! reads it, and it reads it on behalf of a surface that cannot: `stella-tui`
//! never links `stella-graph`, so every neighborhood the deck draws is one
//! the driver queried and handed over (`doc:` the crate's own
//! [`stella_tui::GraphSnapshot`] docs).
//!
//! Two entry points, one per way a user re-roots the tab — a file
//! ([`graph_snapshot_focus`]) or a free-form query ([`graph_query_snapshot`])
//! — and both report the wall clock they spent, because the deck cannot
//! measure a query it did not run (#4335).

use stella_tui::{GraphEdge, GraphNode, GraphSnapshot};

/// Query the code graph (if `stella init` has built it) for the
/// best-connected file's neighborhood, converted to the deck's Graph-tab
/// snapshot. `None` when there is no index, it is empty, or any read fails —
/// the tab then shows its "run stella init" hint instead of an empty graph.
///
/// This is [`graph_snapshot_focus`] with no explicit focus: the neighborhood
/// centers on [`busiest_file`](stella_graph::CodeGraph::busiest_file), which
/// the deck opens on and can re-root away from via the picker.
pub(crate) fn graph_snapshot(workspace_root: &std::path::Path) -> Option<GraphSnapshot> {
    graph_snapshot_focus(workspace_root, None)
}

/// Build the Graph-tab snapshot centered on `focus` (a root-relative file
/// path), or on the busiest file when `focus` is `None`. The snapshot always
/// carries the full [`files`](stella_tui::GraphSnapshot::files) list so the
/// deck's picker can re-root onto any of them — the deck answers a
/// `FocusGraphFile` request by calling this with `Some(file)` and shipping the
/// result back as a fresh `Inbound::GraphSnapshot`. `None` when there is no
/// index, it is empty, or any read fails.
pub(crate) fn graph_snapshot_focus(
    workspace_root: &std::path::Path,
    focus: Option<&str>,
) -> Option<GraphSnapshot> {
    let db_path = open_path(workspace_root)?;
    // The query bar reports what this cost, so the clock covers the whole
    // round-trip — open, read, close — and not just the neighborhood read.
    // Opening the database is a real per-query cost here (there is no pooled
    // handle to amortize it against), so a number that excluded it would be
    // the answer to a question nobody asked (#4335).
    let started = std::time::Instant::now();
    let graph = stella_graph::CodeGraph::open(workspace_root, &db_path).ok()?;
    // An explicit pick roots there; otherwise fall back to the busiest file.
    let focus = match focus {
        Some(f) => f.to_string(),
        None => graph.busiest_file().ok()??,
    };
    let hood = graph.file_neighborhood(std::path::Path::new(&focus)).ok()?;
    // The full file list backs the picker (a superset of this neighborhood).
    let files = graph.all_files().unwrap_or_default();
    graph.shutdown();

    let (nodes, edges) = neighborhood_graph(&hood);
    Some(GraphSnapshot {
        focus: hood.file,
        nodes,
        edges,
        files,
        query_ms: Some(elapsed_ms(started)),
        query: None,
    })
}

/// Answer a free-form query from the Graph tab's `q` box.
///
/// `text` is resolved as a **symbol name** against the index's definitions.
/// That is narrower than the CGP host's `ContextQuery`: the
/// host assembles prose frames with snippets and provenance for a *model* to
/// read, while this tab needs structure — a label, a kind, a file and a line
/// per node — and rendering prose as a graph would be a worse answer than
/// the one the index can give directly.
///
/// The neighborhood is rooted on the file holding the first definition, so a
/// query lands the reader somewhere real rather than on a bare node. Every
/// other definition of the same name rides as its own node, because a name
/// defined in several places is ambiguous and the tab should show that
/// rather than silently pick one.
///
/// `None` when there is no index or a read fails. A query that simply matches
/// nothing is NOT `None`: it comes back as a snapshot carrying the query and
/// no nodes, so the tab can say the query found nothing instead of leaving
/// the previous neighborhood on screen as if it were the answer.
pub(crate) fn graph_query_snapshot(
    workspace_root: &std::path::Path,
    text: &str,
) -> Option<GraphSnapshot> {
    let db_path = open_path(workspace_root)?;
    let started = std::time::Instant::now();
    let graph = stella_graph::CodeGraph::open(workspace_root, &db_path).ok()?;
    let needle = text.trim();
    let spans = graph.definition_spans(needle).unwrap_or_default();
    let hood = spans.first().and_then(|span| {
        graph
            .file_neighborhood(std::path::Path::new(&span.path))
            .ok()
    });
    let files = graph.all_files().unwrap_or_default();
    graph.shutdown();

    let (mut nodes, mut edges) = match &hood {
        Some(hood) => neighborhood_graph(hood),
        None => (Vec::new(), Vec::new()),
    };
    // The further definitions, hung off the root so the ambiguity is an edge
    // rather than a footnote. Skipping the first: it is inside the
    // neighborhood already.
    for span in spans.iter().skip(1) {
        if !nodes.is_empty() {
            edges.push(GraphEdge {
                from: 0,
                to: nodes.len(),
                kind: "defines".to_string(),
            });
        }
        nodes.push(GraphNode {
            label: span.name.clone(),
            kind: span.kind.clone(),
            location: Some(format!("{}:{}", span.path, span.start_line)),
        });
    }
    Some(GraphSnapshot {
        focus: needle.to_string(),
        nodes,
        edges,
        files,
        query_ms: Some(elapsed_ms(started)),
        query: Some(needle.to_string()),
    })
}

/// The index path, or `None` when `stella init` has never run here.
fn open_path(workspace_root: &std::path::Path) -> Option<std::path::PathBuf> {
    let db_path =
        stella_store::existing_workspace_private_sqlite_path(workspace_root, "codegraph.db")
            .ok()??;
    db_path.exists().then_some(db_path)
}

/// Whole milliseconds since `started`, saturating rather than wrapping — a
/// number the query bar prints must never be a wrapped one.
fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// One file neighborhood as the deck's node/edge pair: the file at index 0,
/// then the symbols it defines, the modules it imports, and the files that
/// import it.
fn neighborhood_graph(hood: &stella_graph::FileNeighborhood) -> (Vec<GraphNode>, Vec<GraphEdge>) {
    let mut nodes = vec![GraphNode {
        label: hood.file.clone(),
        kind: "file".to_string(),
        location: Some(hood.file.clone()),
    }];
    let mut edges = Vec::new();
    for symbol in &hood.symbols {
        edges.push(GraphEdge {
            from: 0,
            to: nodes.len(),
            kind: "defines".to_string(),
        });
        nodes.push(GraphNode {
            label: symbol.name.clone(),
            kind: symbol.kind.clone(),
            location: Some(format!("{}:{}", hood.file, symbol.start_line)),
        });
    }
    for import in &hood.imports {
        edges.push(GraphEdge {
            from: 0,
            to: nodes.len(),
            kind: "imports".to_string(),
        });
        nodes.push(GraphNode {
            label: import.clone(),
            kind: "module".to_string(),
            location: None,
        });
    }
    for importer in &hood.importers {
        edges.push(GraphEdge {
            from: nodes.len(),
            to: 0,
            kind: "imports".to_string(),
        });
        nodes.push(GraphNode {
            label: importer.clone(),
            kind: "file".to_string(),
            location: Some(importer.clone()),
        });
    }
    (nodes, edges)
}
