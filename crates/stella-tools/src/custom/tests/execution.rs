// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The execution surface (#3776): spawning a custom tool's script, stdin/env
//! delivery, timeouts, output elision, and process-group cleanup on drop.
//! A child of [`super`], so `super::*` still reaches the private surface —
//! see `custom/tests.rs`'s module doc for the pattern.
//!
//! [`super::script_tool`] and [`super::script_tool_with_timeout`] stay
//! behind in `tests.rs` rather than moving here: `super`'s own
//! `CustomToolSet composition` tests share them too.

use super::*;

#[tokio::test]
async fn exit_zero_captures_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let tool = script_tool(dir.path(), "ok.sh", "#!/bin/sh\necho custom_ran\n");
    let out = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    match out {
        ToolOutput::Ok { content, .. } => assert!(content.contains("custom_ran"), "{content}"),
        ToolOutput::Error { message, .. } => panic!("expected ok: {message}"),
    }
}

/// Witness for #3303: a silent success (exit 0, empty stdout) must NOT render
/// as the empty string on every call — that is byte-identical output whatever
/// the arguments, the exact shape the stagnation detector kills a legitimate
/// work loop over. Distinct inputs must render distinct outputs, while an
/// identical-input repeat must still render identically so genuine stagnation
/// is still caught.
#[tokio::test]
async fn silent_success_is_stamped_with_the_input_identity() {
    let dir = tempfile::tempdir().unwrap();
    let tool = script_tool(dir.path(), "silent.sh", "#!/bin/sh\nexit 0\n");
    let render = |input: Value| {
        let tool = &tool;
        let dir = dir.path();
        async move {
            match run_custom(tool, &input, dir).await {
                ToolOutput::Ok { content, .. } => content,
                ToolOutput::Error { message, .. } => panic!("expected ok: {message}"),
            }
        }
    };
    let a = render(serde_json::json!({ "path": "a.rs" })).await;
    let b = render(serde_json::json!({ "path": "b.rs" })).await;
    let a_again = render(serde_json::json!({ "path": "a.rs" })).await;
    assert_ne!(a, b, "distinct inputs rendered byte-identical output");
    assert_eq!(
        a, a_again,
        "an identical-input repeat must render identically"
    );
    assert!(a.contains("`t` exited 0 with no output"), "{a}");
    assert!(a.contains("input sha256/8 "), "{a}");
}

/// Witness for #749: a script that is transiently open for writing spawns
/// anyway once the writer goes away. Holding a write descriptor anywhere
/// in the system — here, in the test process itself — makes `exec` fail
/// with `ETXTBSY`, which is exactly the race the parallel test threads hit
/// on CI; without the bounded retry this returns the spawn error.
#[cfg(unix)]
#[tokio::test]
async fn spawn_retries_while_script_is_open_for_writing() {
    let dir = tempfile::tempdir().unwrap();
    let tool = script_tool(dir.path(), "busy.sh", "#!/bin/sh\necho recovered\n");
    let writer = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("busy.sh"))
        .unwrap();
    let release = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(writer);
    });
    let out = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    release.await.unwrap();
    match out {
        ToolOutput::Ok { content, .. } => assert!(content.contains("recovered"), "{content}"),
        ToolOutput::Error { message, .. } => panic!("expected ok after retry: {message}"),
    }
}

#[tokio::test]
async fn input_json_is_delivered_on_stdin() {
    let dir = tempfile::tempdir().unwrap();
    // `cat` echoes the JSON document written to stdin.
    let tool = script_tool(dir.path(), "stdin.sh", "#!/bin/sh\ncat\n");
    let out = run_custom(
        &tool,
        &serde_json::json!({ "path": "src/lib.rs" }),
        dir.path(),
    )
    .await;
    match out {
        ToolOutput::Ok { content, .. } => {
            assert!(content.contains("\"path\""), "{content}");
            assert!(content.contains("src/lib.rs"), "{content}");
        }
        ToolOutput::Error { message, .. } => panic!("expected ok: {message}"),
    }
}

