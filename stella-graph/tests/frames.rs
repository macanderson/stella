//! The query → [`ContextFrame`] contract: frames carry mandatory human
//! citation labels (L-C4), `code-graph` provenance (task brief), valid scores,
//! and respect the query's token budget (L-C5).

use std::fs;

use stella_graph::{CodeGraph, ContextQuery, FrameKind, PROVIDER_ID};
use tempfile::TempDir;

fn query(text: &str, max_frames: u32, max_tokens: u32) -> ContextQuery {
    ContextQuery {
        goal: text.to_string(),
        query_text: Some(text.to_string()),
        embedding: None,
        kinds: Vec::new(),
        anchors: Vec::new(),
        max_frames,
        max_tokens,
        as_of: None,
        representation_preferences: vec![],
    }
}

fn fixture() -> (TempDir, TempDir, CodeGraph) {
    let ws = TempDir::new().unwrap();
    let dbdir = TempDir::new().unwrap();
    fs::write(
        ws.path().join("driver.rs"),
        "pub fn run_turn() {\n    // drive one turn\n}\npub struct Engine;\n",
    )
    .unwrap();
    let graph = CodeGraph::open(ws.path(), &dbdir.path().join("context.db")).unwrap();
    graph.index_all().unwrap();
    (ws, dbdir, graph)
}

#[test]
fn definition_frames_have_human_citation_labels_and_provenance() {
    let (_ws, _db, graph) = fixture();
    let frames = graph.definitions("run_turn").unwrap();
    assert_eq!(frames.len(), 1);
    let frame = &frames[0];

    // L-C4: cited by a human label like `fn run_turn (driver.rs:1)`, never a
    // raw id (the id is structured but is not the citation surface).
    let label = frame
        .citation_label
        .as_deref()
        .expect("citation label is mandatory");
    assert!(
        label.starts_with("fn run_turn ("),
        "unexpected label: {label}"
    );
    assert!(
        label.contains("driver.rs:1"),
        "label should cite path:line: {label}"
    );
    assert_ne!(label, frame.id, "the citation label must not be the raw id");

    assert_eq!(frame.kind, FrameKind::Symbol);
    assert!(frame.has_valid_score());
    // §B3: the declared inline token_cost is the canonical `budget_tokens`
    // count over the content, exact — a metering host is told the truth.
    assert!(
        frame.declares_honest_token_cost(),
        "token_cost {} must equal the canonical inline count {}",
        frame.token_cost,
        frame.expected_inline_token_cost(),
    );

    // Provenance carries the provider id `code-graph` and a file entry.
    assert!(
        frame
            .provenance
            .iter()
            .any(|p| p.by.as_deref() == Some(PROVIDER_ID)),
        "provenance must attribute the frame to the code-graph provider"
    );
    assert!(frame.provenance.iter().any(|p| p.kind == "file"));
}

#[test]
fn query_returns_frames_and_respects_the_budget() {
    let (_ws, _db, graph) = fixture();

    // Ample budget: the definition for `run_turn` comes back.
    let frames = graph
        .query(&query("look at run_turn and Engine", 20, 4000))
        .unwrap();
    assert!(!frames.is_empty());
    assert!(frames.iter().all(|f| f.citation_label.is_some()));
    assert!(frames.iter().all(|f| f.has_valid_score()));
    assert!(frames.iter().any(|f| {
        f.citation_label
            .as_deref()
            .unwrap()
            .starts_with("fn run_turn (")
    }));

    // Tight budget: total token cost never exceeds the cap.
    let tight = graph
        .query(&query("look at run_turn and Engine", 20, 8))
        .unwrap();
    let total: u32 = tight.iter().map(|f| f.token_cost).sum();
    assert!(
        total <= 8,
        "budget-pack must not exceed max_tokens; got {total}"
    );
}

#[test]
fn kind_filter_limits_returned_frames() {
    let (_ws, _db, graph) = fixture();
    let mut q = query("run_turn", 20, 4000);
    q.kinds = vec![FrameKind::Graph]; // no graph frames for a bare definition query
    let frames = graph.query(&q).unwrap();
    assert!(
        frames.iter().all(|f| f.kind == FrameKind::Graph),
        "kind filter must exclude non-matching frames"
    );
}

