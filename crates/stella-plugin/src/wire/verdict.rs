// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What the host concluded about a turn, and why.
//!
//! The rule a manifest declares ([`VerdictRule`]), the three-answer conclusion
//! ([`Verdict`]), and the four types that say what was found: a failed clause,
//! an undecided one, and the two reason vocabularies behind them.
//!
//! Split out of `wire.rs` when that file met the 1500-line ceiling — which it
//! did on #5267, growing `Verdict` so a board can tell a clause that failed
//! from one nobody could decide. A subject rather than an arbitrary cut, and
//! re-exported from the parent so every `wire::Verdict` path still resolves.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{EvidenceProvenance, FlipObservation};
use crate::{Oracle, PluginManifest};

/// The rule that decides done, as data.
///
/// Assembled from what the manifest already declares — `[requirements]` and
/// the `[oracle]` block's flip/tamper policy and checks — so a Python author
/// and a Rust author write the identical artifact and neither writes a verdict
/// as code (`doc:pipeline-as-plugins` §6). It travels on the wire because a
/// remote host has to be able to evaluate it, but a plugin never *sends* one:
/// the rule is read from the manifest a human consented to at install.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerdictRule {
    /// The enumerable definition of done: requirement name → the statement a
    /// hold cites. A `BTreeMap`, so the order a verdict reports failures in is
    /// deterministic (AGENTS.md #7's discipline).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub requirements: BTreeMap<String, String>,
    /// The plugin's oracle — its flip/tamper policy and its checks, when the
    /// manifest declared one. The oracle runs in the plugin's own process and
    /// reports what it saw (#3511); what travels here is the *rule*, which the
    /// host reads off the manifest and evaluates itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle: Option<Oracle>,
}

impl VerdictRule {
    /// Read the rule a manifest declares.
    ///
    /// Total: a manifest with no requirements yields a rule with none, which
    /// `judge` answers [`Verdict::Met`] for — a steering-grade wrapper that
    /// contributes context and gathers nothing has nothing to hold open.
    #[must_use]
    pub fn from_manifest(manifest: &PluginManifest) -> Self {
        Self {
            requirements: manifest.requirements.clone().unwrap_or_default(),
            oracle: manifest.oracle.clone(),
        }
    }
}

/// What the host decided from the evidence.
///
/// Three answers, and the third one is why this is not a `bool`: "nothing
/// available proved it either way" is a real outcome, and reporting it as a
/// failure blames a worker for the instrument while reporting it as a pass is
/// the false claim this project exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Every declared requirement is met by the evidence — and on whose
    /// observation. The provenance is carried, never consulted: `judge`
    /// reaches this arm on the evidence alone (#3513).
    Met {
        /// Whose observation the requirements were met on.
        evidence: EvidenceProvenance,
    },
    /// At least one requirement is determinately unmet.
    Unmet {
        /// The failures, in requirement order.
        unmet: Vec<UnmetRequirement>,
        /// Requirements the evidence could not decide, in requirement order.
        ///
        /// Carried beside the failures rather than discarded (#5267). The
        /// verdict is still `Unmet` — a determinate failure outranks an
        /// abstention — but a clause nobody could decide is not a clause that
        /// held, and a board built from `unmet` alone painted it green. That
        /// is the flattering claim this path exists to prevent.
        ///
        /// `#[serde(default)]` because this is additive on the wire: a
        /// verdict serialized before #5267 carries no such list, and an empty
        /// one restores exactly the old reading.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        undecided: Vec<UndecidedRequirement>,
    },
    /// The evidence cannot decide. Scored as unverified: not a pass, and
    /// explicitly not a failure.
    Undecided {
        /// Why, in the words a report prints.
        ///
        /// The first undecidable clause in requirement order, unchanged — a
        /// report prints one sentence. `undecided` below is the same fact
        /// per requirement, for a reader who needs the rows.
        reason: UndecidedReason,
        /// Every requirement the evidence could not decide, in requirement
        /// order (#5267).
        ///
        /// Empty on a rule-wide abstention that names no requirement —
        /// [`UndecidedReason::NoOracle`], which is decided before any
        /// requirement is looked at — and on a verdict serialized before
        /// #5267. A reader that finds it empty must fall back to treating
        /// every requirement as undecided, which is what the whole verdict
        /// says.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        undecided: Vec<UndecidedRequirement>,
    },
}

