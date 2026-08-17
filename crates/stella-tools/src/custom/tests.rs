// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Unit tests for [`super`]: manifest parsing, the execution surface, and
//! the decorator's forwarding contract.
//!
//! Split out of `custom.rs` (like `process.rs` / `process/tests.rs` and
//! `registry.rs` / `registry/tests.rs`) so the module that ships custom
//! tools is not dominated by the module that checks them, and so the file
//! stays under the 1500-line gate. A child module, so `super::*` still
//! reaches the private surface.
use std::os::unix::fs::PermissionsExt;

use super::*;

// manifest parsing

const HAPPY: &str = r#"
name = "lint_fix"
description = "Run the lint auto-fixer on a path"
command = ["./scripts/lint-fix.sh", "--quiet"]
timeout_ms = 60000

[env]
LINT_PROFILE = "strict"

[input_schema]
type = "object"
[input_schema.properties.path]
type = "string"
description = "Directory or file to fix"
[input_schema.properties.dry_run]
type = "boolean"
"#;

#[test]
fn parses_a_complete_manifest() {
    let tool = parse_manifest(HAPPY, Path::new("/x/lint_fix.toml")).expect("valid manifest");
    assert_eq!(tool.name, "lint_fix");
    assert_eq!(tool.command, vec!["./scripts/lint-fix.sh", "--quiet"]);
    assert_eq!(tool.timeout_ms, 60000);
    assert_eq!(
        tool.env.get("LINT_PROFILE").map(String::as_str),
        Some("strict")
    );
    assert_eq!(tool.source, Path::new("/x/lint_fix.toml"));
}

#[test]
fn input_schema_toml_converts_to_json_faithfully_including_nested() {
    let tool = parse_manifest(HAPPY, Path::new("x.toml")).unwrap();
    let schema = &tool.input_schema;
    assert_eq!(schema["type"], "object");
    // Nested table → nested JSON object, preserved verbatim.
    assert_eq!(schema["properties"]["path"]["type"], "string");
    assert_eq!(
        schema["properties"]["path"]["description"],
        "Directory or file to fix"
    );
    assert_eq!(schema["properties"]["dry_run"]["type"], "boolean");
}

#[test]
fn numeric_and_bool_schema_values_round_trip() {
    let src = r#"
name = "tt"
description = "d"
command = ["./x.sh"]
[input_schema]
type = "object"
additionalProperties = false
[input_schema.properties.count]
type = "integer"
minimum = 1
maximum = 10
"#;
    let tool = parse_manifest(src, Path::new("t.toml")).unwrap();
    assert_eq!(tool.input_schema["additionalProperties"], false);
    assert_eq!(tool.input_schema["properties"]["count"]["minimum"], 1);
    assert_eq!(tool.input_schema["properties"]["count"]["maximum"], 10);
}

#[test]
fn missing_description_is_rejected() {
    let src = r#"name = "t"
command = ["./x.sh"]"#;
    let err = parse_manifest(src, Path::new("t.toml")).unwrap_err();
    // serde reports the missing field; message mentions `description`.
    assert!(err.contains("description"), "got: {err}");
}

#[test]
fn empty_description_is_rejected() {
    let src = r#"name = "tt"
description = "   "
command = ["./x.sh"]"#;
    let err = parse_manifest(src, Path::new("t.toml")).unwrap_err();
    assert!(err.contains("description"), "got: {err}");
}

#[test]
fn empty_command_is_rejected() {
    let src = r#"name = "tt"
description = "d"
command = []"#;
    let err = parse_manifest(src, Path::new("t.toml")).unwrap_err();
    assert!(err.contains("command"), "got: {err}");
}

#[test]
fn bad_names_are_rejected() {
    for bad in [
        "Lint",
        "1tool",
        "a",
        "has-dash",
        "has space",
        "way_too_long_name_that_keeps_going_way_past_the_sixty_four_character_limit_x",
    ] {
        let src = format!("name = \"{bad}\"\ndescription = \"d\"\ncommand = [\"./x.sh\"]");
        assert!(
            parse_manifest(&src, Path::new("t.toml")).is_err(),
            "name `{bad}` should be rejected"
        );
    }
}

