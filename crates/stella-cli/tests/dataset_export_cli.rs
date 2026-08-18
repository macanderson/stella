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
//! `reward` and the unvouched execution is exported instead of counted; and
//! the #2123 assertions fail on the pre-#2123 tree, where
//! `--include-unverified-transcripts` is not an argument at all and the
//! unvouched turn is unreachable by any invocation.
//!
//! The last of those is the pre-receipts turn's reward label: read the empty
//! receipts plane as a zero step count and the record exports a scalar
//! *above* what the same trajectory earns once its calls are counted, on the
//! records whose provenance is weakest. That is what
//! `a_turn_with_no_receipts_at_all_withholds_the_reward_scalar_it_cannot_shape`
//! pins.

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

/// The seeded workspace and the execution ids the assertions name.
struct Seeded {
    dir: tempfile::TempDir,
    flipped: i64,
    tampered: i64,
    /// The turn whose receipts plane holds nothing at all — see
    /// [`seeded_workspace`].
    pre_receipts: i64,
}

/// A workspace whose store holds five settled executions, one per filter arm:
///
/// - `flipped`: accepted — `completed`, a mutating change, a digest-verified
///   worker transcript, and a `SubmitFast` verdict (the deterministic flip,
///   reward outcome +1.0);
/// - `tampered`: accepted — same shape, but the verdict settled `Revise`
///   with `passed: false` (the tampered/red-test arm, reward outcome −1.0);
/// - one `aborted` turn, excluded by outcome;
/// - one `completed` turn whose only receipt cites a block with no journal
///   preimage, so its reconstruction cannot verify: excluded by the
///   transcript gate and counted in the manifest;
/// - `pre_receipts`: a `completed` turn with a mutating change and a passing
///   verdict but **no `step_receipt` row at all** — the shape of a whole store
///   predating the receipts plane, which is the headline case #2123 exists
///   for. It reaches the unvouched arm by the other route (nothing to
///   reconstruct rather than a reconstruction that fell short), and it is the
///   only fixture on which the reward's step term has nothing to count.
///
/// A project-scope `.stella/settings.json` pins the default reward weights
/// explicitly, so the labels asserted below cannot drift with whatever the
/// developer's user-scope settings say.
fn seeded_workspace() -> Seeded {
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

    // A settled success from before the receipts plane existed: the journal
    // holds the change and the verdict, and `step_receipt` holds nothing, so
    // there is no recorded model call to reconstruct — and no count of how
    // many calls the turn actually bought.
    let pre_receipts = store
        .begin_execution(
            "run",
            "a turn from before the receipts plane",
            "zai",
            "glm-5.2",
        )
        .expect("execution");
    let pre_receipts_events = [
        AgentEvent::FileChange {
            path: "src/legacy.rs".into(),
            kind: FileChangeKind::Modified,
            added: 4,
            removed: 1,
            diff: None,
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
    for (seq, event) in pre_receipts_events.iter().enumerate() {
        store
            .record_event(pre_receipts, seq as u64, event)
            .expect("event");
    }
    store
        .finish_execution(pre_receipts, "completed", 0.10)
        .expect("finish");

    Seeded {
        dir,
        flipped,
        tampered,
        pre_receipts,
    }
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
    let Seeded {
        dir,
        flipped,
        tampered,
        ..
    } = seeded_workspace();
    let summary = export(&dir, "out", &[]);
    assert!(
        summary.contains("2 accepted turn(s) from 5 settled execution(s)"),
        "the summary reports what it kept and what it read: {summary}"
    );

    let jsonl = String::from_utf8(read(dir.path(), "out", "dataset.jsonl")).expect("utf8");
    assert_eq!(
        jsonl.lines().count(),
        2,
        "the aborted and the two unvouched executions are excluded: {jsonl}"
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
    assert_eq!(
        record["transcript_verified"], true,
        "a default-filter record's transcript is the digest-verified one (#2123)"
    );
    assert_eq!(record["transcript_mismatch_severity"], "none");
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
    assert_eq!(manifest["filter"]["executions_scanned"], 5);
    assert_eq!(manifest["filter"]["executions_transcripts_unverified"], 2);
    assert_eq!(manifest["filter"]["include_unverified_transcripts"], false);
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
    let dir = seeded_workspace().dir;
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
    let dir = seeded_workspace().dir;
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
    let dir = seeded_workspace().dir;

    export(&dir, "future", &["--since", "2099-01-01"]);
    let manifest: serde_json::Value =
        serde_json::from_slice(&read(dir.path(), "future", "manifest.json")).expect("json");
    assert_eq!(manifest["records"], 0);
    assert_eq!(manifest["filter"]["executions_scanned"], 5);
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

/// #2123: "excluded under the default filter" implies a non-default path, and
/// this is it. The unvouched turn — the shape a whole store predating the
/// receipts plane has — is absent by default and present under
/// `--include-unverified-transcripts`, carrying its journal trajectory with
/// its transcript honestly withheld rather than faked. The manifest states the
/// loosened rule verbatim and keeps counting the same executions, so the two
/// modes stay comparable.
#[test]
fn the_unvouched_turn_is_reachable_only_under_the_transcript_opt_in() {
    let dir = seeded_workspace().dir;

    // The default is unchanged: neither unvouched turn is exported.
    export(&dir, "strict", &[]);
    let strict = String::from_utf8(read(dir.path(), "strict", "dataset.jsonl")).expect("utf8");
    assert_eq!(strict.lines().count(), 2, "the default still excludes them");

    let summary = export(&dir, "loose", &["--include-unverified-transcripts"]);
    assert!(
        summary.contains("4 accepted turn(s) from 5 settled execution(s)"),
        "both unvouched turns are now kept: {summary}"
    );
    assert!(
        summary.contains("2 turn(s) exported with transcript_verified=false"),
        "the summary says the transcripts were withheld, not that nothing happened: {summary}"
    );

    let jsonl = String::from_utf8(read(dir.path(), "loose", "dataset.jsonl")).expect("utf8");
    assert_eq!(
        jsonl.lines().count(),
        4,
        "the two unvouched executions join the two verified ones: {jsonl}"
    );
    let unvouched: serde_json::Value =
        serde_json::from_str(jsonl.lines().nth(2).unwrap()).expect("record json");

    // The trajectory the journal fold can still prove.
    assert_eq!(unvouched["outcome"], "completed");
    assert_eq!(unvouched["prompt"], "an unprovable transcript");
    assert_eq!(unvouched["changes"].as_array().map(Vec::len), Some(1));
    assert_eq!(unvouched["changes"][0]["path"], "src/lib.rs");

    // ...and the transcript it cannot. Empty calls beside an explicit marker,
    // never unverified bytes wearing a verified transcript's shape.
    assert_eq!(unvouched["transcript_verified"], false);
    assert_eq!(
        unvouched["calls"].as_array().map(Vec::len),
        Some(0),
        "an unvouched transcript is withheld whole: {unvouched}"
    );
    assert_eq!(
        unvouched["transcript_mismatch_severity"], "none",
        "this fixture's receipt cites an unresolvable block; nothing mismatched"
    );
    // The verified records are unaffected by the flag.
    let verified: serde_json::Value =
        serde_json::from_str(jsonl.lines().next().unwrap()).expect("record json");
    assert_eq!(verified["transcript_verified"], true);
    assert_eq!(verified["calls"].as_array().map(Vec::len), Some(1));

    let manifest: serde_json::Value =
        serde_json::from_slice(&read(dir.path(), "loose", "manifest.json")).expect("manifest json");
    assert_eq!(
        manifest["schema"], 3,
        "the record shape changed, so did the schema"
    );
    assert_eq!(manifest["records"], 4);
    assert_eq!(manifest["filter"]["include_unverified_transcripts"], true);
    assert_eq!(
        manifest["filter"]["executions_transcripts_unverified"], 2,
        "the same count in both modes is what makes them comparable"
    );
    assert_eq!(manifest["filter"]["executions_accepted"], 4);
    assert_eq!(
        manifest["filter"]["acceptance_predicate"],
        "executions.outcome in {completed, goal_met} AND at least one mutating \
         file_change event AND (at least one recorded model call, every one \
         reconstructing digest-verified (Reconstruction::is_verified) — OR exported \
         with transcript_verified=false and no calls)",
        "the manifest states the rule that actually ran, not the default one"
    );

    // Determinism survives the non-default path.
    export(&dir, "loose-again", &["--include-unverified-transcripts"]);
    for file in ["dataset.jsonl", "manifest.json"] {
        assert_eq!(
            read(dir.path(), "loose", file),
            read(dir.path(), "loose-again", file),
            "{file} is not byte-stable under --include-unverified-transcripts"
        );
    }
}

/// #2123, the headline case: a store predating the receipts plane holds no
/// `step_receipt` row, so nothing recorded how many model calls its turns
/// bought. The flag exports those turns — and the reward label refuses to
/// price a step count nobody wrote down.
///
/// Read as zero, the shaping subtracts nothing, so the record would carry a
/// scalar strictly ABOVE what the same trajectory earns once its calls are
/// counted: `1.0 − 0.5·$0.10 = 0.95` here, against `0.93` for a five-call
/// version of the identical turn. That inflation would land on exactly the
/// records whose provenance is weakest, and nothing on the record would say
/// so. The label reports `steps_unknown` instead, keeping the rung, the
/// outcome term and the policy — everything a consumer needs to re-shape it
/// under a count of its own.
#[test]
fn a_turn_with_no_receipts_at_all_withholds_the_reward_scalar_it_cannot_shape() {
    let Seeded {
        dir, pre_receipts, ..
    } = seeded_workspace();
    export(&dir, "loose", &["--include-unverified-transcripts"]);

    let jsonl = String::from_utf8(read(dir.path(), "loose", "dataset.jsonl")).expect("utf8");
    let record: serde_json::Value = jsonl
        .lines()
        .map(|line| serde_json::from_str(line).expect("record json"))
        .find(|record: &serde_json::Value| record["execution_id"] == pre_receipts)
        .expect("the pre-receipts turn is exported under the flag");

    // The journal half is intact: this is a real trajectory, worth exporting.
    assert_eq!(record["outcome"], "completed");
    assert_eq!(record["changes"][0]["path"], "src/legacy.rs");
    assert_eq!(record["verdict"]["passed"], true);
    assert_eq!(record["transcript_verified"], false);
    assert_eq!(record["calls"].as_array().map(Vec::len), Some(0));
    assert_eq!(
        record["transcript_mismatch_severity"], "none",
        "there is nothing to reconstruct, so nothing mismatched"
    );

    // The label half: everything true is published, and only the number that
    // would have been wrong is withheld.
    assert_eq!(record["reward"]["rung"], "submit_fast");
    assert_eq!(record["reward"]["outcome"], 1.0);
    assert_eq!(record["reward"]["policy"]["shaping"]["per_step"], 0.02);
    assert!(
        record["reward"]["cost"]["steps"].is_null(),
        "an unrecorded step count is null, never a zero that prices as free: {record}"
    );
    assert!(
        record["reward"]["reward"].is_null(),
        "no scalar is claimed for a trajectory whose step cost is unknown: {record}"
    );
    assert_eq!(
        record["reward"]["discard"], "steps_unknown",
        "and the reason is named, so the row is selectable rather than lost"
    );

    // A turn whose receipts DID record its calls still scores, unverified
    // transcript or not: the gap is the step count, not the flag.
    let verified: serde_json::Value =
        serde_json::from_str(jsonl.lines().next().unwrap()).expect("record json");
    assert_eq!(verified["reward"]["cost"]["steps"], 1);
    assert!(verified["reward"]["reward"].as_f64().is_some());
}

/// The dataset carries redacted prompts and full tool outputs, which is at
/// least as sensitive as the session archive that was hardened for the same
/// reason — so the directory is owner-only and so are both files.
#[cfg(unix)]
#[test]
fn the_dataset_and_its_directory_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let dir = seeded_workspace().dir;
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
