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
//!
//! The two reach the index by different routes, on purpose. The picker names
//! a **file** and wants that file's stored neighborhood verbatim, so it reads
//! [`stella_graph::CodeGraph`] directly. The `q` box asks a **question**, and
//! a question is what the Context Graph Protocol exists to route: it goes out
//! as a [`ContextQuery`] through a [`contextgraph_host::Host`] and comes back
//! as [`ContextFrame`]s, so the tab names no `stella-graph` type and a
//! graph-capable provider registered on that host later answers the `q` box
//! with no change here (#4335).
//!
//! Going through frames costs fidelity the direct path keeps, and the cost is
//! paid where [`node_kind`] and [`frame_location`] are written: a frame
//! carries the citation keyword (`fn`), not the index's stored tag
//! (`function` / `method`), and carries a symbol's line inside
//! `provenance.range` as the string `"L12-40"` rather than as a number.
//! Both are recoverable, neither is free, and a reader deciding whether to
//! move the picker onto this path should read those two functions first.

use contextgraph_host::Host;
use contextgraph_types::{ContextFrame, ContextQuery, FrameKind, Relation};
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

/// How many frames the tab asks the host for.
///
/// The tab draws every node it is given, so this is a guard against a
/// pathological query (a one-letter needle matching thousands of symbols)
/// rather than a context budget. The host drops frames past this count, so it
/// is set well above any neighborhood a human can read.
const QUERY_MAX_FRAMES: u32 = 256;

/// The token budget the tab declares to the host.
///
/// The tab renders no content, so every token a frame declares is spent on
/// something it discards — but the host drops a provider's whole leg when its
/// frames sum above `max_tokens`
/// (`ProviderResult::BudgetLie`), and a code-graph frame quotes up to sixty
/// lines of source. Set high enough that a full neighborhood of quoted
/// definitions never trips that audit, because a dropped leg would reach the
/// user as "your query found nothing".
const QUERY_MAX_TOKENS: u32 = 1_000_000;

/// Answer a free-form query from the Graph tab's `q` box, through the CGP host.
///
/// The query goes out as a [`ContextQuery`] and the answer comes back as
/// [`ContextFrame`]s: this function names no `stella-graph` type, so whichever
/// graph-capable providers are registered on `host` are the ones that answer
/// (#4335). Two round trips, because a rooted neighborhood needs to know its
/// root first:
///
/// 1. `query_text` = the needle, `kinds = [Symbol]`. The best-scoring frame's
///    file is the root — a query lands the reader somewhere real rather than
///    on a bare node.
/// 2. `anchors = [that file]`, `kinds = [Symbol, Graph]`. Anchors are what the
///    protocol offers for "the neighborhood around here", and the `Graph`
///    frames are the ones carrying [`Relation`]s, which become the edges.
///
/// Every definition of the needle rides as its own node, because a name
/// defined in several places is ambiguous and the tab should show that rather
/// than silently pick one.
///
/// `None` when the workspace has no index at all — the tab then shows its "run
/// `stella init`" hint. A query that simply matches nothing is NOT `None`: it
/// comes back as a snapshot carrying the query and no nodes, so the tab can
/// say the query found nothing instead of leaving the previous neighborhood on
/// screen as if it were the answer.
pub(crate) async fn graph_query_snapshot(
    host: &Host,
    workspace_root: &std::path::Path,
    text: &str,
) -> Option<GraphSnapshot> {
    // An unindexed workspace is the one case the tab must tell apart from an
    // empty answer, and the host cannot report it: a provider with no index
    // answers "no frames", exactly as it does for a needle that matches
    // nothing. So the index's existence is still checked on disk.
    open_path(workspace_root)?;
    let started = std::time::Instant::now();
    let needle = text.trim();
    // Frame URIs are minted against the index's own root, and
    // `CodeGraph::open` canonicalizes it — so on any workspace reached through
    // a symlink (every macOS `/tmp` and `/var` path, among others) the deck's
    // spelling of the root is not the one the frames carry, and stripping the
    // wrong prefix costs every node its file and line.
    let index_root = canonical_root(workspace_root);

    // A fan-out nobody answered is not an empty answer: returning `None` here
    // leaves the neighborhood the reader was already looking at on screen,
    // rather than replacing it with "no matches" for a query that never
    // reached a provider.
    let hits = crate::contextgraph::query_frames_via_host(
        host,
        &tab_query(needle, Vec::new(), vec![FrameKind::Symbol]),
    )
    .await?;
    let root = hits
        .first()
        .and_then(|frame| frame_rel_path(frame, &index_root));

    let frames = match &root {
        Some(rel) => {
            crate::contextgraph::query_frames_via_host(
                host,
                &tab_query(
                    needle,
                    vec![file_uri(&index_root, rel)],
                    vec![FrameKind::Symbol, FrameKind::Graph],
                ),
            )
            .await?
        }
        // Nothing matched, so there is no neighborhood to ask for. The
        // first-pass frames are the whole answer (an empty one).
        None => hits,
    };

    let (nodes, edges) = frames_graph(&frames, root.as_deref(), &index_root);
    Some(GraphSnapshot {
        focus: needle.to_string(),
        nodes,
        edges,
        // The picker's inventory, not the query's answer: it lists every
        // indexed file so any of them can be re-rooted onto, which is a
        // question about the index rather than about `needle`. No CGP query
        // asks "what do you hold" — the protocol answers questions about
        // content — so this stays a direct read.
        files: all_files(workspace_root),
        query_ms: Some(elapsed_ms(started)),
        query: Some(needle.to_string()),
    })
}

