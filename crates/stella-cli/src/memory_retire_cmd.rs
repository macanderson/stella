//! `stella memory retired` and `stella memory reaffirm` — the human surface on
//! Phase 4's reversible retirement (#715 deliverable 5).
//!
//! These are the two halves the gate names: retired records are *still
//! explicitly retrievable* (`retired` lists them with their reasons and
//! evidence) and *restorable* (`reaffirm` returns one to automatic selection).
//! Neither command deletes anything.

use stella_context::ContextStore;
use stella_core::context_record::PromotionAction;

use crate::memory::{retirement, uses};

/// Open the workspace's context ledger, or report why not.
///
/// Read-only politeness, matching `stella memory forgotten`: a workspace with
/// no ledger reads as "nothing retired" rather than creating a database as a
/// side effect of asking a question.
fn open_ledger(workspace_root: &std::path::Path) -> Result<Option<ContextStore>, String> {
    let path = stella_store::existing_workspace_private_sqlite_path(workspace_root, "context.db")
        .map_err(|e| format!("cannot resolve context store: {e}"))?;
    let Some(path) = path else {
        return Ok(None);
    };
    ContextStore::open(&path)
        .map(Some)
        .map_err(|e| format!("cannot open context store: {e}"))
}

/// Now, as the ledger stores it.
fn now_rfc3339() -> String {
    stella_context::format_rfc3339(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0) as i64,
    )
}

/// `stella memory retired` — what this workspace stopped selecting, and why.
pub fn run_memory_retired() -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let Some(store) = open_ledger(&workspace_root)? else {
        println!("nothing retired in this workspace");
        return Ok(());
    };

    let standings = retirement::standings(&store);
    let mut retired: Vec<_> = standings
        .iter()
        .filter(|(_, event)| event.action == PromotionAction::Retired)
        .collect();
    if retired.is_empty() {
        println!("nothing retired in this workspace");
        return Ok(());
    }
    // Sorted so two runs print the same thing — the same determinism the fold
    // itself guarantees.
    retired.sort_by(|a, b| a.0.cmp(b.0));

    // The evidence behind each decision, so "why" is answerable here rather
    // than only in the ledger.
    let policy = crate::memory::tuning::selection_health_policy(&workspace_root);
    let health = uses::selection_health(&store, policy);

    println!("{} retired:", retired.len());
    for (record_id, event) in &retired {
        println!("  {record_id}");
        println!("    reason: {}", event.reason);
        if let Some(h) = health.iter().find(|h| &h.context_record_id == *record_id) {
            println!(
                "    evidence: {} of {} assessed uses not helpful, {} uses across {} task(s)",
                h.not_helpful, h.assessed_uses, h.uses, h.distinct_tasks
            );
        }
        println!("    restore: stella memory reaffirm {record_id}");
    }
    Ok(())
}

/// `stella memory retire <id> --reason` — a person's explicit judgement that a
/// memory has stopped earning its place.
///
/// This is the evidence tier the automatic sweep cannot reach on its own. A
/// citation is an `agent_self_report` and is deliberately not pruning-eligible
/// (see [`crate::memory::uses`]), so until a deterministic or external evidence
/// source is wired, a human is the one source strong enough to retire
/// something. That is the correct default: the loop shows its work and a person
/// decides.
pub fn run_memory_retire(id: &str, reason: &str) -> Result<(), String> {
    if reason.trim().is_empty() {
        return Err("a retirement must carry a reason".into());
    }
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let Some(store) = open_ledger(&workspace_root)? else {
        return Err("this workspace has no context ledger yet".into());
    };

    let standings = retirement::standings(&store);
    if let Some(protection) = retirement::protection_for(standings.get(id)) {
        return Err(format!(
            "{id} is {} and may not be retired",
            protection.as_str()
        ));
    }
    if !retirement::retire_explicitly(&store, id, reason, &now_rfc3339()) {
        return Err(format!("could not record the retirement of {id}"));
    }
    println!("retired {id} — `stella memory reaffirm {id}` restores it");
    Ok(())
}

/// `stella memory reaffirm <id>` — put a retired record back into selection.
pub fn run_memory_reaffirm(id: &str, reason: &str) -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let Some(store) = open_ledger(&workspace_root)? else {
        return Err(format!("{id} is not retired in this workspace"));
    };

    // Refuse rather than write a no-op event: a reaffirmation of something
    // that was never retired would sit in the log claiming a reversal that
    // never happened.
    if !retirement::retired_ids(&store).contains(id) {
        return Err(format!("{id} is not retired — nothing to reaffirm"));
    }

    let reason = if reason.trim().is_empty() {
        "reaffirmed by the user"
    } else {
        reason
    };
    if !retirement::reaffirm(&store, id, reason, &now_rfc3339()) {
        return Err(format!("could not record the reaffirmation of {id}"));
    }
    println!("reaffirmed {id} — it will be selected again");
    Ok(())
}
