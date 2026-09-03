//! The two dispatch-lease knobs, seen from outside the crate.
//!
//! One sets how long a claim stays live with no heartbeat. The other sets
//! how long a dispatch waits for a rival claim to lapse.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use stella_core::{BudgetGuard, Clock};
use stella_fleet::{
    ClaimOutcome, DEFAULT_DISPATCH_LEASE_TTL, Fleet, FleetConfig, FleetError, FleetWorker, GitCli,
    GitError, GitOutput, Ledger, MIN_DISPATCH_LEASE_TTL, Task, WorkerControls, WorkerOutcome,
    WorktreeManager, dispatch_claim_key,
};
use stella_protocol::BudgetMode;

/// The start time every fleet here reads. It never moves, so the lease end
/// times below are exact.
const NOW_MS: u64 = 1_700_000_000_000;

/// A clock that stands still. The ledger only stores and compares the times
/// it is handed, so no test here needs a moving one.
struct StoppedClock;
impl Clock for StoppedClock {
    fn now_ms(&self) -> u64 {
        NOW_MS
    }
}

/// A clock that reads tokio time. A sleep inside the dispatch moves it, so a
/// short lease lapses with no real waiting.
struct VirtualClock {
    origin: tokio::time::Instant,
}
impl VirtualClock {
    fn started_now() -> Self {
        Self {
            origin: tokio::time::Instant::now(),
        }
    }
}
impl Clock for VirtualClock {
    fn now_ms(&self) -> u64 {
        NOW_MS + self.origin.elapsed().as_millis() as u64
    }
}

/// A git that says yes to all. Every task here shares the tree, so the
/// worktree manager needs one but never calls it.
struct OkGit;
#[async_trait]
impl GitCli for OkGit {
    async fn run(&self, _repo: &Path, _args: &[&str]) -> Result<GitOutput, GitError> {
        Ok(GitOutput::ok(""))
    }
}

fn settled(summary: &str) -> WorkerOutcome {
    WorkerOutcome {
        cost_usd: 0.0,
        commits: Vec::new(),
        summary: summary.to_string(),
        success: true,
    }
}

/// A worker that wins, and counts its runs for the test.
struct CountingWorker(Arc<AtomicUsize>);
#[async_trait]
impl FleetWorker for CountingWorker {
    async fn run(&self, _task: &Task, _root: &Path, _controls: WorkerControls) -> WorkerOutcome {
        self.0.fetch_add(1, Ordering::SeqCst);
        settled("worked")
    }
}

/// What the probe worker saw from inside a live attempt.
#[derive(Debug, Clone, Copy)]
struct Probe {
    /// When the lease its own fleet holds runs out.
    expires_at_ms: u64,
    /// True if a rival claiming at `probe_at_ms` won the task.
    rival_won: bool,
}

/// A worker that reads its own fleet's live claim, then lets a rival try to
/// take it. Inside the attempt is the only place the lease can be seen: the
/// guard hands it back as soon as the attempt ends.
struct RivalProbe {
    db: PathBuf,
    probe_at_ms: u64,
    seen: Arc<Mutex<Option<Probe>>>,
}
#[async_trait]
impl FleetWorker for RivalProbe {
    async fn run(&self, task: &Task, _root: &Path, _controls: WorkerControls) -> WorkerOutcome {
        let ledger = Ledger::open(&self.db).unwrap();
        let key = dispatch_claim_key(&task.id);
        let held = ledger
            .dispatch_claim(&key)
            .unwrap()
            .expect("a fleet holds its task's lease for the whole attempt");
        let rival = ledger
            .claim_dispatch(&key, "run-rival", self.probe_at_ms, 60_000)
            .unwrap();
        *self.seen.lock().unwrap() = Some(Probe {
            expires_at_ms: held.expires_at_ms,
            rival_won: matches!(rival, ClaimOutcome::Granted(_)),
        });
        settled("probed")
    }
}

/// A fleet on `db` with the given worker and clock. No claim store: no task
/// here asks for a file lock.
fn fleet<W: FleetWorker, C: Clock>(
    db: &Path,
    run_id: &str,
    worker: W,
    clock: C,
) -> Fleet<W, OkGit, C> {
    Fleet::new(
        worker,
        WorktreeManager::new(OkGit, "/repo"),
        Ledger::open(db).unwrap(),
        BudgetGuard::new(BudgetMode::Observed, None, None),
        clock,
        FleetConfig::new(run_id, "HEAD"),
    )
    .unwrap()
}

/// The one task each test sends.
fn one_task() -> Task {
    Task::new("t1", "title", "prompt").shared_tree()
}

