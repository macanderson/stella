//! How this repository writes issues, learned rather than assumed.
//!
//! `doc:backlog-self-driving` §3.1a. The loop files issues — that is the whole
//! point of `backlog file`, and it is what makes "nothing left behind" a
//! guarantee instead of a habit. But a tracker is not a bag of text. Every
//! repository the loop attaches to already has a classification scheme and a
//! writing standard, and filing into it wrongly is worse than not filing at
//! all: the queue the loop ranks is the queue the loop just polluted.
//!
//! # The defect this exists to prevent
//!
//! It is a feedback loop, and it is specific to *this* repository's
//! automation. [`crate::rank_defects`] counts an issue as a defect if it
//! carries `bug` **or** `triage` — an untriaged issue is a defect nobody has
//! classified yet. Meanwhile `.github/workflows/issue-triage.yml` adds
//! `triage` to any issue opened without one of its `TYPE_LABELS`
//! (`bug feature epic documentation question`), and the comment above that
//! rule says why: "Issues opened outside the template (`gh issue create`, the
//! API) carry no labels at all — those are exactly the ones this catches."
//!
//! A loop filing through the API is exactly that case. So an unlabelled
//! filing is stamped `triage` by the workflow, and the loop's own next cycle
//! reads it back as an untriaged defect it is responsible for triaging. The
//! loop manufactures its own queue, and every measure of whether it is
//! gaining or losing ground against the backlog silently includes its own
//! exhaust. `a_filing_that_omits_the_type_axis_is_refused` is the witness.
//!
//! # Why the standard is learned and not written down here
//!
//! Because it is not ours. A convention hardcoded here would fit this
//! repository and fail the next one, which is the same mistake
//! `stella_protocol::issue` refuses when it declines to model a Jira workflow
//! scheme — the customer's vocabulary is the customer's. What is portable is
//! the *shape* of a convention (axes, membership, requirement, reserved
//! labels) and the precedence rule for learning it, and that is all this
//! module holds.
//!
//! # No workspace dependency, deliberately
//!
//! This crate depends on no other workspace crate, which is what lets
//! `stella-observatory` link its folds without linking the machinery it
//! observes. So [`conform`] takes borrowed label text rather than a
//! `stella_protocol::Issue`, exactly as [`crate::rank_defects`] keeps its own
//! [`crate::QueueIssue`] input type for the same reason, and the one mapping
//! from a tracker's shape into this one lives in the caller.

use serde::{Deserialize, Serialize};

/// The label marking an issue the loop tried and could not resolve.
///
/// **The loop applies this one to itself.** Everything else in this module is
/// about classification a human owns; this is the loop recording its own
/// failure so the next run does not spend the same money discovering it again.
///
/// It is a *label* rather than a closure because the issue is not resolved: the
/// work is still wanted, and a human may pick it up, adjust the ceiling, or
/// break it into pieces the loop can take. Closing it would be a lie, and
/// leaving it unmarked is how a loop burns its budget on the same unsolvable
/// issue every cycle forever.
///
/// A session installs it if the tracker does not already carry it, because a
/// label that does not exist cannot be applied and the loop would fail at the
/// first escalation rather than at setup.
pub const ESCALATION_LABEL: &str = "agent-escalated";

/// Where a rule about how issues are written came from.
///
/// This is a precedence order, strongest first, and the ordering is the whole
/// value of the type: two sources will disagree, and the tie has to break the
/// same way every time or the loop's filings drift as the repository changes
/// around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConventionSource {
    /// Automation acts on it.
    ///
    /// The only source that is a fact rather than a claim. A workflow that
    /// adds `triage` to an untyped issue is not *describing* a convention, it
    /// **is** one — it will do that to the loop's filings whatever any
    /// document says. Read this first and let it win, because it is the only
    /// source that can be wrong about itself in no way at all.
    Enforced,
    /// A human wrote it down — an issue template, a label description, a
    /// `CONTRIBUTING` section, a slash command's handoff format.
    ///
    /// Authoritative about intent and silent about practice. It may describe
    /// a rule nothing enforces and everyone ignores.
    Declared,
    /// Inferred from the distribution over recent issues.
    ///
    /// The weakest source and the only one that is a guess. Used to fill an
    /// axis the two above leave undefined, never to override either. An
    /// inferred axis is recorded with this source precisely so a human
    /// reading the bound convention can see which parts nobody actually
    /// stated.
    Observed,
}

