//! Wiring the rules miner — deliverable 5.
//!
//! `stella_core::rules::mine_candidates` is the structural twin of the skills
//! miner. It has shipped with **zero non-test references** in the workspace
//! since `crate::mining` was extracted specifically to keep the two aligned:
//! the shared module exists so a stopword or slug tweak cannot land in one
//! miner and miss the other, which only pays off if both actually run.
//!
//! This routes it through the same typed path the skills loop uses — the same
//! observations, the same ledger, the same proposal record, the same review
//! surface — rather than bolting on a second mechanism. That is the point of
//! the shared module restated at the next layer up: one observation pool, one
//! proposal kind per artifact, one governance path.
//!
//! ## Directives steer, so they are governed harder than skills
//!
//! A skill informs; a rule is injected into the system prefix as an
//! instruction. Spec §5.4 puts inferred directives under the stricter regime,
//! and two rules follow:
//!
//! 1. **The inferred guard is stripped, always.** `rules::infer_guard` can
//!    derive a `RuleGuard` from consistent file evidence, and a rule carrying
//!    one is Tier 2: `evaluate_guards` *denies the tool call*. Writing an
//!    inferred guard would mint a blocking directive from inference alone,
//!    which is exactly what "no inferred directive reaches blocking by any
//!    path" forbids. It is stripped here, unconditionally, and tested — not
//!    left to the accident that reflection observations happen to carry no
//!    files today.
//! 2. **Auto-activation needs real confidence.** A skill lands as soon as it is
//!    eligible. A rule additionally needs `confidence >=
//!    context.promotion.inferred_directive.auto_activate_at_confidence` (85 by
//!    default), which three observations across three tasks do not reach — they
//!    score 70. Below that bar the proposal is recorded and waits for an
//!    explicit Keep. So the common case for a directive is *review*, and
//!    auto-activation is reserved for evidence that is genuinely strong.

use std::path::Path;

use stella_context::ContextStore;
use stella_core::context_record::{
    EvidencePool, ObservationRecord, ProposalRecord, ProposalScore, RecordProposalKind,
    RecordProposalStatus, confidence_from_score,
};
use stella_core::rules::{self, EvidenceSource, MineConfig, RawObservation, Rule, RuleCandidate};
use stella_protocol::provenance::ProvenanceGrade;

use super::proposals::record_proposal;

/// A mined rule candidate together with the durable proposal recorded for it.
pub(crate) struct InducedRule {
    pub candidate: RuleCandidate,
    pub proposal: ProposalRecord,
}

/// Project typed observations onto the shape the shipped rules miner consumes.
///
/// `files` is empty and `memory_kind` is `None` because a reflection lesson
/// carries neither. Both are inputs to `infer_guard`, so today it returns
/// `None` regardless — but this module strips the guard anyway rather than
/// depending on that, since a future observation source with file evidence
/// would otherwise start arming guards silently.
fn as_raw_observations(observations: &[ObservationRecord]) -> Vec<RawObservation> {
    observations
        .iter()
        .map(|o| RawObservation {
            text: o.text.clone(),
            source: EvidenceSource::Memory,
            reference: o.source_ref.clone(),
            occurred_at: occurred_at_of(o),
            files: Vec::new(),
            salient: false,
            memory_kind: None,
        })
        .collect()
}

