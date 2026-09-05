//! What the lock holds apart, and what it leaves alone.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use tempfile::TempDir;

use super::*;
use crate::git::WorktreeManager;

/// How many calls a fake saw at once, and the peak.
#[derive(Default)]
struct Overlap {
    in_flight: AtomicUsize,
    peak: AtomicUsize,
}

/// A [`GitCli`] that stays inside every command for `hold`. Callers the lock
/// does not hold back pile up in there.
struct OverlapGit {
    overlap: Arc<Overlap>,
    hold: Duration,
}

#[async_trait]
impl GitCli for OverlapGit {
    async fn run(&self, _repo: &Path, _args: &[&str]) -> Result<GitOutput, GitError> {
        let now = self.overlap.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.overlap.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(self.hold).await;
        self.overlap.in_flight.fetch_sub(1, Ordering::SeqCst);
        Ok(GitOutput::ok(""))
    }
}

/// The witness for the race. Eight tasks go into one manager at once. The
/// fake sleeps inside `worktree add`. With no lock, all eight sit in the
/// command together and the peak is eight.
///
/// Run 9255 is what that costs: git read a sibling's half-written
/// `.git/worktrees/<name>/commondir` and the run lost a task.
///
/// The fake's `await` makes the overlap, so no race has to be won. Without
/// the lock this test fails every time, not most of the time.
#[tokio::test]
async fn concurrent_creates_never_overlap_a_worktree_add() {
    let repo = TempDir::new().expect("temp repo root");
    let overlap = Arc::new(Overlap::default());
    let manager = WorktreeManager::new(
        OverlapGit {
            overlap: Arc::clone(&overlap),
            hold: Duration::from_millis(20),
        },
        repo.path(),
    );

    let ids: Vec<String> = (0..8).map(|n| format!("t{n}")).collect();
    let created = futures_util::future::join_all(ids.iter().map(|id| manager.create(id, "base")))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("every dispatch creates its tree");

    assert_eq!(
        overlap.peak.load(Ordering::SeqCst),
        1,
        "two `git worktree add` calls were in flight against one repository"
    );
    let paths: HashSet<_> = created.iter().map(|tree| tree.path.clone()).collect();
    assert_eq!(paths.len(), 8, "every worker needs a tree of its own");
    let branches: HashSet<_> = created.iter().map(|tree| tree.branch.clone()).collect();
    assert_eq!(branches.len(), 8);
}

/// The control. Same fake, same eight callers, no lock in the path. The peak
/// is eight. An unlocked `create` reaches the same number, so the counter is
/// known to climb.
#[tokio::test]
async fn the_fake_reports_overlap_when_nothing_holds_the_callers_apart() {
    let repo = TempDir::new().expect("temp repo root");
    let overlap = Arc::new(Overlap::default());
    let git = OverlapGit {
        overlap: Arc::clone(&overlap),
        hold: Duration::from_millis(20),
    };
    // `status --porcelain` is a worker's own check. It never touches
    // `.git/worktrees`, so it takes no lock.
    let statuses = futures_util::future::join_all(
        (0..8).map(|_| git.run(repo.path(), &["status", "--porcelain"])),
    )
    .await;
    assert!(statuses.iter().all(Result::is_ok));
    assert_eq!(overlap.peak.load(Ordering::SeqCst), 8);
}

#[tokio::test]
async fn the_lock_file_is_taken_and_released() {
    let repo = TempDir::new().expect("temp repo root");
    let path = lock_path(repo.path());
    let held = LockFile::acquire(&path).await.expect("lock file");
    assert!(path.exists());
    drop(held);
    assert!(!path.exists());
}

/// A live lock file is honoured until the wait runs out. Then the waiter goes
/// ahead without it, rather than losing the task.
#[tokio::test]
async fn a_live_lock_file_is_honoured_then_given_up_on() {
    let repo = TempDir::new().expect("temp repo root");
    let path = lock_path(repo.path());
    let _held = LockFile::acquire(&path).await.expect("lock file");
    let second =
        LockFile::acquire_within(&path, Duration::from_millis(50), Duration::from_secs(1800)).await;
    assert!(second.is_none());
}

