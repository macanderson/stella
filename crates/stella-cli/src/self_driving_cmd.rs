//! `stella self-driving` — the deterministic half of the perpetual delivery loop
//! as a first-class subcommand (#1548).
//!
//! The decision logic (AIMD, the aperture ladder, the dry-streak oracle, the
//! digest, the governor, the folds) lives in `stella_core::self_driving`, pure
//! and property-tested. This module owns everything invariant 2 keeps out of
//! the engine: the probes that read this machine, the state directory under
//! `~/.stella/self-driving/<slug>/`, the `gh` calls that read the defect queue,
//! and the exact output shapes the `/self-driving` slash commands eval.
//!
//! Flag spellings deliberately match `scripts/self-driving.sh`, whose ported
//! verbs are now one-line `exec stella self-driving …` delegations — the shell
//! harness (`make self-driving-test`) drives those delegations end-to-end, so the
//! two surfaces cannot drift.

pub(crate) mod probes;
pub(crate) mod state;

use std::collections::BTreeMap;
use std::process::Command;

use clap::Subcommand;
use serde_json::Value;
use stella_core::self_driving::{self, CycleOutcome, CycleRecord, Demand, Tooling};

use crate::query_format::{QueryFormat, Rows};
use crate::timefmt::{now_unix, rfc3339_utc_now};
use state::LoopState;

// ---------------------------------------------------------------------------
// The argument tree
// ---------------------------------------------------------------------------

#[derive(Subcommand)]
pub(crate) enum SelfDrivingCmd {
    /// Size this cycle to the machine it is actually running on: supply x
    /// demand x calibration -> a tier and concrete knobs, emitted as shell
    /// assignments so the caller cannot mis-transcribe them.
    Plan {
        /// Show the reasoning instead of the shell assignments.
        #[arg(long)]
        explain: bool,
    },

