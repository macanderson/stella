// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a killed classic-pipeline turn was running *inside*, so a resume
//! never presents an unverified answer as a finished one (#1615).
//!
//! `stella run --pipeline classic` used to drive a staged pipeline — triage,
//! plan, execute, witness, verify — and declared a **frame** beside the
//! ordinary engine checkpoint, in the same work-journal commit
//! ([`stella_store::work_journal::PIPELINE_BLOB`]), naming the staging the
//! killed turn belonged to. The staged pipeline itself has been removed from
//! this build (#3846: `crates/stella-pipeline` is gone workspace-wide), so
//! there is no more restoration path — but an operator's disk may still hold
//! a frame an *older* build wrote, and `stella daemon resume` must keep
//! reading it rather than crash on it.
//!
//! # Detect, never reconstruct
//!
//! [`PipelineFrame`] therefore keeps only the plain-scalar fields
//! (`test_command`, `witness_writer`, `candidates`, `isolation_possible`,
//! `max_revisions`) that this module's own advisory needs, and reads
//! `responsibilities`/`progress` — the two fields that used to decode into
//! `stella_pipeline` types — as opaque JSON. A historical frame still
//! deserializes byte-for-byte (nothing about its on-disk shape changed), but
//! nothing in this build re-hydrates those two fields into a roster or a
//! resumable pipeline state, because the code that could act on them is gone.
//! [`ResumeFrame::advisory`] reports the graceful refusal: the resume always
//! takes the bare-turn path now, and says so.
//!
//! # Fail loud, not open
//!
//! A frame this build cannot parse is [`ResumeFrame::Unreadable`], not
//! [`ResumeFrame::BareTurn`]. The two are indistinguishable to a naive reader
//! and opposite in consequence: guessing "bare turn" on an unreadable frame is
//! precisely the silent degradation this module exists to end.

use serde::{Deserialize, Serialize};

/// The frame format's version, bumped when a field's *meaning* changes.
///
/// Additive fields do not bump it — they carry `#[serde(default)]` and an
/// older frame simply reads them as absent. A frame numbered above this is
/// refused rather than half-understood, the same posture
/// `stella_core::step::Checkpoint` takes.
pub const FRAME_VERSION: u32 = 1;

/// The staged pipeline a checkpointed turn was running inside, as far as this
/// build can still say.
///
/// **Detection-only, since #3846.** `responsibilities` and `progress` used to
/// decode into `stella_pipeline::AssignmentOverride`/`FrameProgress` — real
/// types owned by the crate that has been deleted. They are read here as
/// opaque JSON instead: a historical frame's bytes still parse (nothing about
/// the on-disk shape changed, so `#[serde(default)]` never has to fire on
/// these two), but nothing reconstructs a roster or a resumable pipeline
/// state from them any more, because the code that could act on either is
/// gone. The five plain-scalar fields are unaffected — they were always just
/// data, never `stella_pipeline` types — and are what
/// [`ResumeFrame::advisory`] still reports from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineFrame {
    /// See [`FRAME_VERSION`].
    pub version: u32,
    /// The deterministic verify ladder's command, when `--test-command` armed
    /// it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_command: Option<String>,
    /// Whether the witness stage would have had an independent model author a
    /// failing test up front — the flip oracle's whole input.
    #[serde(default)]
    pub witness_writer: bool,
    /// The run's responsibility-roster override block, exactly as it arrived
    /// on disk. Opaque JSON since #3846 (see the struct doc) — kept so a
    /// historical frame still round-trips its bytes, not because anything
    /// here decodes it.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub responsibilities: serde_json::Map<String, serde_json::Value>,
    /// Candidates the run was fanning out over (1 for the ordinary path).
    #[serde(default)]
    pub candidates: u32,
    /// Whether the run's worktree policy allowed isolating execution in a
    /// candidate workspace.
    #[serde(default)]
    pub isolation_possible: bool,
    /// Revision rounds the deterministic verify ladder could still have
    /// demanded.
    #[serde(default)]
    pub max_revisions: u32,
    /// The facts the pipeline learned while running — task class, plan,
    /// execute cursor, test baseline — exactly as they arrived on disk.
    /// Opaque JSON since #3846 (see the struct doc): its presence still means
    /// "this frame recorded enough to have restored the pipeline in an
    /// earlier build" (reported by [`ResumeFrame::advisory`]), but nothing
    /// decodes the record any more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<serde_json::Value>,
    /// The `[wrapper]` variant the run was launched with (#3408), `None` for
    /// the built-in `classic` order. `stella_plugin::Wrapper` is a genuinely
    /// independent type (never part of `stella-pipeline`), so it keeps
    /// decoding structurally rather than as opaque JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<stella_plugin::Wrapper>,
}

/// What the run being resumed was, as far as the durable record can say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeFrame {
    /// No frame beside the checkpoint: the killed turn was a plain engine
    /// turn, and resuming it as one loses nothing.
    BareTurn,
    /// The killed turn belonged to a staged pipeline run.
    Pipeline(Box<PipelineFrame>),
    /// A frame is present and this build cannot read it. Carries the reason,
    /// which is reported rather than swallowed — see the module docs.
    Unreadable(String),
}