/// The `occurred_at` an observation was minted with, recovered from
/// `source_ref` (`reflection:<unix secs>`) — the same recovery the skills path
/// does, and for the same reason: this integer is quoted verbatim into the
/// artifact's evidence lines.
fn occurred_at_of(observation: &ObservationRecord) -> u64 {
    observation
        .source_ref
        .rsplit(':')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Strip any inferred guard, so a mined rule is prompt-only (Tier 1).
///
/// Separate and named rather than inlined: this is the single line standing
/// between "the loop suggests a convention" and "the loop silently starts
/// denying the user's tool calls", and it should be findable by grep.
fn without_inferred_guard(mut candidate: RuleCandidate) -> RuleCandidate {
    candidate.guard = None;
    candidate
}

/// Mine observations into rule candidates and record a durable proposal for
/// each.
///
/// The miner itself is called unchanged, with unchanged [`MineConfig`]
/// thresholds, over the same observations the skills path sees.
pub(crate) fn induce_rule_proposals(
    store: &ContextStore,
    observations: &[ObservationRecord],
    existing: &[Rule],
    config: &MineConfig,
) -> Vec<InducedRule> {
    let candidates = rules::mine_candidates(as_raw_observations(observations), existing, config);

    let mut induced = Vec::new();
    for candidate in candidates {
        let candidate = without_inferred_guard(candidate);

        // Resolve the miner's evidence back to the observations behind it, so
        // the proposal carries distinct TASKS rather than raw occurrences.
        let mut tasks: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        // The observations themselves, not just their ids: the proposal's
        // evidence grade is folded from them, and a grade cannot be folded
        // from an id (#2782).
        let mut supporting: Vec<&ObservationRecord> = Vec::new();
        for evidence in &candidate.evidence {
            if let Some(observation) = observations.iter().find(|o| {
                o.source_ref == evidence.reference
                    && o.text.chars().take(160).collect::<String>() == evidence.snippet
            }) {
                tasks.insert(observation.task_id.as_str());
                supporting.push(observation);
            }
        }

        let score = ProposalScore {
            occurrences: candidate.occurrences as u32,
            distinct_tasks: tasks.len() as u32,
            salient: candidate.salient,
            rank: f64::from(candidate.score),
        };
        let Ok(confidence) = confidence_from_score(&score) else {
            continue;
        };
        let status = if score.distinct_tasks >= 3 && score.occurrences >= 3 {
            RecordProposalStatus::Eligible
        } else {
            RecordProposalStatus::Collecting
        };
        let Ok(proposal) = ProposalRecord::new(
            RecordProposalKind::Directive,
            status,
            &candidate.id,
            &candidate.description,
            &candidate.text,
            Vec::new(),
            EvidencePool::from_observations(supporting),
            score,
            confidence,
            stella_context::format_rfc3339(
                candidate
                    .evidence
                    .iter()
                    .map(|e| e.occurred_at)
                    .max()
                    .unwrap_or(0) as i64,
            ),
        ) else {
            continue;
        };
        let _ = record_proposal(store, &proposal);
        induced.push(InducedRule {
            candidate,
            proposal,
        });
    }
    induced
}

/// `<workspace>/.stella/rules` — where `FsRuleSource` reads project rules from,
/// and therefore the only place a mined rule takes effect.
pub(crate) fn workspace_rules_dir(workspace_root: &Path) -> std::path::PathBuf {
    workspace_root.join(".stella").join("rules")
}

/// Publish a mined rule as a TOML context record under `.stella/rules/`,
/// never clobbering.
///
/// `Ok(Some(path))` when a file was written; `Ok(None)` when one already
/// exists (on either the record surface or the retired markdown one — that
/// file still loads, and a TOML twin would double-inject it); `Err` when the
/// record cannot be built or written, which the caller must surface rather
/// than fold into "already exists". The no-clobber posture checks the
/// **filesystem** rather than a loaded list: the rules loader silently skips
/// unreadable files and directories, so a list-membership test would have the
/// same blind spot [#737](https://github.com/macanderson/stella/issues/737)
/// describes on the skills side — and the write itself is `create_new` inside
/// [`crate::context_records::write_record`], because the exists checks here
/// are advisory and racy.
/// Publish a mined rule as a context record.
///
/// `evidence_grade` is the grade of the proposal being published (#2782). It
/// is a parameter rather than something this function derives, because the
/// evidence is two hops back: only the caller holding the proposal knows it,
/// and a rule file that does not carry it cannot recover it later.
pub(crate) fn write_rule(
    workspace_root: &Path,
    candidate: &RuleCandidate,
    evidence_grade: Option<ProvenanceGrade>,
) -> Result<Option<std::path::PathBuf>, String> {
    let candidate = without_inferred_guard(candidate.clone());
    let set_id = crate::ingest_cmd::derive_set_id(workspace_root);
    let record = crate::context_records::inferred_rule_record(
        &set_id,
        &candidate.id,
        &candidate.text,
        "observation",
        &format!("proposal:rule:{}", candidate.id),
        evidence_grade,
    )?;
    if workspace_rules_dir(workspace_root)
        .join(format!("{}.md", candidate.id))
        .exists()
    {
        return Ok(None);
    }
    let Some(path) = crate::context_records::publication_path(
        workspace_root,
        stella_core::ingest::record::SharingScope::Repository,
        &record.lineage_id,
    ) else {
        return Err("cannot determine where to publish this record".to_string());
    };
    if path.exists() {
        return Ok(None);
    }
    crate::context_records::write_record(&path, &set_id, &record)?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests;