#[test]
fn file_neighborhood_returns_the_files_symbols() {
    // The structured neighborhood the deck's Graph tab renders (as opposed to
    // the prose frames above): the file's own symbols, with kinds and lines.
    let (_ws, _db, graph) = fixture();
    let hood = graph
        .file_neighborhood(std::path::Path::new("driver.rs"))
        .unwrap();
    assert_eq!(hood.file, "driver.rs");
    let names: Vec<&str> = hood.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"run_turn"), "symbols: {names:?}");
    assert!(names.contains(&"Engine"), "symbols: {names:?}");
    assert!(
        hood.symbols.iter().any(|s| s.kind == "function"),
        "a function symbol should carry the persisted `function` kind tag"
    );
}

#[test]
fn busiest_file_names_the_most_connected_file() {
    let (_ws, _db, graph) = fixture();
    // The one-file fixture makes driver.rs the only (thus busiest) candidate.
    assert_eq!(graph.busiest_file().unwrap().as_deref(), Some("driver.rs"));
}

#[test]
fn file_neighborhood_round_trips_through_serde() {
    // The type crosses the crate boundary (the deck consumes it), so it must
    // survive a serde round-trip like every other public wire type.
    let (_ws, _db, graph) = fixture();
    let hood = graph
        .file_neighborhood(std::path::Path::new("driver.rs"))
        .unwrap();
    let json = serde_json::to_string(&hood).unwrap();
    let back: stella_graph::FileNeighborhood = serde_json::from_str(&json).unwrap();
    assert_eq!(hood, back);
}

/// WITNESS (#451, `docs/context-reuse.md` §1): every graph frame declares a
/// `content_digest` over exactly the bytes it carries inline. Without one the
/// frame is *not verifiable* and a host must re-query it rather than reuse it
/// (D4) — forfeiting the byte-stable prefix canonical composition buys.
#[test]
fn frames_declare_a_content_digest_over_their_inline_content() {
    let (_ws, _db, graph) = fixture();
    let frames = graph.query(&query("run_turn", 10, 4_000)).unwrap();
    assert!(!frames.is_empty(), "fixture must produce frames");
    for frame in &frames {
        let digest = frame
            .content_digest
            .as_deref()
            .unwrap_or_else(|| panic!("frame `{}` declares no content_digest", frame.id));
        let hex = digest
            .strip_prefix("sha256:")
            .unwrap_or_else(|| panic!("digest `{digest}` is not `sha256:<hex>`"));
        assert_eq!(hex.len(), 64, "digest `{digest}` is not 64 hex chars");
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "digest `{digest}` must be lowercase hex"
        );
        // The digest must name the bytes actually served: recompute it.
        let content = frame.content.as_deref().unwrap_or_default();
        let expected = sha256_hex(content.as_bytes());
        assert_eq!(
            hex, expected,
            "frame `{}` digest does not match its own content — identity would be a lie (§1 D1)",
            frame.id
        );
    }
}

/// Editing the source must change the digest, or §4's staleness detection
/// cannot work (§1 D1).
#[test]
fn changing_a_file_changes_its_frames_content_digest() {
    let (ws, _db, graph) = fixture();
    let before = graph.definitions("run_turn").unwrap()[0]
        .content_digest
        .clone()
        .expect("digest");
    fs::write(
        ws.path().join("driver.rs"),
        "pub fn run_turn() {\n    // drive one turn, differently\n}\npub struct Engine;\n",
    )
    .unwrap();
    graph.index_all().unwrap();
    let after = graph.definitions("run_turn").unwrap()[0]
        .content_digest
        .clone()
        .expect("digest");
    assert_ne!(
        before, after,
        "changed content MUST change content_digest (§1 D1)"
    );
}

