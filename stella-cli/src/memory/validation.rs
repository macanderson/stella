//! Deterministic validation — the first pruning-eligible evidence source
//! (#753).
//!
//! Phase 4 (#715) shipped the whole retirement mechanism, but every verdict the
//! turn loop writes is `agent_self_report`, which is recognized evidence and
//! deliberately never pruning-eligible — so the automatic sweep had nothing it
//! was allowed to act on, and the only retirements were human ones via
//! `stella memory retire`. This module closes that gap with the cheapest
//! non-self-report source there is: a memory whose file-path anchors have *all*
//! left the tree is deterministically stale — checkable by anyone, reproducible,
//! no model involved. That is exactly
//! [`ContextEvaluationMethod::DETERMINISTIC_VALIDATION`], which is
//! pruning-eligible.
//!
//! Two things happen for such a memory, and they are deliberately separate:
//!
//! 1. **A verdict is recorded** — a [`ContextUseFeedback`] with
//!    `NotHelpful` / `deterministic_validation` / `had_opportunity: true`,
//!    attached to the memory's most recent recorded use and naming the missing
//!    paths as `observable_effect_refs`. This is the ledger evidence: selection
//!    health picks it up and displays it, and "at least one
//!    non-`agent_self_report` source writes verdicts" holds.
//! 2. **Retirement fires directly** via [`super::retirement::sweep_vanished`],
//!    which runs the same single protection predicate and writes the same
//!    reversible `PromotionEvent` as the health-driven sweep. It does not wait
//!    for the health fold's `min_attributable_uses` floor — that floor exists
//!    because N model judgements of one turn each carry noise, and re-running a
//!    deterministic file-existence check five times adds no information the
//!    first run didn't have.
//!
//! The check itself must stay conservative in one direction: **all** anchors
//! gone, not some. A memory naming three files of which one was renamed is a
//! memory that is partially right, and partially right is a human's call
//! (`stella memory validate` reports it; nothing acts on it).
//!
//! `agent_self_report` remains excluded and the pruning-eligibility tier is
//! untouched — this adds a source, it does not relax the policy.

use std::path::Path;

use stella_context::ContextStore;
use stella_core::context_record::{
    Confidence, ContextEvaluationMethod, ContextInfluenceStage, ContextOutcomeRelation,
    ContextRecordKind, ContextUse, ContextUseEvaluation, ContextUseFeedback,
};

use super::anchors::{extract_path_anchors, workspace_file_names};

/// A memory none of whose path anchors still exists in the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VanishedMemory {
    /// The memory's public id (`nod_…`) — the lineage the retirement names.
    pub(crate) public_id: String,
    /// Every anchor the memory carries; all of them are gone.
    pub(crate) missing_paths: Vec<String>,
}

/// Every recallable memory whose anchors have *all* vanished from the tree.
///
/// A memory with no anchors is unverifiable, not vanished; a memory with any
/// surviving anchor is partially right, and neither is returned. Errors read
/// as "nothing vanished" — this feeds a best-effort background loop that must
/// never fail a turn.
pub(crate) fn vanished_memories(
    context: &ContextStore,
    workspace_root: &Path,
) -> Vec<VanishedMemory> {
    let Ok(nodes) = context.recallable_nodes() else {
        return Vec::new();
    };
    // Built lazily, exactly as `validate_memories` does: a workspace whose
    // memories name no bare filenames never pays for the walk.
    let mut file_names: Option<std::collections::HashSet<String>> = None;
    let mut out = Vec::new();
    for node in &nodes {
        let anchors = extract_path_anchors(&node.content);
        if anchors.is_empty() {
            continue;
        }
        let all_missing = anchors.iter().all(|anchor| {
            if anchor.contains('/') {
                !workspace_root.join(anchor).exists()
            } else {
                !file_names
                    .get_or_insert_with(|| workspace_file_names(workspace_root))
                    .contains(anchor.as_str())
            }
        });
        if all_missing {
            out.push(VanishedMemory {
                public_id: node.public_id.clone(),
                missing_paths: anchors,
            });
        }
    }
    out
}

/// How many use/feedback records the verdict writer reads. Same bound and
/// reasoning as [`super::uses`]'s health read: the ledger grows monotonically
/// and this runs on a background loop.
const READ_LIMIT: usize = 20_000;

/// Record one `deterministic_validation` verdict per vanished memory, attached
/// to the memory's most recent recorded use. Returns how many were written.
///
/// At most one deterministic verdict is ever written per use — the check is
/// reproducible, so re-asserting it on every pass would manufacture evidence
/// multiplicity out of a single fact about the tree. A memory with no recorded
/// use gets no verdict (there is no opportunity to attach it to); it is still
/// retired by [`super::retirement::sweep_vanished`], whose event carries the
/// missing paths.
pub(crate) fn record_vanished_verdicts(
    context: &ContextStore,
    vanished: &[VanishedMemory],
    observed_at: &str,
) -> usize {
    if vanished.is_empty() {
        return 0;
    }
    let uses: Vec<(String, ContextUse)> = context
        .records_of_kind(ContextRecordKind::ContextUse.as_str(), READ_LIMIT)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| {
            serde_json::from_str::<ContextUse>(&row.body)
                .ok()
                .map(|body| (row.record_id, body))
        })
        .collect();
    let existing: Vec<ContextUseFeedback> = context
        .records_of_kind(ContextRecordKind::ContextUseFeedback.as_str(), READ_LIMIT)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| serde_json::from_str(&row.body).ok())
        .collect();

    let mut written = 0usize;
    for memory in vanished {
        // The most recent use: `records_of_kind` is `observed_at`-ordered, so
        // the last matching row is the latest.
        let Some((use_id, use_record)) = uses
            .iter()
            .filter(|(_, u)| u.context_record_id == memory.public_id)
            .next_back()
        else {
            continue;
        };
        let already = existing.iter().any(|f| {
            f.context_use_id == *use_id
                && f.evaluation_method.as_ref().is_some_and(|m| {
                    m.as_str() == ContextEvaluationMethod::DETERMINISTIC_VALIDATION
                })
        });
        if already {
            continue;
        }
        let feedback = ContextUseFeedback {
            context_use_id: use_id.clone(),
            use_trace_id: use_record.use_trace_id.clone(),
            task_id: use_record.task_id.clone(),
            evaluation: ContextUseEvaluation::NotHelpful,
            had_opportunity: true,
            // The record was rendered (opportunity), but which stage a
            // vanished-anchor memory influenced is unknowable after the fact —
            // claiming one would be invention.
            influence_stage: ContextInfluenceStage::None,
            outcome_relation: ContextOutcomeRelation::Unknown,
            observable_effect_refs: memory
                .missing_paths
                .iter()
                .map(|p| format!("path-missing:{p}"))
                .collect(),
            evaluation_method: ContextEvaluationMethod::new(
                ContextEvaluationMethod::DETERMINISTIC_VALIDATION,
            )
            .ok(),
            attribution_confidence: deterministic_attribution_confidence(),
            outcome_assessment_id: None,
            influence_statement: None,
            observed_at: observed_at.to_string(),
        };
        if super::uses::append_feedback(context, &feedback)
            == Some(stella_context::AppendOutcome::Appended)
        {
            written += 1;
        }
    }
    written
}

/// The confidence a deterministic file-existence check carries: the maximum.
/// Unlike a citation (50 — the model referenced the record, nothing more),
/// "every path this memory names is gone" is a reproducible fact about the
/// tree, not a judgement.
fn deterministic_attribution_confidence() -> Confidence {
    Confidence::new(100).expect("100 is within 0..=100")
}

#[cfg(test)]
mod tests;
