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
    ObservationRecord, ProposalRecord, ProposalScore, RecordProposalKind, RecordProposalStatus,
    confidence_from_score,
};
use stella_core::rules::{self, EvidenceSource, MineConfig, RawObservation, Rule, RuleCandidate};

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
        let mut supporting: Vec<String> = Vec::new();
        for evidence in &candidate.evidence {
            if let Some(observation) = observations.iter().find(|o| {
                o.source_ref == evidence.reference
                    && o.text.chars().take(160).collect::<String>() == evidence.snippet
            }) {
                tasks.insert(observation.task_id.as_str());
                supporting.push(observation.record_id.clone());
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
            supporting,
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

/// Write a mined rule to `.stella/rules/<id>.md`, never clobbering.
///
/// Returns the path when a file was actually written. Mirrors the skills
/// path's no-clobber posture, but checks the **filesystem** rather than a
/// loaded list: the rules loader silently skips unreadable files and
/// directories, so a list-membership test would have the same blind spot
/// [#737](https://github.com/macanderson/stella/issues/737) describes on the
/// skills side. A new call site should not inherit a known defect.
pub(crate) fn write_rule(
    workspace_root: &Path,
    candidate: &RuleCandidate,
) -> Option<std::path::PathBuf> {
    let candidate = without_inferred_guard(candidate.clone());
    let dir = workspace_rules_dir(workspace_root);
    let path = dir.join(format!("{}.md", candidate.id));
    if path.exists() {
        return None;
    }
    std::fs::create_dir_all(&dir).ok()?;
    // `create_new` rather than `write`: the exists() check above is advisory
    // and racy, and this is the write that must not destroy a user's file.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .ok()?;
    use std::io::Write as _;
    file.write_all(rules::render_rule_markdown(&candidate).as_bytes())
        .ok()?;
    Some(path)
}

#[cfg(test)]
mod tests;