/// A barrel file's import frame must survive budget-packing. `budget_pack`
/// *skips* a frame that would exceed `max_tokens` rather than truncating it, so
/// an uncapped listing of 300 edges means the caller silently gets no import
/// context at all — the failure mode is invisible. The listing is capped at 50
/// edges plus a `+N more` marker; the structured relations stay complete,
/// because they carry no inline content and cost the frame's declared
/// `token_cost` nothing.
#[test]
fn barrel_file_import_frame_is_capped_and_survives_the_budget() {
    let ws = TempDir::new().unwrap();
    let dbdir = TempDir::new().unwrap();
    const EDGES: usize = 300;
    let mut barrel = String::new();
    for i in 0..EDGES {
        fs::write(
            ws.path().join(format!("mod_{i}.ts")),
            format!("export const v{i} = {i};\n"),
        )
        .unwrap();
        barrel.push_str(&format!("export * from \"./mod_{i}\";\n"));
    }
    fs::write(ws.path().join("index.ts"), &barrel).unwrap();

    let graph = CodeGraph::open(ws.path(), &dbdir.path().join("context.db")).unwrap();
    graph.index_all().unwrap();

    let frames = graph.imports_of(&ws.path().join("index.ts")).unwrap();
    let frame = frames
        .iter()
        .find(|f| f.kind == FrameKind::Graph)
        .expect("barrel file has an import frame");

    let content = frame
        .content
        .as_deref()
        .expect("import frames carry content");
    assert_eq!(
        content.lines().count(),
        51,
        "50 edges plus the `+N more` marker: {content}"
    );
    assert!(
        content.lines().last().unwrap().starts_with("+250 more"),
        "the elision must be visible: {content}"
    );
    // Every edge is still reachable structurally — only the prose is capped.
    assert_eq!(frame.relations.len(), EDGES);

    // The point of the cap. 800 tokens comfortably holds the capped listing
    // (~280) but not the uncapped 300-edge one (~1650), and `budget_pack`
    // *skips* an over-budget frame rather than truncating it — so without the
    // cap this query returns no import context at all.
    assert!(
        frame.token_cost < 800,
        "capped listing should cost well under the budget; got {}",
        frame.token_cost
    );
    let packed = graph
        .query(&ContextQuery {
            anchors: vec![ws.path().join("index.ts").display().to_string()],
            kinds: vec![FrameKind::Graph],
            ..query("imports", 20, 800)
        })
        .unwrap();
    assert!(
        packed.iter().any(|f| f.id == frame.id),
        "the capped import frame must fit the budget"
    );
}

/// A multi-sentence goal yields dozens of identifier tokens, each costing a SQL
/// round trip plus a snippet file read. We deliberately stop looking after the
/// first 12 distinct tokens — the frames past that are budget-packed away
/// anyway, so the lookups are pure cost.
#[test]
fn query_stops_looking_after_twelve_distinct_identifiers() {
    let ws = TempDir::new().unwrap();
    let dbdir = TempDir::new().unwrap();
    const NAMES: usize = 30;
    let mut src = String::new();
    for i in 0..NAMES {
        src.push_str(&format!("pub fn sym_{i:02}() {{}}\n"));
    }
    fs::write(ws.path().join("wide.rs"), &src).unwrap();
    let graph = CodeGraph::open(ws.path(), &dbdir.path().join("context.db")).unwrap();
    graph.index_all().unwrap();

    // Every one of the 30 names is indexed and would match on its own.
    assert_eq!(graph.definitions("sym_00").unwrap().len(), 1);
    assert_eq!(graph.definitions("sym_29").unwrap().len(), 1);

    let goal: Vec<String> = (0..NAMES).map(|i| format!("sym_{i:02}")).collect();
    // Budget deliberately ample: the bound under test is the identifier cap,
    // not `budget_pack`.
    let frames = graph
        .query(&query(&goal.join(" "), 1000, 1_000_000))
        .unwrap();
    assert_eq!(
        frames.len(),
        12,
        "only the first 12 distinct tokens are looked up"
    );
    // …and it is the *first* twelve, in query order.
    assert!(frames.iter().any(|f| f.title == "fn sym_00"));
    assert!(frames.iter().all(|f| f.title != "fn sym_12"));
}

/// The reference scan bounds what any ONE file may cost it: a file past the
/// 400 KiB per-file ceiling is skipped outright — the ceiling is on the
/// scan's read, not on indexing, so the oversized file is still in the graph.
#[test]
fn reference_scan_skips_files_over_the_byte_ceiling() {
    let ws = TempDir::new().unwrap();
    let dbdir = TempDir::new().unwrap();
    fs::write(
        ws.path().join("small.rs"),
        "pub fn uses() { special_needle(); }\n",
    )
    .unwrap();
    // Well past the ceiling, in ordinary short lines so the minified-content
    // filter does not exclude it — indexable, just too large to scan.
    let mut big = String::new();
    while big.len() <= 450 * 1024 {
        big.push_str("pub fn filler() { special_needle(); }\n");
    }
    fs::write(ws.path().join("big.rs"), &big).unwrap();
    let graph = CodeGraph::open(ws.path(), &dbdir.path().join("context.db")).unwrap();
    graph.index_all().unwrap();
    assert_eq!(graph.file_count().unwrap(), 2, "the big file IS indexed");

    let frames = graph.references("special_needle").unwrap();
    assert!(!frames.is_empty(), "the small file still answers");
    assert!(
        frames.iter().all(|f| !f.id.contains("big.rs")),
        "an oversized file must not be read by the reference scan"
    );
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
