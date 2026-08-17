// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The wrapper variant this pipeline runs — read from a manifest, not from
//! branches.
//!
//! Today the stage order lives as conditional branches inside
//! [`crate::pipeline`], a file on the god-file list and closed to growth, so
//! the design cannot absorb a second variant even when someone wants one
//! (`doc:turn-loop-wrappers` §5). This module is the reader that lets it: the
//! built-in order ships as [`CLASSIC_MANIFEST`] — a real `[wrapper]` manifest
//! under `variants/classic.toml` — and [`classic_program`] resolves it against
//! the facts a turn has, producing the ordered
//! [`StageProgram`] the run will follow.
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
//! # What is not yet bound
//!
//! Nothing here dispatches a stage. The four wrapper interception points are
//! #3380 and do not exist, and [`crate::pipeline`] still takes its own
//! branches — so a resolved program is an answer a host may consult and log,
//! not the schedule the pipeline is currently driven by. Binding the two is
//! the socket work (#3408, #3380); until then the manifest and the branches are
//! kept honest by `tests/variant_program.rs`, not by construction.

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

/// The host's half of the contract: every published signal, filled in from the
/// facts this turn actually has.
///
/// One place, deliberately. The manifest's conditions are only as trustworthy
/// as the values behind them, so "what does `plans` mean" is answered by
/// [`TaskClass::plans`](crate::triage::TaskClass::plans) here rather than by
/// each caller's idea of it.
#[must_use]
pub fn signals(
    assessment: &TaskAssessment,
    research_questions: usize,
    test_command: Option<&str>,
) -> SignalValues {
    SignalValues {
        test_command: test_command.is_some(),
        conversational: assessment.conversational,
        // Saturating rather than wrapping: a question count past `u64::MAX`
        // cannot exist, and every comparison in the grammar answers the same
        // for the ceiling as for the real number.
        questions: u64::try_from(research_questions).unwrap_or(u64::MAX),
        plans: assessment.class.plans(),
        verifies: assessment.class.verifies_unconditionally(),
    }
}
