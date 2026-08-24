//! End-to-end witness for `stella inspect`: drive the real binary against a
//! real store and check that the context a past call was sent comes back out.
//!
//! The receipts plane could reconstruct a step long before this command
//! existed; what was missing was any way to ask it. These tests exercise the
//! asking — argument parsing, the SQL, and the rendering — not the
//! reconstruction algorithm (covered by `stella-store`'s own tests).

use std::process::Command;

use stella_store::{ContextBlockRow, ManifestBlockRow, StepManifestRow, Store};

mod common;
use common::SealsEmbedderBackend;

/// Digest helper matching `stella-core`'s block identity: the store verifies
/// journal-resolved blocks against this, and skips the check for gap kinds
/// (which carry their bytes locally), so a placeholder is fine here.
fn block(block_id: &str, kind: &str, content: &str) -> ContextBlockRow {
    ContextBlockRow {
        block_id: block_id.into(),
        kind: kind.into(),
        origin_turn: 0,
        origin_step: 0,
        call_id: None,
        memory_id: None,
        token_cost: Some(10),
        content_digest: format!("sha256:{block_id}"),
        citation_label: None,
        // Gap kinds carry their preimage locally — this is the field that makes
        // a system prompt readable after the fact.
        content: Some(content.into()),
    }
}

fn entry(block_id: &str, message_index: u64) -> ManifestBlockRow {
    ManifestBlockRow {
        block_id: block_id.into(),
        cache_zone: "cacheable".into(),
        token_cost: Some(10),
        resident_since_step: 0,
        message_index,
        call_id: None,
    }
}

/// Step 0's system prompt. Step 1's is this plus one line — the shape a real
/// prompt drifts in, and the shape a whole-context view makes unreadable.
const SYSTEM_V1: &str = "You are Stella.\nBe careful.\nCite your sources.";
const SYSTEM_V2: &str =
    "You are Stella.\nBe careful.\nThe workspace is a git repo.\nCite your sources.";

/// A workspace whose store holds one execution with two model calls at the
/// SAME step: the engine's worker (seq 0) and an overflow summarizer (seq 1) —
/// plus a second worker call at step 1, so there is a same-role predecessor for
/// `--diff` to measure against.
fn seeded_workspace() -> (tempfile::TempDir, i64) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    let id = store
        .begin_execution("run", "fix the failing test", "anthropic", "opus")
        .expect("execution");

    store
        .record_context_block(id, &block("blk_sys", "system_prefix", SYSTEM_V1))
        .expect("system block");
    store
        .record_context_block(id, &block("blk_sys2", "system_prefix", SYSTEM_V2))
        .expect("drifted system block");
    store
        .record_context_block(id, &block("blk_goal", "user_goal", "fix the failing test"))
        .expect("goal block");
    store
        .record_context_block(
            id,
            &block("blk_sum", "system_prefix", "Condense this span faithfully."),
        )
        .expect("summarizer block");

    store
        .record_step_manifest(
            id,
            &StepManifestRow {
                turn_instance: 0,
                step: 0,
                call_seq: 0,
                provider: "anthropic".into(),
                model: "opus".into(),
                call_role: "worker".into(),
                effective_budget_tokens: 136_363,
                calibration_factor: 1.1,
                estimated_input_tokens: 20,
                stall_seconds_requested: None,
                compiled_frame_id: None,
                frame_hash: None,
                blocks: vec![entry("blk_sys", 0), entry("blk_goal", 1)],
            },
        )
        .expect("worker manifest");
    store
        .record_step_manifest(
            id,
            &StepManifestRow {
                turn_instance: 0,
                step: 0,
                call_seq: 1,
                provider: "anthropic".into(),
                model: "haiku".into(),
                call_role: "summarization".into(),
                effective_budget_tokens: 136_363,
                calibration_factor: 1.1,
                estimated_input_tokens: 10,
                stall_seconds_requested: None,
                compiled_frame_id: None,
                frame_hash: None,
                blocks: vec![entry("blk_sum", 0)],
            },
        )
        .expect("summarizer manifest");
    store
        .record_step_manifest(
            id,
            &StepManifestRow {
                turn_instance: 0,
                step: 1,
                call_seq: 0,
                provider: "anthropic".into(),
                model: "opus".into(),
                call_role: "worker".into(),
                effective_budget_tokens: 136_363,
                calibration_factor: 1.1,
                estimated_input_tokens: 24,
                stall_seconds_requested: None,
                compiled_frame_id: None,
                frame_hash: None,
                blocks: vec![entry("blk_sys2", 0), entry("blk_goal", 1)],
            },
        )
        .expect("second worker manifest");
    (dir, id)
}

