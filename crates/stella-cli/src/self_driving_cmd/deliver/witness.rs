// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The delivery loop's pull request steps, run end to end. The forge is not
//! GitHub.
//!
//! Before the port, these steps ran `gh pr create`, `gh pr view`, `gh pr
//! ready` and `gh pr merge` from inside `deliver`. So none of them could be
//! reached without the GitHub CLI installed, logged in, and pointed at a real
//! repository. Every test that survived had to stop at a decoded payload.
//!
//! Here the same shipped functions run to the end over
//! [`FixtureForge`](super::fixture::FixtureForge), which spawns nothing. One
//! process starts in all of this. It is the `git push` in
//! [`open`](super::open), against a bare repository in a temp directory.

use std::path::Path;
use std::process::Command;

use stella_autonomy::{BlockingPolicy, CiConclusion, Mergeability, ReviewState};
use stella_protocol::pull_request::{Check, CheckOutcome, PullRequestProvider, PullRequestState};

use super::fixture::FixtureForge;

/// One check, in the port's vocabulary.
fn check(name: &str, outcome: CheckOutcome) -> Check {
    Check {
        name: name.to_owned(),
        outcome,
    }
}

/// Run `git` in `root`. Fail the test with its stderr if git refuses.
fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git is installed");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A work tree on `branch`, with one commit and a bare remote to push to.
fn workspace_with_remote(branch: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let remote = dir.path().join("remote.git");
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).expect("the work tree");

    let bare = Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(&remote)
        .output()
        .expect("git is installed");
    assert!(bare.status.success());

    git(&work, &["init", "-q"]);
    git(&work, &["config", "user.email", "loop@example.invalid"]);
    git(&work, &["config", "user.name", "the loop"]);
    git(&work, &["checkout", "-q", "-b", branch]);
    std::fs::write(work.join("README.md"), "witness\n").expect("a file to commit");
    git(&work, &["add", "README.md"]);
    git(&work, &["commit", "-q", "-m", "first"]);
    git(
        &work,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    );
    dir
}

/// **The witness.** Open, observe, mark ready and merge, with no GitHub.
///
/// It fails on the code this replaced, for the plainest reason there is.
/// Those four functions took no forge, so there was no way to hand them one.
/// Each shelled out to `gh`, which a test cannot supply.
///
/// It asserts the journal, not just the end state. A merge that never asked
/// the forge to merge would pass against a fixture that merged by itself.
#[test]
fn the_delivery_steps_run_against_a_forge_that_is_not_github() {
    let branch = "fix/1-a-witness";
    let dir = workspace_with_remote(branch);
    let work = dir.path().join("work");

    let forge = FixtureForge::new();
    forge.set_required(&["fmt + clippy + test"]);
    forge.set_commits(&["aaaa", "bbbb", "cccc"]);
    forge.set_checks(
        "main",
        vec![check("fmt + clippy + test", CheckOutcome::Passed)],
    );
    for sha in ["aaaa", "bbbb", "cccc"] {
        forge.set_checks(
            sha,
            vec![check("fmt + clippy + test", CheckOutcome::Passed)],
        );
    }
    forge.set_checks(
        branch,
        vec![check("fmt + clippy + test", CheckOutcome::Running)],
    );

    let pr = super::open(
        &forge,
        &work,
        branch,
        "1",
        "fix(x): a witness",
        stella_autonomy::SIGNATURE,
    )
    .expect("the pull request opens");
    assert_eq!(pr, "1");

    let opened = forge.pr(&pr).expect("the forge holds it");
    assert!(opened.draft, "it opens as a draft");
    assert!(
        opened.body.contains("Closes #1"),
        "the body closes its issue: {:?}",
        opened.body
    );

    let policy = BlockingPolicy::default();

    let waiting = super::observe(&forge, &pr, &policy).expect("the forge answers");
    assert_eq!(waiting.observation.ci, CiConclusion::Pending);
    assert_eq!(waiting.observation.base_ci, CiConclusion::Green);
    assert_eq!(waiting.observation.mergeable, Mergeability::Clean);
    assert_eq!(waiting.observation.review, ReviewState::None);
    assert!(waiting.observation.draft);
    assert!(!waiting.settled);

    forge.set_checks(
        branch,
        vec![check("fmt + clippy + test", CheckOutcome::Passed)],
    );
    let green = super::observe(&forge, &pr, &policy).expect("the forge answers");
    assert_eq!(green.observation.ci, CiConclusion::Green);

    super::mark_ready(&forge, &pr).expect("it comes out of draft");
    assert!(!forge.pr(&pr).expect("still there").draft);

    super::merge(&forge, &pr).expect("it merges");
    assert_eq!(
        forge.pr(&pr).expect("still there").state,
        PullRequestState::Merged
    );

    let settled = super::observe(&forge, &pr, &policy).expect("the forge answers");
    assert!(
        settled.settled,
        "a merged pull request has nothing left to decide"
    );

    let journal = forge.journal();
    for call in ["open fix/1-a-witness", "get 1", "ready 1", "merge 1"] {
        assert!(
            journal.iter().any(|entry| entry == call),
            "`{call}` is missing from {journal:?}"
        );
    }
}