/// How many members of an axis a conformant issue carries.
///
/// Named for the count rather than for a policy word like `required`, because
/// the interesting failure is not "missing" — it is **ambiguous**. Two
/// priority labels on one issue is not a stricter filing; it is a filing whose
/// rank is decided by whichever label the ranker happens to test first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AxisRequirement {
    /// Exactly one member. Absent and ambiguous are both violations.
    ExactlyOne,
    /// At most one member. Absent is legal; two is not.
    AtMostOne,
    /// Any number, including none. An axis that only groups.
    Any,
}

/// One dimension a repository classifies issues along.
///
/// `type`, `priority`, `area` are this repository's three; nothing here
/// assumes those names or that count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelAxis {
    /// What the axis is called, for a human reading a refusal.
    pub name: String,
    /// The labels that belong to it, as the tracker spells them.
    pub members: Vec<String>,
    /// How many of them a conformant issue carries.
    pub requirement: AxisRequirement,
    /// Where this axis was learned from.
    pub source: ConventionSource,
}

impl LabelAxis {
    /// Which members of this axis appear in `labels`.
    fn present<'a>(&self, labels: &[&'a str]) -> Vec<&'a str> {
        labels
            .iter()
            .filter(|l| self.members.iter().any(|m| m == *l))
            .copied()
            .collect()
    }
}

/// Whether a discovered convention may steer a filing yet.
///
/// The loop may propose any convention and grant itself none — §7's rule,
/// pointed at classification. This is the field that makes that structural
/// rather than a promise in a doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Acceptance {
    /// Learned from the repository and bound. Filings conform to it.
    Bound,
    /// The loop found no standard and is proposing one.
    ///
    /// A convention in this state is **inert**: [`conform`] refuses every
    /// filing against it, because a loop that invented a taxonomy and then
    /// filed under it has granted itself the authority to define how this
    /// repository classifies work. Acceptance is a human's, and under
    /// governance mode `regulated` it stays one.
    Proposed,
}

/// The bound classification scheme for one repository's tracker.
///
/// Serialized to a source-tracked manifest beside the provider manifest under
/// `.stella/issues/`, for the same reason the provider manifest is tracked: a
/// convention that was *inferred* has to be readable by a human before it
/// steers a filing. The loop never infers and files in one motion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacklogConvention {
    /// The dimensions, in the order a filing should be read.
    pub axes: Vec<LabelAxis>,
    /// Labels the loop must never apply itself.
    ///
    /// `triage` is this repository's: it marks an issue that arrived from
    /// outside without a type, and the loop knows what it found — applying it
    /// would be claiming to be a stranger to its own filing. Reserved is not
    /// the same as unknown: a reserved label is one the loop is *forbidden*
    /// to set, not one it fails to recognize.
    #[serde(default)]
    pub reserved: Vec<String>,
    /// Whether this convention may steer a filing.
    pub acceptance: Acceptance,
}

/// One reason a filing is not conformant.
///
/// Typed rather than a message string (invariant 5), and the variants are
/// split by **what the caller does differently**: a missing axis is fixable by
/// choosing a label, an ambiguous one by dropping a label, a reserved one by
/// removing it, and an unaccepted convention is not fixable by the loop at
/// all — it needs a human.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "violation")]
pub enum Violation {
    /// An `ExactlyOne` axis had no member present.
    AxisMissing {
        /// The axis that was not satisfied.
        axis: String,
        /// What could have satisfied it, so a refusal is actionable.
        candidates: Vec<String>,
    },
    /// An axis admitting at most one member had several.
    AxisAmbiguous {
        /// The axis that was over-satisfied.
        axis: String,
        /// The members that were all present.
        present: Vec<String>,
    },
    /// The filing carried a label the loop is forbidden to apply.
    ReservedApplied {
        /// The reserved label that appeared.
        label: String,
    },
    /// The convention has not been accepted, so nothing may be filed under it.
    ConventionUnaccepted,
}

/// The verdict on one prospective filing.
///
/// Deliberately not a `bool` and not a `Result<(), Vec<Violation>>`: the
/// caller has to render every violation to a human at once. A filing refused
/// one reason at a time is a filing refused four times.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "conformance")]
pub enum Conformance {
    /// The filing may be sent to the tracker.
    Conformant,
    /// It may not, and here is every reason.
    Refused {
        /// Every violation found, in axis order then reserved order. Never
        /// empty — an empty refusal would be a conformant filing.
        violations: Vec<Violation>,
    },
}

impl Conformance {
    /// Whether the filing may be sent.
    #[must_use]
    pub fn is_conformant(&self) -> bool {
        matches!(self, Self::Conformant)
    }
}

