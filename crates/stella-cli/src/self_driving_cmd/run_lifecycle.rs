//! The run ledger — one self-driving *run* opened, closed, and reported.
//!
//! A run is the outer bracket around the cycles `lifecycle` drives: `run_start`
//! mints the id and stamps the first heartbeat, `run_end_as` closes it with a
//! status and a reason, and `runs_report` folds the record stream into the
//! table `stella self-driving runs` prints. The fold itself is
//! `stella_autonomy::fold_runs` — pure and property-tested there, so everything
//! in this file is the I/O half invariant 2 keeps out of the engine.
//!
//! Split out of `self_driving_cmd.rs` when that file reached the 1500-line
//! ceiling (#4044), following `stella-core`'s `driver/settlement.rs`.

use std::collections::BTreeMap;

use serde_json::Value;

use super::state::LoopState;
use super::{say, state};
use crate::timefmt::{now_unix, rfc3339_utc_now};

pub(super) fn run_start(st: &LoopState) -> Result<(), String> {
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

pub(super) fn run_end(st: &LoopState, status: &str, reason: &str) -> Result<(), String> {
    run_end_as(st, status, reason)
}

pub(super) fn run_end_as(st: &LoopState, status: &str, reason: &str) -> Result<(), String> {
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

pub(super) fn runs_report(st: &LoopState) -> Result<(), String> {
    let rows = stella_autonomy::fold_runs(
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
