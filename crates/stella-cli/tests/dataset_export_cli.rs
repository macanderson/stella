//! End-to-end witness for `stella dataset export` (#872): drive the real
//! binary against a seeded workspace and check the four things the issue
//! actually asks for — the two files, the redaction of a planted key in both
//! a prompt and a tool argument, the provenance stamp, and byte-identity
//! across runs.
//!
//! Every assertion here fails on the pre-#872 tree, where the subcommand does
//! not parse at all (clap exits 2 with "unrecognized subcommand").

use std::path::Path;
use std::process::Command;

use stella_protocol::{
    AgentEvent, FileChangeKind, JudgeEvidence, ModelCallRole, ToolCall, ToolOutput,
};
use stella_store::Store;

/// A GitHub PAT shape planted in the user's prompt.
const PROMPT_SECRET: &str = "ghp_016C7e4a9b2d3f5081726354ABCDabcd1234";
/// An AWS key id planted in an `edit` tool's arguments — the path the prompt
/// alone would not cover.
const ARG_SECRET: &str = "AKIAIOSFODNN7EXAMPLE";

/// A workspace whose store holds one ACCEPTED turn (settled `completed`, with
/// a mutating file change) and one rejected one (`aborted`), so the filter has
/// something to exclude as well as something to keep.
fn seeded_workspace() -> (tempfile::TempDir, i64) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");

    let accepted = store
        .begin_execution(
            "run",
            &format!("rotate the deploy key {PROMPT_SECRET}"),
            "zai",
            "glm-5.2",
        )
        .expect("execution");
    store
        .set_execution_session(accepted, "ses-fixture")
        .expect("session");

    let events = [
        AgentEvent::StepManifest {
            turn_instance: 0,
            step: 0,
            call_seq: 0,
            role: ModelCallRole::Worker,
            provider: "zai".into(),
            model: "glm-5.2".into(),
            blocks: Vec::new(),
            effective_budget_tokens: 1000,
            calibration_factor: 1.0,
            estimated_input_tokens: 10,
            compiled_frame: None,
        },
        AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "c1".into(),
                name: "edit".into(),
                input: serde_json::json!({
                    "path": "src/lib.rs",
                    "new_string": format!("const KEY: &str = \"{ARG_SECRET}\";"),
                }),
            },
        },
        AgentEvent::ToolResult {
            call_id: "c1".into(),
            output: ToolOutput::Ok {
                content: "edited src/lib.rs".into(),
            },
            duration_ms: 5,
            speculated: false,
        },
        AgentEvent::FileChange {
            path: "src/lib.rs".into(),
            kind: FileChangeKind::Modified,
            added: 1,
            removed: 1,
            diff: Some("@@ -1 +1 @@\n-old\n+new\n".into()),
        },
        AgentEvent::JudgeVerdict {
            passed: true,
            evidence: JudgeEvidence {
                summary: "the touched tests are green".into(),
                deterministic: true,
                evidence_refs: Vec::new(),
                ladder: None,
            },
        },
    ];
    for (seq, event) in events.iter().enumerate() {
        store
            .record_event(accepted, seq as u64, event)
            .expect("event");
    }
    store
        .finish_execution(accepted, "completed", 0.01)
        .expect("finish");

    // A turn that changed a file but did not land: settled, and excluded.
    let rejected = store
        .begin_execution("run", "try the other approach", "zai", "glm-5.2")
        .expect("execution");
    store
        .record_event(
            rejected,
            0,
            &AgentEvent::FileChange {
                path: "src/other.rs".into(),
                kind: FileChangeKind::Modified,
                added: 3,
                removed: 0,
                diff: None,
            },
        )
        .expect("event");
    store
        .finish_execution(rejected, "aborted", 0.02)
        .expect("finish");

    (dir, accepted)
}