#[tokio::test]
async fn scalar_inputs_are_exported_as_env_vars() {
    let dir = tempfile::tempdir().unwrap();
    let tool = script_tool(
        dir.path(),
        "env.sh",
        "#!/bin/sh\necho \"path=$STELLA_INPUT_PATH dry=$STELLA_INPUT_DRY_RUN n=$STELLA_INPUT_COUNT\"\n",
    );
    let input = serde_json::json!({ "path": "hello", "dry_run": true, "count": 7 });
    let out = run_custom(&tool, &input, dir.path()).await;
    match out {
        ToolOutput::Ok { content, .. } => {
            assert!(content.contains("path=hello"), "{content}");
            assert!(content.contains("dry=true"), "{content}");
            assert!(content.contains("n=7"), "{content}");
        }
        ToolOutput::Error { message, .. } => panic!("expected ok: {message}"),
    }
}

#[tokio::test]
async fn nested_input_is_not_exported_as_env_but_still_on_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let tool = script_tool(
        dir.path(),
        "nested.sh",
        "#!/bin/sh\necho \"nested=[$STELLA_INPUT_NESTED]\"\ncat\n",
    );
    let input = serde_json::json!({ "nested": { "a": 1 } });
    let out = run_custom(&tool, &input, dir.path()).await;
    match out {
        ToolOutput::Ok { content, .. } => {
            assert!(
                content.contains("nested=[]"),
                "object must not export env: {content}"
            );
            assert!(content.contains("\"nested\""), "still on stdin: {content}");
        }
        ToolOutput::Error { message, .. } => panic!("expected ok: {message}"),
    }
}

#[tokio::test]
async fn manifest_env_is_applied() {
    let dir = tempfile::tempdir().unwrap();
    let mut tool = script_tool(
        dir.path(),
        "menv.sh",
        "#!/bin/sh\necho \"p=$LINT_PROFILE\"\n",
    );
    tool.env.insert("LINT_PROFILE".into(), "strict".into());
    let out = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    match out {
        ToolOutput::Ok { content, .. } => assert!(content.contains("p=strict"), "{content}"),
        ToolOutput::Error { message, .. } => panic!("expected ok: {message}"),
    }
}

#[tokio::test]
async fn manifest_cannot_reintroduce_credentials_but_benign_env_survives() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        "#!/bin/sh\n{}\n",
        crate::subprocess_env::test_support::PROBE_COMMAND
    );
    let mut tool = script_tool(dir.path(), "secret-env.sh", &body);
    tool.env.extend([
        ("OPENROUTER_API_KEY".into(), "manifest-openrouter".into()),
        ("GITHUB_TOKEN".into(), "manifest-github".into()),
        ("AWS_SECRET_ACCESS_KEY".into(), "manifest-aws".into()),
        ("STELLA_TEST_BENIGN".into(), "visible".into()),
    ]);
    let out = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    match out {
        ToolOutput::Ok { content, .. } => {
            crate::subprocess_env::test_support::assert_scrubbed(&content)
        }
        ToolOutput::Error { message, .. } => panic!("expected ok: {message}"),
    }
}

#[tokio::test]
async fn a_custom_tool_is_spawned_without_git_or_forced_color_env() {
    // #820: a custom tool spawns a model-invoked command, so — like the
    // hook runner — it must strip the surrounding git-repo and forced-color
    // env, not only credentials.
    // Fails on the old `scrub_sensitive_env` call (which left both set).
    let _hygiene = crate::subprocess_env::test_support::SpawnHygieneFixture::install();
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        "#!/bin/sh\n{}\n",
        crate::subprocess_env::test_support::SPAWN_HYGIENE_PROBE_COMMAND
    );
    let tool = script_tool(dir.path(), "spawn-hygiene.sh", &body);
    let out = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    match out {
        ToolOutput::Ok { content, .. } => {
            crate::subprocess_env::test_support::assert_spawn_hygiene_scrubbed(&content)
        }
        ToolOutput::Error { message, .. } => panic!("expected ok: {message}"),
    }
}