#[test]
fn good_names_are_accepted() {
    for good in ["ab", "lint_fix", "run_tests_2", "a1"] {
        let src = format!("name = \"{good}\"\ndescription = \"d\"\ncommand = [\"./x.sh\"]");
        assert!(
            parse_manifest(&src, Path::new("t.toml")).is_ok(),
            "name `{good}` should be accepted"
        );
    }
}

#[test]
fn reserved_names_are_rejected() {
    for reserved in RESERVED_NAMES {
        let src = format!("name = \"{reserved}\"\ndescription = \"d\"\ncommand = [\"./x.sh\"]");
        let err = parse_manifest(&src, Path::new("t.toml")).unwrap_err();
        assert!(err.contains("reserved"), "reserved `{reserved}` -> {err}");
    }
}

#[test]
fn timeout_defaults_when_omitted_and_clamps_over_cap() {
    let base = "name = \"tt\"\ndescription = \"d\"\ncommand = [\"./x.sh\"]";
    let default = parse_manifest(base, Path::new("t.toml")).unwrap();
    assert_eq!(default.timeout_ms, DEFAULT_TIMEOUT_MS);

    let over = format!("{base}\ntimeout_ms = 99999999");
    let clamped = parse_manifest(&over, Path::new("t.toml")).unwrap();
    assert_eq!(clamped.timeout_ms, MAX_TIMEOUT_MS);

    let zero = format!("{base}\ntimeout_ms = 0");
    let zeroed = parse_manifest(&zero, Path::new("t.toml")).unwrap();
    assert_eq!(zeroed.timeout_ms, DEFAULT_TIMEOUT_MS);
}

#[test]
fn absent_input_schema_defaults_to_object() {
    let src = "name = \"tt\"\ndescription = \"d\"\ncommand = [\"./x.sh\"]";
    let tool = parse_manifest(src, Path::new("t.toml")).unwrap();
    assert_eq!(tool.input_schema["type"], "object");
}

// discovery

fn write_manifest(dir: &Path, file: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(file), body).unwrap();
}

fn ws_tools(root: &Path) -> PathBuf {
    root.join(".stella").join("tools")
}

/// The user-global tools dir under a fixture home. The API takes the
/// stella ROOT (`<home>/.stella`), so the fixture layout on disk is
/// unchanged and only what the caller passes moved (#2178).
fn global_tools(home: &Path) -> PathBuf {
    user_root(home).join("tools")
}

/// The stella root under a fixture home — what `discover_in*` now takes.
fn user_root(home: &Path) -> PathBuf {
    home.join(".stella")
}

#[test]
fn discovers_workspace_and_global_tools() {
    let ws = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write_manifest(
        &ws_tools(ws.path()),
        "a.toml",
        "name = \"a_tool\"\ndescription = \"d\"\ncommand = [\"./a.sh\"]",
    );
    write_manifest(
        &global_tools(home.path()),
        "b.toml",
        "name = \"b_tool\"\ndescription = \"d\"\ncommand = [\"./b.sh\"]",
    );

    let report = discover_in(ws.path(), Some(&user_root(home.path())));
    let names: Vec<&str> = report.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"a_tool"), "names: {names:?}");
    assert!(names.contains(&"b_tool"), "names: {names:?}");
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
}

#[test]
fn workspace_tools_are_excluded_when_project_scope_is_not_allowed() {
    let ws = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write_manifest(
        &ws_tools(ws.path()),
        "workspace.toml",
        "name = \"workspace_tool\"\ndescription = \"d\"\ncommand = [\"./workspace.sh\"]",
    );
    write_manifest(
        &global_tools(home.path()),
        "global.toml",
        "name = \"global_tool\"\ndescription = \"d\"\ncommand = [\"./global.sh\"]",
    );

    let report = discover_in_scopes(ws.path(), Some(&user_root(home.path())), false);
    let names: Vec<&str> = report.tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, vec!["global_tool"]);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
}

