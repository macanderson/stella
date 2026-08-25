// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `q` box's route from a typed query to a drawn neighborhood.
//!
//! Two layers, because they fail differently. The mapping tests pin what a
//! frame *becomes* and need no index — they are where a protocol field
//! quietly changing shape (a `provenance.range` that stops saying `L12-40`)
//! shows up as a failure rather than as a node with no line. The end-to-end
//! tests run a real index behind a real [`Host`] and pin that the tab's answer
//! survives the whole round trip (#4335).

use contextgraph_types::{ContextFrame, FrameKind, Provenance, Relation};

use super::*;

/// A symbol frame shaped the way `stella-graph`'s `symbol_frame` mints one:
/// `"{keyword} {name}"` title, a `file://` uri, and the line span living in
/// provenance as a string.
fn symbol_frame(
    root: &std::path::Path,
    rel: &str,
    keyword: &str,
    name: &str,
    line: u32,
) -> ContextFrame {
    let uri = file_uri(root, rel);
    let mut frame = ContextFrame::full(
        format!("code-graph:sym:{rel}:{line}:{name}"),
        FrameKind::Symbol,
        format!("{keyword} {name}"),
        "body",
        0.9,
        4,
    );
    frame.provenance = vec![Provenance {
        kind: "file".to_string(),
        uri: Some(uri.clone()),
        range: Some(format!("L{line}-{}", line + 2)),
        digest: None,
        method: None,
        by: None,
    }];
    frame.uri = Some(uri);
    frame
}

/// `"L12-40"` is the only place the protocol puts a line number, so every
/// node's `path:line` depends on parsing it back out.
#[test]
fn a_provenance_range_yields_its_first_line() {
    assert_eq!(range_start_line("L12-40"), Some(12));
    assert_eq!(range_start_line("L7"), Some(7));
    assert_eq!(range_start_line("12-40"), Some(12));
    assert_eq!(range_start_line(""), None);
    assert_eq!(range_start_line("Lnope"), None);
}

/// A `file://` uri round-trips back to the workspace-relative path the deck
/// draws; anything outside the workspace is not a node location.
#[test]
fn a_file_uri_round_trips_to_a_relative_path() {
    let root = std::path::Path::new("/w");
    assert_eq!(
        uri_to_rel(&file_uri(root, "src/x.rs"), root).as_deref(),
        Some("src/x.rs")
    );
    assert_eq!(uri_to_rel("file:///elsewhere/y.rs", root), None);
    assert_eq!(uri_to_rel("symbol:run_turn", root), None);
}

/// The citation keyword becomes the deck's node kind. `fn` is the lossy one:
/// the index's `function` and `method` both arrive as `fn`, so both draw as a
/// function — the documented cost of answering through frames.
#[test]
fn a_citation_keyword_becomes_the_decks_node_kind() {
    assert_eq!(node_kind("fn"), "function");
    assert_eq!(node_kind("struct"), "struct");
    assert_eq!(node_kind("trait"), "trait");
    assert_eq!(node_kind(""), "symbol");
}

/// Symbol frames become nodes hung off the root file; a `Graph` frame is an
/// edge listing and never becomes a node of its own.
#[test]
fn symbol_frames_become_nodes_and_graph_frames_become_edges() {
    let root = std::path::Path::new("/w");
    let frames = vec![
        symbol_frame(root, "hub.rs", "fn", "a", 1),
        symbol_frame(root, "hub.rs", "struct", "C", 3),
    ];
    let (nodes, edges) = frames_graph(&frames, Some("hub.rs"), root);

    assert_eq!(nodes[0].label, "hub.rs", "the root file is node 0");
    assert_eq!(nodes[0].kind, "file");
    assert_eq!(
        nodes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(),
        vec!["hub.rs", "a", "C"],
        "node order follows frame order, so the deck's goldens can pin it"
    );
    assert_eq!(
        nodes[1].kind, "function",
        "`fn` maps to the deck's vocabulary"
    );
    assert_eq!(
        nodes[1].location.as_deref(),
        Some("hub.rs:1"),
        "the line comes out of provenance, not out of the citation prose"
    );
    assert!(
        edges.iter().all(|e| e.from == 0 && e.kind == "defines"),
        "every symbol is defined by the root file"
    );
}

