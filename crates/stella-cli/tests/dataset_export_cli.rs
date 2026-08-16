//! End-to-end witness for `stella dataset export` (#872, extended by #2083):
//! drive the real binary against a seeded workspace and check what the issues
//! actually ask for — the two files, the redaction of a planted key in a
//! prompt, a tool argument, AND a reconstructed transcript, the provenance
//! stamp, the reward labels with their policy, the transcript-verification
//! gate with its manifest count, and byte-identity across runs.
//!
//! Every assertion here fails on the pre-#872 tree, where the subcommand does
//! not parse at all (clap exits 2 with "unrecognized subcommand"); the #2083
//! assertions fail on the pre-#2083 tree, where records carry no `calls` or
//! `reward` and the unvouched execution is exported instead of counted.

use std::path::Path;
use std::process::Command;

use stella_protocol::{
    AgentEvent, FileChangeKind, FlipOutcome, LadderRung, LadderSnapshot, ModelCallRole, ToolCall,
    ToolOutput, VerdictEvidence,
};
use stella_store::{ContextBlockRow, ManifestBlockRow, StepManifestRow, Store};

/// A GitHub PAT shape planted in the user's prompt.
const PROMPT_SECRET: &str = "ghp_016C7e4a9b2d3f5081726354ABCDabcd1234";
/// An AWS key id planted in an `edit` tool's arguments — the path the prompt
/// alone would not cover.
const ARG_SECRET: &str = "AKIAIOSFODNN7EXAMPLE";

/// `sha256:<hex>` over `content` — the digest form the receipts plane records
/// and reconstruction re-checks.
fn digest(content: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}")
}

/// A block whose content the receipt carries locally (system prefix, goal).
fn gap_block(block_id: &str, kind: &str, content: &str) -> ContextBlockRow {
    ContextBlockRow {
        block_id: block_id.into(),
        kind: kind.into(),
        origin_turn: 0,
        origin_step: 0,
        call_id: None,
        memory_id: None,
        token_cost: Some(10),
        content_digest: digest(content),
        citation_label: None,
        content: Some(content.into()),
    }
}

/// A block that resolves from the journal by `call_id` and must re-hash to
/// its recorded digest.
fn journal_block(block_id: &str, kind: &str, call_id: &str, content: &str) -> ContextBlockRow {
    ContextBlockRow {
        block_id: block_id.into(),
        kind: kind.into(),
        origin_turn: 0,
        origin_step: 0,
        call_id: Some(call_id.into()),
        memory_id: None,
        token_cost: Some(10),
        content_digest: digest(content),
        citation_label: None,
        content: None,
    }
}

fn manifest_entry(block_id: &str, message_index: u64) -> ManifestBlockRow {
    ManifestBlockRow {
        block_id: block_id.into(),
        cache_zone: "cacheable".into(),
        token_cost: Some(10),
        resident_since_step: 0,
        message_index,
        call_id: None,
    }
}

/// The worker call's receipt at (turn 0, step 0, call_seq 0).
fn worker_manifest(blocks: Vec<ManifestBlockRow>) -> StepManifestRow {
    StepManifestRow {
        turn_instance: 0,
        step: 0,
        call_seq: 0,
        provider: "zai".into(),
        model: "glm-5.2".into(),
        call_role: "worker".into(),
        effective_budget_tokens: 1000,
        calibration_factor: 1.0,
        estimated_input_tokens: 40,
        compiled_frame_id: None,
        frame_hash: None,
        blocks,
    }
}

/// Ladder evidence naming the rung a verdict settled on — what the reward
/// label is derived from (#1043).
fn ladder(rung: LadderRung) -> Option<Box<LadderSnapshot>> {
    Some(Box::new(LadderSnapshot {
        rung: Some(rung),
        tracked_command: None,
        oracle_trace: Vec::new(),
        flip: if matches!(rung, LadderRung::SubmitFast) {
            FlipOutcome::Achieved
        } else {
            FlipOutcome::NotAchieved
        },
        unstable_flip: false,
        flip_refused_different_failure: false,
        touched_tests_passed: None,
        test_infra: None,
        diff_lines: 2,
        diff_budget: 100,
        diff_available: true,
        mutating_actions: 1,
        new_diag_errors: 0,
        new_diag_warnings: 0,
        witness_intact: None,
        witness_mutation: None,
        diff_coverage: None,
        verify_done_flip: false,
        no_test_surface: false,
        errored_commands: 0,
        verifier_independent: None,
    }))
}

