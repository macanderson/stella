//! `loop-bench` — an inexpensive turn-loop + context-query correctness harness
//! over Terminal-Bench.
//!
//! The full benchmark (task pass rate) is expensive: Docker containers plus a
//! real model per task. But most of what actually breaks a coding agent is not
//! the model — it is the *loop*: aborting a task having done zero work, dying
//! without saying why, getting caught cycling the same tools, verification
//! that lies, code-intelligence tools that go unused. Those are observable in
//! Stella's own event stream, and they show up on a **cheap** model just as
//! clearly as an expensive one.
//!
//! So this tool runs N Terminal-Bench tasks through Stella — cheap model,
//! budget-capped, in parallel — and reports the correctness signals that the
//! pass-rate number hides. The report types, verdict vocabulary, and
//! distillation live in this crate's library (`src/lib.rs`), where the
//! contract is documented; this binary is the CLI + subprocess orchestration.
//!
//! ```bash
//! # cheapest: 4 tasks on a flash-tier model, $0.20/task cap, 4 concurrent
//! cargo run -p loop-bench -- --n 4
//!
//! # pick tasks + model explicitly
//! cargo run -p loop-bench -- --tasks fix-git,prove-plus-comm -m openrouter/z-ai/glm-5.2
//!
//! # analyze a finished jobs dir without spending anything
//! cargo run -p loop-bench -- --analyze-only --jobs-dir /path/to/jobs --job-name my-run
//! ```
//!
//! Run it from the workspace root: the harbor adapter is put on `PYTHONPATH`
//! by the relative path `ADAPTER_PYTHONPATH`.
//!
//! Exit codes — the contract `.github/workflows/nightly-bench.yml` gates on
//! (#873), and any other caller. Running this needs Docker + harbor + a
//! provider key, so it is a scheduled job, never a per-PR one:
//!
//! - `0` — every trial that reported did real work (or passed).
//! - `1` — the loop misbehaved on at least one trial: silent death, zero
//!   work, a stuck loop, or a requested task that never ran.
//! - `2` — bad invocation: the task list resolved to nothing, or
//!   `--json-out` names a path that cannot be written.
//! - `3` — no trial artifacts were found at all — an infrastructure failure
//!   (harbor never launched anything), distinct from a loop regression.
//! - `4` — the loop was healthy but fewer than `--min-pass` trials passed.
//! - `5` — the run completed and the `--json-out` report could not be
//!   written; the JSON is dumped on stdout instead.
//!
//! `1` outranks `4`: when both fire, the loop failure is the one a human can
//! act on, and a broken loop explains the missing passes anyway.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use clap::Parser;

use loop_bench::{TrialReport, analyze, below_pass_floor, print_table, tally};

/// A small, representative default pool — mixed languages and difficulties, the
/// same tasks the loop-hardening work was measured against. `--n` takes the
/// first N; `--tasks` overrides entirely.
const DEFAULT_POOL: &[&str] = &[
    "fix-git",
    "prove-plus-comm",
    "overfull-hbox",
    "cobol-modernization",
    "git-multibranch",
    "polyglot-c-py",
    "kv-store-grpc",
    "nginx-request-logging",
];

/// A flash-tier model: cheapest thing that still exercises the whole loop.
/// Loop and context correctness do not need a strong model — only a running
/// one — so the default optimizes for cost, not pass rate.
const DEFAULT_MODEL: &str = "openrouter/z-ai/glm-4.7-flash";

/// Where the harbor adapter package lives, relative to the workspace root.
/// harbor loads `stella_harbor:StellaAgent` by import path, so this has to be
/// on `PYTHONPATH` — which is also why this tool must be run from the root.
const ADAPTER_PYTHONPATH: &str = "bench/harbor_adapter";

#[derive(Parser, Debug)]
#[command(
    name = "loop-bench",
    about = "Inexpensive turn-loop + context-query correctness over Terminal-Bench"
)]
struct Args {
    /// Number of tasks from the default pool (ignored when --tasks is given).
    #[arg(long, default_value_t = 4)]
    n: usize,

    /// Explicit comma-separated task names (overrides --n / the default pool).
    #[arg(long, value_delimiter = ',')]
    tasks: Vec<String>,

    /// Provider/model to run. Defaults to a flash-tier model for cheapness.
    #[arg(short = 'm', long, default_value = DEFAULT_MODEL)]
    model: String,

    /// Concurrent trials.
    #[arg(long, default_value_t = 4)]
    concurrent: usize,

    /// Per-task USD budget cap (STELLA_BUDGET).
    #[arg(long, default_value_t = 0.20)]
    budget: f64,

    /// Harbor dataset name.
    #[arg(long, default_value = "terminal-bench")]
    dataset: String,

    /// Where harbor writes job results.
    #[arg(long, default_value = "loop-bench-jobs")]
    jobs_dir: String,

    /// Job name (the sub-directory under --jobs-dir).
    #[arg(long, default_value = "loop-bench")]
    job_name: String,