/// **Witness, the tunable half.** The fleet sets its own lease time. With a
/// 60-second lease, a rival one minute in takes the task. With the default
/// lease, the same rival is turned away.
///
/// A fleet that dropped the setting would turn that rival away each time.
#[tokio::test]
async fn a_shorter_lease_ttl_frees_a_stalled_task_sooner() {
    // One minute in. That is inside the default lease and past a 60-second
    // one.
    let probe_at_ms = NOW_MS + 60_000;

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("default.db");
    let seen = Arc::new(Mutex::new(None));
    let default = fleet(
        &db,
        "run-default",
        RivalProbe {
            db: db.clone(),
            probe_at_ms,
            seen: Arc::clone(&seen),
        },
        StoppedClock,
    );
    assert_eq!(default.dispatch_lease_ttl(), DEFAULT_DISPATCH_LEASE_TTL);
    assert!(default.dispatch(&one_task()).await.unwrap().outcome.success);
    let probe = seen.lock().unwrap().expect("the worker probed");
    assert_eq!(
        probe.expires_at_ms,
        NOW_MS + 900_000,
        "the default lease runs for fifteen minutes"
    );
    assert!(
        !probe.rival_won,
        "a rival one minute in must not take a task the default lease still covers"
    );

    let short_dir = tempfile::tempdir().unwrap();
    let short_db = short_dir.path().join("short.db");
    let short_seen = Arc::new(Mutex::new(None));
    let short = fleet(
        &short_db,
        "run-short",
        RivalProbe {
            db: short_db.clone(),
            probe_at_ms,
            seen: Arc::clone(&short_seen),
        },
        StoppedClock,
    )
    .with_dispatch_lease_ttl(Duration::from_secs(60));
    assert_eq!(short.dispatch_lease_ttl(), Duration::from_secs(60));
    assert!(short.dispatch(&one_task()).await.unwrap().outcome.success);
    let probe = short_seen.lock().unwrap().expect("the worker probed");
    assert_eq!(
        probe.expires_at_ms,
        NOW_MS + 60_000,
        "the set lease time is what the claim is written with"
    );
    assert!(
        probe.rival_won,
        "a lapsed claim is up for grabs, and this one lapsed a minute in"
    );
}

/// A lease shorter than the floor is raised to it, and the fleet says so.
/// Under a second, the beat would hit the ledger three times a second, and
/// any real pause would give a live worker's task away.
#[test]
fn a_lease_ttl_under_the_floor_is_raised_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("floor.db");
    let runs = Arc::new(AtomicUsize::new(0));
    let f = fleet(&db, "run-floor", CountingWorker(runs), StoppedClock)
        .with_dispatch_lease_ttl(Duration::ZERO);
    assert_eq!(f.dispatch_lease_ttl(), MIN_DISPATCH_LEASE_TTL);
}

/// **Witness, the wait half.** By default, a task a rival holds fails the
/// dispatch at once. A re-run right behind a finishing sibling loses work it
/// could have waited for. With a wait set, the same fleet takes the task as
/// soon as the rival's lease lapses.
///
/// The two tests share one ledger row. A dispatch that gives up on the
/// first refusal passes the test above and fails this one.
#[tokio::test(start_paused = true)]
async fn a_bounded_wait_takes_the_task_when_a_rivals_lease_lapses() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("wait.db");

    // A rival holds the task for 200ms, like a sibling about to finish.
    let rival = Ledger::open(&db).unwrap();
    assert!(matches!(
        rival
            .claim_dispatch(&dispatch_claim_key("t1"), "run-rival", NOW_MS, 200)
            .unwrap(),
        ClaimOutcome::Granted(_)
    ));

    let impatient_runs = Arc::new(AtomicUsize::new(0));
    let impatient = fleet(
        &db,
        "run-impatient",
        CountingWorker(Arc::clone(&impatient_runs)),
        VirtualClock::started_now(),
    );
    assert_eq!(impatient.dispatch_claim_wait(), Duration::ZERO);
    match impatient.dispatch(&one_task()).await {
        Err(FleetError::DispatchClaimed { holder, .. }) => {
            assert_eq!(holder, "run-rival", "the refusal names the session on it");
        }
        other => panic!("a fleet with no wait must fail on a live claim, got {other:?}"),
    }
    assert_eq!(
        impatient_runs.load(Ordering::SeqCst),
        0,
        "nothing ran: the task was never dispatched"
    );

    let patient_runs = Arc::new(AtomicUsize::new(0));
    let patient = fleet(
        &db,
        "run-patient",
        CountingWorker(Arc::clone(&patient_runs)),
        VirtualClock::started_now(),
    )
    .with_dispatch_claim_wait(Duration::from_secs(5));
    assert_eq!(patient.dispatch_claim_wait(), Duration::from_secs(5));
    let handle = patient
        .dispatch(&one_task())
        .await
        .expect("the wait outlasts the rival's 200ms lease");
    assert!(handle.outcome.success);
    assert_eq!(patient_runs.load(Ordering::SeqCst), 1);
}
