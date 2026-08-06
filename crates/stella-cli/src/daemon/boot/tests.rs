// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Witness tests for #1627: what a boot-time sweep continues, what it refuses
//! to touch, and what bounds it.
//!
//! A true boot cycle is not testable in-process — it needs a reboot, a service
//! manager and a real killed turn — so these drive the two halves that decide
//! everything a boot does: the pure selection rule ([`decide`] / [`plan`]) over
//! hand-written registry rows, and the durable attempt ledger. What they
//! cannot cover is stated in the PR: the `launchctl`/`systemctl` load itself,
//! and the resumed child's own continuation, which is #1586's contract and is
//! witnessed there.

use super::*;

/// A supervised run the kernel took mid-turn: its status was never replaced,
/// its lock is gone, and it left a resume point.
fn killed_mid_turn() -> BootCandidate {
    BootCandidate {
        id: "ses-1754431200000-84213".to_string(),
        title: "proj: finish the migration".to_string(),
        supervised: true,
        stored_status: SessionStatus::InProgress,
        lock_held: false,
        has_resume_point: true,
        workspace_exists: true,
        attempts: 0,
    }
}

#[test]
fn a_run_killed_mid_turn_is_continued_at_boot() {
    assert_eq!(decide(&killed_mid_turn()), BootDecision::Continue);
}

#[test]
fn a_run_the_operator_ended_is_never_continued_at_boot() {
    // Every terminal status, including the two #1653 cannot tell apart. The
    // whole point of the rule is that the ambiguity does not matter here:
    // `Error` is skipped whether it means "crashed after saying so" or
    // "stopped by policy", and the operator's deliberate stop is safe either
    // way.
    for status in [
        SessionStatus::Cancelled,
        SessionStatus::Complete,
        SessionStatus::Paused,
        SessionStatus::Archived,
        SessionStatus::Error,
    ] {
        let candidate = BootCandidate {
            stored_status: status,
            ..killed_mid_turn()
        };
        assert_eq!(
            decide(&candidate),
            BootDecision::Skip(SkipReason::EndedDeliberately),
            "a boot must not resume a run recorded as {status:?} — even holding a resume point"
        );
    }
}

#[test]
fn a_run_that_survived_the_boot_is_not_started_a_second_time() {
    let candidate = BootCandidate {
        lock_held: true,
        ..killed_mid_turn()
    };
    assert_eq!(
        decide(&candidate),
        BootDecision::Skip(SkipReason::StillRunning)
    );
}

#[test]
fn an_unsupervised_or_workspaceless_or_pointless_run_is_skipped_with_its_own_reason() {
    let cases = [
        (
            BootCandidate {
                supervised: false,
                ..killed_mid_turn()
            },
            SkipReason::NotSupervised,
        ),
        (
            BootCandidate {
                has_resume_point: false,
                ..killed_mid_turn()
            },
            SkipReason::NoResumePoint,
        ),
        (
            BootCandidate {
                workspace_exists: false,
                ..killed_mid_turn()
            },
            SkipReason::WorkspaceGone,
        ),
    ];
    for (candidate, reason) in cases {
        assert_eq!(decide(&candidate), BootDecision::Skip(reason));
    }
}