#[tokio::test]
async fn nonzero_exit_becomes_error_with_code_and_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let tool = script_tool(dir.path(), "fail.sh", "#!/bin/sh\necho boom >&2\nexit 3\n");
    let out = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    match out {
        ToolOutput::Ok { content, .. } => panic!("expected error: {content}"),
        ToolOutput::Error { message, .. } => {
            assert!(message.contains("code 3"), "{message}");
            assert!(message.contains("boom"), "{message}");
        }
    }
}

#[tokio::test]
async fn timeout_kills_and_returns_fast() {
    let dir = tempfile::tempdir().unwrap();
    let tool = script_tool_with_timeout(dir.path(), "slow.sh", "#!/bin/sh\nsleep 600\n", 200);
    let start = std::time::Instant::now();
    let out = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    let elapsed = start.elapsed();
    assert!(out.is_error());
    if let ToolOutput::Error { message, .. } = out {
        assert!(message.contains("timed out"), "{message}");
    }
    // The margin is deliberately enormous relative to the 200ms budget: the
    // claim is "the timer fired instead of the script finishing", and the
    // script sleeps for 600s, so anything under a minute falsifies the
    // alternative just as well while leaving no room for load to decide the
    // outcome (#2011).
    assert!(
        elapsed.as_secs() < 60,
        "should not wait for the full sleep: {elapsed:?}"
    );
}

/// Witness for #2011: an execution test's script must not be racing a
/// few-second budget. This script deliberately outlives the old hardcoded
/// 5000ms, so under the previous helper it returned
/// ``custom tool `t` timed out after 5000ms``; with the budget at
/// [`super::NO_TIMEOUT_MS`] it simply runs to completion.
#[tokio::test]
async fn a_script_outliving_the_old_fixed_budget_still_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let tool = script_tool(
        dir.path(),
        "slowish.sh",
        "#!/bin/sh\nsleep 6\necho survived\n",
    );
    match run_custom(&tool, &serde_json::json!({}), dir.path()).await {
        ToolOutput::Ok { content, .. } => assert!(content.contains("survived"), "{content}"),
        ToolOutput::Error { message, .. } => panic!("expected the script to finish: {message}"),
    }
}

// `GroupKillGuard`'s own witness for this spawn site lives in
// `stella-tools/tests/group_kill_witnesses.rs`, not here: it needs to run on
// Windows too, and this file's `#[cfg(test)] mod` does not (#4698).

#[tokio::test]
async fn missing_script_names_the_path_tried() {
    let dir = tempfile::tempdir().unwrap();
    let tool = CustomTool {
        name: "t".into(),
        description: "d".into(),
        command: vec!["./does-not-exist.sh".into()],
        timeout_ms: NO_TIMEOUT_MS,
        input_schema: serde_json::json!({ "type": "object" }),
        env: HashMap::new(),
        source: dir.path().join("t.toml"),
        foundry: None,
        claimed_read_only: false,
        claimed_risk: None,
        claimed_idempotent: false,
        output_schema: None,
        foundry_runtime: Default::default(),
        contributed_by: None,
    };
    let out = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    match out {
        ToolOutput::Ok { content, .. } => panic!("expected error: {content}"),
        ToolOutput::Error { message, .. } => {
            assert!(message.contains("./does-not-exist.sh"), "{message}");
            assert!(message.contains("failed to spawn"), "{message}");
        }
    }
}

#[tokio::test]
async fn oversized_output_is_elided_middle_out() {
    let dir = tempfile::tempdir().unwrap();
    // Emit ~200k bytes of 'X' (well past MAX_OUTPUT_BYTES).
    let tool = script_tool(
        dir.path(),
        "big.sh",
        "#!/bin/sh\nhead -c 200000 /dev/zero | tr '\\0' 'X'\n",
    );
    let out = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    match out {
        ToolOutput::Ok { content, .. } => {
            assert!(content.contains("truncated"), "elision marker present");
            assert!(
                content.len() <= MAX_OUTPUT_BYTES + 200,
                "capped: {}",
                content.len()
            );
        }
        ToolOutput::Error { message, .. } => panic!("expected ok: {message}"),
    }
}

