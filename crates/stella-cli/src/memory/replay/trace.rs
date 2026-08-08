//! The trace format — a recorded engagement, replayable with no model calls.
//!
//! Versioned and serde-first (invariant 4): every type here round-trips through
//! `serde_json` byte-for-byte, and [`Trace::parse`] refuses an unknown version
//! rather than best-effort parsing it. A fixture that a newer loader wrote is a
//! fixture this loader cannot honestly replay, and quietly dropping the fields
//! it does not understand would produce a *plausible* summary — the worst
//! possible failure for a harness whose whole output is a claim about what the
//! learner built.
//!
//! ## Two departures from `doc:trace-replay-learning-harness` §3
//!
//! The spec sketches `shell: Vec<ShellInvocation>` and
//! `ScriptedReflection::Lessons(Vec<ReflectionLesson>)` directly. Neither
//! survives contact with the types:
//!
//! - `stella_core::tool_foundry::ShellInvocation` derives no serde impls, and
//!   giving it one would put a fixture-format concern in the engine. [`TraceShell`]
//!   is the two-field mirror, converted at the boundary.
//! - A `ReflectionLesson` carries `occurred_at` and `task_id`, which the
//!   *replayer* owns — a trace that could set them would be able to contradict
//!   its own clock. [`TraceLesson`] carries only what the model would have said.
//!
//! Both are cases of the fixture format deliberately not being the runtime
//! type, so a trace cannot express a state the live loop could never reach.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use stella_core::tool_foundry::ShellInvocation;
use stella_protocol::CompletionMessage;

use crate::memory::LessonKind;

/// The only trace version this loader accepts.
///
/// Bump it in the same change that alters the format, and teach [`Trace::parse`]
/// the migration or the refusal. It is `u32` and not a semver string because
/// there is exactly one consumer and a total order is all it needs.
pub(crate) const TRACE_VERSION: u32 = 1;

/// One recorded engagement: an ordered sequence of sessions, each a sequence of
/// turns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Trace {
    pub version: u32,
    /// A one-line description of what this corpus is *for*, printed beside the
    /// metrics. §6's honesty guard begins here: a number that cannot name its
    /// fixture is not reportable.
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub workspace: WorkspaceSeed,
    pub sessions: Vec<TraceSession>,
}

/// The tree a trace replays against, before any turn runs.
///
/// Anchor resolution reads the real filesystem (`memory::anchors`), so a lesson
/// naming `crates/stella-model/src/mistral.rs` anchors only if that path exists.
/// Seeding files is therefore not decoration — it is what puts the anchoring
/// path under test instead of silently skipping it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct WorkspaceSeed {
    /// Domain names written to `.stella/domains.toml`. A lesson's `domains` are
    /// filtered against these, so an undeclared domain is dropped exactly as it
    /// would be live.
    #[serde(default)]
    pub domains: Vec<String>,
    /// Path → contents, relative to the workspace root. Contents may be empty;
    /// only existence matters to anchoring.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
}

/// One session. `task_id` is the boundary governance counts distinct tasks by,
/// and supplying it is the entire reason `SessionMemory::set_task_id` exists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TraceSession {
    pub id: String,
    pub task_id: String,
    pub turns: Vec<TraceTurn>,
}

/// One turn: an instant, what the model would have said, and what the shell did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TraceTurn {
    /// Unix seconds. The replayer *sets* the clock to this before the turn;
    /// nothing in the harness ever reads the wall clock (#2320).
    pub at: i64,
    pub prompt: String,
    /// The transcript stub the reflection prompt digests. Only the tail matters
    /// — `reflect_on_turn` reverses and takes twelve — so a stub need not be a
    /// whole transcript.
    #[serde(default)]
    pub transcript: Vec<TraceMessage>,
    /// What the model would have said, replayed through the scripted provider.
    pub reflection: ScriptedReflection,
    /// The turn's shell history, accumulated and fed to `detect_tool_gaps`.
    #[serde(default)]
    pub shell: Vec<TraceShell>,
    /// Drives the reflection prompt template and the recorded outcome.
    #[serde(default = "yes")]
    pub succeeded: bool,
    /// Statements the user forgot *before* this turn ran.
    ///
    /// A real engagement contains forgetting, and it is the one input that can
    /// prove suppression is durable: a tombstone written here must stop the
    /// lesson being re-learned for the rest of the trace, including when a
    /// later turn restates it in different words. Applied ahead of the turn's
    /// reflection, which is where a user's "forget that" lands relative to the
    /// next thing the agent learns.
    #[serde(default)]
    pub forget: Vec<String>,
    /// Paths that left the tree *before* this turn ran.
    ///
    /// The deterministic half of retirement: a memory whose path anchors have
    /// all vanished is stale by a reproducible check, needing no model and no
    /// human. Without a way to delete a seeded file mid-trace there is no way
    /// to reach that sweep at all, so a corpus could never certify that a
    /// retired memory stops being rendered.
    #[serde(default)]
    pub removed_files: Vec<String>,
}

