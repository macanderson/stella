//! THE dispatch seam (L-E9). Subagent fan-out goes through exactly one
//! API — [`Fleet::dispatch`] — that claims the task's declared paths
//! (cooperative file locks in the workspace [`Store`], held for the
//! attempt's duration), allocates the task's workspace (shared tree by
//! default; a git worktree when the task opts into isolation), records the attempt in the
//! [`Ledger`], invokes the [`FleetWorker`] port, stamps the resulting commits
//! and parent→child lineage into the ledger, and **meters the child's spend
//! into the parent [`BudgetGuard`]**. No ad-hoc process spawning for agents:
//! hand-rolled per-call-site fan-out is exactly what lost lineage and left
//! budgets uncounted in the TS era (L-E9).
//!
//! [`Fleet::run_wave`] dispatches a set of dependency-ready tasks concurrently
//! (bounded concurrency), and [`Fleet::run_plan`] walks the whole DAG wave by
//! wave. Budget enforcement follows the engine's contract (`stella_core::budget`):
//! when a child's spend pushes the parent guard over an `enforced` limit, the
//! **remaining waves are not launched** — but in-flight siblings are never
//! cancelled mid-run (no mid-tool kill; a running worker settles first).
//!
//! The seam also carries **per-task control**: every dispatched worker
//! receives [`WorkerControls`] (a pause watch + a stop oneshot — the exact
//! channel shapes the deck's sub-sessions use), and the fleet exposes the
//! matching verbs, [`Fleet::pause_task`] / [`Fleet::resume_task`] /
//! [`Fleet::stop_task`]. That closes the "fleet supervisor seam"
//! `command_deck.rs` named as the follow-up: the verbs are reachable by a
//! user, driven by the `stella fleet` live dashboard's `[p]`/`[r]`/`[x]`
//! keys through `stella_tui::FleetControl` and the control pump in
//! `stella-cli/src/fleet_cmd.rs` (#645). Surfacing fleet tasks as deck lanes
//! remains a separate follow-up. Restart is deliberately not a fleet verb —
//! [`Fleet::dispatch`] is re-runnable, so a restart is the caller
//! re-dispatching the same [`Task`]; the fleet keeps no respawn state.
//!
//! [`Isolation`]: crate::plan::Isolation

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use stella_core::{BudgetGuard, BudgetOutcome, Clock};
use stella_store::Store;
use tokio::sync::{oneshot, watch};

use crate::cache_schedule::{RunnableSession, warmest_first};
use crate::git::{GitCli, Worktree, WorktreeError, WorktreeManager};
use crate::ledger::{
    AttemptFinish, AttemptId, AttemptStart, CommitRecord, Ledger, LedgerError, RunRecord,
    lease::{ClaimOutcome, DispatchLease, RenewOutcome},
};
use crate::plan::{Isolation, Plan, PlanError, Task, TaskId};

/// The port a fleet worker implements — the CLI glue later backs this with
/// `stella_core::Engine` / `stella_pipeline`; tests back it with fakes. It
/// receives the task, the workspace root it must operate in (the isolated
/// worktree, or the shared repo root for a [`Isolation::SharedTree`] task),
/// and its [`WorkerControls`], and reports what it did.
///
/// [`Isolation::SharedTree`]: crate::plan::Isolation::SharedTree
#[async_trait]
pub trait FleetWorker: Send + Sync {
    async fn run(
        &self,
        task: &Task,
        workspace_root: &Path,
        controls: WorkerControls,
    ) -> WorkerOutcome;
}

/// The receiver halves of one task's control lines, handed to the
/// [`FleetWorker`] alongside its task — the fleet-side twin of the deck
/// sub-sessions' channels (`stella-cli/src/subsession.rs`): a pause watch
/// and a stop oneshot, driven by [`Fleet::pause_task`] /
/// [`Fleet::resume_task`] / [`Fleet::stop_task`].
///
/// Because the controls ride the [`FleetWorker`] port itself, the
/// capability is structural: workers run no nested spawns today (a
/// worker's own `task_assign` requests are reported, not dispatched — the
/// deck documents that v1 scope), so there is nothing recursive to wire —
/// but any future nested spawn dispatched back through this port would
/// receive its own control lines with no new machinery.
pub struct WorkerControls {
    /// `true` = park at the next safe step boundary until it flips back
    /// (the engine's `TurnGate` boundary — never mid-tool). A closed
    /// channel reads as resumed: a worker must never park forever after
    /// the fleet dropped its handle.
    pub pause: watch::Receiver<bool>,
    /// Fires when [`Fleet::stop_task`] consumes this task's stop line. A
    /// channel closed *without* firing means no stop will ever come —
    /// treat it as "run to completion", never as a stop.
    pub stop: oneshot::Receiver<()>,
}

/// The sender halves the fleet retains for one live task. `stop` is
/// `Option` because firing a oneshot consumes it — a second stop on the
/// same attempt is a stale no-op, exactly the deck sub-session semantics.
struct TaskControlHandle {
    pause: watch::Sender<bool>,
    stop: Option<oneshot::Sender<()>>,
}

/// Holds one attempt's claims (rows in the workspace store's DURABLE
/// `file_locks` table) for exactly the scope it lives in, releasing them on
/// `Drop`.
///
/// A straight-line release after the worker's `.await` is skipped by the two
/// paths that matter most: a [`FleetWorker`] that panics (the port is
/// caller-supplied code and nothing catches unwind), and a dropped dispatch
/// future (cancellation — a `select!` losing the race, Ctrl-C tearing down
/// the runtime, a stream being dropped). A leaked row survives process exit,
/// so every later run in that workspace fails
/// [`FleetError::ClaimConflict`] on that path, naming a run id that no longer
/// exists. Tying release to a scope instead of to reaching a statement makes
/// both paths release.
///
/// Release is best-effort and **never panics**: a failed delete leaves a row
/// that NAMES this holder, so the next claimant's conflict error points
/// straight back here.
struct ClaimGuard<'a> {
    /// `None` when there is nothing to release (a task with no claims), so
    /// the guard is uniform at the call site.
    store: Option<&'a Store>,
    holder: String,
    paths: &'a [String],
}

