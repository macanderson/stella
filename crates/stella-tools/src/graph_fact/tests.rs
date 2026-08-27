//! The carrier's round trip, and the real index behind [`Codegraph`].

use super::*;

/// A workspace with one Rust file importing another, indexed.
fn indexed_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/helper.rs"),
        "pub fn thing() -> u8 {\n    7\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub mod helper;\nuse crate::helper::thing;\npub fn go() -> u8 {\n    thing()\n}\n",
    )
    .unwrap();
    crate::search::codegraph::open_or_build(root).unwrap();
    dir
}

/// The facts survive the wire beside a key another producer already wrote,
/// which is the shape every mutation output has: `changes` and `graph_facts`
/// ride the same object.
#[test]
fn attaching_facts_keeps_the_data_another_producer_already_wrote() {
    let fact = GraphFact::InboundRefs {
        path: "src/helper.rs".into(),
        inbound: 3,
    };
    let output = ToolOutput::ok_with_data("deleted src/helper.rs", serde_json::json!({"keep": 1}));
    let output = attach(output, std::slice::from_ref(&fact));
    let ToolOutput::Ok { content, data } = &output else {
        panic!("attach must keep the Ok arm");
    };
    assert_eq!(content, "deleted src/helper.rs");
    assert_eq!(data.as_ref().unwrap()["keep"], 1);
    assert_eq!(from_output(&output), vec![fact]);
}

/// Nothing measured means no key at all — never a present-and-empty payload
/// a consumer could read as "the check ran and found nothing".
#[test]
fn a_call_that_measured_no_fact_leaves_the_key_absent() {
    let output = attach(ToolOutput::ok("deleted a.txt"), &[]);
    let ToolOutput::Ok { data, .. } = &output else {
        panic!("attach must keep the Ok arm");
    };
    assert!(data.is_none(), "{data:?}");
    assert!(from_output(&output).is_empty());
}

/// A failed call carries no fact, and reading one out of it is empty rather
/// than an error at the seam.
#[test]
fn a_failed_output_carries_no_facts() {
    let output = attach(
        ToolOutput::error("could not delete"),
        &[GraphFact::Registered {
            path: "a.rs".into(),
        }],
    );
    assert!(output.is_error());
    assert!(from_output(&output).is_empty());
}

/// The real index, asked the real question: `src/lib.rs` imports
/// `src/helper.rs`, so the helper has one inbound reference and the library
/// root has none.
#[test]
fn the_workspace_index_counts_the_files_that_import_a_path() {
    let dir = indexed_workspace();
    let root = dir.path();
    assert_eq!(
        Codegraph.inbound_refs(root, &root.join("src/helper.rs")),
        Some(1)
    );
    assert_eq!(
        Codegraph.inbound_refs(root, &root.join("src/lib.rs")),
        Some(0)
    );
}

/// A path the index holds no node for has no count. `Some(0)` here would say
/// "nothing imports this file" about a file the graph has never seen.
#[test]
fn a_path_the_index_never_saw_has_no_count_rather_than_zero() {
    let dir = indexed_workspace();
    let root = dir.path();
    std::fs::write(root.join("src/fresh.rs"), "pub fn fresh() {}\n").unwrap();
    assert_eq!(
        Codegraph.inbound_refs(root, &root.join("src/fresh.rs")),
        None
    );
}

/// Registering is what closes that gap, and it answers about the node it
/// made rather than about the pass that made it.
#[test]
fn registering_a_created_file_makes_it_a_node() {
    let dir = indexed_workspace();
    let root = dir.path();
    let fresh = root.join("src/fresh.rs");
    std::fs::write(&fresh, "pub fn fresh() {}\n").unwrap();
    assert_eq!(Codegraph.register(root, &fresh), Some(true));
    assert_eq!(Codegraph.inbound_refs(root, &fresh), Some(0));
}

/// A file no grammar claims takes no node, and says so — `Some(false)`, not
/// a registration the index never made.
#[test]
fn a_file_the_index_takes_no_node_for_is_not_registered() {
    let dir = indexed_workspace();
    let root = dir.path();
    let notes = root.join("notes.log");
    std::fs::write(&notes, "nothing to parse\n").unwrap();
    assert_eq!(Codegraph.register(root, &notes), Some(false));
}

/// The elision this whole module exists for: a workspace nobody has indexed
/// answers neither question, and creates no index by being asked.
#[test]
fn a_workspace_with_no_code_graph_answers_neither_question() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("a.rs"), "pub fn a() {}\n").unwrap();
    assert_eq!(Codegraph.inbound_refs(root, &root.join("a.rs")), None);
    assert_eq!(Codegraph.register(root, &root.join("a.rs")), None);
    assert!(
        !root.join(".stella").exists(),
        "asking must not create an index"
    );
}