/// Turn two's system prompt: turn one's, plus a line. Drift across turns is the
/// only drift a system prompt can legitimately have — inside one execution the
/// prefix is byte-stable for the prompt cache — so this is the shape the
/// interesting comparison actually takes.
const SYSTEM_V3: &str = "You are Stella.\nBe careful.\nThe workspace is a git repo.\nToday is Tuesday.\nCite your sources.";

/// Two executions sharing one `session_id` — i.e. two *turns* of one
/// conversation, which is what a person means by "the previous turn". Returns
/// the workspace plus both execution ids, oldest first.
fn seeded_session() -> (tempfile::TempDir, i64, i64) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    let session = "ses-test";

    let first = store
        .begin_execution("chat", "fix the failing test", "anthropic", "opus")
        .expect("first turn");
    store
        .set_execution_session(first, session)
        .expect("first session");
    store
        .record_context_block(first, &block("blk_sys", "system_prefix", SYSTEM_V2))
        .expect("system block");
    store
        .record_context_block(
            first,
            &block("blk_goal", "user_goal", "fix the failing test"),
        )
        .expect("goal block");
    store
        .record_step_manifest(
            first,
            &worker_manifest(0, vec![entry("blk_sys", 0), entry("blk_goal", 1)]),
        )
        .expect("first manifest");

    let second = store
        .begin_execution("chat", "now make it fast", "anthropic", "opus")
        .expect("second turn");
    store
        .set_execution_session(second, session)
        .expect("second session");
    store
        .record_context_block(second, &block("blk_sys3", "system_prefix", SYSTEM_V3))
        .expect("drifted system block");
    store
        .record_context_block(second, &block("blk_goal2", "user_goal", "now make it fast"))
        .expect("second goal block");
    store
        .record_step_manifest(
            second,
            &worker_manifest(0, vec![entry("blk_sys3", 0), entry("blk_goal2", 1)]),
        )
        .expect("second manifest");

    (dir, first, second)
}

fn worker_manifest(step: u64, blocks: Vec<ManifestBlockRow>) -> StepManifestRow {
    StepManifestRow {
        turn_instance: 0,
        step,
        call_seq: 0,
        provider: "anthropic".into(),
        model: "opus".into(),
        call_role: "worker".into(),
        effective_budget_tokens: 136_363,
        calibration_factor: 1.1,
        estimated_input_tokens: 20,
        stall_seconds_requested: None,
        compiled_frame_id: None,
        frame_hash: None,
        blocks,
    }
}

