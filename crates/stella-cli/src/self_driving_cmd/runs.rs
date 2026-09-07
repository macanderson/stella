//! Starting, ending and listing a **run**.
//!
//! A run spans many cycles. A person starts one and stops it.
//!
//! Each run is a row in `runs.jsonl` plus a live doc the phase stamps write
//! to. `stella_autonomy::fold_runs` turns both into the table [`report`]
//! prints. Nothing here folds anything.
//!
//! Split from `self_driving_cmd.rs` at the gate's line ceiling. The seam is
//! the banner that file drew around these verbs.

use std::collections::BTreeMap;

use serde_json::Value;

use super::state::LoopState;
use super::{hooks, say, state};
use crate::timefmt::{now_unix, rfc3339_utc_now};

/// Open a run. Bank its first record, stamp a heartbeat, and print the id
/// every later verb writes under.
pub(super) fn start(st: &LoopState) -> Result<(), String> {
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

    hooks::drive(
        &st.repo_root,
        &hooks::settings_for(&st.repo_root),
        hooks::HookEvent::DriveRunStart,
        hooks::HookRunInfo::new(&rid),
        None,
    );

    say(&format!("run {rid} started"));
    println!("SELF_DRIVING_RUN_ID={rid}");
    Ok(())
}

/// Close the live run with a status and a reason.
pub(super) fn end(st: &LoopState, status: &str, reason: &str) -> Result<(), String> {
    let rid = st
        .current_run_id()
        .ok_or_else(|| "run end: no run in progress".to_string())?;
    let mut fields = BTreeMap::new();
    fields.insert("run_id".to_string(), Value::String(rid.clone()));
    fields.insert("status".to_string(), Value::String(status.to_string()));
    fields.insert("reason".to_string(), Value::String(reason.to_string()));
    st.append_run_record(fields)?;
    st.clear_run_doc();
    // After the record is banked and before the line is printed, so a
    // subscriber that reads the ledger on being woken finds this run's ending
    // already in it.
    hooks::drive(
        &st.repo_root,
        &hooks::settings_for(&st.repo_root),
        hooks::HookEvent::DriveRunEnd,
        hooks::HookRunInfo::new(&rid),
        Some(format!("{status}: {reason}")),
    );
    say(&format!("run {rid} -> {status}"));
    Ok(())
}

/// The run a lifecycle event belongs to.
///
/// A run that was never opened still has events worth reporting. `cycle
/// begin` outside `run start` is a normal way to drive the loop by hand. So
/// the id falls back to `-` and the event is still reported. A `CycleRecord`
/// written outside a run carries the same mark.
pub(super) fn info(st: &LoopState) -> hooks::HookRunInfo {
    hooks::HookRunInfo::new(st.current_run_id().unwrap_or_else(|| "-".to_string()))
}

/// Every run, folded: status, cycles, yield, live phase.
pub(super) fn report(st: &LoopState) -> Result<(), String> {
    let rows = stella_autonomy::fold_runs(
        &st.run_records(),
        &st.cycles().rows,
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
