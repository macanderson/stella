//! Proposal induction — the typed half of the skills loop.
//!
//! ## What is reused, verbatim
//!
//! **The miner.** `stella_core::skills::mine_skill_candidates` is called
//! unchanged, with unchanged [`SkillMineConfig`] thresholds, over observations
//! carrying the same `text`, `reference`, `occurred_at`, and `domains` the
//! lexical path fed it. Same clustering, same `<slug>-<hash8>` identity, same
//! ranking, same `already_captured` guard. That is not laziness: the candidate
//! id is the *skill filename*, so re-deriving it differently would orphan every
//! skill file a user already has, and the rendered `## Evidence` lines quote
//! `reference` and `occurred_at` directly, so changing either would change the
//! bytes of files already on disk.
//!
//! ## What is added
//!
//! A durable [`ProposalRecord`] per candidate, carrying its supporting
//! observations, its **distinct-task count**, and its scored components — the
//! thing a reviewer can look at before a skill file exists, which is the part
//! users cannot do today.
//!
//! ## Distinct tasks, never raw events
//!
//! The miner counts cluster members, which are events. The proposal counts the
//! distinct `task_id`s behind those members. Only the latter gates
//! ([`ProposalRecord::is_eligible`]), so thirty repetitions inside one task
//! produce a proposal that is recorded and visible but never eligible — spec §7.

use std::collections::{BTreeSet, HashMap};

use stella_context::{ContextStore, LedgerAppend};
use stella_core::context_record::{
    ContextRecordKind, LIFECYCLE_SCHEMA_VERSION, ObservationRecord, ProposalRecord, ProposalScore,
    RecordProposalKind, RecordProposalStatus, confidence_from_score,
};
use stella_core::skills::{self, Skill, SkillCandidate, SkillMineConfig, SkillObservation};

/// A mined candidate together with the durable proposal recorded for it.
pub(crate) struct InducedProposal {
    pub candidate: SkillCandidate,
    pub proposal: ProposalRecord,
}

