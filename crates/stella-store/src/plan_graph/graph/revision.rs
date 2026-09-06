// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The revision gate: a failing gate puts a plan revision up, and nothing runs
//! until somebody approves it (`design/tui-v2/SPEC.md` §8.1 item 3).
//!
//! # The withholding is the point
//!
//! [`RevisionGate::admits`] answers `false` for as long as a proposal stands,
//! and that is the whole mechanism — a caller asks before it runs anything, so
//! "nothing runs until approval" is a question with one answer rather than a
//! convention every call site re-implements. There is a second, structural
//! half underneath it: an approval is the only thing that puts the proposed
//! task into the plan, and [`PlanGraph::ran`] refuses a task the current
//! revision does not contain. So even a caller that never asked cannot record
//! the inserted task as having run.
//!
//! `nothing_runs_while_a_proposal_stands` and
//! `the_proposed_task_cannot_run_until_the_revision_is_approved` are the two
//! halves of that claim.
//!
//! # A proposal answers evidence; it does not gather any
//!
//! [`RevisionGate::observe`] reads a [`GateBoard`] — the host's evaluation of
//! what an installed verification plugin reported (AGENTS.md's opening). It
//! re-runs no gate and re-checks no evidence; it turns a failure somebody else
//! observed into a question for the person driving. The cause it carries is
//! the gate's own words, cut to one line, so a reader can tell what the
//! proposal is answering without opening the log.
//!
//! # Why the first failed gate
//!
//! One proposal per board, authored from the first failure in the rule's own
//! order — the same choice `stella_tui::deck_ui::row_keys`'s
//! `selected_failed_board` documents for the `r` key, and for the same reason:
//! two surfaces that picked differently would disagree about which failure the
//! reader is looking at. A board with several failures loses the rest, and
//! that is a real limitation rather than a hidden one — the next board after
//! this repair carries whatever is still red.
//!
//! # Pure
//!
//! Owned data and no I/O (AGENTS.md #2), which is what lets the acceptance
//! criterion be witnessed with no terminal, no engine and no plugin.

use stella_protocol::plan_graph::{DivergenceCause, PlanRevision, RevisionProposal, TaskNode};
use stella_protocol::{GateBoard, GateState};

use super::{PlanGraph, PlanGraphError};

/// How much of a gate's log becomes the cause of the revision it provokes.
///
/// One line, cut, matching `stella-cli`'s plan gate: a cause rides a
/// breadcrumb and a proposal row, never a log pane, and the full text is
/// already under the failing gate's own row.
const CAUSE_CHARS: usize = 120;

/// Why a revision-gate call was refused. Named errors, never a bare string
/// (AGENTS.md #5): "nobody proposed anything" and "the plan moved under the
/// proposal you are approving" are different repairs, and a caller that had to
/// read prose to tell them apart would guess.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RevisionError {
    #[error(
        "no plan revision is pending; there is nothing to approve, edit or dismiss until a gate \
         fails and one is put up"
    )]
    NothingPending,
    #[error(
        "a proposed task needs a subject; a revision that inserts an unnamed task tells the next \
         reader that the plan grew and not what it grew by"
    )]
    BlankSubject,
    #[error(
        "the plan moved to r{current} while r{proposed} was standing, so approving would author \
         something other than what was put up; the proposal is stale and belongs back in front of \
         whoever is driving"
    )]
    PlanMoved { proposed: u32, current: u32 },
    #[error("the revision could not be written: {0}")]
    Graph(#[from] PlanGraphError),
}

/// One session's standing plan-revision proposal, and the withholding it
/// implies.
///
/// At most one proposal at a time, and [`Self::observe`] will not replace one
/// that is already standing: a second board must not retitle the thing the
/// reader is looking at while they decide about it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RevisionGate {
    pending: Option<RevisionProposal>,
}

impl RevisionGate {
    /// Put a revision up if `board` carries a failure the plan does not
    /// already answer, and return it.
    ///
    /// `revision` is the number the revision *would* be — `graph.revision()
    /// .next()` — and `planned` is the plan as it stands.
    ///
    /// `None` in four cases, each of which is a reason not to ask rather than
    /// a failure: a board with no determinate failure (an undecided gate
    /// blames the instrument, not the worker — see [`GateBoard::has_failure`]);
    /// a proposal already standing; a plan that already contains a task with
    /// this subject, so the repair the board is asking for is one somebody
    /// already planned; and a failure whose evidence says nothing at all,
    /// which cannot carry a [`DivergenceCause`].
    pub fn observe(
        &mut self,
        revision: PlanRevision,
        planned: &[TaskNode],
        board: &GateBoard,
    ) -> Option<&RevisionProposal> {
        if self.pending.is_some() {
            return None;
        }
        let gate = board.gates.iter().find(|gate| gate.failed())?;
        let GateState::Failed { case, log } = &gate.state else {
            return None;
        };
        let cause =
            DivergenceCause::new(cause_line(if case.trim().is_empty() { log } else { case }))?;
        let subject = repair_subject(&gate.name, case);
        if planned.iter().any(|task| task.subject == subject) {
            return None;
        }
        self.pending = Some(RevisionProposal {
            revision,
            subject,
            gate: gate.name.clone(),
            cause,
            issue: linked_issue(case).or_else(|| linked_issue(log)),
        });
        self.pending.as_ref()
    }