/// Decide whether a prospective filing may be sent to the tracker.
///
/// **Conform or refuse**, never best-effort. The module docs carry the
/// argument: a filing the tracker's automation will re-label is a filing the
/// loop will read back as somebody else's untriaged defect.
///
/// `labels` is the set the loop proposes to apply. Duplicates are tolerated —
/// a label applied twice is one label — but they are counted once, so
/// `["P1", "P1"]` is not ambiguous.
///
/// # Examples
///
/// Both directions, because the function is a gate rather than advice: a
/// filing that satisfies every axis passes, and one that does not is refused
/// with every reason rather than degraded to a warning.
///
/// ```
/// # use stella_autonomy::{BacklogConvention, Acceptance, AxisRequirement,
/// #     ConventionSource, LabelAxis, conform};
/// let convention = BacklogConvention {
///     axes: vec![LabelAxis {
///         name: "type".into(),
///         members: vec!["bug".into(), "feature".into()],
///         requirement: AxisRequirement::ExactlyOne,
///         source: ConventionSource::Enforced,
///     }],
///     reserved: vec![],
///     acceptance: Acceptance::Bound,
/// };
///
/// assert!(conform(&convention, &["bug"]).is_conformant());
/// assert!(!conform(&convention, &["P1"]).is_conformant());
/// ```
#[must_use]
pub fn conform(convention: &BacklogConvention, labels: &[&str]) -> Conformance {
    if convention.acceptance == Acceptance::Proposed {
        return Conformance::Refused {
            violations: vec![Violation::ConventionUnaccepted],
        };
    }

    let mut violations = Vec::new();

    for axis in &convention.axes {
        let mut present = axis.present(labels);
        present.sort_unstable();
        present.dedup();

        match (axis.requirement, present.len()) {
            (AxisRequirement::ExactlyOne, 0) => violations.push(Violation::AxisMissing {
                axis: axis.name.clone(),
                candidates: axis.members.clone(),
            }),
            (AxisRequirement::ExactlyOne | AxisRequirement::AtMostOne, n) if n > 1 => {
                violations.push(Violation::AxisAmbiguous {
                    axis: axis.name.clone(),
                    present: present.iter().map(|s| (*s).to_owned()).collect(),
                });
            }
            _ => {}
        }
    }

    for label in &convention.reserved {
        if labels.contains(&label.as_str()) {
            violations.push(Violation::ReservedApplied {
                label: label.clone(),
            });
        }
    }

    if violations.is_empty() {
        Conformance::Conformant
    } else {
        Conformance::Refused { violations }
    }
}

/// Why an axis needs a decision rather than a mechanical fix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum ChoiceReason {
    /// No member of the axis is present.
    Missing,
    /// Several are, and only one may be.
    Ambiguous {
        /// Which ones.
        present: Vec<String>,
    },
}

/// An axis a judgement has to settle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AxisChoice {
    /// Which axis.
    pub axis: String,
    /// What could satisfy it.
    pub candidates: Vec<String>,
    /// Why it needs deciding.
    pub reason: ChoiceReason,
}

/// What would bring an existing issue into conformance.
///
/// Split into what a machine may do and what it may not, because those are
/// different authorities and collapsing them is how a loop starts inventing
/// classifications.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repair {
    /// Labels to remove. Decidable without judgement — these are the reserved
    /// ones the loop is forbidden to carry.
    pub remove: Vec<String>,
    /// Axes something with judgement must settle.
    ///
    /// **Never auto-filled.** Choosing between `bug` and `feature`, or between
    /// `P0` and `P2`, is a reading of what the issue says; a loop that guessed
    /// would be manufacturing the very classification the queue is ranked by,
    /// and the ranking would then be measuring its own invention.
    pub choose: Vec<AxisChoice>,
}

impl Repair {
    /// Whether nothing needs doing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.remove.is_empty() && self.choose.is_empty()
    }

    /// Whether a machine can finish this alone.
    #[must_use]
    pub fn is_mechanical(&self) -> bool {
        self.choose.is_empty()
    }
}

