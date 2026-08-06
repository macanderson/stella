//! Unit tests for [`super`] — split out to keep `fleet.rs` under the
//! file-size ratchet; a child module, so the dispatch seam's private
//! internals stay reachable via `super::*` (the same shape as
//! `stella-pipeline/src/pipeline/tests.rs`).

use super::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};

use stella_protocol::BudgetMode;

use crate::git::{GitCli, GitError, GitOutput};

// Fakes

/// A monotonically-increasing clock so started_at < finished_at without a
/// real wall clock.
struct SeqClock(AtomicU64);
impl SeqClock {
    fn new() -> Self {
        Self(AtomicU64::new(1))
    }
}
impl Clock for SeqClock {
    fn now_ms(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

/// A `GitCli` that says yes to everything (worktree add etc.) and records
/// calls — no real repo needed for dispatch-seam tests.
struct OkGit {
    calls: Arc<StdMutex<Vec<Vec<String>>>>,
}
impl OkGit {
    fn new() -> Self {
        Self {
            calls: Arc::new(StdMutex::new(Vec::new())),
        }
    }
}
#[async_trait]
impl GitCli for OkGit {
    async fn run(&self, _repo: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(args.iter().map(|s| s.to_string()).collect());
        Ok(GitOutput::ok(""))
    }
}

/// A worker that reports a fixed cost + one commit per task and records
/// the workspace root it was handed.
struct FakeWorker {
    cost_usd: f64,
    success: bool,
    seen_roots: Arc<StdMutex<Vec<(String, PathBuf)>>>,
}
impl FakeWorker {
    fn new(cost_usd: f64) -> Self {
        Self {
            cost_usd,
            success: true,
            seen_roots: Arc::new(StdMutex::new(Vec::new())),
        }
    }
    /// A worker whose every task reports failure — for wave-scheduling
    /// tests that a failed prerequisite must not unblock dependents.
    fn failing(cost_usd: f64) -> Self {
        Self {
            success: false,
            ..Self::new(cost_usd)
        }
    }
}

/// A `GitCli` that fails every `git worktree` invocation (add/remove) with
/// a non-zero exit — the shape of a real "worktree already exists" or
/// permission failure, so `WorktreeManager::create` returns an error.
struct FailGit;
#[async_trait]
impl GitCli for FailGit {
    async fn run(&self, _repo: &Path, args: &[&str]) -> Result<GitOutput, GitError> {
        if args.first() == Some(&"worktree") {
            return Ok(GitOutput::failed(128, "fatal: worktree add failed"));
        }
        Ok(GitOutput::ok(""))
    }
}
#[async_trait]
impl FleetWorker for FakeWorker {
    async fn run(
        &self,
        task: &Task,
        workspace_root: &Path,
        _controls: WorkerControls,
    ) -> WorkerOutcome {
        self.seen_roots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((task.id.clone(), workspace_root.to_path_buf()));
        WorkerOutcome {
            cost_usd: self.cost_usd,
            commits: vec![CommitRecord {
                sha: format!("sha-{}", task.id),
                branch: format!("fleet/{}", task.id),
                task_id: task.id.clone(),
                message: format!("work on {}", task.id),
                timestamp_ms: 1,
            }],
            summary: format!("did {}", task.id),
            success: self.success,
        }
    }
}

fn fleet(
    worker: FakeWorker,
    git: OkGit,
    budget: BudgetGuard,
    config: FleetConfig,
) -> Fleet<FakeWorker, OkGit, SeqClock> {
    let manager = WorktreeManager::new(git, "/repo");
    let ledger = Ledger::open_in_memory().unwrap();
    Fleet::new(worker, manager, ledger, budget, SeqClock::new(), config).unwrap()
}

// dispatch: the seam records everything

#[tokio::test]
async fn dispatch_isolated_task_creates_worktree_and_stamps_ledger() {
    let worker = FakeWorker::new(0.10);
    let seen = worker.seen_roots.clone();
    let f = fleet(
        worker,
        OkGit::new(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        FleetConfig::new("run1", "HEAD"),
    );

    let task = Task::new("t1", "title", "prompt").isolated();
    let handle = f.dispatch(&task).await.unwrap();

    // An isolated task got a worktree, and the worker was handed its path.
    // The branch carries a collision-free hash suffix (`fleet/t1-<hash>`).
    let wt = handle.worktree.expect("isolated task has a worktree");
    assert!(wt.branch.starts_with("fleet/t1-"), "{}", wt.branch);
    let roots = seen.lock().unwrap();
    assert_eq!(roots[0].0, "t1");
    assert_eq!(roots[0].1, wt.path);

    // Ledger: lineage, commit, and spend all recorded.
    assert_eq!(f.ledger_lineage_children().unwrap(), vec!["t1".to_string()]);
    let commits = f.ledger_commits_for_task("t1").unwrap();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].sha, "sha-t1");
    assert!((f.ledger_total_spend().unwrap() - 0.10).abs() < 1e-9);

    // Parent budget metered the child's cost.
    assert!((f.budget_snapshot().session_spent_usd() - 0.10).abs() < 1e-9);
}

#[tokio::test]
async fn dispatch_shared_tree_task_uses_repo_root_and_no_worktree() {
    let worker = FakeWorker::new(0.05);
    let seen = worker.seen_roots.clone();
    let git = OkGit::new();
    let git_calls = git.calls.clone();
    let f = fleet(
        worker,
        git,
        BudgetGuard::new(BudgetMode::Observed, None, None),
        FleetConfig::new("run1", "HEAD"),
    );

    let task = Task::new("t1", "title", "prompt").shared_tree();
    let handle = f.dispatch(&task).await.unwrap();
    assert!(
        handle.worktree.is_none(),
        "shared-tree task has no worktree"
    );
    assert_eq!(seen.lock().unwrap()[0].1, PathBuf::from("/repo"));
    // No `git worktree add` was issued for a shared-tree task.
    assert!(
        !git_calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.first().map(String::as_str) == Some("worktree")),
        "shared-tree dispatch must not create a worktree"
    );
}