impl Drop for ClaimGuard<'_> {
    fn drop(&mut self) {
        let Some(store) = self.store else {
            return;
        };
        for path in self.paths {
            let _ = store.release_file_lock(path, &self.holder);
        }
    }
}

/// Holds one attempt's **dispatch claim** (#1136) — the lease that stops a
/// second session starting the same task — for exactly the scope it lives in,
/// releasing it on `Drop`.
///
/// Same reason as [`ClaimGuard`]: release must be tied to a scope, not to
/// reaching a statement, because a panicking worker and a dropped dispatch
/// future both skip the statement. Unlike a file lock, a leaked claim here is
/// self-healing — it expires — but making the next session wait out
/// [`DISPATCH_LEASE_TTL`] for work that settled seconds ago is a bad enough
/// experience to be worth a guard.
///
/// Release is fenced (see [`Ledger::release_dispatch`]), so a guard whose
/// lease was already reclaimed by a rival drops without touching the rival's
/// row. It never panics: a failed release only leaves a row that expires on
/// its own.
struct LeaseGuard<'a> {
    ledger: &'a Mutex<Ledger>,
    lease: DispatchLease,
}

impl Drop for LeaseGuard<'_> {
    fn drop(&mut self) {
        let ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
        let _ = ledger.release_dispatch(&self.lease);
    }
}

/// Keeps one live worker's [`TaskControlHandle`] registered for exactly the
/// scope it lives in, deregistering it on `Drop`.
///
/// Same shape and same reason as [`ClaimGuard`]: an unguarded
/// `remove(&task.id)` after the worker's `.await` is skipped by a panicking
/// worker and by a dropped dispatch future, and the map then grows without
/// bound for a long-lived [`Fleet`]. A stale entry is also still *addressable*
/// whenever any receiver outlives the worker's own return — a worker that
/// handed its pause watch to a background task keeps one alive — so
/// [`Fleet::pause_task`]/[`Fleet::stop_task`] would answer `true` and signal
/// into the settled attempt's lines instead of reporting "no live worker".
struct ControlGuard<'a> {
    controls: &'a Mutex<HashMap<TaskId, TaskControlHandle>>,
    id: TaskId,
}

impl Drop for ControlGuard<'_> {
    fn drop(&mut self) {
        // Recovering a poisoned lock (rather than unwrapping) keeps this
        // infallible: a panicking worker is exactly when it runs.
        self.controls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

/// What a [`FleetWorker`] reports back for one task attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkerOutcome {
    /// USD the worker spent — metered into the parent budget (L-E9) and
    /// recorded in the ledger's per-task spend.
    pub cost_usd: f64,
    /// Commits the worker landed (stamped into the ledger).
    pub commits: Vec<CommitRecord>,
    /// A human summary of the attempt (stored on the attempt row).
    pub summary: String,
    /// Whether the task succeeded.
    pub success: bool,
}

/// Static configuration of a fleet run.
#[derive(Debug, Clone)]
pub struct FleetConfig {
    /// This run's id — the ledger key, the lineage parent, and the prefix of
    /// every claim holder this fleet mints ([`Fleet::dispatch`] claims under
    /// `<run_id>/<task_id>`).
    ///
    /// That last role carries an unenforced contract: `acquire_claims` reaps
    /// **provably-dead** holders before treating a refusal as real (#613), and
    /// `stella_store::holder_pid` decides liveness by parsing the trailing
    /// `-<digits>` of the segment before the `/`. `fleet_cmd` therefore mints
    /// `fleet-<start_ms>-<pid>`, and a run id that ends in some other number —
    /// `release-2024`, `run-7` — is read as that *pid*. If it names a process
    /// that is not running, a sibling losing a claim race reaps this run's own
    /// live claims and both workers proceed to write the same path. A run id
    /// should end in the minting process's pid, or in nothing numeric at all
    /// (an unparsable holder is assumed alive, which is the safe direction).
    pub run_id: String,
    /// The git ref every isolated worktree is branched from.
    pub base_ref: String,
    /// Max tasks dispatched concurrently within one wave (clamped to ≥1).
    /// Bounds the fleet's fan-out so it shares a bounded executor rather than
    /// spawning freely (L-S4).
    pub max_concurrency: usize,
    /// Wall-clock ceiling on one worker attempt. `None` (the default) keeps
    /// the historical behavior: nothing bounds a worker, and a hung one
    /// occupies its `buffer_unordered` slot forever — on a piped or CI run,
    /// with no dashboard `[x]` reachable, that is the whole plan stalled with
    /// no way out. With a limit, expiry fires the task's own stop line (the
    /// same clean cancel the dashboard sends), waits `TASK_STOP_GRACE` for
    /// the worker to settle and report, and only then gives up on it.
    pub task_timeout: Option<std::time::Duration>,
}

impl FleetConfig {
    /// A config for `run_id`, branching isolated worktrees from `base_ref`,
    /// with the default fan-out width of 4 (override with
    /// [`with_max_concurrency`](Self::with_max_concurrency)).
    #[must_use]
    pub fn new(run_id: impl Into<String>, base_ref: impl Into<String>) -> Self {
        Self {
            run_id: run_id.into(),
            base_ref: base_ref.into(),
            max_concurrency: 4,
            task_timeout: None,
        }
    }

    /// Bound one wave's fan-out (builder style). `0` is clamped to `1` at
    /// dispatch time, so a wave always makes progress.
    #[must_use]
    pub fn with_max_concurrency(mut self, n: usize) -> Self {
        self.max_concurrency = n;
        self
    }

    /// Bound one worker attempt's wall-clock (builder style). See
    /// [`FleetConfig::task_timeout`].
    #[must_use]
    pub fn with_task_timeout(mut self, limit: std::time::Duration) -> Self {
        self.task_timeout = Some(limit);
        self
    }
}

