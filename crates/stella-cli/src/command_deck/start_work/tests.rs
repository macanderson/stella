// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The start-work driver: what a draft derives, and what an approval writes.

use std::path::Path;
use std::process::Command;

use stella_protocol::issue::{Issue, IssueClass, IssueKey, IssueState};

use super::*;

fn issue(title: &str, body: &str) -> Issue {
    Issue {
        key: IssueKey::from("151"),
        title: title.to_string(),
        body: body.to_string(),
        state: IssueState::Open,
        class: IssueClass::Feature,
        labels: Vec::new(),
        created_at: String::new(),
        url: String::new(),
        parent: None,
    }
}

fn subjects(tasks: &[DraftTask]) -> Vec<&str> {
    tasks.iter().map(|task| task.subject.as_str()).collect()
}

/// A git repository with one commit, so `checkout -b` has a HEAD to branch
/// from.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(dir.path().join("README.md"), "hello\n").expect("write");
    git(&["add", "-A"]);
    git(&["commit", "-qm", "one"]);
    dir
}

fn current_branch(root: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .expect("git");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn branches(root: &Path) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["branch", "--format=%(refname:short)"])
        .output()
        .expect("git");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

// ── the draft ──────────────────────────────────────────────────────────

#[test]
fn the_plan_is_the_issues_own_definition_of_done_list() {
    let body = "## Problem\n\n- a symptom nobody planned\n\n\
                ## Definition of done\n\n\
                - [ ] read the seen-set write path\n\
                - [ ] persist the digest set\n\
                - [x] restore it on start\n";
    let tasks = plan_tasks(&issue("dedup digest", body));
    assert_eq!(
        subjects(&tasks),
        vec![
            "read the seen-set write path",
            "persist the digest set",
            "restore it on start",
        ],
        "the heading's list wins over the body's first one"
    );
}

#[test]
fn an_issue_with_no_plan_heading_falls_back_to_its_first_list() {
    let tasks = plan_tasks(&issue(
        "t",
        "context\n\n1. wire the digest\n2. restore it\n",
    ));
    assert_eq!(subjects(&tasks), vec!["wire the digest", "restore it"]);
}

#[test]
fn an_issue_with_no_list_at_all_is_one_task_named_by_its_title() {
    let tasks = plan_tasks(&issue("stop the digest resetting", "prose only\n"));
    assert_eq!(subjects(&tasks), vec!["stop the digest resetting"]);
}

#[test]
fn a_plan_never_runs_past_the_cap() {
    let body = (0..40).fold(String::from("## Tasks\n"), |mut body, i| {
        body.push_str(&format!("- step {i}\n"));
        body
    });
    assert_eq!(plan_tasks(&issue("t", &body)).len(), MAX_TASKS);
}

/// SPEC 8.2 item 4: a read-only task declares no contract, and everything
/// else declares exactly one.
#[test]
fn a_reading_task_declares_no_contract_and_a_diff_task_declares_one() {
    let tasks = plan_tasks(&issue(
        "t",
        "## Tasks\n- read the seen-set write path\n- persist the digest set\n",
    ));
    assert!(tasks[0].contract.is_none(), "{:?}", tasks[0]);
    let contract = tasks[1].contract.as_ref().expect("a contract");
    assert!(!contract.done_means.is_empty());
    assert!(contract.deterministic, "a command reaches no model");
}

#[test]
fn the_mechanism_comes_from_the_tasks_own_words() {
    let cases = [
        ("add a witness test for the flip", "unit"),
        ("keep make gate green", "gate"),
        ("make the workspace compile again", "build"),
        ("write crates/stella-core/src/seen.rs", "graph"),
        ("stop the digest resetting", "unit"),
    ];
    for (subject, wanted) in cases {
        let contract = contract_for(subject).expect("a contract");
        assert_eq!(contract.mechanism, wanted, "{subject}");
        assert!(
            MECHANISMS
                .iter()
                .any(|(name, det)| *name == contract.mechanism && *det == contract.deterministic),
            "the det tag is read out of the table: {subject}"
        );
    }
}

#[test]
fn a_graph_contract_names_the_path_the_task_named() {
    let contract = contract_for("write crates/stella-core/src/seen.rs").expect("a contract");
    assert_eq!(
        contract.done_means,
        "crates/stella-core/src/seen.rs is changed on the branch"
    );
}

#[test]
fn path_tokens_finds_paths_and_not_prose() {
    let found: Vec<String> =
        path_tokens("see `crates/a/b.rs` and src/x.ts, but not e.g. or v1.2 or a/b").collect();
    assert_eq!(found, vec!["crates/a/b.rs", "src/x.ts"]);
}

