//! `stella fleet` — multi-agent fan-out from the CLI, wired through
//! `stella-fleet`'s one dispatch seam: a DAG of tasks (from positional
//! prompts or a `--plan` file), a git worktree per isolated task, wave
//! scheduling with bounded concurrency, and every attempt/commit/dollar
//! stamped into the SQLite ledger (`.stella/private/fleet.db`). A task that declares
//! `claims` (workspace-relative paths it will touch) holds them as
//! cooperative file locks in `.stella/private/store.db` for the attempt's duration —
//! a path another task (or another run) already claims fails that dispatch
//! by name instead of letting two agents edit the same file.
//!
//! Wave dispatch is **cache-TTL-aware** (#1222): [`crate::fleet_warmth`]
//! projects each task's last-attempt timestamp through the provider's
//! prompt-cache TTL, so a ready wave resumes soonest-to-expire prefixes
//! first — see that module for the signal's honest limits.
//!
//! Each worker is a full Stella engine turn (the raw step-loop) running in
//! its task's workspace with the standard tool registry — headless: no MCP,
//! no custom tools, so a worker can never block on stdin. It is **steered
//! like every other door**, though (#3947): the byte-stable prefix (workspace
//! memories + enforced rules) and the volatile recall block (recalled frames,
//! selected skills, matched context records, today's date) both reach it — see
//! [`worker_recall_block`], which states what each half can offer inside an
//! isolated worktree and why arming the A/B control per worker is correct. The
//! withheld surfaces are the *tool* ones, and they are withheld for a stdin
//! reason rather than a token one; an unattended lane is precisely where the
//! repository's published steering should still apply.
//!
//! The lane steers **out** as well as in (#3956): an attempt mines its own turn
//! like every other door, into the *invocation* root's memory rather than the
//! disposable tree it ran in — see [`mine_attempt_lesson`] for why those two
//! roots differ, what an unattended lane is and is not allowed to teach, and
//! how the extra call stays inside the `--spend-limit` below.
//!
//! The
//! parent `--spend-limit` is enforced twice, per the fleet's contract: each child
//! runs under its own enforced guard, and the fleet stops launching new
//! waves once the metered total crosses the cap (in-flight siblings settle
//! first, never a mid-tool kill).
//!
//! Every worker also honors its `stella_fleet::WorkerControls`: the stop
//! line races the turn (the clean drop-at-await cancel the deck's
//! sub-sessions use) and the pause line gates the raw step-loop at the
//! engine's step boundary via a `TurnGate`. The control verbs
//! (`Fleet::pause_task` / `resume_task` / `stop_task`) are driven from the
//! live dashboard: its `[p]`/`[r]`/`[x]` keys send a
//! `stella_tui::FleetControl` down a channel that this module's control pump
//! applies to the `Fleet` (#645). Surfacing fleet tasks as
//! controllable deck lanes is still the named follow-up.
//!
//! Worktrees are deliberately left in place after the run — the branches
//! (`fleet/<task>`) carry the work product for the user to review and merge.
//! `git worktree list` shows them; the report names each one.
//!
//! With `--watch`, the run ends in the fleet PR/CI monitor
//! (`stella_fleet::Monitor` over the real `gh`): every branch that carries
//! successful work is watched to CI completion as a capped deferred wait,
//! its PR status is reconciled live, and a red branch fails the command.

use std::collections::HashSet;
use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use colored::Colorize;
use stella_core::{Engine, TurnOutcome};
use stella_fleet::{
    CiWatchOutcome, Fleet, FleetConfig, FleetRunReport, FleetWorker, GhCli, Ledger, Monitor,
    MonitorError, Plan, SystemGhCli, SystemGitCli, Task, TaskId, TimeoutReason, WatchConfig,
    WorkerControls, WorkerOutcome, WorktreeManager,
};
use stella_protocol::{AgentEvent, CompletionMessage, PrStatus};
use stella_tools::ToolRegistry;
use stella_tools::hook_runner::HostHookRunner;
use stella_tui::{FleetDashResult, FleetMsg, FleetStatus};
use tokio::sync::{mpsc, oneshot, watch};

use crate::config::Config;
use crate::runtime::{SystemClock, TokioSleeper, WallClock};
use crate::{agent, plain, rules};

/// Cap on the per-task summary line so the report table stays a table.
const SUMMARY_CHARS: usize = 96;