/// How long a timed-out worker gets to observe its stop line and settle
/// before dispatch stops waiting for it. Generous on purpose: the stop lands
/// at the worker's next await point and a settling worker reports its real
/// spend and commits — synthesizing a result instead loses both, so the
/// grace is the cheap path and the synthesis the last resort.
const TASK_STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// How long a dispatch claim stays live without a heartbeat (#1136).
///
/// It bounds only the *crash* case — a healthy attempt renews every
/// [`DispatchLease::heartbeat_interval_ms`] (a third of this) for as long as
/// its worker runs, so the TTL never has to cover a task's duration. What it
/// does have to cover is the longest pause a live dispatch can take between
/// beats without being dead, which is why it is minutes rather than seconds:
/// a machine that swaps, a laptop lid, or a `SIGSTOP`ped process should not
/// hand its work to a rival. The other direction costs a human's patience —
/// after a hard kill, this is how long the task looks taken.
const DISPATCH_LEASE_TTL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// The ledger key one fleet task is claimed under (#1136).
///
/// Namespaced because the claim table is shared with every other kind of
/// dispatcher in a workspace — an issue-driven session claims `issue:<n>` —
/// and a bare task id could collide with one of those by accident.
#[must_use]
pub fn dispatch_claim_key(task_id: &str) -> String {
    format!("task:{task_id}")
}

/// The completed record of one dispatched task: what the worker produced, the
/// worktree it ran in (if isolated), the ledger attempt id, and the parent
/// budget's disposition after metering this child's spend.
#[derive(Debug, Clone)]
pub struct TaskHandle {
    pub task_id: TaskId,
    pub attempt_id: AttemptId,
    pub outcome: WorkerOutcome,
    /// `Some` for an isolated task (its worktree), `None` for a shared-tree
    /// task.
    pub worktree: Option<Worktree>,
    /// The parent [`BudgetGuard`]'s outcome after this child's cost was
    /// metered — `AbortTurn` signals `run_plan` to stop launching new waves.
    pub budget: BudgetOutcome,
    /// `Some(reason)` when the attempt's durable ledger close failed AFTER
    /// the worker settled. The in-memory result above is authoritative for
    /// this run — a disk error must not convert a completed worker into a
    /// dispatch failure and skip its dependents — but the `attempts` row is
    /// left open and its commits/spend rows were not written, so anything
    /// reading the ledger later sees an attempt that never closed.
    pub ledger_error: Option<String>,
}

/// The result of running a whole plan.
#[derive(Debug, Default)]
pub struct FleetRunReport {
    /// Every dispatched task's handle, in wave order and sorted by task id
    /// **within** each wave — deterministic for a given plan regardless of
    /// which worker finished first, but not globally id-sorted (a later
    /// wave's `a` still follows an earlier wave's `z`).
    pub handles: Vec<TaskHandle>,
    /// Task ids whose worker reported success — the set that unblocked
    /// dependents.
    pub completed: HashSet<TaskId>,
    /// Tasks whose dispatch itself errored (worktree creation, ledger I/O)
    /// before a worker could produce a handle, with the reason. Counted as
    /// failures.
    pub dispatch_failures: Vec<(TaskId, String)>,
    /// Tasks never attempted because a dependency failed (or the run stopped
    /// on budget before their wave).
    pub skipped: Vec<TaskId>,
    /// `true` if the run stopped early because the parent budget was enforced
    /// and a child pushed it over — the remaining waves were not launched.
    pub budget_aborted: bool,
}

impl FleetRunReport {
    /// Total spend across every dispatched child.
    #[must_use]
    pub fn total_cost_usd(&self) -> f64 {
        self.handles.iter().map(|h| h.outcome.cost_usd).sum()
    }

    /// Whether every task ran and reported success — dispatch failures and
    /// dependency-skipped tasks count against this.
    #[must_use]
    pub fn all_succeeded(&self) -> bool {
        self.dispatch_failures.is_empty()
            && self.skipped.is_empty()
            && self.handles.iter().all(|h| h.outcome.success)
    }
}

/// Typed fleet failures.
#[derive(Debug, thiserror::Error)]
pub enum FleetError {
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    /// A declared path is already claimed — by a sibling in this run or by
    /// another run in the same workspace. The holder is named so the user
    /// can tell a live conflict from a crashed run's leftover claim (the
    /// holder embeds its run id).
    ///
    /// Terminal for that task within the run: [`Fleet::run_plan`] records it
    /// as a dispatch failure and never re-offers the task, even though a
    /// *sibling's* claim is released the moment that sibling settles. Two
    /// independent tasks declaring the same path therefore cost one of them
    /// its work — declare the overlap as a `depends_on` edge instead, so the
    /// second claims in a later wave.
    #[error("task `{task}` claims `{path}`, already claimed by `{holder}`")]
    ClaimConflict {
        task: TaskId,
        path: String,
        holder: String,
    },
    /// Another session already holds the dispatch lease on this task
    /// (#1136), so this fleet did not start a second worker on it. The holder
    /// is named — it is the rival's run id, which embeds its pid — and so is
    /// the instant its lease lapses, because "somebody is on it" and "it has
    /// been stuck since Tuesday" are different problems and only the second
    /// is the user's to act on.
    ///
    /// Terminal for that task within this run, like a claim conflict:
    /// [`Fleet::run_plan`] records it as a dispatch failure rather than
    /// waiting out a lease it cannot bound.
    #[error(
        "task `{task}` is already claimed by `{holder}` (lease expires at {expires_at_ms}ms); \
         another session is working it"
    )]
    DispatchClaimed {
        task: TaskId,
        holder: String,
        expires_at_ms: u64,
    },
    /// The plan declares claims but no claim store is wired
    /// ([`Fleet::with_claim_store`]) — refusing to run unenforced claims
    /// silently.
    #[error("task `{task}` declares file claims but the fleet has no claim store")]
    ClaimsWithoutStore { task: TaskId },
    #[error(transparent)]
    Claims(#[from] stella_store::StoreError),
    /// The aggregate parent budget was already exhausted, so this task was
    /// skipped before launching a worker rather than being run. Reported as a
    /// budget skip, not a task failure.
    #[error("task `{task}` skipped: fleet budget exhausted before it could start")]
    BudgetExhausted { task: TaskId },
}

