// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The evolution-surface matrix — one reviewed row per way Stella changes
//! itself, so "what may Stella change about itself today, and on what
//! evidence?" is a question with one answer someone wrote down (#2780).
//!
//! # Why a fourth ledger
//!
//! This repository already enforces three declaration ledgers, each born from
//! the same defect shape — a capability that shipped on one axis and silently
//! missed another, found by a run paying for it rather than by a test:
//! provider feature parity (`stella-model`'s `provider_parity`,
//! `invariant #8`), cross-surface capability parity ([`crate`], the sibling of
//! this module), and signal consumption (`stella-protocol`'s
//! `event::consumers`, `invariant #10`).
//!
//! Self-improvement had none, and it is the surface where a gap costs most.
//! Stella changes itself through several distinct object classes, and each one
//! used to answer "when may this change, on what evidence, published by whom,
//! and how is it undone?" in a different file or nowhere at all. Skills
//! promote on measured lift; context records have a governance mode and a
//! hash-chained promotions ledger; weight-space adapters were designed with a
//! promotion gate that nothing structurally enforced. No single reviewed table
//! held the six answers side by side, which is exactly what the other three
//! ledgers exist to provide.
//!
//! # The three instruments
//!
//! - **A row per surface, and no way to skip one.** [`EvolutionSurface`] and
//!   [`EVOLUTION_SURFACES`] are generated from one `evolution_surfaces!`
//!   invocation, so a surface and its row are the same table cell. A new
//!   surface with no row is not a red test that can be forgotten — it is
//!   unwritable, because the only place a surface can be named is the place
//!   the row is required. This is the `consumers.rs` treatment #2780 asked
//!   for, one turn stronger: there the enum is hand-written and omission is
//!   an `E0004`, and here there is no hand-written enum to omit from.
//! - **Witness tests named and checked.** A [`EvolutionPosture::Shipped`] or
//!   [`EvolutionPosture::Experimental`] row names the test proving the surface
//!   can actually be changed, and this module's tests fail when that function
//!   no longer exists in the swept sources — the same substring check
//!   `provider_parity` and [`crate::CAPABILITIES`] use.
//! - **Gaps cited, never silent.** [`EvolutionPosture::Planned`] cites the
//!   GitHub issue deciding it, [`EvolutionPosture::Prohibited`] cites a stable
//!   design document by `id`, and [`EvolutionPosture::ShippedUnwitnessed`] is
//!   bounded by [`UNWITNESSED_EVOLUTION_BASELINE`], a ratchet that only goes
//!   down.
//!
//! # The evidence column does not restate the policy
//!
//! #2780 asks that this ledger's `evidence` column reference the provenance
//! grades from #2782 "so the two ledgers cannot drift". Referencing them is
//! not quite enough — a row could still name a grade the policy would not
//! agree with, and nothing would notice.
//!
//! So a row does not declare a grade at all. It declares its
//! [`ImpactClass`] — what a wrong change to this surface can break — and the
//! required grade and authority are *derived* from it through
//! [`ImpactClass::required_grade`] and [`ImpactClass::required_authority`].
//! There is one policy, it lives in `stella-protocol`, and this ledger reads
//! it rather than copying it. Changing what a blocking guard costs changes
//! every row that publishes one, in the same edit.

use stella_protocol::provenance::{ImpactClass, ProvenanceGrade, PublicationAuthority};

/// When a change to a surface takes effect.
///
/// The column exists because "Stella may change this" means something very
/// different for a value the running turn picks up than for a file the next
/// session reads. A surface that mutates mid-turn can change the behaviour
/// that is producing the evidence for the change.
///
/// #2780 wrote the middle rung as "end-of-session". It is spelled
/// [`EvolutionTiming::BetweenTurns`] here because that is when the code
/// actually lands changes: the memory sweep and skill creation both run on the
/// post-turn reflection path, mid-session, and the next turn sees the result.
/// No live surface waits for the session to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvolutionTiming {
    /// The running turn observes the change. The most dangerous timing: the
    /// loop can alter its own behaviour while the evidence for altering it is
    /// still being gathered. No row claims it today.
    InTurn,
    /// The change lands after a turn and the next turn in the same session
    /// sees it.
    BetweenTurns,
    /// The change is made outside any turn — an operator command or a batch
    /// run — and is picked up when a session next loads it.
    OfflineBatch,
}

