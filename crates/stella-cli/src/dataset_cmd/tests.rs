//! Unit cover for the dataset fold: the acceptance predicate, the journal
//! pass, the truncation inference, and the post-assembly redaction.
//!
//! The end-to-end witness — the real binary against a seeded workspace, both
//! output files, byte-identity — lives in `tests/dataset_export_cli.rs`.

use super::*;
use stella_protocol::{FileChangeKind, VerdictEvidence, ModelCallRole, ToolCall, ToolOutput};
use stella_store::SessionEventRecord;

fn journal(events: Vec<AgentEvent>) -> Vec<SessionEventRecord> {
    events
        .into_iter()
        .enumerate()
        .map(|(seq, event)| SessionEventRecord {
            execution_id: 1,
            seq: seq as i64,
            event,
        })
        .collect()
}

fn edit_events() -> Vec<AgentEvent> {
    vec![
        AgentEvent::StepManifest {
            turn_instance: 2,
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
                input: serde_json::json!({"path": "src/lib.rs"}),
            },
        },
        AgentEvent::ToolResult {
            call_id: "c1".into(),
            output: ToolOutput::Ok {
                content: "edited".into(),
            },
            duration_ms: 7,
            speculated: false,
        },
        AgentEvent::FileChange {
            path: "src/lib.rs".into(),
            kind: FileChangeKind::Modified,
            added: 1,
            removed: 1,
            diff: Some("@@ -1 +1 @@\n-old\n+new\n".into()),
        },
    ]
}

/// The trajectory shape #872 asks for comes back out of one ordered pass:
/// tool arguments AND outputs paired by `call_id`, mutating changes only, the
/// last verdict, and the turn instance the change landed in.
#[test]
fn one_pass_over_the_journal_recovers_the_whole_trajectory() {
    let mut events = edit_events();
    // A read is telemetry, not a change — it must not reach `changes`.
    events.push(AgentEvent::FileChange {
        path: "README.md".into(),
        kind: FileChangeKind::Read,
        added: 0,
        removed: 0,
        diff: None,
    });
    events.push(AgentEvent::Verdict {
        passed: true,
        evidence: VerdictEvidence {
            summary: "cargo test -p stella-cli flipped fail -> pass".into(),
            deterministic: true,
            evidence_refs: Vec::new(),
            ladder: None,
        },
    });

    let fold = fold_journal(&journal(events));

    assert_eq!(
        fold.turn_instance, 2,
        "the manifest's turn instance is kept"
    );
    assert_eq!(fold.tool_calls.len(), 1);
    let call = &fold.tool_calls[0];
    assert_eq!(call.name, "edit");
    assert_eq!(call.arguments["path"], "src/lib.rs");
    assert_eq!(call.output.as_deref(), Some("edited"));
    assert_eq!(call.is_error, Some(false));
    assert_eq!(call.duration_ms, Some(7));
    assert_eq!(call.turn_instance, 2, "calls carry their enclosing turn");

    assert_eq!(
        fold.changes
            .iter()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>(),
        vec!["src/lib.rs"],
        "a read is not a change"
    );
    assert_eq!(fold.changes[0].kind, "modified");
    let verdict = fold.verdict.as_ref().expect("verdict folded");
    assert!(verdict.passed && verdict.deterministic);
}

/// A tool that never returned still appears — with its output honestly
/// absent, rather than an invented empty string.
#[test]
fn an_unclosed_tool_call_reports_its_output_as_unknown() {
    let fold = fold_journal(&journal(vec![AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".into(),
            name: "bash".into(),
            input: serde_json::json!({"command": "sleep 999"}),
        },
    }]));
    assert_eq!(fold.tool_calls.len(), 1);
    assert_eq!(fold.tool_calls[0].output, None);
    assert_eq!(fold.tool_calls[0].is_error, None);
}

/// Success alone is not acceptance: the turn has to have changed something,
/// and the failure outcomes never qualify however much they touched.
#[test]
fn acceptance_needs_a_success_outcome_and_a_real_change() {
    let with_change = fold_journal(&journal(edit_events()));
    let read_only = fold_journal(&journal(vec![AgentEvent::FileChange {
        path: "README.md".into(),
        kind: FileChangeKind::Read,
        added: 0,
        removed: 0,
        diff: None,
    }]));

    assert!(is_accepted("completed", &with_change, false));
    assert!(is_accepted("goal_met", &with_change, false));
    assert!(
        !is_accepted("completed", &read_only, false),
        "a read-only success is not a trajectory to distil from"
    );
    for outcome in [
        "goal_unmet",
        "verification_failed",
        "aborted",
        "cancelled",
        "failed",
        "error",
        "interrupted",
    ] {
        assert!(
            !is_accepted(outcome, &with_change, false),
            "{outcome} must never be accepted"
        );
    }
}

/// `--require-verdict` tightens to the judged subset, and says so in the
/// predicate string the manifest carries.
#[test]
fn require_verdict_demands_a_passing_judgement() {
    let unjudged = fold_journal(&journal(edit_events()));
    assert!(is_accepted("completed", &unjudged, false));
    assert!(
        !is_accepted("completed", &unjudged, true),
        "no verdict cannot satisfy --require-verdict"
    );

    let mut events = edit_events();
    events.push(AgentEvent::Verdict {
        passed: false,
        evidence: VerdictEvidence {
            summary: "the tests still fail".into(),
            deterministic: false,
            evidence_refs: Vec::new(),
            ladder: None,
        },
    });
    let failed = fold_journal(&journal(events));
    assert!(!is_accepted("completed", &failed, true));

    assert!(!acceptance_predicate(false).contains("verdict"));
    assert!(acceptance_predicate(true).contains("verdict"));
}