/// The fleet orchestrator. Owns the worker, the worktree manager, the ledger,
/// the parent budget guard, and the live tasks' control lines (the last
/// three behind `Mutex`es so concurrent wave dispatch serializes their fast
/// synchronous writes — a lock is never held across an `.await`).
/// Optionally owns the workspace [`Store`] whose
/// `file_locks` back task claims: workers are in-process, so this single
/// orchestrator-held store is the multi-process "lock-holder" the store's
/// concurrency contract prescribes.
pub struct Fleet<W: FleetWorker, G: GitCli, C: Clock> {
    worker: W,
    worktrees: WorktreeManager<G>,
    ledger: Mutex<Ledger>,
    budget: Mutex<BudgetGuard>,
    clock: C,
    config: FleetConfig,
    claims: Option<Store>,
    /// Live workers' control lines, keyed by task id — registered by
    /// `dispatch_claimed` for exactly the span its worker runs, so the
    /// control verbs address live workers only.
    controls: Mutex<HashMap<TaskId, TaskControlHandle>>,
    /// Per-task prompt-cache warmth, for [`Fleet::with_cache_warmth`]
    /// (issue #269) — `None` (the default) leaves every ready wave in its
    /// existing order.
    warmth: Option<CacheWarmthLookup>,
}

/// Seconds until a task's session prompt-cache prefix expires, `None` when
/// there is no warm prefix to preserve — the caller-supplied signal
/// [`Fleet::with_cache_warmth`] reorders ready waves against. Boxed so a
/// caller can close over its own store/clock rather than the fleet owning
/// one; `stella-fleet` stays free of `stella-model`/`stella-store`, matching
/// [`crate::cache_schedule`]'s pure, no-I/O contract (this closure is the
/// caller's I/O, called synchronously between waves).
pub type CacheWarmthLookup = Box<dyn Fn(&TaskId) -> Option<u64> + Send + Sync>;