/// Run a fleet: build/load the plan, dispatch it wave by wave, report —
/// then, with `watch`, hold the fleet PR/CI monitor on the branches.
#[allow(clippy::too_many_arguments)] // composition-root wiring; one caller
pub async fn run_fleet(
    cfg: &Config,
    prompts: &[String],
    plan_file: Option<&Path>,
    base_ref: Option<&str>,
    max_concurrency: usize,
    budget_limit: Option<f64>,
    watch: bool,
    task_timeout: Option<std::time::Duration>,
    output_format: crate::OutputFormat,
    pipeline: crate::wrapper_plugin::PipelineChoice<'_>,
) -> Result<(), String> {
    // The whole door is refused under the enterprise process-free authority,
    // whatever `--pipeline` says (`authorize_execution_surface_with` admits
    // `RawOneShot` alone) — so a wrapper plugin's child process can never
    // start inside that boundary through this door, and `stella run`'s own
    // `pipeline.is_raw()` check has no counterpart to grow here.
    crate::enterprise_telemetry::authorize_execution_surface(
        crate::enterprise_telemetry::ExecutionSurface::Fleet,
    )?;
    let root = cfg.workspace_root.clone();
    let plan = load_plan(prompts, plan_file)?;
    plan.validate().map_err(|e| format!("invalid plan: {e}"))?;
    // Resolved once here, before a worktree is cut or a provider is built, for
    // the reason `run_raw_one_shot` resolves before its own paid call: a
    // `--pipeline` naming nothing installed must fail as a typo, not once per
    // task after the fan-out has started. Each worker binds its own process
    // from this same roster and root (`wrapped::bind_for_attempt`) — this
    // resolve is the pre-flight and the one place the roster's notices are
    // printed.
    let wrapper_variant = pipeline.plugin();
    if let Some(variant) = wrapper_variant {
        crate::wrapper_plugin::resolve(&root, variant, &mut |line| eprintln!("  ! {line}"))?;
    }

    // Pin the base to a sha now: "HEAD" would silently drift as shared-tree
    // tasks commit, and every isolated branch should cut from the same base.
    let base_sha = git_stdout(
        &root,
        &["rev-parse", "--verify", base_ref.unwrap_or("HEAD")],
    )
    .await
    .map_err(|e| format!("cannot resolve the fleet base ref: {e}"))?;

    plain::section_header("Stella — fleet");
    println!(
        "  {} task(s) from {}, base {}, ≤{} concurrent\n",
        plan.tasks.len(),
        plan_file
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "the command line".to_string()),
        &base_sha[..12.min(base_sha.len())],
        max_concurrency.max(1),
    );
    for task in &plan.tasks {
        let deps = if task.depends_on.is_empty() {
            String::new()
        } else {
            format!(" (after {})", task.depends_on.join(", "))
        };
        println!("    {} {} — {}{deps}", "·".dimmed(), task.id, task.title);
    }
    println!();

    let ledger_path = stella_store::workspace_private_sqlite_path(&root, "fleet.db")
        .map_err(|e| format!("could not prepare private fleet state: {e}"))?;
    let ledger =
        Ledger::open(&ledger_path).map_err(|e| format!("could not open the fleet ledger: {e}"))?;

    // Millisecond + pid: two runs in the same second (scripted/CI) must not
    // share a ledger run id — `record_run` is INSERT OR REPLACE, so a
    // collision would merge both runs' accounting under one row. A pre-epoch
    // clock is a hard error rather than a silent fallback to a constant (which
    // would reintroduce the very collision this guards against).
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch — cannot mint a unique fleet run id")?
        .as_millis();
    let run_id = format!("fleet-{now_ms}-{}", std::process::id());

    // The live grid takes over the terminal only on a fully interactive TTY
    // (both stdin AND stdout) running the default (Text) output format.
    // `--output-format json|stream-json`, a piped stdout, or a redirected stdin
    // (`… < /dev/null`, some CI wrappers) all keep the headless path untouched —
    // the workers still persist telemetry and the end-of-run report prints as
    // before. Requiring an interactive stdin also means the dashboard's key
    // reader can never immediately hit EOF (which would otherwise spin the draw
    // loop). This is what preserves the machine-readable contract.
    let live = std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && output_format == crate::OutputFormat::Text;
    let mut dash_rx = None;
    let mut done_rx = None;
    let mut done_tx = None;
    let worker_dash = if live {
        let (tx, rx) = mpsc::unbounded_channel::<FleetMsg>();
        let (dt, dr) = oneshot::channel::<()>();
        dash_rx = Some(rx);
        done_rx = Some(dr);
        done_tx = Some(dt);
        Some(tx)
    } else {
        None
    };

    let worker = EngineWorker {
        cfg: cfg.clone(),
        // Divide the aggregate cap across the concurrency width so one wave's
        // in-flight children can't collectively overshoot `--spend-limit`.
        per_child_budget: budget_limit.map(|b| b / max_concurrency.max(1) as f64),
        run_id: run_id.clone(),
        dash: worker_dash,
        wrapper_variant: wrapper_variant.map(str::to_string),
    };
    let fleet = Fleet::new(
        worker,
        // The run id scopes every worktree/branch slug: task ids repeat
        // across runs (`t1`, `t2`, …) and worktrees are kept for review, so
        // an unscoped second run would collide on `git worktree add`.
        WorktreeManager::new(SystemGitCli, root.clone()).with_run_scope(&run_id),
        ledger,
        agent::build_budget_guard(budget_limit),
        // Wall-anchored, NOT `SystemClock`: every stamp this clock feeds is
        // a durable ledger row that must stay comparable across runs — the
        // warmth projection (#1222) reads a PRIOR run's `finished_at_ms`.
        // `SystemClock`'s per-process origin made every run start near zero.
        WallClock,
        {
            let mut config =
                FleetConfig::new(&run_id, &base_sha).with_max_concurrency(max_concurrency.max(1));
            if let Some(limit) = task_timeout {
                config = config.with_task_timeout(limit);
            }
            config
        },
    )
    .map_err(|e| format!("could not start the fleet: {e}"))?;
    // File claims live in the workspace store (`.stella/private/store.db`), opened
    // only when the plan declares any: enforcing claims requires the store
    // (a claim silently unenforced defeats its purpose), but a claim-free
    // run must not grow a new failure mode.
    let fleet = if plan.tasks.iter().any(|t| !t.claims.is_empty()) {
        let store = stella_store::Store::open(&root).map_err(|e| {
            format!("this plan declares file claims but the workspace store cannot open: {e}")
        })?;
        // Crash hygiene at run start, mirroring what the deck has always
        // done (`command_deck.rs`) and the fleet never did — which is why
        // stranded claims surfaced on fleet runs. Dead-holder release first
        // and age second: liveness is exact where age is a heuristic, and
        // `acquire_file_lock` never refreshes `acquired_at`, so the age
        // sweep alone would eventually mistake a long healthy run's own
        // claims for stale ones. Best-effort — a sweep that fails must not
        // stop the run.
        let _ = store.release_file_locks_of_dead_holders();
        let _ = store.prune_stale_file_locks(crate::claims::STALE_CLAIM_MAX_AGE_SECS);
        fleet.with_claim_store(store)
    } else {
        fleet
    };
    // Cache-TTL-aware wave dispatch (#1222) — see `crate::fleet_warmth`.
    let fleet = crate::fleet_warmth::install(fleet, cfg.provider.id, &ledger_path);

    let report = if live {
        // Paint the live grid over the alternate screen while `run_plan` fans
        // the workers out. The dashboard exits when the run returns (the
        // `done` signal) or the user detaches with `q`; either way it restores
        // the terminal before we print the end-of-run report below.
        let seed: Vec<(String, String)> = plan
            .tasks
            .iter()
            .map(|t| (t.id.clone(), t.title.clone()))
            .collect();
        let label = root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("stella")
            .to_string();
        let dash_rx = dash_rx.take().expect("live implies a dashboard channel");
        let done_rx = done_rx.take().expect("live implies a done channel");
        let done_tx = done_tx.take().expect("live implies a done sender");
        let run_and_signal = async {
            let r = fleet.run_plan(&plan).await;
            // Tell the dashboard the run has settled so it exits its loop.
            let _ = done_tx.send(());
            r
        };
        // The supervisor seam (#645): the dashboard sends verbs, this pump
        // applies them to the real `Fleet`. It lives in the same `join!` as the
        // run because `Fleet`'s three control verbs take `&self` and are
        // synchronous — no clone, no `Arc`, no second task. The pump ends when
        // the dashboard drops its sender, which is exactly when the dashboard
        // returns, so the `join!` always completes.
        let (ctl_tx, mut ctl_rx) = mpsc::unbounded_channel::<stella_tui::FleetControl>();
        let control_pump = async {
            while let Some(verb) = ctl_rx.recv().await {
                // Every verb answers `false` for a task with no live worker
                // (already finished, or never dispatched). That is the
                // documented stale-verb no-op, not an error worth surfacing
                // over the alternate screen.
                let _ = match &verb {
                    stella_tui::FleetControl::Pause(id) => fleet.pause_task(id),
                    stella_tui::FleetControl::Resume(id) => fleet.resume_task(id),
                    stella_tui::FleetControl::Stop(id) => fleet.stop_task(id),
                };
            }
        };
        let (run_result, dash_result, ()) = tokio::join!(
            run_and_signal,
            stella_tui::run_fleet_dashboard(label, seed, dash_rx, done_rx, ctl_tx),
            control_pump
        );
        let report = run_result.map_err(|e| format!("fleet run failed: {e}"))?;
        if let Ok(res) = dash_result {
            print_dash_summary(&res);
        }
        report
    } else {
        fleet
            .run_plan(&plan)
            .await
            .map_err(|e| format!("fleet run failed: {e}"))?
    };

    render_report(&plan, &report, &ledger_path);
    if report.budget_aborted {
        return Err(format!(
            "budget cap reached after ${:.4} — remaining waves were not launched",
            report.total_cost_usd()
        ));
    }

    // Post-fanout PR/CI watch (`--watch`): the fleet monitor over the real
    // `gh`. Only branches carrying successful work are watched — failed
    // tasks already fail the run below.
    let mut red_branches: Vec<String> = Vec::new();
    if watch {
        let targets = watch_targets(&report);
        if targets.is_empty() {
            println!(
                "  {}\n",
                "nothing to watch — no successful task landed commits".dimmed()
            );
        } else {
            let config = WatchConfig::default();
            let monitor =
                Monitor::new(SystemGhCli, Box::new(SystemClock::new())).with_config(config);
            println!(
                "  watching CI for {} fleet branch(es) — polling every {}s, wall cap {}m\n",
                targets.len(),
                config.poll_interval_ms / 1_000,
                config.max_total_ms / 60_000,
            );
            for (task_id, branch) in &targets {
                let watched = watch_branch(&monitor, task_id, branch).await;
                render_watch_line(&watched);
                if !watched.is_green() {
                    red_branches.push(watched.branch);
                }
            }
            println!();
        }
    }

    if !report.all_succeeded() {
        return Err("one or more fleet tasks failed — see the report above".to_string());
    }
    if !red_branches.is_empty() {
        return Err(format!(
            "CI is not green for: {} — see the watch report above",
            red_branches.join(", ")
        ));
    }
    Ok(())
}

