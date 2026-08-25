// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a search is allowed to see, and what it must not.
//!
//! A sibling of the ladder tests rather than part of them: `tests.rs` sits
//! against the 1500-line ceiling, and this is its own subject — not whether a
//! rung ranks well, but which files reach a rung at all.
//!
//! Every assertion here is two-sided by construction. A test that only proves
//! a file *is* found would pass just as happily if the walk had stopped
//! excluding anything, so each fixture plants a decoy carrying the same query
//! terms in the place that must stay unreachable.
//!
//! #3162 was answered here for the scan rung and left the three graph-backed
//! rungs unanswered, because `.toml` named no `stella_graph::Language` and a
//! record reached no index (#4492). The four tests below its own are that
//! second half: what the index holds, and one witness per rung.

use super::*;

/// A published context record, in the shape
/// `docs/spec/adaptive-context/context-pr.md` §6.1 gives one: a preamble, the
/// `[[record]]` entry carrying the statement, and the dotted sub-tables that
/// say how it steers.
///
/// Its statement is deliberately about redaction, so [`ConceptEmbedder`]
/// projects it onto the same dimensions the semantic witness queries.
const RECORD: &str = "schema = \"context-record/v0.1\"\n\
     set_id = \"demo\"\n\
     \n\
     [[record]]\n\
     lineage_id = \"ctx.demo.redaction\"\n\
     kind = \"constraint\"\n\
     statement = \"Every emitted diagnostic must scrub the confidential \
     credential it carries.\"\n\
     \n\
     [record.steering]\n\
     force = \"must\"\n\
     precedence = 100\n\
     \n\
     [record.steering.applies_to]\n\
     keywords = [\"redaction\"]\n";

/// An ordinary indexed file with nothing in common with [`RECORD`] — the
/// thing every rung below ranks the record *against*, so a hit is a choice
/// rather than the only row in the index.
const NEIGHBOUR: &str = "//! Matrix arithmetic.\n\
     pub struct Matrix;\n\
     pub fn multiply(a: &Matrix, b: &Matrix) -> Matrix {\n\
     a.rows().sum(b)\n\
     }\n";

/// The two files that carry the record's own words and must never reach the
/// index: a manifest sharing its extension, and the private state directory.
const DECOY: &str =
    "# Every emitted diagnostic must scrub the confidential credential it carries.\n";

/// Write the record fixture and index it, exactly as `stella init` would.
/// Returns the canonical root and an open graph; the caller shuts it down.
fn indexed_records(workspace: &std::path::Path) -> (std::path::PathBuf, CodeGraph) {
    fs::create_dir_all(workspace.join(".stella/rules")).expect("mkdir rules");
    fs::create_dir_all(workspace.join(".stella/private")).expect("mkdir private");
    fs::create_dir_all(workspace.join("src")).expect("mkdir src");
    fs::write(workspace.join(".stella/rules/ctx.demo.toml"), RECORD).expect("write the record");
    fs::write(workspace.join("src/tally.rs"), NEIGHBOUR).expect("write the neighbour");
    // Both decoys say what the record says, so their absence from the index is
    // a refusal rather than a coincidence.
    fs::write(workspace.join("Cargo.toml"), DECOY).expect("write the manifest decoy");
    fs::write(workspace.join(".stella/private/notes.toml"), DECOY).expect("write private state");
    let root = workspace.canonicalize().expect("canonicalize");
    let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
    graph.index_all().expect("index");
    (root, graph)
}

/// The paths a search answered with, in order.
fn paths(answer: &Answer) -> Vec<&str> {
    answer.hits.iter().map(|hit| hit.path.as_str()).collect()
}

/// **The index half of #4492.** A `.toml` under `.stella/rules/` is a document
/// the graph holds, chunked by its table headers; every other `.toml` in the
/// tree is not. One walk asserts each, because either alone is satisfiable by
/// the wrong answer — indexing every `.toml` proves the record reachable,
/// indexing none proves the manifest refused.
#[test]
fn the_graph_holds_context_records_and_no_other_toml() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = indexed_records(workspace.path());

    let mut files = graph.all_files().expect("all_files");
    files.sort();
    assert_eq!(
        files,
        vec![
            ".stella/rules/ctx.demo.toml".to_string(),
            "src/tally.rs".to_string()
        ],
        "the records are indexed; `Cargo.toml` and `.stella/private/` are not"
    );

    let neighborhood = graph
        .file_neighborhood(std::path::Path::new(".stella/rules/ctx.demo.toml"))
        .expect("the record has a neighborhood");
    let mut names: Vec<String> = neighborhood
        .symbols
        .iter()
        .map(|symbol| symbol.name.clone())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "record".to_string(),
            "record.steering".to_string(),
            "record.steering.applies_to".to_string()
        ],
        "a record is chunked by its table headers, not held as one blob"
    );

    graph.shutdown();
    drop(root);
}