impl<W, G, C> Fleet<W, G, C>
where
    W: FleetWorker,
    G: GitCli,
    C: Clock,
{
    /// Construct a fleet and record its run row. `budget` is the *parent*
    /// guard — every child's spend is metered into it.
    pub fn new(
        worker: W,
        worktrees: WorktreeManager<G>,
        ledger: Ledger,
        budget: BudgetGuard,
        clock: C,
        config: FleetConfig,
    ) -> Result<Self, FleetError> {
        let created_at_ms = clock.now_ms();
        ledger.record_run(&RunRecord {
            id: config.run_id.clone(),
            root_task_count: 0,
            created_at_ms,
        })?;
        Ok(Self {
            worker,
            worktrees,
            ledger: Mutex::new(ledger),
            budget: Mutex::new(budget),
            clock,
            config,
            claims: None,
            controls: Mutex::new(HashMap::new()),
            warmth: None,
        })
    }

    /// Back task claims with the workspace store's `file_locks` table
    /// (builder style). Without this, a plan that declares claims fails
    /// dispatch with [`FleetError::ClaimsWithoutStore`] — claims are never
    /// silently unenforced.
    #[must_use]
    pub fn with_claim_store(mut self, store: Store) -> Self {
        self.claims = Some(store);
        self
    }

    /// Make each ready wave's dispatch order cache-TTL-aware (builder style,
    /// issue #269): before every `run_wave`, `run_plan` sorts that wave
    /// warmest-first (soonest-to-expire prefix first) using `lookup`,
    /// falling back to `ready_tasks`' existing order for any task the lookup
    /// reports no warmth for. This only changes anything when a wave has more
    /// ready tasks than `max_concurrency` (`run_wave`'s `buffer_unordered`
    /// fills its bounded concurrency window in the order it is handed), so a
    /// session about to lose its cached prefix is resumed before a colder one
    /// gets the slot instead. Without this call (the default) every wave
    /// dispatches in `ready_tasks`' order, unchanged from before #269.
    #[must_use]
    pub fn with_cache_warmth(mut self, lookup: CacheWarmthLookup) -> Self {
        self.warmth = Some(lookup);
        self
    }

    /// Reorder one ready wave warmest-first when a warmth lookup is
    /// installed; a single priority class (fleet.rs has no priority concept
    /// of its own — [`crate::cache_schedule::warmest_first`]'s priority
    /// dimension goes unused here, `id` tie-breaking the rest) so this is
    /// purely a warmth sort. Returns `ready` unchanged when no lookup was
    /// installed, so every existing caller keeps today's order.
    fn order_ready_by_warmth<'a>(&self, ready: Vec<&'a Task>) -> Vec<&'a Task> {
        let Some(lookup) = &self.warmth else {
            return ready;
        };
        let sessions: Vec<RunnableSession> = ready
            .iter()
            .map(|t| RunnableSession {
                id: t.id.clone(),
                priority: 0,
                warmth_secs: lookup(&t.id),
            })
            .collect();
        // Index once instead of a linear `find` per session (O(n^2) in wave
        // width). `remove` rather than `get` differs from a `find` only when
        // two ready tasks share an id — which `run_plan`, the sole caller,
        // has already ruled out via `plan.validate()`.
        let mut by_id: HashMap<&str, &'a Task> =
            ready.iter().map(|t| (t.id.as_str(), *t)).collect();
        warmest_first(&sessions)
            .into_iter()
            .filter_map(|s| by_id.remove(s.id.as_str()))
            .collect()
    }

    /// Recover the mutex guard even if a prior holder panicked — the fleet
    /// itself never panics while holding a lock, so poison can only come from
    /// a panicking worker on another task; recovering keeps the fleet
    /// panic-free rather than cascading.
    fn lock_ledger(&self) -> MutexGuard<'_, Ledger> {
        self.ledger.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_budget(&self) -> MutexGuard<'_, BudgetGuard> {
        self.budget.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_controls(&self) -> MutexGuard<'_, HashMap<TaskId, TaskControlHandle>> {
        self.controls.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// THE one dispatch entry point (L-E9). Claims the task's declared
    /// paths, allocates the workspace per the task's isolation, records the
    /// attempt, runs the worker, then meters its cost into the parent budget
    /// and stamps its commits + lineage + spend into the ledger (one
    /// transaction) — returning a [`TaskHandle`].
    pub async fn dispatch(&self, task: &Task) -> Result<TaskHandle, FleetError> {
        // 0a. Aggregate budget gate — enforced BEFORE the worker runs. The
        //     post-run `record_spend` alone only stopped launching the NEXT
        //     wave, so a single wide fan-out wave (the common case: every
        //     positional prompt is an independent task, all in one wave) ran to
        //     completion and could spend far past `--budget`. Checking the
        //     shared parent guard as each task claims a concurrency slot stops
        //     launching further workers once the cap is crossed. Combined with
        //     each child's per-child remaining-budget sub-cap (fleet_cmd), the
        //     aggregate honors the flag.
        //
        //     The gate is a snapshot, not a reservation: up to
        //     `max_concurrency` workers can pass it before any of them records
        //     spend, so the worst-case overshoot is one in-flight window's
        //     cost. That is exactly why `fleet_cmd` divides the cap by the
        //     concurrency width before handing each child its own guard —
        //     a caller wiring this crate directly must do the same or accept
        //     the window.
        if let BudgetOutcome::AbortTurn { .. } = self.lock_budget().evaluate() {
            return Err(FleetError::BudgetExhausted {
                task: task.id.clone(),
            });
        }
        // 0b. Claim the declared paths before anything else exists for this
        //    attempt — a conflict is a plain dispatch failure with nothing
        //    (worktree, ledger rows) to clean up.
        //
        //    Claims are per-attempt and released by the guard's `Drop` when
        //    this scope ends: on success, on a worker failure (its work sits
        //    committed on its branch; holding on would starve dependents and
        //    retries), on a panicking worker, and on this future being
        //    dropped mid-flight. The rows are DURABLE, so a missed release
        //    would outlive the process — see [`ClaimGuard`].
        //
        // 0c. Claim the TASK ITSELF before its paths (#1136). Path claims stop
        //    two workers writing one file; they say nothing about two sessions
        //    doing the same work — a second `stella fleet` in this workspace
        //    used to re-dispatch a task another run was already on, and both
        //    ran to completion. This is the dispatch-level check-and-set, and
        //    it comes first because losing it means doing nothing at all.
        let mut lease = self.acquire_dispatch_lease(task)?;
        let _claims = self.acquire_claims(task)?;
        self.dispatch_leased(task, &mut lease).await
    }

    /// Take this task's dispatch lease, or fail with the rival that holds it.
    ///
    /// Keyed `task:<id>` in the workspace's ledger, held under this run's id
    /// (which embeds the minting pid — see [`FleetConfig::run_id`]), so the
    /// holder in a refusal names a session a human can go look for.
    fn acquire_dispatch_lease(&self, task: &Task) -> Result<LeaseGuard<'_>, FleetError> {
        let now_ms = self.clock.now_ms();
        let outcome = {
            let ledger = self.lock_ledger();
            ledger.claim_dispatch(
                &dispatch_claim_key(&task.id),
                &self.config.run_id,
                now_ms,
                DISPATCH_LEASE_TTL.as_millis().min(u128::from(u64::MAX)) as u64,
            )?
        };
        match outcome {
            ClaimOutcome::Granted(lease) => Ok(LeaseGuard {
                ledger: &self.ledger,
                lease,
            }),
            ClaimOutcome::Held(held) => Err(FleetError::DispatchClaimed {
                task: task.id.clone(),
                holder: held
                    .as_ref()
                    .map_or_else(|| "another session".to_string(), |c| c.owner.clone()),
                expires_at_ms: held.as_ref().map_or(now_ms, |c| c.expires_at_ms),
            }),
        }
    }

    /// [`dispatch`](Self::dispatch) with the task's dispatch lease held,
    /// heartbeating it for as long as the attempt runs.
    ///
    /// The heartbeat rides in this future rather than a spawned task on
    /// purpose: it needs nothing `'static`, it cannot outlive the dispatch it
    /// is proving alive, and a cancelled dispatch stops beating by
    /// construction. `biased` polls the attempt first, so a worker that
    /// settles on the same tick as a beat is never delayed by one.
    ///
    /// **A lost lease does not stop the worker.** If this attempt is
    /// superseded anyway — stalled past its TTL, or a clock that jumped — the
    /// honest options are to kill a worker mid-flight (which this codebase
    /// refuses to do anywhere else: budget aborts wait for a safe boundary,
    /// and a timeout asks the worker to stop rather than dropping it) or to
    /// finish the work that is already paid for. It finishes. The row is not
    /// re-taken behind the rival's back — renewal fails and the guard's
    /// fenced release leaves the rival's claim alone — so the overlap is
    /// visible in the ledger rather than silently repaired.
    async fn dispatch_leased(
        &self,
        task: &Task,
        lease: &mut LeaseGuard<'_>,
    ) -> Result<TaskHandle, FleetError> {
        let beat = std::time::Duration::from_millis(lease.lease.heartbeat_interval_ms());
        let attempt = self.dispatch_claimed(task);
        tokio::pin!(attempt);
        let mut holding = true;
        loop {
            tokio::select! {
                biased;
                settled = &mut attempt => return settled,
                () = tokio::time::sleep(beat), if holding => {
                    let now_ms = self.clock.now_ms();
                    let renewed = {
                        let ledger = self.lock_ledger();
                        ledger.renew_dispatch(&lease.lease, now_ms)
                    };
                    match renewed {
                        Ok(RenewOutcome::Renewed(extended)) => lease.lease = extended,
                        // Superseded, or the ledger is unreadable: either way
                        // this attempt no longer has a lease to prove, so stop
                        // beating and let the work it already paid for finish.
                        Ok(RenewOutcome::Lost(_)) | Err(_) => holding = false,
                    }
                }
            }
        }
    }

    /// [`dispatch`](Self::dispatch) after the task's claims are held.
    async fn dispatch_claimed(&self, task: &Task) -> Result<TaskHandle, FleetError> {
        // 1. Allocate the workspace: a worktree only for tasks that opted
        //    into isolation; the shared tree (default) allocates nothing.
        let worktree = match task.isolation {
            Isolation::Isolated => Some(
                self.worktrees
                    .create(&task.id, &self.config.base_ref)
                    .await?,
            ),
            Isolation::SharedTree => None,
        };
        let workspace_root = match &worktree {
            Some(wt) => wt.path.clone(),
            None => self.worktrees.repo_root().to_path_buf(),
        };
        let branch = worktree
            .as_ref()
            .map(|w| w.branch.clone())
            .unwrap_or_else(|| "(shared-tree)".to_string());

        // 2. Record the task, its lineage, and the opening of this attempt —
        //    before the worker runs, so a crash mid-attempt still leaves a
        //    row naming what was in flight.
        let started_at_ms = self.clock.now_ms();
        let attempt_id = {
            let ledger = self.lock_ledger();
            ledger.record_task(&self.config.run_id, task)?;
            ledger.record_lineage(&self.config.run_id, &task.id, started_at_ms)?;
            ledger.start_attempt(&AttemptStart {
                run_id: self.config.run_id.clone(),
                task_id: task.id.clone(),
                worktree_path: workspace_root.to_string_lossy().into_owned(),
                branch,
                started_at_ms,
            })?
        };

        // 3. Run the worker — the slow part, concurrent across a wave. No
        //    lock is held across the await. Its control lines are registered
        //    first and deregistered the moment it settles, so the control
        //    verbs address exactly the tasks with a live worker. The map is
        //    keyed by task id alone, which is total within a plan —
        //    `run_plan` never has two live attempts of one id. An ad-hoc
        //    caller dispatching a still-live id concurrently gets the honest
        //    limit of that key: the second registration displaces the first,
        //    and whichever attempt settles first deregisters the other's
        //    control lines, leaving a live worker unaddressable by
        //    pause/stop. Re-dispatch a task after its previous attempt
        //    settles, not alongside it.
        let (controls, control_guard) = self.register_controls(task);
        let outcome = match self.config.task_timeout {
            None => self.worker.run(task, &workspace_root, controls).await,
            Some(limit) => {
                let run = self.worker.run(task, &workspace_root, controls);
                tokio::pin!(run);
                match tokio::time::timeout(limit, &mut run).await {
                    Ok(outcome) => outcome,
                    Err(_) => {
                        // Expiry fires the worker's OWN stop line — the same
                        // clean cancel the dashboard's `[x]` sends — then
                        // keeps awaiting: a stopping worker settles at its
                        // next await point and reports its real spend and
                        // commits, which a synthesized result cannot.
                        self.stop_task(&task.id);
                        match tokio::time::timeout(TASK_STOP_GRACE, &mut run).await {
                            Ok(mut outcome) => {
                                outcome.success = false;
                                outcome.summary = format!(
                                    "task timeout after {}s: {}",
                                    limit.as_secs(),
                                    outcome.summary
                                );
                                outcome
                            }
                            // The worker ignored its stop line for the whole
                            // grace. Dropping the future here is the same
                            // abandon seam a cancelled dispatch takes (#803):
                            // the CLI worker's abandon channel closes with it
                            // and its detached thread stops at ITS next await.
                            // The synthesized result is honest about what was
                            // lost — spend and commits were never observed.
                            Err(_) => WorkerOutcome {
                                cost_usd: 0.0,
                                commits: Vec::new(),
                                summary: format!(
                                    "task timeout after {}s: stop was requested and the worker \
                                     did not settle within the {}s grace; its spend and commits \
                                     could not be observed",
                                    limit.as_secs(),
                                    TASK_STOP_GRACE.as_secs()
                                ),
                                success: false,
                            },
                        }
                    }
                }
            }
        };
        // The worker settled — drop its control handle so a later pause or
        // stop for this id reports "no live worker" instead of signalling
        // into the void. Dropping the guard explicitly keeps deregistration
        // at exactly this point; its `Drop` is what also covers the worker
        // panicking or this future being cancelled mid-run.
        drop(control_guard);

        // 4. Meter the child's cost into the parent budget (L-E9), then stamp
        //    the outcome (attempt close + commits + spend) atomically.
        //
        //    The order matters only on the error path, and it matters a lot:
        //    the worker has already spent real money by the time it returns,
        //    so a ledger write that fails must not ALSO drop that spend from
        //    the in-memory gate. Stamping first meant the `?` returned before
        //    `record_spend` ran, and the parent guard then let the rest of the
        //    fan-out run as if this child had cost nothing — a fleet spending
        //    past `--budget` because a disk error, not because of its plan.
        //    Over-counting a child whose ledger row was lost is the safe
        //    direction; under-counting is not.
        let finished_at_ms = self.clock.now_ms();
        let budget = {
            let mut guard = self.lock_budget();
            guard.record_spend(outcome.cost_usd)
        };
        // The stamp can fail; the result must survive it. The worker has
        // already done its work and its spend is already in the gate above —
        // propagating a disk/`SQLITE_BUSY` error here converted a COMPLETED
        // worker into a dispatch failure, which dropped its result, skipped
        // its dependents, and left the attempts row permanently open anyway.
        // The same honesty argument as the spend ordering: losing the durable
        // row is bad, and losing the run's real outcome with it is worse. The
        // failure is carried on the handle, never swallowed.
        let ledger_error = {
            let ledger = self.lock_ledger();
            ledger
                .finish_attempt(&AttemptFinish {
                    attempt_id,
                    run_id: self.config.run_id.clone(),
                    task_id: task.id.clone(),
                    finished_at_ms,
                    success: outcome.success,
                    summary: outcome.summary.clone(),
                    commits: outcome.commits.clone(),
                    cost_usd: outcome.cost_usd,
                    spend_at_ms: finished_at_ms,
                })
                .err()
                .map(|e| e.to_string())
        };

        Ok(TaskHandle {
            task_id: task.id.clone(),
            attempt_id,
            outcome,
            worktree,
            budget,
            ledger_error,
        })
    }

    /// The lock-table identity a task claims under: run-scoped, so a crashed
    /// run's leftover claim is distinguishable from this run's by eye (the
    /// run id embeds its start time and pid). That pid is also what makes the
    /// dead-holder reap in [`acquire_claims`](Self::acquire_claims) decidable
    /// — see the contract on [`FleetConfig::run_id`].
    fn claim_holder(&self, task: &Task) -> String {
        format!("{}/{}", self.config.run_id, task.id)
    }

    /// This fleet's view of who currently holds a task, live or lapsed —
    /// `None` when nobody has claimed it. A dispatcher can ask before
    /// planning; the answer is advisory (only [`Fleet::dispatch`]'s own
    /// check-and-set decides anything), which is exactly why it is a separate,
    /// clearly-named read.
    pub fn dispatch_claim_holder(
        &self,
        task_id: &TaskId,
    ) -> Result<Option<crate::ledger::lease::DispatchClaim>, FleetError> {
        Ok(self
            .lock_ledger()
            .dispatch_claim(&dispatch_claim_key(task_id))?)
    }

    /// Acquire every path in `task.claims` — all-or-nothing: on a conflict
    /// (or a store error) the paths already acquired roll back and the error
    /// names what blocked. Acquisition is re-entrant per holder, so a
    /// duplicate path within one task is harmless.
    ///
    /// Paths are claimed in **sorted order**, not in the order the task
    /// declared them. [`Fleet::run_wave`] dispatches a wave concurrently, so
    /// two siblings that overlap on `{a, b}` and declare them in opposite
    /// orders would otherwise each win one row and each fail on the other —
    /// the classic lock-ordering inversion, and here it costs BOTH tasks
    /// rather than one (acquisition never blocks, so it surfaces as two
    /// spurious [`FleetError::ClaimConflict`]s instead of a hang). A single
    /// global order makes the outcome deterministic: whoever takes the lowest
    /// contended path takes them all, and at most one sibling loses.
    ///
    /// Only the acquisition ORDER is canonicalized; `task.claims` itself is
    /// untouched, and [`ClaimGuard`] still releases over the declared slice —
    /// release order cannot deadlock, since it never waits.
    ///
    /// Returns the [`ClaimGuard`] that owns the release: the caller holds it
    /// for the attempt's duration, and the rows go away when it drops —
    /// including on a panicking worker or a cancelled dispatch.
    fn acquire_claims<'a>(&'a self, task: &'a Task) -> Result<ClaimGuard<'a>, FleetError> {
        if task.claims.is_empty() {
            return Ok(ClaimGuard {
                store: None,
                holder: String::new(),
                paths: &task.claims,
            });
        }
        let Some(store) = &self.claims else {
            return Err(FleetError::ClaimsWithoutStore {
                task: task.id.clone(),
            });
        };
        let holder = self.claim_holder(task);
        // Every holder this run mints is `<run_id>/<task_id>`, so a refusal
        // naming one of those is a live sibling of ours.
        let own_prefix = format!("{}/", self.config.run_id);
        let mut ordered: Vec<&String> = task.claims.iter().collect();
        ordered.sort_unstable();
        for (i, path) in ordered.iter().copied().enumerate() {
            let mut outcome = store.acquire_file_lock(path, &holder);
            // A refusal may be a ghost: a crashed run cannot release its own
            // claims, so its rows outlive it and fail every later run in
            // this workspace with a conflict naming a run id that no longer
            // exists. Reap provably-dead holders once and retry before
            // treating the refusal as real. The deck's `ClaimTap` has done
            // this since it landed; the fleet never did, which is why the
            // stranded-claim reports came from fleet runs.
            //
            // But never reap on a refusal we can already see is a sibling's:
            // the reap is workspace-WIDE and verifiers liveness from the pid
            // embedded in each holder id, so a run id whose trailing digits
            // name a process that is not running (`release-2024`) makes it
            // read THIS run as dead too and release the live claims of every
            // sibling in it — after which two workers write the same path.
            // See the contract on [`FleetConfig::run_id`].
            let refused = matches!(outcome, Ok(false));
            let blocker = if refused {
                store.file_lock_holder(path).ok().flatten()
            } else {
                None
            };
            let sibling_conflict = blocker.is_some_and(|h| h.starts_with(&own_prefix));
            if refused && !sibling_conflict && store.release_file_locks_of_dead_holders().is_ok() {
                outcome = store.acquire_file_lock(path, &holder);
            }
            let failure = match outcome {
                Ok(true) => continue,
                Ok(false) => FleetError::ClaimConflict {
                    task: task.id.clone(),
                    path: path.clone(),
                    holder: store
                        .file_lock_holder(path)
                        .ok()
                        .flatten()
                        // The holder released between the two reads — the
                        // conflict was real when acquisition failed.
                        .unwrap_or_else(|| "(already released)".to_string()),
                },
                Err(e) => FleetError::Claims(e),
            };
            for claimed in &ordered[..i] {
                let _ = store.release_file_lock(claimed, &holder);
            }
            return Err(failure);
        }
        Ok(ClaimGuard {
            store: Some(store),
            holder,
            paths: &task.claims,
        })
    }

    /// Open one task's control lines and register the fleet's sender halves,
    /// returning the worker's [`WorkerControls`] and the [`ControlGuard`]
    /// that owns deregistration — the registration lives exactly as long as
    /// the guard, never as long as reaching a later statement.
    fn register_controls(&self, task: &Task) -> (WorkerControls, ControlGuard<'_>) {
        let (pause_tx, pause_rx) = watch::channel(false);
        let (stop_tx, stop_rx) = oneshot::channel();
        self.lock_controls().insert(
            task.id.clone(),
            TaskControlHandle {
                pause: pause_tx,
                stop: Some(stop_tx),
            },
        );
        (
            WorkerControls {
                pause: pause_rx,
                stop: stop_rx,
            },
            ControlGuard {
                controls: &self.controls,
                id: task.id.clone(),
            },
        )
    }

    /// Dispatch a wave of dependency-ready tasks concurrently, bounded by
    /// `max_concurrency`. Results come back in completion order, each tagged
    /// with its task id (a dispatch `Err` — e.g. a failed `worktree add` —
    /// doesn't carry a handle, and the caller must still know WHICH task it
    /// lost); the caller (`run_plan`) reorders deterministically.
    pub async fn run_wave(&self, tasks: &[&Task]) -> Vec<(TaskId, Result<TaskHandle, FleetError>)> {
        let concurrency = self.config.max_concurrency.max(1);
        stream::iter(tasks.iter().copied())
            .map(|task| async move { (task.id.clone(), self.dispatch(task).await) })
            .buffer_unordered(concurrency)
            .collect()
            .await
    }

    /// Execute an entire plan wave by wave, honoring DAG order. Records the
    /// run and its tasks up front, then repeatedly dispatches the ready set
    /// concurrently until the plan drains — or until the parent budget is
    /// enforced and a child trips it, at which point the remaining waves are
    /// not launched (in-flight siblings still settle; see the module docs).
    pub async fn run_plan(&self, plan: &Plan) -> Result<FleetRunReport, FleetError> {
        plan.validate()?;
        let root_task_count = plan
            .tasks
            .iter()
            .filter(|t| t.depends_on.is_empty())
            .count() as u32;
        {
            let ledger = self.lock_ledger();
            ledger.record_run(&RunRecord {
                id: self.config.run_id.clone(),
                root_task_count,
                created_at_ms: self.clock.now_ms(),
            })?;
            for task in &plan.tasks {
                ledger.record_task(&self.config.run_id, task)?;
            }
        }

        let mut report = FleetRunReport::default();
        // `succeeded` gates dependents: a failed (or dispatch-errored) task
        // must NOT unblock the tasks that depend on it — running them against
        // work that never landed just burns budget on doomed turns. They are
        // reported as `skipped` instead. `attempted` keeps a failed task from
        // being re-offered by `ready_tasks` (it is never in `succeeded`).
        let mut succeeded: HashSet<TaskId> = HashSet::new();
        let mut attempted: HashSet<TaskId> = HashSet::new();

        loop {
            let ready: Vec<&Task> = plan
                .ready_tasks(&succeeded)
                .into_iter()
                .filter(|t| !attempted.contains(&t.id))
                .collect();
            if ready.is_empty() {
                break;
            }
            // Cache-TTL-aware scheduling (#269): a no-op unless
            // `with_cache_warmth` was installed.
            let ready = self.order_ready_by_warmth(ready);

            // A dispatch error (worktree creation, ledger I/O) is recorded as
            // that task's failure — never an early return that would throw
            // away the settled siblings' handles (their spend, commits, and
            // worktrees would otherwise vanish from the report).
            let mut handles: Vec<TaskHandle> = Vec::with_capacity(ready.len());
            let mut wave_tripped_budget = false;
            for (task_id, result) in self.run_wave(&ready).await {
                match result {
                    Ok(handle) => {
                        attempted.insert(task_id.clone());
                        handles.push(handle);
                    }
                    // A pre-dispatch budget skip is NOT a task failure and NOT
                    // an attempt — the worker never ran. Leaving it out of
                    // `attempted` lets the final recompute report it (and every
                    // task behind the aborted wave) as `skipped`. Just trip the
                    // run so no further waves launch.
                    Err(FleetError::BudgetExhausted { .. }) => {
                        wave_tripped_budget = true;
                    }
                    Err(e) => {
                        attempted.insert(task_id.clone());
                        report.dispatch_failures.push((task_id, e.to_string()));
                    }
                }
            }
            // Deterministic order regardless of completion timing.
            handles.sort_by(|a, b| a.task_id.cmp(&b.task_id));
            for handle in handles {
                if handle.outcome.success {
                    succeeded.insert(handle.task_id.clone());
                }
                if matches!(handle.budget, BudgetOutcome::AbortTurn { .. }) {
                    wave_tripped_budget = true;
                }
                report.handles.push(handle);
            }

            if wave_tripped_budget {
                report.budget_aborted = true;
                break; // stop launching remaining waves (never cancel in-flight)
            }
        }

        report.skipped = plan
            .tasks
            .iter()
            .filter(|t| !attempted.contains(&t.id))
            .map(|t| t.id.clone())
            .collect();
        // Dispatch failures accrue in wave-completion order, which varies run
        // to run; sort by task id so the report reads identically for the same
        // plan (matching the deterministic `handles` ordering).
        report.dispatch_failures.sort_by(|a, b| a.0.cmp(&b.0));
        report.completed = succeeded;
        Ok(report)
    }

    // Per-task control: Pause / Resume / Stop

    /// Pause a live worker at its next safe step boundary (the engine's
    /// `TurnGate` — the same boundary budget aborts use, never mid-tool).
    /// `false` when no live worker is registered under `id`; a stale pause
    /// is a no-op, never an error — the deck sub-session semantics.
    ///
    /// Restart is deliberately NOT a fleet verb: [`dispatch`](Self::dispatch)
    /// is already re-runnable, so a restart is the caller re-dispatching the
    /// same [`Task`] — the fleet keeps no respawn state.
    pub fn pause_task(&self, id: &TaskId) -> bool {
        match self.lock_controls().get(id) {
            Some(handle) => handle.pause.send(true).is_ok(),
            None => false,
        }
    }

    /// Resume a paused worker (flip its pause watch back to `false`).
    /// `false` when no live worker is registered under `id`.
    pub fn resume_task(&self, id: &TaskId) -> bool {
        match self.lock_controls().get(id) {
            Some(handle) => handle.pause.send(false).is_ok(),
            None => false,
        }
    }

    /// Fire a live worker's stop line (the CLI worker drops its turn future
    /// at the next await point — the same clean cancel the deck uses).
    /// Consumes the line: `false` when no live worker is registered under
    /// `id`, or when this attempt was already stopped.
    pub fn stop_task(&self, id: &TaskId) -> bool {
        match self.lock_controls().get_mut(id).and_then(|h| h.stop.take()) {
            Some(tx) => tx.send(()).is_ok(),
            None => false,
        }
    }

    // Read-through accessors. Every caller today is a test (no production one).

    /// The parent budget guard's current state (a `Copy` snapshot).
    #[must_use]
    pub fn budget_snapshot(&self) -> BudgetGuard {
        *self.lock_budget()
    }

    /// Total spend recorded in the ledger for this run.
    pub fn ledger_total_spend(&self) -> Result<f64, FleetError> {
        Ok(self.lock_ledger().total_spend(&self.config.run_id)?)
    }

    /// Child task ids recorded as lineage under this run.
    pub fn ledger_lineage_children(&self) -> Result<Vec<String>, FleetError> {
        Ok(self.lock_ledger().lineage_children(&self.config.run_id)?)
    }

    /// Commits recorded for a task in this run.
    pub fn ledger_commits_for_task(&self, task_id: &str) -> Result<Vec<CommitRecord>, FleetError> {
        Ok(self
            .lock_ledger()
            .commits_for_task(&self.config.run_id, task_id)?)
    }
}

#[cfg(test)]
mod tests;
