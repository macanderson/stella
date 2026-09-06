// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The renderer links no tool executor, and says so in a test.
//!
//! This crate draws a tool row, an approval card, and an index-hold notice.
//! To do that it once took `stella-tools`. That edge carried the whole
//! executor: a bundled SQLite and nine tree-sitter grammar builds, by way of
//! the code-graph index the `search` tool ranks over. A screen paid for a code
//! indexer it never calls.
//!
//! The shared half lives in `stella-tool-facts` now. This file holds the edge
//! shut. It reads the manifest rather than a call graph, so it fails the
//! moment the line comes back.
//!
//! `[dev-dependencies]` is out of scope. `tests/hello_panel_demo.rs` drives a
//! real plugin process through `stella-runtime`, which does take
//! `stella-tools`. That edge builds tests and never the shipped renderer, and
//! the manifest says why.
//!
//! Shaped after `stella-runtime`'s `no_policy_crate_edge.rs`, which holds the
//! same kind of line for the assembly seam.

/// Crates this renderer must never take in `[dependencies]`, and why.
///
/// A list, so the next one is added here rather than worked out again. Each
/// row names a crate that exists, which `every_forbidden_crate_still_exists`
/// holds: a row for a deleted crate can never fail.
const FORBIDDEN_DEPENDENCIES: &[(&str, &str)] = &[(
    "stella-tools",
    "the tool executor brings a code-graph index, a bundled SQLite and nine \
     tree-sitter grammar builds; the facts a screen reads live in \
     stella-tool-facts",
)];

/// Every dependency a manifest declares, paired with the table it sits under.
///
/// Hand-parsed. The shape is one line per dependency under a known header,
/// and taking a TOML crate to read that is a worse trade than the parse.
/// `env!("CARGO_MANIFEST_DIR")` is resolved by the compiler, so this reads no
/// ambient state.
fn declared_dependencies(manifest_path: &std::path::Path) -> Vec<(String, String)> {
    let manifest = std::fs::read_to_string(manifest_path)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", manifest_path.display()));

    let mut declared = Vec::new();
    let mut table: Option<String> = None;
    for line in manifest.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            table = header.ends_with("dependencies").then(|| header.to_string());
            continue;
        }
        let Some(header) = &table else { continue };
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = line.split_once('=') {
            declared.push((header.clone(), name.trim().trim_matches('"').to_string()));
        }
    }
    declared
}

fn own_manifest() -> std::path::PathBuf {
    std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
}

#[test]
fn the_renderer_declares_no_edge_to_the_tool_executor() {
    let declared = declared_dependencies(&own_manifest());

    // Guard the parser. A reformat that made the scan return nothing would
    // make the assertion below true for the wrong reason.
    assert!(
        declared.iter().any(|(_, name)| name == "stella-protocol"),
        "parsed no `stella-protocol` edge, so the scan is broken rather than \
         clean; declared = {declared:?}"
    );

    for (forbidden, why) in FORBIDDEN_DEPENDENCIES {
        assert!(
            !declared
                .iter()
                .any(|(table, name)| table == "dependencies" && name == forbidden),
            "stella-tui declares `{forbidden}` as a build dependency: {why}"
        );
    }
}

/// The leaf stays a leaf. `stella-tool-facts` takes one workspace crate,
/// `stella-protocol`, and that is what makes it free for a screen to hold.
///
/// Without this, the edge could come back one hop down and nothing here would
/// see it.
#[test]
fn the_facts_crate_takes_one_workspace_edge() {
    let manifest = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../stella-tool-facts/Cargo.toml"
    ));
    let workspace_edges: Vec<String> = declared_dependencies(&manifest)
        .into_iter()
        .filter(|(table, name)| table == "dependencies" && name.starts_with("stella-"))
        .map(|(_, name)| name)
        .collect();

    assert_eq!(
        workspace_edges,
        vec!["stella-protocol".to_string()],
        "stella-tool-facts must take `stella-protocol` and nothing else, or a \
         screen stops being able to afford it"
    );
}

/// A forbidden row that names a crate nothing ships can never fail, so the
/// list would rot into prose. This is what catches that.
#[test]
fn every_forbidden_crate_still_exists() {
    let crates_dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."));
    for (forbidden, _) in FORBIDDEN_DEPENDENCIES {
        assert!(
            crates_dir.join(forbidden).join("Cargo.toml").is_file(),
            "`{forbidden}` is on the forbidden list but is not a crate here, \
             so that row can never fail — drop it or repoint it"
        );
    }
}