/// **Rung one: exact symbol.** `[[record]]` is a name the graph knows, so
/// asking for it is a lookup rather than a ranking — the same contract a Rust
/// `fn` gets.
#[tokio::test]
async fn the_exact_symbol_rung_reaches_a_records_table() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = indexed_records(workspace.path());

    let answer = dispatch(Some(&graph), &root, "record", None).await;
    assert_eq!(
        answer.strategies,
        vec![Strategy::ExactSymbol],
        "note={:?}",
        answer.note
    );
    assert_eq!(paths(&answer), vec![".stella/rules/ctx.demo.toml"]);

    graph.shutdown();
}

/// **Rung two: graph names.** A prose query names no symbol exactly, so it
/// falls to the name ranking — which now has a record's table keys and its
/// path to rank, where before it had nothing at all for this file.
#[tokio::test]
async fn the_graph_name_rung_reaches_a_records_steering_tables() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = indexed_records(workspace.path());

    let answer = dispatch(Some(&graph), &root, "steering rules", None).await;
    assert_eq!(
        answer.strategies,
        vec![Strategy::GraphNames],
        "note={:?}",
        answer.note
    );
    assert_eq!(paths(&answer), vec![".stella/rules/ctx.demo.toml"]);

    graph.shutdown();
}

/// **Rung three: semantic.** The witness the issue asks for by name — a
/// record surfaces on its *rationale prose*, with no query word appearing in
/// its path or its table keys. The premise is asserted first, so a fixture
/// that drifted into being name-solvable would fail loudly rather than pass
/// vacuously.
#[tokio::test]
async fn the_semantic_rung_reaches_a_record_by_its_prose_alone() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = indexed_records(workspace.path());

    const QUESTION: &str = "sensitive passwords redacted before logging";
    let record_path = ".stella/rules/ctx.demo.toml";
    for word in QUESTION.split_whitespace() {
        assert!(
            !record_path.contains(word),
            "`{word}` appears in the record's path — a name match would find it and this \
             test would prove nothing"
        );
        assert!(
            !RECORD.to_lowercase().contains(word),
            "`{word}` appears in the record itself — a term match would find it and this \
             test would prove nothing"
        );
    }

    crate::search::backfill::backfill_opened(&graph, &ConceptEmbedder, &mut |_| {}).await;
    let answer = dispatch(Some(&graph), &root, QUESTION, Some(&ConceptEmbedder)).await;
    assert_eq!(
        answer.strategies,
        vec![Strategy::Semantic],
        "note={:?}",
        answer.note
    );
    assert_eq!(
        answer.hits.first().map(|hit| hit.path.as_str()),
        Some(record_path),
        "the record's rationale is the only thing that answers this: {:?}",
        answer.hits
    );

    graph.shutdown();
}

/// Witness for #3162. `.stella/` was skipped wholesale, so the published
/// context records were invisible to every rung — including the one that
/// needs no index at all. They are the one part of `.stella/` tracked in Git,
/// because a record only steers a teammate's session if it travels with the
/// repository, and a retrieval tool that cannot see the repository's own
/// steering policy is the hole the issue names.
///
/// The other half is what must stay invisible, and the fixture asserts both
/// in one walk: `.stella/private/` holds SQLite state and OAuth tokens, and
/// `.stella/settings.json` sits directly on the path the walk now crosses to
/// reach the records.
#[test]
fn the_scan_rung_sees_context_records_and_still_never_sees_private_state() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = workspace.path().canonicalize().expect("canonicalize");
    fs::create_dir_all(root.join(".stella").join("rules")).expect("mkdir rules");
    fs::create_dir_all(root.join(".stella").join("private")).expect("mkdir private");
    fs::write(
        root.join(".stella").join("rules").join("ctx.demo.toml"),
        "[[record]]\nstatement = \"a provider adapter never reaches into the engine\"\n",
    )
    .expect("write the record");
    // Both of these mention the query, so their absence from the answer is a
    // refusal rather than a coincidence.
    fs::write(
        root.join(".stella").join("settings.json"),
        "{\"note\": \"provider adapter\"}",
    )
    .expect("write settings");
    fs::write(
        root.join(".stella").join("private").join("notes.jsonl"),
        "{\"lesson\": \"provider adapter\"}",
    )
    .expect("write private state");

    let outcome = scan::scan_hits_bounded(&root, "provider adapter", 10, 100);
    let paths: Vec<&str> = outcome.hits.iter().map(|hit| hit.path.as_str()).collect();
    assert!(
        paths.contains(&".stella/rules/ctx.demo.toml"),
        "a context record must be reachable by the scan rung (#3162): {paths:?}"
    );
    assert!(
        !paths
            .iter()
            .any(|path| path.starts_with(".stella/private/")),
        "`.stella/private/` must stay unsearchable: {paths:?}"
    );
    assert!(
        !paths.contains(&".stella/settings.json"),
        "crossing `.stella` to reach the records must not scan `.stella` itself: {paths:?}"
    );
}

