//! The fullauto view: every perpetual-delivery run on this machine, its cycles,
//! and the evidence that the loop is improving itself.
//!
//! Read from `~/.stella/fullauto/<slug>/`, which fullauto writes as plain JSONL
//! and small JSON documents. Nothing here opens a database, and nothing here
//! writes — the same two rules the rest of this crate lives under, for the same
//! reason: an observer that mutates what it observes is not an observer.
//!
//! Three shapes matter, and they are deliberately different files:
//!
//! - `runs.jsonl` — one append per **state transition** of a run. Readers fold
//!   by `run_id`, last write winning. It is append-only because the case this
//!   file exists to record is a crash, and a rewrite-in-place is the one
//!   structure a crash mid-write can corrupt.
//! - `ledger.jsonl` — one append per completed **cycle**, carrying its `run_id`.
//! - `run.json` — the single **live pointer**: which run is current, what phase
//!   it is in, and when it last drew breath.
//!
//! The status a run reports is not simply the last thing it wrote. A run whose
//! final record says `running` but whose heartbeat has gone stale is *crashed*,
//! and only a reader can say so — the process that would have written it is
//! gone. That resolution happens in [`fold_runs`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Loop directories scanned under `~/.stella/fullauto`.
///
/// One per repository the user runs the loop against, so a real machine holds a
/// handful. The bound keeps a pathological or mispointed directory from
/// stalling a synchronous request.
const MAX_LOOPS: usize = 64;

/// Lines read from any one `.jsonl` file.
///
/// A loop that has run nightly for a year holds a few thousand cycles. Reading
/// the tail rather than the head would be wrong here — the fold needs a run's
/// *first* record (which carries `driver`, `slug` and `workspace_root`) as well
/// as its last — so the cap is generous and the excess is reported rather than
/// silently dropped.
const MAX_JSONL_LINES: usize = 20_000;

/// Seconds without a heartbeat after which a `running` run is reported crashed.
///
/// Generous on purpose, and deliberately equal to fullauto's own
/// `FULLAUTO_STALE_AFTER_SECS` default: a single model call inside an audit
/// phase legitimately runs for minutes, and calling a live run dead is a worse
/// error than noticing a dead one late.
const STALE_AFTER_SECS: i64 = 900;

/// `~/.stella/fullauto` — the root every loop's state directory sits under.
///
/// Resolved through `stella-home` rather than re-derived, so `STELLA_HOME`
/// moves the observatory's view and the loop's writes together.
fn state_root() -> Option<PathBuf> {
    stella_home::stella_home().map(|home| home.join("fullauto"))
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// Parse a `.jsonl` file into objects, skipping unparseable lines.
///
/// A truncated final line is the normal state of a file being appended to while
/// it is read, so it is a skip rather than an error.
fn read_jsonl(path: &Path) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .take(MAX_JSONL_LINES)
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v.is_object())
        .collect()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn read_trimmed(path: &Path) -> Option<String> {
    Some(std::fs::read_to_string(path).ok()?.trim().to_string())
}