/// The half-open window, on the timestamp form SQLite actually writes.
#[test]
fn the_date_window_is_half_open_and_accepts_a_bare_date() {
    let ts = "2026-08-02 14:30:00";
    assert!(in_range(ts, None, None));
    assert!(in_range(ts, Some("2026-08-02"), Some("2026-08-03")));
    assert!(!in_range(ts, Some("2026-08-03"), None));
    assert!(
        !in_range(ts, None, Some("2026-08-02")),
        "--until is exclusive, so a bare date excludes that whole day"
    );
}

/// Truncation is inferred by comparison against the recorder's authoritative
/// counts, and only ever claimed in the direction the evidence supports.
#[test]
fn a_bounded_hunk_is_flagged_only_when_it_provably_lost_lines() {
    let complete = "@@ -1 +1 @@\n-old\n+new\n";
    assert!(!hunk_is_truncated(Some(complete), 1, 1));
    assert!(
        hunk_is_truncated(Some(complete), 300, 1),
        "one rendered + line cannot represent 300 added lines"
    );
    assert!(!hunk_is_truncated(None, 9, 9), "no hunk, no claim");
    // A region diff can render more lines than it changed; that is not
    // truncation and must not be reported as its opposite either.
    assert!(!hunk_is_truncated(Some(complete), 0, 0));
}

/// The redaction criterion, at the unit level: a planted key in the prompt
/// AND one in a tool argument are both gone from the serialized record, the
/// `redacted` flag says so, and the field order is the struct's own.
#[test]
fn redaction_runs_after_assembly_over_every_string_leaf() {
    let record = DatasetRecord {
        schema: DATASET_SCHEMA_VERSION,
        execution_id: 1,
        session_id: Some("ses-fixture".into()),
        repo: "abc123".into(),
        timestamp: "2026-08-02 14:30:00".into(),
        turn_instance: 0,
        kind: "run".into(),
        provider: "zai".into(),
        model: "glm-5.2".into(),
        outcome: "completed".into(),
        cost_usd: 0.01,
        prompt: "rotate ghp_0123456789abcdef0123456789abcdef012345".into(),
        tool_calls: vec![DatasetToolCall {
            seq: 1,
            turn_instance: 0,
            name: "edit".into(),
            call_id: "c1".into(),
            arguments: serde_json::json!({"new_string": "AKIAIOSFODNN7EXAMPLE"}),
            output: Some("wrote sk-ant-api03-abcdefghijklmnopqrstuvwxyz012345".into()),
            is_error: Some(false),
            duration_ms: Some(7),
        }],
        changes: Vec::new(),
        verdict: None,
        redacted: false,
    };

    let redacted = redact_after_assembly(&record).expect("redact");
    assert!(redacted.redacted, "the flag reports what actually changed");
    let line = serde_json::to_string(&redacted).expect("serialize");
    for secret in [
        "ghp_0123456789abcdef",
        "AKIAIOSFODNN7EXAMPLE",
        "sk-ant-api03-",
    ] {
        assert!(
            !line.contains(secret),
            "{secret} survived redaction: {line}"
        );
    }
    assert!(line.contains(PLACEHOLDER));
    // The session id is provenance, not a secret — redacting it would make
    // the dataset untraceable, which is the opposite of what #872 wants.
    assert_eq!(redacted.session_id.as_deref(), Some("ses-fixture"));
    // Serializing the STRUCT (not the intermediate Value) pins the key order
    // to the declaration order, whatever `serde_json::Map` happens to be.
    assert!(
        line.starts_with(r#"{"schema":1,"execution_id":1,"session_id":"ses-fixture""#),
        "declaration order is the wire order: {line}"
    );
}

/// A record with nothing to redact keeps its flag false — "we scanned it and
/// found nothing" must stay distinguishable from "we replaced something".
#[test]
fn a_clean_record_is_not_reported_as_redacted() {
    let record = DatasetRecord {
        schema: DATASET_SCHEMA_VERSION,
        execution_id: 1,
        session_id: None,
        repo: "abc123".into(),
        timestamp: "2026-08-02 14:30:00".into(),
        turn_instance: 0,
        kind: "run".into(),
        provider: "zai".into(),
        model: "glm-5.2".into(),
        outcome: "completed".into(),
        cost_usd: 0.0,
        prompt: "fix the failing test".into(),
        tool_calls: Vec::new(),
        changes: Vec::new(),
        verdict: None,
        redacted: false,
    };
    assert!(!redact_after_assembly(&record).expect("redact").redacted);
}

/// The manifest's redactor stamp is stable across calls (so the manifest is
/// byte-stable) and is a real digest, not a placeholder.
#[test]
fn the_redaction_digest_is_stable_and_sized() {
    let digest = redaction_behavior_digest();
    assert_eq!(digest, redaction_behavior_digest());
    assert_eq!(digest.len(), 16);
    assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
}

/// Every probe the digest is taken over does what it claims: the positives
/// are redacted, the negatives are left alone. A probe corpus nobody checks
/// would let the digest freeze around a redactor that stopped working.
#[test]
fn the_probe_corpus_exercises_both_directions() {
    let (positives, negatives) = REDACTION_PROBES.split_at(REDACTION_PROBES.len() - 3);
    for probe in positives {
        assert!(
            redact_secrets(probe).redacted,
            "probe should be redacted: {probe}"
        );
    }
    for probe in negatives {
        assert!(
            !redact_secrets(probe).redacted,
            "probe must NOT be redacted: {probe}"
        );
    }
}
