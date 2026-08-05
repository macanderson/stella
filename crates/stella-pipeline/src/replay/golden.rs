//! Golden trajectories: a recorded `AgentEvent` stream on a fixed task, the
//! manifest that says *what produced it*, and a loader that refuses anything
//! which is not a well-formed recording of **this** protocol.
//!
//! [`super`] supplies the comparison machinery (`validate_stream`,
//! `structural_diff`). This module supplies the thing that machinery is
//! pointed at: a fixture format with provenance, so a committed golden can
//! never be silently mistaken for something it isn't.
//!
//! # Why a manifest, and why it carries a count
//!
//! [`parse_jsonl`] deliberately tolerates a torn final line (L-T1) — the right
//! posture for a *live* reader recovering from a crashed writer, and exactly
//! the wrong posture for a *committed fixture*, where a truncated recording
//! would load one event short and quietly weaken every assertion made against
//! it. The manifest's `event_count` closes that hole: a recording that parses
//! to a different length than it was recorded at is a
//! [`GoldenError::Truncated`], not a slightly smaller golden.
//!
//! # Provenance is part of the fixture, not a comment
//!
//! [`RecordingSource`] distinguishes a recording made from this workspace's own
//! pipeline from one made through an independent reference engine. The two
//! answer different questions and must never be confused:
//!
//! - [`RecordingSource::RustStack`] is a **self-recorded baseline**. Replaying
//!   the Rust stack against it detects *protocol drift* — a stage that stopped
//!   being emitted, a tool that changed name, an event that moved. It is not
//!   independent evidence of correctness, because both sides are the same code.
//! - [`RecordingSource::Reference`] is a **reference trajectory** recorded from
//!   another engine through an adapter that emits this protocol. Only this one
//!   answers "does the Rust stack agree with the reference implementation?".
//!
//! Encoding the distinction in the fixture (rather than in prose) is what stops
//! a baseline from being cited as a reference later. See
//! `docs/spec/replay-golden-trajectories.md` for the recording procedure and for the
//! adapter contract a reference engine has to satisfy.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use stella_protocol::AgentEvent;

use super::{
    JsonlError, StreamDiff, StreamViolation, parse_jsonl, structural_diff, to_jsonl,
    validate_stream,
};

/// What produced a recording. Serialized as an internally-tagged enum so the
/// manifest reads as prose (`"kind": "rust_stack"`), and so adding a future
/// source is additive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordingSource {
    /// Recorded from this workspace's own `stella-pipeline`. A drift baseline,
    /// **not** independent evidence — see the module doc.
    RustStack {
        /// How the run was driven, so a refresh can reproduce it (e.g. the
        /// name of the test that records it, or the CLI invocation).
        recorded_by: String,
    },
    /// Recorded from an independent reference engine through an adapter that
    /// emits this protocol.
    Reference {
        /// The engine that ran the task.
        engine: String,
        /// The adapter that mapped that engine's native events onto
        /// `AgentEvent`. Named because the mapping is where the fidelity of a
        /// reference trajectory actually lives.
        adapter: String,
    },
}

impl RecordingSource {
    /// Whether this recording is independent evidence (a reference engine) as
    /// opposed to a self-recorded drift baseline.
    pub fn is_reference(&self) -> bool {
        matches!(self, RecordingSource::Reference { .. })
    }
}

/// The sidecar describing one golden trajectory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldenManifest {
    /// Stable id; also the fixture's file stem.
    pub task_id: String,
    /// What the fixed task is, in one line.
    pub description: String,
    /// What produced the recording.
    pub source: RecordingSource,
    /// How many events the recording held when it was written. Guards against
    /// a silently truncated fixture — see the module doc.
    pub event_count: usize,
}

impl GoldenManifest {
    /// Build a manifest for a freshly-recorded stream, deriving `event_count`
    /// from the recording so the two can never disagree at authoring time.
    pub fn for_recording(
        task_id: impl Into<String>,
        description: impl Into<String>,
        source: RecordingSource,
        events: &[AgentEvent],
    ) -> Self {
        Self {
            task_id: task_id.into(),
            description: description.into(),
            source,
            event_count: events.len(),
        }
    }
}