// task timeout: a hung worker costs minutes, never the plan

/// A worker that never completes on its own and settles only when its
/// stop line fires — the honoring half of the task-timeout contract.
struct StopHonoringWorker;
#[async_trait]
impl FleetWorker for StopHonoringWorker {
    async fn run(&self, _task: &Task, _root: &Path, controls: WorkerControls) -> WorkerOutcome {
        let WorkerControls { stop, .. } = controls;
        let _ = stop.await;
        WorkerOutcome {
            cost_usd: 0.25,
            commits: Vec::new(),
            summary: "settled after stop".to_string(),
            success: true,
        }
    }
}

/// A worker that ignores every control line — the grace-expiry half.
struct DeafWorker;
#[async_trait]
impl FleetWorker for DeafWorker {
    async fn run(&self, _task: &Task, _root: &Path, _controls: WorkerControls) -> WorkerOutcome {
        std::future::pending::<WorkerOutcome>().await
    }
}

/// Expiry fires the worker's own stop line, and a worker that honors it
/// settles and keeps its REAL spend — the whole reason the timeout stops
/// through the control seam instead of dropping the future outright.
#[tokio::test(start_paused = true)]
async fn a_task_timeout_stops_the_worker_and_keeps_its_observed_spend() {
    let manager = WorktreeManager::new(OkGit::new(), "/repo");
    let ledger = Ledger::open_in_memory().unwrap();
    let f = Fleet::new(
        StopHonoringWorker,
        manager,
        ledger,
        BudgetGuard::new(BudgetMode::Observed, None, None),
        SeqClock::new(),
        FleetConfig::new("run1", "HEAD").with_task_timeout(std::time::Duration::from_secs(300)),
    )
    .unwrap();

    let handle = f
        .dispatch(&Task::new("t1", "title", "prompt").shared_tree())
        .await
        .unwrap();
    assert!(
        !handle.outcome.success,
        "a timed-out attempt must not report success"
    );
    assert!(
        handle
            .outcome
            .summary
            .starts_with("task timeout after 300s:"),
        "the summary names the timeout: {}",
        handle.outcome.summary
    );
    assert!(
        (handle.outcome.cost_usd - 0.25).abs() < 1e-9,
        "the settling worker's real spend survives"
    );
    assert!(
        (f.budget_snapshot().session_spent_usd() - 0.25).abs() < 1e-9,
        "and is metered into the parent gate"
    );
}

/// A worker that ignores its stop line for the whole grace is given up
/// on with an honest synthesized result — a handle, never a hang and
/// never a dispatch error.
#[tokio::test(start_paused = true)]
async fn a_worker_deaf_to_its_stop_line_is_abandoned_after_the_grace() {
    let manager = WorktreeManager::new(OkGit::new(), "/repo");
    let ledger = Ledger::open_in_memory().unwrap();
    let f = Fleet::new(
        DeafWorker,
        manager,
        ledger,
        BudgetGuard::new(BudgetMode::Observed, None, None),
        SeqClock::new(),
        FleetConfig::new("run1", "HEAD").with_task_timeout(std::time::Duration::from_secs(60)),
    )
    .unwrap();

    let handle = f
        .dispatch(&Task::new("t1", "title", "prompt").shared_tree())
        .await
        .unwrap();
    assert!(!handle.outcome.success);
    assert!(
        handle.outcome.summary.contains("could not be observed"),
        "the synthesized result must say what was lost: {}",
        handle.outcome.summary
    );
}

// ledger close failure: the result survives the stamp

/// A ledger that cannot close the attempt must not convert a completed
/// worker into a dispatch failure — the handle survives, carrying the
/// error, and dependents are unblocked by the worker's real success.
#[tokio::test]
async fn a_failed_ledger_close_keeps_the_result_and_carries_the_error() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fleet.db");
    let worker = FakeWorker::new(0.10);
    let manager = WorktreeManager::new(OkGit::new(), "/repo");
    let ledger = Ledger::open(&db).unwrap();
    let f = Fleet::new(
        worker,
        manager,
        ledger,
        BudgetGuard::new(BudgetMode::Observed, None, None),
        SeqClock::new(),
        FleetConfig::new("run1", "HEAD"),
    )
    .unwrap();

    // Break the durable half from a second connection, after the run row
    // exists: the attempt close's spend INSERT now has no table.
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch("DROP TABLE spend")
        .unwrap();

    let handle = f
        .dispatch(&Task::new("t1", "title", "prompt").shared_tree())
        .await
        .expect("a ledger stamp failure is not a dispatch failure");
    assert!(handle.outcome.success, "the worker's result is untouched");
    assert!(
        handle.ledger_error.is_some(),
        "and the lost durable row is named on the handle"
    );
    // The spend still reached the in-memory gate — the direction the
    // spend-before-stamp ordering already guarantees.
    assert!((f.budget_snapshot().session_spent_usd() - 0.10).abs() < 1e-9);
}

// claims: cooperative file locks through the workspace store

