//! Registry unit tests: construction of the tool set, the read-only /
//! speculation-safe schema claims, and per-tool dispatch.

use super::*;

/// A registry rooted in a fresh empty tempdir. Rooting tests at a shared
/// path like `/tmp` is not hermetic. The `TempDir` is returned so the root
/// outlives the registry.
fn bare_registry() -> (tempfile::TempDir, ToolRegistry) {
    let root = tempfile::tempdir().unwrap();
    let reg = ToolRegistry::new(root.path().to_path_buf());
    (root, reg)
}

#[tokio::test]
async fn unknown_tool_returns_error_not_panic() {
    let (_root, reg) = bare_registry();
    let result = reg.execute("nonexistent", &Value::Null).await;
    assert!(result.is_error());
}

/// The class a refusal carries (#3145): the unknown-tool refusal is
/// `NotFound`, not an unclassified error — "the model named a tool that is
/// not on the menu" is model misuse, and a per-tool error-rate ceiling reads
/// the class without matching on prose.
#[tokio::test]
async fn unknown_tool_refusal_is_classified_not_found() {
    use stella_protocol::ErrorClass;
    let (_root, reg) = bare_registry();
    let out = reg.execute("no_such_tool", &serde_json::json!({})).await;
    let ToolOutput::Error { class, .. } = out else {
        panic!("an unknown tool must refuse");
    };
    assert_eq!(class, Some(ErrorClass::NotFound));
}

#[test]
fn every_registry_tool_is_reserved_against_custom_shadowing() {
    // RESERVED_NAMES is an alias for catalog::ALL_NAMES rather than a
    // hand-mirrored copy (#450), so declaring a tool reserves it. What
    // still needs proving is the *other* direction: a tool that registers
    // but was never declared is shadowable by a custom manifest.
    let (_root, reg) = bare_registry();
    for schema in reg.schemas() {
        assert!(
            crate::custom::RESERVED_NAMES.contains(&schema.name.as_str()),
            "built-in tool `{}` is missing from custom::RESERVED_NAMES — \
             declare it in stella-tools/src/catalog.rs, or a custom \
             manifest could shadow it",
            schema.name
        );
    }
}

/// Witness for #450: the advertised set is pinned as an exact **name set**
/// against the canonical catalog, not as a count. A magic-number pin
/// (`names.len() == 12`) merges to a plausible-but-wrong integer when two
/// parallel PRs each bump it off the same base — this fails by name
/// instead, naming exactly what was added or dropped.
#[test]
fn registry_advertises_exactly_the_catalog_tool_set() {
    let (_root, reg) = bare_registry();
    let schemas = reg.schemas();
    let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        crate::catalog::always_on(),
        "registry disagrees with catalog::CATALOG — register the tool, or \
         add/remove its line in stella-tools/src/catalog.rs"
    );
}

/// Witness for #450: a tool that registers but was never declared in the
/// catalog is caught by *name*. Simulated here by dropping a name from the
/// expectation — the same failure a real unregistered tool produces.
#[test]
fn an_undeclared_tool_fails_the_catalog_pin_by_name() {
    let (_root, reg) = bare_registry();
    let schemas = reg.schemas();
    let live: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
    let mut catalog_missing_one = crate::catalog::always_on();
    let dropped = catalog_missing_one.pop().expect("catalog is non-empty");
    assert_ne!(
        live, catalog_missing_one,
        "a catalog missing `{dropped}` must not compare equal to the live \
         registry — otherwise the pin cannot catch an undeclared tool"
    );
    // And the difference is reported as a name, not an arity.
    assert!(!catalog_missing_one.contains(&dropped));
    assert!(live.contains(&dropped));
}

/// The schema list is serialized verbatim into the prompt prefix; a
/// nondeterministic order (HashMap iteration) breaks byte-level
/// prompt-cache prefix matching across processes.
#[test]
fn schemas_are_sorted_by_name_for_prompt_cache_stability() {
    let (_root, reg) = bare_registry();
    let names: Vec<String> = reg.schemas().iter().map(|s| s.name.clone()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "schemas must be name-sorted");
}

/// Every advertised schema's `read_only` and `speculation_safe` flags match
/// the catalog's declared row. The engine parallelizes and speculates on
/// these bits, so a tool whose live schema drifts from its declaration would
/// corrupt the dispatch contract silently.
#[test]
fn read_only_flags_partition_the_registry_correctly() {
    let (_root, reg) = bare_registry();
    for schema in reg.schemas() {
        let entry = crate::catalog::get(&schema.name)
            .unwrap_or_else(|| panic!("`{}` has no catalog row", schema.name));
        assert_eq!(
            schema.read_only, entry.read_only,
            "`{}`: live schema and catalog disagree on read_only",
            schema.name
        );
        assert_eq!(
            schema.speculation_safe, entry.speculation_safe,
            "`{}`: live schema and catalog disagree on speculation_safe",
            schema.name
        );
    }
}

/// The sub-agent spawn tool is the one built-in claiming
/// [`Tool::parallel_safe`]: its children are read-only by construction and
/// its dispatcher carves budget per child. Nothing else may claim it
/// without the same construction argument.
#[test]
fn the_subagent_spawn_tool_is_the_only_parallel_safe_builtin() {
    let (_root, reg) = bare_registry();
    let names = ToolExecutor::parallel_safe_names(&reg);
    assert_eq!(
        names,
        std::collections::HashSet::from(["task".to_string()]),
        "parallel_safe is a per-tool construction argument, not a default"
    );
}

/// The scratch state plane round-trips through registry dispatch: a saved
/// entry is listed, read back, and deleted through the same `execute` seam
/// the engine drives.
#[tokio::test]
async fn state_tools_round_trip_through_registry_dispatch() {
    let (_root, reg) = bare_registry();
    let out = reg
        .execute(
            "save_state",
            &serde_json::json!({"key": "probe", "content": "forty-two"}),
        )
        .await;
    assert!(!out.is_error(), "{out:?}");

    let ToolOutput::Ok { content } = reg.execute("list_state", &serde_json::json!({})).await else {
        panic!("list_state must succeed");
    };
    assert!(content.contains("probe"), "{content}");

    let ToolOutput::Ok { content } = reg
        .execute("get_state", &serde_json::json!({"key": "probe"}))
        .await
    else {
        panic!("get_state must succeed");
    };
    assert!(content.contains("forty-two"), "{content}");

    let out = reg
        .execute("delete_state", &serde_json::json!({"key": "probe"}))
        .await;
    assert!(!out.is_error(), "{out:?}");
    let out = reg
        .execute("get_state", &serde_json::json!({"key": "probe"}))
        .await;
    assert!(out.is_error(), "a deleted key must not read back");
}

/// `get_environment` dispatches through the registry and reports the scratch
/// directory the registry created — the one fact the CLI's prompt block
/// cannot carry (#3102).
#[tokio::test]
async fn get_environment_reports_the_registrys_scratch_directory() {
    let (_root, reg) = bare_registry();
    let ToolOutput::Ok { content } = reg.execute("get_environment", &serde_json::json!({})).await
    else {
        panic!("get_environment must succeed with zero arguments");
    };
    assert!(
        content.contains("Scratch directory: ") && !content.contains("unavailable this session"),
        "the registry-owned scratch dir must be reported by path: {content}"
    );
}