/// A recording that failed to load as a golden trajectory. Every variant names
/// the task, because a failure surfaces from a fixture directory where one
/// bad file among many is the usual case.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GoldenError {
    /// A fixture file could not be read.
    #[error("reading golden `{task_id}` from {path}: {message}")]
    Io {
        task_id: String,
        path: PathBuf,
        message: String,
    },
    /// The manifest sidecar was not valid `GoldenManifest` JSON.
    #[error("manifest for golden `{task_id}` is not valid: {message}")]
    Manifest { task_id: String, message: String },
    /// The manifest's `task_id` disagrees with the file it was loaded as — a
    /// copy-paste that would otherwise attribute a recording to the wrong task.
    #[error("golden `{task_id}` has a manifest claiming task `{declared}`")]
    TaskIdMismatch { task_id: String, declared: String },
    /// The recording is not parseable as an `AgentEvent` stream. This is the
    /// error a recording in *another* engine's wire format produces.
    #[error("recording for golden `{task_id}` is not an AgentEvent stream: {source}")]
    Recording {
        task_id: String,
        #[source]
        source: JsonlError,
    },
    /// The recording parsed to a different length than the manifest declares —
    /// a torn tail, a truncated copy, or a stale manifest.
    #[error(
        "golden `{task_id}` is truncated: the manifest declares {declared} events, \
         the recording parsed {parsed}"
    )]
    Truncated {
        task_id: String,
        declared: usize,
        parsed: usize,
    },
    /// The recording parsed but violates the protocol's structural invariants.
    /// A golden is the yardstick other runs are measured against, so it must
    /// itself be well-formed — an ill-formed one would license ill-formed runs.
    #[error("golden `{task_id}` violates the protocol's structural invariants: {violations:?}")]
    NotWellFormed {
        task_id: String,
        violations: Vec<StreamViolation>,
    },
}

/// A loaded golden trajectory: a well-formed `AgentEvent` recording plus its
/// provenance. Constructing one is the only way to get a golden, so every
/// golden in the process has already passed the gates in [`GoldenTrajectory::new`].
// `AgentEvent` is deliberately not `PartialEq` — structural equivalence
// ([`GoldenTrajectory::matches`]), not field equality, is what a golden is
// compared by — so this type isn't either.
#[derive(Debug, Clone)]
pub struct GoldenTrajectory {
    manifest: GoldenManifest,
    events: Vec<AgentEvent>,
}

impl GoldenTrajectory {
    /// Gate a manifest + recording into a golden. Rejects a truncated recording
    /// and one that violates the structural invariants; both would otherwise
    /// become a weaker yardstick rather than a loud failure.
    pub fn new(manifest: GoldenManifest, events: Vec<AgentEvent>) -> Result<Self, GoldenError> {
        if manifest.event_count != events.len() {
            return Err(GoldenError::Truncated {
                task_id: manifest.task_id,
                declared: manifest.event_count,
                parsed: events.len(),
            });
        }
        let violations = validate_stream(&events);
        if !violations.is_empty() {
            return Err(GoldenError::NotWellFormed {
                task_id: manifest.task_id,
                violations,
            });
        }
        Ok(Self { manifest, events })
    }

    /// Parse a golden from in-memory documents — the pure core of
    /// [`GoldenTrajectory::load`], so callers (and tests) can exercise every
    /// gate without touching the filesystem.
    ///
    /// `task_id` is the identity the caller is loading *as*; a manifest that
    /// declares a different one is a [`GoldenError::TaskIdMismatch`].
    pub fn parse(task_id: &str, manifest_json: &str, recording: &str) -> Result<Self, GoldenError> {
        let manifest: GoldenManifest =
            serde_json::from_str(manifest_json).map_err(|e| GoldenError::Manifest {
                task_id: task_id.to_string(),
                message: e.to_string(),
            })?;
        if manifest.task_id != task_id {
            return Err(GoldenError::TaskIdMismatch {
                task_id: task_id.to_string(),
                declared: manifest.task_id,
            });
        }
        let events = parse_jsonl(recording).map_err(|source| GoldenError::Recording {
            task_id: task_id.to_string(),
            source,
        })?;
        Self::new(manifest, events)
    }

    /// Load `<dir>/<task_id>.manifest.json` + `<dir>/<task_id>.jsonl`.
    pub fn load(dir: &Path, task_id: &str) -> Result<Self, GoldenError> {
        let read = |path: PathBuf| -> Result<String, GoldenError> {
            fs::read_to_string(&path).map_err(|e| GoldenError::Io {
                task_id: task_id.to_string(),
                path,
                message: e.to_string(),
            })
        };
        let manifest_json = read(manifest_path(dir, task_id))?;
        let recording = read(recording_path(dir, task_id))?;
        Self::parse(task_id, &manifest_json, &recording)
    }

