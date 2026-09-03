// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The start-work driver: what a draft derives, and what an approval writes.

use std::path::{Path, PathBuf};
use std::process::Command;

use stella_autonomy::{Contention, ContentionPolicy, ContentionVerdict, contention_verdict};
use stella_protocol::issue::{Issue, IssueClass, IssueKey, IssueState};

use super::*;
use crate::self_driving_cmd::contention::branches_naming;

fn issue(title: &str, body: &str) -> Issue {
    Issue {
        key: IssueKey::from("151"),
        title: title.to_string(),
        body: body.to_string(),
        state: IssueState::Open,
        class: IssueClass::Feature,
        labels: Vec::new(),
        created_at: String::new(),
        updated_at: String::new(),
        url: String::new(),
        parent: None,
    }
}

/// The issue every approval test starts work on, in the browse row's own
/// spelling. Named once so the tests below read as one fixture rather than
/// eight repetitions of a literal.
const KEY: &str = "#151";

fn subjects(tasks: &[DraftTask]) -> Vec<&str> {
    tasks.iter().map(|task| task.subject.as_str()).collect()
}

/// `git` in `cwd`, insisting it succeeded.
fn git_ok(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A git repository with one commit, so `checkout -b` has a HEAD to branch
/// from. No remote — see [`clones`] for the fixture that has one.
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    git_ok(dir.path(), &["init", "-q", "-b", "main"]);
    git_ok(dir.path(), &["config", "user.email", "t@example.com"]);
    git_ok(dir.path(), &["config", "user.name", "t"]);
    std::fs::write(dir.path().join("README.md"), "hello\n").expect("write");
    git_ok(dir.path(), &["add", "-A"]);
    git_ok(dir.path(), &["commit", "-qm", "one"]);
    dir
}

/// Two clones of one repository: the deck's, and a peer's on another machine.
///
/// The remote is a bare repository on disk, so the fixture reaches no network
/// and needs no credentials while still being a genuine second checkout — the
/// peer learns nothing about the deck's clone except through `origin`, which
/// is the whole question this fixture asks.
struct Clones {
    _dir: tempfile::TempDir,
    deck: PathBuf,
    peer: PathBuf,
}

fn clones() -> Clones {
    let dir = tempfile::tempdir().expect("tempdir");
    let origin = dir.path().join("origin.git");
    let deck = dir.path().join("deck");
    let peer = dir.path().join("peer");

    std::fs::create_dir(&origin).expect("mkdir");
    git_ok(&origin, &["init", "-q", "--bare", "-b", "main"]);

    std::fs::create_dir(&deck).expect("mkdir");
    git_ok(&deck, &["init", "-q", "-b", "main"]);
    git_ok(&deck, &["config", "user.email", "t@example.com"]);
    git_ok(&deck, &["config", "user.name", "t"]);
    std::fs::write(deck.join("README.md"), "hello\n").expect("write");
    git_ok(&deck, &["add", "-A"]);
    git_ok(&deck, &["commit", "-qm", "one"]);
    git_ok(
        &deck,
        &["remote", "add", "origin", &origin.display().to_string()],
    );
    git_ok(&deck, &["push", "-q", "-u", "origin", "main"]);

    git_ok(
        dir.path(),
        &[
            "clone",
            "-q",
            &origin.display().to_string(),
            &peer.display().to_string(),
        ],
    );

    Clones {
        _dir: dir,
        deck,
        peer,
    }
}

/// The heads `root`'s `origin` publishes, exactly as `contention::gather`
/// reads them.
fn ls_remote(root: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-remote", "--heads", "origin"])
        .output()
        .expect("git");
    assert!(out.status.success(), "git ls-remote: {out:?}");
    String::from_utf8_lossy(&out.stdout).to_string()
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
    let line =
        approve(dir.path(), KEY, &["wire it".into(), "restore it".into()]).expect("the approval");
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
    let error = approve(dir.path(), KEY, &[]).expect_err("refused");
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
    let error = approve(dir.path(), KEY, &["wire it".into()]).expect_err("refused");
    assert!(error.contains("already claimed by"), "{error}");
    assert!(error.contains("self-driving:9999"), "{error}");
    assert_eq!(current_branch(dir.path()), "main", "no branch was opened");

    // Releasing the peer's lease lets the same approval through, which is what
    // proves the refusal was the claim and not something else.
    drop(held);
    approve(dir.path(), KEY, &["wire it".into()]).expect("granted once the peer released");
    assert_eq!(current_branch(dir.path()), "stella/issue-151-wire-it");
}

