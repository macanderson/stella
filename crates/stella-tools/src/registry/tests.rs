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

/// **Witness (#4175).** A solo mutating call is measured on its own, and the
/// three shapes that cannot be attributed to one call are not.
///
/// Before this, `FileChange` had one producer — the turn-boundary tree
/// snapshot — so a turn yielded exactly one aggregate change per path, landing
/// after every `ToolResult` had already folded. Every mutating row in the deck
/// therefore rendered with no diff and no `+N −M`, however many files it wrote
/// (#4155). This is the per-call producer's dispatch rule, which is the half
/// that decides whether an attribution is honest at all.
///
/// The exclusions are read from each tool's own schema rather than from a name
/// list, so a new tool inherits the right answer: see `crate::call_measure` for
/// why a concurrently-dispatched call must not be measured alone.
#[tokio::test]
async fn only_a_successful_solo_mutating_call_is_measured_on_its_own() {
    use crate::call_measure::CallMeasure;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Counting(AtomicUsize);
    impl CallMeasure for Counting {
        fn measure_and_publish(&self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    // The rule, over the tools this registry really ships — so the test also
    // proves the shipped declarations are the ones it depends on, rather than
    // proving a synthetic tool agrees with itself.
    let (_root, reg) = bare_registry();
    for (name, expected) in [
        ("write_file", true),
        ("edit_file", true),
        ("delete_file", true),
        // Not a file tool, and exactly why the producer cannot be a hook on
        // the file tools: `bash` mutates the tree naming no path in any schema.
        ("bash", true),
        ("read_file", false),
        ("search", false),
        // Mutating, but dispatched in a concurrent group with its siblings,
        // so no before/after reading can say which sibling wrote what.
        ("delegate", false),
        ("no_such_tool", false),
    ] {
        assert_eq!(
            reg.measures_alone(name),
            expected,
            "`{name}` should{} be measured on its own",
            if expected { "" } else { " NOT" }
        );
    }

    // And the success gate, driven through a real dispatch: a call that wrote
    // bytes measures, a read does not, and a failure does not.
    let write = serde_json::json!({
        "path": "a.txt",
        "content": "hello",
    });
    let counter = Arc::new(Counting::default());
    reg.attach_call_measure(counter.clone());

    assert!(!reg.execute("write_file", &write).await.is_error());
    assert_eq!(
        counter.0.load(Ordering::Relaxed),
        1,
        "a successful write measures the change it made"
    );

    reg.execute("read_file", &serde_json::json!({"path": "a.txt"}))
        .await;
    assert_eq!(
        counter.0.load(Ordering::Relaxed),
        1,
        "a read adds nothing to measure"
    );

    assert!(
        reg.execute("read_file", &serde_json::json!({"path": "gone.txt"}))
            .await
            .is_error(),
        "the failing shape this asserts on must really fail"
    );
    assert_eq!(
        counter.0.load(Ordering::Relaxed),
        1,
        "and a failed call attributes nothing — rendering the path's previous \
         diff under a ✗ would claim a change the call never made"
    );
}

/// Detaching the turn's stream drops the measurer with it.
///
/// They hold clones of one channel, so a measurer left attached keeps that
/// channel open — which is #960 exactly: a completed run printed its terminal
/// event and then hung, because a sender nobody remembered was still alive.
#[tokio::test]
async fn detaching_the_event_stream_drops_the_call_measurer_too() {
    use crate::call_measure::CallMeasure;

    struct Noop;
    impl CallMeasure for Noop {
        fn measure_and_publish(&self) {}
    }

    let (_root, reg) = bare_registry();
    reg.attach_call_measure(Arc::new(Noop));
    assert!(
        reg.call_measure.read().unwrap().is_some(),
        "attached for the turn"
    );

    reg.detach_event_stream();
    assert!(
        reg.call_measure.read().unwrap().is_none(),
        "and gone when the turn's stream closes — a live one would hold the \
         channel open after the renderer was meant to finish"
    );
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
        std::collections::HashSet::from(["delegate".to_string()]),
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

    let ToolOutput::Ok { content, .. } = reg.execute("list_state", &serde_json::json!({})).await
    else {
        panic!("list_state must succeed");
    };
    assert!(content.contains("probe"), "{content}");

    let ToolOutput::Ok { content, .. } = reg
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

/// Witness for #3297: two `save_state` calls writing *different same-length*
/// content under one key must render distinct success outputs. The engine's
/// stagnation detector (`stella-core`'s `loop_detect`, rung 3) kills
/// consecutive same-tool calls whose outputs are byte-identical — whatever
/// their arguments — so the old constant-shape `saved {key} (N bytes)`
/// string let a legitimate checkpoint loop (a fixed-width counter updated
/// under one key) be killed as stagnant mid-solve. The `sha256/8` suffix is
/// the content-derived identity that makes the outputs distinct.
#[tokio::test]
async fn save_state_outputs_differ_for_different_same_length_content() {
    let (_root, reg) = bare_registry();
    let mut outputs = Vec::new();
    for content in ["counter=1", "counter=2"] {
        let ToolOutput::Ok { content, .. } = reg
            .execute(
                "save_state",
                &serde_json::json!({"key": "checkpoint", "content": content}),
            )
            .await
        else {
            panic!("save_state must succeed");
        };
        outputs.push(content);
    }
    assert!(
        outputs[0].starts_with("saved checkpoint (9 bytes"),
        "the `saved {{key}} ({{N}} bytes` prefix is stable — the identity is \
         appended, never restructured: {:?}",
        outputs[0]
    );
    assert_ne!(
        outputs[0], outputs[1],
        "different same-length content under one key must render distinct \
         outputs, or the stagnation detector kills a legitimate checkpoint \
         loop as stagnant (#3297)"
    );
}

/// The complement that guards the detector's contract: re-saving *identical*
/// bytes must still render byte-identical output. A loop genuinely stuck
/// re-saving the same state is exactly what the detector exists to catch,
/// and only a content-derived suffix — never a timing, never randomness
/// (#2706) — keeps that catch intact.
#[tokio::test]
async fn save_state_output_is_identical_for_identical_content() {
    let (_root, reg) = bare_registry();
    let save = serde_json::json!({"key": "checkpoint", "content": "counter=1"});
    let ToolOutput::Ok { content: first, .. } = reg.execute("save_state", &save).await else {
        panic!("save_state must succeed");
    };
    let ToolOutput::Ok {
        content: second, ..
    } = reg.execute("save_state", &save).await
    else {
        panic!("save_state must succeed");
    };
    assert_eq!(
        first, second,
        "re-saving identical bytes must render byte-identical output — the \
         stagnation detector's catch of a genuinely stuck loop depends on it"
    );
}

/// `get_environment` dispatches through the registry and reports the scratch
/// directory the registry created — the one fact the CLI's prompt block
/// cannot carry (#3102).
#[tokio::test]
async fn get_environment_reports_the_registrys_scratch_directory() {
    let (_root, reg) = bare_registry();
    let ToolOutput::Ok { content, .. } =
        reg.execute("get_environment", &serde_json::json!({})).await
    else {
        panic!("get_environment must succeed with zero arguments");
    };
    assert!(
        content.contains("Scratch directory: ") && !content.contains("unavailable this session"),
        "the registry-owned scratch dir must be reported by path: {content}"
    );
}

/// A `task` delegation dispatched through the registry lands one `agent_uses`
/// row carrying the child's minted agent id (#3807).
///
/// The witness: before the [`crate::ctx::ToolCtx`] attribution seam the tool
/// had no reach into the ledger at all, so eight delegations drained zero
/// rows and sub-agent health had no audit trail. Written against
/// `execute` + `drain_agent_uses` — the same two calls the session driver
/// makes — so it measures the wiring rather than the new method.
#[tokio::test]
async fn a_delegate_call_records_an_agent_use_row() {
    use stella_core::subagent::{SubAgentOutcome, SubAgentReport};

    struct StubDispatcher;

    #[async_trait::async_trait]
    impl stella_core::subagent::SubAgentDispatcher for StubDispatcher {
        async fn dispatch(&self, _spec: stella_core::subagent::SubAgentSpec) -> SubAgentOutcome {
            SubAgentOutcome::Completed(SubAgentReport {
                summary: "the retry policy is in stella-core/src/retry.rs".into(),
                truncated: false,
                cost_usd: 0.004,
                steps: 3,
                absorbed_messages: 7,
            })
        }
    }

    let (_root, reg) = bare_registry();
    reg.attach_sub_agent_dispatcher(Arc::new(StubDispatcher));

    let out = reg
        .execute(
            "delegate",
            &serde_json::json!({
                "description": "find retry policy",
                "prompt": "Which file defines the retry policy?",
            }),
        )
        .await;
    assert!(!out.is_error(), "the stub child completes: {out:?}");

    let uses = reg.drain_agent_uses();
    assert_eq!(uses.len(), 1, "one delegation, one row: {uses:?}");
    assert_eq!(
        uses[0].agent, "find-retry-policy",
        "the row carries the child's minted agent id"
    );
    assert_eq!(uses[0].version, 1, "a minted child is un-versioned");
    assert_eq!(uses[0].reason, "find retry policy");
    assert!(
        reg.drain_agent_uses().is_empty(),
        "the drain discipline holds — a second drain has nothing"
    );
}