fn yes() -> bool {
    true
}

/// A transcript line. Deliberately not `CompletionMessage`: a trace needs a
/// role and some text, and nothing downstream of the reflection digest reads
/// tool calls or ids off it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TraceMessage {
    pub role: TraceRole,
    pub content: String,
}

/// Who spoke. Only the roles a reflection digest distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TraceRole {
    User,
    Assistant,
}

impl From<&TraceMessage> for CompletionMessage {
    fn from(message: &TraceMessage) -> Self {
        match message.role {
            TraceRole::User => CompletionMessage::user(&message.content),
            TraceRole::Assistant => CompletionMessage::assistant(&message.content),
        }
    }
}

/// One shell invocation, mirroring [`ShellInvocation`] with serde attached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TraceShell {
    pub command: String,
    #[serde(default = "yes")]
    pub succeeded: bool,
}

impl From<&TraceShell> for ShellInvocation {
    fn from(shell: &TraceShell) -> Self {
        ShellInvocation {
            command: shell.command.clone(),
            succeeded: shell.succeeded,
        }
    }
}

/// What the reflection model would have returned this turn.
///
/// **The two failure arms are not padding.** They are what the live loop
/// actually hits, and the ones that starve learning *silently*:
/// `reflect_and_record` returns `recorded: 0` both for "nothing worth learning"
/// and for "I could not read the response", and telling those apart is the
/// entire purpose of the `Unreadable` branch. A corpus that only ever scripts
/// happy-path lessons cannot certify the starvation guard, which is the most
/// valuable assertion the harness has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ScriptedReflection {
    /// A well-formed lessons array.
    Lessons {
        lessons: Vec<TraceLesson>,
        /// Where these lessons came from. Defaults to [`Provenance::Recorded`]
        /// so a hand-written fixture needs no ceremony; an adapter that
        /// *derives* lessons from a foreign transcript must say so, or a
        /// downstream number could silently mix invented lessons with recorded
        /// ones (`doc:trace-replay-learning-harness` §7.2).
        #[serde(default)]
        provenance: Provenance,
    },
    /// A response the parser cannot read — drives `ReflectionParse::Unreadable`.
    Unreadable { text: String },
    /// The model call itself failed — drives `StandaloneCallError`.
    ModelError { message: String },
    /// **The source carried no reflection at all**, so this turn runs the
    /// foundry and the timeline but never touches the reflection path.
    ///
    /// The arm exists because of a real property of the only non-synthetic
    /// corpus available: Claude Code transcripts contain no Stella reflection
    /// JSON (§7.2). Every way of expressing that with the other three arms is a
    /// claim the source does not support — `Lessons { lessons: [] }` asserts the
    /// model had nothing to say, and `Unreadable` asserts it said something
    /// unparseable. Both would be *invented signal* in the one place the spec
    /// warns hardest about, and `Unreadable` in particular would fabricate
    /// starvation and corrupt assertion 7's own metric.
    ///
    /// So it is its own arm, counted separately in the summary, and the
    /// replayer skips the model boundary entirely for it.
    NotRecorded,
}

/// One lesson as the model would have emitted it.
///
/// No `occurred_at` and no `task_id`: both are the replayer's to stamp, from the
/// turn's clock and the session's boundary. A trace that could set them would be
/// able to contradict its own timeline, and every assertion about recency or
/// distinct-task counting would then be asserting the fixture rather than the
/// learner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TraceLesson {
    pub lesson: String,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub kind: LessonKind,
}

/// Whether a scripted reflection was recorded or derived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Provenance {
    /// Written by hand, or captured from a real Stella reflection. The claim a
    /// metric may be reported against.
    #[default]
    Recorded,
    /// Synthesized by an adapter from a source that carried no reflection JSON.
    /// Present so option (a) in §7.2 is *labelled* if it is ever built — a
    /// number derived from these measures the synthesizer, not the learner.
    Derived,
}

/// Why a trace could not be loaded. Typed, not a `String` (invariant 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TraceError {
    /// The bytes are not the trace format at all.
    Malformed(String),
    /// A version this loader does not implement. Never best-effort parsed.
    UnsupportedVersion { found: u32, supported: u32 },
    /// A structurally valid trace that could not be replayed as written.
    Unreplayable(String),
}