    /// The recorded stream.
    pub fn events(&self) -> &[AgentEvent] {
        &self.events
    }

    /// The recording's provenance.
    pub fn manifest(&self) -> &GoldenManifest {
        &self.manifest
    }

    /// Structurally diff a candidate run against this golden — the golden is
    /// the left-hand side, so a diff's `left` is what was expected and `right`
    /// is what the run produced.
    pub fn diff(&self, run: &[AgentEvent]) -> Vec<StreamDiff> {
        structural_diff(&self.events, run)
    }

    /// Whether a candidate run is structurally equivalent to this golden.
    pub fn matches(&self, run: &[AgentEvent]) -> bool {
        self.diff(run).is_empty()
    }

    /// Render the two fixture documents (manifest JSON, recording JSONL) —
    /// the inverse of [`GoldenTrajectory::parse`], used by the refresh path.
    pub fn render(&self) -> (String, String) {
        // The manifest is a plain struct of owned, always-serializable fields;
        // `expect` documents that invariant rather than hiding a fallible path.
        let manifest = serde_json::to_string_pretty(&self.manifest)
            .expect("GoldenManifest is always serializable");
        (format!("{manifest}\n"), to_jsonl(&self.events))
    }

    /// Write this golden into `dir`, creating it if needed. Used by the
    /// refresh target; ordinary test runs only ever read.
    pub fn write(&self, dir: &Path) -> Result<(), GoldenError> {
        let io = |path: PathBuf, e: std::io::Error| GoldenError::Io {
            task_id: self.manifest.task_id.clone(),
            path,
            message: e.to_string(),
        };
        fs::create_dir_all(dir).map_err(|e| io(dir.to_path_buf(), e))?;
        let (manifest, recording) = self.render();
        let manifest_path = manifest_path(dir, &self.manifest.task_id);
        fs::write(&manifest_path, manifest).map_err(|e| io(manifest_path, e))?;
        let recording_path = recording_path(dir, &self.manifest.task_id);
        fs::write(&recording_path, recording).map_err(|e| io(recording_path, e))?;
        Ok(())
    }
}

/// Path of a golden's manifest sidecar.
pub fn manifest_path(dir: &Path, task_id: &str) -> PathBuf {
    dir.join(format!("{task_id}.manifest.json"))
}