impl EvolutionTiming {
    /// The canonical `snake_case` tag.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InTurn => "in_turn",
            Self::BetweenTurns => "between_turns",
            Self::OfflineBatch => "offline_batch",
        }
    }
}

/// How a surface stands today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvolutionPosture {
    /// Stella changes this surface today, with the named witness test proving
    /// the path — checked for existence by this module's tests.
    Shipped {
        /// The mechanism, for a human reading the matrix.
        mechanism: &'static str,
        /// The test function proving the surface can be changed.
        witness: &'static str,
    },
    /// Stella changes this surface today, but nothing pins the path. A debt
    /// this matrix counts rather than hides, bounded by
    /// [`UNWITNESSED_EVOLUTION_BASELINE`].
    ShippedUnwitnessed {
        mechanism: &'static str,
        /// What a witness for this surface would pin.
        missing: &'static str,
    },
    /// The path exists and is not trusted yet: reachable only behind an
    /// explicit opt-in, and still named by a witness.
    Experimental {
        mechanism: &'static str,
        witness: &'static str,
        /// What has to be true before this becomes `Shipped`.
        graduates_when: &'static str,
    },
    /// Not built. Cites the GitHub issue where it is being decided, so a
    /// parked surface is distinguishable from a forgotten one.
    Planned {
        /// A `#NNNN` GitHub issue reference.
        issue: &'static str,
        /// What exists today in place of the surface, if anything.
        today: &'static str,
    },
    /// Stella is not permitted to change this surface, with the design reason
    /// a reviewer can check. Cites a design document by `id` — never by path,
    /// which moves.
    Prohibited {
        /// A `doc:<id>` reference.
        design_doc: &'static str,
        reason: &'static str,
    },
    /// Considered, and dropped. Cites the issue where that was decided.
    ///
    /// #2780's four postures do not describe this, and the tree contains it:
    /// weight-space adaptation was designed, filed, and closed as **not
    /// planned**. Calling that `Planned` would be false, and `Prohibited`
    /// wants a design document that was never written — the decision lives in
    /// the closed issue and nowhere else. A fifth posture costs one line; the
    /// alternative is a row that reads as parked work somebody will pick up.
    NotPursued {
        /// A `#NNNN` GitHub issue, closed as not planned.
        decided_in: &'static str,
        /// What exists in place of the surface, so the row is not read as
        /// "nothing here".
        today: &'static str,
    },
}

impl EvolutionPosture {
    /// The witness this posture names, if it names one.
    #[must_use]
    pub fn witness(&self) -> Option<&'static str> {
        match self {
            Self::Shipped { witness, .. } | Self::Experimental { witness, .. } => Some(witness),
            Self::ShippedUnwitnessed { .. }
            | Self::Planned { .. }
            | Self::Prohibited { .. }
            | Self::NotPursued { .. } => None,
        }
    }

    /// Whether Stella can change this surface at all today.
    #[must_use]
    pub fn is_live(&self) -> bool {
        matches!(
            self,
            Self::Shipped { .. } | Self::ShippedUnwitnessed { .. } | Self::Experimental { .. }
        )
    }
}

/// One evolution surface and the terms on which it may change.
#[derive(Debug, Clone, Copy)]
pub struct EvolutionRow {
    /// The surface this row governs.
    pub surface: EvolutionSurface,
    /// How it stands today.
    pub posture: EvolutionPosture,
    /// When a change takes effect.
    pub timing: EvolutionTiming,
    /// **What a wrong change here can break.** The evidence and authority
    /// columns are derived from this through #2782's policy rather than
    /// restated, so the two ledgers cannot disagree — see
    /// [`EvolutionRow::required_evidence`].
    pub impact: ImpactClass,
    /// The named artifact that reverses a change to this surface. Prose a
    /// reviewer can check, and the column that turns "Stella changed itself"
    /// into something a human can undo.
    pub rollback: &'static str,
}

