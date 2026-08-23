// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! How each built-in tool's output relates to loop comparison — the input
//! contract of `stella_core::loop_detect`, declared per tool and checked from
//! both sides.
//!
//! # The hazard
//!
//! Every loop verdict is defined on tool output **bytes**:
//! `exact_repeat_threshold` counts identical `(name + input + output)` calls,
//! and the stagnation rung counts byte-identical outputs. One timestamp, one
//! elapsed-milliseconds figure, one running counter in a `ToolOutput` makes
//! both rungs permanently blind *for that tool* — the tool's own bytes disarm
//! the detector, and nothing errors, so the only symptom is a turn that spins
//! until `max_steps`.
//!
//! The rule against it existed only as prose in this crate's README plus
//! scattered discipline: `save_state`'s success line carries a
//! content-derived digest and says why (#3297), `read_file`'s volatile footer
//! has a normalizer in `stella-core` pinned by one cross-crate test, and the
//! halted-tool result is a fixed string for this exact reason. Nothing
//! stopped the next tool from shipping an elapsed time in its verdict, which
//! is the posture `stella_store::content_free` replaced for egress: a
//! reviewed table plus enforcement from both sides.
//!
//! # What is enforced
//!
//! - **Totality.** Every name in [`crate::catalog::ALL_NAMES`] has exactly one
//!   row here, and every row names a live tool. A tool added without a row
//!   fails `every_catalog_tool_declares_its_loop_comparability`, so the
//!   question is answered in the PR that adds the tool.
//! - **The sentinel.** Each [`LoopComparability::Deterministic`] tool is
//!   driven twice through the real [`crate::registry::ToolRegistry`], in one
//!   session, with its fixture restored before each call, and its two
//!   `comparable_output` renderings must be byte-identical. Each
//!   [`LoopComparability::VolatileWithNormalizer`] tool is driven the same way
//!   and must produce outputs that **differ raw** and **match normalized** —
//!   the first half is what proves the fixture actually exercises the volatile
//!   bytes, without which the second half is a test of nothing.
//! - **A negative control.** `the_sentinel_can_fail` shows the comparison
//!   separates a volatile output from a stable one, so a green run above is a
//!   result rather than a harness that cannot see.
//!
//! # What is not
//!
//! An [`LoopComparability::ExemptWorldState`] row is not sentinel-tested —
//! that is what the exemption *is*. Its `rationale` is prose for a reviewer,
//! and the reviewable claim is narrow: this tool's output describes state the
//! harness does not own, so two identical calls legitimately differ and the
//! detector cannot compare them. Eight of the built-ins are in that class
//! today and each says which state it means.
//!
//! Part of epic #2701, whose converse case this is: not a produced signal
//! with no consumer, but a consumer whose input contract was enforced by
//! nothing (#2706).

/// How one tool's output relates to loop comparison.
///
/// Every registered tool declares one. `VolatileWithNormalizer` is the
/// `read_file` case: volatile bytes are permitted **only** when
/// `stella_core::driver::loop_evidence::comparable_output` strips them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopComparability {
    /// Byte-identical output for an identical `(input, session, workspace)`.
    Deterministic,
    /// Output carries volatile bytes, and the normalizer that strips them
    /// exists. Both fields are for a reader: the enforcement is the sentinel,
    /// which requires the raw outputs to differ and the normalized ones to
    /// match.
    VolatileWithNormalizer {
        /// The normalizer seam in `stella-core` that strips the volatile
        /// bytes.
        normalizer: &'static str,
        /// The test pinning the two ends of that contract together.
        pinned_by: &'static str,
    },
    /// Inherently volatile and deliberately exempt: the output describes
    /// state this crate does not own and cannot restore, so two identical
    /// calls legitimately produce different bytes.
    ExemptWorldState {
        /// Which state, and why restoring it is not this harness's to do.
        rationale: &'static str,
    },
}

/// One row per name in [`crate::catalog::ALL_NAMES`], in the same order.
///
/// A table rather than a match arm so the tests can walk it: totality is
/// checked against the catalog from both directions, and the sentinel is
/// driven from these rows rather than from a hand list that could quietly
/// stop covering a tool.
pub const REGISTRY: &[(&str, LoopComparability)] = &[
    (
        "bash",
        LoopComparability::ExemptWorldState {
            rationale: "the output is whatever the command printed, and the command is the \
                        model's — a build log, a test run, a `date`. Nothing about it is this \
                        crate's to make stable, and the detector's other rungs are what cover \
                        a shell loop.",
        },
    ),
    (
        "read_file",
        LoopComparability::VolatileWithNormalizer {
            normalizer: "stella_core::driver::loop_evidence::comparable_output",
            pinned_by: "crate::read::tests::the_footer_a_read_writes_is_the_one_loop_comparison_\
                        strips",
        },
    ),
    ("write_file", LoopComparability::Deterministic),
    ("edit_file", LoopComparability::Deterministic),
    ("delete_file", LoopComparability::Deterministic),
    ("search", LoopComparability::Deterministic),
    (
        "task_create",
        LoopComparability::ExemptWorldState {
            rationale: TASK_BOARD_RATIONALE,
        },
    ),
    (
        "task_list",
        LoopComparability::ExemptWorldState {
            rationale: TASK_BOARD_RATIONALE,
        },
    ),
    (
        "task_start",
        LoopComparability::ExemptWorldState {
            rationale: TASK_BOARD_RATIONALE,
        },
    ),
    (
        "task_complete",
        LoopComparability::ExemptWorldState {
            rationale: TASK_BOARD_RATIONALE,
        },
    ),
    (
        "task_cancel",
        LoopComparability::ExemptWorldState {
            rationale: TASK_BOARD_RATIONALE,
        },
    ),
    (
        "task_assign",
        LoopComparability::ExemptWorldState {
            rationale: TASK_BOARD_RATIONALE,
        },
    ),
    (
        "delegate",
        LoopComparability::ExemptWorldState {
            rationale: "the output is a sub-agent's answer, produced by a model call. Two \
                        identical delegations differ for the same reason two identical prompts \
                        do, and no normalizer can make them comparable.",
        },
    ),
    ("save_state", LoopComparability::Deterministic),
    ("get_state", LoopComparability::Deterministic),
    ("list_state", LoopComparability::Deterministic),
    ("delete_state", LoopComparability::Deterministic),
    ("get_environment", LoopComparability::Deterministic),
    (
        "ask_question",
        LoopComparability::ExemptWorldState {
            rationale: "the output is a human's answer. Two identical questions differ because \
                        the person answering is not obliged to repeat themselves, and a session \
                        that asks the same question twice is a loop the *broker* bounds, not \
                        the byte comparison.",
        },
    ),
];

/// Shared by the six board tools: one sentence, one place to correct it.
const TASK_BOARD_RATIONALE: &str = "the output is the session task board, which is append-only within a session — a second \
     identical `task_create` describes a board with one more row on it. Restoring the board \
     between two calls is not something this crate exposes, and a session that could would no \
     longer be the session the detector compares across.";

/// How `name`'s output relates to loop comparison, or `None` for a name no
/// built-in claims.
///
/// A linear scan over a table of nineteen `&'static str`s: this answers a
/// review question and a test, never a hot path.
pub fn for_tool(name: &str) -> Option<LoopComparability> {
    REGISTRY
        .iter()
        .find(|(declared, _)| *declared == name)
        .map(|(_, posture)| *posture)
}

#[cfg(test)]
mod tests;
