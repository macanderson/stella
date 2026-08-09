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
//! # Report first, restoration where the record permits
//!
//! The report half shipped first (#1615): a resumed run **says** it came
//! back smaller instead of presenting an unverified answer as a finished
//! one. The restoration half (#1671) rides the same frame: the pipeline
//! pushes its mid-run progress — class, goal, plan cursor, test baseline —
//! through [`stella_pipeline::ResumeFrameSink`] into
//! [`PipelineFrame::progress`], and a resume whose frame carries enough
//! re-enters the pipeline via [`stella_pipeline::Pipeline::resume`] instead
//! of degrading. The advisory shrinks to what genuinely remains
//! unrestorable ([`restored_advisory`]); a frame that predates the progress
//! record, or a run that executed in a candidate worktree (it died with the
//! process), keeps the full report and the bare-turn path.
//!
//! # Fail loud, not open
//!
//! A frame this build cannot parse is [`ResumeFrame::Unreadable`], not
//! [`ResumeFrame::BareTurn`]. The two are indistinguishable to a naive reader
//! and opposite in consequence: guessing "bare turn" on an unreadable frame is
//! precisely the silent degradation this module exists to end.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use stella_protocol::ModelCallRole;

/// The frame format's version, bumped when a field's *meaning* changes.
///
/// Additive fields do not bump it — they carry `#[serde(default)]` and an
/// older frame simply reads them as absent. A frame numbered above this is
/// refused rather than half-understood, the same posture
/// `stella_core::step::Checkpoint` takes.
pub const FRAME_VERSION: u32 = 1;

/// The staged pipeline a checkpointed turn was running inside.
///
/// The configuration half is *decisions*, not content: which stages the run
/// had configured. [`Self::progress`] is the deliberate exception (#1671) —
/// restoration needs the goal, the plan and the test baseline, which exist
/// nowhere the resume can re-derive them (the plan's unreached steps are not
/// in the transcript; the baseline observed a tree that no longer exists).
/// The frame lives beside the transcript in `.stella/private/`, so the
/// exception widens no exposure.
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
    ///
    /// **Derived, never authoritative** (#2458): written from the
    /// `witness_author` row of [`Self::responsibilities`], and read back only
    /// by [`PipelineFrame::roster`] when that field is absent, which is how a
    /// frame written before the roster rode along is upgraded. Kept rather
    /// than dropped so a build predating `responsibilities` still prints the
    /// witness line in its advisory instead of silently losing it.
    #[serde(default)]
    pub witness_writer: bool,
    /// The run's responsibility roster (#2381), as the override block that
    /// reproduces it from `Roster::default` — the exact shape
    /// [`stella_pipeline::Roster::apply`] reads, so the resume path and the
    /// settings path decode a roster through one function rather than two.
    ///
    /// The **whole** roster and not just the witness enablement, because every
    /// ablation has to survive a resume: before this, a run whose triage was
    /// ablated and whose verdict was reassigned came back as neither, and the
    /// resumed leg's transcript described a pipeline that had never run.
    ///
    /// Additive, so no [`FRAME_VERSION`] bump — an older frame reads it as an
    /// empty map and is upgraded from [`Self::witness_writer`] alone, which is
    /// the only ablation such a frame could express. A default roster
    /// serializes to nothing at all, so the overwhelmingly common frame does
    /// not grow.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub responsibilities: BTreeMap<String, stella_pipeline::AssignmentOverride>,
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
    /// The facts the pipeline learned while running — task class, plan,
    /// execute cursor, test baseline — pushed by the pipeline through
    /// [`stella_pipeline::ResumeFrameSink`] as each settles (#1671). `None`
    /// on a frame from before the progress record existed, or on a run
    /// killed before triage; either way the resume declines to restore and
    /// keeps the honest bare-turn path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<stella_pipeline::FrameProgress>,
}

impl PipelineFrame {
    /// The frame for a run configured by `config`.
    pub fn of(config: &stella_pipeline::PipelineConfig) -> Self {
        Self {
            version: FRAME_VERSION,
            test_command: config.test_command.clone(),
            // Both from the roster, which since #2458 is the only place the
            // answer lives — the bool is a projection of the map beside it and
            // cannot disagree with it.
            witness_writer: config.roster.enabled(ModelCallRole::WitnessAuthor),
            responsibilities: config.roster.overrides(),
            candidates: config.candidates.unwrap_or(1),
            isolation_possible: !matches!(
                config.create_worktrees,
                stella_pipeline::ports::WorktreePolicy::Never
            ),
            max_revisions: config.max_revisions,
            progress: None,
        }
    }

