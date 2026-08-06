// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a killed turn was running *inside*, so a resume cannot silently give
//! it back as something smaller (#1615).
//!
//! `stella run` drives the staged pipeline — triage, plan, witness, execute,
//! verify, verdict — and the engine checkpoint that `stella daemon resume`
//! restores describes only the innermost of those: one worker turn. Resuming
//! from it alone therefore hands the operator a run that answers but was never
//! verified, which is the one thing this repository refuses to call done. The
//! checkpoint cannot carry the difference itself: the engine must not learn
//! pipeline shapes (architecture invariant 1).
//!
//! So the pipeline declares a **frame** beside the checkpoint —
//! [`stella_store::work_journal::PIPELINE_BLOB`], the same commit, one store,
//! not two — naming the staging the turn belonged to. The resume path reads it
//! and reports, in [`ResumeFrame::advisory`], exactly which stages it is not
//! restoring.
//!
//! # Why this is a report and not yet a restoration
//!
//! Re-entering the pipeline at the execute stage needs more than the frame:
//! the candidate workspace the turn was writing into died with the process,
//! and `stella_pipeline::Pipeline` exposes no seam to run the post-execute
//! stages over an already-produced result (`run` is its only entry point).
//! Both are tracked separately. What ships here is the half that must never
//! wait for them: a resumed run **says** it came back smaller instead of
//! presenting an unverified answer as a finished one.
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

/// The staged pipeline a checkpointed turn was running inside.
///
/// Deliberately made of *decisions*, not content: which stages the run had
/// configured, never the goal text, the plan, or a diff. The transcript in the
/// checkpoint beside it already carries all of that, and duplicating it would
/// make the frame a second, divergent copy of the same facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipelineFrame {
    /// See [`FRAME_VERSION`].
    pub version: u32,
    /// The deterministic verify ladder's command, when `--test-command` armed
    /// it. `None` means every verification would have escalated to the model
    /// verifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_command: Option<String>,
    /// Whether the witness stage would have had an independent model author a
    /// failing test up front — the flip oracle's whole input.
    #[serde(default)]
    pub witness_writer: bool,
    /// Candidates the run was fanning out over (1 for the ordinary path).
    #[serde(default)]
    pub candidates: u32,
    /// Whether the run's worktree policy allowed isolating execution in a
    /// candidate workspace.
    ///
    /// *Possible*, not *actual*: the decision is taken at triage, after the
    /// task class is known, and the frame is written before the first stage.
    /// Reporting the maybe is the honest reading and still the actionable one
    /// — if a candidate was created it died with the process, and the resumed
    /// turn writes into the workspace instead.
    #[serde(default)]
    pub isolation_possible: bool,
    /// Revision rounds the verifier could still have demanded.
    #[serde(default)]
    pub max_revisions: u32,
}

