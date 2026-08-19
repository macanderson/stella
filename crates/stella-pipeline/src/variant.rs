// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The wrapper variant this pipeline runs — read from a manifest, not from
//! branches.
//!
//! The built-in order ships as [`CLASSIC_MANIFEST`] — a real `[wrapper]`
//! manifest under `variants/classic.toml` — and [`classic_program`] resolves
//! it against the facts a turn has at the triage boundary, producing the
//! ordered [`StageProgram`] the run follows.
//!
//! [`StageProgram`]: stella_plugin::StageProgram
//!
//! # Where the boundary is
//!
//! `stella-plugin` declares, validates and resolves; this module is the
//! *host* half — it gathers the facts the manifest's conditions read and hands
//! them in as [`SignalValues`]. That split is invariant 1 applied to the
//! wrapper: `stella-core` never learns plugins exist, and the manifest crate
//! never learns what a `TaskAssessment` is. The one edge is this direction,
//! from the orchestration plane down to a near-leaf types-and-rules crate.
//!
//! # What is bound today
//!
//! [`crate::pipeline`] IS driven by this manifest (#3408): every stage
//! question the closed condition grammar can express — `research`'s
//! `"questions > 0"`, `plan`/`scope`'s `"plans"`, `witness`'s
//! `"no-test-command"`, `verify`'s `"verifies"` — is answered by
//! [`crate::schedule::Schedule`], not by a branch that merely happens to
//! agree with `classic.toml`. What stays in Rust is exactly what the
//! manifest's own header says it cannot name: the conversational fast path
//! (an early *return*, not a stage a program can skip), witness's extra
//! roster/independence conjuncts, and verify's zero-diff simple-lookup arm.
//! See [`crate::schedule`] for the mechanism and
//! `crates/stella-pipeline/tests/variant_dispatch.rs` for the witness that a
//! second variant actually takes a different path through the same code.
//!
//! [`classic_program`] and [`signals`] below remain the *one-shot*
//! triage-boundary reader — useful for a variant (like `classic`) whose every
//! condition is answered by then, and for logging a whole program before a
//! turn starts. A condition that reads what `execute`, `witness` or `verify`
//! publishes needs [`crate::schedule::Schedule`] instead, which resolves
//! progressively rather than from one fixed snapshot.

use std::sync::OnceLock;

use stella_plugin::{ManifestError, PluginManifest, SignalValues, StageProgram, Wrapper};

use crate::triage::TaskAssessment;

/// The built-in variant's id — what `executions.pipeline_variant` records for
/// a run of today's stage order (#3388), and the id a default or fallback path
/// writes rather than leaving the column blank.
pub const CLASSIC_VARIANT_ID: &str = "classic";

/// The shipped `classic` manifest, embedded at build time.
///
/// Public so a host can show, diff, or copy it as the starting point for a new
/// variant — "trying a different pipeline design becomes editing a file"
/// (`doc:turn-loop-wrappers` §5) only works if the file is reachable.
pub const CLASSIC_MANIFEST: &str = include_str!("../variants/classic.toml");

/// A built-in variant manifest could not be turned into a stage program.
///
/// Typed rather than a `String` because the two arms need different responses:
/// a [`VariantError::Load`] is a defect in a shipped file that no user action
/// can fix, while a [`VariantError::Resolve`] names a manifest a user may have
/// authored. Both are unreachable for the shipped `classic` variant — which is
/// asserted, not assumed, by this crate's `variant_program` tests.
#[derive(Debug, thiserror::Error)]
pub enum VariantError {
    /// The manifest did not load: malformed TOML, or a rule the plugin
    /// manifest enforces.
    #[error("the built-in \"{id}\" variant manifest failed to load: {source}")]
    Load {
        /// The variant whose manifest is broken.
        id: &'static str,
        /// Why the manifest was rejected.
        #[source]
        source: ManifestError,
    },
    /// The manifest loaded but declares no `[wrapper]` block, so there is no
    /// stage order in it at all.
    #[error("the built-in \"{id}\" variant manifest declares no [wrapper] block")]
    NoWrapper {
        /// The variant whose manifest declares no stages.
        id: &'static str,
    },
    /// Resolution failed against the supplied signal values. Unreachable for a
    /// manifest that came from the loader — the load-time graph rules make
    /// resolution total — so this arm exists for a hand-built wrapper.
    #[error("the \"{id}\" variant did not resolve into a stage order: {source}")]
    Resolve {
        /// The variant being resolved.
        id: &'static str,
        /// Why resolution stopped.
        #[source]
        source: ManifestError,
    },
}

