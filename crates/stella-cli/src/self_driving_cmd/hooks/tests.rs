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

use stella_core::hooks::{HookAction, HookMatcher, Hooks};

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
    let mut hooks = Hooks::default();
    match event {
        "PreIssueWork" => hooks.pre_issue_work = Some(matchers),
        "PostIssueWork" => hooks.post_issue_work = Some(matchers),
        other => panic!("unhandled event {other}"),
    }
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