/// Path of a golden's recording.
pub fn recording_path(dir: &Path, task_id: &str) -> PathBuf {
    dir.join(format!("{task_id}.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::StageKind;

    fn stage(name: StageKind) -> AgentEvent {
        AgentEvent::Stage { name }
    }
    fn complete() -> AgentEvent {
        AgentEvent::Complete {
            model: "scripted/only".into(),
            cost_usd: 0.0,
        }
    }
    fn events() -> Vec<AgentEvent> {
        vec![
            stage(StageKind::Triage),
            stage(StageKind::Execute),
            complete(),
        ]
    }
    fn source() -> RecordingSource {
        RecordingSource::RustStack {
            recorded_by: "a_test".into(),
        }
    }
    fn golden() -> GoldenTrajectory {
        let events = events();
        let manifest = GoldenManifest::for_recording("t", "a task", source(), &events);
        GoldenTrajectory::new(manifest, events).expect("well-formed")
    }

    // manifest serde

    #[test]
    fn the_manifest_round_trips_through_serde_json() {
        for src in [
            source(),
            RecordingSource::Reference {
                engine: "reference-engine".into(),
                adapter: "adapters/reference.rs".into(),
            },
        ] {
            let manifest = GoldenManifest::for_recording("t", "a task", src, &events());
            let json = serde_json::to_string(&manifest).unwrap();
            let back: GoldenManifest = serde_json::from_str(&json).unwrap();
            assert_eq!(manifest, back);
        }
    }

    #[test]
    fn only_a_reference_recording_counts_as_independent_evidence() {
        assert!(!source().is_reference());
        assert!(
            RecordingSource::Reference {
                engine: "e".into(),
                adapter: "a".into(),
            }
            .is_reference()
        );
    }

    // the gates

    #[test]
    fn a_well_formed_recording_loads_and_matches_itself() {
        let g = golden();
        assert_eq!(g.events().len(), 3);
        assert!(g.matches(&events()));
        assert!(g.diff(&events()).is_empty());
    }

    #[test]
    fn a_truncated_recording_is_rejected_rather_than_loaded_short() {
        // The manifest was written against three events; the recording lost
        // its tail. `parse_jsonl` alone would happily return the short prefix.
        let manifest = GoldenManifest::for_recording("t", "a task", source(), &events());
        let mut short = events();
        short.pop();
        match GoldenTrajectory::new(manifest, short) {
            Err(GoldenError::Truncated {
                declared, parsed, ..
            }) => {
                assert_eq!((declared, parsed), (3, 2));
            }
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    #[test]
    fn a_torn_tail_in_a_fixture_is_truncation_not_tolerance() {
        // A writer that died partway through the final event. L-T1 tolerance is
        // right for a live reader — keep the clean prefix — and wrong for a
        // committed fixture, where it would silently shrink the yardstick by an
        // event. Without the manifest's count this parses cleanly to 2 events.
        let g = golden();
        let (manifest_json, full) = g.render();
        let mut lines: Vec<&str> = full.lines().collect();
        let torn = &lines.pop().expect("a final line")[..12];
        let recording = format!("{}\n{torn}", lines.join("\n"));
        assert_eq!(
            parse_jsonl(&recording)
                .expect("a torn tail still parses")
                .len(),
            2,
            "the tolerant parser alone would hand back a short stream"
        );
        match GoldenTrajectory::parse("t", &manifest_json, &recording) {
            Err(GoldenError::Truncated { .. }) => {}
            other => panic!("expected Truncated, got {other:?}"),
        }
    }

    #[test]
    fn an_ill_formed_recording_cannot_become_a_yardstick() {
        // Two Completes — a stream terminates once. A golden that violates the
        // invariants would license runs that violate them too.
        let events = vec![complete(), complete()];
        let manifest = GoldenManifest::for_recording("t", "a task", source(), &events);
        match GoldenTrajectory::new(manifest, events) {
            Err(GoldenError::NotWellFormed { violations, .. }) => {
                assert!(!violations.is_empty());
            }
            other => panic!("expected NotWellFormed, got {other:?}"),
        }
    }

    #[test]
    fn a_manifest_for_another_task_is_rejected() {
        let (manifest_json, recording) = golden().render();
        match GoldenTrajectory::parse("other", &manifest_json, &recording) {
            Err(GoldenError::TaskIdMismatch { declared, .. }) => assert_eq!(declared, "t"),
            other => panic!("expected TaskIdMismatch, got {other:?}"),
        }
    }

    #[test]
    fn a_malformed_manifest_names_the_task_it_failed_for() {
        match GoldenTrajectory::parse("t", "{ not json }", "") {
            Err(GoldenError::Manifest { task_id, .. }) => assert_eq!(task_id, "t"),
            other => panic!("expected Manifest, got {other:?}"),
        }
    }

    // round-trip through the filesystem

    #[test]
    fn a_golden_round_trips_through_render_and_parse() {
        let g = golden();
        let (manifest_json, recording) = g.render();
        let back = GoldenTrajectory::parse("t", &manifest_json, &recording).expect("re-parses");
        assert_eq!(back.manifest(), g.manifest());
        assert!(back.matches(g.events()));
    }

    #[test]
    fn a_golden_round_trips_through_write_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let g = golden();
        g.write(dir.path()).expect("writes");
        let back = GoldenTrajectory::load(dir.path(), "t").expect("loads");
        assert_eq!(back.manifest(), g.manifest());
        assert!(back.matches(g.events()));
    }

    #[test]
    fn a_missing_fixture_names_the_path_it_looked_for() {
        let dir = tempfile::tempdir().unwrap();
        match GoldenTrajectory::load(dir.path(), "absent") {
            Err(GoldenError::Io { path, .. }) => {
                assert!(path.ends_with("absent.manifest.json"), "got {path:?}");
            }
            other => panic!("expected Io, got {other:?}"),
        }
    }

    // divergence

    #[test]
    fn a_diverging_run_reports_the_golden_on_the_left() {
        let g = golden();
        let mut run = events();
        run[1] = stage(StageKind::Verdict);
        let diff = g.diff(&run);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].left.as_deref(), Some("stage:Execute"));
        assert_eq!(diff[0].right.as_deref(), Some("stage:Verdict"));
    }
}
