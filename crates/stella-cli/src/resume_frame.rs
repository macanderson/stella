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
//! this build (#3865: `crates/stella-pipeline` is gone workspace-wide), so
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
/// **Detection-only, since #3865.** `responsibilities` and `progress` used to
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
    /// on disk. Opaque JSON since #3865 (see the struct doc) — kept so a
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
    /// Opaque JSON since #3865 (see the struct doc): its presence still means
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
    /// **The graceful refusal (#3865).** A [`Self::Pipeline`] frame used to
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pipeline_frame() -> PipelineFrame {
        PipelineFrame {
            version: FRAME_VERSION,
            test_command: Some("cargo test -p stella-core".into()),
            witness_writer: true,
            responsibilities: serde_json::Map::new(),
            candidates: 1,
            isolation_possible: true,
            max_revisions: 2,
            progress: None,
            variant: None,
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

    /// **The #1615 witness, still true post-#3865.** A turn killed inside a
    /// staged pipeline resumes knowing it is coming back smaller: the
    /// advisory names the verify command that will not re-run, the witness
    /// flip that will not be credited, and the candidate worktree that died
    /// — and the audit row is labelled `resumed_complete_unverified` rather
    /// than borrowing the ordinary success label. A plain engine turn is
    /// untouched by any of this — the delta, not just the addition, is what
    /// is being witnessed.
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
        assert!(text.contains("no evidence is recorded"), "{text}");
        assert!(text.contains("revision round"), "{text}");
        assert!(text.contains("candidate worktree"), "{text}");
        assert!(
            text.contains("fail→pass flip is NOT credited"),
            "witness_writer: true must still say so: {text}"
        );

        let bare = ResumeFrame::BareTurn;
        assert!(!bare.degrades());
        assert_eq!(bare.advisory(), None);
        assert_eq!(bare.completed_label(), "resumed_complete");
        assert!(!bare.completed_banner(7).contains("UNVERIFIED"));
    }

    /// A run with no witness author is not told its (nonexistent) flip went
    /// uncredited — the one ablation `witness_writer` can still express
    /// without decoding the now-opaque `responsibilities` block.
    #[test]
    fn an_ablated_witness_author_drops_its_own_sentence_and_nothing_else() {
        let frame = ResumeFrame::Pipeline(Box::new(PipelineFrame {
            witness_writer: false,
            ..pipeline_frame()
        }));
        let text = frame
            .advisory()
            .expect("a pipeline frame advises")
            .join("\n");
        assert!(
            !text.contains("fail→pass flip is NOT credited"),
            "a run that authored no witness must not be told its flip went uncredited: {text}"
        );
        assert!(
            text.contains("revision round"),
            "and nothing else may vanish with it: {text}"
        );
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
        assert_eq!(
            frame.progress, None,
            "no FRAME_VERSION bump for the progress addition (#1671): an old \
             frame reads as progress-unknown"
        );
    }

    /// **The graceful refusal (#3865), and this slice's own witness.** A
    /// frame a pre-#3865 build wrote — carrying real `responsibilities` and
    /// `progress` JSON, the exact shape that used to restore the pipeline via
    /// `stella_pipeline::Pipeline::resume` — still deserializes cleanly in
    /// this build (it does not panic, and does not misclassify as a bare
    /// turn), and its advisory says plainly that the restoration path this
    /// frame once qualified for no longer exists.
    ///
    /// On a build that still had `crates/stella-pipeline`, `responsibilities`
    /// decoded into `stella_pipeline::AssignmentOverride` and `progress` into
    /// `stella_pipeline::FrameProgress` — both types are gone now, so this
    /// data can only ever be read back as opaque JSON, never reconstructed.
    #[test]
    fn a_stored_classic_frame_with_restorable_progress_deserializes_into_the_graceful_refusal() {
        // A `responsibilities`/`progress` shape a real pre-#3865 build could
        // have written — this build does not need to know their internal
        // structure to accept them as opaque JSON.
        let json = serde_json::json!({
            "version": 1,
            "test_command": "cargo test -p widget",
            "witness_writer": true,
            "responsibilities": {
                "witness_author": { "enabled": false }
            },
            "candidates": 1,
            "isolation_possible": false,
            "max_revisions": 2,
            "progress": {
                "task_class": "single_task",
                "goal": "fix the parser",
                "executing": true
            }
        })
        .to_string();

        let frame = ResumeFrame::parse(&json);
        let ResumeFrame::Pipeline(pipeline_frame) = &frame else {
            panic!("a well-formed classic frame must classify as Pipeline, not {frame:?}");
        };
        assert!(
            pipeline_frame.progress.is_some(),
            "the progress block survives as opaque JSON rather than being dropped"
        );
        assert!(
            !pipeline_frame.responsibilities.is_empty(),
            "the responsibilities block survives as opaque JSON rather than being dropped"
        );

        // No restoration function exists any more to call on this frame —
        // that is the refusal. What remains is that the resume reports it
        // honestly rather than silently degrading or panicking.
        assert!(frame.degrades());
        let text = frame
            .advisory()
            .expect("a pipeline frame advises")
            .join("\n");
        assert!(
            text.contains("no longer exists"),
            "the advisory must name the removed restoration path explicitly: {text}"
        );
        assert!(
            text.contains("recorded enough progress"),
            "the advisory must say this specific frame once qualified for it: {text}"
        );
    }

    /// **#3408 P2.** A run launched under a non-default `[wrapper]` variant
    /// still comes back naming that variant — `PipelineFrame::variant` is a
    /// genuinely independent field (never a `stella_pipeline` type, per its
    /// own doc comment), so it round-trips structurally like any other.
    #[test]
    fn a_frames_variant_round_trips_structurally() {
        let variant = stella_plugin::Wrapper {
            id: "lean-diff-v1".into(),
            stages: vec![
                stella_plugin::WrapperStage {
                    name: stella_plugin::StageName::Host(stella_plugin::HostStage::Triage),
                    condition: None,
                },
                stella_plugin::WrapperStage {
                    name: stella_plugin::StageName::Host(stella_plugin::HostStage::Execute),
                    condition: None,
                },
                stella_plugin::WrapperStage {
                    name: stella_plugin::StageName::Host(stella_plugin::HostStage::Verify),
                    condition: Some("diff-lines > 0".into()),
                },
            ],
        };
        let frame = PipelineFrame {
            variant: Some(variant.clone()),
            ..pipeline_frame()
        };

        let json = serde_json::to_string(&frame).expect("the frame writes");
        let ResumeFrame::Pipeline(parsed) = ResumeFrame::parse(&json) else {
            panic!("the frame this test wrote must read back: {json}");
        };
        assert_eq!(parsed.variant, Some(variant));
    }

    /// The overwhelmingly common case — no configured variant — adds nothing
    /// to the frame.
    #[test]
    fn a_frame_with_no_variant_writes_no_variant_key() {
        let frame = PipelineFrame {
            variant: None,
            ..pipeline_frame()
        };
        let json = serde_json::to_string(&frame).expect("the frame writes");
        assert!(
            !json.contains("variant"),
            "the absent variant is skipped rather than written as `null`: {json}"
        );
    }
}
