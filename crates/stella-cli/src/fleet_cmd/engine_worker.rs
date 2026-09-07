//! The fleet worker: one `Engine::run_turn` per task.
//!
//! Its own file. `fleet_cmd.rs` is at its size ceiling, and this piece
//! stands alone: the dispatch settings, and the bridge from the engine's
//! turn future onto the fleet's worker port. The attempt itself stays next
//! door, in `run_task`.

use std::path::Path;
use std::sync::Arc;

use stella_fleet::{FleetWorker, Task, WorkerControls, WorkerOutcome};
use stella_tui::{FleetMsg, FleetStatus};
use tokio::sync::mpsc;

use super::{run_task, wrapped};
use crate::config::Config;

/// One turn per task: the raw `Engine::run_turn` step loop, in the task's
/// own workspace, with the standard tool set and no terminal to ask at.
pub(super) struct EngineWorker {
    pub(super) cfg: Config,
    /// What one child may spend: `--spend-limit` split across the width, not
    /// the whole cap. Give each child the whole cap and one wave spends it
    /// many times over. The fleet guard holds the true total, and stops
    /// launching once it is crossed.
    pub(super) per_child_budget: Option<f64>,
    /// The fleet run id. With the task id it spells the worker's lock-table
    /// name, `<run>/<task>`. The fleet takes a task's declared claims under
    /// that same name, so a tool write inside the task re-enters its own
    /// claims rather than blocking on them.
    pub(super) run_id: String,
    /// The live grid's channel, set only when `stella fleet` has a terminal.
    /// The worker then says when it starts and how it ends, and `run_task`
    /// tees every event to the grid. `None` is a run with no grid.
    pub(super) dash: Option<mpsc::UnboundedSender<FleetMsg>>,
    /// The plugin every attempt runs under (`--pipeline <name>`), or `None`
    /// for the raw step loop.
    ///
    /// A name, not a bound wrapper. Binding starts nothing, but a bound
    /// [`stella_runtime::wrapper::WrapperDispatch`] cannot cross to the
    /// worker's own thread and cannot be shared by two attempts at once:
    /// each holds one plugin talk at a time. So each attempt binds its own,
    /// in its own tree. `run_fleet` has already proven the name resolves.
    pub(super) wrapper_variant: Option<String>,
    /// `--require-verdict`: the plugin's verdict gates the attempt. It says
    /// nothing without `wrapper_variant`, and is refused before the fan-out
    /// in that case.
    pub(super) require_verdict: bool,
    /// Where an attempt's plugin report waits while the grid owns the
    /// terminal. `run_fleet` prints it once the grid gives the screen back.
    /// A line written onto the grid's screen is wiped by the next frame.
    /// `Some` exactly when `dash` is. A run with no grid prints at once.
    pub(super) held_reports: Option<Arc<wrapped::HeldReports>>,
}

#[async_trait::async_trait]
impl FleetWorker for EngineWorker {
    async fn run(
        &self,
        task: &Task,
        workspace_root: &Path,
        controls: WorkerControls,
    ) -> WorkerOutcome {
        // The engine's turn future is not `Send`. It holds provider futures
        // and the retry jitter across awaits. The fleet's worker port asks
        // for a `Send` future. So each task gets its own thread with its own
        // runtime, and this side awaits the `Send` half of a oneshot. Fleet
        // workers run at the same time, so a thread each is right anyway.
        let cfg = self.cfg.clone();
        let per_child_budget = self.per_child_budget;
        let wrapper_variant = self.wrapper_variant.clone();
        let require_verdict = self.require_verdict;
        let held_reports = self.held_reports.clone();
        let task = task.clone();
        let root = workspace_root.to_path_buf();
        let claim_holder = format!("{}/{}", self.run_id, task.id);
        let task_id = task.id.clone();
        // The worker fills this in the moment it opens an execution. So this
        // side can still read what the attempt spent when the worker dies
        // before it reports.
        let spend = crate::fleet_spend::SpendRecovery::default();

        // Dispatch → the row flips to Running the instant the wave picks it up.
        if let Some(d) = &self.dash {
            let _ = d.send(FleetMsg::Status {
                id: task_id.clone(),
                status: FleetStatus::Running,
            });
        }

        let worker_dash = self.dash.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        // The cancel seam. Drop this future — Ctrl-C, or a `select!` that
        // lost — and stella-fleet's `ClaimGuard` gives the task's file
        // claims back on the same unwind. The worker is a loose thread and
        // would keep writing under claims that are gone. `abandon_tx` is
        // held across the await, so the unwind closes this line first: this
        // future's state drops before the guard declared above it. The
        // worker reads a closed line as a stop.
        let (abandon_tx, abandon_rx) = tokio::sync::oneshot::channel::<()>();
        let worker_spend = spend.clone();
        std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("worker runtime failed to start: {e}"))
                .and_then(|rt| {
                    rt.block_on(run_task(
                        &cfg,
                        per_child_budget,
                        &task,
                        &root,
                        &claim_holder,
                        controls,
                        abandon_rx,
                        worker_dash,
                        worker_spend,
                        wrapper_variant.as_deref(),
                        require_verdict,
                        held_reports.as_deref(),
                    ))
                });
            let _ = tx.send(result);
        });
        let outcome = match rx.await {
            Ok(Ok(outcome)) => outcome,
            // A worker that cannot start at all — no provider, no git — is a
            // failed attempt with a named reason. Never a panic. Never a hang.
            Ok(Err(e)) => {
                crate::fleet_spend::unreported_outcome(&spend, format!("worker error: {e}"))
            }
            Err(_) => crate::fleet_spend::unreported_outcome(
                &spend,
                "worker thread died before reporting".into(),
            ),
        };
        // Only now may the abandon line close. The worker has reported, so
        // closing it signals nothing.
        drop(abandon_tx);
        // The worker's own verdict is the end state. Reading done or failed
        // out of the event stream is a guess.
        if let Some(d) = &self.dash {
            let status = if outcome.success {
                FleetStatus::Done
            } else {
                FleetStatus::Failed
            };
            let _ = d.send(FleetMsg::Status {
                id: task_id.clone(),
                status,
            });
        }
        outcome
    }
}