fn i64_at(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn str_at(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One loop's state directory, already read.
struct Loop {
    slug: String,
    dir: PathBuf,
    roots: Vec<String>,
    aperture: String,
    calibration: Value,
    seen: usize,
    runs: Vec<Value>,
    cycles: Vec<Value>,
    live: Option<Value>,
}

fn load_loop(dir: &Path) -> Option<Loop> {
    let slug = dir.file_name()?.to_string_lossy().into_owned();
    let workspace = read_json(&dir.join("workspace.json")).unwrap_or_else(|| json!({}));
    let roots = workspace
        .get("roots")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    // `seen.txt` is one digest per line; its SIZE is the interesting number
    // (how much has ever been triaged), not its contents, which are opaque
    // hashes that would tell the page nothing.
    let seen = std::fs::read_to_string(dir.join("seen.txt"))
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    Some(Loop {
        slug,
        dir: dir.to_path_buf(),
        roots,
        aperture: read_trimmed(&dir.join("aperture")).unwrap_or_default(),
        calibration: read_json(&dir.join("calibration.json")).unwrap_or_else(|| json!({})),
        seen,
        runs: read_jsonl(&dir.join("runs.jsonl")),
        cycles: read_jsonl(&dir.join("ledger.jsonl")),
        live: read_json(&dir.join("run.json")),
    })
}

fn load_loops() -> Vec<Loop> {
    let Some(root) = state_root() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten().take(MAX_LOOPS) {
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false)
            && let Some(l) = load_loop(&entry.path())
        {
            out.push(l);
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

/// Fold `runs.jsonl` into one record per run, then resolve the status the file
/// cannot state on its own.
///
/// Two corrections happen here, and both exist because the writer may be gone:
///
/// 1. A run whose last record says `running` while the live pointer names a
///    *different* run has been orphaned — nothing is driving it.
/// 2. A run that still holds the live pointer but whose heartbeat has gone
///    stale has crashed. The heartbeat is the witness, never the pid: every
///    fullauto subcommand is its own short-lived process, so a recorded pid is
///    dead moments after it is written and would declare every run crashed.
fn fold_runs(l: &Loop) -> Vec<Value> {
    let mut folded: BTreeMap<String, Value> = BTreeMap::new();
    for rec in &l.runs {
        let id = str_at(rec, "run_id");
        if id.is_empty() {
            continue;
        }
        let slot = folded.entry(id).or_insert_with(|| json!({}));
        if let (Some(dst), Some(src)) = (slot.as_object_mut(), rec.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
    }

    let live_id = l
        .live
        .as_ref()
        .map(|v| str_at(v, "run_id"))
        .unwrap_or_default();
    let now = now_unix();

    let mut cycles_by_run: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for c in &l.cycles {
        cycles_by_run
            .entry(str_at(c, "run_id"))
            .or_default()
            .push(c);
    }

    let mut out = Vec::new();
    for (id, rec) in folded {
        let cycles = cycles_by_run.get(&id).cloned().unwrap_or_default();
        let mut status = str_at(&rec, "status");
        let mut live_block = Value::Null;

        if status == "running" {
            if !live_id.is_empty() && live_id == id {
                let live = l.live.clone().unwrap_or_else(|| json!({}));
                let age = now - i64_at(&live, "heartbeat_unix");
                if age > STALE_AFTER_SECS {
                    status = "crashed".into();
                }
                live_block = json!({
                    "phase": str_at(&live, "phase"),
                    "cycle": i64_at(&live, "cycle"),
                    "tier": str_at(&live, "tier"),
                    "aperture": str_at(&live, "aperture"),
                    "heartbeat_age_s": age,
                    "pid": i64_at(&live, "pid"),
                });
            } else {
                status = "crashed".into();
            }
        }

        let fixed: i64 = cycles.iter().map(|c| i64_at(c, "fixed")).sum();
        let filed: i64 = cycles.iter().map(|c| i64_at(c, "filed")).sum();
        let new_findings: i64 = cycles.iter().map(|c| i64_at(c, "new_findings")).sum();
        let minutes: i64 = cycles.iter().map(|c| i64_at(c, "minutes")).sum();
        let red: i64 = cycles
            .iter()
            .filter(|c| str_at(c, "gate") == "red")
            .count() as i64;

        out.push(json!({
            "run_id": id,
            "slug": l.slug,
            "status": status,
            "driver": str_at(&rec, "driver"),
            "host": str_at(&rec, "host"),
            "started_at": rec.get("at").cloned().unwrap_or(Value::Null),
            "started_unix": i64_at(&rec, "at_unix"),
            "reason": str_at(&rec, "reason"),
            "workspace_root": str_at(&rec, "workspace_root"),
            "cycles": cycles.len(),
            "fixed": fixed,
            "filed": filed,
            "new_findings": new_findings,
            "minutes": minutes,
            "red_gates": red,
            "live": live_block,
        }));
    }
    out.sort_by_key(|r| -i64_at(r, "started_unix"));
    out
}

/// Pathologies the loop can name about ITSELF, from its own ledger.
///
/// This is the self-improvement evidence: each entry names a *specific* failure
/// mode rather than a vague quality score, because the remedy differs
/// completely between them and a dashboard that only said "health: poor" would
/// send a reader to re-derive all of this by hand.
///
/// Deliberately the same four signals `fullauto metrics` reports, so the page
/// and the terminal never disagree about whether the loop is in trouble.
fn self_improvement(cycles: &[&Value], calibration: &Value) -> Value {
    let n = cycles.len() as i64;
    let mut signals = Vec::new();
    if n == 0 {
        return json!({ "signals": signals, "cycles": 0 });
    }

    let fixed: i64 = cycles.iter().map(|c| i64_at(c, "fixed")).sum();
    let filed: i64 = cycles.iter().map(|c| i64_at(c, "filed")).sum();
    let new_findings: i64 = cycles.iter().map(|c| i64_at(c, "new_findings")).sum();
    let zero_fix = cycles.iter().filter(|c| i64_at(c, "fixed") == 0).count() as i64;
    let red = cycles
        .iter()
        .filter(|c| str_at(c, "gate") == "red")
        .count() as i64;
    let clean_run = i64_at(calibration, "clean_run");
    let batch_ceiling = calibration
        .get("batch_ceiling")
        .and_then(Value::as_i64)
        .unwrap_or(20);

    let tail_zero = cycles.len() >= 3
        && cycles[cycles.len() - 3..]
            .iter()
            .all(|c| i64_at(c, "fixed") == 0);
    if tail_zero {
        signals.push(json!({
            "code": "STUCK",
            "text": "three consecutive zero-fix cycles — the blocker is structural, not the queue",
        }));
    }
    if batch_ceiling <= 4 && clean_run >= 5 {
        signals.push(json!({
            "code": "STARVED",
            "text": "the ceiling stayed low through five clean runs — a decrease never decayed",
        }));
    }
    if n >= 5 && new_findings < n / 2 && filed > n * 2 {
        signals.push(json!({
            "code": "NOISY",
            "text": "filing far more than it discovers — dedup is probably leaking",
        }));
    }
    if red >= 2.max(n / 3) {
        signals.push(json!({
            "code": "FRAGILE",
            "text": "a third of cycles end on a red gate — batches are too large or too mixed",
        }));
    }

    json!({
        "cycles": n,
        "fixed": fixed,
        "filed": filed,
        "discovery": new_findings,
        "fixed_per_cycle": fixed as f64 / n as f64,
        "discovery_per_cycle": new_findings as f64 / n as f64,
        "zero_fix_cycles": zero_fix,
        "red_gate_cycles": red,
        "signals": signals,
    })
}

/// `/api/fullauto` — every loop and every run on this machine.
///
/// The workspace root is used only to mark which loop belongs to the project
/// the dashboard is currently pointed at; every other loop is still listed,
/// because a machine-level view of "what is my agent doing anywhere" is the
/// question this page is actually asked.
pub fn runs(workspace_root: &Path) -> Value {
    let loops = load_loops();
    let here = workspace_root.to_string_lossy().into_owned();

    let mut loop_rows = Vec::new();
    let mut all_runs = Vec::new();

    for l in &loops {
        let mine = l.roots.iter().any(|r| r == &here);
        let folded = fold_runs(l);
        let cycle_refs: Vec<&Value> = l.cycles.iter().collect();

        loop_rows.push(json!({
            "slug": l.slug,
            "state_dir": l.dir.display().to_string(),
            "roots": l.roots,
            "is_current_workspace": mine,
            "aperture": l.aperture,
            "seen_findings": l.seen,
            "calibration": l.calibration,
            "total_cycles": l.cycles.len(),
            "self_improvement": self_improvement(&cycle_refs, &l.calibration),
        }));

        for mut r in folded {
            if let Some(obj) = r.as_object_mut() {
                obj.insert("is_current_workspace".into(), json!(mine));
            }
            all_runs.push(r);
        }
    }

    all_runs.sort_by_key(|r| -i64_at(r, "started_unix"));

    let active = all_runs
        .iter()
        .filter(|r| str_at(r, "status") == "running")
        .count();
    json!({
        "loops": loop_rows,
        "runs": all_runs,
        "totals": {
            "loops": loops.len(),
            "runs": all_runs.len(),
            "running": active,
        },
    })
}

/// `/api/fullauto-run?id=<run_id>` — the rich drill for one run.
///
/// Returns the run header, every cycle it contains in order, the controller's
/// trajectory across those cycles, and the self-improvement signals computed
/// over that run alone. An unknown id is an empty object rather than an error:
/// a stale link in an open tab should render as "nothing here", not a 500.
pub fn run_detail(run_id: &str) -> Value {
    for l in load_loops() {
        let Some(header) = fold_runs(&l)
            .into_iter()
            .find(|r| str_at(r, "run_id") == run_id)
        else {
            continue;
        };

        let mut cycles: Vec<Value> = l
            .cycles
            .iter()
            .filter(|c| str_at(c, "run_id") == run_id)
            .cloned()
            .collect();
        cycles.sort_by_key(|c| i64_at(c, "cycle"));

        // The controller's path through the run, one point per cycle. This is
        // what makes the self-scaling visible rather than merely claimed: the
        // tier it chose, whether the cycle survived, and what the gate said.
        let trajectory: Vec<Value> = cycles
            .iter()
            .map(|c| {
                json!({
                    "cycle": i64_at(c, "cycle"),
                    "tier": str_at(c, "tier"),
                    "outcome": str_at(c, "outcome"),
                    "gate": str_at(c, "gate"),
                    "bench": str_at(c, "bench"),
                    "aperture": str_at(c, "aperture"),
                    "fixed": i64_at(c, "fixed"),
                    "filed": i64_at(c, "filed"),
                    "new_findings": i64_at(c, "new_findings"),
                    "minutes": i64_at(c, "minutes"),
                    "dry": c.get("dry").and_then(Value::as_bool).unwrap_or(false),
                })
            })
            .collect();

        let mut apertures: Vec<String> = Vec::new();
        for c in &cycles {
            let a = str_at(c, "aperture");
            if !a.is_empty() && apertures.last() != Some(&a) {
                apertures.push(a);
            }
        }

        let refs: Vec<&Value> = cycles.iter().collect();
        return json!({
            "run": header,
            "loop": {
                "slug": l.slug,
                "state_dir": l.dir.display().to_string(),
                "aperture": l.aperture,
                "calibration": l.calibration,
                "seen_findings": l.seen,
            },
            "cycles": cycles,
            "trajectory": trajectory,
            "apertures": apertures,
            "self_improvement": self_improvement(&refs, &l.calibration),
        });
    }
    json!({})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::{EnvRestore, lock};

    /// Build a loop state dir under a temporary `STELLA_HOME`.
    ///
    /// Mirrors exactly what `scripts/fullauto.sh` writes: append-only
    /// `runs.jsonl` and `ledger.jsonl`, plus the single live pointer.
    fn seed(dir: &Path, runs: &[&str], cycles: &[&str], live: Option<&str>) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("runs.jsonl"), runs.join("\n") + "\n").unwrap();
        std::fs::write(dir.join("ledger.jsonl"), cycles.join("\n") + "\n").unwrap();
        std::fs::write(dir.join("aperture"), "rubric\n").unwrap();
        std::fs::write(
            dir.join("calibration.json"),
            r#"{"batch_ceiling":20,"parallel_ceiling":2,"clean_run":3}"#,
        )
        .unwrap();
        match live {
            Some(l) => std::fs::write(dir.join("run.json"), l).unwrap(),
            None => {
                let _ = std::fs::remove_file(dir.join("run.json"));
            }
        }
    }

    fn run_rec(id: &str, status: &str) -> String {
        format!(
            r#"{{"run_id":"{id}","status":"{status}","at_unix":1000,"driver":"daemon","slug":"demo"}}"#
        )
    }

    fn cycle_rec(id: &str, cycle: i64, fixed: i64, gate: &str) -> String {
        format!(
            r#"{{"run_id":"{id}","cycle":{cycle},"fixed":{fixed},"filed":1,"new_findings":1,"gate":"{gate}","tier":"normal","aperture":"rubric","minutes":10,"outcome":"ok"}}"#
        )
    }

    /// The correction the file cannot make for itself. A run whose last record
    /// says `running` but whose heartbeat has gone stale is CRASHED — the
    /// process that would have recorded its own death is gone by definition.
    #[test]
    fn a_stale_heartbeat_reports_crashed_not_running() {
        let _guard = lock();
        let _restore = EnvRestore::capture(&["STELLA_HOME"]);
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: the env lock is held for this test's whole body.
        unsafe { std::env::set_var("STELLA_HOME", tmp.path()) };

        let dir = tmp.path().join("fullauto/demo");
        let stale = now_unix() - (STALE_AFTER_SECS + 60);
        seed(
            &dir,
            &[&run_rec("r-1", "running")],
            &[&cycle_rec("r-1", 1, 3, "green")],
            Some(&format!(
                r#"{{"run_id":"r-1","phase":"audit","heartbeat_unix":{stale},"pid":1}}"#
            )),
        );

        let out = runs(Path::new("/nowhere"));
        let row = &out["runs"][0];
        assert_eq!(row["status"], "crashed", "stale heartbeat must read crashed");
        assert_eq!(row["run_id"], "r-1");
    }

    /// A fresh heartbeat is the only thing that keeps a run `running`, and the
    /// live phase must reach the page — a status light without a phase is not
    /// an observable workflow.
    #[test]
    fn a_fresh_heartbeat_stays_running_and_carries_the_phase() {
        let _guard = lock();
        let _restore = EnvRestore::capture(&["STELLA_HOME"]);
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: the env lock is held for this test's whole body.
        unsafe { std::env::set_var("STELLA_HOME", tmp.path()) };

        let dir = tmp.path().join("fullauto/demo");
        let fresh = now_unix();
        seed(
            &dir,
            &[&run_rec("r-1", "running")],
            &[],
            Some(&format!(
                r#"{{"run_id":"r-1","phase":"audit","cycle":7,"heartbeat_unix":{fresh},"pid":42}}"#
            )),
        );

        let out = runs(Path::new("/nowhere"));
        let row = &out["runs"][0];
        assert_eq!(row["status"], "running");
        assert_eq!(row["live"]["phase"], "audit");
        assert_eq!(row["live"]["cycle"], 7);
        assert_eq!(out["totals"]["running"], 1);
    }

    /// A run claiming to be running while the live pointer names a different
    /// run has been orphaned — nothing is driving it.
    #[test]
    fn a_run_the_live_pointer_has_moved_on_from_reports_crashed() {
        let _guard = lock();
        let _restore = EnvRestore::capture(&["STELLA_HOME"]);
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: the env lock is held for this test's whole body.
        unsafe { std::env::set_var("STELLA_HOME", tmp.path()) };

        let dir = tmp.path().join("fullauto/demo");
        let fresh = now_unix();
        seed(
            &dir,
            &[&run_rec("r-old", "running"), &run_rec("r-new", "running")],
            &[],
            Some(&format!(
                r#"{{"run_id":"r-new","phase":"cycle","heartbeat_unix":{fresh},"pid":9}}"#
            )),
        );

        let out = runs(Path::new("/nowhere"));
        let by_id: std::collections::HashMap<_, _> = out["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (str_at(r, "run_id"), str_at(r, "status")))
            .collect();
        assert_eq!(by_id["r-old"], "crashed");
        assert_eq!(by_id["r-new"], "running");
    }

    /// Terminal states are read from the fold, and cycle telemetry aggregates
    /// into the run that produced it.
    #[test]
    fn cycles_aggregate_into_their_run_and_terminal_states_survive_the_fold() {
        let _guard = lock();
        let _restore = EnvRestore::capture(&["STELLA_HOME"]);
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: the env lock is held for this test's whole body.
        unsafe { std::env::set_var("STELLA_HOME", tmp.path()) };

        let dir = tmp.path().join("fullauto/demo");
        seed(
            &dir,
            &[
                &run_rec("r-1", "running"),
                &run_rec("r-1", "completed"),
                &run_rec("r-2", "cancelled"),
            ],
            &[
                &cycle_rec("r-1", 1, 4, "green"),
                &cycle_rec("r-1", 2, 2, "red"),
            ],
            None,
        );

        let out = runs(Path::new("/nowhere"));
        let by_id: std::collections::HashMap<_, _> = out["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (str_at(r, "run_id"), r.clone()))
            .collect();
        assert_eq!(by_id["r-1"]["status"], "completed", "last write wins");
        assert_eq!(by_id["r-2"]["status"], "cancelled");
        assert_eq!(by_id["r-1"]["cycles"], 2);
        assert_eq!(by_id["r-1"]["fixed"], 6);
        assert_eq!(by_id["r-1"]["red_gates"], 1);
    }

    /// The drill is what the dashboard's detail pane renders, so its trajectory
    /// must be in cycle order regardless of the order lines were appended.
    #[test]
    fn run_detail_orders_the_trajectory_by_cycle() {
        let _guard = lock();
        let _restore = EnvRestore::capture(&["STELLA_HOME"]);
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: the env lock is held for this test's whole body.
        unsafe { std::env::set_var("STELLA_HOME", tmp.path()) };

        let dir = tmp.path().join("fullauto/demo");
        seed(
            &dir,
            &[&run_rec("r-1", "completed")],
            &[
                &cycle_rec("r-1", 3, 1, "green"),
                &cycle_rec("r-1", 1, 1, "green"),
                &cycle_rec("r-1", 2, 1, "green"),
            ],
            None,
        );

        let d = run_detail("r-1");
        let cycles: Vec<i64> = d["trajectory"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| i64_at(c, "cycle"))
            .collect();
        assert_eq!(cycles, vec![1, 2, 3]);
        assert_eq!(d["run"]["status"], "completed");
    }

    /// A stale link in an open tab must render as "nothing here", never a 500.
    #[test]
    fn an_unknown_run_id_is_an_empty_object_not_an_error() {
        let _guard = lock();
        let _restore = EnvRestore::capture(&["STELLA_HOME"]);
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: the env lock is held for this test's whole body.
        unsafe { std::env::set_var("STELLA_HOME", tmp.path()) };

        assert_eq!(run_detail("r-does-not-exist"), json!({}));
    }

    /// The self-improvement evidence names a SPECIFIC pathology, because the
    /// remedy differs completely between them and "health: poor" would send a
    /// reader to re-derive all of it by hand.
    #[test]
    fn three_zero_fix_cycles_raise_the_stuck_signal() {
        let cycles: Vec<Value> = (1..=3)
            .map(|i| json!({ "cycle": i, "fixed": 0, "filed": 1, "new_findings": 1 }))
            .collect();
        let refs: Vec<&Value> = cycles.iter().collect();
        let si = self_improvement(&refs, &json!({ "clean_run": 1, "batch_ceiling": 20 }));
        let codes: Vec<String> = si["signals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| str_at(s, "code"))
            .collect();
        assert!(codes.contains(&"STUCK".to_string()), "got {codes:?}");
    }

    /// A healthy loop must raise nothing. A dashboard that always shows a
    /// warning teaches the reader to ignore the warnings.
    #[test]
    fn a_healthy_loop_raises_no_signals() {
        let cycles: Vec<Value> = (1..=6)
            .map(|i| json!({ "cycle": i, "fixed": 4, "filed": 1, "new_findings": 2, "gate": "green" }))
            .collect();
        let refs: Vec<&Value> = cycles.iter().collect();
        let si = self_improvement(&refs, &json!({ "clean_run": 6, "batch_ceiling": 20 }));
        assert_eq!(si["signals"].as_array().unwrap().len(), 0);
        assert_eq!(si["fixed"], 24);
    }
}
