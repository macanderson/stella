//! `stella proposals` — the review surface.
//!
//! The observable this phase exists for: a proposal is reviewable **before** it
//! becomes a skill, which is the part users cannot do today. `list` shows what
//! the loop wants to make durable and the evidence behind it; `keep`, `edit`,
//! and `ignore` decide, and each decision is an immutable promotion event in the
//! lifecycle ledger.
//!
//! ## Governance lives here, and it is not configurable
//!
//! Three of spec §5.4's authority rules are enforced on this path:
//!
//! * **an inferred record starts advisory and can never start blocking.** Every
//!   `keep` and `edit` records `Advisory`. There is no flag to change that —
//!   `PromotionEventRecord::new` refuses a system-granted blocking enforcement
//!   outright, so the rule holds even against a future caller here.
//! * **scope and sharing never widen automatically.** Nothing on this path
//!   writes a sharing scope at all. A proposal induced from local evidence stays
//!   local; publication is out of scope for this phase (spec §9) and there is no
//!   code path to it.
//! * **a declined proposal does not come back next turn.** `ignore` writes a
//!   `Rejected` event, and induction consults those events as a cooldown.
//!
//! ## Why every outcome is an event rather than a status column
//!
//! A mutable status answers "what is the state now" and destroys "how did it get
//! there". The gate criterion is that Keep/Edit/Ignore *replay identically from
//! the event log*, which is only meaningful if the log is the authority. So a
//! decision appends; nothing is ever updated.

use clap::Subcommand;
use colored::Colorize;

use stella_context::{ContextStore, LedgerAppend};
use stella_core::context_record::{
    ContextRecordKind, DirectiveEnforcement, LIFECYCLE_SCHEMA_VERSION, PromotionAction,
    PromotionActor, PromotionEventRecord, ProposalRecord,
};