    /// The proposal on the table, if there is one.
    #[must_use]
    pub fn pending(&self) -> Option<&RevisionProposal> {
        self.pending.as_ref()
    }

    /// Whether work may proceed — `false` for as long as a proposal stands.
    ///
    /// SPEC 8.1's "nothing runs until approval", as one question with one
    /// answer. See the module docs for the structural half underneath it.
    #[must_use]
    pub fn admits(&self) -> bool {
        self.pending.is_none()
    }

    /// SPEC 8.1's `e edit`: keep the proposal, change what it would insert.
    ///
    /// The gate, the cause and the revision number are not editable, and that
    /// is the point of a separate verb: they are what the evidence said, and a
    /// proposal whose cause a reader could rewrite would be a record of the
    /// reader's opinion rather than of the failure.
    pub fn edit(&mut self, subject: impl Into<String>) -> Result<(), RevisionError> {
        let subject = subject.into();
        if subject.trim().is_empty() {
            return Err(RevisionError::BlankSubject);
        }
        let pending = self.pending.as_mut().ok_or(RevisionError::NothingPending)?;
        pending.subject = subject;
        Ok(())
    }

    /// SPEC 8.1's `x dismiss`: the reader does not want this task.
    ///
    /// Returns what was standing, so a caller can say what it dropped. The
    /// plan is untouched — a dismissed proposal authored no revision, which is
    /// the difference between declining a change and reverting one — and the
    /// gate admits again.
    pub fn dismiss(&mut self) -> Option<RevisionProposal> {
        self.pending.take()
    }

    /// SPEC 8.1's `a approve r4`: write the revision the proposal describes.
    ///
    /// The insertion goes to the end of the current plan and through
    /// [`PlanGraph::revise`], so the `[:NEXT]` chain, the retained predecessor
    /// and the [`Divergence`] carrying the gate's cause all fall out of the
    /// existing machinery rather than a second code path that could disagree
    /// with it.
    ///
    /// Refused when the plan has moved since the proposal was put up: the
    /// reader agreed to `r4: add task "<title>"`, and authoring `r5` instead
    /// would be a different change wearing the answer to this one.
    ///
    /// [`Divergence`]: stella_protocol::Divergence
    pub fn approve(&mut self, graph: &mut PlanGraph) -> Result<PlanRevision, RevisionError> {
        let proposal = self.pending.as_ref().ok_or(RevisionError::NothingPending)?;
        let current = graph.revision().next();
        if current != proposal.revision {
            return Err(RevisionError::PlanMoved {
                proposed: proposal.revision.get(),
                current: current.get(),
            });
        }
        let mut tasks = graph.planned(graph.revision());
        tasks.push(TaskNode::new(
            next_task_id(&tasks),
            proposal.subject.clone(),
        ));
        let revision = graph.revise(tasks, proposal.cause.clone())?;
        self.pending = None;
        Ok(revision)
    }
}

/// SPEC 8.1's `<title>`: what the inserted task is, in the gate's own words.
///
/// The failing case when the gate named one, because that is the thing a
/// repair step has to make pass; the gate's name otherwise. Nothing is
/// invented — a subject stella made up would be the one line of a proposal a
/// reader cannot check against the board above it.
fn repair_subject(gate: &str, case: &str) -> String {
    let case = case.trim();
    if case.is_empty() {
        format!("repair the failing {gate} gate")
    } else {
        format!("repair {case}")
    }
}

/// The first line of `text`, cut to [`CAUSE_CHARS`] characters.
///
/// Characters rather than bytes, so a cut never lands inside a multi-byte
/// character and produces text no terminal can draw.
fn cause_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .chars()
        .take(CAUSE_CHARS)
        .collect()
}

/// The first `#1234` in `text`, as SPEC 8.1's "any linked issue".
///
/// Read out of the evidence rather than supplied beside it: an issue the gate
/// itself named is one the reader can go and open, and a link from anywhere
/// else would be this function guessing which issue a failure belongs to.
/// `None` where the evidence named none, which renders as no cell at all.
///
/// Every `#` is tried, not just the first. Rust evidence is full of hashes
/// that are not issue numbers — `#[test]`, `#![cfg(unix)]` — and stopping at
/// the first one would drop the link in `#[test] a_case … see #151` while
/// finding it in `see #151 … #[test] a_case`.
fn linked_issue(text: &str) -> Option<String> {
    text.match_indices('#').find_map(|(at, _)| {
        let digits: String = text[at + 1..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        (!digits.is_empty()).then(|| format!("#{digits}"))
    })
}

/// The id the next task in `planned` would take, on the task board's own
/// scheme: per-session ordinals, counting up, never reused.
///
/// Derived from the plan rather than minted here, so an approved insertion
/// lands on the id the board would give it and the two id spaces stay one.
fn next_task_id(planned: &[TaskNode]) -> String {
    let highest = planned
        .iter()
        .filter_map(|task| task.id.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    (highest.saturating_add(1)).to_string()
}

#[cfg(test)]
mod tests;
