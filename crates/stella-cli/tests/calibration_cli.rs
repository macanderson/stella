//! End-to-end witness for `stella calibration` (#3866): drive the real binary
//! against a real store and a real git history, and check that the measured
//! false-positive rates come back out.
//!
//! This is the acceptance for reinstating the command. Its measurement model
//! lived in `stella_pipeline::replay` and went out of the tree with that crate
//! (#3865), leaving `run_calibration` a stub that returned an error naming the
//! issue — so on `main` before this change, every assertion below fails at the
//! `status.success()` check, whatever the fixture says.
//!
//! Two sources are exercised together because only the whole command joins
//! them: the terminal CI verdicts recorded in the event journal (#871), and the
//! reverts read out of `git log` (#1293). The unit tests in
//! `stella-cli/src/calibration/` cover the fold itself; what is witnessed here
//! is the wiring — the store enumeration, the git spawn, and the JSON shape a
//! script parses.

use std::process::Command;

use stella_protocol::{
    AgentEvent, CiStatus, FlipOutcome, LadderSnapshot, PrStatus, VerdictEvidence,
};
use stella_store::Store;

/// A snapshot that observed no corroboration at all: no achieved flip, no green
/// touched test, no `verify_done` confirmation. `verifier_independent` is
/// recorded so the grader partition (#1865) has a fact to sort on.
fn uncorroborated_snapshot() -> LadderSnapshot {
    LadderSnapshot {
        rung: None,
        tracked_command: None,
        oracle_trace: vec![],
        flip: FlipOutcome::NotAchieved,
        unstable_flip: false,
        flip_refused_different_failure: false,
        touched_tests_passed: None,
        test_infra: None,
        diff_lines: 4,
        diff_budget: 400,
        diff_available: true,
        mutating_actions: 2,
        new_diag_errors: 0,
        new_diag_warnings: 0,
        witness_intact: None,
        witness_mutation: None,
        diff_coverage: None,
        verify_done_flip: false,
        no_test_surface: false,
        errored_commands: 0,
        verifier_independent: Some(false),
    }
}

/// A git repository holding one commit and a revert of it, returning the SHA
/// the revert names — the ground truth #1293 added, in the only form the
/// command reads it: git's own generated `This reverts commit <sha>.` body.
fn repo_with_a_reverted_commit(dir: &std::path::Path) -> String {
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.test")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.test")
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    git(&["init", "--quiet", "--initial-branch", "main"]);
    // A signing key configured globally would make every commit below fail on a
    // developer's machine and pass in CI; pin the decision instead of
    // inheriting it, the same discipline the colour tests keep.
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(dir.join("widget.txt"), "the work\n").expect("write");
    git(&["add", "widget.txt"]);
    git(&["commit", "--quiet", "-m", "feat: add the widget"]);
    let sha = git(&["rev-parse", "HEAD"]);
    git(&["revert", "--no-edit", "--no-gpg-sign", &sha]);
    sha
}

/// A workspace whose store holds one session: an unproven pass a failing CI run
/// settled in-stream, and a deterministic pass settled only by the revert in
/// the git history — the late path, which no single session could reach.
fn seeded_workspace() -> (tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("home");
    let reverted = repo_with_a_reverted_commit(dir.path());

    let events = [
        // An unproven pass, carrying the snapshot that says nothing
        // corroborated it.
        AgentEvent::Verdict {
            passed: true,
            evidence: VerdictEvidence {
                summary: "UNVERIFIED: nothing deterministic settled this turn".into(),
                deterministic: false,
                evidence_refs: vec![],
                ladder: Some(Box::new(uncorroborated_snapshot())),
            },
        },
        // …which this terminal CI verdict reconciles, in-stream, as wrong.
        AgentEvent::Pr {
            url: "https://example.test/pr/1".into(),
            status: PrStatus::Merged,
            number: Some(1),
            ci: Some(CiStatus::Failing),
        },
        // A deterministic pass with no CI observation after it: unreconciled
        // when its session ends, and reachable only by the git history.
        AgentEvent::Verdict {
            passed: true,
            evidence: VerdictEvidence {
                summary: "flip achieved".into(),
                deterministic: true,
                evidence_refs: vec![],
                ladder: None,
            },
        },
        AgentEvent::Commit {
            sha: reverted,
            message: "feat: add the widget".into(),
        },
    ];

    let store = Store::open(dir.path()).expect("store");
    let execution = store
        .begin_execution("run", "add the widget", "anthropic", "opus")
        .expect("execution");
    store
        .set_execution_session(execution, "sess-a")
        .expect("session");
    for (seq, event) in events.iter().enumerate() {
        store
            .record_event(execution, seq as u64, event)
            .expect("record event");
    }
    (dir, home)
}