/// The tab's [`ContextQuery`] shape, varying only in what it anchors on and
/// which frame kinds it will accept.
fn tab_query(needle: &str, anchors: Vec<String>, kinds: Vec<FrameKind>) -> ContextQuery {
    ContextQuery {
        // `goal` is the protocol's required field and providers fall back to
        // it when `query_text` is absent; the tab sets both to the needle so a
        // provider reading either one asks the same question.
        goal: needle.to_string(),
        query_text: Some(needle.to_string()),
        embedding: None,
        kinds,
        anchors,
        max_frames: QUERY_MAX_FRAMES,
        max_tokens: QUERY_MAX_TOKENS,
        as_of: None,
        representation_preferences: Vec::new(),
    }
}

/// Every indexed file, for the picker. Empty when the index cannot be read:
/// the picker lists nothing rather than the tab refusing to answer a query it
/// did answer.
fn all_files(workspace_root: &std::path::Path) -> Vec<String> {
    let Some(db_path) = open_path(workspace_root) else {
        return Vec::new();
    };
    let Ok(graph) = stella_graph::CodeGraph::open(workspace_root, &db_path) else {
        return Vec::new();
    };
    let files = graph.all_files().unwrap_or_default();
    graph.shutdown();
    files
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

/// The workspace root in the spelling the index uses.
///
/// [`stella_graph::CodeGraph::open`] canonicalizes the root it is given and
/// mints every frame URI against that, so comparing URIs to the deck's own
/// path only works once both sides are canonical. Falls back to the path as
/// given when it cannot be resolved — a workspace that does not exist yields
/// no frames either way, so the fallback costs nothing.
fn canonical_root(workspace_root: &std::path::Path) -> std::path::PathBuf {
    workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf())
}

/// `file://` URI for a root-relative path, matching what the code-graph
/// provider mints so an anchor round-trips back to the same file.
fn file_uri(root: &std::path::Path, rel: &str) -> String {
    format!("file://{}", root.join(rel).display())
}

/// A `file://` URI (or bare path) as a root-relative slash path, or `None`
/// when it points outside the workspace — the inverse of [`file_uri`].
fn uri_to_rel(uri: &str, root: &std::path::Path) -> Option<String> {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let rel = std::path::Path::new(raw).strip_prefix(root).ok()?;
    let mut out = String::new();
    for part in rel.components() {
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(&part.as_os_str().to_string_lossy());
    }
    (!out.is_empty()).then_some(out)
}

/// The root-relative file a frame is about, from its `uri`.
fn frame_rel_path(frame: &ContextFrame, root: &std::path::Path) -> Option<String> {
    uri_to_rel(frame.uri.as_deref()?, root)
}

/// A frame's title split into the citation keyword and the symbol name —
/// `"fn run_turn"` → `("fn", "run_turn")`. A title with no space is all name,
/// which is what an edge frame's title (`"imports of src/x.rs"`) would
/// degrade to; those never reach here as nodes.
fn split_title(title: &str) -> (&str, &str) {
    match title.split_once(' ') {
        Some((keyword, name)) => (keyword, name),
        None => ("", title),
    }
}

/// The citation keyword as the node kind the deck's `kind_glyph` understands.
///
/// This is where the frame route loses fidelity the direct read keeps. The
/// index stores `function` and `method` as distinct tags and
/// `SymbolKind::keyword` projects **both** onto `fn`, so a method arrives here
/// indistinguishable from a free function and is drawn as one. That is a
/// one-glyph difference in a browsing view, which is why it is accepted rather
/// than worked around: recovering the tag would mean either re-reading the
/// index the query just went through, or widening a frame `id` that the
/// protocol calls stable for dedup. Every other keyword already *is* the
/// deck's vocabulary and passes through unchanged.
fn node_kind(keyword: &str) -> String {
    match keyword {
        "fn" => "function".to_string(),
        "" => "symbol".to_string(),
        other => other.to_string(),
    }
}

/// `path:line` for a symbol frame's detail panel.
///
/// The line lives in `provenance.range` as the string `"L12-40"` — the
/// protocol has no numeric span — so it is parsed back out here. A frame whose
/// provenance carries no range still gets its file, because a node that can be
/// opened at line 1 beats a node with no location at all.
fn frame_location(frame: &ContextFrame, root: &std::path::Path) -> Option<String> {
    let rel = frame_rel_path(frame, root)?;
    let line = frame
        .provenance
        .iter()
        .find_map(|p| p.range.as_deref())
        .and_then(range_start_line);
    Some(match line {
        Some(line) => format!("{rel}:{line}"),
        None => rel,
    })
}

