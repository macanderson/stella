// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Saying out loud that the loop keeps hitting the same wall.
//!
//! `stella_autonomy::curate` holds the rules. This is the half that touches
//! the world: it reads the loop's own journal, groups what recurs, and writes
//! each recurring wall down as a proposal beside the loop's other evidence.
//!
//! # A proposal is a suggestion, and nothing here can apply one
//!
//! No path in this module writes a skill file, a record under
//! `.stella/rules/`, or a line of `.stella/rules/promotions.jsonl`. That is
//! the rule the loop is held to: it may propose any authority, and it may
//! grant itself none. Acceptance stays where it already lives — a person
//! reads the proposal and goes through `stella context keep` and `stella
//! context promote`, which append to the hash-chained ledger under a real
//! approver.
//!
//! # What the evidence is
//!
//! Every step the loop could not take is already a line in `audit.jsonl`: a
//! check that failed, a rule it waived, an issue it handed back, a command it
//! retried. [`WALLS`] names the actions that count and the surface each one
//! points at. A line's session is the run it happened in, and the recurrence
//! count is over distinct runs, so an afternoon spent retrying one command is
//! one observation rather than forty.
//!
//! # What a proposal costs
//!
//! Nothing here restates what the evidence has to be worth. The surface's
//! impact class comes from `stella-parity`'s evolution matrix — the map in
//! [`impact_of`] is pinned against those rows by
//! `the_loop_proposes_at_the_impact_its_evolution_row_declares` — and the
//! grade and authority that impact requires come from
//! `stella_protocol::provenance`, the one place that policy lives.
//!
//! `doc:backlog-self-driving` §3.5 is the design.

use std::path::Path;

use serde::{Deserialize, Serialize};
use stella_autonomy::curate::{Proposal, Sighting, Target};
use stella_protocol::provenance::{
    ImpactClass, PromotionRefusal, ProvenanceGrade, PublicationAuthority, authorises,
};
use stella_records::context_record::TRAJECTORY_ABSTRACTION_MIN_DISTINCT_TASKS;

use super::audit::{self, Action as Audit, AuditEntry};
use super::state::LoopState as Durable;

/// How many separate runs must meet a wall before it becomes a proposal.
///
/// Not a number chosen here. `EvidencePool::from_observations` lifts a pool to
/// `ProvenanceGrade::TrajectoryAbstraction` only once its evidence spans
/// `TRAJECTORY_ABSTRACTION_MIN_DISTINCT_TASKS` distinct tasks, and that is the
/// weakest grade any of the three surfaces below accepts. Under the floor a
/// proposal could not be published however a person decided, so making one
/// would be noise with somebody's attention as its cost.
const RECURRENCE_THRESHOLD: usize = TRAJECTORY_ABSTRACTION_MIN_DISTINCT_TASKS as usize;

/// The most proposals one pass may make.
///
/// A journal full of one-off failures that happen to share a leading clause is
/// a detector misfiring, not twenty habits. The cap turns that into bounded
/// noise.
const MAX_PROPOSALS_PER_PASS: usize = 8;

/// How far back through the journal one pass reads, in lines.
///
/// The newest window, because a wall the loop stopped hitting a year ago is
/// not a wall. Wide enough to hold several runs of an ordinary loop.
const JOURNAL_READ_LIMIT: usize = 5_000;

/// The journal actions that record a wall, and what each one points at.
///
/// A wall is a step the loop could not take. Which surface answers it depends
/// on what kind of step it was:
///
/// - A turn or a check that failed is a piece of work the loop did not know
///   how to do. A procedure is what it lacked, so the ask is a skill.
/// - A waiver, a deferral or an escalation is a judgement the loop made and
///   kept making. A directive settles a judgement once, so the ask is a rule.
/// - A command retried as transient is the environment refusing. Wrapping a
///   command is a tool's job, so the ask is a tool.
///
/// The grouping is also what makes [`grade_of`] answerable per target rather
/// than per line: every action pointing at one surface is the same kind of
/// claim.
const WALLS: &[(Audit, Target)] = &[
    (Audit::WorkFailed, Target::Skill),
    (Audit::VerifyFailed, Target::Skill),
    (Audit::Waived, Target::Rule),
    (Audit::Deferred, Target::Rule),
    (Audit::Escalated, Target::Rule),
    (Audit::Transient, Target::Tool),
];