/// `IMPORTED_BY` points *at* the root, not away from it — the one relation
/// whose direction the deck must reverse, and the one a mapping that just
/// copied `rel` would draw backwards.
#[test]
fn an_imported_by_relation_points_at_the_root() {
    let root = std::path::Path::new("/w");
    let mut edge_frame = ContextFrame::full(
        "code-graph:importers:hub.rs",
        FrameKind::Graph,
        "importers of hub.rs",
        "leaf.rs",
        0.5,
        2,
    );
    edge_frame.uri = Some(file_uri(root, "hub.rs"));
    edge_frame.relations = vec![Relation {
        rel: "IMPORTED_BY".to_string(),
        target_uri: file_uri(root, "leaf.rs"),
        display_name: Some("leaf.rs".to_string()),
    }];
    let frames = vec![edge_frame];
    let (nodes, edges) = frames_graph(&frames, Some("hub.rs"), root);

    assert_eq!(
        nodes.iter().map(|n| n.label.as_str()).collect::<Vec<_>>(),
        vec!["hub.rs", "leaf.rs"],
        "the edge listing itself is not a node — only its target is"
    );
    assert_eq!(edges.len(), 1);
    assert_eq!(
        edges[0].from, 1,
        "leaf.rs imports hub.rs, so the edge leaves leaf.rs"
    );
    assert_eq!(edges[0].to, 0);
    assert_eq!(edges[0].kind, "imports");
}

/// An import the index could not resolve still draws, as a module rather than
/// a file — it names something real that simply is not in this workspace.
#[test]
fn an_unresolved_import_draws_as_a_module_with_no_location() {
    let root = std::path::Path::new("/w");
    let mut edge_frame = ContextFrame::full(
        "code-graph:imports:hub.rs",
        FrameKind::Graph,
        "imports of hub.rs",
        "serde",
        0.5,
        2,
    );
    edge_frame.uri = Some(file_uri(root, "hub.rs"));
    edge_frame.relations = vec![Relation {
        rel: "IMPORTS".to_string(),
        target_uri: "unresolved:serde".to_string(),
        display_name: Some("serde".to_string()),
    }];
    let (nodes, edges) = frames_graph(&[edge_frame], Some("hub.rs"), root);

    assert_eq!(nodes[1].label, "serde");
    assert_eq!(nodes[1].kind, "module");
    assert_eq!(
        nodes[1].location, None,
        "a module outside the workspace opens nowhere"
    );
    assert_eq!(edges[0].from, 0, "hub.rs imports serde");
    assert_eq!(edges[0].to, 1);
}

/// Build a real code-graph index: `hub.rs` holds three symbols, `leaf.rs` one.
fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        root.path().join("hub.rs"),
        "pub fn alpha() {}\npub fn beta() {}\npub struct Gamma;\n",
    )
    .unwrap();
    std::fs::write(root.path().join("leaf.rs"), "pub fn delta() {}\n").unwrap();
    let db = stella_store::workspace_private_sqlite_path(root.path(), "codegraph.db").unwrap();
    let graph = stella_graph::CodeGraph::open(root.path(), &db).expect("open graph");
    graph.index_all().expect("index");
    graph.shutdown();
    root
}