/// A document whose headings nest, so its sections carry a real breadcrumb
/// rather than a single level.
const DOCUMENT: &str = "# Architecture\n\
     \n\
     Ports, not direct dependencies.\n\
     \n\
     ## 8. Provider feature parity\n\
     \n\
     Providers diverge in sneaky ways, and this is guarded on six axes.\n";

/// A second document carrying the same words at top level, so answering with
/// the nested section is a choice between two indexed files rather than the
/// only row available.
const DOCUMENT_DECOY: &str = "# Provider feature parity\n\
     \n\
     Providers diverge in sneaky ways.\n";

/// Write and index the two documents. Returns the canonical root and an open
/// graph; the caller shuts it down.
fn indexed_documents(workspace: &std::path::Path) -> (std::path::PathBuf, CodeGraph) {
    fs::write(workspace.join("NOTES.md"), DOCUMENT).expect("write the document");
    fs::write(workspace.join("OTHER.md"), DOCUMENT_DECOY).expect("write the decoy");
    let root = workspace.canonicalize().expect("canonicalize");
    let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
    graph.index_all().expect("index");
    (root, graph)
}

/// **Witness for #4574.** A markdown section is reachable by the rung that
/// answers with a fact.
///
/// Its name is a breadcrumb, and `is_bare_identifier` refused every one of
/// those, so no section had reached this rung since #3103 put them in the
/// index — every citation an agent wrote was a ranking. The decoy carries the
/// same words at top level, so the answer distinguishes the nested section
/// from a document merely about it.
#[tokio::test]
async fn the_exact_symbol_rung_reaches_a_nested_markdown_section() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = indexed_documents(workspace.path());

    let breadcrumb = format!(
        "Architecture{}8. Provider feature parity",
        stella_graph::BREADCRUMB_SEPARATOR
    );
    let answer = dispatch(Some(&graph), &root, &breadcrumb, None).await;
    assert_eq!(
        answer.strategies,
        vec![Strategy::ExactSymbol],
        "note={:?}",
        answer.note
    );
    assert_eq!(paths(&answer), vec!["NOTES.md"]);

    graph.shutdown();
}

/// The citation form: `stella-graph` stores a section's name without the file
/// path and says a citation composes the two, so the shape a reader actually
/// writes must reach the same fact.
#[tokio::test]
async fn a_path_prefixed_citation_reaches_the_same_section() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = indexed_documents(workspace.path());

    let separator = stella_graph::BREADCRUMB_SEPARATOR;
    let citation = format!("NOTES.md{separator}Architecture{separator}8. Provider feature parity");
    let answer = dispatch(Some(&graph), &root, &citation, None).await;
    assert_eq!(
        answer.strategies,
        vec![Strategy::ExactSymbol],
        "note={:?}",
        answer.note
    );
    assert_eq!(paths(&answer), vec!["NOTES.md"]);

    graph.shutdown();
}

/// The refusal half of #4574: a dotted table key is still a pattern, so it
/// still falls through to the ranking rung rather than becoming a lookup.
///
/// Censused over 786 real queries, a dotted rule newly admits five terms and
/// all five are a filename or a regex `.` — see `enrich::is_bare_identifier`.
/// This is what fails if that decision is reversed without re-measuring.
#[tokio::test]
async fn a_dotted_table_key_is_still_answered_by_ranking_not_lookup() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let (root, graph) = indexed_records(workspace.path());

    let answer = dispatch(Some(&graph), &root, "record.steering.applies_to", None).await;
    assert_eq!(
        answer.strategies,
        vec![Strategy::GraphNames],
        "note={:?}",
        answer.note
    );

    graph.shutdown();
}