/// What the loop's evidence for one target is worth.
///
/// A failed turn, a failed check and a retried command are the environment
/// answering — an exit status, a tracker's refusal — which is
/// [`ProvenanceGrade::EnvironmentObservation`] exactly. A waiver, a deferral
/// or an escalation is the loop's own judgement about a run, which is
/// [`ProvenanceGrade::ModelCritique`]; it grades down for the reason the
/// provenance policy grades down, that under-grading parks a proposal until
/// somebody re-derives it while over-grading publishes on evidence that was
/// never there.
///
/// Recurrence moves neither answer. Aggregation never promotes evidence, so
/// ten runs agreeing that the loop waived the same rule is still one class of
/// claim — which is why a rule proposal from this journal can never reach the
/// `EnvironmentObservation` a steering directive costs.
fn grade_of(target: Target) -> ProvenanceGrade {
    match target {
        Target::Skill | Target::Tool => ProvenanceGrade::EnvironmentObservation,
        Target::Rule => ProvenanceGrade::ModelCritique,
    }
}

/// What a wrong change to the surface a target names can break.
///
/// One line per row of `stella-parity`'s evolution matrix: a skill is that
/// matrix's `Skill` row (`ImpactClass::AdvisoryRecord`), a rule is its
/// `Framework` row (`ImpactClass::SteeringDirective`), a custom tool is its
/// `Tool` row (`ImpactClass::ExecutableTool`). The matrix is the declaration
/// and this is a reader of it, held to it by
/// `the_loop_proposes_at_the_impact_its_evolution_row_declares` in
/// `crates/stella-parity/src/evolution/tests.rs`, which reads this source.
///
/// The grade and the authority an impact costs are not written here at all:
/// `ImpactClass::required_grade` and `ImpactClass::required_authority` answer
/// those, out of the one policy that holds them.
fn impact_of(target: Target) -> ImpactClass {
    match target {
        Target::Skill => ImpactClass::AdvisoryRecord,
        Target::Rule => ImpactClass::SteeringDirective,
        Target::Tool => ImpactClass::ExecutableTool,
    }
}

/// One proposal, as the loop writes it down.
///
/// Every field is something the loop observed or read out of the policy. No
/// field says the proposal was applied, because no path here applies one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ProposalRow {
    /// The dedup key, taken from the grouped shape rather than the whole line,
    /// so tomorrow's sighting of one wall does not read as a second wall.
    pub digest: String,
    /// When the loop wrote it down, RFC3339 UTC.
    pub at: String,
    /// Which surface a person would change — `skill`, `rule` or `tool`.
    pub surface: String,
    /// The wall, in the loop's own words.
    pub statement: String,
    /// The journal lines behind it, oldest first.
    pub evidence: Vec<String>,
    /// The distinct runs that met it.
    pub runs: Vec<String>,
    /// What the loop's evidence for this is worth.
    pub grade: String,
    /// What this surface's impact class requires, read from the policy.
    pub required_grade: String,
    /// Who may publish that impact class, read from the same policy.
    pub required_authority: String,
    /// The governance mode in force when it was written.
    pub governance: String,
    /// Why the loop could not have published this on its own, when it could
    /// not. Absent means its evidence and authority reach the bar — and it
    /// still publishes nothing, because a proposal is all it makes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<PromotionRefusal>,
}

/// The recurring walls this journal shows that are not written down yet.
///
/// The journal is read twice per run: once to count, because the machine's
/// ladder consults the count before it asks for a curate step, and once to
/// write, when it does ask. Both go through here, so the two can never
/// disagree about what a pass would produce.
pub(super) fn fresh(durable: &Durable) -> Vec<Proposal> {
    let walls = sightings(&durable.audit_path());
    let already: Vec<String> = durable
        .proposals()
        .into_iter()
        .map(|row| row.digest)
        .collect();
    stella_autonomy::curate::propose(&walls, RECURRENCE_THRESHOLD)
        .into_iter()
        .filter(|proposal| !already.contains(&stella_autonomy::finding_digest(&proposal.shape)))
        .take(MAX_PROPOSALS_PER_PASS)
        .collect()
}