/// What it would take to bring `labels` into conformance.
///
/// The counterpart to [`conform`]: that decides whether a *filing* may go out,
/// this decides what an *existing* issue is missing. One rule, both directions
/// — which is what keeps the loop's own filings and the backlog it inherited
/// held to the same standard.
#[must_use]
pub fn repair(convention: &BacklogConvention, labels: &[&str]) -> Repair {
    let mut out = Repair::default();

    for label in &convention.reserved {
        if labels.contains(&label.as_str()) {
            out.remove.push(label.clone());
        }
    }

    for axis in &convention.axes {
        let mut present = axis.present(labels);
        present.sort_unstable();
        present.dedup();

        match (axis.requirement, present.len()) {
            (AxisRequirement::ExactlyOne, 0) => out.choose.push(AxisChoice {
                axis: axis.name.clone(),
                candidates: axis.members.clone(),
                reason: ChoiceReason::Missing,
            }),
            (AxisRequirement::ExactlyOne | AxisRequirement::AtMostOne, n) if n > 1 => {
                out.choose.push(AxisChoice {
                    axis: axis.name.clone(),
                    candidates: axis.members.clone(),
                    reason: ChoiceReason::Ambiguous {
                        present: present.iter().map(|s| (*s).to_owned()).collect(),
                    },
                });
            }
            _ => {}
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The authority witness.** A missing type axis is reported as something
    /// to *decide*, never filled in. A loop that guessed `bug` would be
    /// manufacturing the classification the queue is ranked by, and the
    /// ranking would then be measuring the loop's own invention.
    #[test]
    fn a_missing_axis_is_a_choice_never_a_guess() {
        let r = repair(&stella(), &["P1", "area:core"]);

        assert!(r.remove.is_empty(), "nothing to remove: {r:?}");
        assert_eq!(r.choose.len(), 1, "{r:?}");
        assert_eq!(r.choose[0].axis, "type");
        assert_eq!(r.choose[0].reason, ChoiceReason::Missing);
        assert!(!r.is_mechanical(), "a machine must not finish this alone");
    }

    /// The mechanical half: a reserved label the loop is forbidden to carry is
    /// removable without judgement, so triage can do it unattended.
    #[test]
    fn a_reserved_label_is_removed_mechanically() {
        let r = repair(&stella(), &["bug", "triage"]);

        assert_eq!(r.remove, vec!["triage".to_owned()]);
        assert!(r.choose.is_empty(), "{r:?}");
        assert!(r.is_mechanical(), "a machine can finish this alone");
    }

    /// Which of two priorities to drop is also a reading of the issue, so it
    /// is a choice rather than a removal.
    #[test]
    fn an_ambiguous_axis_is_a_choice_not_an_arbitrary_removal() {
        let r = repair(&stella(), &["bug", "P0", "P2"]);

        assert!(r.remove.is_empty(), "must not drop one at random: {r:?}");
        assert_eq!(
            r.choose[0].reason,
            ChoiceReason::Ambiguous {
                present: vec!["P0".into(), "P2".into()],
            }
        );
    }

    /// A conformant issue needs nothing, so triage leaves it alone.
    #[test]
    fn a_conformant_issue_needs_no_repair() {
        assert!(repair(&stella(), &["bug", "P1", "area:core"]).is_empty());
    }

    /// `repair` and `conform` are the same rule pointed in two directions: an
    /// issue needing no repair is one whose labels would pass as a filing.
    #[test]
    fn repair_and_conform_agree_on_every_case() {
        for labels in [
            vec!["bug", "P1", "area:core"],
            vec!["P1"],
            vec!["bug", "triage"],
            vec!["bug", "P0", "P2"],
            vec!["bug"],
        ] {
            let convention = stella();
            assert_eq!(
                repair(&convention, &labels).is_empty(),
                conform(&convention, &labels).is_conformant(),
                "the two disagreed about {labels:?}"
            );
        }
    }

    /// This repository's actual convention, as
    /// `.github/workflows/issue-triage.yml` enforces it and
    /// `scripts/self-driving/commands/self-driving/tickets.md` declares it.
    fn stella() -> BacklogConvention {
        BacklogConvention {
            axes: vec![
                LabelAxis {
                    name: "type".into(),
                    members: ["bug", "feature", "epic", "documentation", "question"]
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect(),
                    requirement: AxisRequirement::ExactlyOne,
                    source: ConventionSource::Enforced,
                },
                LabelAxis {
                    name: "priority".into(),
                    members: ["P0", "P1", "P2"].iter().map(|s| (*s).to_owned()).collect(),
                    requirement: AxisRequirement::AtMostOne,
                    source: ConventionSource::Declared,
                },
                LabelAxis {
                    name: "area".into(),
                    members: ["area:core", "area:cli", "area:protocol"]
                        .iter()
                        .map(|s| (*s).to_owned())
                        .collect(),
                    requirement: AxisRequirement::Any,
                    source: ConventionSource::Declared,
                },
            ],
            reserved: vec!["triage".into()],
            acceptance: Acceptance::Bound,
        }
    }

    /// The witness. A filing with no type label is refused *before* it reaches
    /// the tracker, rather than being filed, stamped `triage` by
    /// `issue-triage.yml`, and read back by `rank_defects` as an untriaged
    /// defect the loop must now triage.
    #[test]
    fn a_filing_that_omits_the_type_axis_is_refused() {
        let verdict = conform(&stella(), &["P1", "area:core"]);

        let Conformance::Refused { violations } = verdict else {
            panic!("an untyped filing must not reach the tracker: {verdict:?}");
        };
        assert_eq!(
            violations,
            vec![Violation::AxisMissing {
                axis: "type".into(),
                candidates: vec![
                    "bug".into(),
                    "feature".into(),
                    "epic".into(),
                    "documentation".into(),
                    "question".into(),
                ],
            }]
        );
    }

    #[test]
    fn a_fully_classified_filing_is_conformant() {
        assert!(conform(&stella(), &["bug", "P1", "area:core"]).is_conformant());
    }

    /// The priority axis admits at most one member, and two is not a stricter
    /// filing — it is one whose rank depends on which label the ranker tests
    /// first.
    #[test]
    fn two_members_of_a_single_valued_axis_are_ambiguous_not_stricter() {
        let verdict = conform(&stella(), &["bug", "P0", "P2"]);

        let Conformance::Refused { violations } = verdict else {
            panic!("two priorities must not pass: {verdict:?}");
        };
        assert_eq!(
            violations,
            vec![Violation::AxisAmbiguous {
                axis: "priority".into(),
                present: vec!["P0".into(), "P2".into()],
            }]
        );
    }

    /// An axis whose requirement is `Any` never fails, in either direction.
    #[test]
    fn an_any_axis_accepts_none_and_several() {
        assert!(conform(&stella(), &["bug"]).is_conformant());
        assert!(conform(&stella(), &["bug", "area:core", "area:cli"]).is_conformant());
    }

    /// `triage` marks an issue that arrived from outside without a type. The
    /// loop knows what it found, so applying it would be claiming to be a
    /// stranger to its own filing.
    #[test]
    fn the_loop_may_not_apply_a_reserved_label() {
        let verdict = conform(&stella(), &["bug", "triage"]);

        let Conformance::Refused { violations } = verdict else {
            panic!("the loop must not triage its own filing: {verdict:?}");
        };
        assert_eq!(
            violations,
            vec![Violation::ReservedApplied {
                label: "triage".into(),
            }]
        );
    }

    /// Every reason at once: a filing refused one reason at a time is a filing
    /// refused four times.
    #[test]
    fn a_refusal_carries_every_violation_not_the_first() {
        let verdict = conform(&stella(), &["P0", "P1", "triage"]);

        let Conformance::Refused { violations } = verdict else {
            panic!("expected a refusal: {verdict:?}");
        };
        assert_eq!(violations.len(), 3, "{violations:?}");
        assert!(matches!(violations[0], Violation::AxisMissing { .. }));
        assert!(matches!(violations[1], Violation::AxisAmbiguous { .. }));
        assert!(matches!(violations[2], Violation::ReservedApplied { .. }));
    }

    /// A label applied twice is one label, not an ambiguity.
    #[test]
    fn a_duplicated_label_is_counted_once() {
        assert!(conform(&stella(), &["bug", "P1", "P1"]).is_conformant());
    }

    /// The structural half of "the loop may propose any convention and grant
    /// itself none". A proposed convention is inert: it does not merely warn,
    /// it refuses, so there is no code path where the loop files under a
    /// taxonomy it invented.
    #[test]
    fn a_proposed_convention_files_nothing() {
        let mut proposed = stella();
        proposed.acceptance = Acceptance::Proposed;

        let verdict = conform(&proposed, &["bug", "P1", "area:core"]);

        assert_eq!(
            verdict,
            Conformance::Refused {
                violations: vec![Violation::ConventionUnaccepted],
            },
            "a filing that would be conformant under a bound convention must \
             still be refused under a proposed one"
        );
    }

    /// Precedence is the whole value of [`ConventionSource`], so the ordering
    /// is asserted rather than left to the declaration order surviving an edit.
    #[test]
    fn enforced_outranks_declared_outranks_observed() {
        assert!(ConventionSource::Enforced < ConventionSource::Declared);
        assert!(ConventionSource::Declared < ConventionSource::Observed);
    }

    #[test]
    fn a_convention_round_trips_through_json() {
        let convention = stella();
        let json = serde_json::to_string(&convention).expect("serialize");
        let back: BacklogConvention = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(convention, back);
    }
}
