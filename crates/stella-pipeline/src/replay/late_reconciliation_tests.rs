//! Late ground-truth reconciliation over a recorded stream — split out
//! of `replay.rs` to keep it under the file-size ratchet; a child
//! module, so the fold internals stay reachable via `super::*`.

use super::ground_truth::{GroundTruth, reconcile};
use super::*;
use stella_protocol::{CiStatus, PrStatus, VerdictEvidence};

fn verifier_pass() -> AgentEvent {
    AgentEvent::Verdict {
        passed: true,
        evidence: VerdictEvidence {
            summary: String::new(),
            deterministic: false,
            evidence_refs: vec![],
            ladder: None,
        },
    }
}

fn commit(sha: &str) -> AgentEvent {
    AgentEvent::Commit {
        sha: sha.into(),
        message: "feat: the work".into(),
    }
}

fn pr(url: &str, ci: Option<CiStatus>) -> AgentEvent {
    AgentEvent::Pr {
        url: url.into(),
        status: PrStatus::Open,
        number: Some(1),
        ci,
    }
}

/// #1293 acceptance: a pass recorded after its session's last CI
/// observation used to be lost. It now leaves the fold carrying the
/// commit it covers, and a revert discovered later settles it.
#[test]
fn a_pass_after_the_last_ci_verdict_survives_its_session_and_a_revert_settles_it() {
    let events = vec![
        verifier_pass(),
        pr("https://example.test/pr/1", Some(CiStatus::Passing)),
        // Everything below is after the session's last terminal verdict —
        // the region that used to be unreachable.
        verifier_pass(),
        commit("abc123abc123"),
    ];
    let (report, pending) = calibration_pending("sess-a", &events);
    assert_eq!(report.verifier_passes, 2);
    assert_eq!(
        report.verifier_reconciled, 1,
        "only the first pass had an in-stream verdict"
    );
    assert_eq!(pending.len(), 1, "the trailing pass is carried out");
    assert_eq!(pending[0].session, "sess-a");
    assert_eq!(pending[0].commits, vec!["abc123abc123".to_string()]);

    let mut report = report;
    let truth = GroundTruth::default().with_reverts(["abc123abc123".to_string()]);
    assert_eq!(reconcile(&mut report, &pending, &truth), 1);
    assert_eq!(report.verifier_reconciled, 2);
    assert_eq!(report.verifier_false_positives, 1);
    assert_eq!(report.verifier_reverted, 1);
    assert!(
        render_calibration(&report).contains("settled by a REVERT"),
        "the render must distinguish a human's revert from a red CI run: {}",
        render_calibration(&report)
    );
}

/// The cross-session half: session A's trailing pass is settled by a
/// terminal CI verdict recorded in session B, for the PR they share.
#[test]
fn a_terminal_verdict_in_a_later_session_settles_an_earlier_ones_pass() {
    let session_a = vec![verifier_pass(), pr("https://example.test/pr/4", None)];
    let session_b = vec![pr("https://example.test/pr/4", Some(CiStatus::Failing))];
    let (mut report, pending) = calibration_pending("sess-a", &session_a);
    assert_eq!(
        report.verifier_reconciled, 0,
        "a PR with no terminal verdict reconciles nothing in its own stream"
    );
    let truth = GroundTruth::default()
        .with_stream(&session_a)
        .with_stream(&session_b);
    assert_eq!(reconcile(&mut report, &pending, &truth), 1);
    assert_eq!(report.verifier_false_positives, 1);
    assert_eq!(
        report.verifier_reverted, 0,
        "CI is not a revert, and the two must not merge into one number"
    );
}

/// The unchanged contract: `calibration` still answers exactly what it
/// answered before, so a caller that only wants the in-stream reading is
/// untouched by any of this.
#[test]
fn the_in_stream_fold_is_unchanged() {
    let events = vec![
        verifier_pass(),
        pr("https://example.test/pr/2", Some(CiStatus::Failing)),
    ];
    let report = calibration(&events);
    assert_eq!(report.verifier_passes, 1);
    assert_eq!(report.verifier_reconciled, 1);
    assert_eq!(report.verifier_false_positives, 1);
    assert_eq!(report.verifier_reverted, 0);
}