#[test]
fn list_item_strips_bullets_ordinals_and_checkboxes() {
    assert_eq!(
        list_item("- [ ] do the thing."),
        Some("do the thing".into())
    );
    assert_eq!(list_item("* [x] done already"), Some("done already".into()));
    assert_eq!(list_item("3. third"), Some("third".into()));
    assert_eq!(list_item("just prose"), None);
    assert_eq!(list_item("- "), None);
}

/// The estimate is a measurement or it is absent — SPEC §1's discipline
/// applied to the three terms SPEC 8.2 item 5 asks for.
#[test]
fn the_estimate_is_the_measured_rates_applied_to_the_measured_bytes() {
    let rates = ModelRates {
        usd_per_token: 0.000_01,
        ms_per_token: 30.0,
        calls: 12,
    };
    // 4000 bytes of input, two tasks.
    let priced = estimate(Some(rates), 4_000, 2).expect("an estimate");
    let per_task = estimate_tokens_for_bytes(4_000);
    assert_eq!(priced.tokens, per_task * 2);
    assert!((priced.usd - 0.000_01 * priced.tokens as f64).abs() < 1e-12);
    assert_eq!(
        priced.minutes,
        (30.0 * priced.tokens as f64 / 60_000.0).round() as u64
    );
}

#[test]
fn a_workspace_with_no_measured_rate_gets_no_estimate() {
    assert!(estimate(None, 4_000, 3).is_none());
}

// ── the approval ───────────────────────────────────────────────────────

#[test]
fn an_approval_opens_the_branch_and_authors_the_plans_first_revision() {
    let dir = repo();
    let line = approve(dir.path(), "#151", &["wire it".into(), "restore it".into()])
        .expect("the approval");
    assert_eq!(current_branch(dir.path()), "stella/issue-151-wire-it");
    assert!(line.contains("plan r1"), "{line}");
    assert!(
        line.contains("2 tasks · 2 [:NEXT] edges"),
        "a two-task lane is a head edge off the plan node plus one link: {line}"
    );
}

/// The witness for SPEC 8.2's "nothing runs before approval", on the driver's
/// side of the wire: the read leaves the repository exactly as it found it.
#[test]
fn drafting_creates_no_branch() {
    let dir = repo();
    let before = branches(dir.path());
    // The tracker read is the one part that needs a network; everything the
    // draft does to the repository is here, and none of it writes.
    let _ = coupled_files(dir.path(), &issue("t", "crates/a/b.rs is wrong\n"));
    let _ = applied_rules(dir.path(), &["crates/a/b.rs".to_string()]);
    let _ = gate_count(dir.path());
    assert_eq!(branches(dir.path()), before, "the draft opened a branch");
    assert_eq!(current_branch(dir.path()), "main");
}

#[test]
fn an_approval_with_no_tasks_is_refused_before_it_touches_git() {
    let dir = repo();
    let error = approve(dir.path(), "#151", &[]).expect_err("refused");
    assert!(error.contains("no tasks"), "{error}");
    assert_eq!(current_branch(dir.path()), "main");
    assert_eq!(branches(dir.path()), vec!["main".to_string()]);
}

/// A peer holding the issue's `dispatch_claims` lease stops the approval, and
/// stops it *before* the branch — the ordering the module docs argue for.
#[test]
fn a_peer_holding_the_issues_claim_stops_the_approval_before_the_branch() {
    let dir = repo();
    let held = crate::self_driving_cmd::claim::acquire_as(dir.path(), "151", "self-driving:9999");
    assert!(
        matches!(held, crate::self_driving_cmd::claim::Claim::Granted(_)),
        "the peer took the claim: {held:?}"
    );
    let error = approve(dir.path(), "#151", &["wire it".into()]).expect_err("refused");
    assert!(error.contains("already claimed by"), "{error}");
    assert!(error.contains("self-driving:9999"), "{error}");
    assert_eq!(current_branch(dir.path()), "main", "no branch was opened");

    // Releasing the peer's lease lets the same approval through, which is what
    // proves the refusal was the claim and not something else.
    drop(held);
    approve(dir.path(), "#151", &["wire it".into()]).expect("granted once the peer released");
    assert_eq!(current_branch(dir.path()), "stella/issue-151-wire-it");
}

#[test]
fn a_branch_that_already_exists_is_gits_refusal_rather_than_a_silent_reuse() {
    let dir = repo();
    approve(dir.path(), "#151", &["wire it".into()]).expect("first");
    Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(["checkout", "-q", "main"])
        .output()
        .expect("git");
    let error = approve(dir.path(), "#151", &["wire it".into()]).expect_err("refused");
    assert!(error.contains("already exists"), "{error}");
    assert_eq!(current_branch(dir.path()), "main");
}

#[test]
fn a_workspace_with_no_makefile_reports_no_gate_count_rather_than_guessing() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(gate_count(dir.path()), 0);
}
