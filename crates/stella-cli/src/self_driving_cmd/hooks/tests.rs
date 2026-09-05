// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `PreIssueWork` gate, over the real shell runner.
//!
//! These spawn actual `bash -c` hooks rather than a fake runner, because the
//! thing worth proving is the whole path a user's `settings.json` takes: the
//! matcher selection, the payload on stdin, the decision parsed off stdout,
//! and the fold into a skip. A fake runner would assert the fold and leave
//! the three steps a user actually writes untested.

use std::path::PathBuf;

use stella_core::hooks::{HookAction, HookMatcher, HookPayload, Hooks};

use super::*;

/// A temp root whose path is safe to interpolate into a shell command.
///
/// A thread id renders as `ThreadId(9)` — parentheses, which `bash -c` reads
/// as syntax. These tests run real shell hooks against real paths, so the name
/// is reduced to characters a shell has no opinion about rather than quoted at
/// every use site.
fn temp_root(label: &str) -> PathBuf {
    let thread: String = format!("{:?}", std::thread::current().id())
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let root = std::env::temp_dir().join(format!(
        "stella-sd-hooks-{label}-{}-{thread}",
        std::process::id(),
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp root");
    root
}

/// Settings carrying one `PreIssueWork` hook running `command`.
fn settings_with(event: &str, command: &str) -> Settings {
    let matchers = vec![HookMatcher {
        matcher: None,
        hooks: vec![HookAction::new(command)],
    }];
    // Through the same serde renames a real `settings.json` uses, rather than
    // a `match` naming one field per event: that match was two arms long and
    // would have needed nineteen, each of which is a place to write the wrong
    // field and have a test pass against the wrong hook.
    let hooks: Hooks = serde_json::from_value(serde_json::json!({ event: matchers }))
        .expect("the event names a field of Hooks");
    // Built by mutation rather than by struct update: `Settings` has private
    // fields, and a test that could name them all would be a test coupled to
    // every future one.
    let mut settings = Settings::default();
    settings.hooks = Some(hooks);
    settings
}

fn issue(number: &str) -> HookIssueInfo {
    HookIssueInfo::new(number)
}

/// **The witness for the whole feature.** A hook can hold the loop off one
/// issue, and the reason it gives reaches the operator.
///
/// This is the `agent-hold` shape: a real hook reads the issue number off the
/// payload, decides, and prints a `deny`. Here the decision is unconditional;
/// what is being proved is that a `deny` becomes a skip carrying the hook's
/// own words.
#[test]
fn a_deny_skips_the_issue_and_carries_the_hook_s_reason() {
    let root = temp_root("deny");
    let settings = settings_with(
        "PreIssueWork",
        r#"echo '{"action":"deny","reason":"labelled agent-hold"}'"#,
    );
    assert_eq!(
        before_issue_work(&root, &settings, &issue("42")),
        WorkGate::Skip {
            reason: "labelled agent-hold".into()
        }
    );
}

/// The payload reaches the hook on stdin, carrying the issue it is being asked
/// about — without which a hook could only ever answer the same way for every
/// issue, which is not a gate.
#[test]
fn the_hook_receives_the_issue_number_on_stdin() {
    let root = temp_root("payload");
    let seen = root.join("payload.json");
    let settings = settings_with("PreIssueWork", &format!("cat > {}", seen.display()));

    assert_eq!(
        before_issue_work(&root, &settings, &issue("4000")),
        WorkGate::Allow,
        "a hook that decides nothing allows"
    );

    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&seen).expect("the hook was run"))
            .expect("the payload is JSON");
    assert_eq!(payload["event"], "PreIssueWork");
    assert_eq!(payload["issue"]["number"], "4000");
    assert_eq!(payload["cwd"], root.display().to_string());
}

/// **Fails closed.** A hook that exits non-zero has not said "go ahead" — it
/// has said nothing, and this gate exists to withhold work. See the module
/// header for why the two outcomes are not symmetric.
#[test]
fn a_hook_that_cannot_be_evaluated_skips_rather_than_proceeds() {
    let root = temp_root("failclosed");
    let settings = settings_with("PreIssueWork", "exit 3");
    let gate = before_issue_work(&root, &settings, &issue("1"));
    let WorkGate::Skip { reason } = gate else {
        panic!("a failed hook must skip, not allow: {gate:?}");
    };
    assert!(reason.contains("could not be evaluated"), "{reason}");
}

/// No human is driving an unattended loop, so an approval request is declined
/// rather than rendered to nobody.
#[test]
fn require_approval_skips_because_no_one_is_there_to_answer() {
    let root = temp_root("approval");
    let settings = settings_with(
        "PreIssueWork",
        r#"echo '{"action":"require_approval","reason":"ask me first"}'"#,
    );
    assert_eq!(
        before_issue_work(&root, &settings, &issue("7")),
        WorkGate::Skip {
            reason: "ask me first".into()
        }
    );
}

