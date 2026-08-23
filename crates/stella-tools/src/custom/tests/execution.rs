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

/// Dropping the future mid-wait (a cancelled turn) must kill the whole
/// process group — the `crate::exec::GroupKillGuard` backstop, the same
/// leak the `bash` tool had. Without it, a cancelled turn left the
/// script's own children running, `setsid`'d beyond anyone's reach.
#[cfg(unix)]
#[tokio::test]
async fn a_dropped_custom_tool_kills_the_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("pid");
    // Record the *grandchild*'s pid: when the group dies, the orphaned
    // sleep is reaped by init, so a surviving pid means a real leak.
    // `kill_on_drop` alone would not reach it — only the group kill does.
    let tool = script_tool(
        dir.path(),
        "bg.sh",
        &format!(
            "#!/bin/sh\nsleep 30 &\necho $! > {}\nwait\n",
            pidfile.display()
        ),
    );
    let root = dir.path().to_path_buf();
    let handle =
        tokio::spawn(async move { run_custom(&tool, &serde_json::json!({}), &root).await });
    let mut pid = None;
    for _ in 0..250 {
        if let Some(p) = std::fs::read_to_string(&pidfile)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
        {
            pid = Some(p);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let pid = pid.expect("the child never started");
    handle.abort();
    let _ = handle.await;
    let mut dead = false;
    for _ in 0..250 {
        if unsafe { libc::kill(pid, 0) } == -1 {
            dead = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(dead, "cancelled custom tool left subprocess {pid} running");
}

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