fn calibration(dir: &tempfile::TempDir, home: &tempfile::TempDir, args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_stella"))
        .arg("calibration")
        .args(args)
        .current_dir(dir.path())
        // The whole user tier, not just the data dir: `credentials.toml` is
        // config-tier, so a narrower override still lets a test read the
        // developer's real home (#3876).
        .env("STELLA_HOME", home.path())
        .env_remove("CLICOLOR_FORCE")
        .env("NO_COLOR", "1")
        .output()
        .expect("run stella calibration");
    assert!(
        output.status.success(),
        "stella calibration {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The #3866 acceptance: `--format json` reports real tallies again, joining
/// the in-stream CI verdict (#871) and the git revert (#1293) that no session
/// could have settled by itself.
#[test]
fn calibration_json_reports_both_ground_truth_sources() {
    let (dir, home) = seeded_workspace();
    let report: serde_json::Value =
        serde_json::from_str(&calibration(&dir, &home, &["--format", "json"])).expect("json");

    assert_eq!(report["sessions"], 1);
    assert_eq!(
        report["verdicts_observed"], 2,
        "both verdicts are observed, whichever cohort they land in"
    );

    // #871: the in-stream half.
    assert_eq!(report["unproven_passes"], 1);
    assert_eq!(report["unproven_reconciled"], 1);
    assert_eq!(report["unproven_false_positives"], 1);
    assert_eq!(report["unproven_false_positive_rate"], 1.0);

    // #1293: the late half — the deterministic pass was settled by a revert
    // read out of the git log, after its own session had ended.
    assert_eq!(report["settled_by_late_evidence"], 1);
    assert_eq!(report["unreconciled_passes"], 0);
    assert_eq!(report["deterministic_passes"], 1);
    assert_eq!(report["deterministic_reconciled"], 1);
    assert_eq!(report["deterministic_false_positives"], 1);
    assert_eq!(
        report["deterministic_reverted"], 1,
        "a revert is counted apart from CI: it is a human's later and stronger statement"
    );
    assert_eq!(report["unproven_reverted"], 0);

    // #1295: the uncorroborated cohort, measured off the recorded snapshot.
    assert_eq!(report["snapshotted_verdicts"], 1);
    assert_eq!(report["uncorroborated_verdicts"], 1);
    assert_eq!(report["uncorroborated_rate"], 1.0);
    assert_eq!(report["unproven_passes_standing_alone"], 1);

    // #1865: the grader partition, which the human report has shown since it
    // landed and the JSON did not until now.
    assert_eq!(report["by_grader"]["self_graded"]["passes"], 1);
    assert_eq!(
        report["by_grader"]["self_graded"]["false_positive_rate"],
        1.0
    );
    assert_eq!(
        report["by_grader"]["independent"]["false_positive_rate"],
        serde_json::Value::Null,
        "an unmeasured rate is null, never zero — the one direction this instrument must not drift"
    );
}

/// The human report states the same measurement, with every rate beside the
/// denominator it was computed over.
#[test]
fn calibration_text_names_the_cohorts_and_the_revert() {
    let (dir, home) = seeded_workspace();
    let rendered = calibration(&dir, &home, &[]);
    assert!(
        rendered.contains("1 session(s) with recorded events"),
        "the session count leads: {rendered}"
    );
    assert!(
        rendered.contains("unproven"),
        "the unproven cohort is named: {rendered}"
    );
    assert!(
        rendered.contains("settled by a REVERT"),
        "a revert is distinguished from a red CI run: {rendered}"
    );
    assert!(
        rendered.contains("late evidence settled 1 verdict(s)"),
        "the late path reports its own reach: {rendered}"
    );
}

/// #3866's second half: a workspace that recorded no verdict at all must say
/// so, not print a page of zeroes that reads as a flawless verifier.
///
/// This is now the ordinary state — nothing in the workspace emits a `Verdict`
/// since the built-in verification path was deleted (#3865), so it is the most
/// likely thing anyone running the command sees.
#[test]
fn a_workspace_with_no_verdicts_says_so_instead_of_reporting_zeroes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("home");
    let store = Store::open(dir.path()).expect("store");
    let execution = store
        .begin_execution("run", "do the thing", "anthropic", "opus")
        .expect("execution");
    store
        .set_execution_session(execution, "sess-quiet")
        .expect("session");
    store
        .record_event(
            execution,
            0,
            &AgentEvent::Commit {
                sha: "abc123abc123abc123".into(),
                message: "feat: the work".into(),
            },
        )
        .expect("record event");

    let rendered = calibration(&dir, &home, &[]);
    assert!(
        rendered.contains("nothing to calibrate"),
        "an empty record names itself: {rendered}"
    );
    assert!(
        !rendered.contains("false-positive rate"),
        "and prints no rate over nothing: {rendered}"
    );

    let report: serde_json::Value =
        serde_json::from_str(&calibration(&dir, &home, &["--format", "json"])).expect("json");
    assert_eq!(report["verdicts_observed"], 0);
    assert_eq!(
        report["unproven_false_positive_rate"],
        serde_json::Value::Null,
        "unmeasured, not zero"
    );
}
