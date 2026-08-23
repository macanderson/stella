//! `stella-runtime` declares no edge to a crate that is *policy above the
//! loop*, and says so executably.
//!
//! The runtime is the assembly bottom half that `stella-serve` and embedded
//! hosts link to obtain an engine without the CLI. Orchestration layered on
//! top of a loop — a verification pipeline, a fan-out planner, a renderer —
//! is not part of the loop's construction, so an edge from here to there
//! makes that policy look like part of the runtime's contract when it is not.
//! That is the exact coupling `doc:turn-lane-assembly` §10.2 counts and
//! `doc:pipeline-as-plugins` §0.4 is trying to keep out of the seam: a
//! wrapper that `stella-cli` can drive and `stella-serve` cannot is a CLI
//! feature wearing a socket's name.
//!
//! It also costs real time. `scripts/impacted-crates.sh` walks *declared*
//! dependencies, so a phantom edge drags this crate and everything
//! downstream of it into `CARGO_SCOPE` whenever the policy crate changes, and
//! costs every consumer a compile of a tree it never calls (#3280).
//!
//! That second cost does not need the edge to be a *policy* edge, which is
//! why this file carries a second, broader assertion: a declared workspace
//! edge nothing in `src/` names is a phantom edge whatever it points at.
//! `stella-context`, `stella-graph` and `stella-mcp` were all three declared
//! here and referenced nowhere, so the crate paid for tree-sitter indexing, an
//! embedder and an MCP client on every build of every consumer, and
//! `impacted-crates.sh` widened `CARGO_SCOPE` to this crate whenever any of
//! the three changed (#3732). A comment could have justified them; none did.
//!
//! This began (#3280) as the witness for one subtraction — the edge to the
//! built-in staged pipeline, which is why the file was called
//! `no_pipeline_edge.rs`. That crate was deleted from the workspace outright
//! (#3865), which would have left the assertion unable to fail for the rest of
//! its life: nobody can declare a dependency on a crate that does not exist.
//! A guard that cannot fail is documentation wearing a test's name, so the
//! row was replaced with the crates that *do* exist and must stay out of the
//! seam. The assertion is still over the manifest rather than over a call,
//! and it still fails the moment the line is added. Lives in `tests/`
//! alongside `no_ambient_reads.rs`, and for the same reason — an in-crate
//! test would have to exempt its own needles from the scan.

/// Workspace crates the runtime must never declare an edge to, and why.
///
/// Deliberately a list rather than a single check: the next crate that must
/// stay out of the assembly seam is added here, not re-derived.
///
/// Each row must name a crate that **exists in this workspace**: a row for a
/// deleted crate is unfailable, which is what happened to this list's original
/// `stella-pipeline` row (#3865). `every_forbidden_crate_still_exists` holds
/// that.
const FORBIDDEN_DEPENDENCIES: &[(&str, &str)] = &[
    (
        "stella-cli",
        "the shipping binary is a consumer of the seam, never a part of it — \
         an edge here is the inversion #3280 names",
    ),
    (
        "stella-fleet",
        "multi-agent fan-out is a policy above the loop, not part of \
         assembling one — see the module doc and #3280",
    ),
    (
        "stella-tui",
        "a renderer in the assembly seam is a terminal dependency every \
         headless host would inherit",
    ),
];

/// Every dependency this crate's own manifest declares, paired with the table
/// header it was declared under.
///
/// Hand-parsed rather than pulled through a TOML dependency: the shape being
/// read is one line per dependency in a known table, and adding a crate to
/// `[dev-dependencies]` to read four lines is a worse trade than the parse.
/// `env!("CARGO_MANIFEST_DIR")` is resolved by the compiler, so this reads no
/// ambient process state and does not disturb `no_ambient_reads.rs`.
///
/// The header comes back because the two assertions below want different
/// slices of the same parse: a forbidden edge is forbidden wherever it is
/// declared, while a phantom edge is a claim about `[dependencies]` alone —
/// a `[dev-dependencies]` entry is *meant* to be invisible to `src/`.
fn declared_dependencies() -> Vec<(String, String)> {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("the crate's own Cargo.toml is readable");

    let mut declared = Vec::new();
    let mut table: Option<String> = None;
    for line in manifest.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            // `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`
            // and their `[target.'…'.dependencies]` forms all count: a
            // phantom edge in any of them still reaches the resolver.
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

#[test]
fn the_assembly_seam_declares_no_edge_to_a_policy_crate() {
    let declared = declared_dependencies();

    // Guard the parser itself: a rename or a reformat that made
    // `declared_dependencies` return nothing would make the assertion below
    // vacuously true, which is the failure mode a subtraction test is most
    // exposed to.
    assert!(
        declared.iter().any(|(_, name)| name == "stella-core"),
        "parsed no `stella-core` edge out of the manifest, so the scan is \
         broken rather than clean; declared = {declared:?}"
    );

    for (forbidden, why) in FORBIDDEN_DEPENDENCIES {
        assert!(
            !declared.iter().any(|(_, name)| name == forbidden),
            "stella-runtime declares `{forbidden}`: {why}"
        );
    }
}

/// Every workspace crate `[dependencies]` declares is named somewhere in
/// `src/` — the subtraction #3732 asked for, kept from coming back.
///
/// A phantom edge is invisible to every other guard in the tree: it compiles,
/// it lints clean, and `cargo` has no opinion about a dependency nothing
/// imports. What it costs is paid by everyone else — the build of the tree it
/// pulls in, and a `CARGO_SCOPE` that widens to this crate whenever the crate
/// it never calls changes.
///
/// Scoped to `[dependencies]` and to `src/` deliberately, in both directions:
/// a `[dev-dependencies]` entry is *supposed* to be absent from `src/`, and a
/// `[dependencies]` entry used only by `tests/` is a dev-dependency that was
/// filed in the wrong table.
#[test]
fn every_declared_workspace_edge_is_named_in_the_source() {
    let source = {
        let mut text = String::new();
        let mut stack = vec![std::path::PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src"
        ))];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("the crate has a src/ directory") {
                let path = entry.expect("a readable source entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    text.push_str(&std::fs::read_to_string(&path).expect("a readable module"));
                }
            }
        }
        text
    };
    assert!(
        source.contains("stella_plugin"),
        "read no source, so the scan is broken rather than clean"
    );

    for (table, name) in declared_dependencies() {
        if table != "dependencies" || !name.starts_with("stella-") {
            continue;
        }
        assert!(
            source.contains(&name.replace('-', "_")),
            "stella-runtime declares `{name}` and names it nowhere in src/. \
             Drop the line, or move it to [dev-dependencies] if only the tests \
             need it — an edge that costs every consumer a compile of a tree \
             this crate never calls is the defect #3732 named."
        );
    }
}

/// The other half of the same discipline: a forbidden row naming a crate that
/// no longer exists can never fail, so the list would rot into prose without
/// anyone noticing. This is the check that caught it once already — the
/// original `stella-pipeline` row outlived its crate (#3865).
#[test]
fn every_forbidden_crate_still_exists() {
    let crates_dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."));
    for (forbidden, _) in FORBIDDEN_DEPENDENCIES {
        assert!(
            crates_dir.join(forbidden).join("Cargo.toml").is_file(),
            "`{forbidden}` is on the forbidden list but is not a crate in this \
             workspace, so that row can never fail — drop it or repoint it"
        );
    }
}