/// A workspace whose store holds four settled executions, one per filter arm:
///
/// - `flipped`: accepted — `completed`, a mutating change, a digest-verified
///   worker transcript, and a `SubmitFast` verdict (the deterministic flip,
///   reward outcome +1.0);
/// - `tampered`: accepted — same shape, but the verdict settled `Revise`
///   with `passed: false` (the tampered/red-test arm, reward outcome −1.0);
/// - one `aborted` turn, excluded by outcome;
/// - one `completed` turn whose only receipt cites a block with no journal
///   preimage, so its reconstruction cannot verify: excluded by the
///   transcript gate and counted in the manifest.
///
/// A project-scope `.stella/settings.json` pins the default reward weights
/// explicitly, so the labels asserted below cannot drift with whatever the
/// developer's user-scope settings say.
fn seeded_workspace() -> (tempfile::TempDir, i64, i64) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    std::fs::write(
        dir.path().join(".stella").join("settings.json"),
        r#"{"reward":{"deterministic_weight":1.0,"per_step":0.02,"per_usd":0.5,"per_revision":0.1}}"#,
    )
    .expect("settings");

    let prompt = format!("rotate the deploy key {PROMPT_SECRET}");
    let flipped = store
        .begin_execution("run", &prompt, "zai", "glm-5.2")
        .expect("execution");
    store
        .set_execution_session(flipped, "ses-fixture")
        .expect("session");

    let call = ToolCall {
        call_id: "c1".into(),
        name: "edit".into(),
        input: serde_json::json!({
            "path": "src/lib.rs",
            "new_string": format!("const KEY: &str = \"{ARG_SECRET}\";"),
        }),
    };
    let output = ToolOutput::Ok {
        content: "edited src/lib.rs".into(),
        data: None,
    };
    let call_json = serde_json::to_string(&call).expect("call json");
    let output_json = serde_json::to_string(&output).expect("output json");

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
        AgentEvent::ToolStart { call: call.clone() },
        AgentEvent::ToolResult {
            call_id: "c1".into(),
            output: output.clone(),
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
        AgentEvent::Verdict {
            passed: true,
            evidence: VerdictEvidence {
                summary: "the touched tests are green".into(),
                deterministic: true,
                evidence_refs: Vec::new(),
                ladder: ladder(LadderRung::SubmitFast),
            },
        },
    ];
    for (seq, event) in events.iter().enumerate() {
        store
            .record_event(flipped, seq as u64, event)
            .expect("event");
    }
    // The receipt: what the worker call saw, digest-verified on reconstruction.
    // The tool round-trip resolves from the journal events above; the system
    // prefix and goal carry their content locally, like real gap blocks.
    for block in [
        gap_block("blk_sys", "system_prefix", "you are a careful engineer"),
        gap_block("blk_goal", "user_goal", &prompt),
        journal_block("blk_call", "tool_call", "c1", &call_json),
        journal_block("blk_res", "tool_result", "c1", &output_json),
    ] {
        store
            .record_context_block(flipped, &block)
            .expect("context block");
    }
    store
        .record_step_manifest(
            flipped,
            &worker_manifest(vec![
                manifest_entry("blk_sys", 0),
                manifest_entry("blk_goal", 1),
                manifest_entry("blk_call", 2),
                manifest_entry("blk_res", 3),
            ]),
        )
        .expect("manifest");
    store
        .finish_execution(flipped, "completed", 0.01)
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

    // A settled success whose witness was tampered with: the ladder refused
    // the flip and settled Revise/failed. Accepted by the outcome-and-change
    // predicate, exported, and labelled −1.0.
    let tampered = store
        .begin_execution("run", "make the witness pass", "zai", "glm-5.2")
        .expect("execution");
    let tampered_events = [
        AgentEvent::FileChange {
            path: "tests/witness.rs".into(),
            kind: FileChangeKind::Modified,
            added: 2,
            removed: 0,
            diff: None,
        },
        AgentEvent::Verdict {
            passed: false,
            evidence: VerdictEvidence {
                summary: "the worker modified the witness files".into(),
                deterministic: true,
                evidence_refs: Vec::new(),
                ladder: ladder(LadderRung::Revise),
            },
        },
    ];
    for (seq, event) in tampered_events.iter().enumerate() {
        store
            .record_event(tampered, seq as u64, event)
            .expect("event");
    }
    for block in [
        gap_block("blk_sys", "system_prefix", "you are a careful engineer"),
        gap_block("blk_goal", "user_goal", "make the witness pass"),
    ] {
        store
            .record_context_block(tampered, &block)
            .expect("context block");
    }
    store
        .record_step_manifest(
            tampered,
            &worker_manifest(vec![
                manifest_entry("blk_sys", 0),
                manifest_entry("blk_goal", 1),
            ]),
        )
        .expect("manifest");
    store
        .finish_execution(tampered, "completed", 0.02)
        .expect("finish");

    // A settled success whose only receipt cites a tool result with no
    // journal preimage: reconstruction reports it unresolved, so the
    // execution cannot vouch for what its model saw. Absent from the
    // dataset, counted in the manifest.
    let unvouched = store
        .begin_execution("run", "an unprovable transcript", "zai", "glm-5.2")
        .expect("execution");
    store
        .record_event(
            unvouched,
            0,
            &AgentEvent::FileChange {
                path: "src/lib.rs".into(),
                kind: FileChangeKind::Modified,
                added: 1,
                removed: 0,
                diff: None,
            },
        )
        .expect("event");
    store
        .record_context_block(
            unvouched,
            &journal_block(
                "blk_orphan",
                "tool_result",
                "missing",
                "{\"ok\":{\"content\":\"x\"}}",
            ),
        )
        .expect("context block");
    store
        .record_step_manifest(
            unvouched,
            &worker_manifest(vec![manifest_entry("blk_orphan", 0)]),
        )
        .expect("manifest");
    store
        .finish_execution(unvouched, "completed", 0.01)
        .expect("finish");

    (dir, flipped, tampered)
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

