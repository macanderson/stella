// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a rule's own turns say about it.
//!
//! `memory::trials` writes one row per rule per turn. The row says what the
//! turn could show, and what it showed. Nothing read those rows. This module
//! reads them, for two answers.
//!
//! **Is the rule still earning its place?** [`sweep`] runs the same
//! `appraise` and `decide_demotion` pass the skill and memory sweeps run. A
//! mined rule the turns say is hurting gets retracted. A rule a person wrote
//! is kept, whatever the numbers say.
//!
//! **Has the rule paid what a directive costs?** [`measured_grade`] turns a
//! `Helps` verdict into `EnvironmentObservation`. That is the bar
//! `SteeringDirective` sets. Reflection prose grades `ModelCritique`, and the
//! gate still says no to it.
//!
//! # The id a trial row is filed under
//!
//! The render handle, like `^pkg-manager`. It is what the render pass knows.
//! The door that retracts a rule takes a lineage instead. So [`lineages`]
//! joins the two, off the registry the origins came from. A handle grows
//! longer when a second lineage ends in the same word. That re-keys a window
//! nobody touched (`#6161`). The join keeps it a lost window, not a
//! retraction of the wrong record.
//!
//! # Nothing here writes to a skill ledger
//!
//! `SkillAppraisal::skill` is a bare string. `appraisals::record_demotion`
//! files under a `skill:<name>` lineage. A rule id in either would clash with
//! a skill of the same name (`#6103`). So this writes to neither. A verdict is
//! acted on and never stored, the way the memory sweep acts on one. A
//! retraction is filed under the record's own `lineage_id`, which is unique in
//! the workspace.
//!
//! # A candidate with no window
//!
//! A rule that never rendered has no arms. [`measured_grade`] answers `None`
//! for it, and the mining grade is refused as before. A window comes from the
//! turns after a first publication. So this path cannot start the loop by
//! itself. The skill queue has the same shape, and answers with a bootstrap
//! switch. That is the wrong answer for something that steers, and `#6162` is
//! where a better one is being picked.

use std::collections::HashMap;
use std::path::Path;

use stella_learn::ledger::ArtifactKind;
use stella_learn::skills::SkillOrigin;
use stella_learn::skills::appraisal::{
    AppraisalConfig, DemotionDecision, DemotionReason, SkillVerdict,
};
use stella_protocol::provenance::ProvenanceGrade;
use stella_records::context_record::{Origin, RecordStatus};
use stella_records::records::{Registry, Trust};

use super::appraisals;

/// Which rules the loop may retract on its own — the origin map
/// [`appraisals::sweep`] reads.
///
/// A record that misses any test below is left out. `decide_demotion` then
/// keeps it, which is the safe way to fail.
///
/// * The loop wrote it (`origin = "inferred"`).
/// * It is a repository record. One in the user's own `~/.stella/rules` is not
///   this workspace's to rewrite.
/// * No plugin shipped it. `stella plugin remove` is that door.
/// * It is still active. A rule already retracted is not retracted twice.
pub(crate) fn origins(registry: &Registry) -> HashMap<String, SkillOrigin> {
    let mut out = HashMap::new();
    for entry in &registry.entries {
        let record = &entry.record;
        let mined = record.record.origin == Some(Origin::Inferred)
            && record.trust == Trust::Project
            && record.contributed_by.is_none()
            && matches!(record.record.status, None | Some(RecordStatus::Active));
        if mined {
            out.insert(record.handle.clone(), SkillOrigin::AutoCreated);
        }
    }
    out
}

/// Handle to `lineage_id`, for every record the registry loaded.
///
/// The trial ledger names a rule by its handle. Every door on the other side
/// names it by lineage — the retraction, the promotion ledger, the file. This
/// is where the two meet.
pub(crate) fn lineages(registry: &Registry) -> HashMap<String, String> {
    registry
        .entries
        .iter()
        .map(|entry| {
            (
                entry.record.handle.clone(),
                entry.record.record.lineage_id.clone(),
            )
        })
        .collect()
}

