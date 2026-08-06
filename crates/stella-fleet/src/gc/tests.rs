//! Tests for the fleet garbage collector.
//!
//! Two layers, the same shape the rest of this crate uses: scripted-`GitCli`
//! tests that assert on the exact argv (so the namespace rules are provable
//! without a repo), and real-`git` fixtures that seed a throwaway repository
//! and prove the end-to-end verdicts on actual worktrees and branches.

use super::*;
use crate::git::{GitError, GitOutput, SystemGitCli, WorktreeManager};
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use tokio::process::Command;

// ---------------------------------------------------------------- fakes

type GitHandler = Box<dyn Fn(&[String]) -> GitOutput + Send + Sync>;

/// A [`GitCli`] that records every invocation and answers via a closure.
struct ScriptedGit {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
    handler: GitHandler,
}

impl ScriptedGit {
    fn new(handler: impl Fn(&[String]) -> GitOutput + Send + Sync + 'static) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            handler: Box::new(handler),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl GitCli for ScriptedGit {
    async fn run(&self, _repo: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(owned.clone());
        Ok((self.handler)(&owned))
    }
}

/// The porcelain listing of a repo with the main checkout, one fleet
/// worktree, and one hand-made worktree outside the fleet namespace.
const MIXED_LISTING: &str = "\
worktree /repo
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /repo/.stella/worktrees/t1-abc
HEAD 2222222222222222222222222222222222222222
branch refs/heads/fleet/t1-abc

worktree /repo/hand-made
HEAD 3333333333333333333333333333333333333333
branch refs/heads/my-feature
";

fn scripted(handler: impl Fn(&[String]) -> GitOutput + Send + Sync + 'static) -> Gc<ScriptedGit> {
    Gc::new(ScriptedGit::new(handler), "/repo")
}

fn arg(call: &[String], i: usize) -> &str {
    call.get(i).map_or("", String::as_str)
}

// ------------------------------------------------- namespace + argv rules

#[tokio::test]
async fn the_main_checkout_and_foreign_worktrees_are_never_candidates() {
    let gc = scripted(|args| match arg(args, 0) {
        "worktree" if arg(args, 1) == "list" => GitOutput::ok(MIXED_LISTING),
        "merge-base" => GitOutput::ok(""),
        "status" => GitOutput::ok(""),
        _ => GitOutput::ok(""),
    });
    let report = gc
        .sweep(
            &[],
            &GcOptions {
                dry_run: true,
                base_ref: Some("main".into()),
                ..GcOptions::default()
            },
            0,
        )
        .await
        .expect("sweep");

    // Only the worktree under .stella/worktrees/ is even reported on.
    assert_eq!(report.worktrees.len(), 1, "{:?}", report.worktrees);
    assert_eq!(
        report.worktrees[0].path,
        PathBuf::from("/repo/.stella/worktrees/t1-abc")
    );
    // And nothing named the main checkout or the hand-made worktree.
    for call in gc.git.calls() {
        assert!(
            !call.iter().any(|a| a == "/repo/hand-made" || a == "my-feature"),
            "a foreign worktree/branch reached git: {call:?}"
        );
    }
}

#[tokio::test]
async fn a_dry_run_mutates_nothing() {
    let gc = scripted(|args| match arg(args, 0) {
        "worktree" if arg(args, 1) == "list" => GitOutput::ok(MIXED_LISTING),
        "for-each-ref" => GitOutput::ok(""),
        _ => GitOutput::ok(""),
    });
    let report = gc
        .sweep(
            &[],
            &GcOptions {
                dry_run: true,
                base_ref: Some("main".into()),
                ..GcOptions::default()
            },
            0,
        )
        .await
        .expect("sweep");
    assert_eq!(report.reclaimed_worktrees(), 1);
    for call in gc.git.calls() {
        assert!(
            !matches!(
                (arg(&call, 0), arg(&call, 1)),
                ("worktree", "remove") | ("worktree", "prune") | ("branch", "-D")
            ),
            "dry run issued a mutation: {call:?}"
        );
    }
}

#[tokio::test]
async fn an_unanswerable_merge_base_keeps_the_branch() {
    // git exits non-zero for its own reasons (a broken ref, say) — not the
    // honest "no". The branch must survive either way.
    let gc = scripted(|args| match arg(args, 0) {
        "worktree" if arg(args, 1) == "list" => GitOutput::ok(MIXED_LISTING),
        "merge-base" => GitOutput::failed(128, "fatal: Not a valid object name"),
        "for-each-ref" => GitOutput::ok(""),
        _ => GitOutput::ok(""),
    });
    let report = gc
        .sweep(
            &[],
            &GcOptions {
                base_ref: Some("main".into()),
                ..GcOptions::default()
            },
            0,
        )
        .await
        .expect("sweep");
    assert_eq!(
        report.worktrees[0].action,
        WorktreeAction::Kept(KeepReason::UnmergedCommits)
    );
}

#[tokio::test]
async fn an_in_flight_attempt_survives_even_force() {
    let gc = scripted(|args| match arg(args, 0) {
        "worktree" if arg(args, 1) == "list" => GitOutput::ok(MIXED_LISTING),
        "for-each-ref" => GitOutput::ok(""),
        _ => GitOutput::ok(""),
    });
    let activity = [WorktreeActivity {
        worktree_path: "/repo/.stella/worktrees/t1-abc".into(),
        unfinished_attempts: 1,
        last_finished_ms: None,
    }];
    let report = gc
        .sweep(
            &activity,
            &GcOptions {
                force: true,
                base_ref: Some("main".into()),
                ..GcOptions::default()
            },
            0,
        )
        .await
        .expect("sweep");
    assert_eq!(
        report.worktrees[0].action,
        WorktreeAction::Kept(KeepReason::InFlight)
    );
    assert_eq!(report.reclaimed_worktrees(), 0);
}

#[tokio::test]
async fn age_keeps_a_recent_worktree_and_an_undatable_one() {
    let listing_git = |args: &[String]| match arg(args, 0) {
        "worktree" if arg(args, 1) == "list" => GitOutput::ok(MIXED_LISTING),
        "for-each-ref" => GitOutput::ok(""),
        _ => GitOutput::ok(""),
    };
    let day = MS_PER_DAY;

    let gc = scripted(listing_git);
    let recent = [WorktreeActivity {
        worktree_path: "/repo/.stella/worktrees/t1-abc".into(),
        unfinished_attempts: 0,
        last_finished_ms: Some(9 * day),
    }];
    let report = gc
        .sweep(
            &recent,
            &GcOptions {
                dry_run: true,
                min_age_days: Some(7),
                base_ref: Some("main".into()),
                ..GcOptions::default()
            },
            10 * day,
        )
        .await
        .expect("sweep");
    assert_eq!(
        report.worktrees[0].action,
        WorktreeAction::Kept(KeepReason::TooRecent { age_days: 1 })
    );

    // No ledger row at all: the age cannot be established, so --age keeps it.
    let gc = scripted(listing_git);
    let report = gc
        .sweep(
            &[],
            &GcOptions {
                dry_run: true,
                min_age_days: Some(7),
                base_ref: Some("main".into()),
                ..GcOptions::default()
            },
            10 * day,
        )
        .await
        .expect("sweep");
    assert_eq!(
        report.worktrees[0].action,
        WorktreeAction::Kept(KeepReason::AgeUnknown)
    );
}

#[tokio::test]
async fn a_loose_unmerged_fleet_branch_is_kept_and_a_merged_one_is_deleted() {
    let gc = scripted(|args| match (arg(args, 0), arg(args, 1)) {
        ("worktree", "list") => GitOutput::ok("worktree /repo\nHEAD 1111\nbranch refs/heads/main\n"),
        ("for-each-ref", _) => GitOutput::ok("fleet/done-1\nfleet/wip-2\n"),
        // Only the first branch is contained in the base ref.
        ("merge-base", _) => {
            if args.iter().any(|a| a == "fleet/done-1") {
                GitOutput::ok("")
            } else {
                GitOutput::failed(1, "")
            }
        }
        _ => GitOutput::ok(""),
    });
    let report = gc
        .sweep(
            &[],
            &GcOptions {
                base_ref: Some("main".into()),
                ..GcOptions::default()
            },
            0,
        )
        .await
        .expect("sweep");
    assert_eq!(
        report.branches,
        vec![
            BranchVerdict {
                branch: "fleet/done-1".into(),
                action: BranchAction::Deleted,
            },
            BranchVerdict {
                branch: "fleet/wip-2".into(),
                action: BranchAction::Kept(KeepReason::UnmergedCommits),
            },
        ]
    );
    let deleted: Vec<Vec<String>> = gc
        .git
        .calls()
        .into_iter()
        .filter(|c| arg(c, 0) == "branch")
        .collect();
    assert_eq!(deleted.len(), 1, "{deleted:?}");
    assert_eq!(arg(&deleted[0], 2), "fleet/done-1");
}

#[tokio::test]
async fn no_resolvable_base_ref_stops_the_sweep() {
    let gc = scripted(|_| GitOutput::failed(1, ""));
    let err = gc
        .sweep(&[], &GcOptions::default(), 0)
        .await
        .expect_err("a sweep with nothing to judge against must not guess");
    assert!(matches!(err, GcError::NoBaseRef), "{err:?}");
}

// --------------------------------------------------- real-git integration

/// Whether a real `git` is on PATH; the integration tests skip cleanly (with
/// a printed note) when it isn't — CI has it.
async fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Init a throwaway repo with a seed commit on `main`; return its sha.
async fn seed_repo(dir: &Path) -> String {
    let git = SystemGitCli;
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "fleet@test.local"],
        vec!["config", "user.name", "Fleet Test"],
        vec!["config", "commit.gpgsign", "false"],
    ] {
        let out = git.run(dir, &args).await.unwrap();
        assert!(out.success, "git {args:?} failed: {}", out.stderr);
    }
    std::fs::write(dir.join("README.md"), "seed\n").unwrap();
    for args in [vec!["add", "--", "README.md"], vec!["commit", "-q", "-m", "seed"]] {
        let out = git.run(dir, &args).await.unwrap();
        assert!(out.success, "git {args:?} failed: {}", out.stderr);
    }
    let out = git.run(dir, &["rev-parse", "HEAD"]).await.unwrap();
    out.stdout.trim().to_string()
}

