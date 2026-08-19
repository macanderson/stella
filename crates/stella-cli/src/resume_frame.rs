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

#[cfg(test)]
mod tests {
    use super::*;

    fn pipeline_frame() -> PipelineFrame {
        PipelineFrame {
            version: FRAME_VERSION,
            test_command: Some("cargo test -p stella-core".into()),
            witness_writer: true,
            responsibilities: BTreeMap::new(),
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
        assert!(text.contains("no evidence is recorded"), "{text}");
        assert!(text.contains("revision round"), "{text}");
        assert!(text.contains("candidate worktree"), "{text}");

        // The model-call half is asserted as the *derived* set, not as fixed
        // strings (#2608): every responsibility this run's roster enables
        // contributes its sentence, and every one it does not is absent. A
        // literal here would re-create the drift the derivation removed.
        let roster = pipeline_frame().roster();
        for &responsibility in ModelCallRole::ALL {
            let Some(sentence) = unrestored_guarantee(responsibility) else {
                continue;
            };
            assert_eq!(
                text.contains(sentence),
                roster.enabled(responsibility),
                "the advisory must name {responsibility:?} exactly when the roster runs it: {text}"
            );
        }

        // The delta: a plain engine turn is untouched by any of this.
        let bare = ResumeFrame::BareTurn;
        assert!(!bare.degrades());
        assert_eq!(bare.advisory(), None);
        assert_eq!(bare.completed_label(), "resumed_complete");
        assert!(!bare.completed_banner(7).contains("UNVERIFIED"));
    }

    /// **The #2608 witness.** The advisory names only responsibilities the
    /// pipeline can actually issue, and it learns that from the roster rather
    /// than from the sentences it was written with.
    ///
    /// On `main` this fails on the first assertion: the advisory prints
    /// "and no verdict is recorded" unconditionally, while
    /// `Roster::is_assignable(Verdict)` has been `false` since verification
    /// went model-free — the two had drifted apart and nothing joined them.
    ///
    /// Both directions are checked, because a set that is derived has to be
    /// derived *both* ways: a responsibility the pipeline cannot issue must
    /// not be named even though [`unrestored_guarantee`] still declares its
    /// sentence, and one it can issue must lose its sentence the moment the
    /// run ablates it.
    #[test]
    fn the_advisory_names_no_responsibility_the_pipeline_cannot_issue() {
        for test_command in [Some("cargo test".to_string()), None] {
            let frame = ResumeFrame::Pipeline(Box::new(PipelineFrame {
                test_command,
                ..pipeline_frame()
            }));
            let text = frame
                .advisory()
                .expect("a pipeline frame advises")
                .join("\n");
            for &responsibility in ModelCallRole::ALL {
                if stella_pipeline::Roster::is_assignable(responsibility) {
                    continue;
                }
                let token = serde_json::to_value(responsibility)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .expect("a fieldless role serializes to its wire token");
                assert!(
                    !text.contains(&token),
                    "the advisory names `{token}`, which the staged pipeline does not issue — \
                     an operator is being told a check was skipped that no longer exists: {text}"
                );
            }
        }

        // The sentence for the responsibility above IS declared; the roster is
        // what keeps it off the page. Deleting the arm would make a removed
        // guarantee indistinguishable from a forgotten one.
        assert!(
            unrestored_guarantee(ModelCallRole::Verdict).is_some(),
            "the verdict's loss stays declared, so its return is one roster row away"
        );

        // The other direction: an ablation the run was launched with drops the
        // sentence for what it ablated, and nothing else.
        let witness_sentence =
            unrestored_guarantee(ModelCallRole::WitnessAuthor).expect("the witness author advises");
        let ablated = ResumeFrame::Pipeline(Box::new(PipelineFrame {
            responsibilities: stella_pipeline::Roster::default()
                .with_enabled(ModelCallRole::WitnessAuthor, false)
                .overrides(),
            witness_writer: false,
            ..pipeline_frame()
        }));
        let text = ablated
            .advisory()
            .expect("a pipeline frame advises")
            .join("\n");
        assert!(
            !text.contains(witness_sentence),
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
             frame reads as progress-unknown, which declines restoration"
        );
    }

    /// The checkpoint a restoration test rides — content is irrelevant to the
    /// decision under test.
    fn checkpoint() -> stella_core::step::Checkpoint {
        stella_core::step::Checkpoint {
            version: stella_core::step::CHECKPOINT_VERSION,
            step: 1,
            messages: vec![],
            budget: stella_core::step::BudgetSnapshot {
                mode: stella_protocol::BudgetMode::Off,
                turn_limit_usd: None,
                session_limit_usd: None,
                turn_spent_usd: 0.0,
                session_spent_usd: 0.0,
            },
            total_cost_usd: 0.0,
            calibration_model: None,
            loop_steered: false,
            loop_steered_pattern: Vec::new(),
            loop_steered_inputs: None,
            transcript_rewrites: 0,
            loop_steers_spent: 0,
        }
    }

    /// **The #1671 witness (decision half).** A frame carrying restorable
    /// progress restores — with the run's original configuration riding along
    /// — and every other frame keeps the bare-turn path. On `main` neither
    /// `PipelineFrame::progress` nor `restoration` exists: a mid-pipeline
    /// kill can only ever resume as a plain engine turn.
    #[test]
    fn a_frame_with_progress_restores_and_the_rest_stay_bare() {
        let mut frame = pipeline_frame();
        frame.progress = Some(stella_pipeline::FrameProgress {
            task_class: Some(stella_pipeline::triage::TaskClass::SingleTask),
            goal: Some("fix the parser".into()),
            executing: true,
            ..stella_pipeline::FrameProgress::default()
        });
        // Round-trip first: the progress record crosses through the stored
        // JSON blob, not through memory (invariant 4).
        let json = serde_json::to_string(&frame).unwrap();
        let parsed = ResumeFrame::parse(&json);

        let (spec, config) =
            restoration(&parsed, &checkpoint()).expect("restorable progress restores");
        assert_eq!(spec.goal, "fix the parser");
        assert_eq!(
            config.test_command.as_deref(),
            Some("cargo test -p stella-core"),
            "the run's ORIGINAL decisions ride along for the pipeline's re-arm"
        );

        // Everything else declines: no progress, a bare turn, an unreadable
        // frame — the honest report path is the fallback, never a guess.
        let plain = ResumeFrame::Pipeline(Box::new(pipeline_frame()));
        assert!(restoration(&plain, &checkpoint()).is_none());
        assert!(restoration(&ResumeFrame::BareTurn, &checkpoint()).is_none());
        assert!(restoration(&ResumeFrame::Unreadable("x".into()), &checkpoint()).is_none());
    }

    /// **Witness (#1672).** Every surface that builds a `Pipeline` declares
    /// its frame first.
    ///
    /// The frame was declared at exactly one of four call sites, so a
    /// checkpoint left by the deck, the goal loop or a fleet worker read as a
    /// plain engine turn and any resume from it degraded in silence — the
    /// failure #1615 closed for `stella run` alone.
    ///
    /// This greps the source rather than driving a turn, deliberately. What
    /// went wrong was **wiring**, not logic: every unit here already passed
    /// while three surfaces never called it. A behavioural test would need one
    /// scripted run per surface and would still only cover the surfaces
    /// somebody remembered to write a test for, which is the same gap one
    /// level up. The repo uses source-grep guards for exactly this shape (the
    /// `stella fullauto` wrapper guards from #1619).
    #[test]
    fn every_pipeline_construction_declares_its_resume_frame() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sites = Vec::new();
        let mut undeclared = Vec::new();

        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                // This guard's own source carries the needle in a string
                // literal, so it would report itself forever.
                if path.file_name().and_then(|n| n.to_str()) == Some("resume_frame.rs") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let lines: Vec<&str> = text.lines().collect();
                for (i, line) in lines.iter().enumerate() {
                    if !line.contains("Pipeline::new(") {
                        continue;
                    }
                    sites.push(format!("{}:{}", path.display(), i + 1));
                    undeclared.push(format!("{}:{}", path.display(), i + 1));
                }
            }
        }

        let _ = sites;
        assert!(
            undeclared.is_empty(),
            "these sites call `Pipeline::new` directly and so declare no resume frame — \
             a checkpoint they leave resumes as a plain engine turn with no notice that \
             its stages are gone. Build through `resume_frame::pipeline` instead: \
             {undeclared:?}"
        );
    }

    /// **The #2458 witness.** Every ablation the run was launched under
    /// survives a kill and a resume — not just witness authoring.
    ///
    /// Before this, the frame carried a lone `witness_writer` bool, so a
    /// resumed leg reconstructed one half of one decision and defaulted
    /// everything else: a run with triage ablated came back running triage,
    /// and its transcript described a pipeline that had never been asked for.
    /// Fails on the parent commit for the plainest reason — `PipelineFrame`
    /// has no field to put a roster in.
    #[test]
    fn a_resumed_run_gets_back_every_ablation_it_was_launched_with() {
        let mut roster = stella_pipeline::Roster::default();
        roster.set_enabled(ModelCallRole::Triage, false);
        roster.set_enabled(ModelCallRole::WitnessAuthor, false);
        roster.set_agent(
            ModelCallRole::Verdict,
            stella_pipeline::AgentId::new("triage"),
        );
        let config = stella_pipeline::PipelineConfig {
            roster: roster.clone(),
            ..stella_pipeline::PipelineConfig::default()
        };

        let json = serde_json::to_string(&PipelineFrame::of(&config)).expect("the frame writes");
        let ResumeFrame::Pipeline(frame) = ResumeFrame::parse(&json) else {
            panic!("the frame this build wrote must read back: {json}");
        };

        assert_eq!(
            frame.roster(),
            roster,
            "the resumed leg must run the pipeline the killed one was running"
        );
        assert!(
            !frame.witness_writer,
            "the legacy projection agrees with the row it is derived from"
        );
    }

    /// **#3408 P2 witness.** A run launched under a non-default `[wrapper]`
    /// variant comes back resuming under that SAME variant, not the built-in
    /// `classic` fallback `PipelineFrame::of` used to silently substitute
    /// (`PipelineConfig::variant` was not captured at all before this).
    #[test]
    fn a_resumed_run_gets_back_the_variant_it_was_launched_with() {
        let variant = stella_plugin::Wrapper {
            id: "lean-diff-v1".into(),
            stages: vec![
                stella_plugin::WrapperStage {
                    name: stella_plugin::StageName::Triage,
                    condition: None,
                },
                stella_plugin::WrapperStage {
                    name: stella_plugin::StageName::Execute,
                    condition: None,
                },
                stella_plugin::WrapperStage {
                    name: stella_plugin::StageName::Verify,
                    condition: Some("diff-lines > 0".into()),
                },
            ],
        };
        let config = stella_pipeline::PipelineConfig {
            variant: Some(variant.clone()),
            ..stella_pipeline::PipelineConfig::default()
        };

        let json = serde_json::to_string(&PipelineFrame::of(&config)).expect("the frame writes");
        let ResumeFrame::Pipeline(frame) = ResumeFrame::parse(&json) else {
            panic!("the frame this build wrote must read back: {json}");
        };

        assert_eq!(
            frame.variant,
            Some(variant),
            "the resumed leg must run the SAME manifest the killed one was running, not \
             silently fall back to classic"
        );
    }

    /// The overwhelmingly common case — no configured variant — adds nothing
    /// to the frame, matching `a_default_roster_run_writes_no_responsibilities_at_all`'s
    /// posture for the roster.
    #[test]
    fn a_classic_run_writes_no_variant_at_all() {
        let frame = PipelineFrame::of(&stella_pipeline::PipelineConfig::default());
        assert!(frame.variant.is_none());
        let json = serde_json::to_string(&frame).expect("the frame writes");
        assert!(
            !json.contains("variant"),
            "the built-in `classic` fallback is skipped rather than written out: {json}"
        );
    }

    /// A default-roster run — the overwhelming majority — adds nothing to the
    /// frame, so the persistence change costs the common path no bytes.
    #[test]
    fn a_default_roster_run_writes_no_responsibilities_at_all() {
        let frame = PipelineFrame::of(&stella_pipeline::PipelineConfig::default());
        assert!(frame.responsibilities.is_empty());
        let json = serde_json::to_string(&frame).expect("the frame writes");
        assert!(
            !json.contains("responsibilities"),
            "an empty block is skipped rather than written as `{{}}`: {json}"
        );
        assert_eq!(frame.roster(), stella_pipeline::Roster::default());
    }

    /// A frame written before the roster rode along still resumes, and its one
    /// expressible ablation is honoured rather than dropped.
    ///
    /// `responsibilities` is additive, so [`FRAME_VERSION`] does not move —
    /// the same posture the `progress` addition took (#1671). The bool is the
    /// only thing such a frame can say about the roster, and
    /// [`PipelineFrame::roster`] is the single place that knows it.
    #[test]
    fn an_older_frame_is_upgraded_from_its_witness_bool() {
        let ResumeFrame::Pipeline(off) =
            ResumeFrame::parse(r#"{"version":1,"witness_writer":false}"#)
        else {
            panic!("a version-1 frame must parse");
        };
        assert!(
            !off.roster().enabled(ModelCallRole::WitnessAuthor),
            "the one ablation an old frame could express must survive"
        );
        assert!(
            off.roster().enabled(ModelCallRole::Triage),
            "and nothing it could not express may be invented"
        );

        let ResumeFrame::Pipeline(on) =
            ResumeFrame::parse(r#"{"version":1,"witness_writer":true}"#)
        else {
            panic!("a version-1 frame must parse");
        };
        assert_eq!(on.roster(), stella_pipeline::Roster::default());
    }
}