impl PipelineFrame {
    /// The frame for a run configured by `config`.
    pub fn of(config: &stella_pipeline::PipelineConfig) -> Self {
        Self {
            version: FRAME_VERSION,
            test_command: config.test_command.clone(),
            witness_writer: config.witness_writer,
            candidates: config.candidates.unwrap_or(1),
            isolation_possible: !matches!(
                config.create_worktrees,
                stella_pipeline::ports::WorktreePolicy::Never
            ),
            max_revisions: config.max_revisions,
        }
    }
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
    /// already assumed was false; naming the verdict, the flip credit and the
    /// candidate workspace individually is what makes it actionable.
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
            "this was a staged pipeline run — the resume continues its interrupted TURN, \
             not the pipeline around it"
                .to_string(),
        ];
        lines.push(match &frame.test_command {
            Some(command) => format!(
                "  the verify stage will NOT re-run `{command}`, and no verdict is recorded"
            ),
            None => {
                "  the verifier will NOT re-read this work, and no verdict is recorded".to_string()
            }
        });
        if frame.witness_writer {
            lines.push(
                "  the witness test's fail→pass flip is NOT credited — nothing proves this done"
                    .to_string(),
            );
        }
        if frame.max_revisions > 0 {
            lines.push(format!(
                "  up to {} revision round(s) the verifier could have demanded will NOT happen",
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

/// Declare that the turns from here on belong to a staged pipeline run, so
/// every checkpoint they write carries the frame describing it.
///
/// Best-effort and silent by the same contract as the rest of durability: a
/// run whose frame cannot be serialized is exactly as resumable as it was
/// before this existed, and refusing to start it would trade a working run for
/// none. Serialization of a struct of scalars cannot fail in practice — the
/// arm exists so this function has no `unwrap` on it.
pub fn declare(
    durability: &crate::durability::SessionDurability,
    config: &stella_pipeline::PipelineConfig,
) {
    if let Ok(json) = serde_json::to_string(&PipelineFrame::of(config)) {
        durability.set_pipeline_frame(json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipeline_frame() -> PipelineFrame {
        PipelineFrame {
            version: FRAME_VERSION,
            test_command: Some("cargo test -p stella-core".into()),
            witness_writer: true,
            candidates: 1,
            isolation_possible: true,
            max_revisions: 2,
        }
    }

    /// Invariant 4: the frame crosses a crate boundary as JSON and must come
    /// back the same value.
    #[test]
    fn a_frame_round_trips_through_json() {
        let frame = pipeline_frame();
        let json = serde_json::to_string(&frame).unwrap();
        assert_eq!(
            ResumeFrame::parse(&json),
            ResumeFrame::Pipeline(Box::new(frame))
        );
    }

    /// **The #1615 witness (report half).** A turn killed inside a staged
    /// pipeline resumes knowing it is coming back smaller: the advisory names
    /// the verify command that will not re-run, the witness flip that will not
    /// be credited, and the candidate worktree that died — and the audit row
    /// is labelled `resumed_complete_unverified` rather than borrowing the
    /// ordinary success label.
    ///
    /// On `main` there is no frame at all: `SessionDurability` has no
    /// `pipeline_frame`, the work journal has no `PIPELINE_BLOB`, and
    /// `run_resume` labels every completed resume `resumed_complete`. The
    /// fail-half is therefore type-level — the API this asserts on does not
    /// exist — which is why the sibling assertion below pins the *unchanged*
    /// bare-turn behaviour in the same test: the delta, not just the addition,
    /// is what is being witnessed.
    #[test]
    fn a_resumed_pipeline_run_reports_every_stage_it_cannot_restore() {
        let frame = ResumeFrame::Pipeline(Box::new(pipeline_frame()));
        assert!(frame.degrades());
        assert_eq!(frame.completed_label(), "resumed_complete_unverified");
        assert!(frame.completed_banner(7).contains("UNVERIFIED"));

        let advisory = frame.advisory().expect("a degraded resume must say so");
        let text = advisory.join("\n");
        assert!(
            text.contains("cargo test -p stella-core"),
            "the verify command that will not re-run is named: {text}"
        );
        assert!(text.contains("no verdict is recorded"), "{text}");
        assert!(text.contains("flip is NOT credited"), "{text}");
        assert!(text.contains("revision round"), "{text}");
        assert!(text.contains("candidate worktree"), "{text}");

        // The delta: a plain engine turn is untouched by any of this.
        let bare = ResumeFrame::BareTurn;
        assert!(!bare.degrades());
        assert_eq!(bare.advisory(), None);
        assert_eq!(bare.completed_label(), "resumed_complete");
        assert!(!bare.completed_banner(7).contains("UNVERIFIED"));
    }

    /// A frame present but unreadable must never read as "no frame": the two
    /// are opposite in consequence, and guessing the safe-looking one is the
    /// silent degradation this module exists to end.
    #[test]
    fn an_unreadable_frame_still_warns() {
        for json in ["{ not json", r#"{"version":9999}"#] {
            let frame = ResumeFrame::parse(json);
            assert!(
                matches!(frame, ResumeFrame::Unreadable(_)),
                "{json} must not be mistaken for a bare turn"
            );
            assert!(frame.degrades());
            assert_eq!(frame.completed_label(), "resumed_complete_unverified");
            let text = frame.advisory().expect("unreadable still warns").join("\n");
            assert!(text.contains("NOT verified"), "{text}");
        }
    }

    /// An older frame stays readable: additive fields default rather than
    /// failing the parse, so a resume never refuses a run it could report on.
    #[test]
    fn a_minimal_frame_from_an_older_writer_still_parses() {
        let ResumeFrame::Pipeline(frame) = ResumeFrame::parse(r#"{"version":1}"#) else {
            panic!("a version-1 frame must parse");
        };
        assert_eq!(frame.test_command, None);
        assert!(!frame.witness_writer);
        assert_eq!(frame.candidates, 0);
    }
}