/// Witness for #1889: over-cap stdout keeps BOTH its first and last lines
/// around a marker naming the cap. Sized from the shared constant so the
/// old 100 KB cap (which left this size untouched) and any head-only cut
/// both fail it.
#[tokio::test]
async fn over_cap_output_keeps_first_and_last_lines_with_a_named_elision() {
    let dir = tempfile::tempdir().unwrap();
    let tool = script_tool(
        dir.path(),
        "big-ends.sh",
        &format!(
            "#!/bin/sh\necho FIRST_SENTINEL_LINE\n\
             head -c {MAX_OUTPUT_BYTES} /dev/zero | tr '\\0' 'x'\necho\n\
             echo LAST_SENTINEL_LINE\n"
        ),
    );
    let out = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    let content = match out {
        ToolOutput::Ok { content, .. } => content,
        ToolOutput::Error { message, .. } => panic!("expected ok: {message}"),
    };
    assert!(
        content.contains("FIRST_SENTINEL_LINE"),
        "the head survives elision"
    );
    assert!(
        content.contains("LAST_SENTINEL_LINE"),
        "the tail survives elision"
    );
    assert!(
        content.contains(&format!("the {MAX_OUTPUT_BYTES}-byte cap")),
        "the marker names the cap it enforced"
    );
    assert!(
        content.contains("bytes truncated"),
        "the marker names the elided byte count"
    );
}

#[tokio::test]
async fn non_object_input_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let tool = script_tool(dir.path(), "cat.sh", "#!/bin/sh\ncat\n");
    // A bare array — no top-level object, so no env vars, but still on stdin.
    let out = run_custom(&tool, &serde_json::json!(["a", "b"]), dir.path()).await;
    match out {
        ToolOutput::Ok { content, .. } => assert!(content.contains("\"a\""), "{content}"),
        ToolOutput::Error { message, .. } => panic!("expected ok: {message}"),
    }
}

// ── The autonomous-foundry launch controls ─────────────────────────────────

/// A foundry-governed tool for the launch-control tests: real provenance, a
/// real adoption row in the workspace's store, telemetry and breaker live.
/// `approved: None` on purpose — these tests exercise the launch controls,
/// not the tamper recheck, which has its own witnesses in `foundry_gate`.
fn governed_tool(root: &Path, file: &str, body: &str) -> CustomTool {
    let mut tool = script_tool(root, file, body);
    tool.name = "governed_t".into();
    tool.foundry = Some(crate::foundry_gate::FoundryProvenance {
        authored_by: crate::foundry_gate::AUTHORED_BY.into(),
        signature: "x <path>".into(),
        occurrences: 3,
        witness_input: serde_json::json!({ "p1": "a" }),
        gap_id: "gap-1".into(),
        approved: None,
    });
    tool
}

fn adopt_enabled(root: &Path, name: &str) -> stella_store::Store {
    let store = stella_store::Store::open(root).expect("store");
    store
        .adopt_foundry_tool(&stella_store::AdoptedTool {
            name: name.into(),
            signature: "x <path>".into(),
            manifest_digest: "m".into(),
            script_digest: "s".into(),
            witness: "proven".into(),
            witness_input: "{}".into(),
            witness_expect: "y".into(),
            enabled: false,
            adopted_at: String::new(),
            disabled_reason: String::new(),
            enabled_authority: None,
        })
        .expect("adopt");
    store
        .set_foundry_tool_enabled(name, Some(stella_store::EnableAuthority::FlagAssertion))
        .expect("enable");
    store
}