    /// The roster the recorded run was launched with (#2458).
    ///
    /// Decoded through [`stella_pipeline::Roster::apply`] — the same function
    /// the settings path calls — so a stored roster and a configured one
    /// cannot drift into meaning different things, and a roster is total by
    /// construction here for the same reason it is there: it is built by
    /// applying a diff to [`stella_pipeline::Roster::default`], never by
    /// trusting a persisted table to still have a row for every
    /// responsibility this build knows about.
    ///
    /// Rejections from `apply` are dropped rather than reported. The frame was
    /// written from a roster that had already cleared `Pipeline::run`'s
    /// pre-spend refusal, so a rejection here means the frame was hand-edited
    /// — and the resumed leg re-validates before spending anyway
    /// (`Pipeline::resume`), which is where that belongs.
    #[must_use]
    pub fn roster(&self) -> stella_pipeline::Roster {
        let mut roster = stella_pipeline::Roster::default();
        if self.responsibilities.is_empty() {
            // Either a default-roster run — nothing to restore — or a frame
            // written before `responsibilities` existed, whose bool is the one
            // ablation it was able to express. `true` is the default, so only
            // `false` is worth acting on, and the two cases need no telling
            // apart: a modern default-roster frame derives `true`.
            if !self.witness_writer {
                roster.set_enabled(ModelCallRole::WitnessAuthor, false);
            }
            return roster;
        }
        let _ = roster.apply(self.responsibilities.clone());
        roster
    }
}

/// The write side of [`PipelineFrame::progress`]: carries the pipeline's
/// progress facts into the frame that rides every checkpoint commit.
///
/// Holds the frame's configuration half and re-serializes the whole frame on
/// every push — the frame slot in [`crate::durability`] is one JSON value,
/// and two writers patching halves of it is how the halves drift.
struct ProgressSink {
    durability: crate::durability::SessionDurability,
    base: PipelineFrame,
}

impl stella_pipeline::ResumeFrameSink for ProgressSink {
    fn record(&self, progress: &stella_pipeline::FrameProgress) {
        let mut frame = self.base.clone();
        frame.progress = Some(progress.clone());
        if let Ok(json) = serde_json::to_string(&frame) {
            self.durability.set_pipeline_frame(json);
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
        // Through the roster, not the bool beside it, so this file reads the
        // legacy field in exactly one place (#2458) — `PipelineFrame::roster`,
        // which is also the only place that knows an old frame carries nothing
        // else.
        if frame.roster().enabled(ModelCallRole::WitnessAuthor) {
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

/// What a resume can restore from `frame`, or `None` for the bare-turn path
/// (#1671): a validated [`stella_pipeline::PipelineResume`] plus the frame's
/// configuration half, which re-arms the pipeline with the run's *original*
/// decisions rather than whatever flags this process happened to start with.
///
/// Pure — the one decision `run_resume` takes is testable without a session
/// behind it. Validation itself lives in `PipelineResume::from_progress`, so
/// "restore approximately" is not a state either layer can reach.
pub fn restoration(
    frame: &ResumeFrame,
    checkpoint: &stella_core::step::Checkpoint,
) -> Option<(stella_pipeline::PipelineResume, PipelineFrame)> {
    let ResumeFrame::Pipeline(pipeline_frame) = frame else {
        return None;
    };
    let progress = pipeline_frame.progress.clone()?;
    let spec = stella_pipeline::PipelineResume::from_progress(checkpoint.clone(), progress)?;
    Some((spec, (**pipeline_frame).clone()))
}

/// The lines to print when a resume IS re-entering the pipeline (#1671) —
/// the short successor to [`ResumeFrame::advisory`] for the restored path:
/// most of that list now comes back, and what remains unrestorable is named
/// so the operator is never told more was restored than was.
pub fn restored_advisory() -> Vec<String> {
    vec![
        "this was a staged pipeline run — resuming INTO it: the interrupted turn \
         continues, then the witness/verify/verdict stages run on the completed work"
            .to_string(),
        "  the pre-crash lint baseline is gone, so the lint-regression veto (#861) \
         sits out this run"
            .to_string(),
        "  an authored witness cannot be re-created after a crash — verification \
         proceeds on the unauthored ladder"
            .to_string(),
    ]
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

/// Build a [`stella_pipeline::Pipeline`] that has already declared its frame.
///
/// **The one construction path**, so a surface cannot come to hold a pipeline
/// whose checkpoints do not say what they are. The frame was previously
/// declared beside `Pipeline::new` at one of four call sites, and the three
/// that forgot left checkpoints that resume as plain engine turns with no
/// notice that their stages are gone (#1672).
///
/// Pairing the two here rather than asking each surface to remember is the
/// difference between a convention and a mechanism.
///
/// Callers still chain what is theirs. The deck and fleet workers add
/// `with_turn_gate` — #1214's seam, where the pipeline attaches the gate to
/// every engine it builds and parks its management calls behind it — because
/// the gate is per-surface while the frame is not.
pub fn pipeline<'a>(
    durability: &crate::durability::SessionDurability,
    ports: stella_pipeline::PipelinePorts<'a>,
    events: impl Into<stella_core::EventSender>,
    config: stella_pipeline::PipelineConfig,
) -> stella_pipeline::Pipeline<'a> {
    declare(durability, &config);
    // The progress sink rides the same construction path as the frame, and
    // for the same reason (#1672): a pipeline whose checkpoints carry no
    // progress cannot be resumed into, and per-surface wiring is how three
    // of four surfaces forgot the frame itself.
    let sink = ProgressSink {
        durability: durability.clone(),
        base: PipelineFrame::of(&config),
    };
    stella_pipeline::Pipeline::new(ports, events, config).with_frame_sink(std::sync::Arc::new(sink))
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