/// Build the plan: an explicit `--plan` file (JSON or TOML, deserializing
/// straight into `stella_fleet::Plan`), or one independent shared-tree task
/// per positional prompt (`Task::new` shares by default; a worktree per task
/// is a plan-file opt-in).
fn load_plan(prompts: &[String], plan_file: Option<&Path>) -> Result<Plan, String> {
    if let Some(path) = plan_file {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read plan {}: {e}", path.display()))?;
        return match path.extension().and_then(|x| x.to_str()) {
            Some("json") => serde_json::from_str::<Plan>(&raw)
                .map_err(|e| format!("invalid JSON plan {}: {e}", path.display())),
            Some("toml") => toml::from_str::<Plan>(&raw)
                .map_err(|e| format!("invalid TOML plan {}: {e}", path.display())),
            _ => Err(format!(
                "plan file must be .json or .toml, got {}",
                path.display()
            )),
        };
    }
    if prompts.is_empty() {
        return Err("no tasks: pass prompts as arguments or --plan <file>".to_string());
    }
    Ok(Plan::new(
        prompts
            .iter()
            .enumerate()
            .map(|(i, prompt)| {
                let title: String = prompt.chars().take(48).collect();
                Task::new(format!("t{}", i + 1), title, prompt.clone())
            })
            .collect(),
    ))
}

/// One fleet branch's post-fanout verdict: the capped CI watch outcome plus
/// the branch's reconciled PR status. `pr` is `None` when the branch has no
/// PR yet — branches are left for review, so that is a normal state, not an
/// error.
struct BranchWatch {
    task_id: TaskId,
    branch: String,
    ci: Result<CiWatchOutcome, MonitorError>,
    pr: Option<PrStatus>,
}

impl BranchWatch {
    /// Green iff CI completed with a passing overall conclusion — a timeout,
    /// a monitor error, and a failing conclusion are all red.
    fn is_green(&self) -> bool {
        matches!(
            &self.ci,
            Ok(CiWatchOutcome::Completed { conclusion, .. }) if !conclusion.is_failure()
        )
    }
}

/// The branches worth watching after the fan-out: every successful task that
/// landed commits, keyed by the branch its commits actually record (correct
/// for isolated worktrees and shared-tree tasks alike), deduped so a branch
/// shared by several tasks is watched once.
fn watch_targets(report: &FleetRunReport) -> Vec<(TaskId, String)> {
    let mut seen = HashSet::new();
    report
        .handles
        .iter()
        .filter(|h| h.outcome.success)
        .filter_map(|h| {
            let branch = h.outcome.commits.last()?.branch.clone();
            seen.insert(branch.clone())
                .then(|| (h.task_id.clone(), branch))
        })
        .collect()
}

/// Watch one fleet branch: its CI to completion (the monitor's capped
/// deferred wait, L-E4), then a live PR-status reconcile — `gh pr view`
/// resolves a branch name to its PR.
async fn watch_branch<H: GhCli>(monitor: &Monitor<H>, task_id: &str, branch: &str) -> BranchWatch {
    let ci = monitor.watch_ci(branch).await;
    let pr = monitor.pr_status(branch).await.ok();
    BranchWatch {
        task_id: task_id.to_string(),
        branch: branch.to_string(),
        ci,
        pr,
    }
}

/// One report line per watched branch: verdict mark, CI outcome, PR status.
fn render_watch_line(watch: &BranchWatch) {
    let mark = if watch.is_green() {
        "✓".green()
    } else {
        "✗".red()
    };
    let ci = match &watch.ci {
        Ok(CiWatchOutcome::Completed {
            conclusion,
            summary,
        }) => {
            let verdict = if conclusion.is_failure() {
                "red"
            } else {
                "green"
            };
            format!("CI {verdict} — {summary}")
        }
        Ok(CiWatchOutcome::TimedOut {
            reason,
            last_observed,
            waited_ms,
        }) => {
            let reason = match reason {
                TimeoutReason::CumulativeCap => "cumulative cap",
                TimeoutReason::Stalled => "stalled",
                TimeoutReason::NoRunsStarted => "no CI runs started",
            };
            format!(
                "CI watch timed out ({reason}) after {}m — last: {last_observed}",
                waited_ms / 60_000
            )
        }
        Err(e) => format!("CI watch failed: {e}"),
    };
    let pr = match watch.pr {
        Some(PrStatus::Draft) => "PR draft",
        Some(PrStatus::Open) => "PR open",
        Some(PrStatus::Merged) => "PR merged",
        Some(PrStatus::Closed) => "PR closed",
        None => "no PR",
    };
    println!(
        "  {mark} {} {} — {ci} · {pr}",
        watch.task_id.bold(),
        watch.branch.bright_magenta()
    );
}

/// The engine-backed [`FleetWorker`]: one turn per task — the raw
/// `Engine::run_turn` step-loop — in the task's own workspace, with the
/// standard (headless) tool registry.
struct EngineWorker {
    cfg: Config,
    /// Per-child spend cap. Derived as `--spend-limit / max_concurrency` (not
    /// the full `--spend-limit`), so a wave of concurrent children can't each spend the
    /// whole cap and blow the aggregate — the parent fleet guard then enforces
    /// the true total, stopping further launches once it is crossed.
    per_child_budget: Option<f64>,
    /// The fleet run id — combined with the task id it forms the worker's
    /// lock-table identity (`<run>/<task>`), the SAME holder string the
    /// fleet's declared-claim acquisition uses, so a task's tool-level
    /// claim-on-first-write is re-entrant with its declared claims.
    run_id: String,
    /// The live-dashboard channel, present only when `stella fleet` runs on an
    /// interactive TTY. When set, the worker announces its lifecycle
    /// (Running → Done/Failed) and its `run_task` tees every `AgentEvent` to
    /// the grid. `None` keeps the headless path untouched.
    dash: Option<mpsc::UnboundedSender<FleetMsg>>,
    /// The installed wrapper plugin every attempt runs under
    /// (`--pipeline <variant>`, #3695), or `None` for the raw step-loop.
    ///
    /// The variant *name* rather than a bound wrapper: binding starts nothing,
    /// but a [`stella_runtime::wrapper::WrapperDispatch`] is neither `Send` to
    /// the worker's own OS thread nor shareable across concurrent attempts —
    /// each holds one plugin conversation at a time. So each attempt binds its
    /// own from this name, in its own tree, and `run_fleet`'s pre-flight has
    /// already proven the name resolves.
    wrapper_variant: Option<String>,
}