fn export(dir: &tempfile::TempDir, out: &str, extra: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_stella"))
        .args(["dataset", "export", "--format", "jsonl", "--output", out])
        .args(extra)
        .current_dir(dir.path())
        // The summary colours its paths, and a developer with CLICOLOR_FORCE=1
        // exported would hand the child ANSI escapes every `contains` below
        // then fails to match. Pin the colour decision instead of inheriting
        // it. STELLA_HOME likewise redirects the store out from under us.
        .env_remove("CLICOLOR_FORCE")
        .env_remove("STELLA_HOME")
        .env("NO_COLOR", "1")
        .output()
        .expect("run stella dataset export");
    assert!(
        output.status.success(),
        "stella dataset export {extra:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn read(dir: &Path, out: &str, file: &str) -> Vec<u8> {
    let path = dir.join(out).join(file);
    std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The acceptance shape of #872: two files, one record per accepted turn, the
/// full provenance stamp, and the trajectory the issue describes.
#[test]
fn export_writes_one_record_per_accepted_turn_with_its_provenance() {
    let (dir, accepted) = seeded_workspace();
    let summary = export(&dir, "out", &[]);
    assert!(
        summary.contains("1 accepted turn(s) from 2 settled execution(s)"),
        "the summary reports what it kept and what it read: {summary}"
    );

    let jsonl = String::from_utf8(read(dir.path(), "out", "dataset.jsonl")).expect("utf8");
    assert_eq!(
        jsonl.lines().count(),
        1,
        "the aborted execution is excluded: {jsonl}"
    );
    let record: serde_json::Value = serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();

    assert_eq!(record["execution_id"], accepted);
    assert_eq!(record["session_id"], "ses-fixture");
    assert!(
        record["repo"].as_str().is_some_and(|r| !r.is_empty()),
        "every record is attributable to a repo: {record}"
    );
    assert!(
        record["timestamp"]
            .as_str()
            .is_some_and(|t| t.starts_with("20")),
        "timestamp is the execution's started_at: {record}"
    );
    assert_eq!(record["turn_instance"], 0);
    assert_eq!(record["outcome"], "completed");

    // The trajectory: prompt -> tool call (args + output) -> the change.
    assert_eq!(record["tool_calls"].as_array().map(Vec::len), Some(1));
    assert_eq!(record["tool_calls"][0]["name"], "edit");
    assert_eq!(record["tool_calls"][0]["arguments"]["path"], "src/lib.rs");
    assert_eq!(record["tool_calls"][0]["output"], "edited src/lib.rs");
    assert_eq!(record["changes"].as_array().map(Vec::len), Some(1));
    assert_eq!(record["changes"][0]["path"], "src/lib.rs");
    assert_eq!(record["changes"][0]["added"], 1);
    assert_eq!(record["verdict"]["passed"], true);
    assert_eq!(record["verdict"]["deterministic"], true);

    // The manifest states the rule that produced the selection.
    let manifest: serde_json::Value =
        serde_json::from_slice(&read(dir.path(), "out", "manifest.json")).expect("manifest json");
    assert_eq!(manifest["records"], 1);
    assert_eq!(manifest["filter"]["executions_scanned"], 2);
    assert_eq!(manifest["filter"]["executions_accepted"], 1);
    assert_eq!(
        manifest["filter"]["acceptance_predicate"],
        "executions.outcome in {completed, goal_met} AND at least one mutating file_change event"
    );
    assert_eq!(
        manifest["redaction"]["function"],
        "stella_core::redact::redact_secrets"
    );
    assert_eq!(manifest["execution_id_range"][0], accepted);
}

/// The redaction criterion, stated exactly as #872 states it: a synthetic API
/// key planted in a prompt OR in a tool argument does not appear in the
/// output. Asserted over the RAW BYTES of both files, so no serialization
/// detail can hide a leak.
#[test]
fn a_planted_key_in_the_prompt_or_a_tool_argument_never_reaches_the_output() {
    let (dir, _) = seeded_workspace();
    export(&dir, "out", &[]);

    for file in ["dataset.jsonl", "manifest.json"] {
        let bytes = read(dir.path(), "out", file);
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(
            !text.contains(PROMPT_SECRET),
            "the prompt's key survived into {file}: {text}"
        );
        assert!(
            !text.contains(ARG_SECRET),
            "the tool argument's key survived into {file}: {text}"
        );
    }

    let jsonl = String::from_utf8(read(dir.path(), "out", "dataset.jsonl")).expect("utf8");
    assert!(
        jsonl.contains("[redacted]"),
        "both secrets were replaced, not merely dropped: {jsonl}"
    );
    let record: serde_json::Value = serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
    assert_eq!(
        record["redacted"], true,
        "the record says redaction happened rather than leaving it to be assumed"
    );
}

/// Determinism: the same store and the same filter produce byte-identical
/// files. Run twice into different directories and compare the bytes.
#[test]
fn the_same_store_and_filter_export_byte_identical_files() {
    let (dir, _) = seeded_workspace();
    export(&dir, "first", &[]);
    export(&dir, "second", &[]);

    for file in ["dataset.jsonl", "manifest.json"] {
        assert_eq!(
            read(dir.path(), "first", file),
            read(dir.path(), "second", file),
            "{file} is not byte-stable across runs"
        );
    }
}

/// The filter flags actually filter, and the manifest reports the narrowed
/// rule rather than the default one.
#[test]
fn the_date_window_and_require_verdict_are_reported_as_applied() {
    let (dir, _) = seeded_workspace();

    export(&dir, "future", &["--since", "2099-01-01"]);
    let manifest: serde_json::Value =
        serde_json::from_slice(&read(dir.path(), "future", "manifest.json")).expect("json");
    assert_eq!(manifest["records"], 0);
    assert_eq!(manifest["filter"]["executions_scanned"], 2);
    assert_eq!(manifest["filter"]["executions_in_window"], 0);
    assert_eq!(manifest["filter"]["since"], "2099-01-01");
    assert_eq!(manifest["execution_id_range"], serde_json::Value::Null);
    assert!(
        read(dir.path(), "future", "dataset.jsonl").is_empty(),
        "an empty dataset is still written — report regardless of state"
    );

    export(&dir, "judged", &["--require-verdict"]);
    let manifest: serde_json::Value =
        serde_json::from_slice(&read(dir.path(), "judged", "manifest.json")).expect("json");
    assert_eq!(manifest["records"], 1, "the fixture's verdict passed");
    assert!(
        manifest["filter"]["acceptance_predicate"]
            .as_str()
            .is_some_and(|p| p.contains("judge_verdict")),
        "the tightened predicate is what the manifest states: {manifest}"
    );
}

/// The dataset carries redacted prompts and full tool outputs, which is at
/// least as sensitive as the session archive that was hardened for the same
/// reason — so the directory is owner-only and so are both files.
#[cfg(unix)]
#[test]
fn the_dataset_and_its_directory_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let (dir, _) = seeded_workspace();
    export(&dir, "out", &[]);

    let out = dir.path().join("out");
    let mode = std::fs::metadata(&out).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700, "the output directory is owner-only");
    for file in ["dataset.jsonl", "manifest.json"] {
        let mode = std::fs::metadata(out.join(file))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "{file} is owner-only");
    }
}