/// The `occurred_at` a reflection observation was minted with.
///
/// Recovered from `source_ref` (`reflection:<unix secs>`) rather than from
/// `observed_at`, because this exact integer is rendered into a skill file's
/// `## Evidence` lines. Re-deriving it from the RFC 3339 timestamp would round-
/// trip through a different representation and risk moving a byte in a file
/// already on disk, which is the one thing the migration may not do.
fn occurred_at_of(observation: &ObservationRecord) -> u64 {
    observation
        .source_ref
        .rsplit(':')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Project typed observations onto the shape the shipped miner consumes.
///
/// `salient` is `false` for every reflection lesson, exactly as the lexical path
/// hard-coded it. Nothing currently marks a reflection salient, and inventing a
/// salience signal here would change the promotion bar as a side effect of a
/// migration.
fn as_skill_observations(observations: &[ObservationRecord]) -> Vec<SkillObservation> {
    observations
        .iter()
        .map(|o| SkillObservation {
            text: o.text.clone(),
            occurred_at: occurred_at_of(o),
            domains: o.domains.clone(),
            salient: false,
            reference: o.source_ref.clone(),
        })
        .collect()
}

/// Every task id seen under each evidence reference.
///
/// Keyed by `(reference, text)` rather than by reference alone: one turn can
/// emit several different lessons, so a reference is not unique. The pair is
/// what the miner's evidence entries carry back.
fn tasks_by_evidence(observations: &[ObservationRecord]) -> HashMap<(String, String), String> {
    observations
        .iter()
        .map(|o| ((o.source_ref.clone(), o.text.clone()), o.task_id.clone()))
        .collect()
}

/// Mine `observations` into candidates and record a durable proposal for each.
///
/// Returns every induced proposal, eligible or not. The caller decides what to
/// do with them; recording an ineligible proposal is deliberate, because "this
/// recurred but only inside one task" is exactly the kind of thing a reviewer
/// should be able to see.
pub(crate) fn induce_proposals(
    store: &ContextStore,
    observations: &[ObservationRecord],
    existing: &[Skill],
    config: &SkillMineConfig,
) -> Vec<InducedProposal> {
    let task_index = tasks_by_evidence(observations);
    let candidates =
        skills::mine_skill_candidates(as_skill_observations(observations), existing, config);

    let mut induced = Vec::new();
    for candidate in candidates {
        // The observations behind this candidate, resolved back through the
        // evidence the miner recorded.
        let mut tasks: BTreeSet<&str> = BTreeSet::new();
        let mut supporting: Vec<String> = Vec::new();
        for evidence in &candidate.evidence {
            // The miner truncates the snippet to 160 chars; match on the same
            // prefix so a long lesson still resolves.
            let key = observations.iter().find(|o| {
                o.source_ref == evidence.reference
                    && o.text.chars().take(160).collect::<String>() == evidence.snippet
            });
            if let Some(observation) = key {
                tasks.insert(observation.task_id.as_str());
                supporting.push(observation.record_id.clone());
            } else if let Some(task) = task_index.get(&(evidence.reference.clone(), String::new()))
            {
                tasks.insert(task.as_str());
            }
        }

        let score = ProposalScore {
            occurrences: candidate.occurrences as u32,
            distinct_tasks: tasks.len() as u32,
            salient: candidate.salient,
            rank: candidate.score,
        };
        let Ok(confidence) = confidence_from_score(&score) else {
            continue;
        };
        // A proposal's status reflects only what the evidence supports.
        // Activation and rejection are promotion-event outcomes, never proposal
        // states (`RecordProposalStatus` says so explicitly).
        let status = if score.distinct_tasks >= 3 && score.occurrences >= 3 {
            RecordProposalStatus::Eligible
        } else {
            RecordProposalStatus::Collecting
        };
        let Ok(proposal) = ProposalRecord::new(
            RecordProposalKind::Knowledge,
            status,
            &candidate.name,
            &candidate.description,
            &candidate.body,
            candidate.domains.clone(),
            supporting,
            score,
            confidence,
            proposal_observed_at(&candidate),
        ) else {
            continue;
        };
        // Best-effort: a proposal that cannot be recorded still gets returned,
        // because failing to persist the audit trail must not also cost the
        // user the skill. The honesty invariant is satisfied by the caller
        // reporting nothing it did not do — not by dropping the work.
        let _ = record_proposal(store, &proposal);
        induced.push(InducedProposal {
            candidate,
            proposal,
        });
    }
    induced
}

/// The proposal's `observed_at` — the newest supporting evidence, which is what
/// "when did this become true" means for an induced record.
fn proposal_observed_at(candidate: &SkillCandidate) -> String {
    let newest = candidate
        .evidence
        .iter()
        .map(|e| e.occurred_at)
        .max()
        .unwrap_or(0);
    stella_context::format_rfc3339(newest as i64)
}

/// Append a proposal to the ledger.
pub(crate) fn record_proposal(
    store: &ContextStore,
    proposal: &ProposalRecord,
) -> Option<stella_context::AppendOutcome> {
    let body = serde_json::to_string(proposal).ok()?;
    store
        .append_record(LedgerAppend {
            record_id: &proposal.record_id,
            lineage_id: &proposal.lineage_id,
            record_kind: ContextRecordKind::RecordProposal.as_str(),
            record_hash: &proposal.record_hash,
            schema_version: LIFECYCLE_SCHEMA_VERSION,
            body: &body,
            observed_at: &proposal.observed_at,
            supersedes: None,
        })
        .ok()
}

/// Every proposal in the ledger, oldest first.
pub(crate) fn all_proposals(store: &ContextStore, limit: usize) -> Vec<ProposalRecord> {
    store
        .records_of_kind(ContextRecordKind::RecordProposal.as_str(), limit)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| serde_json::from_str::<ProposalRecord>(&row.body).ok())
        .collect()
}

#[cfg(test)]
mod tests;
