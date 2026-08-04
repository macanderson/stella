//! The dispatch-side prompt-cache warmth signal for `stella fleet` (#1222) —
//! the lookup [`crate::fleet_cmd`] installs through the fleet's
//! `Fleet::with_cache_warmth` seam, so every ready wave is ordered
//! soonest-to-expire-prefix first before `run_wave` fills its bounded
//! concurrency window.
//!
//! The signal is a projection, not a new record: the fleet ledger already
//! stamps every attempt's finish time (wall-clock since #1222), and the
//! provider's prompt-cache TTL lives in `stella_model::cache_economics`.
//! [`cache_warmth_lookup`] joins the two into "seconds until this task's
//! prefix expires" — the exact shape `stella_fleet::cache_schedule` sorts by.
//! This module is deliberately on the CLI side of the crate boundary:
//! `stella-fleet` stays free of `stella-model`/`stella-store`, keeping the
//! pricing/TTL *policy* here and the scheduling *heuristic* there.

use std::sync::Mutex;

use stella_core::Clock;
use stella_fleet::{CacheWarmthLookup, Fleet, FleetWorker, GitCli, Ledger, TaskId};

use crate::runtime::WallClock;

/// The warmth lookup `stella fleet` installs (#1222): project a task's
/// last-attempt timestamp onto "seconds until its prompt-cache prefix
/// expires" — the signal `Fleet::with_cache_warmth` reorders ready waves by.
///
/// Keyed by what actually varies. On a re-run of a plan the task ids repeat,
/// so a task's own last finished attempt (any run — the ledger's
/// `last_attempt_finish_ms`) is its per-task signal. A task with no history
/// inherits the workspace's latest attempt instead (`latest_attempt_finish_ms`)
/// — the shared byte-stable prefix's last touch, which is uniform across a
/// ready set of first-time tasks and therefore an honest no-op reorder
/// there; it only separates tasks once per-task history exists. An expired
/// prefix, an empty ledger, and a ledger read error all read as `None`
/// (nothing to preserve — `RunnableSession::cold`): warmth is ordering
/// advice, never worth failing a wave over.
///
/// `ledger` is this closure's own connection (the `Fleet` owns the run's
/// writer), behind a `Mutex` because the seam wants `Sync` and rusqlite's
/// connection is not; the lookup runs synchronously between waves, so the
/// lock is uncontended. `now_ms` is injected (wall clock in production) so
/// tests pin the clock. Holding both behind the closure is the point of the
/// `CacheWarmthLookup` seam: the TTL policy (`stella_model`) and the store
/// stay on the CLI side of the crate boundary `stella-fleet` keeps.
pub fn cache_warmth_lookup(
    ledger: Ledger,
    ttl_secs: u64,
    now_ms: Box<dyn Fn() -> u64 + Send + Sync>,
) -> CacheWarmthLookup {
    let ledger = Mutex::new(ledger);
    Box::new(move |task_id: &TaskId| {
        let ledger = ledger.lock().unwrap_or_else(|e| e.into_inner());
        let last_ms = ledger
            .last_attempt_finish_ms(task_id)
            .ok()
            .flatten()
            .or_else(|| ledger.latest_attempt_finish_ms().ok().flatten())?;
        let elapsed_secs = now_ms().saturating_sub(last_ms) / 1_000;
        let warmth = stella_model::CacheWarmth::from_elapsed(elapsed_secs, ttl_secs);
        (!warmth.expired).then_some(warmth.remaining_secs)
    })
}

/// [`cache_warmth_lookup`] wired for production: the provider's catalog TTL
/// and the wall clock, over a fresh connection onto the run's own
/// `fleet.db`. `None` — leave the fleet TTL-blind, exactly today's order —
/// when the provider has no documented eviction window (nothing to schedule
/// around) or the ledger won't open a second connection (warmth is
/// dispatch-order advice, never worth failing the run).
fn provider_warmth_lookup(
    provider_id: &str,
    ledger_path: &std::path::Path,
) -> Option<CacheWarmthLookup> {
    let ttl_secs = stella_model::provider_cache_ttl_secs(provider_id)?;
    let ledger = Ledger::open(ledger_path).ok()?;
    Some(cache_warmth_lookup(
        ledger,
        ttl_secs,
        Box::new(|| WallClock.now_ms()),
    ))
}

