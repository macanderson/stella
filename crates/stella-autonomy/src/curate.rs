// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Turning a wall the loop keeps hitting into a proposal somebody can accept.
//!
//! The loop writes down every step it could not take: a check that failed, a
//! rule it waived, an issue it handed back, a command it retried. One such
//! line is an accident. The same line in several separate runs is a habit,
//! and a habit is the only thing here worth asking a person about.
//!
//! This module holds the rules that turn those lines into proposals. It reads
//! nothing and writes nothing — `stella-cli`'s `self_driving_cmd::curate` is
//! the half that touches the journal.
//!
//! # A proposal is a suggestion, never a change
//!
//! [`Proposal`] carries a statement, the surface a person would change, and
//! the evidence behind it. There is no arm here that applies anything, which
//! is how the loop proposes an authority it cannot grant itself.
//!
//! # Why the statement is cut back before it is grouped
//!
//! Journal lines carry the particulars: an issue key, an error string, a
//! branch name. Two runs blocked by the same thing say it with different
//! particulars, so grouping the raw lines finds nothing. The leading clause is
//! what repeats, so [`shape`] keeps that and drops the rest.
//!
//! # The threshold is the caller's
//!
//! [`propose`] takes the recurrence floor rather than declaring one. The
//! number belongs to the provenance policy, and this crate is a leaf that
//! cannot read it — a constant here would be a second copy free to drift.
//!
//! `doc:backlog-self-driving` §3.5 is the design.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One line of the loop's journal, as evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sighting {
    /// What the loop said it could not do.
    pub statement: String,
    /// The run it happened in. The recurrence count is over distinct values
    /// of this, so a run that hits one wall thirty times still counts once.
    pub run: String,
    /// Where a reader finds the line — a timestamp, an issue key.
    pub evidence: String,
    /// Which surface a person would change to answer it.
    pub target: Target,
}

/// The three things this repository steers itself with.
///
/// A proposal names one. What each costs — the evidence grade and the
/// authority that may publish it — is not declared here: it is derived from
/// the evolution matrix's own impact class by the caller, so this crate holds
/// no copy of a policy it cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// A procedure the agent can follow next time.
    Skill,
    /// A directive that steers the agent away from the wall.
    Rule,
    /// Executable capability the agent does not have.
    Tool,
}

impl Target {
    /// Every target, in the order a reader meets them above.
    pub const ALL: &[Self] = &[Self::Skill, Self::Rule, Self::Tool];

    /// The canonical `snake_case` tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skill => "skill",
            Self::Rule => "rule",
            Self::Tool => "tool",
        }
    }
}

/// A wall the loop keeps hitting, with the evidence that says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    /// Which surface a person would change.
    pub target: Target,
    /// The wall, in the loop's own words — the first sighting's full line.
    pub statement: String,
    /// The shape the sightings were grouped by. Stable across runs, so it is
    /// what a dedup digest is taken from.
    pub shape: String,
    /// Where each supporting line is, oldest first.
    pub evidence: Vec<String>,
    /// The distinct runs that met it, in the order they were seen.
    pub runs: Vec<String>,
}

/// Longest leading clause a statement is grouped by, in characters.
///
/// Long enough to tell two walls apart, short enough that a sentence which
/// keeps going cannot split one wall into several.
const SHAPE_CHARS: usize = 80;

/// The part of a statement that repeats across runs.
///
/// Everything from the first particular onwards is dropped: a parenthesis, a
/// colon, a dash, a backtick. Each of those opens the detail — an error
/// string, an issue key, a branch — that makes two sightings of one wall look
/// like two walls. What is left is lowercased and its whitespace collapsed,
/// so a message reflowed by an editor still groups with its earlier self.
///
/// An empty result means the line was nothing but particulars, and it is not
/// evidence of anything.
#[must_use]
pub fn shape(statement: &str) -> String {
    let head: String = statement
        .chars()
        .take_while(|&c| !matches!(c, '(' | ':' | '`' | '—' | '[' | ';'))
        .take(SHAPE_CHARS)
        .collect();
    let mut out = String::with_capacity(head.len());
    for word in head.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&word.to_lowercase());
    }
    out
}