    /// Wall-clock bound on the harbor child, in seconds; 0 means unbounded.
    /// On expiry the harbor process is killed and whatever landed is analyzed.
    /// Note: killing harbor mid-run can leave task containers running —
    /// `docker ps` and clean up if the timeout fires.
    #[arg(long, default_value_t = 0)]
    timeout: u64,

    /// Path to the (linux) stella binary uploaded into each container. Falls
    /// back to $STELLA_BINARY.
    #[arg(long, env = "STELLA_BINARY")]
    stella_binary: Option<String>,

    /// Skip the harbor run; only analyze an existing jobs dir. Free.
    #[arg(long)]
    analyze_only: bool,

    /// Emit the report as JSON (for CI) instead of the human table.
    #[arg(long)]
    json: bool,

    /// Also write the JSON report to this path, keeping the table on stdout.
    ///
    /// A CI job wants both: the table readable in the run log, the JSON kept
    /// as an artifact to trend. Getting them by invoking twice — once for the
    /// table, then `--analyze-only --json` — would produce two *different*
    /// reports, because `--analyze-only` has no requested task set and so
    /// synthesizes no `NOT-RUN` rows: the artifact would be missing exactly
    /// the rows that gated the run red. One analysis, two renderings.
    #[arg(long, value_name = "PATH")]
    json_out: Option<String>,

    /// Fail (exit 4) when fewer than this many trials pass. 0 disables it.
    ///
    /// Off by default on purpose — see `below_pass_floor` in the library: a
    /// floor guessed rather than observed is red every night on a flash-tier
    /// model, and a gate that is always red is a gate nobody reads.
    #[arg(long, default_value_t = 0, value_name = "N")]
    min_pass: usize,
}

fn main() {
    let args = Args::parse();
    let tasks = resolve_tasks(&args);
    if tasks.is_empty() {
        eprintln!("no tasks to run");
        std::process::exit(2);
    }

    // Prove the report path is writable BEFORE spending anything. A typo'd
    // artifact path discovered after a paid run costs the whole run; caught
    // here it costs one message and no API calls.
    if let Some(path) = &args.json_out
        && let Err(err) = std::fs::write(path, "[]\n")
    {
        eprintln!("--json-out {path} is not writable: {err}");
        std::process::exit(2);
    }

    if !args.analyze_only
        && let Err(code) = run_harbor(&args, &tasks)
    {
        eprintln!("harbor run failed (exit {code}); analyzing whatever landed");
    }

    // The resolved task set scopes the analysis (#611): stale trial dirs from
    // an earlier run under the same job name are skipped, and requested tasks
    // that never launched become NOT-RUN rows. `--analyze-only` reads
    // whatever the finished jobs dir holds instead.
    let requested = if args.analyze_only {
        None
    } else {
        Some(tasks.as_slice())
    };
    let job_dir = Path::new(&args.jobs_dir).join(&args.job_name);
    let reports = analyze(&job_dir, requested);
    // Distinguished from exit 1 (#611): nothing to judge is an infrastructure
    // failure, not a loop regression, and a CI consumer must be able to tell
    // a broken runner from a broken agent.
    if reports.is_empty() {
        eprintln!(
            "no trial artifacts found under {} — nothing to report",
            job_dir.display()
        );
        std::process::exit(3);
    }

    let json = serde_json::to_string_pretty(&reports).unwrap_or_else(|_| "[]".into());
    if args.json {
        println!("{json}");
    } else {
        print_table(&reports);
    }

    // Write the artifact after rendering, so a late failure here (a disk that
    // filled during the run) still leaves the operator the report — dumped on
    // stdout below rather than lost — and is reported as its own exit code.
    let mut report_lost = false;
    if let Some(path) = &args.json_out
        && let Err(err) = std::fs::write(path, format!("{json}\n"))
    {
        eprintln!("could not write the report to {path}: {err}");
        if !args.json {
            eprintln!("dumping it on stdout instead so the run is not evidence-free:");
            println!("{json}");
        }
        report_lost = true;
    }

    // Exit non-zero if the LOOP misbehaved, even when some tasks passed —
    // this tool gates on loop health, not pass rate. See the library docs for
    // which verdicts fold into `loop_broken()`.
    if reports.iter().any(TrialReport::loop_broken) {
        std::process::exit(1);
    }
    // The second, opt-in gate (#873). Checked after loop health so a run that
    // fails both reports the actionable half.
    if below_pass_floor(&reports, args.min_pass) {
        let solved = tally(&reports).solved;
        eprintln!(
            "PASS FLOOR: {solved}/{} solved is below the required --min-pass {}",
            reports.len(),
            args.min_pass
        );
        std::process::exit(4);
    }
    if report_lost {
        std::process::exit(5);
    }
}