/// An explicit allow is an allow, so a hook that inspects and approves does
/// not cost the issue.
#[test]
fn an_explicit_allow_lets_the_work_proceed() {
    let root = temp_root("allow");
    let settings = settings_with("PreIssueWork", r#"echo '{"action":"allow"}'"#);
    assert_eq!(
        before_issue_work(&root, &settings, &issue("9")),
        WorkGate::Allow
    );
}

/// The overwhelmingly common case: nothing registered, nothing run, no
/// runtime built. A loop in a workspace with no hooks must pay nothing for
/// this feature existing.
#[test]
fn a_workspace_with_no_hook_registered_allows_without_running_anything() {
    let root = temp_root("none");
    assert_eq!(
        before_issue_work(&root, &Settings::default(), &issue("1")),
        WorkGate::Allow
    );
    // A hook registered for the *other* event must not be selected for this
    // one — the matcher table is keyed per event, and reading the wrong key
    // would run a reporting hook as a gate.
    let elsewhere = settings_with("PostIssueWork", "exit 1");
    assert_eq!(
        before_issue_work(&root, &elsewhere, &issue("1")),
        WorkGate::Allow,
        "a PostIssueWork hook must not gate PreIssueWork"
    );
}

/// `PostIssueWork` reports and cannot block: it runs after the work, so there
/// is nothing left to veto. A failing one must not panic or propagate.
#[test]
fn post_issue_work_reports_the_outcome_and_never_blocks() {
    let root = temp_root("post");
    let seen = root.join("outcome.json");
    let settings = settings_with("PostIssueWork", &format!("cat > {}", seen.display()));

    after_issue_work(
        &root,
        &settings,
        &issue("4000"),
        HookIssueOutcome::Changed {
            summary: "3 files changed".into(),
        },
    );

    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&seen).expect("the hook was run"))
            .expect("the payload is JSON");
    assert_eq!(payload["event"], "PostIssueWork");
    assert_eq!(payload["issueOutcome"]["status"], "changed");
    assert_eq!(payload["issueOutcome"]["summary"], "3 files changed");

    // A hook that fails is a warning, not a propagated error — the work is
    // already done and there is nothing to undo.
    after_issue_work(
        &root,
        &settings_with("PostIssueWork", "exit 9"),
        &issue("1"),
        HookIssueOutcome::NoChange,
    );
}

/// A failed work unit reports as `failed`, not as `no_change`. The two are
/// the arms a dashboard most needs apart: one is a loop with nothing to do,
/// the other is a loop that needs a human.
#[test]
fn a_failure_is_reported_as_its_own_outcome() {
    let root = temp_root("failed");
    let seen = root.join("outcome.json");
    let settings = settings_with("PostIssueWork", &format!("cat > {}", seen.display()));
    after_issue_work(
        &root,
        &settings,
        &issue("5"),
        HookIssueOutcome::Failed {
            reason: "the turn exited 1".into(),
        },
    );
    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&seen).expect("the hook was run")).unwrap();
    assert_eq!(payload["issueOutcome"]["status"], "failed");
    assert_eq!(payload["issueOutcome"]["reason"], "the turn exited 1");
}

/// **The witness for every reporting event** (#4017): each one reaches a
/// registered hook, under its own name, carrying the identity a subscriber
/// needs.
///
/// One test over the whole vocabulary rather than nineteen near-identical
/// ones, and driven from `HookEvent::ALL` rather than a list written here — a
/// list would be a third copy of the vocabulary, and the failure it would hide
/// is a declared event nothing dispatches, which is the exact defect #4017
/// exists to close.
#[test]
fn every_reporting_event_reaches_a_hook_under_its_own_name() {
    let root = temp_root("reporting");
    for event in HookEvent::ALL {
        // `PreIssueWork` is the one gate, covered above. The rest are the
        // engine's to fire, and never come through here.
        if event.in_turn() || event == HookEvent::PreIssueWork {
            continue;
        }
        let seen = root.join(format!("{event}.json"));
        let settings = settings_with(event.as_str(), &format!("cat > {}", seen.display()));
        report(&settings, payload_for(event, &root));

        let text = std::fs::read_to_string(&seen)
            .unwrap_or_else(|error| panic!("{event} did not reach its hook: {error}"));
        let payload: serde_json::Value = serde_json::from_str(&text).expect("the payload is JSON");
        assert_eq!(payload["event"], event.as_str(), "{payload}");
        assert_eq!(payload["cwd"], root.display().to_string(), "{payload}");
    }
}

