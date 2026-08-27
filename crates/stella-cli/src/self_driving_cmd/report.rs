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

/// What one watch check established.
///
/// Three states rather than two, because a check that could not run has not
/// found the thing quiet — and printing a `✓` beside it is the one outcome
/// this sentinel must never produce. `watch`'s whole job is to answer "does
/// the last clean sweep still hold", and "I could not tell" is not a yes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Finding {
    /// Established, and there is nothing to do.
    Quiet(String),
    /// Established, and it invalidates the sweep.
    Alarm(String),
    /// Not established either way. Wakes, and says why.
    Unknown(String),
}

impl Finding {
    /// Print it, and say whether it wakes the loop.
    fn report(&self) -> bool {
        match self {
            Self::Quiet(text) => {
                println!("  ✓ {text}");
                false
            }
            Self::Alarm(text) => {
                println!("  ! {text}");
                true
            }
            Self::Unknown(text) => {
                println!("  ? {text}");
                true
            }
        }
    }
}

/// What a workflow conclusion says about main.
///
/// GitHub's `conclusion` is `null` while a run is still going and has several
/// terminal spellings. Only `success` establishes green. `failure`,
/// `cancelled`, `timed_out`, `startup_failure` and `action_required` establish
/// red — a cancelled or timed-out run is not a passing one, and reading it as
/// one is how a loop sleeps through a broken tree.
///
/// Everything else establishes nothing: `neutral` and `skipped` decide no
/// question, `null` means the run has not finished, and a spelling GitHub adds
/// later is a word this function has never seen. Those wake, because an
/// unestablished main is exactly the state the last clean sweep no longer
/// covers.
///
/// An indeterminate result is rarely a wake on its own: a run is in flight
/// because main moved, and the head check above has already alarmed on that.
fn ci_verdict(conclusion: Option<&str>) -> Finding {
    let Some(raw) = conclusion.map(str::trim).filter(|s| !s.is_empty()) else {
        return Finding::Unknown(
            "main CI: could not be read (gh returned nothing) — treating as unaudited".to_string(),
        );
    };
    match raw {
        "success" => Finding::Quiet("main CI: success".to_string()),
        "failure" | "cancelled" | "timed_out" | "startup_failure" | "action_required" => {
            Finding::Alarm(format!("main is not green in CI: {raw}"))
        }
        // `null` is what `--jq .[0].conclusion` prints for a run still going.
        "null" => Finding::Unknown("main CI: still running — nothing established yet".to_string()),
        other => Finding::Unknown(format!("main CI: {other} — decides nothing either way")),
    }
}

/// What the defect queue says, given the count or the reason it is unknown.
///
/// An unreachable tracker is not an empty backlog. Both used to arrive here as
/// `Demand::default()` — zero open defects — and the sentinel printed
/// `✓ defect queue empty` and stood the loop down over a queue it had not
/// read.
fn queue_verdict(open_defects: Result<u32, String>) -> Finding {
    match open_defects {
        Ok(0) => Finding::Quiet("defect queue empty".to_string()),
        Ok(n) => Finding::Alarm(format!("{n} open defects in the queue")),
        Err(error) => Finding::Unknown(format!("the defect queue could not be read: {error}")),
    }
}

/// Whether main has moved since the sweep that was recorded clean.
///
/// Both sides are optional and their absences mean different things, so
/// neither may be flattened to a shared sentinel: before this took `Option`s
/// they were both spelled `"none"`, and a `git rev-parse` that failed on a
/// state file with no recorded head compared equal to itself and printed
/// `✓ main unchanged` — two unknowns agreeing that nothing had happened.
fn head_verdict(now: Option<&str>, seen: Option<&str>) -> Finding {
    match (now, seen) {
        (Some(now), Some(seen)) if now == seen => {
            Finding::Quiet("main unchanged since the last clean sweep".to_string())
        }
        (Some(now), Some(seen)) => Finding::Alarm(format!(
            "main moved ({seen} -> {now}) — new code has never been audited"
        )),
        (None, _) => Finding::Unknown(
            "could not read origin/main — cannot tell whether it moved".to_string(),
        ),
        (Some(_), None) => Finding::Unknown(
            "no sweep has recorded a clean head yet — nothing to compare against".to_string(),
        ),
    }
}