/// Install the warmth lookup on a fleet, `run_fleet`'s one call into this
/// module: with [`provider_warmth_lookup`]'s signal when there is one, or
/// the fleet unchanged (today's TTL-blind wave order) when there is not.
/// The lookup's connection is a second one onto the same `fleet.db` the
/// fleet writes — WAL + `busy_timeout` exist for exactly that.
pub fn install<W, G, C>(
    fleet: Fleet<W, G, C>,
    provider_id: &str,
    ledger_path: &std::path::Path,
) -> Fleet<W, G, C>
where
    W: FleetWorker,
    G: GitCli,
    C: Clock,
{
    match provider_warmth_lookup(provider_id, ledger_path) {
        Some(lookup) => fleet.with_cache_warmth(lookup),
        None => fleet,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use stella_fleet::{
        AttemptFinish, AttemptStart, Fleet, FleetConfig, FleetWorker, Plan, RunRecord,
        SystemGitCli, Task, WorkerControls, WorkerOutcome, WorktreeManager,
    };

    use super::*;
    use crate::agent;

    /// Provider cache TTL and re-run wall clock shared by the warmth tests.
    const TTL_SECS: u64 = 300;
    const RERUN_NOW_MS: u64 = 1_700_000_000_000;

    /// Stamp one finished attempt for `task_id` under `run_id`, as a prior
    /// `stella fleet` run would have left it.
    fn seed_attempt(ledger: &Ledger, run_id: &str, task_id: &str, finished_at_ms: u64) {
        let attempt_id = ledger
            .start_attempt(&AttemptStart {
                run_id: run_id.into(),
                task_id: task_id.into(),
                worktree_path: format!("/tmp/{task_id}"),
                branch: "(shared-tree)".into(),
                started_at_ms: finished_at_ms.saturating_sub(1_000),
            })
            .expect("attempt opens");
        ledger
            .finish_attempt(&AttemptFinish {
                attempt_id,
                run_id: run_id.into(),
                task_id: task_id.into(),
                finished_at_ms,
                success: true,
                summary: "ok".into(),
                commits: vec![],
                cost_usd: 0.0,
                spend_at_ms: finished_at_ms,
            })
            .expect("attempt closes");
    }

    /// A prior run whose tasks were last touched `TTL - remaining` seconds
    /// before `RERUN_NOW_MS` — i.e. each has `remaining` seconds of prefix
    /// warmth left when the re-run starts.
    fn seed_prior_run(path: &Path, warmth_shape: &[(&str, u64)]) {
        let ledger = Ledger::open(path).expect("ledger opens");
        ledger
            .record_run(&RunRecord {
                id: "prior-run".into(),
                root_task_count: warmth_shape.len() as u32,
                created_at_ms: RERUN_NOW_MS.saturating_sub(TTL_SECS * 1_000),
            })
            .expect("run row");
        for (task_id, remaining) in warmth_shape {
            let finished_at_ms = RERUN_NOW_MS - (TTL_SECS - remaining) * 1_000;
            seed_attempt(&ledger, "prior-run", task_id, finished_at_ms);
        }
    }

    /// The lookup this module builds (a fresh connection onto `path`, the
    /// production shape), with the clock pinned at `RERUN_NOW_MS`.
    fn pinned_lookup(path: &Path) -> CacheWarmthLookup {
        cache_warmth_lookup(
            Ledger::open(path).expect("warmth ledger opens"),
            TTL_SECS,
            Box::new(|| RERUN_NOW_MS),
        )
    }

    #[test]
    fn warmth_lookup_projects_ledger_history_through_the_provider_ttl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fleet.db");
        {
            let ledger = Ledger::open(&path).expect("ledger opens");
            ledger
                .record_run(&RunRecord {
                    id: "prior-run".into(),
                    root_task_count: 2,
                    created_at_ms: RERUN_NOW_MS.saturating_sub(500_000),
                })
                .expect("run row");
            // "warm" was touched 120s ago (180s of warmth left); "cold"
            // 400s ago — past the 300s TTL, its prefix already evicted.
            seed_attempt(&ledger, "prior-run", "warm", RERUN_NOW_MS - 120_000);
            seed_attempt(&ledger, "prior-run", "cold", RERUN_NOW_MS - 400_000);
        }
        let lookup = pinned_lookup(&path);

        assert_eq!(
            lookup(&"warm".to_string()),
            Some(180),
            "a live prefix projects to its remaining seconds"
        );
        assert_eq!(
            lookup(&"cold".to_string()),
            None,
            "an expired prefix is nothing-to-preserve, not Some(0)"
        );
        // A task with no history of its own inherits the shared prefix's
        // last touch — here the newest finish is "warm"'s, 120s ago.
        assert_eq!(
            lookup(&"first-timer".to_string()),
            Some(180),
            "no per-task history falls back to the workspace's latest attempt"
        );
    }

    #[test]
    fn warmth_lookup_on_an_empty_ledger_is_uniformly_cold() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fleet.db");
        drop(Ledger::open(&path).expect("create the empty ledger"));
        let lookup = pinned_lookup(&path);
        assert_eq!(
            lookup(&"t1".to_string()),
            None,
            "a first-ever run has no prefix anywhere — every task reads cold"
        );
    }

    #[test]
    fn production_wiring_requires_a_documented_ttl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fleet.db");
        // Providers with no documented eviction window install nothing —
        // there is no TTL to schedule around.
        assert!(provider_warmth_lookup("local", &path).is_none());
        // An opt-in cache provider installs a lookup over the ledger.
        assert!(provider_warmth_lookup("anthropic", &path).is_some());
    }

    /// Records the order `run_plan` hands tasks to the worker; with
    /// `max_concurrency: 1` the wave is strictly serial, so this IS the
    /// dispatch order the warmth heuristic chose.
    struct OrderWorker(Arc<Mutex<Vec<String>>>);

    #[async_trait::async_trait]
    impl FleetWorker for OrderWorker {
        async fn run(&self, task: &Task, _root: &Path, _controls: WorkerControls) -> WorkerOutcome {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(task.id.clone());
            WorkerOutcome {
                cost_usd: 0.0,
                commits: vec![],
                summary: String::new(),
                success: true,
            }
        }
    }

    /// A clock frozen at the re-run's start, so the re-run's own ledger
    /// stamps never perturb the seeded warmth arithmetic.
    struct FrozenClock(u64);

    impl Clock for FrozenClock {
        fn now_ms(&self) -> u64 {
            self.0
        }
    }

    /// Re-run `plan` over the seeded ledger at `path` through the real
    /// `Fleet::run_plan` — with the product warmth lookup installed, or
    /// TTL-blind (today's default) — and return the serial dispatch order.
    async fn rerun_dispatch_order(
        path: &Path,
        run_id: &str,
        plan: &Plan,
        lookup: Option<CacheWarmthLookup>,
    ) -> Vec<String> {
        let order = Arc::new(Mutex::new(Vec::new()));
        let dir = path.parent().expect("ledger lives in a dir").to_path_buf();
        let fleet = Fleet::new(
            OrderWorker(order.clone()),
            WorktreeManager::new(SystemGitCli, dir),
            Ledger::open(path).expect("run ledger opens"),
            agent::build_budget_guard(None),
            FrozenClock(RERUN_NOW_MS),
            FleetConfig::new(run_id, "HEAD").with_max_concurrency(1),
        )
        .expect("fleet starts");
        let fleet = match lookup {
            Some(lookup) => fleet.with_cache_warmth(lookup),
            None => fleet,
        };
        let report = fleet.run_plan(plan).await.expect("plan runs");
        assert!(report.all_succeeded());
        let order = order.lock().unwrap_or_else(|e| e.into_inner());
        order.clone()
    }

    /// Issue #1222's acceptance number, measured over the product path: a
    /// re-run of a four-task plan against a real `fleet.db`, one resume slot
    /// per 90s step. Every piece is the shipped one — the ledger file a prior
    /// run left, `cache_warmth_lookup`'s projection, and a real
    /// `Fleet::run_plan` dispatch. TTL-blind (today's order) forfeits 3 of 4
    /// prefixes; warmth-ordered forfeits 2 — the crate heuristic's 2-vs-3,
    /// reproduced through the product signal. Fails without the lookup this
    /// module builds (both arms then dispatch in id order and forfeit 3).
    #[tokio::test]
    async fn a_ttl_aware_re_run_forfeits_fewer_prefixes_than_the_ttl_blind_baseline() {
        const STEP_SECS: u64 = 90;
        // Remaining warmth at re-run start — the 4-session shape the crate's
        // own `expired_rewrites` simulation quantifies as 2-vs-3.
        let warmth_shape: [(&str, u64); 4] = [("s0", 300), ("s1", 60), ("s2", 120), ("s3", 30)];
        let plan = Plan::new(
            warmth_shape
                .iter()
                .map(|(id, _)| Task::new(*id, *id, "p").shared_tree())
                .collect(),
        );
        // A task dispatched in serial slot `i` sat idle `i * STEP_SECS`; at
        // or past its remaining warmth the prefix is gone and the next turn
        // re-writes it (`is_cache_expired_rewrite`'s conservative boundary).
        let forfeited = |order: &[String]| -> usize {
            order
                .iter()
                .enumerate()
                .filter(|(slot, id)| {
                    let remaining = warmth_shape
                        .iter()
                        .find(|(s, _)| s == id)
                        .expect("dispatched task is in the shape")
                        .1;
                    remaining <= *slot as u64 * STEP_SECS
                })
                .count()
        };

        // Each A/B arm re-runs the plan in its own workspace over an
        // identically-seeded prior run — blind must not see warm's rows.
        let blind_dir = tempfile::tempdir().expect("tempdir");
        let blind_db = blind_dir.path().join("fleet.db");
        seed_prior_run(&blind_db, &warmth_shape);
        let blind = rerun_dispatch_order(&blind_db, "rerun-blind", &plan, None).await;

        let warm_dir = tempfile::tempdir().expect("tempdir");
        let warm_db = warm_dir.path().join("fleet.db");
        seed_prior_run(&warm_db, &warmth_shape);
        let warm =
            rerun_dispatch_order(&warm_db, "rerun-warm", &plan, Some(pinned_lookup(&warm_db)))
                .await;

        assert_eq!(
            blind,
            ["s0", "s1", "s2", "s3"],
            "TTL-blind dispatches in `ready_tasks`' id order"
        );
        assert_eq!(
            warm,
            ["s3", "s1", "s2", "s0"],
            "warmth-ordered dispatches soonest-to-expire first"
        );
        let (blind, warm) = (forfeited(&blind), forfeited(&warm));
        assert_eq!(blind, 3, "the TTL-blind re-run forfeits 3 of 4 prefixes");
        assert_eq!(warm, 2, "the warmth-ordered re-run forfeits 2 of 4");
        assert!(
            warm < blind,
            "the product lookup must drop the forfeited-prefix count (warm {warm} < blind {blind})"
        );
    }
}
