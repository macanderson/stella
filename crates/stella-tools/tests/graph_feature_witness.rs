// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **Witness (`#6286`).** The tools this build offers are the tools it built.
//!
//! Every rung of `search` reads the code-graph index. Turn the `graph`
//! feature off and there is no `search` to register.
//!
//! The risk is not the missing tool. It is the table still listing it. The
//! prompt's tool block, `custom::RESERVED_NAMES` and the doc generator all
//! read `catalog::CATALOG`, and none of them sees a `cfg`.
//!
//! `stella_tools::BUILD_FEATURES` says what this build compiled.
//! `catalog::Availability::BuildFeature` says what a row needs. This test
//! runs either way and checks that the two agree.

use stella_tools::{BUILD_FEATURES, ToolRegistry, catalog};

/// The one row this feature governs.
const CONDITIONAL_TOOL: &str = "search";

fn advertised_names() -> Vec<String> {
    let root = tempfile::tempdir().expect("a temp workspace root");
    let registry = ToolRegistry::new(root.path().to_path_buf());
    registry
        .schemas()
        .iter()
        .map(|schema| schema.name.clone())
        .collect()
}

#[test]
fn the_registry_advertises_exactly_what_this_build_compiled() {
    let advertised = advertised_names();
    let declared = catalog::registered_with(BUILD_FEATURES);
    assert_eq!(
        advertised, declared,
        "the live registry and the catalog disagree about what this build \
         registers — a host would be handed a schema for a tool it does not \
         have, or denied one it does"
    );
}

#[test]
fn the_conditional_tool_follows_its_feature() {
    let advertised = advertised_names();
    let compiled = BUILD_FEATURES.contains(&"graph");
    assert_eq!(
        advertised.iter().any(|name| name == CONDITIONAL_TOOL),
        compiled,
        "`{CONDITIONAL_TOOL}` must register when the `graph` feature is \
         compiled and not otherwise; BUILD_FEATURES = {BUILD_FEATURES:?}"
    );
    // The rest of the surface is unconditional, so a build with no features
    // still carries every other row.
    for name in catalog::always_on() {
        assert!(
            advertised.iter().any(|live| live == name),
            "`{name}` is unconditional and must register in every build"
        );
    }
}