impl ResumeFrame {
    /// Read the frame the interrupted turn left beside its checkpoint.
    pub fn read(durability: &crate::durability::SessionDurability) -> Self {
        match durability.pipeline_frame() {
            None => Self::BareTurn,
            Some(json) => Self::parse(&json),
        }
    }

    /// [`Self::read`] over the raw blob, split out so the classification is
    /// testable without a git-backed record behind it.
    pub fn parse(json: &str) -> Self {
        match serde_json::from_str::<PipelineFrame>(json) {
            Ok(frame) if frame.version <= FRAME_VERSION => Self::Pipeline(Box::new(frame)),
            Ok(frame) => Self::Unreadable(format!(
                "it is version {} and this build reads up to {FRAME_VERSION}",
                frame.version
            )),
            Err(e) => Self::Unreadable(e.to_string()),
        }
    }

    /// Whether a resume from this frame gives the operator back less than the
    /// run they lost — the one bit every caller downstream keys off.
    pub fn degrades(&self) -> bool {
        !matches!(self, Self::BareTurn)
    }

    /// The lines to print before the resumed turn's first step, or `None` when
    /// nothing is being lost.
    ///
    /// Written as a list of what will *not* run rather than a single sentence,
    /// because "the pipeline does not resume" is the summary an operator
    /// already assumed was false; naming the verify stage, the flip credit and
    /// the candidate workspace individually is what makes it actionable.
    ///
    /// **The graceful refusal (#3846).** A [`Self::Pipeline`] frame used to
    /// branch here — restore into the staged pipeline when its `progress`
    /// field carried enough, fall back to the bare turn otherwise. The staged
    /// pipeline is gone from this build, so every `Self::Pipeline` frame now
    /// takes the bare-turn path unconditionally; this only reports what that
    /// costs, never attempts to restore anything.
    pub fn advisory(&self) -> Option<Vec<String>> {
        let frame = match self {
            Self::BareTurn => return None,
            Self::Unreadable(why) => {
                return Some(vec![
                    "this run left a staged-pipeline frame this build cannot read".to_string(),
                    format!("  ({why})"),
                    "  treating it as a pipeline run: the resumed turn is NOT verified".to_string(),
                ]);
            }
            Self::Pipeline(frame) => frame,
        };
        let mut lines = vec![
            "this was a staged pipeline run — the staged pipeline has been removed from this \
             build, so the resume continues only its interrupted TURN, never the pipeline \
             around it"
                .to_string(),
        ];
        lines.push(match &frame.test_command {
            Some(command) => format!(
                "  the verify stage will NOT re-run `{command}`, so no evidence is recorded for \
                 this work"
            ),
            None => "  the verify stage will NOT observe this work, so no evidence is recorded \
                     for it"
                .to_string(),
        });
        // `witness_writer` is the one ablation flag every historical frame can
        // still answer without decoding `responsibilities` (now opaque JSON,
        // see the struct doc) — and in practice it was also the only row
        // `assignments()` ever surfaced here: the model verdict call was
        // already gone (#2584) before this pipeline was, so no live frame
        // ever carried an enabled `Verdict` row to report.
        if frame.witness_writer {
            lines.push(
                "  the witness test's fail→pass flip is NOT credited — nothing proves this done"
                    .to_string(),
            );
        }
        if frame.max_revisions > 0 {
            lines.push(format!(
                "  up to {} revision round(s) the verify ladder could have demanded will NOT happen",
                frame.max_revisions
            ));
        }
        if frame.candidates > 1 {
            lines.push(format!(
                "  {} candidates were in flight; only this one's turn comes back",
                frame.candidates
            ));
        }
        if frame.isolation_possible {
            lines.push(
                "  execution MAY have been isolated in a candidate worktree, which died with \
                 the process — the resumed turn writes into the workspace, guarded only by \
                 the restored staleness map"
                    .to_string(),
            );
        }
        if frame.progress.is_some() {
            lines.push(
                "  this frame recorded enough progress to have restored the pipeline in an \
                 earlier build — that restoration path no longer exists, so it is not attempted"
                    .to_string(),
            );
        }
        lines.push("  re-verify it with `stella run` before treating this as done".to_string());
        Some(lines)
    }

    /// The `executions` row's outcome label for a resumed turn that completed.
    ///
    /// A degraded resume gets its own label rather than borrowing the ordinary
    /// one: the audit trail is the only place the difference survives the
    /// terminal scrollback, and a stats query that cannot separate a verified
    /// run from an unverified one reports the wrong number forever.
    pub fn completed_label(&self) -> &'static str {
        if self.degrades() {
            "resumed_complete_unverified"
        } else {
            "resumed_complete"
        }
    }

    /// The one-line terminal banner a completed resume prints.
    pub fn completed_banner(&self, step: usize) -> String {
        if self.degrades() {
            format!("resumed turn completed UNVERIFIED (from step {step}) — its pipeline did not")
        } else {
            format!("resumed turn completed (from step {step})")
        }
    }
}