#[test]
fn a_branch_that_already_exists_is_gits_refusal_rather_than_a_silent_reuse() {
    let dir = repo();
    approve(dir.path(), KEY, &["wire it".into()]).expect("first");
    Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(["checkout", "-q", "main"])
        .output()
        .expect("git");
    let error = approve(dir.path(), KEY, &["wire it".into()]).expect_err("refused");
    assert!(error.contains("already exists"), "{error}");
    assert_eq!(current_branch(dir.path()), "main");
}

/// **Witness.** A peer on a *second clone* defers on an issue the deck
/// has started.
///
/// Asserted through the peer's own probe — the `git ls-remote --heads origin`
/// that `contention::gather` runs, through `branches_naming`, into the verdict
/// `stella_autonomy` returns — rather than by looking for the branch by hand.
/// A test that checked the remote directly would pass on a signal nothing
/// reads.
///
/// It fails before this change by construction: the approval opened a local
/// branch and pushed nothing, so the peer's `ls-remote` named no branch at
/// all, `branches_naming` was empty, and the verdict was `Proceed`.
#[test]
fn a_peer_on_another_clone_defers_once_the_deck_starts_work() {
    let repos = clones();

    assert!(
        branches_naming(&ls_remote(&repos.peer), "151").is_empty(),
        "nothing on the remote names the issue before the approval"
    );

    let line = approve(&repos.deck, KEY, &["wire it".into()]).expect("the approval");
    assert!(line.contains("pushed to origin"), "{line}");

    let seen = branches_naming(&ls_remote(&repos.peer), "151");
    assert_eq!(
        seen,
        vec!["stella/issue-151-wire-it".to_string()],
        "the peer's probe reads the branch the deck opened"
    );
    assert!(
        matches!(
            contention_verdict(
                ContentionPolicy::Defer,
                &Contention {
                    remote_branches: seen,
                    ..Contention::default()
                }
            ),
            ContentionVerdict::Defer { .. }
        ),
        "and defers on it"
    );
}

/// A remote that will not take the branch does not take the approval with it.
///
/// The branch and the plan are real work a human asked for; the push is the
/// protection over it. Losing the protection is a smaller failure than losing
/// the work, so the approval succeeds — and says which of the two it lost, so
/// the card cannot claim a reach it does not have.
#[test]
fn an_approval_with_no_reachable_remote_still_starts_the_work_and_says_so() {
    let dir = repo();

    let line = approve(dir.path(), KEY, &["wire it".into()]).expect("the approval");

    assert_eq!(current_branch(dir.path()), "stella/issue-151-wire-it");
    assert!(line.contains("plan r1"), "{line}");
    assert!(line.contains("local only"), "{line}");
    assert!(!line.contains("pushed to origin"), "{line}");
}

/// git writes its whole conversation to stderr; the summary line quotes the
/// one sentence in it that says no.
#[test]
fn a_push_refusal_quotes_the_line_git_marked_as_one() {
    assert_eq!(
        refusal("fatal: 'origin' does not appear to be a git repository\nfatal: Could not read\n"),
        "fatal: 'origin' does not appear to be a git repository"
    );
    assert_eq!(
        refusal("To /tmp/o.git\n ! [rejected] a -> a (fetch first)\nerror: failed\nhint: pull\n"),
        "! [rejected] a -> a (fetch first)",
        "the rejected ref, not the `To` line above it or the hint below"
    );
    assert_eq!(
        refusal("something git did not mark\n"),
        "something git did not mark"
    );
    assert_eq!(refusal("  \n\n"), "git refused the push and said nothing");
}

#[test]
fn a_workspace_with_no_makefile_reports_no_gate_count_rather_than_guessing() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(gate_count(dir.path()), 0);
}