/// Subcommands under `stella proposals`.
#[derive(Subcommand, Clone)]
pub enum ProposalsCmd {
    /// List proposals the loop has induced, with the evidence behind each.
    List {
        /// Include proposals already decided on.
        #[arg(long)]
        all: bool,
        /// Maximum proposals to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Accept a proposal as written. Recorded as an advisory confirmation.
    Keep {
        /// The proposal's candidate id, as shown by `list`.
        id: String,
        /// Why — recorded on the event, because a governance decision with no
        /// reason is not auditable.
        #[arg(long, default_value = "kept from the review surface")]
        reason: String,
    },
    /// Accept a proposal with different wording.
    Edit {
        /// The proposal's candidate id, as shown by `list`.
        id: String,
        /// The replacement text.
        body: String,
        #[arg(long, default_value = "edited from the review surface")]
        reason: String,
    },
    /// Decline a proposal. It will not be re-proposed while the decision
    /// stands.
    Ignore {
        /// The proposal's candidate id, as shown by `list`.
        id: String,
        #[arg(long, default_value = "declined from the review surface")]
        reason: String,
    },
    /// Extract observations from the reflection log and induce proposals over
    /// them, without waiting for a turn to reflect.
    ///
    /// The loop already does this after every reflection. This verb exists so
    /// the review surface is usable on its own: "show me what is pending" is a
    /// useless question if the only way to populate it is to run a turn and
    /// hope. Replay-idempotent like the loop's own pass — running it twice
    /// changes nothing.
    Refresh,
}

/// How many records a review read will pull. Generous — the ledger is local and
/// a person reviewing proposals wants to see all of them, not a page.
const READ_LIMIT: usize = 2_000;

/// Run a `stella proposals` subcommand against the workspace's context store.
pub fn run_proposals(cmd: &ProposalsCmd) -> Result<(), String> {
    let workspace_root = std::env::current_dir().map_err(|e| e.to_string())?;
    let db_path = crate::memory::context_db_path(&workspace_root)?;
    let store = ContextStore::open(&db_path).map_err(|e| e.to_string())?;

    match cmd {
        ProposalsCmd::List { all, limit } => list(&store, *all, *limit),
        ProposalsCmd::Keep { id, reason } => decide(
            &store,
            id,
            PromotionAction::Confirmed,
            Some(DirectiveEnforcement::Advisory),
            None,
            reason,
        ),
        ProposalsCmd::Edit { id, body, reason } => decide(
            &store,
            id,
            PromotionAction::Confirmed,
            Some(DirectiveEnforcement::Advisory),
            Some(body.clone()),
            reason,
        ),
        ProposalsCmd::Ignore { id, reason } => {
            decide(&store, id, PromotionAction::Rejected, None, None, reason)
        }
        ProposalsCmd::Refresh => refresh(&store, &workspace_root),
    }
}

/// Run one extraction + induction pass over the reflection log.
///
/// Unlike the loop's own pass this reports what it did rather than staying
/// silent, because a person typed it and a command that prints nothing is
/// indistinguishable from one that failed.
fn refresh(store: &ContextStore, workspace_root: &std::path::Path) -> Result<(), String> {
    let log_path = stella_store::workspace_private_state_path(workspace_root, "reflections.jsonl")
        .map_err(|e| format!("cannot resolve the reflection log: {e}"))?;
    if !log_path.exists() {
        println!(
            "  {} no reflection log yet — nothing to mine.",
            "·".dimmed()
        );
        return Ok(());
    }

    let extracted = crate::memory::observations::extract_reflection_observations(store, &log_path);
    let observations = crate::memory::observations::all_observations(store, READ_LIMIT);
    let before = current_proposals(store).len();
    crate::memory::proposals::induce_proposals(
        store,
        &observations,
        &[],
        &stella_core::skills::SkillMineConfig::default(),
    );
    let after = current_proposals(store).len();

    println!(
        "  {} {extracted} new observation(s), {} observation(s) total, \
         {} proposal(s) ({} new)",
        "✦".magenta(),
        observations.len(),
        after,
        after.saturating_sub(before)
    );
    Ok(())
}

/// The newest revision of each proposal lineage, newest lineage first.
///
/// Reads every revision and folds rather than asking the ledger for "the
/// latest": revisions are append-only, so "latest" is a property of the read,
/// and computing it here keeps the storage layer free of policy.
pub(crate) fn current_proposals(store: &ContextStore) -> Vec<ProposalRecord> {
    let mut latest: std::collections::BTreeMap<String, ProposalRecord> =
        std::collections::BTreeMap::new();
    for proposal in crate::memory::proposals::all_proposals(store, READ_LIMIT) {
        latest.insert(proposal.lineage_id.clone(), proposal);
    }
    let mut out: Vec<ProposalRecord> = latest.into_values().collect();
    out.sort_by(|a, b| {
        b.observed_at
            .cmp(&a.observed_at)
            .then_with(|| a.candidate_id.cmp(&b.candidate_id))
    });
    out
}

/// Every promotion event in **append order** — the replay source.
///
/// Append order, not `observed_at` order. Two decisions made in the same second
/// carry the same `observed_at`, and the secondary sort is a content hash, so an
/// `observed_at` read would order "ignore then keep" by hash rather than by what
/// the user actually did last. That is not a flake — it is a deterministic wrong
/// answer, which a test only catches if it happens to pick colliding ids.
pub(crate) fn promotion_events(store: &ContextStore) -> Vec<PromotionEventRecord> {
    store
        .records_of_kind_in_append_order(ContextRecordKind::PromotionEvent.as_str(), READ_LIMIT)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| serde_json::from_str::<PromotionEventRecord>(&row.body).ok())
        .collect()
}

/// The decision standing against each proposal lineage, replayed from the log.
///
/// Last write wins, which is the whole state machine: a user who ignores a
/// proposal and later keeps it has kept it, and both acts remain readable.
pub(crate) fn decisions(
    store: &ContextStore,
) -> std::collections::HashMap<String, PromotionAction> {
    let mut out = std::collections::HashMap::new();
    for event in promotion_events(store) {
        out.insert(event.proposal_lineage_id.clone(), event.action);
    }
    out
}

