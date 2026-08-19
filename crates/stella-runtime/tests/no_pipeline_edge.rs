//! `stella-runtime` does not depend on a staged-pipeline crate, and says so
//! executably.
//!
//! The crate this names, `stella-pipeline`, no longer exists — #3865 deleted
//! it from the workspace — so the assertion cannot fail today for the reason
//! it was written. It is kept rather than deleted because the rule it encodes
//! outlives the crate: this is the list of edges the assembly seam must never
//! take, and re-homing verification as a plugin is exactly the work most
//! likely to reintroduce one. A guard that goes quiet when its subject is
//! removed is how the edge comes back unnoticed.
//!
//! The runtime is the assembly bottom half that `stella-serve` and embedded
//! hosts link to obtain an engine without the CLI. The staged pipeline was a
//! *policy* layered on top of a loop, not part of the loop's construction —
//! so an edge from here to there made the pipeline look like part of the
//! runtime's contract when it was not. That is the exact coupling
//! `doc:turn-lane-assembly` §10.2 counts and `doc:pipeline-as-plugins` §0.4
//! is trying to keep out of the seam: a wrapper that `stella-cli` can drive
//! and `stella-serve` cannot is a CLI feature wearing a socket's name.
//!
//! It also costs real time. `scripts/impacted-crates.sh` walks *declared*
//! dependencies, so a phantom edge drags this crate and everything
//! downstream of it into `CARGO_SCOPE` whenever the pipeline changes, and
//! costs every consumer a compile of a tree it never calls (#3280).
//!
//! This was the witness for a subtraction, which cannot be witnessed by
//! calling anything: the assertion is over the manifest, and it failed before
//! the dependency line was deleted and passed after. Lives in `tests/`
//! alongside `no_ambient_reads.rs`, and for the same reason — an in-crate
//! test would have to exempt its own needles from the scan.

/// Workspace crates the runtime must never declare an edge to, and why.
///
/// Deliberately a list rather than a single check: the next crate that must
/// stay out of the assembly seam is added here, not re-derived.
const FORBIDDEN_DEPENDENCIES: &[(&str, &str)] = &[(
    "stella-pipeline",
    "the staged pipeline was a policy above the loop, not part of assembling \
     one — see the module doc and #3280. The crate itself is gone (#3865); \
     the name stays listed so re-homing verification cannot quietly bring \
     the edge back",
)];

/// Dependency names declared by this crate's own manifest.
///
/// Hand-parsed rather than pulled through a TOML dependency: the shape being
/// read is one line per dependency in a known table, and adding a crate to
/// `[dev-dependencies]` to read four lines is a worse trade than the parse.
/// `env!("CARGO_MANIFEST_DIR")` is resolved by the compiler, so this reads no
/// ambient process state and does not disturb `no_ambient_reads.rs`.
fn declared_dependencies() -> Vec<String> {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("the crate's own Cargo.toml is readable");

    let mut names = Vec::new();
    let mut in_dependency_table = false;
    for line in manifest.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            // `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`
            // and their `[target.'…'.dependencies]` forms all count: a
            // phantom edge in any of them still reaches the resolver.
            in_dependency_table = header.ends_with("dependencies");
            continue;
        }
        if !in_dependency_table || line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = line.split_once('=') {
            names.push(name.trim().trim_matches('"').to_string());
        }
    }
    names
}

#[test]
fn the_assembly_seam_declares_no_edge_to_a_policy_crate() {
    let declared = declared_dependencies();

    // Guard the parser itself: a rename or a reformat that made
    // `declared_dependencies` return nothing would make the assertion below
    // vacuously true, which is the failure mode a subtraction test is most
    // exposed to.
    assert!(
        declared.iter().any(|d| d == "stella-core"),
        "parsed no `stella-core` edge out of the manifest, so the scan is \
         broken rather than clean; declared = {declared:?}"
    );

    for (forbidden, why) in FORBIDDEN_DEPENDENCIES {
        assert!(
            !declared.iter().any(|d| d == forbidden),
            "stella-runtime declares `{forbidden}`: {why}"
        );
    }
}