impl EvolutionRow {
    /// The weakest evidence that may change this surface, read out of #2782's
    /// policy rather than declared here.
    #[must_use]
    pub fn required_evidence(&self) -> ProvenanceGrade {
        self.impact.required_grade()
    }

    /// The weakest authority that may publish a change to this surface, read
    /// out of the same policy.
    #[must_use]
    pub fn required_authority(&self) -> PublicationAuthority {
        self.impact.required_authority()
    }
}

/// Generate the surface enum and its ledger from one table.
///
/// The point of the macro is that there is no second place to edit. A new
/// evolution surface is a new arm here, and an arm carries every column, so a
/// surface that declares nothing is not a row someone forgot — it is a program
/// that does not compile.
macro_rules! evolution_surfaces {
    ($(
        $(#[$meta:meta])*
        $surface:ident => $tag:literal, $posture:expr, $timing:expr, $impact:expr, $rollback:literal;
    )*) => {
        /// A class of thing Stella can change about itself.
        ///
        /// Generated together with [`EVOLUTION_SURFACES`], so a surface and
        /// its row are one edit.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum EvolutionSurface {
            $($(#[$meta])* $surface,)*
        }

        impl EvolutionSurface {
            /// Every surface, in ledger order.
            pub const ALL: &'static [Self] = &[$(Self::$surface,)*];

            /// The canonical `snake_case` tag.
            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$surface => $tag,)*
                }
            }

            /// This surface's row.
            ///
            /// Total by construction, and by the cheapest possible route: the
            /// enum carries no payload and both lists come from one table, so
            /// a surface's discriminant *is* its row index. Pinned by
            /// `a_surfaces_discriminant_is_its_row_index`, which fails if the
            /// enum ever grows a payload and the two stop lining up.
            #[must_use]
            pub fn row(self) -> &'static EvolutionRow {
                &EVOLUTION_SURFACES[self as usize]
            }
        }

        /// The ledger: one row per surface, generated from the same table as
        /// [`EvolutionSurface`].
        pub static EVOLUTION_SURFACES: &[EvolutionRow] = &[
            $(EvolutionRow {
                surface: EvolutionSurface::$surface,
                posture: $posture,
                timing: $timing,
                impact: $impact,
                rollback: $rollback,
            },)*
        ];
    };
}

