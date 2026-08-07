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
        parked: false,
        attempts: 0,
    }
}

#[test]
fn a_run_killed_mid_turn_is_continued_at_boot() {
    assert_eq!(decide(&killed_mid_turn()), BootDecision::Continue);
}

#[test]
fn a_run_the_operator_ended_is_never_continued_at_boot() {
    // Every status that means "this run ended rather than broke". Since
    // #1653 a deliberate policy stop records `Stopped` instead of hiding
    // among the crashes as `Error`, which is what lets `Error` itself be
    // continued (#1696, below).
    for status in [
        SessionStatus::Cancelled,
        SessionStatus::Stopped,
        SessionStatus::Complete,
        SessionStatus::Paused,
        SessionStatus::Archived,
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

/// The #1696 witness: a crash that lived long enough to record `Error` is
/// continued, where every terminal status used to be skipped wholesale.
///
/// That blanket skip was the price of #1653's ambiguity — a policy stop and a
/// crash both stored `Error`, so continuing one risked restarting work the
/// operator ended on purpose. With #1653 landed, `Error` means only "it fell
/// over", and stranding those was the honest cost this pays back.
#[test]
fn a_crash_that_recorded_itself_is_continued_but_a_policy_stop_is_not() {
    let crashed = BootCandidate {
        stored_status: SessionStatus::Error,
        ..killed_mid_turn()
    };
    assert_eq!(
        decide(&crashed),
        BootDecision::Continue,
        "an Error holding a resume point is a crash, and a crash is what this sweep continues"
    );

    // The same row without a resume point — which is what a pre-#1653 build's
    // policy stop actually looks like, because every deliberate ending
    // retracts its checkpoint on the way out — is still skipped, and says the
    // most specific true thing about itself.
    let stopped_by_policy = BootCandidate {
        stored_status: SessionStatus::Error,
        has_resume_point: false,
        ..killed_mid_turn()
    };
    assert_eq!(
        decide(&stopped_by_policy),
        BootDecision::Skip(SkipReason::NoResumePoint)
    );

    // And the status a policy stop records today is skipped outright.
    let stopped = BootCandidate {
        stored_status: SessionStatus::Stopped,
        ..killed_mid_turn()
    };
    assert_eq!(
        decide(&stopped),
        BootDecision::Skip(SkipReason::EndedDeliberately)
    );
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
                            assert!(!lock_held, "{candidate:?}");
                            // Interrupted, or crashed: since #1653 an `Error`
                            // means only "it fell over", so it joins the live
                            // statuses as continuable (#1696); every other
                            // terminal status is a run that ended on purpose.
                            assert!(
                                status.is_live() || status == SessionStatus::Error,
                                "{candidate:?}"
                            );
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
    assert_eq!(
        reloaded, ledger,
        "the bound must survive the reboot it bounds"
    );
    assert_eq!(reloaded.attempts("ses-kept"), 2);

    reloaded.retain_known(&["ses-kept".to_string()]);
    assert_eq!(reloaded.attempts("ses-pruned"), 0);
    assert_eq!(reloaded.attempts("ses-kept"), 2);
}

/// The bound counts one interruption episode, not a session's whole life: a
/// run that ends gives its attempts back, so a long-lived session cannot run
/// out of resumes it never spent.
#[test]
fn a_run_that_reaches_a_terminal_status_gives_its_attempts_back() {
    let mut ledger = AttemptLedger::default();
    let id = &killed_mid_turn().id;
    for _ in 0..MAX_BOOT_ATTEMPTS {
        ledger.record_attempt(id);
    }
    assert_eq!(
        decide(&BootCandidate {
            attempts: ledger.attempts(id),
            ..killed_mid_turn()
        }),
        BootDecision::Skip(SkipReason::AttemptsExhausted)
    );

    // The sweep calls this on exactly the arm that observed the run end.
    ledger.forget(id);
    assert_eq!(ledger.attempts(id), 0);
    assert_eq!(
        decide(&BootCandidate {
            attempts: ledger.attempts(id),
            ..killed_mid_turn()
        }),
        BootDecision::Continue,
        "a fresh interruption after a clean ending starts from zero"
    );
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

/// #1921: the console line for a run the per-run ceiling stopped — the
/// reporting half of the contract, witnessed pure the way
/// [`SkipReason::explain`] is. #1627 holds that a run silently resumed at
/// boot is as bad as one silently lost; a run silently *stopped* at boot
/// would be worse than either.
#[test]
fn a_ceiling_stop_names_the_ceiling_the_safe_stop_and_the_next_step() {
    let line = ceiling_report(std::time::Duration::from_secs(30 * 60));
    assert!(line.contains("30-minute ceiling"), "{line}");
    assert!(
        line.contains("safe boundary"),
        "the line must say the stop was the graceful one: {line}"
    );
    assert!(
        line.contains("the sweep continues"),
        "the whole point of the ceiling is the runs behind this one: {line}"
    );
    assert!(
        line.contains("stella daemon list"),
        "an operator must be handed the command that answers what happened: {line}"
    );
    // The console speaks the flag's unit for whole minutes and falls back to
    // seconds — or milliseconds — so a test ceiling is never rounded into a
    // fiction like "0-second".
    assert_eq!(
        describe_ceiling(std::time::Duration::from_secs(90)),
        "90-second"
    );
    assert_eq!(
        describe_ceiling(std::time::Duration::from_secs(120)),
        "2-minute"
    );
    assert_eq!(
        describe_ceiling(std::time::Duration::from_millis(250)),
        "250-millisecond"
    );
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
        SkipReason::NeedsInput,
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

/// **Witness (#1698).** A run parked on an approval does not stop the sweep
/// from reaching the runs after it.
///
/// `resume_all` continues runs one at a time and streams each to completion.
/// A resumed turn that hits a scope review does not fail — it *parks*, waiting
/// for a human who at boot is not there (there is no terminal under launchd or
/// systemd). So the sweep stopped at that id forever: every later interrupted
/// run stayed unresumed, and the service console showed the parked run as
/// continued and then went quiet. Silent, and it stranded work — the exact
/// shape #1627 exists to prevent.
#[test]
fn a_run_parked_on_an_approval_is_skipped_and_the_sweep_continues_past_it() {
    let parked = BootCandidate {
        id: "ses-1754431200000-00001".to_string(),
        parked: true,
        ..killed_mid_turn()
    };
    assert_eq!(
        decide(&parked),
        BootDecision::Skip(SkipReason::NeedsInput),
        "a run waiting on an answer nobody is there to give must not be resumed"
    );

    // The half that actually matters: everything AFTER it is still decided.
    let later = BootCandidate {
        id: "ses-1754431200000-00002".to_string(),
        ..killed_mid_turn()
    };
    let decisions = plan(&[parked, later.clone()]);
    assert_eq!(
        decisions.len(),
        2,
        "the sweep must not stop at the parked run"
    );
    assert_eq!(
        decisions[1].1,
        BootDecision::Continue,
        "the run behind a parked one is exactly the work #1698 was stranding"
    );
    assert_eq!(decisions[1].0.id, later.id);
}

/// Parking is not a failure, so it must not spend a boot attempt.
///
/// `parked` is checked *before* the `MAX_BOOT_ATTEMPTS` brake deliberately: a
/// run that waits politely across three reboots has done nothing wrong, and
/// retiring it would turn "needs an answer" into "gone, resume it by hand"
/// without anyone having been asked anything.
#[test]
fn a_parked_run_is_reported_as_needing_input_rather_than_retired() {
    let parked_and_old = BootCandidate {
        parked: true,
        attempts: MAX_BOOT_ATTEMPTS,
        ..killed_mid_turn()
    };
    assert_eq!(
        decide(&parked_and_old),
        BootDecision::Skip(SkipReason::NeedsInput),
        "the actionable reason wins: an operator can answer an approval, but \
         cannot un-retire a run they were never asked about"
    );
}