fn inspect(dir: &tempfile::TempDir, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_stella"))
        .without_embedder_backend()
        .arg("inspect")
        .args(args)
        .current_dir(dir.path())
        // `--diff` colours its gutter, and a developer with CLICOLOR_FORCE=1
        // exported (a common shell setting) would hand the child ANSI escapes
        // that every `contains` assertion below then fails to match. Pin the
        // colour decision instead of inheriting it.
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .output()
        .expect("run stella inspect");
    assert!(
        output.status.success(),
        "stella inspect {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn inspect_lists_executions_then_calls_then_the_context_itself() {
    let (dir, id) = seeded_workspace();

    // Level 1: the execution index.
    let index = inspect(&dir, &[]);
    assert!(
        index.contains(&id.to_string()),
        "index lists the execution: {index}"
    );
    assert!(
        index.contains("fix the failing test"),
        "index previews the prompt: {index}"
    );

    // Level 2: both calls of the step are listed — the collision this fixes.
    let calls = inspect(&dir, &[&id.to_string()]);
    assert!(calls.contains("worker"), "worker call listed: {calls}");
    assert!(
        calls.contains("summarization"),
        "the summarizer call is no longer overwritten by the worker: {calls}"
    );

    // Level 3: the worker's actual context.
    let worker = inspect(&dir, &[&id.to_string(), "--step", "0"]);
    assert!(
        worker.contains("You are Stella."),
        "the system prompt is readable: {worker}"
    );
    assert!(
        worker.contains("fix the failing test"),
        "the user goal is readable: {worker}"
    );
    assert!(
        !worker.contains("Condense this span"),
        "the worker call must not show the summarizer's prompt: {worker}"
    );

    // The summarizer's own context — previously unrecoverable entirely.
    let summarizer = inspect(&dir, &[&id.to_string(), "--step", "0", "--call-seq", "1"]);
    assert!(
        summarizer.contains("Condense this span faithfully."),
        "the summarizer's prompt is recorded and readable: {summarizer}"
    );
    assert!(
        !summarizer.contains("You are Stella."),
        "each call shows its own context: {summarizer}"
    );
}

#[test]
fn inspect_json_is_machine_readable_and_reports_verification() {
    let (dir, id) = seeded_workspace();
    let out = inspect(&dir, &[&id.to_string(), "--step", "0", "--format", "json"]);
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    assert_eq!(parsed["verified"], true, "clean path verifies: {out}");
    // The era a script needs to read `digest_mismatches` honestly, straight
    // out of the shipped binary: this execution was begun by this build, so it
    // journals its compaction rewrites and a mismatch here would mean
    // something (#1981). Nothing mismatched, so the severity is `none`.
    assert_eq!(parsed["journal_era"], "compaction_journaled", "{out}");
    assert_eq!(parsed["digest_mismatch_severity"], "none", "{out}");
    let messages = parsed["messages"].as_array().expect("messages array");
    assert_eq!(messages.len(), 2, "system + user: {out}");
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[0]["content"], SYSTEM_V1);
    assert_eq!(messages[1]["role"], "user");
}

#[test]
fn diff_against_the_previous_call_prints_only_the_line_that_moved() {
    // The complaint this answers: two system prompts differing by one line are
    // indistinguishable when both are printed in full. A unified diff makes the
    // one line the only marked line in the output.
    let (dir, id) = seeded_workspace();
    let out = inspect(
        &dir,
        &[&id.to_string(), "--step", "1", "--diff", "--only", "system"],
    );

    assert!(
        out.contains("+The workspace is a git repo."),
        "the added line is marked as added: {out}"
    );
    assert!(out.contains("@@ "), "hunks carry a git-style header: {out}");
    assert!(
        out.contains("--- turn 0 · step 0 · seq 0 (worker)"),
        "the baseline is the previous call OF THE SAME ROLE, not the \
         summarizer that ran alongside it: {out}"
    );
    assert!(
        out.contains("+++ turn 0 · step 1 · seq 0 (worker)"),
        "the target names itself: {out}"
    );
    // Every other system line is unchanged, so it must appear as context — with
    // a leading space, never a `+` or `-`.
    assert!(
        !out.contains("+You are Stella.") && !out.contains("-You are Stella."),
        "an unchanged line is context, not a change: {out}"
    );
    assert!(
        !out.contains("Condense this span"),
        "the summarizer's unrelated prompt never enters a worker diff: {out}"
    );
}