/// The property that makes "continue, never restart" structural rather than a
/// promise in a doc comment: a decision to act is only ever reachable with a
/// resume point in hand, so there is no input for which this module starts a
/// run from the top.
#[test]
fn nothing_is_ever_continued_without_a_resume_point() {
    for supervised in [true, false] {
        for lock_held in [true, false] {
            for has_resume_point in [true, false] {
                for workspace_exists in [true, false] {
                    for status in SessionStatus::ALL {
                        let candidate = BootCandidate {
                            supervised,
                            lock_held,
                            has_resume_point,
                            workspace_exists,
                            stored_status: status,
                            ..killed_mid_turn()
                        };
                        if decide(&candidate) == BootDecision::Continue {
                            assert!(
                                has_resume_point,
                                "a boot-time action without a resume point would be a restart, \
                                 not a resume: {candidate:?}"
                            );
                            assert!(status.is_live() && !lock_held, "{candidate:?}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn the_boot_loop_is_bounded_and_the_bound_is_the_ledger() {
    let mut ledger = AttemptLedger::default();
    let id = &killed_mid_turn().id;
    for expected in 1..=MAX_BOOT_ATTEMPTS {
        // A run still eligible before the attempt is counted...
        let candidate = BootCandidate {
            attempts: ledger.attempts(id),
            ..killed_mid_turn()
        };
        assert_eq!(decide(&candidate), BootDecision::Continue);
        assert_eq!(ledger.record_attempt(id), expected);
    }
    // ...and retired once the bound is reached, however many more boots come.
    let candidate = BootCandidate {
        attempts: ledger.attempts(id),
        ..killed_mid_turn()
    };
    assert_eq!(
        decide(&candidate),
        BootDecision::Skip(SkipReason::AttemptsExhausted),
        "a run that wedges the machine on resume must stop resuming, or every boot spends again"
    );
}

#[test]
fn the_ledger_round_trips_and_forgets_runs_the_registry_has_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(LEDGER_FILE);

    let mut ledger = AttemptLedger::default();
    ledger.record_attempt("ses-kept");
    ledger.record_attempt("ses-kept");
    ledger.record_attempt("ses-pruned");
    ledger.store(&path).unwrap();

    let mut reloaded = AttemptLedger::load(&path);
    assert_eq!(reloaded, ledger, "the bound must survive the reboot it bounds");
    assert_eq!(reloaded.attempts("ses-kept"), 2);

    reloaded.retain_known(&["ses-kept".to_string()]);
    assert_eq!(reloaded.attempts("ses-pruned"), 0);
    assert_eq!(reloaded.attempts("ses-kept"), 2);
}

#[test]
fn a_missing_or_corrupt_ledger_reads_as_empty_rather_than_wedging_the_sweep() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join(LEDGER_FILE);
    assert_eq!(AttemptLedger::load(&missing), AttemptLedger::default());

    let corrupt = dir.path().join("corrupt.json");
    std::fs::write(&corrupt, b"{not json").unwrap();
    assert_eq!(AttemptLedger::load(&corrupt), AttemptLedger::default());
}

/// The other half of "continued, not restarted": what the service manager is
/// actually handed at boot. `--resume-all` registers the sweep verb and
/// nothing else — no prompt, no `run`, nothing that would begin fresh work.
#[test]
fn the_installed_boot_service_registers_the_resume_sweep_and_no_fresh_work() {
    assert_eq!(registered_argv(), vec!["daemon", "resume-all"]);
    assert!(
        !registered_argv().iter().any(|arg| arg == "run"),
        "a boot service that runs `stella run` would restart work, not continue it"
    );
    assert_eq!(BOOT_LABEL, "resume-boot");
}

/// `plan` reports on every row, so no run is silently absent from the console
/// an operator reads after a boot.
#[test]
fn every_candidate_appears_in_the_plan_exactly_once_and_in_order() {
    let candidates = vec![
        killed_mid_turn(),
        BootCandidate {
            id: "ses-stopped".to_string(),
            stored_status: SessionStatus::Cancelled,
            ..killed_mid_turn()
        },
        BootCandidate {
            id: "ses-plain".to_string(),
            supervised: false,
            ..killed_mid_turn()
        },
    ];
    let planned = plan(&candidates);
    assert_eq!(planned.len(), candidates.len());
    for (i, (candidate, _)) in planned.iter().enumerate() {
        assert_eq!(candidate.id, candidates[i].id);
    }
    assert_eq!(planned[0].1, BootDecision::Continue);
    assert!(matches!(planned[1].1, BootDecision::Skip(_)));
    assert!(matches!(planned[2].1, BootDecision::Skip(_)));
}

/// Every skip reason is something an operator can act on — an empty or
/// duplicated explanation would be a row in the boot console that says
/// nothing.
#[test]
fn every_skip_reason_explains_itself_distinctly() {
    let reasons = [
        SkipReason::NotSupervised,
        SkipReason::StillRunning,
        SkipReason::EndedDeliberately,
        SkipReason::NoResumePoint,
        SkipReason::WorkspaceGone,
        SkipReason::AttemptsExhausted,
    ];
    let mut seen: Vec<String> = Vec::new();
    for reason in reasons {
        let text = reason.explain();
        assert!(!text.trim().is_empty(), "{reason:?}");
        assert!(!seen.contains(&text), "duplicate explanation: {text}");
        seen.push(text);
    }
}