/// The built-in `classic` wrapper, parsed once per process.
///
/// # Errors
///
/// [`VariantError::Load`] or [`VariantError::NoWrapper`] if the shipped
/// manifest is broken — a build-time defect, not a runtime condition, which is
/// why the cache holds only the success.
pub fn classic() -> Result<&'static Wrapper, VariantError> {
    static CLASSIC: OnceLock<Wrapper> = OnceLock::new();
    if let Some(wrapper) = CLASSIC.get() {
        return Ok(wrapper);
    }
    let manifest =
        PluginManifest::from_toml_str(CLASSIC_MANIFEST).map_err(|source| VariantError::Load {
            id: CLASSIC_VARIANT_ID,
            source,
        })?;
    let wrapper = manifest.wrapper.ok_or(VariantError::NoWrapper {
        id: CLASSIC_VARIANT_ID,
    })?;
    Ok(CLASSIC.get_or_init(|| wrapper))
}

/// The stages the built-in `classic` variant runs for a turn with these facts.
///
/// # Errors
///
/// Whatever [`classic`] returns, plus [`VariantError::Resolve`] — which the
/// load-time stage-graph rules make unreachable for this manifest.
pub fn classic_program(values: &SignalValues) -> Result<StageProgram, VariantError> {
    classic()?
        .resolve(values)
        .map_err(|source| VariantError::Resolve {
            id: CLASSIC_VARIANT_ID,
            source,
        })
}

/// The host's half of the contract **as of the triage boundary**: every
/// published signal, filled in from the facts this turn has by then.
///
/// One place, deliberately. The manifest's conditions are only as trustworthy
/// as the values behind them, so "what does `plans` mean" is answered by
/// [`TaskClass::plans`](crate::triage::TaskClass::plans) here rather than by
/// each caller's idea of it.
///
/// # What the later stages' signals read here
///
/// `stella-plugin` publishes signals from execute, witness and verify too, and
/// none of them has a value at this point in the run: nothing has executed,
/// nothing has been authored, nothing has been observed. They therefore carry
/// their *nothing-yet* value here, which is only sound for a **one-shot**
/// resolution ([`Wrapper::resolve`](stella_plugin::Wrapper::resolve) via
/// [`classic_program`]) whose every condition is answered by the triage
/// boundary — true of the shipped `classic` manifest, asserted rather than
/// assumed by `tests/variant_program.rs`. A variant whose condition reads
/// what `execute`, `witness` or `verify` actually produced needs
/// [`crate::schedule::Schedule`] instead: it resolves progressively, one
/// stage boundary at a time, so a post-execute condition reads the stage's
/// real output rather than this function's placeholder (#3408, #3491).
#[must_use]
pub fn signals(
    assessment: &TaskAssessment,
    research_questions: usize,
    test_command: Option<&str>,
    candidates: u32,
    budget_metered: bool,
) -> SignalValues {
    SignalValues {
        test_command: test_command.is_some(),
        candidates: u64::from(candidates),
        budget_metered,
        conversational: assessment.conversational,
        // Saturating rather than wrapping: a question count past `u64::MAX`
        // cannot exist, and every comparison in the grammar answers the same
        // for the ceiling as for the real number.
        questions: u64::try_from(research_questions).unwrap_or(u64::MAX),
        plans: assessment.class.plans(),
        verifies: assessment.class.verifies_unconditionally(),
        wants_witness: assessment.wants_witness(),
        wants_verifier: assessment.wants_verifier(),
        // Not yet observed — see this function's contract above. Spelled out
        // one field at a time rather than through a helper, so adding a signal
        // makes a human decide here whether this host can answer it.
        mutating_actions: 0,
        diff_lines: 0,
        witness_authored: false,
        flip_achieved: false,
        tests_red: false,
        tests_green: false,
    }
}