#[test]
fn the_first_call_is_diffed_against_the_prompt_the_user_actually_submitted() {
    // The pre-mutation baseline. The user typed one line; everything else in
    // the assembled context was added by Stella, and this is the only view that
    // says so.
    let (dir, id) = seeded_workspace();
    let out = inspect(&dir, &[&id.to_string(), "--step", "0", "--diff"]);

    assert!(
        out.contains("--- prompt as submitted"),
        "a role's first call has no predecessor, so the baseline is what was \
         typed rather than an error: {out}"
    );
    assert!(
        out.contains("+── system ──") && out.contains("+You are Stella."),
        "the whole system prompt reads as added — it is: {out}"
    );
    assert!(
        out.contains(" fix the failing test"),
        "the line the user actually wrote survives as context, so what they \
         recognise anchors the diff: {out}"
    );
}

#[test]
fn an_unchanged_prompt_is_reported_as_unchanged_not_as_an_empty_diff() {
    // Byte-stable prompts are the cache invariant, so "nothing moved" is a
    // result worth printing — silence would read as a broken command.
    let (dir, id) = seeded_workspace();
    let out = inspect(
        &dir,
        &[
            &id.to_string(),
            "--step",
            "1",
            "--diff",
            "--only",
            "user",
            "--format",
            "json",
        ],
    );
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    assert_eq!(parsed["changed"], false, "the user goal never moved: {out}");
    assert_eq!(parsed["added"], 0);
    assert_eq!(parsed["removed"], 0);
}

#[test]
fn diff_json_names_which_baseline_it_actually_resolved_to() {
    let (dir, id) = seeded_workspace();

    let first = inspect(
        &dir,
        &[&id.to_string(), "--step", "0", "--diff", "--format", "json"],
    );
    let parsed: serde_json::Value = serde_json::from_str(&first).expect("valid json");
    assert_eq!(
        parsed["base"], "prompt",
        "`--diff` asked for prev and got prompt; a script must be able to \
         tell which: {first}"
    );

    let later = inspect(
        &dir,
        &[
            &id.to_string(),
            "--step",
            "1",
            "--diff",
            "--only",
            "system",
            "--format",
            "json",
        ],
    );
    let parsed: serde_json::Value = serde_json::from_str(&later).expect("valid json");
    assert_eq!(parsed["base"], "prev");
    assert_eq!(parsed["added"], 1, "one line gained: {later}");
    assert_eq!(parsed["removed"], 0);
    assert_eq!(parsed["minimal"], true, "exact, not a wholesale replace");
}

#[test]
fn the_previous_turn_is_the_previous_execution_not_just_the_previous_step() {
    // The comparison the whole feature is for. A system prompt cannot drift
    // inside one execution — it is byte-stable there for the prompt cache — so
    // a diff that stopped at the execution boundary would report "no change"
    // for precisely the question worth asking.
    let (dir, first, second) = seeded_session();
    let out = inspect(
        &dir,
        &[
            &second.to_string(),
            "--step",
            "0",
            "--diff",
            "--only",
            "system",
        ],
    );

    assert!(
        out.contains(&format!(
            "--- execution {first} · turn 0 · step 0 · seq 0 (worker)"
        )),
        "the baseline crosses into the previous turn, and names the execution \
         it came from so the crossing is never silent: {out}"
    );
    assert!(
        out.contains("+Today is Tuesday."),
        "the line the previous turn did not have is the only added line: {out}"
    );
    assert!(
        !out.contains("+You are Stella."),
        "everything stable stays context: {out}"
    );
}

#[test]
fn first_reaches_back_to_the_first_turn_of_the_session() {
    let (dir, first, second) = seeded_session();
    let out = inspect(
        &dir,
        &[
            &second.to_string(),
            "--step",
            "0",
            "--diff",
            "first",
            "--only",
            "system",
            "--format",
            "json",
        ],
    );
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    assert_eq!(parsed["base"], "first");
    assert!(
        parsed["base_label"]
            .as_str()
            .expect("label")
            .contains(&format!("execution {first}")),
        "the first ever turn of the session, not of this execution: {out}"
    );
}