/// A lock file left by a dead process is broken, not kept forever.
#[tokio::test]
async fn a_stale_lock_file_is_broken() {
    let repo = TempDir::new().expect("temp repo root");
    let path = lock_path(repo.path());
    // What a process that died leaves behind.
    create_private_dir(path.parent().expect("lock parent")).expect("private dir");
    std::fs::write(&path, b"").expect("stale lock file");
    tokio::time::sleep(Duration::from_millis(10)).await;

    let taken =
        LockFile::acquire_within(&path, Duration::from_secs(5), Duration::from_millis(1)).await;
    assert!(taken.is_some(), "a stale lock must not wedge dispatch");
}

/// A `GitCli` that replays a list of outputs and counts its calls.
struct ScriptedRuns {
    outputs: SyncMutex<Vec<GitOutput>>,
    calls: AtomicUsize,
}

impl ScriptedRuns {
    fn new(outputs: Vec<GitOutput>) -> Self {
        Self {
            outputs: SyncMutex::new(outputs),
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl GitCli for ScriptedRuns {
    async fn run(&self, _repo: &Path, _args: &[&str]) -> Result<GitOutput, GitError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut outputs = self
            .outputs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if outputs.is_empty() {
            return Ok(GitOutput::ok(""));
        }
        Ok(outputs.remove(0))
    }
}

#[tokio::test]
async fn a_transient_failure_is_retried_once() {
    let repo = TempDir::new().expect("temp repo root");
    let git = ScriptedRuns::new(vec![
        GitOutput::failed(
            128,
            "fatal: unable to create '.git/index.lock': File exists",
        ),
        GitOutput::ok(""),
    ]);
    let out = run_worktree(&git, repo.path(), &["worktree", "add", "/tmp/x"])
        .await
        .expect("git ran");
    assert!(out.success);
    assert_eq!(git.calls.load(Ordering::SeqCst), 2);
}

/// A retry that does not help reports the first failure, not the second. The
/// retry is there to beat a clash, not to rename the error.
#[tokio::test]
async fn a_retry_that_fails_reports_the_first_error() {
    let repo = TempDir::new().expect("temp repo root");
    let git = ScriptedRuns::new(vec![
        GitOutput::failed(128, "fatal: failed to read .git/worktrees/t1/commondir"),
        GitOutput::failed(1, "second"),
    ]);
    let out = run_worktree(&git, repo.path(), &["worktree", "add", "/tmp/x"])
        .await
        .expect("git ran");
    assert!(!out.success);
    assert!(out.stderr.contains("commondir"));
    assert_eq!(out.code, Some(128));
}

#[tokio::test]
async fn a_real_collision_is_not_retried() {
    let repo = TempDir::new().expect("temp repo root");
    let git = ScriptedRuns::new(vec![GitOutput::failed(
        128,
        "fatal: '/tmp/x' already exists",
    )]);
    let out = run_worktree(&git, repo.path(), &["worktree", "add", "/tmp/x"])
        .await
        .expect("git ran");
    assert!(!out.success);
    assert_eq!(git.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn transient_signatures_are_the_ones_a_peer_could_have_caused() {
    assert!(is_transient(
        "fatal: Unable to create '.git/index.lock': File exists"
    ));
    assert!(is_transient(
        "fatal: failed to read .git/worktrees/t1-abc/commondir: Success"
    ));
    assert!(!is_transient("fatal: '/tmp/x' already exists"));
    assert!(!is_transient("fatal: invalid reference: nope"));
}

#[test]
fn one_repository_root_maps_to_one_mutex() {
    let repo = TempDir::new().expect("temp repo root");
    let nested = repo.path().join("sub");
    std::fs::create_dir_all(&nested).expect("nested dir");
    let direct = repo_mutex(&nested);
    let round_about = repo_mutex(&nested.join("..").join("sub"));
    assert!(Arc::ptr_eq(&direct, &round_about));
}