#[async_trait::async_trait]
impl FleetWorker for EngineWorker {
    async fn run(
        &self,
        task: &Task,
        workspace_root: &Path,
        controls: WorkerControls,
    ) -> WorkerOutcome {
        // The engine's turn future is deliberately not `Send` (it holds
        // provider futures and the retry jitter RNG across awaits), but the
        // fleet's worker port requires a `Send` future. Bridge the two by
        // giving each task its own OS thread with a current-thread runtime —
        // fleet workers are genuinely parallel — and awaiting the `Send`
        // half of a oneshot from the async side.
        let cfg = self.cfg.clone();
        let per_child_budget = self.per_child_budget;
        let wrapper_variant = self.wrapper_variant.clone();
        let task = task.clone();
        let root = workspace_root.to_path_buf();
        let claim_holder = format!("{}/{}", self.run_id, task.id);
        let task_id = task.id.clone();
        // Published by the worker the moment it opens an execution, so this
        // side can still read the attempt's real spend if the worker never
        // reports one (#1216).
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
        // The cancellation seam (#803): if this dispatch future is dropped
        // (Ctrl-C, a `select!` losing the race), stella-fleet's `ClaimGuard`
        // releases the task's durable file claims on the same unwind — but
        // the worker below is a detached OS thread that would keep writing
        // under claims it no longer holds. `abandon_tx` is held across the
        // await, so that unwind closes this channel first (this future's
        // state drops before the dispatch frame's earlier-declared guard),
        // and `stop_or_abandoned` reads the closure as stop.
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
                    ))
                });
            let _ = tx.send(result);
        });
        let outcome = match rx.await {
            Ok(Ok(outcome)) => outcome,
            // A worker that can't even start (provider, git) is a failed
            // attempt with a named reason — never a panic, never a hang.
            Ok(Err(e)) => {
                crate::fleet_spend::unreported_outcome(&spend, format!("worker error: {e}"))
            }
            Err(_) => crate::fleet_spend::unreported_outcome(
                &spend,
                "worker thread died before reporting".into(),
            ),
        };
        // Only now may the abandon line close: the worker has already
        // reported, so the closure signals nothing.
        drop(abandon_tx);
        // The worker's own verdict is the authoritative terminal state — more
        // reliable than inferring done/failed from the event stream.
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

/// `stella_core::ports::TurnGate` over the task's pause line: the worker's
/// turn parks at its next step boundary while a supervisor holds the watch
/// at `true` (`Fleet::pause_task`) and continues on `false`
/// (`Fleet::resume_task`). A dropped sender (the fleet settled this task's
/// controls) reads as resumed — a worker must never park forever on
/// teardown.
///
/// A deliberate small twin of `subsession.rs`'s private `WatchGate` (the
/// deck's sub-session gate): the two adapters sit on opposite sides of the
/// deck/fleet boundary and share only this trivial shape, so a co-located
/// duplicate reads better than a shared item would.
struct WatchGate(watch::Receiver<bool>);