/// The identity each family carries, asserted against what a subscriber would
/// actually read off the pipe.
#[test]
fn a_reporting_event_carries_the_identity_its_family_is_keyed_by() {
    let root = temp_root("identity");

    let cycle = fired(&root, HookEvent::DriveCycleEnd);
    assert_eq!(cycle["run"]["runId"], "r-99");
    assert_eq!(cycle["run"]["cycle"], 7);

    // A run event brackets every cycle rather than sitting in one, so it
    // carries no cycle number at all — absent, not zero.
    let run = fired(&root, HookEvent::DriveRunStart);
    assert_eq!(run["run"]["runId"], "r-99");
    assert!(run["run"].get("cycle").is_none(), "{run}");

    let escalated = fired(&root, HookEvent::IssueEscalated);
    assert_eq!(escalated["issue"]["number"], "4310");
    assert_eq!(escalated["reason"], "the fix ceiling was reached");

    let broken = fired(&root, HookEvent::BaseBroken);
    assert_eq!(broken["pullRequest"]["number"], "412");

    // And the two check events are two names, which is the whole reason they
    // are two events: a subscriber deciding whether to fix or to wait branches
    // on this and nothing else.
    let ours = fired(&root, HookEvent::ChecksFailed);
    assert_ne!(ours["event"], broken["event"]);
}

/// A hook that cannot be run does not fail the verb that reported through it.
///
/// The half that matters for a loop: a broken notifier must not stop the
/// delivery it was notifying about.
#[test]
fn a_reporting_hook_that_fails_does_not_stop_the_loop() {
    let root = temp_root("tolerant");
    report(
        &settings_with("DriveRunEnd", "exit 9"),
        payload_for(HookEvent::DriveRunEnd, &root),
    );
    report(
        &settings_with("DriveRunEnd", "/nonexistent/hook"),
        payload_for(HookEvent::DriveRunEnd, &root),
    );
}

/// Fire one event into a hook that captures its payload, and read it back.
fn fired(root: &std::path::Path, event: HookEvent) -> serde_json::Value {
    let seen = root.join(format!("{event}.json"));
    let settings = settings_with(event.as_str(), &format!("cat > {}", seen.display()));
    report(&settings, payload_for(event, root));
    serde_json::from_str(&std::fs::read_to_string(&seen).expect("the hook was run"))
        .expect("the payload is JSON")
}

/// The widest payload each event carries, built through the same constructors
/// the verbs use.
fn payload_for(event: HookEvent, root: &std::path::Path) -> HookPayload {
    let cwd = root.display().to_string();
    let run = HookRunInfo::new("r-99");
    match event {
        HookEvent::DriveCycleStart | HookEvent::DriveCycleEnd | HookEvent::DriveIdle => {
            HookPayload::drive(event, cwd, run.in_cycle(7), Some("a reason".into()))
        }
        HookEvent::DriveRunStart
        | HookEvent::DriveRunEnd
        | HookEvent::DriveBudgetExhausted
        | HookEvent::DriveRefused => HookPayload::drive(event, cwd, run, Some("a reason".into())),
        HookEvent::IssueCreated | HookEvent::IssueClosed => {
            HookPayload::tracker(event, cwd, HookIssueInfo::new("4310"), None)
        }
        HookEvent::IssueEscalated => HookPayload::tracker(
            event,
            cwd,
            HookIssueInfo::new("4310"),
            Some("the fix ceiling was reached".into()),
        ),
        HookEvent::PullRequestOpened
        | HookEvent::PullRequestReadyForReview
        | HookEvent::PullRequestConflicted
        | HookEvent::PullRequestMerged
        | HookEvent::ChecksFailed
        | HookEvent::BaseBroken
        | HookEvent::ChecksGreen => HookPayload::pull_request(
            event,
            cwd,
            HookPullRequestInfo::new("412").for_issue("4310"),
            None,
        ),
        HookEvent::PostIssueWork => HookPayload::post_issue_work(
            cwd,
            HookIssueInfo::new("4310"),
            HookIssueOutcome::NoChange,
        ),
        // The gate and the in-turn hooks never come through `report`. A
        // total match keeps a new event out of a default arm that would
        // report nothing.
        HookEvent::PreIssueWork
        | HookEvent::SessionStart
        | HookEvent::PreToolUse
        | HookEvent::PostToolUse
        | HookEvent::Stop
        | HookEvent::PreCompact
        | HookEvent::UserPromptSubmit
        | HookEvent::SubagentStart
        | HookEvent::SubagentStop => {
            panic!("{event} is not dispatched by the loop")
        }
    }
}