#[test]
fn diff_without_a_step_says_which_flag_is_missing() {
    let (dir, id) = seeded_workspace();
    let output = Command::new(env!("CARGO_BIN_EXE_stella"))
        .without_embedder_backend()
        .args(["inspect", &id.to_string(), "--diff"])
        .current_dir(dir.path())
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .output()
        .expect("run");
    assert!(!output.status.success(), "an ambiguous diff is an error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--step"),
        "names the missing flag: {stderr}"
    );
}

#[test]
fn inspect_names_a_missing_receipt_instead_of_printing_an_empty_transcript() {
    let (dir, id) = seeded_workspace();
    let output = Command::new(env!("CARGO_BIN_EXE_stella"))
        .without_embedder_backend()
        .args(["inspect", &id.to_string(), "--step", "99"])
        .current_dir(dir.path())
        .output()
        .expect("run");
    assert!(
        !output.status.success(),
        "an absent step is an error, not silence"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no receipt"),
        "says what was missing: {stderr}"
    );
}

/// A prompt shaped the way `assemble_system_prompt` shapes one: the base
/// persona, the session-environment block, workspace memories, workspace
/// rules, and the SessionStart hook context.
///
/// The headings here are a copy of the bytes the assembler emits, and
/// deliberately so: `stella-cli` ships no library, so an integration test
/// cannot reach the constants. The copy is safe because it is not the source
/// of truth for anything — `agent/prompt/provenance.rs`'s own tests pin the
/// marker table against the emitting constants, and the concatenation
/// assertion below holds whatever the split does. What this fixture has to be
/// is a *realistic* prompt, which it is.
const SYSTEM_SECTIONED: &str = "You are Stella.\
\n\n## Session environment\nWorkspace root: /w — a git repository\
\n\nWorkspace memories (lessons from previous sessions — apply them):\n\n### one\nlesson\n\
\n\n## Workspace rules (cite the ^handle of any you apply)\n\n### Must\n- rule ^r1\
\n\nSession context (from SessionStart hooks):\nhook output";

/// One execution with two worker calls: step 0 was sent a sectioned system
/// prompt, and step 1 was sent no `system_prefix` block at all — the receipt
/// shape that must say so rather than print an empty prompt.
fn seeded_provenance_workspace() -> (tempfile::TempDir, i64) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("store");
    let id = store
        .begin_execution("run", "explain the prompt", "anthropic", "opus")
        .expect("execution");

    store
        .record_context_block(id, &block("blk_prov", "system_prefix", SYSTEM_SECTIONED))
        .expect("sectioned system block");
    store
        .record_context_block(id, &block("blk_goal", "user_goal", "explain the prompt"))
        .expect("goal block");

    for (step, blocks) in [
        (0, vec![entry("blk_prov", 0), entry("blk_goal", 1)]),
        // No system block: the goal is the whole call.
        (1, vec![entry("blk_goal", 0)]),
    ] {
        store
            .record_step_manifest(
                id,
                &StepManifestRow {
                    turn_instance: 0,
                    step,
                    call_seq: 0,
                    provider: "anthropic".into(),
                    model: "opus".into(),
                    call_role: "worker".into(),
                    effective_budget_tokens: 136_363,
                    calibration_factor: 1.1,
                    estimated_input_tokens: 20,
                    stall_seconds_requested: None,
                    compiled_frame_id: None,
                    frame_hash: None,
                    blocks,
                },
            )
            .expect("manifest");
    }
    (dir, id)
}