/// The walls that recur, with the evidence behind each.
///
/// A group is kept when it was met in at least `min_distinct_runs` separate
/// runs. Repetition inside one run counts once, because a loop that retried
/// the same command forty times in one afternoon has one observation and not
/// forty — the same distinct-task rule the provenance policy applies to
/// mined evidence.
///
/// Output is ordered by target and then by shape, so two passes over one
/// journal propose the same things in the same order.
#[must_use]
pub fn propose(sightings: &[Sighting], min_distinct_runs: usize) -> Vec<Proposal> {
    // At least one, so a caller that passes zero cannot turn every single
    // sighting into a proposal.
    let floor = min_distinct_runs.max(1);

    // Keyed by target as well as shape: the same words reached through a
    // waiver and through a failed check are two different asks, and merging
    // them would attribute one proposal's evidence to the other's surface.
    let mut groups: BTreeMap<(Target, String), Proposal> = BTreeMap::new();
    for sighting in sightings {
        let shape = shape(&sighting.statement);
        if shape.is_empty() || sighting.run.trim().is_empty() {
            continue;
        }
        let entry = groups
            .entry((sighting.target, shape.clone()))
            .or_insert_with(|| Proposal {
                target: sighting.target,
                statement: sighting.statement.trim().to_owned(),
                shape,
                evidence: Vec::new(),
                runs: Vec::new(),
            });
        if !entry.evidence.contains(&sighting.evidence) {
            entry.evidence.push(sighting.evidence.clone());
        }
        if !entry.runs.contains(&sighting.run) {
            entry.runs.push(sighting.run.clone());
        }
    }

    groups
        .into_values()
        .filter(|proposal| proposal.runs.len() >= floor)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sighting(statement: &str, run: &str, evidence: &str, target: Target) -> Sighting {
        Sighting {
            statement: statement.to_owned(),
            run: run.to_owned(),
            evidence: evidence.to_owned(),
            target,
        }
    }

    /// **The witness.** A wall met in three separate runs becomes one
    /// proposal, and the proposal names every run behind it.
    #[test]
    fn a_wall_met_in_three_runs_becomes_a_proposal_naming_its_evidence() {
        let seen = vec![
            sighting("could not file `a`: 502", "r1", "at:1", Target::Tool),
            sighting("could not file `b`: 500", "r2", "at:2", Target::Tool),
            sighting("could not file `c`: timeout", "r3", "at:3", Target::Tool),
        ];

        let made = propose(&seen, 3);

        assert_eq!(made.len(), 1, "got {made:?}");
        assert_eq!(made[0].target, Target::Tool);
        assert_eq!(made[0].runs, vec!["r1", "r2", "r3"]);
        assert_eq!(made[0].evidence, vec!["at:1", "at:2", "at:3"]);
        assert_eq!(made[0].shape, "could not file");
    }

    /// Two runs is not a habit when the floor is three.
    #[test]
    fn two_runs_do_not_reach_a_floor_of_three() {
        let seen = vec![
            sighting("could not file `a`: 502", "r1", "at:1", Target::Tool),
            sighting("could not file `b`: 500", "r2", "at:2", Target::Tool),
        ];

        assert!(propose(&seen, 3).is_empty());
    }

    /// Thirty repetitions inside one run are one observation, not thirty.
    /// This is the anti-poisoning rule the provenance policy states, applied
    /// to the loop's own journal.
    #[test]
    fn repetition_inside_one_run_never_reaches_the_floor() {
        let seen: Vec<Sighting> = (0..30)
            .map(|n| {
                sighting(
                    "could not file `x`: 502",
                    "r1",
                    &format!("at:{n}"),
                    Target::Tool,
                )
            })
            .collect();

        assert!(propose(&seen, 3).is_empty());
    }

    /// One wall reached through two different surfaces is two asks.
    #[test]
    fn the_same_words_under_two_targets_stay_two_proposals() {
        let seen = vec![
            sighting("the gate is red", "r1", "at:1", Target::Rule),
            sighting("the gate is red", "r2", "at:2", Target::Rule),
            sighting("the gate is red", "r3", "at:3", Target::Rule),
            sighting("the gate is red", "r1", "at:4", Target::Skill),
            sighting("the gate is red", "r2", "at:5", Target::Skill),
            sighting("the gate is red", "r3", "at:6", Target::Skill),
        ];

        let made = propose(&seen, 3);

        assert_eq!(made.len(), 2, "got {made:?}");
        assert_eq!(made[0].target, Target::Skill);
        assert_eq!(made[1].target, Target::Rule);
    }

    /// A floor of zero still needs one sighting, so a caller cannot turn the
    /// whole journal into proposals by passing nothing.
    #[test]
    fn a_zero_floor_is_read_as_one() {
        let seen = vec![sighting(
            "could not file `a`: 502",
            "r1",
            "at:1",
            Target::Tool,
        )];

        assert_eq!(propose(&seen, 0).len(), 1);
    }

    /// A line with no run behind it is not evidence of a habit.
    #[test]
    fn a_sighting_with_no_run_is_dropped() {
        let seen = vec![
            sighting("could not file `a`: 502", "  ", "at:1", Target::Tool),
            sighting("could not file `b`: 500", "", "at:2", Target::Tool),
            sighting("could not file `c`: 500", "", "at:3", Target::Tool),
        ];

        assert!(propose(&seen, 1).is_empty());
    }

    /// The particulars are what differ between two sightings of one wall, so
    /// the shape stops at the first of them.
    #[test]
    fn the_shape_stops_at_the_first_particular() {
        assert_eq!(
            shape("could not triage (network); ranking"),
            "could not triage"
        );
        assert_eq!(
            shape("the baseline is already red (cargo) — advisory"),
            "the baseline is already red"
        );
        assert_eq!(shape("Could   not   file `x`: 502"), "could not file");
        assert_eq!(shape("(nothing but particulars)"), "");
    }

    /// Every target answers to a tag, and no two share one.
    #[test]
    fn every_target_has_its_own_tag() {
        let tags: Vec<&str> = Target::ALL.iter().map(|t| t.as_str()).collect();
        let mut unique = tags.clone();
        unique.sort_unstable();
        unique.dedup();

        assert_eq!(tags.len(), unique.len(), "got {tags:?}");
    }
}