fn list(store: &ContextStore, all: bool, limit: usize) -> Result<(), String> {
    let decided = decisions(store);
    let proposals = current_proposals(store);
    let mut shown = 0usize;

    for proposal in &proposals {
        let decision = decided.get(&proposal.lineage_id);
        if !all && decision.is_some() {
            continue;
        }
        if shown >= limit {
            break;
        }
        shown += 1;

        let status = match decision {
            Some(PromotionAction::Rejected) => "ignored".red().to_string(),
            Some(action) => action.as_str().green().to_string(),
            None if proposal.is_eligible(3, 3) => "eligible".bright_magenta().to_string(),
            None => "collecting".dimmed().to_string(),
        };
        println!(
            "\n{} {}  [{status}]",
            "✦".magenta(),
            proposal.candidate_id.bold()
        );
        println!("  {}", proposal.body);
        // The explanation the phase promises: "three separate tasks, here they
        // are." Distinct tasks first, because that is the number that gates.
        println!(
            "  {} {} distinct task(s), {} observation(s), confidence {}",
            "why:".dimmed(),
            proposal.score.distinct_tasks,
            proposal.score.occurrences,
            proposal.confidence.get()
        );
        for observation in &proposal.supporting_observations {
            println!("    {} {observation}", "·".dimmed());
        }
    }

    if shown == 0 {
        println!("  {} no proposals awaiting review.", "·".dimmed());
        if !all && !proposals.is_empty() {
            println!(
                "  {} re-run with --all to include decided ones.",
                "·".dimmed()
            );
        }
    }
    Ok(())
}

fn decide(
    store: &ContextStore,
    candidate_id: &str,
    action: PromotionAction,
    enforcement: Option<DirectiveEnforcement>,
    edited_body: Option<String>,
    reason: &str,
) -> Result<(), String> {
    let proposal = current_proposals(store)
        .into_iter()
        .find(|p| p.candidate_id == candidate_id)
        .ok_or_else(|| {
            format!(
                "no proposal with candidate id `{candidate_id}` — run \
                 `stella proposals list` to see what is awaiting review"
            )
        })?;

    // `PromotionActor::User` because a person typed this command. That is also
    // the only actor permitted to grant blocking enforcement — and this path
    // never asks for it, so an inferred record cannot reach blocking here even
    // by mistake.
    let event = PromotionEventRecord::new(
        &proposal.lineage_id,
        action,
        PromotionActor::User,
        enforcement,
        edited_body,
        reason,
        stella_context::format_rfc3339(crate::memory::unix_now_secs()),
    )
    .map_err(|e| e.to_string())?;
    record_event(store, &event)?;

    println!(
        "  {} {} {}",
        "✓".green(),
        action.as_str(),
        proposal.candidate_id.bold()
    );
    Ok(())
}

/// Append a promotion event to the ledger.
pub(crate) fn record_event(
    store: &ContextStore,
    event: &PromotionEventRecord,
) -> Result<(), String> {
    let body = serde_json::to_string(event).map_err(|e| e.to_string())?;
    store
        .append_record(LedgerAppend {
            record_id: &event.record_id,
            lineage_id: &event.lineage_id,
            record_kind: ContextRecordKind::PromotionEvent.as_str(),
            record_hash: &event.record_hash,
            schema_version: LIFECYCLE_SCHEMA_VERSION,
            body: &body,
            observed_at: &event.occurred_at,
            supersedes: None,
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Record a decision against a proposal, for tests in sibling modules that need
/// to drive the review surface without a terminal.
///
/// Deliberately narrow: it takes an action and a reason and nothing else, so it
/// cannot be used to reach an enforcement level the real surface refuses.
#[cfg(test)]
pub(crate) fn decide_for_test(
    store: &ContextStore,
    candidate_id: &str,
    action: PromotionAction,
    reason: &str,
) -> Result<(), String> {
    let enforcement =
        (action == PromotionAction::Confirmed).then_some(DirectiveEnforcement::Advisory);
    decide(store, candidate_id, action, enforcement, None, reason)
}

#[cfg(test)]
mod tests;