/// How many proposals a pass would make right now.
///
/// What the machine reads before it decides whether to spend an idle step on
/// curating. Zero for a loop that has never hit the same wall in three runs,
/// which is the ordinary case and the one that must cost nothing.
pub(super) fn pending(durable: &Durable) -> u32 {
    u32::try_from(fresh(durable).len()).unwrap_or(u32::MAX)
}

/// Write down every recurring wall this journal shows. `true` when anything
/// reached the file.
///
/// Best-effort per proposal: a row that cannot be appended is reported and the
/// rest are still written, on the rule the audit trail itself follows — a
/// failure to write the paperwork must not cost the work.
pub(super) fn pass(durable: &Durable, root: &Path) -> bool {
    let governance = match crate::context_records::read_governance(root) {
        Ok(governance) => governance.mode.as_str().to_owned(),
        // A governance file that will not parse fails closed everywhere else,
        // and it does here too: the loop records what it could not establish
        // rather than writing down the permissive default as a fact.
        Err(error) => {
            audit::record(
                durable,
                Audit::Transient,
                None,
                &format!("could not read the governance mode: {error}"),
            );
            "unknown".to_owned()
        }
    };

    let proposals = fresh(durable);
    if proposals.is_empty() {
        audit::record(
            durable,
            Audit::Swept,
            None,
            "no wall in the journal has been met in enough separate runs to propose anything",
        );
        return false;
    }

    let mut written = 0_u32;
    for proposal in &proposals {
        let row = row_for(proposal, &governance);
        let standing = match &row.refusal {
            None => format!(
                "its {} evidence reaches the {} this surface needs, and a person still decides",
                row.grade, row.required_grade
            ),
            Some(refusal) => format!("the loop could not publish it in any case: {refusal:?}"),
        };
        match durable.append_proposal(&row) {
            Ok(()) => {
                written += 1;
                audit::record(
                    durable,
                    Audit::Swept,
                    None,
                    &format!(
                        "proposing a {} after {} run(s) met the same wall — {standing}. Nothing \
                         is applied; accept it with `stella context keep`, then `stella context \
                         promote`",
                        row.surface,
                        row.runs.len(),
                    ),
                );
            }
            Err(error) => audit::record(
                durable,
                Audit::Transient,
                None,
                &format!(
                    "could not write the proposal for this wall: {error} — {}",
                    proposal.statement
                ),
            ),
        }
    }
    written > 0
}

/// One proposal as a row, with everything the policy says about it filled in.
fn row_for(proposal: &Proposal, governance: &str) -> ProposalRow {
    let impact = impact_of(proposal.target);
    let grade = grade_of(proposal.target);

    ProposalRow {
        digest: stella_autonomy::finding_digest(&proposal.shape),
        at: crate::timefmt::rfc3339_utc_now(),
        surface: proposal.target.as_str().to_owned(),
        statement: proposal.statement.clone(),
        evidence: proposal.evidence.clone(),
        runs: proposal.runs.clone(),
        grade: grade.as_str().to_owned(),
        required_grade: impact.required_grade().as_str().to_owned(),
        required_authority: impact.required_authority().as_str().to_owned(),
        governance: governance.to_owned(),
        // The loop acts as itself, which is the weakest authority there is.
        refusal: authorises(Some(grade), PublicationAuthority::Agent, impact).err(),
    }
}

/// The walls the journal's newest window shows, oldest first.
fn sightings(journal: &Path) -> Vec<Sighting> {
    let Ok(text) = std::fs::read_to_string(journal) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(JOURNAL_READ_LIMIT);

    let mut out = Vec::new();
    for line in &lines[start..] {
        let Ok(entry) = serde_json::from_str::<AuditEntry>(line) else {
            continue;
        };
        let Some((_, target)) = WALLS.iter().find(|(action, _)| *action == entry.action) else {
            continue;
        };
        out.push(Sighting {
            statement: entry.outcome.clone(),
            // The drive session, because that is one launch of the loop. A
            // one-shot verb writes no session and folds into the cycle run
            // around it, which is the next-best boundary and never worse:
            // reading a cycle as a run can only under-count recurrence.
            run: entry
                .session_id
                .clone()
                .unwrap_or_else(|| entry.run_id.clone()),
            evidence: entry.at.clone(),
            target: *target,
        });
    }
    out
}

#[cfg(test)]
mod tests;