fn resolve_tasks(args: &Args) -> Vec<String> {
    if !args.tasks.is_empty() {
        return args.tasks.clone();
    }
    // `--n 0` would measure nothing, so it floors at one task. Asking for more
    // than the pool holds silently under-measures the run — say so, because a
    // green gate over fewer tasks than the operator asked for is a lie.
    let want = args.n.max(1);
    let pool = DEFAULT_POOL.len();
    if want > pool {
        eprintln!(
            "warning: --n {want} exceeds the {pool} tasks in the default pool; \
             running {pool} (pass --tasks to name more)"
        );
    }
    DEFAULT_POOL
        .iter()
        .take(want)
        .map(|s| s.to_string())
        .collect()
}

/// Build and run the harbor command. Returns Err(exit_code) on a non-zero
/// harbor exit — non-fatal, since partial results are still worth analyzing.
fn run_harbor(args: &Args, tasks: &[String]) -> Result<(), i32> {
    // A non-positive (or NaN) cap denies the very first model call, which
    // would report as a zero-work loop failure for every task — the harness
    // manufacturing the signal it gates on. Warn loudly rather than hand
    // harbor a cap that cannot pass. The threshold is not `<= 0.0` because the
    // cap reaches stella at four decimals (see the `STELLA_BUDGET` env below),
    // so a positive-but-tinier-than-that cap is rounded to `0.0000` and is
    // exactly as unspendable — check the value that will actually be sent, not
    // the one the operator typed.
    if !args.budget.is_finite() || args.budget < 0.000_05 {
        eprintln!(
            "warning: --budget {} reaches stella as {:.4}, which is not a spendable \
             cap; every trial will be denied before its first tool call and report as \
             a loop failure",
            args.budget, args.budget
        );
    }
    // Floored like `--n` (#611): harbor's behavior at `-n 0` is undefined
    // from here, and an operator asking for zero concurrency meant one.
    let concurrent = args.concurrent.max(1);
    if args.concurrent == 0 {
        eprintln!("warning: --concurrent 0 is not runnable; using 1");
    }
    // The adapter is imported by path, so it has to exist relative to the cwd.
    // Catch the wrong-directory mistake here instead of inside harbor, where
    // it surfaces as an opaque Python ImportError per trial.
    if !Path::new(ADAPTER_PYTHONPATH).is_dir() {
        eprintln!(
            "warning: {ADAPTER_PYTHONPATH} not found relative to the current directory; \
             run loop-bench from the workspace root or harbor cannot import the adapter"
        );
    }

    let mut cmd = Command::new("harbor");
    cmd.arg("run")
        .args(["--dataset", &args.dataset])
        .args(["--agent-import-path", "stella_harbor:StellaAgent"])
        .args(["-m", &args.model])
        .args(["-k", "1"])
        .args(["-n", &concurrent.to_string()])
        .args(["--job-name", &args.job_name])
        .args(["--jobs-dir", &args.jobs_dir])
        .arg("-y");
    for task in tasks {
        cmd.args(["-i", task]);
    }

    cmd.env("STELLA_BUDGET", format!("{:.4}", args.budget));
    match &args.stella_binary {
        Some(path) => {
            cmd.env("STELLA_BINARY", path);
        }
        // The adapter resolves STELLA_BINARY → `stella` on PATH →
        // ./target/release/stella. On a dev machine the PATH hit lands first
        // and is a host (darwin/arm64) build, which the amd64 container cannot
        // execute — so name that step, not just the last one.
        None => eprintln!(
            "warning: no --stella-binary / $STELLA_BINARY set; the adapter will fall \
             back to `stella` on PATH, then target/release/stella — both must be a \
             LINUX amd64 build to run in the task containers"
        ),
    }
    // The adapter is loaded by import path; make it importable.
    let pythonpath = match std::env::var("PYTHONPATH") {
        Ok(existing) if !existing.is_empty() => {
            format!("{ADAPTER_PYTHONPATH}:{existing}")
        }
        _ => ADAPTER_PYTHONPATH.to_string(),
    };
    cmd.env("PYTHONPATH", pythonpath);

    eprintln!(
        "▶ loop-bench: {} task(s) on {} (budget ${:.2}/task, {} concurrent)",
        tasks.len(),
        args.model,
        args.budget,
        concurrent
    );
    let mut child = cmd.spawn().map_err(|e| {
        eprintln!(
            "could not launch harbor ({e}); is it on PATH? `pip install -e bench/harbor_adapter`"
        );
        127
    })?;

    // A hung harbor (stalled image pull, wedged container) must not hang the
    // harness forever (#611). Poll rather than wait so a deadline can fire;
    // the fallthrough to analyze() still reports whatever landed.
    let deadline = (args.timeout > 0).then(|| Instant::now() + Duration::from_secs(args.timeout));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if let Some(deadline) = deadline
                    && Instant::now() >= deadline
                {
                    eprintln!(
                        "harbor exceeded --timeout {}s; killing it (task containers may \
                         still be running — check `docker ps`)",
                        args.timeout
                    );
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(124);
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(e) => {
                eprintln!("could not wait on harbor: {e}");
                return Err(1);
            }
        }
    };
    if status.success() {
        Ok(())
    } else {
        Err(status.code().unwrap_or(1))
    }
}