impl Verdict {
    /// Attach the wrapper's own advisory note to every clause this verdict
    /// found unmet.
    ///
    /// **After the decision, never before it.** `judge` reads [`EvidenceSet`],
    /// whose fields are closed by construction so that totality is the
    /// compiler's job; a free-text field on that type would make it an
    /// argument instead. So the host decides first and enriches second, and
    /// nothing downstream of this can change a verdict — the only reader is
    /// the correction text a held-open round renders.
    ///
    /// One note across every clause because that is what the plugin reported:
    /// [`ObservedEvidence::detail`](crate::ObservedEvidence::detail) is one
    /// observation about the turn, not a claim about a particular requirement,
    /// and splitting it per clause would invent an attribution the plugin never
    /// made. The renderer prints it once.
    ///
    /// `None` — a plugin with nothing to add, or a verdict with nothing unmet —
    /// leaves everything exactly as `judge` left it.
    #[must_use]
    pub fn with_detail(mut self, detail: Option<String>) -> Self {
        if let (Some(detail), Self::Unmet { unmet, .. }) = (detail, &mut self) {
            for clause in unmet.iter_mut() {
                clause.detail = Some(detail.clone());
            }
        }
        self
    }
}

/// One requirement the evidence could not decide, and why (#5267).
///
/// The requirement travels *beside* the reason rather than inside it, because
/// [`UndecidedReason`] has two rule-wide arms that name none —
/// [`UndecidedReason::NoOracle`] and [`UndecidedReason::FlipUnobservable`].
/// The second is why the pairing is needed: an unobservable flip abstains for
/// every requirement the flip decides, so the same reason is paired with each
/// of them and a bare `Vec<UndecidedReason>` could not say which rows to draw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UndecidedRequirement {
    /// The `[requirements]` key.
    pub requirement: String,
    /// Its human-readable statement, carried for the same reason
    /// [`UnmetRequirement::statement`] is: a row is attributable without a
    /// second lookup.
    pub statement: String,
    /// What could not be decided about it.
    pub reason: UndecidedReason,
}

/// One clause of the definition of done that the evidence did not meet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnmetRequirement {
    /// The `[requirements]` key.
    pub requirement: String,
    /// Its human-readable statement, carried so a hold message is
    /// attributable without a second lookup.
    pub statement: String,
    /// What the evidence said.
    pub because: UnmetBecause,
    /// What the wrapper's own process wanted the next round told, in its own
    /// words — [`ObservedEvidence::detail`](crate::ObservedEvidence::detail),
    /// carried here and nowhere else (#3840).
    ///
    /// **Never an input to a verdict.** It is attached *after* `judge` has
    /// decided, by `Verdict::with_detail`, so the decision is still made over
    /// [`EvidenceSet`]'s closed fields alone and `judge` is still total over
    /// them. The only thing that reads this is the correction text a held-open
    /// round renders, which is exactly the fidelity the built-in goal loop had
    /// and a plugin did not: `stella_core::goal`'s `verifier_feedback_text`
    /// hands the worker the verifier's own sentence every round, where a
    /// plugin's correction was the same static `[requirements]` statement
    /// however the assessment differed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl std::fmt::Display for UnmetRequirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} — {}",
            self.requirement, self.statement, self.because
        )
    }
}

/// Why a requirement is unmet. Closed: each case is one thing the evidence
/// vocabulary can determinately say went wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnmetBecause {
    /// The declared flip was not observed.
    NoFlip {
        /// What was observed instead.
        observed: FlipObservation,
    },
    /// A declared budget was exceeded.
    Budget {
        /// The check as the manifest wrote it.
        check: String,
        /// What the oracle reported for the measurement it reads.
        reported: u64,
    },
    /// The witness artifacts changed between authoring and verification, so
    /// the flip is not credited however it landed.
    Tampered {
        /// The artifact whose identity changed.
        artifact: String,
    },
}

impl std::fmt::Display for UnmetBecause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoFlip { observed } => write!(f, "no fail→pass flip ({observed:?})"),
            Self::Budget { check, reported } => {
                write!(f, "budget \"{check}\" was reported as {reported}")
            }
            Self::Tampered { artifact } => {
                write!(f, "witness artifact \"{artifact}\" was modified")
            }
        }
    }
}

/// Why the evidence could not decide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndecidedReason {
    /// Requirements were declared but no `[oracle]` exists to establish them.
    NoOracle,
    /// A requirement no check decides, under a policy with no flip to decide
    /// it either. Rejected at load
    /// ([`ManifestError::UndecidableRequirement`](crate::ManifestError)), so
    /// this is reachable only for a rule that did not come from a validated
    /// manifest.
    Undecidable {
        /// The requirement nothing could establish.
        requirement: String,
    },
    /// The oracle reported no value for a measurement one of its checks reads.
    MeasurementMissing {
        /// The requirement the check decides.
        requirement: String,
        /// The measurement that was absent from the reported set.
        measurement: String,
    },
    /// A declared check could not be read. Rejected at load, so reachable only
    /// for a hand-built rule.
    UnreadableCheck {
        /// The requirement the check decides.
        requirement: String,
        /// Why it could not be read.
        reason: String,
    },
    /// The flip evidence itself was unavailable: the witness could not be run,
    /// or the wrapper never gathered anything.
    FlipUnobservable,
    /// The witness failed identically before and after, so it discriminates
    /// nothing.
    WitnessUnsatisfiable,
    /// The declared tamper policy demands a snapshot and none was taken, so
    /// the flip cannot be credited or refused.
    TamperUnchecked,
}