/// The first line number in a provenance range string (`"L12-40"` → `12`).
fn range_start_line(range: &str) -> Option<u32> {
    let digits = range.trim_start_matches('L');
    let end = digits.find('-').unwrap_or(digits.len());
    digits[..end].parse().ok()
}

/// The deck's edge kind for a relation's protocol verb. Unknown verbs are
/// lowercased rather than dropped: the protocol says a host MUST NOT reject an
/// unknown `rel`, and an edge drawn with an unfamiliar label still tells the
/// reader the edge is there.
fn edge_kind(rel: &str) -> String {
    match rel {
        "IMPORTS" | "IMPORTED_BY" => "imports".to_string(),
        "CALLS" => "calls".to_string(),
        other => other.to_lowercase(),
    }
}

/// Host frames as the deck's node/edge pair.
///
/// `Symbol` frames are nodes; `Graph` frames are edge listings whose
/// [`Relation`]s are the edges, and are never drawn as nodes themselves — a
/// node captioned "imports of src/x.rs" is a frame's title leaking into a
/// graph. The root file, when the query found one, is node 0 so the tab opens
/// on something real.
///
/// Node order follows frame order, which
/// [`query_frames_via_host`](crate::contextgraph::query_frames_via_host) has
/// already made a total order — the lookup map is only ever read, never
/// iterated, so the result is byte-stable across runs and the deck's goldens
/// can pin it.
fn frames_graph(
    frames: &[ContextFrame],
    root_rel: Option<&str>,
    workspace_root: &std::path::Path,
) -> (Vec<GraphNode>, Vec<GraphEdge>) {
    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut by_key: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    if let Some(rel) = root_rel {
        by_key.insert(rel.to_string(), 0);
        nodes.push(GraphNode {
            label: rel.to_string(),
            kind: "file".to_string(),
            location: Some(rel.to_string()),
        });
    }

    for frame in frames {
        if frame.kind != FrameKind::Symbol {
            continue;
        }
        // Keyed by frame id: the provider mints one per `path:line:name`, so
        // two same-named symbols in different files stay two nodes, and the
        // same symbol reached twice (once as a definition, once as the root
        // file's neighbor) stays one.
        let at = match by_key.get(&frame.id) {
            Some(at) => *at,
            None => {
                let (keyword, name) = split_title(&frame.title);
                let at = nodes.len();
                by_key.insert(frame.id.clone(), at);
                nodes.push(GraphNode {
                    label: name.to_string(),
                    kind: node_kind(keyword),
                    location: frame_location(frame, workspace_root),
                });
                at
            }
        };
        if root_rel.is_some() && at != 0 {
            edges.push(GraphEdge {
                from: 0,
                to: at,
                kind: "defines".to_string(),
            });
        }
    }

    for frame in frames {
        if frame.kind != FrameKind::Graph {
            continue;
        }
        // An edge needs a node to leave from, and node 0 is the fallback.
        if nodes.is_empty() {
            continue;
        }
        // The frame's own `uri` names the file the listing is about; it is the
        // root for the neighborhood query that produced it, but a provider
        // free to answer differently gets its edges hung off the right node.
        let source = frame
            .uri
            .as_deref()
            .and_then(|uri| uri_to_rel(uri, workspace_root))
            .and_then(|rel| by_key.get(&rel).copied())
            .unwrap_or(0);
        for relation in &frame.relations {
            let to = relation_node(relation, workspace_root, &mut nodes, &mut by_key);
            let (from, to) = if relation.rel == "IMPORTED_BY" {
                (to, source)
            } else {
                (source, to)
            };
            edges.push(GraphEdge {
                from,
                to,
                kind: edge_kind(&relation.rel),
            });
        }
    }

    (nodes, edges)
}

/// The node index a relation points at, appending one when it is new.
///
/// A resolved `file://` target is keyed by its workspace path so it collapses
/// onto the file node that is already there; an unresolved import
/// (`unresolved:serde`) or a name-only call target (`symbol:run_turn`) is
/// keyed by the raw URI and drawn as a module, because it names something the
/// index did not resolve to a file in this workspace.
fn relation_node(
    relation: &Relation,
    workspace_root: &std::path::Path,
    nodes: &mut Vec<GraphNode>,
    by_key: &mut std::collections::HashMap<String, usize>,
) -> usize {
    let rel_path = uri_to_rel(&relation.target_uri, workspace_root)
        .filter(|_| relation.target_uri.starts_with("file://"));
    let key = rel_path
        .clone()
        .unwrap_or_else(|| relation.target_uri.clone());
    if let Some(at) = by_key.get(&key) {
        return *at;
    }
    let at = nodes.len();
    by_key.insert(key, at);
    nodes.push(GraphNode {
        label: relation
            .display_name
            .clone()
            .unwrap_or_else(|| relation.target_uri.clone()),
        kind: if rel_path.is_some() { "file" } else { "module" }.to_string(),
        location: rel_path,
    });
    at
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

#[cfg(test)]
mod tests;