#[async_trait::async_trait]
impl stella_core::ports::TurnGate for WatchGate {
    async fn wait_if_paused(&self) {
        let mut rx = self.0.clone();
        while *rx.borrow() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

/// The composite stop line for one worker attempt. Resolves when the
/// supervisor signals an explicit stop (`Fleet::stop_task`) — or when the
/// dispatch future was dropped and its `abandon_tx` went with it, which
/// means stella-fleet's `ClaimGuard` is releasing this task's durable file
/// claims on that same unwind and the worker must stop writing (#803).
///
/// The two closed-channel cases deliberately read in opposite directions.
/// A dropped *supervisor* sender means "the fleet settled this handle — no
/// one will ever signal", so that line parks forever and the work wins
/// (mirroring `subsession.rs::run_worker`). A dropped *abandon* sender is
/// the signal itself: nothing ever sends on it, its closure is the drop of
/// the dispatch frame that owned the claims.
async fn stop_or_abandoned(
    stop: tokio::sync::oneshot::Receiver<()>,
    abandoned: tokio::sync::oneshot::Receiver<()>,
) {
    let explicit_stop = async {
        if stop.await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    let dispatch_dropped = async {
        let _ = abandoned.await;
    };
    tokio::select! {
        _ = explicit_stop => {}
        _ = dispatch_dropped => {}
    }
}

/// The sender a **raw** fleet worker hands the engine.
///
/// `StageKind` is the run owner's vocabulary — the engine emits no boundary of
/// its own (#3416) — and this lane is the owner. The opener is sent outright at
/// the turn's start; the closer cannot be, because it must arrive *ahead of*
/// the `TurnComplete` it annotates, and that event is emitted from inside the
/// turn. So it rides a combinator instead of a send after the race
/// ([`stella_core::EventSender::pairing_stage_complete`], which also holds the
/// completed-only rule: a cancelled or aborted worker gets no boundary claiming
/// its run finished).
///
/// A named seam rather than an inline `.pairing_stage_complete()` so the wiring
/// has something to test. It went missing once already: the lane emitted an
/// opener and no closer, framing half a turn, and nothing failed — the fleet
/// renders lanes from the ledger rather than a stage HUD, so the asymmetry was
/// invisible to every consumer that exists today (#3428).
fn worker_event_sender(tx: &mpsc::UnboundedSender<AgentEvent>) -> stella_core::EventSender {
    stella_core::EventSender::new(tx.clone()).pairing_stage_complete()
}

/// The volatile steering block for one fleet attempt: recalled frames, the
/// selected skills, the matched context records, and today's date.
///
/// Fleet workers used to get the byte-stable prefix alone (#3947) — workspace
/// memories and enforced rules — while every human-facing door also got this
/// block. The omission read as deliberate but was stated nowhere, and it was
/// not harmless: [`agent::build_system_prompt`]'s environment block
/// deliberately keeps today's date OUT of the stable prefix *because* it rides
/// here (#2901), so a worker carried the knowledge-cutoff clause — "treat
/// anything that may have moved since as unverified" — with nothing to measure
/// "since" against.
///
/// Rooted at the attempt's own `root` rather than `cfg.workspace_root`, for
/// the same reason [`agent::open_store`] is above: an isolated task runs in a
/// linked worktree, and parallel workers must not contend on one SQLite
/// writer. What that root can offer differs by task, and both answers are
/// correct — a fresh worktree carries `.stella/rules/*.toml` (the one tracked
/// part of `.stella/`) and still reaches the user-global `~/.stella/skills`,
/// but has no `.stella/private/context.db`, so there the block is records and
/// date and costs no retrieval at all.
///
/// The A/B recall control is armed here, as in every other driver. Parallel
/// workers do not corrupt the schedule by doing so: the suppression counter is
/// durable and each process claims a distinct number, so the arms interleave
/// into one workspace-wide sequence — which is the case
/// `SessionMemory::arm_recall_control`'s docs already name when they list "a
/// fleet task" among the one-turn-per-process surfaces a per-session counter
/// could never schedule.
///
/// Returns the block and its recall telemetry separately: recall must run
/// before the engine is handed its messages, and the attempt's event channel
/// does not exist yet at that point, so the caller owns the send.
async fn worker_recall_block(
    root: &Path,
    cfg: &Config,
    active_rules: &rules::ResolvedRules,
    prompt: &str,
) -> (Option<String>, Option<AgentEvent>) {
    // `warn: false`, the Command Deck's choice for the Command Deck's reason:
    // with `--watch` a live grid owns the terminal, and a per-worker store
    // warning would be N-fold noise painted over it.
    let Some(mut memory) =
        crate::memory::SessionMemory::open_for_session(root, false, &cfg.authority, active_rules)
    else {
        return (None, None);
    };
    memory.arm_recall_control();
    // A fleet attempt recalls before its engine has messages, so there is no
    // conversation to derive touched paths from — the empty anchor set is the
    // honest argument here, and the same scoping the prompt alone always gave.
    let recalled = memory.recall_block_reported(prompt, &[]).await;
    let event = recalled.telemetry_event();
    // `memory` is dropped here, and deliberately not carried to the reflection
    // below: this handle is rooted at the attempt's own tree, and a lesson
    // written through it would land in a database that is deleted with the
    // worktree. [`mine_attempt_lesson`] opens its own, at the invocation root.
    (recalled.text, event)
}

/// The task boundary a fleet attempt stamps onto every lesson it mines.
///
/// The plan's task id — not the attempt, and not the claim-holder identity
/// `{run_id}/{task.id}` that `EngineWorker::run` composes for the claims table
/// (#3989). Governance counts *distinct tasks* before it promotes
/// anything, and the boundary exists to stop a lesson clearing a threshold it
/// has not earned, so where the boundary is uncertain the safe move is to merge
/// rather than split.
///
/// Three attempts at one task share this, and so does the same task id in a
/// later run, or in an unrelated plan that happens to spell a task `t1`. Each
/// of those merges evidence and delays a
/// promotion, which is the recoverable direction. Keying on the run instead
/// would make every nightly re-run of one task a fresh distinct task — the
/// over-counting the session default already has, and the reason a fleet
/// attempt cannot simply keep that default.
fn attempt_task_boundary(task: &Task) -> String {
    format!("fleet:{}", task.id)
}

/// Mine one fleet attempt's turn into the workspace's memory — the steering
/// *out* of an unattended lane, where [`worker_recall_block`] is the steering
/// in (#3956).
///
/// Every other door does this: `stella run`, `/goal` and the REPL all keep
/// their `SessionMemory` alive past the turn and reflect on it. A fleet attempt
/// did not, which made the fan-out asymmetric in the direction that compounds —
/// it consumed the skills and records other doors' reflections produced and
/// contributed none, so every fleet run left the corpus relatively staler. A
/// wave is also the largest batch of turns a workspace ever runs and the one
/// nobody is watching, which is where a mined lesson is worth the most.
///
/// **Where the lesson lands is the decision, not the wiring.** Recall is rooted
/// at the attempt's own tree; this is rooted at `invocation_root`, and the two
/// are genuinely different for an isolated task. Both choices are correct for
/// their half: recall must see the tree the work happens in, while a lesson
/// written into a linked worktree's `.stella/private/context.db` — a fresh
/// empty file, because `.stella/private/` is gitignored and does not travel
/// with `git worktree add` — would be deleted along with the worktree that
/// taught it. It is the same split, for the same reason, that
/// [`agent::open_store`] is called against `cfg.workspace_root` for the
/// coordination store while the attempt's own telemetry store is rooted in the
/// task tree.
///
/// **Which task the lesson lands under** is the other decision. `task_id` is
/// the boundary governance counts distinct tasks with, and a fleet attempt is
/// the first caller in this tree that genuinely knows one;
/// [`attempt_task_boundary`] carries the choice and its retry semantics.
///
/// **What an unattended lane may teach.** Opened through
/// [`crate::memory::SessionMemory::open`] rather than `open_for_session`, so
/// `include_workspace_skills` is false: the lesson reaches `context.db` and the
/// proposal ledger, where recall and `stella proposals list` find it, but a
/// worker never publishes a `SKILL.md` or a rule FILE into the operator's
/// workspace. Promotion to something that steers every later turn stays an
/// attended decision — and the file writes are the half that would have N
/// concurrent workers racing one no-clobber check. The store itself takes
/// concurrent writers by construction (WAL, `busy_timeout`), which is the same
/// property `ab_control`'s durable counter already relies on and names a fleet
/// among its cases; a wave that did contend past the timeout loses that
/// lesson and nothing else, on the best-effort contract the whole learning
/// loop already runs under — never a failed attempt.
///
/// **What it costs.** One model call per attempt that warrants one, on the same
/// terms as every other door: gated by `turn_warrants_reflection` so a tool-free
/// turn spends nothing, bounded by this child's remaining headroom, and settled
/// back into its `BudgetGuard` — so the reflection lands inside the
/// `--spend-limit` the fleet enforces twice (the child's own cap here, and the
/// metered total the parent stops new waves on), rather than beside it.
/// `STELLA_DISABLE_REFLECTION` turns it off, the same switch the one-shot door
/// reads.
///
/// The report's own accounting events are dropped rather than emitted: this
/// runs after the attempt's event channel has closed and its renderer has
/// drained (the friction fold needs the finished journal), and a second,
/// unframed event sequence after a stream's terminal frame is exactly what
/// `surface_reflection` refuses to write on every machine surface. The *cost*
/// is not dropped — it is in the guard, and from there in the attempt's
/// execution row and the fleet ledger.
async fn mine_attempt_lesson(
    invocation_root: &Path,
    cfg: &Config,
    provider: &dyn stella_protocol::Provider,
    evidence: crate::memory::TurnEvidence<'_>,
    execution_id: Option<i64>,
    task_id: &str,
    budget: &mut stella_core::BudgetGuard,
) -> Option<crate::memory::ReflectionReport> {
    let mut memory = crate::memory::SessionMemory::open(invocation_root, false)?;
    if let Some(id) = execution_id {
        memory.set_execution_id(id);
    }
    // Without this every attempt takes the session default — one synthetic task
    // per attempt, so a retry wave reads to governance as several distinct
    // tasks and can clear a promotion threshold on one task's evidence. See
    // [`attempt_task_boundary`] for why the boundary is the task rather than
    // the attempt or the run.
    memory.set_task_id(task_id);
    // `quiet`, for the Command Deck's reason: with `--watch` a live grid owns
    // the terminal, and a per-worker reflection line would be N-fold noise
    // painted over it.
    let mut report = crate::memory::reflect_routed(
        &mut memory,
        cfg,
        provider,
        evidence,
        true,
        agent::remaining_budget(budget),
    )
    .await;
    agent::settle_reflection_budget(&mut report, budget);
    Some(report)
}

/// Whether `attempt_root` and `invocation_root` are the same tree, so a row id
/// minted in one is meaningful in the other.
///
/// The question is never cosmetic: execution ids are per-database autoincrement
/// keys, so stamping an isolated attempt's id (minted in its worktree's
/// `store.db`) onto a reflection writing into the invocation root's store would
/// file that turn's self-review against whatever unrelated execution happens to
/// hold the same number. Canonicalized so a shared task is recognised as one
/// through a symlinked or non-normalized path, and falling back to plain
/// equality when a path cannot be resolved — a false negative only drops the
/// self-review, which is the documented `None` degradation, while a false
/// positive would write a wrong row.
fn same_tree(attempt_root: &Path, invocation_root: &Path) -> bool {
    match (attempt_root.canonicalize(), invocation_root.canonicalize()) {
        (Ok(attempt), Ok(invocation)) => attempt == invocation,
        _ => attempt_root == invocation_root,
    }
}

/// One worker turn in `root`, on the calling thread's runtime — the
/// `Engine::run_turn` step-loop, either raw or dispatched through the
/// installed wrapper plugin `wrapper_variant` names (`--pipeline <variant>`,
/// #3695). Fleet used to also route a worker through the staged pipeline
/// (`--pipeline classic`); that driver has been removed from this build
/// (#3865), so the raw loop is what a wrapper wraps and what an unwrapped
/// attempt runs.
#[allow(clippy::too_many_arguments)] // one caller (EngineWorker::run); composition wiring
async fn run_task(
    cfg: &Config,
    budget_limit: Option<f64>,
    task: &Task,
    root: &Path,
    claim_holder: &str,
    controls: WorkerControls,
    abandoned: tokio::sync::oneshot::Receiver<()>,
    dash: Option<mpsc::UnboundedSender<FleetMsg>>,
    spend: crate::fleet_spend::SpendRecovery,
    wrapper_variant: Option<&str>,
) -> Result<WorkerOutcome, String> {
    // Where this workspace starts. In an ISOLATED worktree this is the whole
    // attribution story — one writer, so the whole advance to `HEAD` is this
    // worker's. Under the shared tree it is not: see `crate::fleet_commits`.
    let start_sha = git_stdout(root, &["rev-parse", "--verify", "HEAD"]).await?;

    // The COORDINATION store lives at the original workspace root — shared
    // by every worker of every fleet in this workspace, which is what makes
    // multiple fleets (and the deck) safe in ONE tree. Captured before the
    // per-worker root override below.
    let claims_store = agent::open_store(&cfg.workspace_root);

    // Bound before the provider is built and before this attempt's first paid
    // call, from the INVOCATION root's roster (still `cfg.workspace_root`
    // here, before the per-worker override below) over the tree this attempt
    // actually runs in — see `wrapped`'s module doc for why those two roots
    // differ and which one each half needs.
    let wrapped = match wrapper_variant {
        Some(variant) => Some(wrapped::bind_for_attempt(
            &cfg.workspace_root,
            root,
            variant,
            &task.id,
            task.test_command.as_deref(),
        )?),
        None => None,
    };

    // Where `stella fleet` was invoked, captured before the per-worker override
    // below takes `cfg.workspace_root` away. It is where this attempt's mined
    // lesson lands (`mine_attempt_lesson`) — the one durable tree in a run whose
    // task trees may not outlive it.
    let invocation_root = cfg.workspace_root.clone();
    let mut cfg = cfg.clone();
    cfg.workspace_root = root.to_path_buf();
    let provider = agent::build_provider(&cfg)?;
    // `Arc` because the worker's sub-agent dispatcher holds a `Weak` back to
    // it (`crate::subagent`) — the registry is the child's tool set.
    let registry = Arc::new(crate::write_dirs::registry_rooted_at(
        &cfg,
        root.to_path_buf(),
    ));
    // As in a deck lane: without a dispatcher the `delegate` tool is advertised
    // and always refuses, and the pause gate published below has nothing to
    // reach. A worker's children inherit its headless posture through the
    // registry they run against.
    crate::subagent::install_for_session(&cfg, &registry)?;
    let active_rules = rules::enforce_workspace_rules(
        &registry,
        root,
        &cfg.authority,
        rules::MidTurnAsk::Headless,
    );
    // Commit attribution (#1216): records the `HEAD` advance this worker is
    // observed making. It sits UNDER the claim tap on purpose — the tap holds
    // the workspace-wide commit lane across the call, so the window this
    // observes is one no sibling can commit inside.
    let committed = crate::fleet_commits::CommitObserver::new(
        &*registry,
        SystemGitCli,
        root.to_path_buf(),
        task.id.clone(),
    );
    // Claim-on-first-write (crate::claims): tool-level write claims + the
    // transient build and commit lanes, coordinated across every writer in
    // the workspace. Same holder as the fleet's declared claims — re-entrant.
    let claims = crate::claims::ClaimTap::new(&committed, claims_store, claim_holder);
    // A fleet worker runs the operator's tool policy and the authorization
    // gate, same as every other driver — an isolated worktree is not a
    // different trust posture. Deliberately NOT `session_stack`:
    // `.stella/tools` customs are withheld from autonomous workers on
    // purpose (#3339, see `policy_stack`'s docs). The principal names the
    // dispatched task.
    let permitted = agent::tool_stack::policy_stack(
        &claims,
        &cfg,
        stella_core::ports::Principal::SubAgent(task.id.to_string()),
        registry.hook_bus(),
    );
    // Every fleet attempt owns the same durable event/accounting envelope as
    // a one-shot or deck turn. The store is rooted in the task worktree so
    // parallel workers never contend on a single SQLite writer.
    let store = agent::open_store(root);
    // The wrapper that actually drove this attempt, or NULL for the raw
    // step-loop — the same honesty rule #3388/#3684 hold every other door to.
    // The staged pipeline that used to write `classic` here (#3381) is gone
    // (#3865), so the only non-NULL value this build can record is an
    // installed plugin's own variant id (#3695).
    // Bound before the call: a composition's variant id is assembled on demand
    // (#3801), so the borrow below needs something that outlives the argument.
    let variant = wrapped.as_ref().map(wrapped::AttemptWrapper::variant);
    let execution = agent::begin_execution(
        &store,
        "fleet",
        &task.prompt,
        &cfg,
        None,
        variant.as_deref(),
    );
    // From here on this attempt's spend is durable in the store even if this
    // thread never lives to report it — publish the handle that makes it
    // readable from the dispatch side (#1216).
    spend.publish(&execution);

    let mut messages = vec![CompletionMessage::system(
        // Each worker is its own session in its own workspace, so its
        // SessionStart hooks fire here, in the worktree.
        agent::with_session_hook_context(
            agent::build_system_prompt(&cfg, root, &active_rules),
            &cfg,
        )
        .await,
    )];
    messages.push(CompletionMessage::user(&task.prompt));
    // The volatile half of this worker's steering (#3947). `build_system_prompt`
    // above is only the byte-stable prefix — memories and enforced rules; the
    // selected skills, the matched context records, and today's date ride the
    // recall block, exactly as they do for `stella run`, `/goal` and the deck.
    // The event is carried to the channel opened below, which this turn's
    // telemetry rides — the same split `agent::goal` documents.
    let (recall_text, recall_event) =
        worker_recall_block(root, &cfg, &active_rules, &task.prompt).await;
    crate::memory::inject_recall_block(&mut messages, recall_text);
    // Everything the engine appends past here is this attempt's own work; the
    // reflection gate reads only that slice, so a turn that called no tool
    // spends no model call on being mined (`turn_warrants_reflection`).
    let turn_start = messages.len();
    // Each child runs under its own enforced guard at the full cap; the
    // parent fleet guard additionally stops new waves on the metered sum.
    let mut budget = agent::build_budget_guard(budget_limit);
    budget.begin_turn();

    // The engine emits `AgentEvent`s into `tx`. The `Json` renderer stays the
    // sink in both modes: it persists telemetry to the store (what `stella
    // observe` reads) and, crucially, prints nothing per-event. When the live
    // grid is up, a forwarder tees each event to the dashboard (tagged by task
    // id) on its way to that silent renderer — telemetry persistence is never
    // sacrificed for the live view.
    let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
    let renderer = match &dash {
        Some(dash) => {
            let (render_tx, render_rx) = mpsc::unbounded_channel::<AgentEvent>();
            let dash = dash.clone();
            let id = task.id.clone();
            let mut src = rx;
            tokio::spawn(async move {
                while let Some(ev) = src.recv().await {
                    let _ = dash.send(FleetMsg::Event {
                        id: id.clone(),
                        event: ev.clone(),
                    });
                    if render_tx.send(ev).is_err() {
                        break;
                    }
                }
            });
            agent::spawn_renderer(
                render_rx,
                crate::OutputFormat::Json,
                execution.clone(),
                cfg.provider.id.to_string(),
                false,
                None,
            )
        }
        None => agent::spawn_renderer(
            rx,
            crate::OutputFormat::Json,
            execution.clone(),
            cfg.provider.id.to_string(),
            false,
            None,
        ),
    };

    // The task's control lines (stella-fleet's `WorkerControls`), composed
    // with the dispatch-drop line from `EngineWorker::run` — see
    // `stop_or_abandoned` for why the two closed-channel cases read in
    // opposite directions.
    let WorkerControls { pause, stop } = controls;
    let stop_wait = stop_or_abandoned(stop, abandoned);
    /// How a raced future resolved — `subsession.rs`'s `RacedTurn` shape,
    /// generic because the two paths race different outcome types.
    enum Raced<T> {
        Outcome(T),
        Stopped,
    }
    /// The stopped attempt's summary. It reports with `success: false` so a
    /// stopped prerequisite never unblocks its dependents.
    const STOPPED: &str = "stopped by fleet control (Fleet::stop_task)";

    // `success`/`summary` are set here, then folded into the WorkerOutcome
    // after the channel drains.
    let (summary, success, outcome_label, force_incomplete): (String, bool, &str, bool) = {
        // The pause line gates the step-loop at the engine's step boundary
        // (never mid-tool), and the stop line races the turn. `Arc` so the
        // gate can be published to the registry as well as borrowed by the
        // engine — a paused worker must not keep spending inside a sub-agent
        // it dispatched, which this worker's dispatcher (installed above)
        // makes reachable.
        let gate: Arc<WatchGate> = Arc::new(WatchGate(pause));
        let _controls = registry
            .attach_turn_controls(stella_core::ports::TurnControls::none().with_gate(gate.clone()));
        let raced: Raced<Result<TurnOutcome, String>> = {
            let hook_runner = HostHookRunner;
            let mut engine = Engine::with_sleeper(
                &*provider,
                &permitted,
                agent::engine_config_for(&cfg),
                &TokioSleeper,
            )
            .with_gate(gate.as_ref());
            if let Some(hooks) = &cfg.hooks {
                engine = engine.with_hooks(hooks, &hook_runner);
            }
            // A fleet worker owns its lane's stage vocabulary; the engine
            // emits no `Stage` of its own (#3416). The opener is here; the
            // closer rides `worker_event_sender` below, ahead of the
            // engine's `TurnComplete` (#3428).
            let _ = tx.send(AgentEvent::Stage {
                name: stella_protocol::StageKind::Execute.into(),
                scope: stella_protocol::StageScope::Run,
            });
            // What recall cost this attempt, on the attempt's own lane (#713,
            // #3947). Recall ran before this channel existed — it has to, the
            // block is part of the messages the engine is about to be handed —
            // so the event waits here rather than being dropped for want of a
            // sink, which is the discard #713 closed everywhere else.
            if let Some(event) = recall_event {
                let _ = tx.send(event);
            }
            match &wrapped {
                // `--pipeline <variant>`: the wrapper bound for this attempt
                // owns the round loop over the same engine the raw arm below
                // drives, and the stop line races the whole dispatch rather
                // than one turn inside it — a stopped worker must not keep
                // spending in a plugin's held-open round either (#3695).
                Some(wrapper) => {
                    let input = wrapper.round_input(&task.prompt, budget_limit.is_some());
                    let mut driver = wrapper.driver(&engine, &mut messages, &mut budget, &tx);
                    // The dispatch's own report is carried out of the race
                    // rather than settled inside it: `settle` consumes the
                    // driver for its last round's outcome, and the racing
                    // future still borrows it until the `select!` scope ends.
                    let dispatched = tokio::select! {
                        report = wrapper.dispatch(input, &mut driver) => Some(report),
                        _ = stop_wait => None,
                    };
                    match dispatched {
                        Some(report) => Raced::Outcome(wrapper.settle(report, driver)),
                        None => Raced::Stopped,
                    }
                }
                // A fleet worker owns its run (#3379) — no pipeline above it,
                // so the run's terminator is this lane's to emit. It is sent
                // once below, gated on the worker's own `success` flag, rather
                // than sealed on drop here: a cancelled or aborted worker must
                // end on `Error` alone, and only the code after the race knows
                // which of the three ways this lane ended.
                None => {
                    let worker = worker_event_sender(&tx);
                    tokio::select! {
                        outcome = engine.run_turn_with_sender(&mut messages, &mut budget, &worker) => {
                            Raced::Outcome(Ok(outcome))
                        }
                        _ = stop_wait => Raced::Stopped,
                    }
                }
            }
        };
        match raced {
            Raced::Outcome(Ok(TurnOutcome::Completed { text, .. })) => {
                (truncate(&text), true, "completed", false)
            }
            Raced::Outcome(Ok(TurnOutcome::Aborted { reason, .. })) => {
                (truncate(&reason), false, "aborted", false)
            }
            // A wrapper that could not be driven fails the attempt by name —
            // never a silently successful one, and never a downgrade to the
            // raw loop the operator did not ask for.
            Raced::Outcome(Err(reason)) => (truncate(&reason), false, "aborted", false),
            Raced::Stopped => (STOPPED.to_string(), false, "cancelled", true),
        }
    };
    // One task is one run on its own stream, so this is its terminator
    // (#3398). `success` is the worker's own flag, decided just above.
    if success {
        agent::persistence::emit_run_complete_on_raw(
            &tx,
            &cfg.model_id,
            budget.session_spent_usd(),
        );
    }
    drop(tx);
    let rendered = renderer.await.unwrap_or_default();
    claims.release_all();

    // The steering out of this lane (#3956). Placed here, after the renderer has
    // drained and before the spend is read: the friction fold needs the finished
    // journal, and the guard has to have absorbed the reflection call before
    // `spent` becomes this attempt's cost in the execution row, the fleet ledger
    // and the parent's metered total.
    //
    // A stopped attempt is not mined, on the same rule the other doors apply
    // (`should_reflect_on`): an operator's soft stop is the one outcome that is
    // not a learning signal — nothing concluded, and the transcript ends
    // mid-thought.
    if !force_incomplete
        && !agent::reflection_explicitly_disabled()
        && crate::memory::turn_warrants_reflection(&messages[turn_start..])
    {
        // Folded from the journal the renderer just finished draining (#3946) —
        // what the turn cost, how long it took, and whether it retried or
        // looped, none of which the transcript records.
        let friction = crate::memory::TurnFriction::from_events(&rendered.events);
        mine_attempt_lesson(
            &invocation_root,
            &cfg,
            &*provider,
            crate::memory::TurnEvidence::with_friction(&messages, &friction, success),
            // Only when this attempt's execution was minted in the very store
            // the lesson is being written into — see `same_tree`.
            same_tree(root, &invocation_root)
                .then(|| execution.as_ref().map(|(_, id)| *id))
                .flatten(),
            // The task, not this attempt: a retry wave is one task's worth of
            // evidence and must count as one (#3989).
            &attempt_task_boundary(task),
            &mut budget,
        )
        .await;
    }

    let spent = budget.session_spent_usd();
    let _ = finalize_fleet_execution(
        &execution,
        &registry,
        outcome_label,
        spent,
        rendered.persistence_complete,
        force_incomplete,
    );
    let commits = crate::fleet_commits::for_attempt(&committed, root, &start_sha, task).await;
    Ok(WorkerOutcome {
        cost_usd: spent,
        commits,
        summary,
        success,
    })
}

fn finalize_fleet_execution(
    execution: &crate::fleet_spend::ExecutionHandle,
    registry: &ToolRegistry,
    outcome_label: &str,
    cost_usd: f64,
    persistence_complete: bool,
    force_incomplete: bool,
) -> bool {
    let Some((store, execution_id)) = execution else {
        return false;
    };
    agent::record_execution_end(
        store,
        *execution_id,
        registry,
        outcome_label,
        cost_usd,
        persistence_complete && !force_incomplete,
    )
}

/// Run `git -C root <args>` and return trimmed stdout, or the stderr as the
/// error. Routed through fleet's [`stella_fleet::SystemGitCli`] — the
/// workspace's one git spawn point — so this path inherits its
/// non-interactive (`GIT_TERMINAL_PROMPT=0`) *and* `kill_on_drop`
/// discipline; the old local `Command` copy could leak a hung git child.
async fn git_stdout(root: &Path, args: &[&str]) -> Result<String, String> {
    use stella_fleet::{GitCli, SystemGitCli};
    let output = SystemGitCli
        .run(root, args)
        .await
        .map_err(|e| format!("git did not run: {e}"))?;
    if !output.success {
        return Err(output.stderr.trim().to_string());
    }
    Ok(output.stdout.trim().to_string())
}

fn truncate(s: &str) -> String {
    let one_line = s.replace('\n', " ");
    let mut out: String = one_line.chars().take(SUMMARY_CHARS).collect();
    if one_line.chars().count() > SUMMARY_CHARS {
        out.push('…');
    }
    out
}

/// The live grid's one-screen recap, printed on the normal screen after the
/// dashboard restores it: each task's final status, wall-clock ELAPSED, and
/// tool-call count, then the total SESSION time. The `render_report` below
/// follows with the durable details (spend, commits, worktrees).
fn print_dash_summary(res: &FleetDashResult) {
    let fmt_elapsed = |d: Duration| {
        let s = d.as_secs();
        format!("{:02}:{:02}", s / 60, s % 60)
    };
    let fmt_session = |d: Duration| {
        let s = d.as_secs();
        format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    };
    println!();
    for t in &res.tasks {
        let glyph = match t.status {
            FleetStatus::Done => t.status.glyph().green(),
            FleetStatus::Failed | FleetStatus::Killed => t.status.glyph().red(),
            FleetStatus::Blocked => t.status.glyph().yellow(),
            _ => t.status.glyph().normal(),
        };
        println!(
            "  {glyph} {} — {} ({}, {} tool call{})",
            t.id.bold(),
            t.title,
            fmt_elapsed(t.elapsed),
            t.tool_calls,
            if t.tool_calls == 1 { "" } else { "s" },
        );
    }
    let tail = if res.detached {
        " (detached — run continued to completion)"
    } else {
        ""
    };
    println!(
        "  {} session {}{tail}",
        "·".dimmed(),
        fmt_session(res.session_elapsed).bold()
    );
}

/// The end-of-run report: per task its outcome, spend, commits, and (when
/// isolated) the worktree that holds the work, then the totals and where the
/// receipts live.
fn render_report(plan: &Plan, report: &FleetRunReport, ledger_path: &Path) {
    println!();
    for handle in &report.handles {
        let ok = handle.outcome.success;
        let mark = if ok { "✓".green() } else { "✗".red() };
        let title = plan
            .task(&handle.task_id)
            .map(|t| t.title.as_str())
            .unwrap_or("");
        println!(
            "  {mark} {} — {} (${:.4}, {} commit{})",
            handle.task_id.bold(),
            title,
            handle.outcome.cost_usd,
            handle.outcome.commits.len(),
            if handle.outcome.commits.len() == 1 {
                ""
            } else {
                "s"
            },
        );
        if let Some(worktree) = &handle.worktree {
            println!(
                "      {} {} @ {}",
                "↳".dimmed(),
                worktree.branch.bright_magenta(),
                worktree.path.display().to_string().dimmed()
            );
        }
        if !handle.outcome.summary.is_empty() {
            println!("      {}", handle.outcome.summary.dimmed());
        }
        // Durable-failure notices (a ledger close that failed after the
        // worker settled, a dispatch lease lost mid-run) are composed in
        // stella-fleet — this file is at its size ceiling (#1677).
        for notice in stella_fleet::handle_notices(handle) {
            println!("      {} {notice}", "!".yellow());
        }
    }
    for (task_id, reason) in &report.dispatch_failures {
        println!(
            "  {} {} — dispatch failed: {}",
            "✗".red(),
            task_id.bold(),
            reason.dimmed()
        );
    }
    if !report.skipped.is_empty() {
        println!(
            "  {} skipped (dependency failed or budget stop): {}",
            "○".yellow(),
            report.skipped.join(", ").dimmed()
        );
    }
    println!(
        "\n  total ${:.4} · ledger {} · worktrees kept for review (`git worktree list`)\n",
        report.total_cost_usd(),
        ledger_path.display(),
    );
}

mod wrapped;

/// Where the fleet command's plan-shape belongs in docs/tests: a plan file is
/// the serde form of [`stella_fleet::Plan`].
#[cfg(test)]
mod tests;