#[test]
fn absent_home_skips_global_scan_without_error() {
    let ws = tempfile::tempdir().unwrap();
    write_manifest(
        &ws_tools(ws.path()),
        "a.toml",
        "name = \"a_tool\"\ndescription = \"d\"\ncommand = [\"./a.sh\"]",
    );
    let report = discover_in(ws.path(), None);
    assert_eq!(report.tools.len(), 1);
    assert!(report.diagnostics.is_empty());
}

#[test]
fn malformed_manifest_becomes_a_diagnostic_and_does_not_kill_discovery() {
    let ws = tempfile::tempdir().unwrap();
    let dir = ws_tools(ws.path());
    write_manifest(
        &dir,
        "good.toml",
        "name = \"good_tool\"\ndescription = \"d\"\ncommand = [\"./g.sh\"]",
    );
    write_manifest(&dir, "bad.toml", "this is not = valid toml [[[");

    let report = discover_in(ws.path(), None);
    assert_eq!(report.tools.len(), 1);
    assert_eq!(report.tools[0].name, "good_tool");
    assert_eq!(report.diagnostics.len(), 1);
    assert!(report.diagnostics[0].path.ends_with("bad.toml"));
}

#[test]
fn reserved_name_collision_is_a_diagnostic_and_skipped() {
    let ws = tempfile::tempdir().unwrap();
    write_manifest(
        &ws_tools(ws.path()),
        "task.toml",
        "name = \"task\"\ndescription = \"d\"\ncommand = [\"./b.sh\"]",
    );
    let report = discover_in(ws.path(), None);
    assert!(report.tools.is_empty(), "reserved tool must be skipped");
    assert_eq!(report.diagnostics.len(), 1);
    assert!(report.diagnostics[0].reason.contains("reserved"));
}

#[test]
fn workspace_wins_over_global_on_name_collision() {
    let ws = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write_manifest(
        &ws_tools(ws.path()),
        "dup.toml",
        "name = \"dup\"\ndescription = \"WORKSPACE\"\ncommand = [\"./w.sh\"]",
    );
    write_manifest(
        &global_tools(home.path()),
        "dup.toml",
        "name = \"dup\"\ndescription = \"GLOBAL\"\ncommand = [\"./g.sh\"]",
    );

    let report = discover_in(ws.path(), Some(&user_root(home.path())));
    assert_eq!(report.tools.len(), 1);
    assert_eq!(report.tools[0].description, "WORKSPACE");
    assert_eq!(report.diagnostics.len(), 1, "global dup must be flagged");
    assert!(report.diagnostics[0].path.starts_with(home.path()));
}

// execution

/// Write an executable `#!/bin/sh` script into `root` and return a
/// [`CustomTool`] whose relative `command[0]` resolves against `root`.
fn script_tool(root: &Path, file: &str, body: &str, timeout_ms: u64) -> CustomTool {
    let path = root.join(file);
    std::fs::write(&path, body).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    CustomTool {
        name: "t".into(),
        description: "d".into(),
        command: vec![format!("./{file}")],
        timeout_ms,
        input_schema: serde_json::json!({ "type": "object" }),
        env: HashMap::new(),
        source: path,
        foundry: None,
        claimed_read_only: false,
        claimed_risk: None,
        claimed_idempotent: false,
        output_schema: None,
        contributed_by: None,
    }
}