impl std::fmt::Display for TraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TraceError::Malformed(why) => write!(f, "trace is malformed: {why}"),
            TraceError::UnsupportedVersion { found, supported } => write!(
                f,
                "trace version {found} is not supported (this loader implements {supported}); \
                 refusing rather than parsing it partially"
            ),
            TraceError::Unreplayable(why) => write!(f, "trace cannot be replayed: {why}"),
        }
    }
}

impl Trace {
    /// Parse a trace, refusing an unknown version.
    pub(crate) fn parse(json: &str) -> Result<Self, TraceError> {
        let trace: Trace =
            serde_json::from_str(json).map_err(|e| TraceError::Malformed(e.to_string()))?;
        if trace.version != TRACE_VERSION {
            return Err(TraceError::UnsupportedVersion {
                found: trace.version,
                supported: TRACE_VERSION,
            });
        }
        trace.check()?;
        Ok(trace)
    }

    /// Load a committed fixture by name from `tests/fixtures/traces/`.
    ///
    /// Resolved through `CARGO_MANIFEST_DIR`, the in-crate convention already
    /// used by `crate::paths` and four other places — never a path relative to
    /// the process's cwd, which differs between `cargo test` and a debugger.
    pub(crate) fn fixture(name: &str) -> Result<Self, TraceError> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("traces")
            .join(format!("{name}.json"));
        let json = std::fs::read_to_string(&path)
            .map_err(|e| TraceError::Malformed(format!("cannot read {}: {e}", path.display())))?;
        Self::parse(&json)
    }

    /// Structural checks a replay depends on but serde cannot express.
    ///
    /// Non-decreasing time is the one that matters. The clock is *set* per turn
    /// rather than advanced, so a trace that went backwards would replay
    /// happily and produce a log whose recency ordering contradicts its own
    /// line order — an assertion about retirement or ranking would then be
    /// asserting nonsense. Refuse at load, where the fixture author can see it.
    fn check(&self) -> Result<(), TraceError> {
        let mut previous: Option<i64> = None;
        let mut seen_sessions: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for session in &self.sessions {
            if !seen_sessions.insert(session.id.as_str()) {
                return Err(TraceError::Unreplayable(format!(
                    "session id {:?} appears twice; ids identify a session's own \
                     `SessionMemory`, so a repeat would silently merge two sessions",
                    session.id
                )));
            }
            if session.turns.is_empty() {
                return Err(TraceError::Unreplayable(format!(
                    "session {:?} has no turns",
                    session.id
                )));
            }
            for turn in &session.turns {
                if let Some(previous) = previous
                    && turn.at < previous
                {
                    return Err(TraceError::Unreplayable(format!(
                        "session {:?} steps time backwards ({previous} → {}); the replayer \
                         sets the clock per turn, so a trace must be non-decreasing",
                        session.id, turn.at
                    )));
                }
                previous = Some(turn.at);
            }
        }
        if self.sessions.is_empty() {
            return Err(TraceError::Unreplayable("trace has no sessions".into()));
        }
        Ok(())
    }

    /// Every turn in the trace, in replay order, tagged with its session.
    pub(crate) fn turns(&self) -> impl Iterator<Item = (&TraceSession, &TraceTurn)> {
        self.sessions
            .iter()
            .flat_map(|session| session.turns.iter().map(move |turn| (session, turn)))
    }

    /// How many distinct tasks the corpus spans — the denominator §6's
    /// recurrence profile is read against.
    pub(crate) fn distinct_tasks(&self) -> usize {
        self.sessions
            .iter()
            .map(|s| s.task_id.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }
}

impl ScriptedReflection {
    /// The wire text a provider would have returned for this arm.
    ///
    /// `Lessons` renders as the bare JSON array the reflection prompt asks for,
    /// which is what puts the fixture on the same parsing path a live response
    /// takes — including the domain filter and the three-lesson truncation.
    pub(crate) fn as_response(&self) -> Option<String> {
        match self {
            ScriptedReflection::Lessons { lessons, .. } => {
                Some(serde_json::to_string(lessons).unwrap_or_else(|_| "[]".to_string()))
            }
            ScriptedReflection::Unreadable { text } => Some(text.clone()),
            ScriptedReflection::ModelError { .. } | ScriptedReflection::NotRecorded => None,
        }
    }

    /// Whether this turn reaches the reflection path at all.
    ///
    /// [`ScriptedReflection::NotRecorded`] does not: there is nothing to script,
    /// and scripting *something* would invent the signal under test.
    pub(crate) fn reaches_the_model(&self) -> bool {
        !matches!(self, ScriptedReflection::NotRecorded)
    }
}