/// The retraction a demoted rule has earned, or `None`.
///
/// Split from the write, so the choice can be tested with no workspace on
/// disk, and so the reason is built in one place. A retraction whose cause
/// nobody can read is what the ledger exists to stop.
pub(crate) fn retraction_reason(decision: &DemotionDecision) -> Option<String> {
    let DemotionDecision::Demote { reason } = decision else {
        return None;
    };
    Some(match reason {
        DemotionReason::Harmful { lift } => format!(
            "holding it back improved the turns it was offered on (lift {lift:.3}). \
             The efficacy loop retracted it. The file keeps the statement; set its \
             `status` back to `active` to put it back."
        ),
        DemotionReason::Inert { trials } => format!(
            "no measured gain across {trials} recorded turns. The efficacy loop \
             retracted it. The file keeps the statement; set its `status` back to \
             `active` to put it back."
        ),
    })
}

/// What one sweep did, for reporting.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct RetractionSweep {
    /// The lineages just retracted.
    pub(crate) retracted: Vec<String>,
    /// Lineages that earned a retraction the writer could not make, and why.
    /// Reported, not dropped: a sweep that quietly skips half its findings
    /// reads as "nothing earned it", which is a different claim.
    pub(crate) refused: Vec<(String, String)>,
}

/// Appraise every rule the trial ledger has rows for. Retract the mined ones
/// whose own turns say they stopped helping.
///
/// The registry is read from disk, not taken from the session. The file says
/// what is published, which is why `proposals_cmd::retract` reads the rules
/// directory too.
pub(crate) fn sweep(workspace_root: &Path) -> RetractionSweep {
    let registry = crate::context_records::load_registry(workspace_root);
    let origins = origins(&registry);
    let lineages = lineages(&registry);
    let mut out = RetractionSweep::default();
    for (appraisal, decision) in appraisals::sweep(
        workspace_root,
        ArtifactKind::Rule,
        &origins,
        &AppraisalConfig::default(),
    ) {
        let Some(reason) = retraction_reason(&decision) else {
            continue;
        };
        // `SkillAppraisal::skill` is the id the sweep judged. For this kind
        // that is the record's render handle.
        let Some(lineage) = lineages.get(&appraisal.skill) else {
            // The ledger names a handle no record carries now. There is
            // nothing to retract, and nothing is wrong: an append-only ledger
            // outlives what it describes.
            continue;
        };
        match crate::proposals_cmd::retract_published(workspace_root, lineage, &reason) {
            Ok((retracted, _path)) => out.retracted.push(retracted),
            Err(why) => out.refused.push((lineage.clone(), why)),
        }
    }
    out
}

/// The grade `rule_id`'s own turns have earned, or `None`.
///
/// `EnvironmentObservation` on [`SkillVerdict::Helps`] alone. The turns that
/// showed the rule beat the turns that withheld it, on rows the render pass
/// wrote. That is the environment answering. It is what the grade means, and
/// why a directive costs it.
///
/// Every other verdict answers `None`, a guard-blocked lift included. The
/// caller then falls back on the mining grade, which the gate refuses. A rule
/// with no rows answers `None` too, so a candidate nobody measured publishes
/// nothing.
pub(crate) fn measured_grade(workspace_root: &Path, rule_id: &str) -> Option<ProvenanceGrade> {
    // The origin here is a stand-in. This reads the verdict and drops the
    // decision beside it.
    let origins = HashMap::from([(rule_id.to_string(), SkillOrigin::AutoCreated)]);
    let (appraisal, _) = appraisals::sweep(
        workspace_root,
        ArtifactKind::Rule,
        &origins,
        &AppraisalConfig::default(),
    )
    .into_iter()
    .find(|(appraisal, _)| appraisal.skill == rule_id)?;
    matches!(appraisal.verdict, SkillVerdict::Helps { .. })
        .then_some(ProvenanceGrade::EnvironmentObservation)
}

#[cfg(test)]
mod tests;