#[tokio::test]
async fn exit_zero_captures_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let tool = script_tool(dir.path(), "ok.sh", "#!/bin/sh\necho custom_ran\n", 5000);
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
    let tool = script_tool(dir.path(), "silent.sh", "#!/bin/sh\nexit 0\n", 5000);
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
    let tool = script_tool(dir.path(), "busy.sh", "#!/bin/sh\necho recovered\n", 5000);
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
    let tool = script_tool(dir.path(), "stdin.sh", "#!/bin/sh\ncat\n", 5000);
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
        5000,
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
        5000,
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
        5000,
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
    let mut tool = script_tool(dir.path(), "secret-env.sh", &body, 5000);
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
    let tool = script_tool(dir.path(), "spawn-hygiene.sh", &body, 5000);
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
    let tool = script_tool(
        dir.path(),
        "fail.sh",
        "#!/bin/sh\necho boom >&2\nexit 3\n",
        5000,
    );
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
    let tool = script_tool(dir.path(), "slow.sh", "#!/bin/sh\nsleep 30\n", 200);
    let start = std::time::Instant::now();
    let out = run_custom(&tool, &serde_json::json!({}), dir.path()).await;
    let elapsed = start.elapsed();
    assert!(out.is_error());
    if let ToolOutput::Error { message, .. } = out {
        assert!(message.contains("timed out"), "{message}");
    }
    assert!(
        elapsed.as_secs() < 5,
        "should not wait for the full sleep: {elapsed:?}"
    );
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
        60_000,
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
        timeout_ms: 5000,
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
        5000,
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
        5000,
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
    let tool = script_tool(dir.path(), "cat.sh", "#!/bin/sh\ncat\n", 5000);
    // A bare array — no top-level object, so no env vars, but still on stdin.
    let out = run_custom(&tool, &serde_json::json!(["a", "b"]), dir.path()).await;
    match out {
        ToolOutput::Ok { content, .. } => assert!(content.contains("\"a\""), "{content}"),
        ToolOutput::Error { message, .. } => panic!("expected ok: {message}"),
    }
}

// CustomToolSet composition

struct FakeInner;
#[async_trait]
impl ToolExecutor for FakeInner {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "bash".into(),
            description: "run".into(),
            input_schema: serde_json::json!({ "type": "object" }),
            read_only: false,
            speculation_safe: false,
        }]
    }
    async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: format!("inner ran {name}"),
            data: None,
        }
    }
}

#[tokio::test]
async fn set_advertises_inner_plus_custom_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let tool = script_tool(dir.path(), "s.sh", "#!/bin/sh\necho hi\n", 5000);
    let inner = FakeInner;
    let set = CustomToolSet::new(&inner, vec![tool], dir.path().to_path_buf());
    let names: Vec<String> = set.schemas().into_iter().map(|s| s.name).collect();
    assert!(names.contains(&"bash".to_string()));
    assert!(names.contains(&"t".to_string()));
}

#[tokio::test]
async fn set_routes_custom_names_and_falls_through_for_others() {
    let dir = tempfile::tempdir().unwrap();
    let mut tool = script_tool(dir.path(), "s.sh", "#!/bin/sh\necho from_custom\n", 5000);
    tool.name = "my_tool".into();
    tool.command = vec!["./s.sh".into()];
    let inner = FakeInner;
    let set = CustomToolSet::new(&inner, vec![tool], dir.path().to_path_buf());

    let custom = set.execute("my_tool", &serde_json::json!({})).await;
    match custom {
        ToolOutput::Ok { content, .. } => assert!(content.contains("from_custom"), "{content}"),
        ToolOutput::Error { message, .. } => panic!("expected ok: {message}"),
    }

    // Unknown-to-custom name falls through to the inner executor.
    let fell = set.execute("bash", &serde_json::json!({})).await;
    match fell {
        ToolOutput::Ok { content, .. } => assert_eq!(content, "inner ran bash"),
        ToolOutput::Error { message, .. } => panic!("expected fallthrough: {message}"),
    }
}

#[tokio::test]
async fn owned_inner_delegates_schemas_and_fallthrough() {
    // The Arc-owned variant (best-of-N candidates) must behave exactly
    // like the borrowed one: inner schemas + customs, and unknown names
    // fall through to the owned inner.
    let dir = tempfile::tempdir().unwrap();
    let mut tool = script_tool(dir.path(), "s.sh", "#!/bin/sh\necho from_custom\n", 5000);
    tool.name = "my_tool".into();
    tool.command = vec!["./s.sh".into()];
    let inner: std::sync::Arc<dyn ToolExecutor> = std::sync::Arc::new(FakeInner);
    let set = CustomToolSet::new_owned(inner, vec![tool], dir.path().to_path_buf());

    let names: Vec<String> = set.schemas().into_iter().map(|s| s.name).collect();
    assert!(names.contains(&"bash".to_string()), "owned inner's schema");
    assert!(names.contains(&"my_tool".to_string()), "custom schema");

    match set.execute("bash", &serde_json::json!({})).await {
        ToolOutput::Ok { content, .. } => assert_eq!(content, "inner ran bash"),
        ToolOutput::Error { message, .. } => panic!("expected owned-inner fallthrough: {message}"),
    }
}

