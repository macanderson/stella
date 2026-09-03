// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Asking the loop where it stands, and telling it.
//!
//! The four read/adjust verbs `self_driving_cmd.rs` already grouped under one
//! banner — `aperture / seen / calibrate / queue`. None of them drives a turn
//! or touches a pull request: each reports a facet of the loop's own state, or
//! nudges that state by hand. They are the surface an operator uses between
//! runs, which is why they read together and why they move together.
//!
//! Split out at the gate's 1500-line ceiling (#5225). The seam is the one the
//! file drew for itself; this change only made it a module boundary.

use stella_autonomy::{CycleOutcome, Tooling};

use super::{LoopState, QueryFormat, advance_aperture, backlog, collapse_ws, config, state};

pub(super) fn aperture(
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
        for l in stella_autonomy::LENSES {
            let marker = if l.name == open { "*" } else { " " };
            let heavy = if l.heavy_only { " [heavy tier]" } else { "" };
            let backing = match l.tooling {
                Tooling::Command { run, .. } => collapse_ws(run),
                Tooling::ModelOnly { note } => format!("model-only — {}", collapse_ws(note)),
            };
            println!("  {marker} {:<13} {backing}{heavy}", l.name);
        }
        let marker = if open == stella_autonomy::WATCH {
            "*"
        } else {
            " "
        };
        println!(
            "  {marker} {:<13} cheap sentinels; any trigger reopens rubric",
            stella_autonomy::WATCH
        );
    }
    Ok(())
}

pub(super) fn seen(
    st: &LoopState,
    digest: &[String],
    new: &[String],
    add: &[String],
    count: bool,
) -> Result<(), String> {
    if !digest.is_empty() {
        println!("{}", stella_autonomy::finding_digest(&digest.join(" ")));
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

pub(super) fn calibrate_cmd(
    st: &LoopState,
    ok: bool,
    resource_fail: bool,
    show: bool,
) -> Result<(), String> {
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
    stella_autonomy::calibrate(&mut cal, outcome, &state::aimd_limits());
    st.write_calibration(&cal)?;
    println!(
        "{}",
        serde_json::to_string(&cal).map_err(|e| e.to_string())?
    );
    Ok(())
}

/// The queue verb: the ranked defect batch this cycle draws from.
pub(super) fn queue(st: &LoopState, limit: usize, format: QueryFormat) -> Result<(), String> {
    backlog::render_queue(
        st,
        &crate::issue_provider::GhIssueProvider,
        &config::load(&st.repo_root),
        limit,
        format,
    )
}