/// The whole round trip: a typed needle goes out as a `ContextQuery` through a
/// real `Host`, and the neighborhood the tab draws is assembled from the
/// frames that came back. This is the test that fails if the tab ever goes
/// back to reading `CodeGraph` behind the host's back (#4335).
#[tokio::test]
async fn a_query_is_answered_through_the_host() {
    let root = fixture();
    let host = crate::contextgraph::graph_tab_host(root.path().to_path_buf());
    let snap = graph_query_snapshot(&host, root.path(), "alpha")
        .await
        .expect("an indexed workspace answers");

    assert_eq!(
        snap.query.as_deref(),
        Some("alpha"),
        "the bar echoes the query"
    );
    assert_eq!(snap.focus, "alpha");
    assert!(snap.query_ms.is_some(), "the driver times what it spent");
    assert!(
        snap.nodes
            .iter()
            .any(|n| n.label == "hub.rs" && n.kind == "file"),
        "the answer is rooted on the file holding the definition, got {:?}",
        snap.nodes
    );
    assert!(
        snap.nodes
            .iter()
            .any(|n| n.label == "alpha" && n.kind == "function"),
        "the queried symbol is a node, typed, got {:?}",
        snap.nodes
    );
    assert!(
        snap.nodes
            .iter()
            .find(|n| n.label == "alpha")
            .and_then(|n| n.location.as_deref())
            .is_some_and(|loc| loc.starts_with("hub.rs:")),
        "a node the reader can open"
    );
    assert_eq!(
        snap.files,
        vec!["hub.rs".to_string(), "leaf.rs".to_string()],
        "the picker's inventory still lists every indexed file"
    );
}

/// The neighborhood, not just the hit: querying a symbol also brings back the
/// other symbols its file defines, which is what makes the answer a graph
/// rather than a single node.
#[tokio::test]
async fn a_query_brings_back_the_neighborhood_around_its_hit() {
    let root = fixture();
    let host = crate::contextgraph::graph_tab_host(root.path().to_path_buf());
    let snap = graph_query_snapshot(&host, root.path(), "alpha")
        .await
        .expect("snapshot");

    for sibling in ["beta", "Gamma"] {
        assert!(
            snap.nodes.iter().any(|n| n.label == sibling),
            "{sibling} shares hub.rs with alpha and belongs in its neighborhood, got {:?}",
            snap.nodes
        );
    }
    assert!(
        !snap.edges.is_empty(),
        "a neighborhood with no edges is a list, not a graph"
    );
}

/// A needle that matches nothing is an *answer* — an empty one — not a
/// missing snapshot. The tab must be able to say "no matches" instead of
/// leaving the previous neighborhood up as if it were the result.
#[tokio::test]
async fn a_query_matching_nothing_is_an_empty_answer_not_a_missing_one() {
    let root = fixture();
    let host = crate::contextgraph::graph_tab_host(root.path().to_path_buf());
    let snap = graph_query_snapshot(&host, root.path(), "nosuchsymbol")
        .await
        .expect("an indexed workspace still answers");

    assert!(
        snap.nodes.is_empty(),
        "nothing matched, so nothing is drawn"
    );
    assert!(snap.edges.is_empty());
    assert_eq!(
        snap.query.as_deref(),
        Some("nosuchsymbol"),
        "the bar still shows what was asked"
    );
}

/// A host with no provider registered answers nothing at all, which must not
/// read as "your query found nothing" — the tab keeps what it had. This is the
/// shape a timed-out or crashed provider leg takes, and the reason the fan-out
/// helper distinguishes "nobody answered" from "answered, empty".
#[tokio::test]
async fn a_fanout_nobody_answered_is_not_an_empty_answer() {
    let root = fixture();
    let empty_host = contextgraph_host::Host::new();
    assert!(
        graph_query_snapshot(&empty_host, root.path(), "alpha")
            .await
            .is_none(),
        "no provider answered, so the tab is told nothing rather than told 'no matches'"
    );
}

/// No index at all is the one case that is *not* an empty answer: the tab
/// shows its "run `stella init`" hint instead, and only `None` gets it there.
#[tokio::test]
async fn a_workspace_with_no_index_gets_no_snapshot() {
    let root = tempfile::tempdir().expect("tempdir");
    let host = crate::contextgraph::graph_tab_host(root.path().to_path_buf());
    assert!(
        graph_query_snapshot(&host, root.path(), "alpha")
            .await
            .is_none()
    );
}