/// The acceptance shape of #872 + #2083: two files, one record per accepted
/// turn, the full provenance stamp, the trajectory, the verified transcript,
/// and the reward labels the issues describe.
#[test]
fn export_writes_one_record_per_accepted_turn_with_its_provenance() {
    let (dir, flipped, tampered) = seeded_workspace();
    let summary = export(&dir, "out", &[]);
    assert!(
        summary.contains("2 accepted turn(s) from 4 settled execution(s)"),
        "the summary reports what it kept and what it read: {summary}"
    );

    let jsonl = String::from_utf8(read(dir.path(), "out", "dataset.jsonl")).expect("utf8");
    assert_eq!(
        jsonl.lines().count(),
        2,
        "the aborted and the unvouched executions are excluded: {jsonl}"
    );
    let mut lines = jsonl.lines();
    let record: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();

    assert_eq!(record["execution_id"], flipped);
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

    // #2083: the verified transcript. One worker call, non-empty
    // prompt_messages, and the exact message sequence the receipt recorded —
    // with the planted prompt secret redacted inside it.
    assert_eq!(record["calls"].as_array().map(Vec::len), Some(1));
    let call = &record["calls"][0];
    assert_eq!(call["role"], "worker");
    assert_eq!(call["turn_instance"], 0);
    assert_eq!(call["call_seq"], 0);
    let messages = call["prompt_messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 4, "system, goal, tool call, tool result");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");
    let goal = messages[1]["content"].as_str().expect("goal content");
    assert!(
        goal.starts_with("rotate the deploy key") && goal.contains("[redacted]"),
        "the transcript is the reconstructed goal, secret-redacted: {goal}"
    );

    // #2083: the reward label — the deterministic flip earns +1.0, and the
    // policy it was computed under rides on the label.
    assert_eq!(record["reward"]["rung"], "submit_fast");
    assert_eq!(record["reward"]["outcome"], 1.0);
    assert_eq!(record["reward"]["policy"]["outcome"]["deterministic"], 1.0);
    let reward = record["reward"]["reward"].as_f64().expect("scored");
    // outcome 1.0 − 0.02·1 step − 0.5·$0.01 − 0 revisions.
    assert!(
        (reward - 0.975).abs() < 1e-9,
        "the composite reward prices the effort: {reward}"
    );

    // The tampered-witness turn is the second record: same acceptance, the
    // negative label.
    let negative: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(negative["execution_id"], tampered);
    assert_eq!(negative["reward"]["rung"], "revise");
    assert_eq!(negative["reward"]["outcome"], -1.0);
    assert_eq!(negative["verdict"]["passed"], false);

    // The manifest states the rule that produced the selection — including
    // the transcript gate — and counts what that gate excluded.
    let manifest: serde_json::Value =
        serde_json::from_slice(&read(dir.path(), "out", "manifest.json")).expect("manifest json");
    assert_eq!(manifest["records"], 2);
    assert_eq!(manifest["filter"]["executions_scanned"], 4);
    assert_eq!(manifest["filter"]["executions_transcripts_unverified"], 1);
    assert_eq!(manifest["filter"]["executions_accepted"], 2);
    assert_eq!(
        manifest["filter"]["acceptance_predicate"],
        "executions.outcome in {completed, goal_met} AND at least one mutating \
         file_change event AND at least one recorded model call, every one \
         reconstructing digest-verified (Reconstruction::is_verified)"
    );
    assert_eq!(
        manifest["redaction"]["function"],
        "stella_core::redact::redact_secrets"
    );
    assert_eq!(manifest["execution_id_range"][0], flipped);
    assert_eq!(manifest["execution_id_range"][1], tampered);
}

/// The redaction criterion, stated exactly as #872 states it: a synthetic API
/// key planted in a prompt OR in a tool argument does not appear in the
/// output. Asserted over the RAW BYTES of both files, so no serialization
/// detail can hide a leak.
#[test]
fn a_planted_key_in_the_prompt_or_a_tool_argument_never_reaches_the_output() {
    let (dir, _, _) = seeded_workspace();
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
    let (dir, _, _) = seeded_workspace();
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
    let (dir, _, _) = seeded_workspace();

    export(&dir, "future", &["--since", "2099-01-01"]);
    let manifest: serde_json::Value =
        serde_json::from_slice(&read(dir.path(), "future", "manifest.json")).expect("json");
    assert_eq!(manifest["records"], 0);
    assert_eq!(manifest["filter"]["executions_scanned"], 4);
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
    assert_eq!(
        manifest["records"], 1,
        "only the flip's PASSING verdict qualifies — the tampered turn's failed one does not"
    );
    assert!(
        manifest["filter"]["acceptance_predicate"]
            .as_str()
            .is_some_and(|p| p.contains("verdict")),
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

    let (dir, _, _) = seeded_workspace();
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