#[test]
fn env_var_name_uppercases_and_sanitizes() {
    assert_eq!(env_var_name("path"), "STELLA_INPUT_PATH");
    assert_eq!(env_var_name("dry_run"), "STELLA_INPUT_DRY_RUN");
    assert_eq!(env_var_name("weird-key.x"), "STELLA_INPUT_WEIRD_KEY_X");
}

// #3287 — manifest claims are recorded, displayed, and buy nothing.

#[test]
fn manifest_claims_parse_into_the_declared_contract_and_buy_nothing() {
    let manifest = r#"
name = "peek"
description = "look at things"
command = ["./peek.sh"]
read_only = true
risk = "low"
idempotent = true
"#;
    let tool = parse_manifest(manifest, Path::new("peek.toml")).unwrap();
    assert!(tool.claimed_read_only);
    assert_eq!(tool.claimed_risk, Some(stella_protocol::RiskLevel::Low));
    assert!(tool.claimed_idempotent);

    // The advertised schema never carries the claim: dispatch trusts the
    // bit directly, and a self-report must not buy concurrency.
    assert!(!tool.schema().read_only);

    // The contract carries the claim verbatim, at untrusted provenance —
    // visible to policy, vouched for by nobody.
    let contract = tool.contract();
    assert_eq!(contract.provenance, stella_protocol::Provenance::Declared);
    assert!(contract.schema.read_only, "the claim is preserved");
    assert!(!contract.trusted_read_only(), "and buys no trust");
    assert!(contract.idempotent);
    assert_eq!(
        contract.risk,
        stella_protocol::RiskLevel::High,
        "a claimed `low` never lowers the declared grade"
    );
    assert_eq!(
        tool.claims_label(),
        "[claims read-only, risk: low, idempotent] "
    );
}

#[test]
fn a_claimed_destructive_risk_raises_the_grade_and_a_claimless_manifest_is_unchanged() {
    let destructive = r#"
name = "wipe"
description = "deletes things"
command = ["./wipe.sh"]
risk = "destructive"
"#;
    let tool = parse_manifest(destructive, Path::new("wipe.toml")).unwrap();
    assert_eq!(
        tool.contract().risk,
        stella_protocol::RiskLevel::Destructive,
        "a self-report may make a tool look more dangerous"
    );

    let plain = r#"
name = "plain"
description = "no claims"
command = ["./plain.sh"]
"#;
    let tool = parse_manifest(plain, Path::new("plain.toml")).unwrap();
    assert_eq!(tool.contract().risk, stella_protocol::RiskLevel::High);
    assert_eq!(tool.claims_label(), "");
}

/// The #3287 witness (manifest half): a `read_only = true` claim is carried
/// on the contract, refused by a `Medium` risk ceiling, and absent from the
/// read-only dispatch set — while a genuinely read-only *built-in* passes
/// all three the other way (that half lives in `contracts.rs`'s
/// `a_builtin_resolves_to_its_reviewed_row`).
#[tokio::test]
async fn a_claimed_read_only_custom_tool_is_ceiling_refused_and_outside_the_read_only_set() {
    let dir = tempfile::tempdir().unwrap();
    let mut tool = script_tool(dir.path(), "peek.sh", "#!/bin/sh\necho ok\n", 5000);
    tool.name = "peek".into();
    tool.claimed_read_only = true;
    let set = CustomToolSet::new(&FakeInner, vec![tool], dir.path().to_path_buf());

    // Not in the read-only dispatch set: the claim never touches the
    // advertised schema, so `ReadOnlyTools` (which trusts the bit) excludes
    // the tool entirely.
    let read_only = stella_core::ports::ReadOnlyTools::new(&set);
    assert!(
        !stella_core::ports::ToolExecutor::schemas(&read_only)
            .iter()
            .any(|s| s.name == "peek"),
        "an unreviewed claim must not admit a tool to the read-only set"
    );

    // Refused by a Medium risk ceiling: the contract snapshot the gate takes
    // carries the declared High grade.
    let gated = crate::gated::GatedToolSet::new(
        &set,
        std::sync::Arc::new(stella_core::ports::RiskCeiling::new(
            stella_protocol::RiskLevel::Medium,
        )),
        stella_core::ports::Principal::User,
    );
    let out = gated.execute("peek", &serde_json::json!({})).await;
    assert!(out.is_error(), "a Medium ceiling must refuse it: {out:?}");

    // And the claim IS visible where policy looks: the composed contracts.
    let contract = stella_core::ports::ToolExecutor::contracts(&set)
        .into_iter()
        .find(|c| c.name() == "peek")
        .expect("advertised");
    assert!(contract.schema.read_only && !contract.trusted_read_only());
}