fn verdict_for<'r>(report: &'r GcReport, needle: &str) -> &'r WorktreeVerdict {
    report
        .worktrees
        .iter()
        .find(|v| v.path.to_string_lossy().contains(needle))
        .unwrap_or_else(|| panic!("no verdict for {needle} in {:?}", report.worktrees))
}

/// **The witness (#1217).** On a real repository, one sweep reclaims the
/// worktree and branch of a task whose work is already in the base ref, and
/// leaves alone the three shapes that must survive: work still in flight, an
/// uncommitted working tree, and a branch carrying unmerged commits.
#[tokio::test]
async fn real_git_gc_reclaims_a_finished_worktree_and_keeps_live_dirty_and_unmerged_ones() {
    if !git_available().await {
        eprintln!("skipping real-git gc test: `git` not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = seed_repo(repo).await;
    let mgr = WorktreeManager::new(SystemGitCli, repo);

    // (1) finished: created, nothing committed → already contained in main.
    let done = mgr.create("done", &base).await.unwrap();
    // (2) in flight: identical on disk, but the ledger says a worker has it.
    let live = mgr.create("live", &base).await.unwrap();
    // (3) dirty: an uncommitted file.
    let dirty = mgr.create("dirty", &base).await.unwrap();
    std::fs::write(dirty.path.join("scratch.txt"), "unsaved work\n").unwrap();
    // (4) unmerged: a commit main does not have.
    let unmerged = mgr.create("unmerged", &base).await.unwrap();
    std::fs::write(unmerged.path.join("f.txt"), "real work\n").unwrap();
    mgr.commit_paths(&unmerged.path, &[Path::new("f.txt")], "work")
        .await
        .unwrap();

    let activity = [WorktreeActivity {
        worktree_path: live.path.to_string_lossy().into_owned(),
        unfinished_attempts: 1,
        last_finished_ms: None,
    }];

    let gc = Gc::new(SystemGitCli, repo);
    let report = gc
        .sweep(
            &activity,
            &GcOptions {
                base_ref: Some("main".into()),
                ..GcOptions::default()
            },
            0,
        )
        .await
        .expect("sweep");

    assert_eq!(
        verdict_for(&report, "done-").action,
        WorktreeAction::Removed {
            branch_deleted: true
        }
    );
    assert_eq!(
        verdict_for(&report, "live-").action,
        WorktreeAction::Kept(KeepReason::InFlight)
    );
    assert_eq!(
        verdict_for(&report, "dirty-").action,
        WorktreeAction::Kept(KeepReason::Dirty)
    );
    assert_eq!(
        verdict_for(&report, "unmerged-").action,
        WorktreeAction::Kept(KeepReason::UnmergedCommits)
    );

    // The reclaimed one is gone from disk and from git's branch list; the
    // other three are still there, with their work intact.
    assert!(!done.path.exists(), "the finished worktree is gone");
    assert!(live.path.exists() && dirty.path.exists() && unmerged.path.exists());
    assert_eq!(
        std::fs::read_to_string(dirty.path.join("scratch.txt")).unwrap(),
        "unsaved work\n",
        "uncommitted work must never be touched"
    );
    let branches = SystemGitCli
        .run(repo, &["branch", "--format=%(refname:short)"])
        .await
        .unwrap()
        .stdout;
    assert!(!branches.contains(&done.branch), "{branches}");
    for kept in [&live, &dirty, &unmerged] {
        assert!(branches.contains(&kept.branch), "{branches}");
    }
}

/// `--force` is what reclaims work at rest — and only that. The dirty and
/// unmerged worktrees above go; the in-flight one still does not.
#[tokio::test]
async fn real_git_force_reclaims_dirty_and_unmerged_but_not_in_flight() {
    if !git_available().await {
        eprintln!("skipping real-git force test: `git` not available");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let base = seed_repo(repo).await;
    let mgr = WorktreeManager::new(SystemGitCli, repo);

    let live = mgr.create("live", &base).await.unwrap();
    let dirty = mgr.create("dirty", &base).await.unwrap();
    std::fs::write(dirty.path.join("scratch.txt"), "unsaved\n").unwrap();
    let unmerged = mgr.create("unmerged", &base).await.unwrap();
    std::fs::write(unmerged.path.join("f.txt"), "work\n").unwrap();
    mgr.commit_paths(&unmerged.path, &[Path::new("f.txt")], "work")
        .await
        .unwrap();

    let activity = [WorktreeActivity {
        worktree_path: live.path.to_string_lossy().into_owned(),
        unfinished_attempts: 1,
        last_finished_ms: None,
    }];
    let gc = Gc::new(SystemGitCli, repo);
    let report = gc
        .sweep(
            &activity,
            &GcOptions {
                force: true,
                base_ref: Some("main".into()),
                ..GcOptions::default()
            },
            0,
        )
        .await
        .expect("sweep");

    assert_eq!(
        verdict_for(&report, "dirty-").action,
        WorktreeAction::Removed {
            branch_deleted: true
        }
    );
    assert_eq!(
        verdict_for(&report, "unmerged-").action,
        WorktreeAction::Removed {
            branch_deleted: true
        }
    );
    assert_eq!(
        verdict_for(&report, "live-").action,
        WorktreeAction::Kept(KeepReason::InFlight),
        "--force is a judgment about work at rest, never about a running worker"
    );
    assert!(live.path.exists());
}