/// A pull request whose every required check is stuck on the base is waived.
///
/// The second read the observation needs is the base's own history. That runs
/// through the port too, so the whole of `observe` works without GitHub.
#[test]
fn a_check_stuck_on_the_base_is_waived_rather_than_blocking() {
    let forge = FixtureForge::new();
    forge.set_required(&["flaky"]);
    forge.set_commits(&["aaaa", "bbbb", "cccc", "dddd"]);
    for sha in ["aaaa", "bbbb", "cccc", "dddd", "main"] {
        forge.set_checks(sha, vec![check("flaky", CheckOutcome::Failed)]);
    }

    let key = forge
        .open(&stella_protocol::pull_request::PullRequestDraft {
            head_ref: "fix/2".to_owned(),
            title: "t".to_owned(),
            body: "b".to_owned(),
            draft: true,
        })
        .expect("the fixture opens it");
    forge.set_checks("fix/2", vec![check("flaky", CheckOutcome::Failed)]);

    let reading = super::observe(&forge, key.as_str(), &BlockingPolicy::default())
        .expect("the forge answers");
    assert_eq!(reading.waived.len(), 1, "{:?}", reading.waived);
    assert!(
        reading.waived[0].starts_with("flaky "),
        "a check red on every recent base commit is nobody's to fix: {:?}",
        reading.waived
    );
}

/// The stale-verdict remedy asks for a base sync first. It re-runs only when
/// the forge says there is nothing to sync.
#[test]
fn refreshing_prefers_a_base_sync_and_falls_back_to_a_rerun() {
    let forge = FixtureForge::new();
    super::refresh_against_base(&forge, "7").expect("the sync succeeds");
    assert_eq!(forge.journal(), vec!["sync 7".to_owned()]);

    let declining = FixtureForge::new();
    declining.set_sync_succeeds(false);
    super::refresh_against_base(&declining, "7").expect("the rerun succeeds");
    assert_eq!(
        declining.journal(),
        vec!["sync 7".to_owned(), "rerun 7".to_owned()]
    );
}

/// A restarted loop finds what it was carrying by asking the forge.
#[test]
fn the_listings_read_the_forge_rather_than_a_remembered_list() {
    let forge = FixtureForge::new();
    for (head, body) in [
        ("stella/fix-1", "Closes #1"),
        ("stella/fix-2", "Closes #2"),
        ("someone-else/fix-3", "Closes #3"),
    ] {
        forge
            .open(&stella_protocol::pull_request::PullRequestDraft {
                head_ref: head.to_owned(),
                title: "t".to_owned(),
                body: body.to_owned(),
                draft: true,
            })
            .expect("the fixture opens it");
    }

    assert_eq!(
        super::open_prs_for_prefix(&forge, "stella/").expect("the forge answers"),
        vec!["1".to_owned(), "2".to_owned()]
    );
    assert_eq!(
        super::open_prs_for_issue(&forge, "2").expect("the forge answers"),
        vec!["2".to_owned()]
    );
    assert_eq!(
        super::prs_matching(&forge, "3").expect("the forge answers"),
        vec!["3".to_owned()]
    );
}

/// Local proof is recorded on the pull request, so it survives a restart.
#[test]
fn a_locally_verified_change_carries_its_label_into_the_next_read() {
    let forge = FixtureForge::new();
    let key = forge
        .open(&stella_protocol::pull_request::PullRequestDraft {
            head_ref: "fix/9".to_owned(),
            title: "t".to_owned(),
            body: "b".to_owned(),
            draft: true,
        })
        .expect("the fixture opens it");
    forge.set_checks("fix/9", Vec::new());

    super::mark_verified_locally(&forge, key.as_str()).expect("the label lands");

    // No required check means nothing the forge will gate on, so the label is
    // the whole verdict.
    let reading = super::observe(&forge, key.as_str(), &BlockingPolicy::default())
        .expect("the forge answers");
    assert!(reading.verified_locally);
    assert_eq!(reading.observation.ci, CiConclusion::Green);
}

/// An unreadable forge reports a healthy base rather than an emergency.
#[test]
fn a_base_nobody_can_read_is_not_reported_broken() {
    let forge = FixtureForge::new();
    forge.set_required(&["ci"]);
    assert!(
        !super::base_is_broken(&forge, "main"),
        "the fixture knows nothing about `main`, and a guess would file a \
         breakage report about nothing"
    );

    forge.set_checks("main", vec![check("ci", CheckOutcome::Failed)]);
    assert!(super::base_is_broken(&forge, "main"));
}

/// No pull request `gh` command survives outside the adapter.
///
/// The port only helps while every caller uses it. This reads the delivery
/// loop's own source. It fails on a `gh` spawned anywhere in it, which stops
/// the next change adding one beside the port rather than behind it.
/// `crate::pull_request_provider` is the one file allowed to spell it.
///
/// Two readers are named as exceptions, because they are not about pull
/// requests. `self_driving_cmd.rs` asks whether the CLI is installed, and
/// reads the last workflow run on `main` for `report`'s wake-or-sleep
/// verdict. `drive/deploy.rs` watches the release workflow. Neither one
/// opens, reads or merges a pull request.
#[test]
fn the_delivery_loop_spawns_no_gh_of_its_own() {
    const NOT_ABOUT_PULL_REQUESTS: [&str; 2] = ["self_driving_cmd.rs", "deploy.rs"];

    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    let mut pending = vec![src.join("self_driving_cmd")];
    let mut files = vec![src.join("self_driving_cmd.rs")];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path).expect("the delivery loop's source") {
            let child = entry.expect("a directory entry").path();
            if child.is_dir() {
                pending.push(child);
            } else if child.extension().is_some_and(|ext| ext == "rs") {
                files.push(child);
            }
        }
    }
    for file in files {
        let named = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if NOT_ABOUT_PULL_REQUESTS.contains(&named.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(&file).expect("a source file");
        if text.contains("Command::new(\"gh\")") {
            offenders.push(file.display().to_string());
        }
    }
    assert!(
        offenders.is_empty(),
        "these spawn `gh` outside the pull request adapter: {offenders:?}"
    );
}