fn fleet_with_claims(worker: FakeWorker) -> Fleet<FakeWorker, OkGit, SeqClock> {
    fleet(
        worker,
        OkGit::new(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        FleetConfig::new("run1", "HEAD").with_max_concurrency(2),
    )
    .with_claim_store(Store::in_memory().unwrap())
}

/// A worker that parks until the test grants a permit — keeps its
/// task's attempt (and claims) open so a sibling's acquire genuinely
/// races a HELD claim instead of a released one.
struct GatedWorker {
    gate: Arc<tokio::sync::Semaphore>,
}
#[async_trait]
impl FleetWorker for GatedWorker {
    async fn run(
        &self,
        task: &Task,
        _workspace_root: &Path,
        _controls: WorkerControls,
    ) -> WorkerOutcome {
        if let Ok(permit) = self.gate.acquire().await {
            permit.forget(); // one permit releases exactly one worker
        }
        WorkerOutcome {
            cost_usd: 0.0,
            commits: Vec::new(),
            summary: format!("did {}", task.id),
            success: true,
        }
    }
}

#[tokio::test]
async fn parallel_siblings_claiming_the_same_path_conflict_once() {
    // Two independent tasks in one concurrent wave, both claiming the
    // same path: exactly one wins the claim and runs; the other is a
    // recorded dispatch failure naming the conflict.
    let plan = Plan::new(vec![
        Task::new("t1", "t1", "p").claims(["src/shared.rs"]),
        Task::new("t2", "t2", "p").claims(["src/shared.rs"]),
    ]);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let f = Fleet::new(
        GatedWorker { gate: gate.clone() },
        WorktreeManager::new(OkGit::new(), "/repo"),
        Ledger::open_in_memory().unwrap(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        SeqClock::new(),
        FleetConfig::new("run1", "HEAD").with_max_concurrency(2),
    )
    .unwrap()
    .with_claim_store(Store::in_memory().unwrap());

    // On this current-thread test runtime both dispatches reach their
    // claim before the permits below land: the winner parks in its
    // gated worker still holding the claim, so the loser's acquire is a
    // true mid-attempt conflict.
    let (report, ()) = tokio::join!(f.run_plan(&plan), async {
        gate.add_permits(2);
    });
    let report = report.unwrap();
    assert_eq!(report.handles.len(), 1, "exactly one claimant ran");
    assert!(report.handles[0].outcome.success);
    assert_eq!(report.dispatch_failures.len(), 1);
    let (loser, reason) = &report.dispatch_failures[0];
    assert_ne!(loser, &report.handles[0].task_id);
    assert!(
        reason.contains("claims `src/shared.rs`") && reason.contains("run1/"),
        "the conflict names the path and the winning holder: {reason}"
    );

    // Both the winner's claim and the loser's rollback released: a fresh
    // task can claim the same path (one gate permit is left for it).
    let retry = Task::new("t3", "t3", "p").claims(["src/shared.rs"]);
    f.dispatch(&retry).await.expect("path released after wave");
}

#[tokio::test]
async fn dependent_tasks_reclaim_a_path_released_by_their_prerequisite() {
    // Claims are per-attempt, so a dependent editing the same file
    // acquires it in the next wave.
    let plan = Plan::new(vec![
        Task::new("a", "a", "p").claims(["src/shared.rs"]),
        Task::new("b", "b", "p")
            .depends_on(["a"])
            .claims(["src/shared.rs"]),
    ]);
    let f = fleet_with_claims(FakeWorker::new(0.10));
    let report = f.run_plan(&plan).await.unwrap();
    assert!(report.all_succeeded(), "sequential claimants never clash");
    assert_eq!(report.handles.len(), 2);
}

#[tokio::test]
async fn a_failed_worker_still_releases_its_claims() {
    let f = fleet_with_claims(FakeWorker::failing(0.10));
    let task = Task::new("t1", "t1", "p").claims(["src/a.rs"]);
    let handle = f.dispatch(&task).await.unwrap();
    assert!(!handle.outcome.success);

    // The failed attempt released its claim — a retry can acquire it.
    let retry = Task::new("t2", "t2", "p").claims(["src/a.rs"]);
    f.dispatch(&retry).await.expect("claim released on failure");
}

#[tokio::test]
async fn a_cancelled_dispatch_releases_its_claims_and_control_handle() {
    // Cancellation — a dropped dispatch future — must release the DURABLE
    // claim row and deregister the control handle, exactly like a normal
    // return does. Without the RAII guards both leak: the claim row
    // outlives the process and blocks every later run on that path, and
    // `pause_task` keeps answering `true` for a worker that is gone.
    use futures_util::FutureExt;

    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let f = Fleet::new(
        GatedWorker { gate: gate.clone() },
        WorktreeManager::new(OkGit::new(), "/repo"),
        Ledger::open_in_memory().unwrap(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        SeqClock::new(),
        FleetConfig::new("run1", "HEAD"),
    )
    .unwrap()
    .with_claim_store(Store::in_memory().unwrap());

    let id = "t1".to_string();
    let task = Task::new("t1", "t1", "p").claims(["src/a.rs"]);
    {
        let mut dispatch = Box::pin(f.dispatch(&task));
        // One poll drives dispatch through the claim, the ledger rows and
        // the control registration, and parks it in the gated worker.
        assert!(
            dispatch.as_mut().now_or_never().is_none(),
            "the gated worker parks, so the dispatch is still in flight"
        );
        assert!(f.pause_task(&id), "the parked worker is registered");
    } // the future is dropped here — cancellation mid-attempt

    assert!(
        !f.pause_task(&id),
        "a cancelled attempt leaves no live control handle"
    );
    // The claim released, so a sibling can take the same path (the permit
    // lets its worker settle).
    gate.add_permits(1);
    let sibling = Task::new("t2", "t2", "p").claims(["src/a.rs"]);
    f.dispatch(&sibling)
        .await
        .expect("a cancelled attempt released its claim");
}

#[tokio::test]
async fn a_foreign_claim_fails_dispatch_by_name_and_rolls_back() {
    // A claim held by another LIVE run in the same workspace: dispatch
    // fails naming that holder, and the paths this task had already
    // acquired roll back. The rival's identity carries this process's
    // own pid, so it is provably alive and the dead-holder reap must not
    // touch it — that distinction is the point of
    // `a_dead_holders_stranded_claim_does_not_block_a_later_run` below.
    let store = Store::in_memory().unwrap();
    let live_rival = format!("fleet-{}/t9", std::process::id());
    assert!(store.acquire_file_lock("src/b.rs", &live_rival).unwrap());
    let f = fleet(
        FakeWorker::new(0.10),
        OkGit::new(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        FleetConfig::new("run1", "HEAD"),
    )
    .with_claim_store(store);

    let task = Task::new("t1", "t1", "p").claims(["src/a.rs", "src/b.rs"]);
    match f.dispatch(&task).await {
        Err(FleetError::ClaimConflict { task, path, holder }) => {
            assert_eq!((task.as_str(), path.as_str()), ("t1", "src/b.rs"));
            assert_eq!(holder, live_rival);
        }
        other => panic!("expected a claim conflict, got {other:?}"),
    }
    // src/a.rs was acquired before the conflict and must have rolled
    // back — a sibling can claim it.
    let sibling = Task::new("t2", "t2", "p").claims(["src/a.rs"]);
    f.dispatch(&sibling)
        .await
        .expect("partial claims roll back");
}

/// Claims are acquired in a canonical (sorted) order, not the order the
/// task declared them — the lock-ordering invariant that keeps two
/// concurrent siblings from each winning one path of an overlapping set
/// and then both failing on the other.
///
/// The inversion itself needs two dispatches interleaved mid-acquisition,
/// which no deterministic test can stage. So this pins the property that
/// PREVENTS it, and pins it where the two orders visibly disagree: a rival
/// holds BOTH contended paths, and the task declares them in reverse
/// order. Declaration order reports the conflict on the declared-first
/// path (`src/z.rs`); canonical order reports it on the sorted-first one
/// (`src/a.rs`). Only one of those can be observed, so the assertion
/// cannot pass under the old behaviour.
#[tokio::test]
async fn claims_are_acquired_in_sorted_order_not_declaration_order() {
    let store = Store::in_memory().unwrap();
    let live_rival = format!("fleet-{}/t9", std::process::id());
    assert!(store.acquire_file_lock("src/a.rs", &live_rival).unwrap());
    assert!(store.acquire_file_lock("src/z.rs", &live_rival).unwrap());

    let f = fleet(
        FakeWorker::new(0.10),
        OkGit::new(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        FleetConfig::new("run1", "HEAD"),
    )
    .with_claim_store(store);

    // Declared z-then-a; both are blocked, so whichever is TRIED first is
    // the one the conflict names.
    let task = Task::new("t1", "t1", "p").claims(["src/z.rs", "src/a.rs"]);
    match f.dispatch(&task).await {
        Err(FleetError::ClaimConflict { path, .. }) => assert_eq!(
            path, "src/a.rs",
            "acquisition must follow sorted order, not declaration order"
        ),
        other => panic!("expected a claim conflict, got {other:?}"),
    }
}

/// The rollback half of the canonical order: a task whose sorted-later
/// path conflicts must release the sorted-EARLIER paths it already took,
/// even though those are not a prefix of the declared slice. Getting the
/// rollback slice wrong (`task.claims[..i]` against a reordered `i`) would
/// strand a live row under this run's own holder.
#[tokio::test]
async fn a_conflict_rolls_back_the_paths_taken_before_it_in_sorted_order() {
    let store = Store::in_memory().unwrap();
    let live_rival = format!("fleet-{}/t9", std::process::id());
    // Only the sorted-LAST path is blocked, so `src/a.rs` is acquired
    // first and must roll back. Declared last-first to make the declared
    // prefix (`src/z.rs`) different from the acquired one (`src/a.rs`).
    assert!(store.acquire_file_lock("src/z.rs", &live_rival).unwrap());

    let f = fleet(
        FakeWorker::new(0.10),
        OkGit::new(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        FleetConfig::new("run1", "HEAD"),
    )
    .with_claim_store(store);

    let task = Task::new("t1", "t1", "p").claims(["src/z.rs", "src/a.rs"]);
    assert!(matches!(
        f.dispatch(&task).await,
        Err(FleetError::ClaimConflict { .. })
    ));
    // `src/a.rs` was taken before the conflict and must be free again.
    let sibling = Task::new("t2", "t2", "p").claims(["src/a.rs"]);
    f.dispatch(&sibling)
        .await
        .expect("the sorted-earlier claim must roll back");
}

/// #613 — the escape hatch for claims already stranded in the field.
///
/// A crashed run cannot release its own claims. Before this, those rows
/// outlived it and failed *every* later run in that workspace with a
/// conflict naming a run id that no longer exists, and nothing could
/// clear them. The holder identity ends in the minting pid, so liveness
/// is decidable: a provably-dead holder's claims are reaped and the
/// acquire retried.
#[tokio::test]
async fn a_dead_holders_stranded_claim_does_not_block_a_later_run() {
    let store = Store::in_memory().unwrap();
    // `pid_t::try_from` rejects this outright, so it reads as dead on
    // every platform rather than depending on what pid happens to be
    // free on the test machine.
    let dead_rival = "fleet-1753-4294967294/t9";
    assert!(store.acquire_file_lock("src/b.rs", dead_rival).unwrap());

    let f = fleet(
        FakeWorker::new(0.10),
        OkGit::new(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        FleetConfig::new("run1", "HEAD"),
    )
    .with_claim_store(store);

    let task = Task::new("t1", "t1", "p").claims(["src/a.rs", "src/b.rs"]);
    f.dispatch(&task)
        .await
        .expect("a dead holder's stranded claim must not block a later run");
}

#[tokio::test]
async fn claims_without_a_store_are_a_named_dispatch_failure() {
    // No claim store wired: a task that declares claims must fail loudly
    // (through the same recorded-dispatch-failure path as any other
    // dispatch error), never run with its claims silently unenforced.
    let plan = Plan::new(vec![Task::new("t1", "t1", "p").claims(["src/a.rs"])]);
    let f = fleet(
        FakeWorker::new(0.10),
        OkGit::new(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        FleetConfig::new("run1", "HEAD"),
    );
    let report = f.run_plan(&plan).await.unwrap();
    assert!(report.handles.is_empty());
    assert_eq!(report.dispatch_failures.len(), 1);
    assert!(
        report.dispatch_failures[0].1.contains("no claim store"),
        "{}",
        report.dispatch_failures[0].1
    );
}

// run_plan: waves, lineage, budget metering, abort

#[tokio::test]
async fn run_plan_executes_all_waves_and_meters_total_spend() {
    // a -> {b, c} -> d: two middle tasks run concurrently in one wave.
    let plan = Plan::new(vec![
        Task::new("a", "a", "p"),
        Task::new("b", "b", "p").depends_on(["a"]),
        Task::new("c", "c", "p").depends_on(["a"]),
        Task::new("d", "d", "p").depends_on(["b", "c"]),
    ]);
    let f = fleet(
        FakeWorker::new(0.25),
        OkGit::new(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        FleetConfig::new("run1", "HEAD").with_max_concurrency(2),
    );

    let report = f.run_plan(&plan).await.unwrap();
    assert_eq!(report.completed.len(), 4);
    assert!(report.all_succeeded());
    assert!(!report.budget_aborted);
    assert!((report.total_cost_usd() - 1.0).abs() < 1e-9);
    // Handles come back in deterministic task-id order.
    let order: Vec<&str> = report.handles.iter().map(|h| h.task_id.as_str()).collect();
    assert_eq!(order, vec!["a", "b", "c", "d"]);
    // Every task is stamped as lineage under the run.
    assert_eq!(
        f.ledger_lineage_children().unwrap(),
        vec!["a", "b", "c", "d"]
    );
    assert!((f.ledger_total_spend().unwrap() - 1.0).abs() < 1e-9);
}

#[tokio::test]
async fn enforced_parent_budget_aborts_remaining_waves() {
    // A 4-deep chain a->b->c->d, each costing 0.5, with an enforced $1.00
    // parent limit. Spend after each wave: 0.5, 1.0 (== limit, Continue),
    // 1.5 (> limit → AbortTurn). So c trips it and d never runs.
    let plan = Plan::new(vec![
        Task::new("a", "a", "p"),
        Task::new("b", "b", "p").depends_on(["a"]),
        Task::new("c", "c", "p").depends_on(["b"]),
        Task::new("d", "d", "p").depends_on(["c"]),
    ]);
    let f = fleet(
        FakeWorker::new(0.5),
        OkGit::new(),
        BudgetGuard::new(BudgetMode::Enforced, Some(1.0), None),
        FleetConfig::new("run1", "HEAD"),
    );

    let report = f.run_plan(&plan).await.unwrap();
    assert!(
        report.budget_aborted,
        "the enforced budget must abort the run"
    );
    assert!(
        report.completed.contains("c") && !report.completed.contains("d"),
        "c runs and trips the budget; d must never be dispatched (completed = {:?})",
        report.completed
    );
    assert_eq!(report.completed.len(), 3);
    // d has no attempt row at all — it was never dispatched.
    assert!(f.ledger_commits_for_task("d").unwrap().is_empty());
}

#[tokio::test]
async fn enforced_budget_stops_a_wide_single_wave_fan_out() {
    // THE fleet-budget P1: six INDEPENDENT tasks (no deps) all land in one
    // wave — the common `stella fleet` shape where every positional prompt
    // is its own task. Each costs 0.5 under an enforced $1.00 cap. The old
    // code metered spend only AFTER each worker and only gated the NEXT
    // wave, so this single wave ran all six to completion and spent
    // 6 × 0.5 = $3.00 — 3× the cap. The pre-dispatch gate in `dispatch()`
    // re-checks the shared parent guard as each task claims its slot, so
    // once cumulative spend passes the cap the remaining tasks are skipped.
    //
    // Serialize the wave (max_concurrency = 1) so the trip point is exact:
    // a,b run (spend 0.5, 1.0 == cap → Continue), c runs (spend 1.5 > cap),
    // then d,e,f each evaluate over-cap and are skipped before running.
    let plan = Plan::new(vec![
        Task::new("a", "a", "p"),
        Task::new("b", "b", "p"),
        Task::new("c", "c", "p"),
        Task::new("d", "d", "p"),
        Task::new("e", "e", "p"),
        Task::new("f", "f", "p"),
    ]);
    let f = fleet(
        FakeWorker::new(0.5),
        OkGit::new(),
        BudgetGuard::new(BudgetMode::Enforced, Some(1.0), None),
        FleetConfig::new("run1", "HEAD").with_max_concurrency(1),
    );

    let report = f.run_plan(&plan).await.unwrap();

    assert!(report.budget_aborted, "the wide wave must trip the cap");
    // Only three tasks ran; the fan-out did NOT spend 6 × 0.5.
    assert_eq!(
        report.completed.len(),
        3,
        "fan-out ran past the cap (completed = {:?})",
        report.completed
    );
    // Total ledger spend is bounded near the cap, not N × cap.
    let spent = f.ledger_total_spend().unwrap();
    assert!(
        spent <= 1.5 + 1e-9,
        "aggregate spend {spent} exceeded the bounded trip point"
    );
    // The three un-launched tasks are reported as skipped (not silently
    // dropped, not counted as failures), and never touched the ledger.
    assert_eq!(report.skipped.len(), 3, "skipped = {:?}", report.skipped);
    for id in ["d", "e", "f"] {
        assert!(
            report.skipped.contains(&id.to_string()),
            "{id} should be skipped: {:?}",
            report.skipped
        );
        assert!(
            f.ledger_commits_for_task(id).unwrap().is_empty(),
            "{id} must have no attempt/commit row — it never ran"
        );
    }
}

#[tokio::test]
async fn observed_budget_over_limit_never_aborts_the_run() {
    // Same chain and per-task cost, but `observed` mode: it warns, never
    // aborts, so every wave runs.
    let plan = Plan::new(vec![
        Task::new("a", "a", "p"),
        Task::new("b", "b", "p").depends_on(["a"]),
        Task::new("c", "c", "p").depends_on(["b"]),
    ]);
    let f = fleet(
        FakeWorker::new(0.5),
        OkGit::new(),
        BudgetGuard::new(BudgetMode::Observed, Some(0.4), None),
        FleetConfig::new("run1", "HEAD"),
    );
    let report = f.run_plan(&plan).await.unwrap();
    assert!(!report.budget_aborted);
    assert_eq!(report.completed.len(), 3);
}

#[tokio::test]
async fn a_failed_prerequisite_skips_its_dependents() {
    // a (fails) -> b -> c. a's failure must NOT unblock b, and c (behind
    // b) is skipped in turn — dependents of a failure never run, so no
    // budget is spent on doomed turns.
    let plan = Plan::new(vec![
        Task::new("a", "a", "p"),
        Task::new("b", "b", "p").depends_on(["a"]),
        Task::new("c", "c", "p").depends_on(["b"]),
    ]);
    let f = fleet(
        FakeWorker::failing(0.10),
        OkGit::new(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        FleetConfig::new("run1", "HEAD"),
    );

    let report = f.run_plan(&plan).await.unwrap();
    assert!(
        !report.all_succeeded(),
        "a failed → the run did not succeed"
    );
    assert!(
        !report.completed.contains("a"),
        "a failed, so it is not in the succeeded set"
    );
    // Only a was ever dispatched; b and c were skipped, not attempted.
    assert_eq!(report.handles.len(), 1);
    assert_eq!(report.handles[0].task_id, "a");
    let mut skipped = report.skipped.clone();
    skipped.sort();
    assert_eq!(skipped, vec!["b".to_string(), "c".to_string()]);
}

#[tokio::test]
async fn a_git_worktree_failure_is_a_recorded_dispatch_failure() {
    // Two independent isolated tasks in one wave; git worktree add fails
    // for both. Each is a recorded dispatch failure (not an early return
    // that discards the wave), the run did not succeed, and the failures
    // are reported in deterministic task-id order.
    let plan = Plan::new(vec![
        Task::new("t1", "t1", "p").isolated(),
        Task::new("t2", "t2", "p").isolated(),
    ]);
    let f = Fleet::new(
        FakeWorker::new(0.10),
        WorktreeManager::new(FailGit, "/repo"),
        Ledger::open_in_memory().unwrap(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        SeqClock::new(),
        FleetConfig::new("run1", "HEAD").with_max_concurrency(2),
    )
    .unwrap();

    let report = f.run_plan(&plan).await.unwrap();
    assert!(!report.all_succeeded(), "dispatch failures fail the run");
    assert!(report.handles.is_empty(), "no task produced a handle");
    let failed: Vec<&str> = report
        .dispatch_failures
        .iter()
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(failed, vec!["t1", "t2"], "both failures recorded, sorted");
}

// per-task control: pause / resume / stop

/// A worker that observes its pause line: parks until the fleet flips
/// it `true` (announcing the sighting on `saw_pause`), then until it
/// flips back `false`, recording each value it saw — proof the
/// pause/resume verbs reach the worker through the [`FleetWorker`]
/// port.
struct PauseProbeWorker {
    observed: Arc<StdMutex<Vec<bool>>>,
    /// Fired after the worker observes `true` — the test resumes only
    /// then, so the pause edge can never be overwritten unseen (a
    /// `join!` may poll the control arm before re-polling the worker).
    saw_pause: Arc<tokio::sync::Notify>,
}
#[async_trait]
impl FleetWorker for PauseProbeWorker {
    async fn run(
        &self,
        task: &Task,
        _workspace_root: &Path,
        mut controls: WorkerControls,
    ) -> WorkerOutcome {
        // `wait_for` errors only if the fleet dropped the sender — then
        // nothing is recorded and the assertions below fail loudly.
        if let Ok(v) = controls.pause.wait_for(|paused| *paused).await {
            self.observed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(*v);
            self.saw_pause.notify_one();
        }
        if let Ok(v) = controls.pause.wait_for(|paused| !*paused).await {
            self.observed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(*v);
        }
        WorkerOutcome {
            cost_usd: 0.0,
            commits: Vec::new(),
            summary: format!("did {}", task.id),
            success: true,
        }
    }
}

/// A worker that runs until its stop line fires — proof `stop_task`
/// cancels through the port, and that the line is consumed (a second
/// stop on the same attempt is a stale no-op).
struct StopProbeWorker;
#[async_trait]
impl FleetWorker for StopProbeWorker {
    async fn run(
        &self,
        _task: &Task,
        _workspace_root: &Path,
        controls: WorkerControls,
    ) -> WorkerOutcome {
        let stopped = controls.stop.await.is_ok();
        WorkerOutcome {
            cost_usd: 0.0,
            commits: Vec::new(),
            summary: if stopped { "stopped" } else { "never stopped" }.to_string(),
            // A stopped task must not read as success — it would unblock
            // dependents against work that never landed.
            success: !stopped,
        }
    }
}

fn control_fleet<W: FleetWorker>(worker: W) -> Fleet<W, OkGit, SeqClock> {
    Fleet::new(
        worker,
        WorktreeManager::new(OkGit::new(), "/repo"),
        Ledger::open_in_memory().unwrap(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        SeqClock::new(),
        FleetConfig::new("run1", "HEAD"),
    )
    .unwrap()
}

#[tokio::test]
async fn pause_and_resume_reach_the_worker_through_the_port() {
    let worker = PauseProbeWorker {
        observed: Arc::new(StdMutex::new(Vec::new())),
        saw_pause: Arc::new(tokio::sync::Notify::new()),
    };
    let observed = worker.observed.clone();
    let saw_pause = worker.saw_pause.clone();
    let f = control_fleet(worker);
    let task = Task::new("t1", "t1", "p").shared_tree();
    let id = "t1".to_string();

    // The first `join!` poll drives the dispatch far enough to register
    // the task's controls and park the worker on its pause watch, so
    // `pause_task` addresses a live worker; resuming waits for the
    // worker's own "saw the pause" signal so the `true` edge can never
    // be overwritten before it was observed.
    let (handle, ()) = tokio::join!(f.dispatch(&task), async {
        assert!(f.pause_task(&id), "a live worker accepts pause");
        saw_pause.notified().await;
        assert!(f.resume_task(&id), "a live worker accepts resume");
    });
    assert!(handle.unwrap().outcome.success);
    assert_eq!(
        observed.lock().unwrap().as_slice(),
        &[true, false],
        "the worker observed the pause, then the resume"
    );
}

#[tokio::test]
async fn stop_fires_once_and_a_second_stop_is_a_stale_no_op() {
    let f = control_fleet(StopProbeWorker);
    let task = Task::new("t1", "t1", "p").shared_tree();
    let id = "t1".to_string();

    // The worker parks on its stop line before the verbs run (the
    // current-thread join polls the dispatch first); both stops land
    // while it is still parked, so the second exercises the consumed
    // line — not a deregistered one.
    let (handle, ()) = tokio::join!(f.dispatch(&task), async {
        assert!(f.stop_task(&id), "a live worker accepts the stop");
        assert!(!f.stop_task(&id), "the stop line is consumed");
    });
    let handle = handle.unwrap();
    assert_eq!(handle.outcome.summary, "stopped");
    assert!(
        !handle.outcome.success,
        "a stopped attempt is not a success"
    );
}

#[test]
fn control_verbs_on_an_unknown_task_are_stale_no_ops() {
    let f = control_fleet(FakeWorker::new(0.0));
    let ghost = "ghost".to_string();
    assert!(!f.pause_task(&ghost));
    assert!(!f.resume_task(&ghost));
    assert!(!f.stop_task(&ghost));
}

#[tokio::test]
async fn control_handles_are_removed_after_the_worker_settles() {
    let f = control_fleet(FakeWorker::new(0.0));
    let task = Task::new("t1", "t1", "p").shared_tree();
    f.dispatch(&task).await.unwrap();
    let id = "t1".to_string();
    assert!(!f.pause_task(&id), "a settled task has no live pause line");
    assert!(!f.resume_task(&id));
    assert!(!f.stop_task(&id), "a settled task has no live stop line");
}

// cache-TTL-aware scheduling (#269)

#[test]
fn ready_order_is_unchanged_without_a_warmth_lookup() {
    let f = fleet(
        FakeWorker::new(0.0),
        OkGit::new(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        FleetConfig::new("run1", "HEAD"),
    );
    let (a, b) = (Task::new("a", "a", "p"), Task::new("b", "b", "p"));
    let order = f.order_ready_by_warmth(vec![&a, &b]);
    assert_eq!(
        order.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
}

/// End-to-end proof `run_plan` actually consults the warmth order: three
/// independent (no `depends_on`) tasks land in one ready wave,
/// `max_concurrency: 1` makes `run_wave`'s `buffer_unordered` strictly
/// serial, so `FakeWorker`'s recorded dispatch order IS the warmth order
/// — fails on the pre-#269 `Fleet` (no `with_cache_warmth` to call).
#[tokio::test]
async fn run_plan_dispatches_a_serial_wave_warmest_first() {
    let plan = Plan::new(vec![
        Task::new("cold", "cold", "p"),
        Task::new("hot", "hot", "p"),
        Task::new("warm", "warm", "p"),
    ]);
    let worker = FakeWorker::new(0.0);
    let seen = worker.seen_roots.clone();
    let f = fleet(
        worker,
        OkGit::new(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        FleetConfig::new("run1", "HEAD").with_max_concurrency(1),
    )
    .with_cache_warmth(Box::new(|id: &TaskId| match id.as_str() {
        "cold" => Some(280),
        "hot" => Some(10),
        "warm" => Some(90),
        _ => None,
    }));
    f.run_plan(&plan).await.unwrap();
    let dispatched: Vec<String> = seen
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|(id, _)| id.clone())
        .collect();
    assert_eq!(dispatched, vec!["hot", "warm", "cold"]);
}

// dispatch claims: the lease that stops two sessions taking one task (#1136)

/// A fleet over a real `fleet.db` file, so a second connection can play the
/// rival session that a second `stella fleet` process would be.
fn fleet_on(db: &Path, run_id: &str, worker: FakeWorker) -> Fleet<FakeWorker, OkGit, SeqClock> {
    Fleet::new(
        worker,
        WorktreeManager::new(OkGit::new(), "/repo"),
        Ledger::open(db).unwrap(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        SeqClock::new(),
        FleetConfig::new(run_id, "HEAD"),
    )
    .unwrap()
}

/// **Witness (#1136).** A task another session is holding is not dispatched a
/// second time: the rival's live lease turns it into a named dispatch failure
/// instead of a duplicate worker.
#[tokio::test]
async fn a_task_another_session_holds_is_not_dispatched_twice() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fleet.db");
    let f = fleet_on(&db, "run-a", FakeWorker::new(0.10));

    // The rival session — a second process in this workspace — claims the
    // task first. `SeqClock` starts at 1, so a lease taken at 0 is live for
    // the whole test.
    let rival = Ledger::open(&db).unwrap();
    assert!(matches!(
        rival
            .claim_dispatch(&dispatch_claim_key("t1"), "run-b", 0, 900_000)
            .unwrap(),
        ClaimOutcome::Granted(_)
    ));

    let err = f
        .dispatch(&Task::new("t1", "title", "prompt").shared_tree())
        .await
        .expect_err("a claimed task must not be dispatched again");
    match err {
        FleetError::DispatchClaimed { task, holder, .. } => {
            assert_eq!(task, "t1");
            assert_eq!(holder, "run-b", "the refusal names the session on it");
        }
        other => panic!("expected a dispatch-claim refusal, got {other:?}"),
    }
    // Nothing was started: no attempt row, no spend.
    assert_eq!(f.ledger_total_spend().unwrap(), 0.0);
    assert!(f.ledger_commits_for_task("t1").unwrap().is_empty());
    // And the rival's claim is untouched by the loser.
    assert_eq!(
        f.dispatch_claim_holder(&"t1".to_string())
            .unwrap()
            .unwrap()
            .owner,
        "run-b"
    );
}

/// **Witness (#1136), the expiry half.** A session that died holding a task
/// cannot wedge it: once the lease lapses the next fleet takes it with no
/// reaper, no operator, and no force flag.
#[tokio::test]
async fn a_lapsed_claim_from_a_dead_session_does_not_wedge_the_task() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fleet.db");
    let f = fleet_on(&db, "run-a", FakeWorker::new(0.10));

    // A claim taken with a zero TTL is already expired by the time the
    // fleet's clock reads 1 — exactly the row a SIGKILLed session leaves.
    let dead = Ledger::open(&db).unwrap();
    assert!(matches!(
        dead.claim_dispatch(&dispatch_claim_key("t1"), "run-dead", 0, 0)
            .unwrap(),
        ClaimOutcome::Granted(_)
    ));

    let handle = f
        .dispatch(&Task::new("t1", "title", "prompt").shared_tree())
        .await
        .expect("an expired claim is reclaimable");
    assert!(handle.outcome.success);
    // The attempt released its own lease when it settled, so the key is free
    // again immediately rather than after the TTL.
    assert_eq!(f.dispatch_claim_holder(&"t1".to_string()).unwrap(), None);
}

/// The lease is genuinely held for the attempt's duration — not taken and
/// dropped — so a rival racing a *live* worker loses.
#[tokio::test]
async fn a_live_attempt_holds_its_task_against_a_rival() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fleet.db");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let f = Fleet::new(
        GatedWorker { gate: gate.clone() },
        WorktreeManager::new(OkGit::new(), "/repo"),
        Ledger::open(&db).unwrap(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        SeqClock::new(),
        FleetConfig::new("run-a", "HEAD"),
    )
    .unwrap();
    let rival = Ledger::open(&db).unwrap();

    let task = Task::new("t1", "title", "prompt").shared_tree();
    let (handle, rival_outcome) = tokio::join!(f.dispatch(&task), async {
        // The dispatch parks in its gated worker holding the lease; only
        // then does the rival try, and only then is it released.
        let outcome = rival
            .claim_dispatch(&dispatch_claim_key("t1"), "run-b", 1_000, 900_000)
            .unwrap();
        gate.add_permits(1);
        outcome
    });
    assert!(handle.unwrap().outcome.success);
    match rival_outcome {
        ClaimOutcome::Held(Some(claim)) => assert_eq!(claim.owner, "run-a"),
        other => panic!("a live attempt must hold its task, got {other:?}"),
    }

    // Settled: the same task can be re-dispatched (restart is re-dispatch).
    gate.add_permits(1);
    assert!(f.dispatch(&task).await.unwrap().outcome.success);
}

/// **Witness (#1677).** An attempt whose dispatch lease is reclaimed by a
/// rival mid-run still finishes — the no-mid-flight-kill contract — but the
/// loss no longer vanishes: it is reported on the settled handle, naming the
/// session that took the task over. Fails on the old code, where
/// `TaskHandle` had no field to carry it.
#[tokio::test(start_paused = true)]
async fn a_lost_dispatch_lease_is_reported_on_the_settled_handle() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fleet.db");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let f = Fleet::new(
        GatedWorker { gate: gate.clone() },
        WorktreeManager::new(OkGit::new(), "/repo"),
        Ledger::open(&db).unwrap(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        SeqClock::new(),
        FleetConfig::new("run-a", "HEAD"),
    )
    .unwrap();
    let rival = Ledger::open(&db).unwrap();

    let task = Task::new("t1", "title", "prompt").shared_tree();
    let (handle, ()) = tokio::join!(f.dispatch(&task), async {
        // While the worker is parked on its gate, the rival reclaims the
        // task as if this attempt had stalled past its TTL — a far-future
        // `now_ms` expires the live lease inside the claim statement itself.
        let outcome = rival
            .claim_dispatch(
                &dispatch_claim_key("t1"),
                "run-b",
                u64::from(u32::MAX),
                900_000,
            )
            .unwrap();
        assert!(
            matches!(outcome, ClaimOutcome::Granted(_)),
            "the rival reclaims the expired lease, got {outcome:?}"
        );
        // Paused time auto-advances to the dispatch's heartbeat while every
        // task is parked; sleeping two beats guarantees the renewal has
        // failed against the moved fence before the gate opens.
        tokio::time::sleep(DISPATCH_LEASE_TTL * 2 / 3).await;
        gate.add_permits(1);
    });

    let handle = handle.unwrap();
    assert!(handle.outcome.success, "the worker still finishes");
    let loss = handle
        .lease_loss
        .expect("a lost lease must be reported on the handle");
    assert_eq!(
        loss.taken_by.as_deref(),
        Some("run-b"),
        "the report names the session that took the task over"
    );
}