/// The `[output_schema]` promise (#3287): JSON stdout matching the schema
/// flows into the structured half; non-JSON stdout is the tool's own defect.
#[tokio::test]
async fn a_declared_output_schema_holds_a_script_to_its_promise() {
    let dir = tempfile::tempdir().unwrap();
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "count": { "type": "integer" } },
        "required": ["count"]
    });

    let mut honest = script_tool(
        dir.path(),
        "honest.sh",
        "#!/bin/sh\necho '{\"count\": 3}'\n",
        5000,
    );
    honest.output_schema = Some(schema.clone());
    match run_custom(&honest, &serde_json::json!({}), dir.path()).await {
        ToolOutput::Ok { data, .. } => {
            assert_eq!(data, Some(serde_json::json!({ "count": 3 })));
        }
        other => panic!("a kept promise must pass: {other:?}"),
    }

    let mut liar = script_tool(dir.path(), "liar.sh", "#!/bin/sh\necho 'not json'\n", 5000);
    liar.output_schema = Some(schema.clone());
    match run_custom(&liar, &serde_json::json!({}), dir.path()).await {
        ToolOutput::Error { message, class } => {
            assert_eq!(class, Some(stella_protocol::ErrorClass::Internal));
            assert!(message.contains("output contract"), "{message}");
        }
        other => panic!("a broken promise is a tool defect: {other:?}"),
    }

    let mut wrong = script_tool(
        dir.path(),
        "wrong.sh",
        "#!/bin/sh\necho '{\"count\": \"three\"}'\n",
        5000,
    );
    wrong.output_schema = Some(schema);
    match run_custom(&wrong, &serde_json::json!({}), dir.path()).await {
        ToolOutput::Error { message, class } => {
            assert_eq!(class, Some(stella_protocol::ErrorClass::Internal));
            assert!(
                message.contains("field `count`"),
                "names the field: {message}"
            );
        }
        other => panic!("a schema breach is a tool defect: {other:?}"),
    }
}

// Plugin-contributed tools (#3380)

/// Write a `<name>.toml` script-tool manifest into `dir`.
fn manifest_at(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).expect("tools dir");
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!("name = \"{name}\"\ndescription = \"d\"\ncommand = [\"./{name}.sh\"]\n"),
    )
    .expect("manifest");
}

/// **Witness: a plugin's tool reaches the surface, and it is attributed.**
///
/// Before #3380 a plugin could ship a `tools/` directory and nothing read
/// it: the tool simply did not exist. The provenance is the half that has to
/// hold as hard as the presence — an unattributed third-party tool cannot be
/// authorized as its plugin and cannot be traced back to it.
#[test]
fn a_plugin_contributes_a_tool_and_it_carries_the_plugin_name() {
    let root = tempfile::tempdir().expect("a temp dir");
    let plugin_dir = root.path().join("plugins").join("vera").join("tools");
    manifest_at(&plugin_dir, "vera_review");

    let none = discover_in_scopes(root.path(), None, true);
    assert!(
        none.names().is_empty(),
        "anti-vacuity: without the plugin tier the tool is nowhere"
    );

    let found = discover_with_plugins(
        root.path(),
        None,
        true,
        &[PluginToolDir {
            plugin: "vera".into(),
            dir: plugin_dir,
        }],
    );
    assert_eq!(found.names(), vec!["vera_review"]);
    let (tools, _) = found.into_parts();
    assert_eq!(tools[0].contributed_by.as_deref(), Some("vera"));
}