    /// The loop's durable state: cycle counter, dry streak, seen findings,
    /// and the last few ledger records.
    State {
        /// Print only the current lens's consecutive dry-cycle count.
        #[arg(long)]
        dry_streak: bool,

        /// Output format: the human summary, or the ledger's records under
        /// the versioned query envelope (#1568).
        #[arg(
            long,
            value_enum,
            default_value = "text",
            conflicts_with = "dry_streak"
        )]
        format: QueryFormat,
    },

    /// Allocate a cycle or append its ledger record.
    Cycle {
        #[command(subcommand)]
        cmd: CycleCmd,
    },

    /// Low-duty mode once every lens is dry: check what would invalidate the
    /// last clean sweep. Exits 0 (WAKE — run a full cycle) or 1 (SLEEP).
    Watch,

    /// Fold the ledger into the loop's evidence about ITSELF — the numbers a
    /// meta-cycle argues from, with named pathology signals.
    Metrics,

    /// The audit lens ladder, and what mechanically backs each lens.
    #[command(group = clap::ArgGroup::new("op").required(true))]
    Aperture {
        /// Print the open lens.
        #[arg(long, group = "op")]
        current: bool,

        /// Open the next lens (or drop to watch when the ladder is done).
        #[arg(long, group = "op")]
        advance: bool,

        /// Reopen the ladder at `rubric`.
        #[arg(long, group = "op")]
        reset: bool,

        /// List every lens with its tool backing; `*` marks the open one.
        #[arg(long, group = "op")]
        list: bool,
    },

    /// The finding dedup set the loop's termination story rests on.
    #[command(group = clap::ArgGroup::new("op").required(true))]
    Seen {
        /// Digest a finding description into its dedup key.
        #[arg(long, group = "op", num_args = 1.., value_name = "TEXT")]
        digest: Vec<String>,

        /// Print only the digests not yet recorded.
        #[arg(long, group = "op", num_args = 1.., value_name = "DIGEST")]
        new: Vec<String>,

        /// Record digests; prints how many were actually new.
        #[arg(long, group = "op", num_args = 1.., value_name = "DIGEST")]
        add: Vec<String>,

        /// Print how many findings have ever been triaged.
        #[arg(long, group = "op")]
        count: bool,
    },

    /// Move the AIMD ceilings from evidence — the only place they move.
    #[command(group = clap::ArgGroup::new("op").required(true))]
    Calibrate {
        /// The cycle completed inside its resources.
        #[arg(long, group = "op")]
        ok: bool,

        /// Killed compiler, OOM, ENOSPC, thrash. A red gate is NOT this.
        #[arg(long, group = "op")]
        resource_fail: bool,

        /// Print the controller's current state.
        #[arg(long, group = "op")]
        show: bool,
    },

    /// The ranked defect queue: P0 > P1 > P2 > untriaged, oldest first
    /// inside a rank. Feature work is excluded — this loop closes defects.
    Queue {
        /// How many defects to pick (after ranking, not before).
        #[arg(long, default_value_t = 20)]
        limit: usize,

        /// Output format: the ranked table, or the picked issues under the
        /// versioned query envelope (#1568).
        #[arg(long, value_enum, default_value = "text")]
        format: QueryFormat,
    },

    /// Run lifecycle — a RUN spans many cycles and is the unit a person
    /// starts, stops, and drills into.
    Run {
        #[command(subcommand)]
        cmd: RunCmd,
    },

    /// List runs (shorthand for `run list`).
    Runs,

    /// Stamp the live run's phase, turning a status light into an
    /// observable workflow.
    Phase {
        /// The phase name (e.g. fix, audit, bench, ship).
        name: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum CycleCmd {
    /// Allocate cycle N and emit the shell-evalable cycle context.
    Begin,

    /// Append the ledger record, recalibrate, and advance the aperture when
    /// the lens has gone dry.
    End {
        #[arg(long)]
        cycle: u64,
        #[arg(long, default_value_t = 0)]
        fixed: u64,
        #[arg(long, default_value_t = 0)]
        filed: u64,
        /// Findings this audit produced that no previous audit had (the
        /// dry-streak input).
        #[arg(long = "new", default_value_t = 0)]
        new_findings: u64,
        #[arg(long, default_value = "skipped")]
        bench: String,
        #[arg(long, default_value = "unknown")]
        gate: String,
        /// Comma-separated PR references landed this cycle.
        #[arg(long, default_value = "")]
        prs: String,
        #[arg(long, default_value = "unknown")]
        tier: String,
        #[arg(long, default_value_t = 0)]
        minutes: u64,
        /// `ok` or `resource-fail` — the ONLY input to the controller.
        #[arg(long, value_enum, default_value = "ok")]
        outcome: OutcomeArg,
        /// The tool that produced this cycle's findings for the open lens.
        /// Defaults to the lens's declared backing (#1549).
        #[arg(long)]
        lens_tool: Option<String>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum OutcomeArg {
    Ok,
    ResourceFail,
}

impl OutcomeArg {
    fn as_str(self) -> &'static str {
        match self {
            OutcomeArg::Ok => "ok",
            OutcomeArg::ResourceFail => "resource-fail",
        }
    }
    fn to_core(self) -> CycleOutcome {
        match self {
            OutcomeArg::Ok => CycleOutcome::Ok,
            OutcomeArg::ResourceFail => CycleOutcome::ResourceFail,
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum RunCmd {
    /// Start a run and print SELF_DRIVING_RUN_ID for the driver to keep.
    Start,

    /// End the current run.
    End {
        #[arg(long, default_value = "completed")]
        status: String,
        #[arg(long, default_value = "")]
        reason: String,
    },

    /// End the current run as cancelled — a person stops a run.
    Cancel {
        /// Why it was stopped.
        reason: Option<String>,
    },

    /// Every run, folded: status, cycles, yield, live phase.
    List,

    /// Record which pid is executing the in-flight cycle (the daemon's
    /// handle on a child that `sh -c` exec-optimized out of the process
    /// table). Omit the pid to clear it.
    #[command(name = "stamp-cycle-pid")]
    StampCyclePid { pid: Option<u32> },
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

pub(crate) fn run(cmd: &SelfDrivingCmd) -> Result<(), String> {
    let st = LoopState::open()?;
    match cmd {
        SelfDrivingCmd::Plan { explain } => plan(&st, *explain),
        SelfDrivingCmd::State { dry_streak, format } => cmd_state(&st, *dry_streak, *format),
        SelfDrivingCmd::Cycle { cmd } => match cmd {
            CycleCmd::Begin => cycle_begin(&st),
            CycleCmd::End {
                cycle,
                fixed,
                filed,
                new_findings,
                bench,
                gate,
                prs,
                tier,
                minutes,
                outcome,
                lens_tool,
            } => cycle_end(
                &st,
                *cycle,
                *fixed,
                *filed,
                *new_findings,
                bench,
                gate,
                prs,
                tier,
                *minutes,
                *outcome,
                lens_tool.as_deref(),
            ),
        },
        SelfDrivingCmd::Watch => watch(&st),
        SelfDrivingCmd::Metrics => metrics(&st),
        SelfDrivingCmd::Aperture {
            current,
            advance,
            reset,
            list,
        } => aperture(&st, *current, *advance, *reset, *list),
        SelfDrivingCmd::Seen {
            digest,
            new,
            add,
            count,
        } => seen(&st, digest, new, add, *count),
        SelfDrivingCmd::Calibrate {
            ok,
            resource_fail,
            show,
        } => calibrate_cmd(&st, *ok, *resource_fail, *show),
        SelfDrivingCmd::Queue { limit, format } => queue(&st, *limit, *format),
        SelfDrivingCmd::Run { cmd } => match cmd {
            RunCmd::Start => run_start(&st),
            RunCmd::End { status, reason } => run_end(&st, status, reason),
            RunCmd::Cancel { reason } => run_end_as(
                &st,
                "cancelled",
                reason.as_deref().unwrap_or("stopped by hand"),
            ),
            RunCmd::List => runs_report(&st),
            RunCmd::StampCyclePid { pid } => {
                st.update_run_doc(|doc| match pid {
                    Some(p) => {
                        doc.insert("cycle_pid".into(), u64::from(*p).into());
                    }
                    None => {
                        doc.remove("cycle_pid");
                    }
                });
                Ok(())
            }
        },
        SelfDrivingCmd::Runs => runs_report(&st),
        SelfDrivingCmd::Phase { name } => {
            st.run_write(name, None, None);
            println!("phase: {name}");
            Ok(())
        }
    }
}

/// The bold-line banner the shell driver printed; kept as a plain blank-line
/// separator so eval'd outputs and greps never meet an escape code.
fn say(text: &str) {
    println!("\n{text}");
}

// ---------------------------------------------------------------------------
// plan
// ---------------------------------------------------------------------------

fn gh_available() -> bool {
    Command::new("gh")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Every parsed `gh` call goes through here with color force-disabled: ANSI
/// escapes inside a JSON payload are invisible in a terminal and fatal to
/// every parser (the CLICOLOR_FORCE lesson, inherited from the shell driver).
fn gh_plain(args: &[&str]) -> Option<String> {
    let out = Command::new("gh")
        .args(args)
        .env("NO_COLOR", "1")
        .env("CLICOLOR_FORCE", "0")
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn demand() -> Demand {
    if !gh_available() {
        return Demand::default();
    }
    let count = |extra: &[&str]| -> u32 {
        let mut args = vec!["issue", "list", "--state", "open", "--label", "bug"];
        args.extend_from_slice(extra);
        args.extend_from_slice(&["--json", "number", "--jq", "length"]);
        gh_plain(&args)
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    };
    Demand {
        open_defects: count(&[]),
        p0: count(&["--label", "P0"]),
    }
}

fn plan(st: &LoopState, explain: bool) -> Result<(), String> {
    let supply = probes::supply(&st.repo_root);
    let demand = demand();
    let cal = st.calibration();
    let plan = self_driving::plan_cycle(supply, demand, &cal, state::floors());
    let aperture = st.aperture();

    if explain {
        say(&format!("self-driving plan — tier {}", plan.tier.as_str()));
        println!("  {}\n", plan.reason);
        println!(
            "  supply    {} cores (load {}) · {}GB RAM ({}GB free) · {}GB disk · battery={} · busy={}",
            supply.cpu,
            supply.load1,
            supply.mem_total_gb,
            supply.mem_free_gb,
            supply.disk_free_gb,
            u8::from(supply.on_battery),
            u8::from(supply.busy),
        );
        println!(
            "  demand    {} open defects ({} P0)",
            demand.open_defects, demand.p0
        );
        println!(
            "  ceilings  batch<={} parallel<={} (AIMD, {} clean runs)\n",
            cal.batch_ceiling, cal.parallel_ceiling, cal.clean_run
        );
        println!("  batch     {} defects", plan.batch);
        println!("  parallel  {} worktree(s)", plan.parallel);
        println!(
            "  build     {}",
            if plan.local_build {
                format!("local, scope={}", plan.scope.as_str())
            } else {
                "PUSH TO CI — do not compile here".to_string()
            }
        );
        println!("  audit     {} (aperture: {aperture})", plan.audit.as_str());
        println!("  bench     {}", plan.bench.as_str());
        return Ok(());
    }

    println!("SELF_DRIVING_TIER={}", plan.tier.as_str());
    println!("SELF_DRIVING_BATCH={}", plan.batch);
    println!("SELF_DRIVING_PARALLEL={}", plan.parallel);
    println!("SELF_DRIVING_LOCAL_BUILD={}", u8::from(plan.local_build));
    println!("SELF_DRIVING_SCOPE={}", plan.scope.as_str());
    println!("SELF_DRIVING_AUDIT={}", plan.audit.as_str());
    println!("SELF_DRIVING_BENCH={}", plan.bench.as_str());
    println!("SELF_DRIVING_APERTURE={aperture}");
    println!("SELF_DRIVING_CPU={}", supply.cpu);
    println!("SELF_DRIVING_MEM_FREE_GB={}", supply.mem_free_gb);
    println!("SELF_DRIVING_DISK_FREE_GB={}", supply.disk_free_gb);
    println!("SELF_DRIVING_QUEUE={}", demand.open_defects);
    println!("SELF_DRIVING_QUEUE_P0={}", demand.p0);
    println!("SELF_DRIVING_PLAN_REASON=\"{}\"", plan.reason);
    Ok(())
}

// ---------------------------------------------------------------------------
// state / cycle
// ---------------------------------------------------------------------------

fn cmd_state(st: &LoopState, dry_streak: bool, format: QueryFormat) -> Result<(), String> {
    if dry_streak {
        let streak = self_driving::dry_streak(&st.cycles(), &st.aperture());
        println!("{streak}");
        return Ok(());
    }
    let cycles = st.cycles();
    if format == QueryFormat::Json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Rows::new(&cycles)).map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    say(&format!("self-driving state — {}", st.dir.display()));
    println!("  cycle           {}", st.cycle_counter());
    println!(
        "  dry streak      {} / {}",
        self_driving::dry_streak(&cycles, &st.aperture()),
        state::dry_streak_target()
    );
    println!("  findings seen   {}", st.seen_count());
    println!("  cycles logged   {}", cycles.len());
    println!();
    if !cycles.is_empty() {
        say("last 5 cycles");
        for r in cycles.iter().rev().take(5).rev() {
            let flag = if r.dry { "DRY" } else { "   " };
            println!(
                "  #{:<3} {flag}  fixed {:>2}  filed {:>2}  new {:>2}  gate {:<7} {:<7} {}",
                r.cycle, r.fixed, r.filed, r.new_findings, r.gate, r.tier, r.aperture
            );
        }
    }
    Ok(())
}

/// The declared tool backing for a lens name (#1549): what the cycle should
/// run, or an explicit statement that it works unaided.
fn declared_tool(lens_name: &str) -> String {
    match self_driving::lens(lens_name).map(|l| l.tooling) {
        Some(Tooling::Command { run, .. }) => collapse_ws(run),
        Some(Tooling::ModelOnly { .. }) => "model-only".to_string(),
        None => "none".to_string(),
    }
}

/// The `&'static str` tables carry continuation-indented text; collapse runs
/// of whitespace so a tool string prints (and records) as one clean line.
fn collapse_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cycle_begin(st: &LoopState) -> Result<(), String> {
    let n = st.cycle_counter() + 1;
    st.set_cycle_counter(n)?;
    // A run record still present here means the last cycle never reached
    // cycle-end. Bank it before overwriting, so the ledger records the
    // abandonment instead of the cycle just vanishing from the history.
    st.reconcile_abandoned_run();
    st.run_write("starting", Some(n), None);

    let aperture = st.aperture();
    let streak = self_driving::dry_streak(&st.cycles(), &aperture);
    println!("SELF_DRIVING_CYCLE={n}");
    println!("SELF_DRIVING_DRY_STREAK_SO_FAR={streak}");
    println!(
        "SELF_DRIVING_DRY_STREAK_TARGET={}",
        state::dry_streak_target()
    );
    println!("SELF_DRIVING_STATE_DIR={}", st.dir.display());
    println!("SELF_DRIVING_SEEN_COUNT={}", st.seen_count());
    println!("SELF_DRIVING_STARTED_AT={}", rfc3339_utc_now());
    println!(
        "SELF_DRIVING_BASE_SHA={}",
        state::git(&st.repo_root, &["rev-parse", "HEAD"]).unwrap_or_else(|| "none".to_string())
    );
    println!("SELF_DRIVING_APERTURE={aperture}");
    println!(
        "SELF_DRIVING_APERTURE_TOOL=\"{}\"",
        declared_tool(&aperture)
    );
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "one argument per ledger field; a params struct would only move \
              the list one file away from the clap definition it mirrors"
)]
fn cycle_end(
    st: &LoopState,
    cycle: u64,
    fixed: u64,
    filed: u64,
    new_findings: u64,
    bench: &str,
    gate: &str,
    prs: &str,
    tier: &str,
    minutes: u64,
    outcome: OutcomeArg,
    lens_tool: Option<&str>,
) -> Result<(), String> {
    let aperture = st.aperture();
    let rec = CycleRecord {
        cycle,
        run_id: st.current_run_id().unwrap_or_else(|| "-".to_string()),
        ended_at: rfc3339_utc_now(),
        ended_at_unix: now_unix(),
        fixed,
        filed,
        new_findings,
        bench: bench.to_string(),
        gate: gate.to_string(),
        prs: prs
            .split(',')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect(),
        tier: tier.to_string(),
        aperture: aperture.clone(),
        lens_tool: Some(lens_tool.map_or_else(|| declared_tool(&aperture), str::to_string)),
        outcome: outcome.as_str().to_string(),
        minutes,
        dry: new_findings == 0,
        extra: serde_json::Map::new(),
    };
    st.append_cycle(&rec)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&rec).map_err(|e| e.to_string())?
    );

    // Close the control loop: the controller learns what this box survives,
    // and the aperture widens the moment a lens stops producing.
    let mut cal = st.calibration();
    self_driving::calibrate(&mut cal, outcome.to_core(), &state::aimd_limits());
    st.write_calibration(&cal)?;

    // The CYCLE ended, not the run — only `run end`/`run cancel` closes one.
    st.run_write("idle", None, Some(tier));

    let streak = self_driving::dry_streak(&st.cycles(), &aperture);
    if streak >= state::dry_streak_target() {
        say(&format!("aperture {aperture} is dry after {streak} cycles"));
        advance_aperture(st, &aperture)?;
        // Remember the commit the sweep was clean AT, so watch mode can tell
        // "nothing changed" from "nobody has looked yet".
        let head = state::git(&st.repo_root, &["rev-parse", "origin/main"])
            .unwrap_or_else(|| "none".to_string());
        let mut cal = st.calibration();
        cal.extra
            .insert("last_clean_head".to_string(), Value::String(head));
        st.write_calibration(&cal)?;
    }
    Ok(())
}

fn advance_aperture(st: &LoopState, current: &str) -> Result<(), String> {
    let next = self_driving::advance(current);
    st.set_aperture(next)?;
    if next == self_driving::WATCH {
        println!("aperture {current} -> watch (every lens dry; the loop sleeps, it does not stop)");
    } else {
        println!("aperture {current} -> {next}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// watch / metrics
// ---------------------------------------------------------------------------

fn watch(st: &LoopState) -> Result<(), String> {
    let mut triggered = false;
    say("watch mode — checking what would invalidate the last clean sweep");

    let head_now = state::git(&st.repo_root, &["rev-parse", "origin/main"])
        .unwrap_or_else(|| "none".to_string());
    let head_seen = st
        .calibration()
        .extra
        .get("last_clean_head")
        .and_then(Value::as_str)
        .unwrap_or("none")
        .to_string();
    if head_now == head_seen {
        println!("  ✓ main unchanged since the last clean sweep");
    } else {
        println!("  ! main moved ({head_seen} -> {head_now}) — new code has never been audited");
        triggered = true;
    }

    if gh_available() {
        let d = demand();
        if d.open_defects > 0 {
            println!("  ! {} open defects in the queue", d.open_defects);
            triggered = true;
        } else {
            println!("  ✓ defect queue empty");
        }

        let ci = gh_plain(&[
            "run",
            "list",
            "--branch",
            "main",
            "--limit",
            "1",
            "--json",
            "conclusion",
            "--jq",
            ".[0].conclusion",
        ])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
        if ci == "failure" {
            println!("  ! main is red in CI");
            triggered = true;
        } else {
            println!("  ✓ main CI: {ci}");
        }
    }

    println!();
    if triggered {
        st.set_aperture("rubric")?;
        println!("WAKE — aperture reset to rubric; run a full cycle.");
        return Ok(());
    }
    println!("SLEEP — nothing changed. Check again next interval; spend nothing.");
    // The sleep verdict is an exit code, not an error: the driver loops on
    // `watch || continue`, and an error envelope here would read as a broken
    // sentinel rather than a quiet night.
    std::process::exit(1);
}

fn metrics(st: &LoopState) -> Result<(), String> {
    let rows = st.cycles();
    if rows.is_empty() {
        println!("no cycles recorded yet");
        return Ok(());
    }
    let cal = st.calibration();
    let m = self_driving::metrics(&rows);
    let n = m.cycles;

    println!("cycles            {n}");
    println!(
        "fixed / cycle     {:.1}   ({} total)",
        m.fixed as f64 / n as f64,
        m.fixed
    );
    println!(
        "filed / cycle     {:.1}   ({} total)",
        m.filed as f64 / n as f64,
        m.filed
    );
    println!(
        "discovery / cycle {:.1}   (unseen findings)",
        m.new_findings as f64 / n as f64
    );
    println!(
        "zero-fix cycles   {} ({:.0}%)",
        m.zero_fix_cycles,
        100.0 * m.zero_fix_cycles as f64 / n as f64
    );
    println!(
        "red-gate cycles   {} ({:.0}%)",
        m.red_gate_cycles,
        100.0 * m.red_gate_cycles as f64 / n as f64
    );
    println!(
        "controller        batch<={} parallel<={} ({} clean runs) — {}",
        cal.batch_ceiling, cal.parallel_ceiling, cal.clean_run, cal.note
    );

    let mut signals = m.signals;
    if let Some(s) = self_driving::starved(&cal) {
        // Keep the shell driver's order: STARVED reported right after STUCK.
        let at = usize::from(signals.first().is_some_and(|s| s.code == "STUCK"));
        signals.insert(at, s);
    }
    if !signals.is_empty() {
        println!("\nsignals for /self-driving:evolve");
        for s in signals {
            println!("  ! {}: {}", s.code, s.text);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// aperture / seen / calibrate / queue
// ---------------------------------------------------------------------------

fn aperture(
    st: &LoopState,
    current: bool,
    advance: bool,
    reset: bool,
    list: bool,
) -> Result<(), String> {
    let open = st.aperture();
    if current {
        println!("{open}");
    } else if advance {
        advance_aperture(st, &open)?;
    } else if reset {
        st.set_aperture("rubric")?;
        println!("aperture reset to rubric");
    } else if list {
        for l in self_driving::LENSES {
            let marker = if l.name == open { "*" } else { " " };
            let heavy = if l.heavy_only { " [heavy tier]" } else { "" };
            let backing = match l.tooling {
                Tooling::Command { run, .. } => collapse_ws(run),
                Tooling::ModelOnly { note } => format!("model-only — {}", collapse_ws(note)),
            };
            println!("  {marker} {:<13} {backing}{heavy}", l.name);
        }
        let marker = if open == self_driving::WATCH {
            "*"
        } else {
            " "
        };
        println!(
            "  {marker} {:<13} cheap sentinels; any trigger reopens rubric",
            self_driving::WATCH
        );
    }
    Ok(())
}

fn seen(
    st: &LoopState,
    digest: &[String],
    new: &[String],
    add: &[String],
    count: bool,
) -> Result<(), String> {
    if !digest.is_empty() {
        println!("{}", self_driving::finding_digest(&digest.join(" ")));
    } else if !new.is_empty() {
        let known = st.seen();
        for d in new {
            if !known.iter().any(|k| k == d) {
                println!("{d}");
            }
        }
    } else if !add.is_empty() {
        // The set grows as we append, so a digest repeated within one call
        // still counts (and lands) exactly once.
        let mut known: std::collections::HashSet<String> = st.seen().into_iter().collect();
        let mut added = 0;
        for d in add {
            if known.insert(d.clone()) {
                st.add_seen(d)?;
                added += 1;
            }
        }
        println!("{added}");
    } else if count {
        println!("{}", st.seen_count());
    }
    Ok(())
}

fn calibrate_cmd(st: &LoopState, ok: bool, resource_fail: bool, show: bool) -> Result<(), String> {
    if show {
        let cal = st.calibration();
        println!(
            "{}",
            serde_json::to_string_pretty(&cal).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    // clap's ArgGroup guarantees exactly one flag; `ok` therefore implies
    // `!resource_fail` and vice versa.
    let outcome = match (ok, resource_fail) {
        (_, true) => CycleOutcome::ResourceFail,
        _ => CycleOutcome::Ok,
    };
    let mut cal = st.calibration();
    self_driving::calibrate(&mut cal, outcome, &state::aimd_limits());
    st.write_calibration(&cal)?;
    println!(
        "{}",
        serde_json::to_string(&cal).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn queue(_st: &LoopState, limit: usize, format: QueryFormat) -> Result<(), String> {
    let raw = gh_plain(&[
        "issue",
        "list",
        "--state",
        "open",
        "--limit",
        "200",
        "--json",
        "number,title,labels,createdAt,url",
    ])
    .ok_or_else(|| "gh issue list failed (is gh installed and authenticated?)".to_string())?;
    let issues: Vec<self_driving::QueueIssue> =
        serde_json::from_str(&raw).map_err(|e| format!("gh issue list payload: {e}"))?;
    let total_issues = issues.len();

    let defects = self_driving::rank_defects(issues);
    let picked = &defects[..limit.min(defects.len())];

    if format == QueryFormat::Json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Rows::new(picked)).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    for i in picked {
        let prio = ["P0", "P1", "P2"]
            .into_iter()
            .find(|p| i.labels.iter().any(|l| l.name == *p))
            .unwrap_or("--");
        let area = i
            .labels
            .iter()
            .find(|l| l.name.starts_with("area:"))
            .map(|l| l.name.as_str())
            .unwrap_or("");
        println!("{prio:>2}  #{:<6} {area:<18} {}", i.number, i.title);
    }
    eprintln!(
        "\n{} of {} open defects ({} open issues total)",
        picked.len(),
        defects.len(),
        total_issues
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// run lifecycle
// ---------------------------------------------------------------------------

fn run_start(st: &LoopState) -> Result<(), String> {
    let rid = state::new_run_id();
    let driver = std::env::var("SELF_DRIVING_DRIVER").unwrap_or_else(|_| "interactive".to_string());
    let mut fields = BTreeMap::new();
    fields.insert("run_id".to_string(), Value::String(rid.clone()));
    fields.insert("status".to_string(), Value::String("running".to_string()));
    fields.insert("driver".to_string(), Value::String(driver));
    fields.insert(
        "slug".to_string(),
        Value::String(state::repo_slug(&st.repo_root)),
    );
    fields.insert(
        "workspace_root".to_string(),
        Value::String(st.repo_root.to_string_lossy().into_owned()),
    );
    fields.insert("pid".to_string(), u64::from(std::process::id()).into());
    st.append_run_record(fields)?;

    let started = rfc3339_utc_now();
    st.update_run_doc(|doc| {
        doc.insert("run_id".into(), rid.clone().into());
        doc.entry("started_at".to_string())
            .or_insert_with(|| Value::String(started));
    });
    // Stamp the first heartbeat through the SAME writer every other phase
    // uses — the record must be complete from its first byte, not completed
    // by whatever happens next.
    st.run_write("idle", None, None);

    say(&format!("run {rid} started"));
    println!("SELF_DRIVING_RUN_ID={rid}");
    Ok(())
}

fn run_end(st: &LoopState, status: &str, reason: &str) -> Result<(), String> {
    run_end_as(st, status, reason)
}

fn run_end_as(st: &LoopState, status: &str, reason: &str) -> Result<(), String> {
    let rid = st
        .current_run_id()
        .ok_or_else(|| "run end: no run in progress".to_string())?;
    let mut fields = BTreeMap::new();
    fields.insert("run_id".to_string(), Value::String(rid.clone()));
    fields.insert("status".to_string(), Value::String(status.to_string()));
    fields.insert("reason".to_string(), Value::String(reason.to_string()));
    st.append_run_record(fields)?;
    st.clear_run_doc();
    say(&format!("run {rid} -> {status}"));
    Ok(())
}

fn runs_report(st: &LoopState) -> Result<(), String> {
    let rows = self_driving::fold_runs(
        &st.run_records(),
        &st.cycles(),
        st.run_doc().as_ref(),
        now_unix(),
        state::stale_after_secs(),
    );
    if rows.is_empty() {
        println!("no self-driving runs recorded yet");
        return Ok(());
    }
    println!(
        "{:<28} {:<10} {:>3} {:>5} {:>5} {:>4}  phase",
        "run", "status", "cyc", "fixed", "filed", "new"
    );
    for r in rows {
        println!(
            "{:<28} {:<10} {:>3} {:>5} {:>5} {:>4}  {}",
            r.run_id, r.status, r.cycles, r.fixed, r.filed, r.new_findings, r.phase
        );
    }
    Ok(())
}