/// Witness (breaker): the configured consecutive-failure threshold
/// trips the breaker, the trip is recorded in the ledger with its reason, and
/// the NEXT launch is refused before any process spawns — all through the
/// live `run_custom` path.
#[tokio::test]
async fn the_breaker_trips_after_configured_failures_and_blocks_the_next_launch() {
    let dir = tempfile::tempdir().unwrap();
    let mut tool = governed_tool(dir.path(), "fail.sh", "#!/bin/sh\necho no >&2\nexit 3\n");
    tool.foundry_runtime.breaker = Some(BreakerPolicy {
        consecutive_failures: 2,
        window: 10,
        failure_rate: 0.5,
    });
    let store = adopt_enabled(dir.path(), "governed_t");

    // First failure: recorded, breaker not yet tripped.
    let first = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    let ToolOutput::Error { message, .. } = &first else {
        panic!("the script exits 3");
    };
    assert!(
        !message.contains("circuit breaker"),
        "one failure is below the threshold: {message}"
    );

    // Second consecutive failure: the breaker trips and says so.
    let second = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    let ToolOutput::Error { message, .. } = &second else {
        panic!("the script exits 3");
    };
    assert!(
        message.contains("circuit breaker"),
        "the trip must be user-visible: {message}"
    );
    let row = store
        .adopted_foundry_tool("governed_t")
        .expect("read")
        .expect("row");
    assert!(!row.enabled, "the trip disables the tool");
    assert!(
        row.disabled_reason.contains("2 consecutive failures"),
        "the reason is recorded: {}",
        row.disabled_reason
    );

    // Third launch: refused at the seam, script never runs.
    let third = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    let ToolOutput::Error { message, .. } = &third else {
        panic!("a disabled tool must not run");
    };
    assert!(
        message.contains("did not run") && message.contains("circuit breaker"),
        "the refusal names the recorded reason: {message}"
    );

    // And the telemetry ledger holds exactly the two real launches.
    let outcomes = store
        .recent_foundry_outcomes("governed_t", 10)
        .expect("outcomes");
    assert_eq!(outcomes, vec![false, false]);
}

/// Telemetry: every governed launch writes one row carrying the
/// gap-id lineage and the outcome — success included.
#[tokio::test]
async fn a_governed_launch_writes_one_telemetry_row() {
    let dir = tempfile::tempdir().unwrap();
    let tool = governed_tool(dir.path(), "ok.sh", "#!/bin/sh\necho fine\n");
    let store = adopt_enabled(dir.path(), "governed_t");

    let out = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    assert!(matches!(out, ToolOutput::Ok { .. }));
    let outcomes = store
        .recent_foundry_outcomes("governed_t", 10)
        .expect("outcomes");
    assert_eq!(outcomes, vec![true]);
}

/// A hand-written tool is untouched by all of it: no store is opened, no
/// telemetry lands, no breaker applies — the controls govern exactly what
/// Stella wrote for itself.
#[tokio::test]
async fn a_hand_written_tool_records_no_foundry_telemetry() {
    let dir = tempfile::tempdir().unwrap();
    let tool = script_tool(dir.path(), "fail.sh", "#!/bin/sh\nexit 1\n");
    for _ in 0..4 {
        let _ = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    }
    let store = stella_store::Store::open(dir.path()).expect("store");
    assert!(
        store
            .recent_foundry_outcomes("t", 10)
            .expect("outcomes")
            .is_empty(),
        "a hand-written tool must not accrue foundry telemetry"
    );
}

/// Network denial: where the platform mechanism is live, a governed
/// tool that attempts a TCP connect is denied at the OS level, while the
/// same script on the operator's allowlist gets through the wrapper choice
/// unwrapped. Skipped (trivially green) where no mechanism exists — the
/// autonomy pipeline separately refuses to auto-adopt there, which is the
/// degraded control this test cannot fake.
#[tokio::test]
async fn a_governed_tool_cannot_reach_the_network_where_the_mechanism_is_live() {
    if !crate::netdeny::available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // Tries a fast TCP connect; prints REACHED only if the connect succeeds.
    let body =
        "#!/bin/bash\nif exec 3<>/dev/tcp/1.1.1.1/53; then echo REACHED; else echo DENIED; fi\n";
    let tool = governed_tool(dir.path(), "net.sh", body);
    let _store = adopt_enabled(dir.path(), "governed_t");

    let out = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    let rendered = match &out {
        ToolOutput::Ok { content, .. } => content.clone(),
        ToolOutput::Error { message, .. } => message.clone(),
    };
    assert!(
        !rendered.contains("REACHED"),
        "a foundry tool off the allowlist must not reach the network: {rendered}"
    );
}