/// **Witness: a contributed tool authorizes as its plugin, never as the
/// human.** The rule the whole package surface rests on.
#[test]
fn a_contributed_tool_is_authorized_as_the_plugin_and_a_users_own_is_not() {
    let root = tempfile::tempdir().expect("a temp dir");
    manifest_at(&root.path().join(".stella").join("tools"), "mine");
    let plugin_dir = root.path().join("pkg").join("tools");
    manifest_at(&plugin_dir, "theirs");

    let (tools, _) = discover_with_plugins(
        root.path(),
        None,
        true,
        &[PluginToolDir {
            plugin: "vera".into(),
            dir: plugin_dir,
        }],
    )
    .into_parts();

    let mine = tools.iter().find(|t| t.name == "mine").expect("mine");
    let theirs = tools.iter().find(|t| t.name == "theirs").expect("theirs");
    assert_eq!(mine.principal(&Principal::User), Principal::User);
    assert_eq!(
        theirs.principal(&Principal::User),
        Principal::Plugin("vera".into()),
        "a plugin's script never runs under the operator's identity"
    );
    // And it does not widen either: a lane's principal is not inherited.
    assert_eq!(
        theirs.principal(&Principal::Role("worker".into())),
        Principal::Plugin("vera".into())
    );
    assert_eq!(
        mine.principal(&Principal::Role("worker".into())),
        Principal::Role("worker".into())
    );
}

/// **Witness: precedence.** A package may not capture a name the user's own
/// manifest already defines, and the collision is reported rather than
/// silently resolved.
#[test]
fn a_plugin_never_takes_a_name_the_user_already_defined() {
    let root = tempfile::tempdir().expect("a temp dir");
    manifest_at(&root.path().join(".stella").join("tools"), "deploy");
    let plugin_dir = root.path().join("pkg").join("tools");
    manifest_at(&plugin_dir, "deploy");

    let found = discover_with_plugins(
        root.path(),
        None,
        true,
        &[PluginToolDir {
            plugin: "vera".into(),
            dir: plugin_dir,
        }],
    );
    let reasons: Vec<String> = found
        .diagnostics()
        .iter()
        .map(|d| d.reason.clone())
        .collect();
    assert_eq!(reasons.len(), 1, "{reasons:?}");
    assert!(
        reasons[0].contains("yours is the one that runs"),
        "{reasons:?}"
    );

    let (tools, _) = found.into_parts();
    assert_eq!(tools.len(), 1);
    assert_eq!(
        tools[0].contributed_by, None,
        "the surviving `deploy` is the user's own"
    );
}

/// Plugin-versus-plugin resolves deterministically by the order the host
/// supplies (roster order, i.e. plugin name), and the loser is reported.
#[test]
fn two_plugins_claiming_one_name_resolve_in_roster_order() {
    let root = tempfile::tempdir().expect("a temp dir");
    let first = root.path().join("alpha").join("tools");
    let second = root.path().join("zeta").join("tools");
    manifest_at(&first, "shared");
    manifest_at(&second, "shared");

    let found = discover_with_plugins(
        root.path(),
        None,
        true,
        &[
            PluginToolDir {
                plugin: "alpha".into(),
                dir: first,
            },
            PluginToolDir {
                plugin: "zeta".into(),
                dir: second,
            },
        ],
    );
    assert_eq!(found.diagnostics().len(), 1);
    let (tools, _) = found.into_parts();
    assert_eq!(tools[0].contributed_by.as_deref(), Some("alpha"));
}

/// A manifest cannot claim a provenance it does not have: `contributed_by`
/// comes from the directory discovery read, and `parse_manifest` — the only
/// path a manifest's own bytes take — always leaves it `None`.
#[test]
fn a_manifest_cannot_claim_to_have_been_shipped_by_a_plugin() {
    let tool = parse_manifest(
        "name = \"xx\"\ndescription = \"d\"\ncommand = [\"./x.sh\"]\ncontributed_by = \"vera\"\n",
        Path::new("/x/xx.toml"),
    )
    .expect("unknown fields are ignored, as they always have been");
    assert_eq!(tool.contributed_by, None);
}