pub(super) fn watch(st: &LoopState) -> Result<(), String> {
    let mut triggered = false;
    say("watch mode — checking what would invalidate the last clean sweep");

    let head_now = state::git(&st.repo_root, &["rev-parse", "origin/main"]);
    let calibration = st.calibration();
    let head_seen = calibration
        .extra
        .get("last_clean_head")
        .and_then(Value::as_str);
    triggered |= head_verdict(head_now.as_deref(), head_seen).report();

    if gh_available() {
        triggered |= queue_verdict(demand(&st.repo_root).map(|d| d.open_defects)).report();

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
        ]);
        triggered |= ci_verdict(ci.as_deref()).report();
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
    let ledger = st.cycles();
    let rows = ledger.rows;
    if rows.is_empty() {
        if ledger.unreadable > 0 {
            println!(
                "no cycles could be read — {} ledger line(s) are unreadable",
                ledger.unreadable
            );
        } else {
            println!("no cycles recorded yet");
        }
        return Ok(());
    }
    let cal = st.calibration();
    let m = stella_autonomy::metrics(&rows);
    let n = m.cycles;

    println!("cycles            {n}");
    // Every rate below divides by `n`. A ledger line that could not be read is
    // not in `n`, so saying nothing here would print a confident rate over a
    // denominator that is quietly short.
    if ledger.unreadable > 0 {
        println!(
            "unreadable        {} ledger line(s) — every rate below is over {n}, not {}",
            ledger.unreadable,
            n as usize + ledger.unreadable
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The watch sentinel never prints a tick beside something it could not
    /// read.
    ///
    /// `gh_plain` returns `None` when the call fails — rate limit, network,
    /// expired auth. That `None` used to be flattened to the string
    /// `"unknown"`, compared against `"failure"`, and printed as
    /// `✓ main CI: unknown`, with `triggered` left false. The sentinel then
    /// said SLEEP: a loop whose job is keeping main green, standing down on a
    /// main whose state nobody had established.
    #[test]
    fn an_unreadable_ci_result_is_not_a_green_one() {
        let finding = ci_verdict(None);
        assert!(
            matches!(finding, Finding::Unknown(_)),
            "gh returning nothing establishes nothing: {finding:?}"
        );
        assert!(finding.report(), "and it wakes rather than sleeping");
    }

    /// Only `success` is green. The other terminal conclusions are not.
    ///
    /// `conclusion == "failure"` was the whole test, so a run that was
    /// cancelled, timed out, or died at startup printed `✓ main CI: cancelled`
    /// — a tick beside a run that established nothing about the tree.
    #[test]
    fn a_run_that_did_not_pass_is_not_reported_as_passing() {
        assert!(matches!(ci_verdict(Some("success")), Finding::Quiet(_)));
        for red in [
            "failure",
            "cancelled",
            "timed_out",
            "startup_failure",
            "action_required",
        ] {
            let finding = ci_verdict(Some(red));
            assert!(
                matches!(finding, Finding::Alarm(_)),
                "{red} is not a passing run: {finding:?}"
            );
            assert!(finding.report(), "{red} wakes the loop");
        }
    }

    /// A run still in flight has a null conclusion, and null is not success.
    #[test]
    fn a_running_workflow_establishes_nothing() {
        // What `gh --jq .[0].conclusion` prints for an unfinished run.
        let finding = ci_verdict(Some("null"));
        assert!(matches!(finding, Finding::Unknown(_)), "{finding:?}");
        assert!(finding.report());
    }

    /// A conclusion GitHub adds later is not assumed to be good news.
    #[test]
    fn an_unrecognised_conclusion_wakes_rather_than_ticking() {
        for other in ["neutral", "skipped", "stale", "some_future_word"] {
            let finding = ci_verdict(Some(other));
            assert!(
                matches!(finding, Finding::Unknown(_)),
                "{other} decides nothing: {finding:?}"
            );
            assert!(finding.report());
        }
    }

    /// An unreadable defect queue is not an empty one.
    ///
    /// `demand` returned `Demand::default()` for both an unreachable tracker
    /// and a genuinely empty backlog, so this check printed
    /// `✓ defect queue empty` about a queue it had never read, left
    /// `triggered` false, and the sentinel said SLEEP.
    #[test]
    fn a_defect_queue_that_could_not_be_read_is_not_an_empty_one() {
        let finding = queue_verdict(Err("gh: not authenticated".to_string()));
        assert!(matches!(finding, Finding::Unknown(_)), "{finding:?}");
        assert!(finding.report(), "and it wakes rather than sleeping");
    }

    /// The two measured answers are unchanged.
    #[test]
    fn a_measured_queue_is_quiet_when_empty_and_loud_when_not() {
        let empty = queue_verdict(Ok(0));
        assert!(matches!(empty, Finding::Quiet(_)));
        assert!(!empty.report());

        let full = queue_verdict(Ok(4));
        assert!(matches!(full, Finding::Alarm(_)));
        assert!(full.report());
    }

    /// Two unknowns do not agree that nothing happened.
    ///
    /// Both sides were flattened to the string `"none"` — `git rev-parse`
    /// failing, and a calibration with no `last_clean_head`. On a fresh state
    /// file in a directory where git could not answer, `"none" == "none"` and
    /// the sentinel printed `✓ main unchanged since the last clean sweep`
    /// about a repository it had not read.
    #[test]
    fn a_missing_head_does_not_compare_equal_to_a_missing_record() {
        let finding = head_verdict(None, None);
        assert!(
            matches!(finding, Finding::Unknown(_)),
            "neither side was read: {finding:?}"
        );
        assert!(finding.report());

        // Each absence alone is also unknown, and they say different things.
        assert!(matches!(
            head_verdict(None, Some("abc123")),
            Finding::Unknown(_)
        ));
        assert!(matches!(
            head_verdict(Some("abc123"), None),
            Finding::Unknown(_)
        ));
    }

    /// The ordinary two answers still work.
    #[test]
    fn a_head_that_matches_is_quiet_and_one_that_moved_alarms() {
        let same = head_verdict(Some("abc123"), Some("abc123"));
        assert!(matches!(same, Finding::Quiet(_)));
        assert!(!same.report(), "an unchanged head does not wake the loop");

        let moved = head_verdict(Some("def456"), Some("abc123"));
        assert!(matches!(moved, Finding::Alarm(_)));
        assert!(moved.report());
    }
}
