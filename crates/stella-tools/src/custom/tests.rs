// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Unit tests for [`super`]: manifest parsing, discovery, and the
//! `CustomToolSet` composition tests plus the execution fixtures they share
//! with [`execution`]. The execution surface itself, manifest claims, and
//! plugin-contributed tools live in [`execution`] and [`claims_and_plugins`]
//! respectively — siblings rather than sections here, for the same reason
//! this file is a sibling of `custom.rs`.
//!
//! Split out of `custom.rs` (like `process.rs` / `process/tests.rs` and
//! `registry.rs` / `registry/tests.rs`) so the module that ships custom
//! tools is not dominated by the module that checks them, and so the file
//! stays under the 1500-line gate. A child module, so `super::*` still
//! reaches the private surface.
use std::os::unix::fs::PermissionsExt;

use super::*;

mod claims_and_plugins;
mod execution;
mod reservation;

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

/// #3237: a name Stella once dispatched and retired is refused too. The
/// catalog stopped reserving it the moment the row was deleted, so a manifest
/// could claim `run_tests` or `graph_query` and inherit the model's priors
/// about the built-in that used to answer to it.
#[test]
fn retired_builtin_names_are_rejected() {
    for retired in crate::catalog::RETIRED_TOOL_NAMES
        .iter()
        .chain(crate::catalog::RETIRED_NAMES_TOO_AMBIGUOUS_TO_SCAN)
    {
        let src = format!("name = \"{retired}\"\ndescription = \"d\"\ncommand = [\"./x.sh\"]");
        let err = parse_manifest(&src, Path::new("t.toml")).unwrap_err();
        assert!(err.contains("is reserved"), "reserved `{retired}` -> {err}");

        // The REASON has to match the name, not just be present. A name that
        // is also a live group switch key gets a different sentence, because
        // telling an operator that `{"task": "off"}` addresses "nothing" is
        // false -- it withholds every tool in that group (#3192).
        if crate::catalog::groups().contains(retired) {
            assert!(
                err.contains("switch key for the") && err.contains("group"),
                "group key `{retired}` should be refused as a group key -> {err}"
            );
            assert!(
                !err.contains("instead of nothing"),
                "group key `{retired}` must not claim it addresses nothing -> {err}"
            );
        } else {
            assert!(err.contains("retired"), "retired `{retired}` -> {err}");
        }
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

// execution fixtures — shared by `execution` and `CustomToolSet composition`
// below, so they stay here rather than moving into the `execution` submodule.

/// Budget for an execution test that is *not* about the timeout: the script is
/// expected to finish, so the only honest budget is the largest one a manifest
/// may ask for. A hand-picked few seconds measures how loaded the machine is
/// rather than anything about the tool, and a saturated `make gate` run turned
/// a trivial `/bin/sh` spawn into three red tests (#2011). Only
/// [`timeout_kills_and_returns_fast`] sets its own budget, because there the
/// budget is the subject.
const NO_TIMEOUT_MS: u64 = MAX_TIMEOUT_MS;

/// Write an executable `#!/bin/sh` script into `root` and return a
/// [`CustomTool`] whose relative `command[0]` resolves against `root`.
///
/// The budget is [`NO_TIMEOUT_MS`]; a test whose subject is the timeout uses
/// [`script_tool_with_timeout`].
fn script_tool(root: &Path, file: &str, body: &str) -> CustomTool {
    script_tool_with_timeout(root, file, body, NO_TIMEOUT_MS)
}

/// [`script_tool`] with an explicit budget — for the one test that measures it.
fn script_tool_with_timeout(root: &Path, file: &str, body: &str, timeout_ms: u64) -> CustomTool {
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
    let tool = script_tool(dir.path(), "s.sh", "#!/bin/sh\necho hi\n");
    let inner = FakeInner;
    let set = CustomToolSet::new(&inner, vec![tool], dir.path().to_path_buf());
    let names: Vec<String> = set.schemas().into_iter().map(|s| s.name).collect();
    assert!(names.contains(&"bash".to_string()));
    assert!(names.contains(&"t".to_string()));
}

#[tokio::test]
async fn set_routes_custom_names_and_falls_through_for_others() {
    let dir = tempfile::tempdir().unwrap();
    let mut tool = script_tool(dir.path(), "s.sh", "#!/bin/sh\necho from_custom\n");
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
    let mut tool = script_tool(dir.path(), "s.sh", "#!/bin/sh\necho from_custom\n");
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