evolution_surfaces! {
    /// Prompts, policies, and the loop's own instructions.
    Framework => "framework",
        EvolutionPosture::Shipped {
            mechanism: "the rules miner induces directive candidates from reflection \
                        observations and `stella proposals` publishes the kept ones as TOML \
                        records under `.stella/rules/`, which `FsRuleSource` loads into the \
                        system prefix. The system prompt template itself is assembled and \
                        never self-edited: no path writes AGENTS.md or CLAUDE.md",
            witness: "mined_rules_land_where_the_loader_reads",
        },
        EvolutionTiming::OfflineBatch,
        ImpactClass::SteeringDirective,
        "no command retracts a published rule — `stella proposals` offers only \
         list/keep/edit/ignore/refresh, and `stella memory retire` does not suppress a \
         file-backed rule because the loader filters by authority rather than by retirement \
         standing. Reversal today is deleting or reverting `.stella/rules/<id>.toml`";

    /// What Stella remembers between turns.
    Memory => "memory",
        EvolutionPosture::Shipped {
            mechanism: "uses and citations are folded into selection health per record, and \
                        the post-turn sweep retires one that keeps failing",
            witness: "the_loop_closes_a_repeatedly_unhelpful_record_is_retired_and_restorable",
        },
        EvolutionTiming::BetweenTurns,
        ImpactClass::RecallBias,
        "`stella memory reaffirm <id>` and `stella memory restore <id>`. Nothing is \
         deleted: standings are a last-write-wins fold over append-only promotion events, so \
         a retirement is reversed by appending, never by editing history";

    /// Reusable procedures Stella writes for itself.
    Skill => "skill",
        EvolutionPosture::Shipped {
            mechanism: "mined candidates clear the distinct-task floor and are written as \
                        SKILL.md. The measured-lift gate exists and is off by default \
                        (`SkillMineConfig::require_measured_lift` is `false`), so the shipped \
                        default promotes on mining eligibility — which is why this row's \
                        impact is an advisory record and not a steering one",
            witness: "a_skill_that_helps_is_promoted_with_its_lift_recorded",
        },
        EvolutionTiming::BetweenTurns,
        ImpactClass::AdvisoryRecord,
        "none wired. The demotion half — `record_turn`, `sweep`, `record_appraisal`, \
         `queued_candidates` — carries `#[allow(dead_code)]` for want of a production caller, \
         so a promoted skill that later regresses is demoted by nothing. Tracked in #4754";

    /// Executable capability Stella adds to its own working surface.
    Tool => "tool",
        EvolutionPosture::Shipped {
            mechanism: "the foundry's authored → staged → adopted → enabled protocol: a \
                        staged tool is invisible to discovery, an adopted one is still \
                        uncallable until enabled, and `recheck_before_launch` re-digests the \
                        bytes on every call, so a script rewritten mid-session stops launching",
            witness: "a_tool_is_unreachable_until_it_is_both_proven_and_approved",
        },
        EvolutionTiming::OfflineBatch,
        ImpactClass::ExecutableTool,
        "`stella tools --disable <name>` stops offering an adopted tool while keeping its \
         proof on file; `forget_foundry_tool` removes the adoption and its approval outright";

    /// How Stella configures its own runs.
    Workflow => "workflow",
        EvolutionPosture::Shipped {
            mechanism: "`stella tune` reads two measured result files and the local ledger \
                        and writes settings only under `--promote` — no provider call, no \
                        API key, so the evidence is a measurement rather than a judgement",
            witness: "a_clean_win_promotes_and_rollback_restores",
        },
        EvolutionTiming::OfflineBatch,
        ImpactClass::SteeringDirective,
        "`stella tune rollback` replays an append-only rollback ledger under \
         `.stella/private/` and restores the prior setting, including the `effort_auto` flag \
         the promotion also moved";

    /// The weights Stella runs on.
    Model => "model",
        EvolutionPosture::NotPursued {
            decided_in: "#836",
            today: "`stella dataset export` writes an SFT corpus plus a manifest and stops. \
                    The corpus has no in-tree consumer: there is no trainer, no adapter \
                    registry, and no promotion loop anywhere in the workspace, and human \
                    sign-off is required before a dataset reaches a training run",
        },
        EvolutionTiming::OfflineBatch,
        ImpactClass::ExecutableTool,
        "not applicable — nothing is promoted, so there is nothing to reverse. Were this \
         surface built, the rollback artifact would have to exist before the promotion path did";
}

/// How many live rows name no witness.
///
/// A ratchet that only goes down, checked for **exact** equality like
/// [`crate::UNWITNESSED_BASELINE`] and `provider_parity`'s: writing a missing
/// witness promotes the row and lowers this number in the same PR, and raising
/// it to turn a red gate green is the expedient CLAUDE.md forbids.
///
/// It is zero, and the way to keep it zero is to write the witness before
/// adding the row.
pub const UNWITNESSED_EVOLUTION_BASELINE: usize = 0;

/// The sources swept for witness functions.
///
/// Each file is named individually rather than globbed, for the reason
/// [`crate::CAPABILITIES`]'s sweep names its own: a file-size split moves tests
/// out from under a parent module, and an explicit list fails loudly when that
/// happens instead of silently sweeping less.
#[cfg(test)]
fn evolution_sources() -> [&'static str; 5] {
    [
        include_str!("../../stella-cli/src/memory/rules_mining/tests.rs"),
        include_str!("../../stella-cli/src/memory/uses/tests.rs"),
        include_str!("../../stella-core/src/skills/appraisal/tests.rs"),
        include_str!("../../stella-cli/src/tool_foundry/adopt/tests.rs"),
        include_str!("../../stella-cli/src/memory/self_tuning.rs"),
    ]
}

#[cfg(test)]
mod tests;
