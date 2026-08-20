//! The loop's two read-only reporting verbs: `watch` and `metrics`.
//!
//! Both answer a question about the loop without moving it. `watch` asks
//! whether anything has happened that would invalidate the last clean sweep;
//! `metrics` folds the recorded cycles into the rates `/self-driving:evolve`
//! reads. Neither files, fixes, or advances the aperture — `watch`'s one write
//! is the aperture *reset* that a wake implies, which is the observation
//! becoming the decision rather than a side effect of reporting.
//!
//! Split out of `self_driving_cmd.rs` when that file crossed the 1500-line
//! ratchet (#4044). They were already one section behind a shared banner
//! there, so the seam is the one the file had drawn for itself.

use serde_json::Value;

use super::state::{self, LoopState};
use super::{demand, gh_available, gh_plain, say};

pub(super) fn watch(st: &LoopState) -> Result<(), String> {
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
        let d = demand(&st.repo_root);
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

pub(super) fn metrics(st: &LoopState) -> Result<(), String> {
    let rows = st.cycles();
    if rows.is_empty() {
        println!("no cycles recorded yet");
        return Ok(());
    }
    let cal = st.calibration();
    let m = stella_autonomy::metrics(&rows);
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
    if let Some(s) = stella_autonomy::starved(&cal) {
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