/// The feature #4602 asks for: the terminal reader gets the same sectioned
/// provenance view the Observatory has — every span headed by what it is and
/// by the setting surface that produced it.
#[test]
fn system_prompt_sections_name_the_setting_behind_each_span() {
    let (dir, id) = seeded_provenance_workspace();
    let out = inspect(
        &dir,
        &[&id.to_string(), "--step", "0", "--system-prompt", "--full"],
    );

    for label in [
        "Base instructions",
        "Session environment",
        "Workspace memories",
        "Workspace rules",
        "SessionStart hook context",
    ] {
        assert!(
            out.contains(label),
            "section {label} is missing from: {out}"
        );
    }
    // A label alone would only rename the bytes. The setting surface is what
    // turns "this paragraph exists" into "this is the knob that put it here".
    for source in [
        ".stella/memories/*.md",
        ".stella/rules/*.toml",
        "hooks.SessionStart",
    ] {
        assert!(out.contains(source), "no attribution for {source}: {out}");
    }
    assert!(
        out.contains("Workspace root: /w"),
        "sections carry their bodies, not just their headings: {out}"
    );
}

/// The property the whole view rests on: however the boundaries fall, the
/// sections are the exact bytes the call was sent, in order. A reader who
/// distrusts a boundary has lost nothing by reading the sections.
#[test]
fn the_sections_concatenate_to_the_bytes_the_call_was_sent() {
    let (dir, id) = seeded_provenance_workspace();
    let out = inspect(
        &dir,
        &[
            &id.to_string(),
            "--step",
            "0",
            "--system-prompt",
            "--format",
            "json",
        ],
    );
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
    assert_eq!(parsed["found"], true, "{out}");
    let sections = parsed["sections"].as_array().expect("sections array");
    assert_eq!(
        sections
            .iter()
            .map(|s| s["label"].as_str().expect("label"))
            .collect::<Vec<_>>(),
        [
            "Base instructions",
            "Session environment",
            "Workspace memories",
            "Workspace rules",
            "SessionStart hook context",
        ],
        "{out}"
    );
    let rebuilt: String = sections
        .iter()
        .map(|s| s["body"].as_str().expect("body"))
        .collect();
    assert_eq!(
        rebuilt, SYSTEM_SECTIONED,
        "the sections must reproduce the prompt byte-for-byte"
    );
    assert_eq!(parsed["bytes"], SYSTEM_SECTIONED.len());
}

/// A call whose receipt holds no `system_prefix` block says so. Printing an
/// empty prompt would read as "the model was sent nothing", which is a claim
/// the receipt does not make.
#[test]
fn a_call_with_no_system_prefix_says_so_instead_of_printing_an_empty_prompt() {
    let (dir, id) = seeded_provenance_workspace();
    let out = inspect(&dir, &[&id.to_string(), "--step", "1", "--system-prompt"]);
    assert!(
        out.contains("no system_prefix block resolved"),
        "names the gap: {out}"
    );

    let json = inspect(
        &dir,
        &[
            &id.to_string(),
            "--step",
            "1",
            "--system-prompt",
            "--format",
            "json",
        ],
    );
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    assert_eq!(
        parsed["found"], false,
        "a script must tell an absent prompt from an empty one: {json}"
    );
    assert!(parsed["sections"].as_array().expect("array").is_empty());
}

/// `--system-prompt` and `--diff` are two whole-output views of one call.
/// Letting either win silently would answer a question the caller did not ask.
#[test]
fn system_prompt_and_diff_together_are_refused_rather_than_one_winning() {
    let (dir, id) = seeded_provenance_workspace();
    let output = Command::new(env!("CARGO_BIN_EXE_stella"))
        .without_embedder_backend()
        .args([
            "inspect",
            &id.to_string(),
            "--step",
            "0",
            "--system-prompt",
            "--diff",
        ])
        .current_dir(dir.path())
        .output()
        .expect("run");
    assert!(!output.status.success(), "the combination is refused");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--diff --only system"),
        "names the flag that answers the question they were reaching for: {stderr}"
    );
}
