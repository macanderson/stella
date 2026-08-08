//! The learning plane reads its time from an injected clock, not the wall
//! clock (#2320).
//!
//! `stella-context` has shipped a `Clock` port since the store went
//! bi-temporal, and `ContextStore::open_with` has always accepted one. The
//! CLI's learning path routed around it: `SystemTime::now()` at six sites, plus
//! a pid folded into the task id. Two consequences, and only the second is
//! about replay:
//!
//! - a session's mined log could not be reproduced, which blocks every
//!   assertion in `doc:trace-replay-learning-harness`; and
//! - `retire_failing_context` always handed the truth sweep *today*, so no test
//!   could place a record past its TTL and watch it retire. That one is a
//!   defect on its own terms.
//!
//! What makes the first assertion below a witness rather than a tautology is
//! worth stating, because the obvious version is not one. "Two sessions on a
//! fixed clock write byte-identical logs" **passes on `main` by luck**: two
//! opens milliseconds apart almost always land in the same wall-clock second
//! and share a pid, so the ids match and nothing is proven. The assertion that
//! actually fails on `main` is the second one — the log's timestamps equal the
//! *fixed instant we chose*, which is years away from today. Byte-identity is
//! kept beside it because it is the property the harness depends on; the
//! pinned instant is what proves the seam exists.

use std::sync::Mutex;

use async_trait::async_trait;
use stella_context::{Clock, FixedClock};
use stella_protocol::{
    CompletionMessage, CompletionRequestRef, CompletionResult, CompletionUsage, Provider,
    ProviderError,
};

use crate::memory::*;

/// The instant both sessions run at: 2023-11-14T22:13:20Z. Deliberately far in
/// the past, so a wall-clock read cannot coincide with it.
const REPLAY_INSTANT: i64 = 1_700_000_000;

/// A provider that answers every call with one fixed completion — the reflection
/// the model "would have said". The single model boundary in the learning loop,
/// stubbed; everything downstream of it is already deterministic.
struct ScriptedProvider {
    text: String,
    calls: Mutex<u32>,
}

impl ScriptedProvider {
    fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            calls: Mutex::new(0),
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }

    async fn complete_ref(
        &self,
        _request: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        *self.calls.lock().expect("call count") += 1;
        Ok(CompletionResult {
            text: self.text.clone(),
            tool_calls: Vec::new(),
            // `reported: true` is load-bearing, not decoration: the standalone
            // call's accounting closeout refuses to settle a call whose usage
            // never arrived, and the reflection surfaces that as
            // "accounting closeout failed" — indistinguishable, from the
            // outside, from a model that said nothing.
            usage: CompletionUsage {
                reported: true,
                input_tokens: 1,
                ..CompletionUsage::default()
            },
            model: "scripted".to_string(),
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// One well-formed lessons array, with a domain the workspace does not declare
/// so the domain filter has something to strip — keeping the fixture on the
/// same path a real reflection takes.
const SCRIPTED_LESSONS: &str = r#"[
  {"lesson": "Provider adapters live one file per vendor under stella-model/src.", "domains": [], "kind": "domain"},
  {"lesson": "Run make gate before pushing; a red gate is an automatic not-yet.", "domains": [], "kind": "process"}
]"#;

/// Run one scripted reflection turn against a fresh workspace on a clock frozen
/// at `REPLAY_INSTANT`, and hand back the mining log it wrote.
async fn one_scripted_turn(root: &Path) -> String {
    let clock: std::sync::Arc<dyn Clock> = std::sync::Arc::new(FixedClock::new(REPLAY_INSTANT));
    let mut memory = SessionMemory::open_with_clock(root, false, false, clock, "witness")
        .expect("session memory");
    let provider = ScriptedProvider::new(SCRIPTED_LESSONS);
    let transcript = vec![
        CompletionMessage::user("add the mistral adapter"),
        CompletionMessage::assistant("added crates/stella-model/src/mistral.rs"),
    ];
    let report = memory
        .reflect_and_record(&provider, "scripted", &transcript, true, true, None)
        .await;
    // A silent `recorded: 0` is exactly the starvation the loop is prone to, so
    // the fixture refuses to be satisfied by one: if the scripted reflection
    // did not land, say why rather than failing later on an empty file.
    assert!(
        report.model_error.is_none(),
        "the scripted reflection failed: {:?}",
        report.model_error
    );
    std::fs::read_to_string(
        root.join(".stella")
            .join("private")
            .join("reflections.jsonl"),
    )
    .unwrap_or_default()
}

/// A fresh workspace with the `.stella` skeleton the memory paths expect.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".stella")).expect("workspace");
    dir
}

/// THE witness. Every timestamp a reflection turn records comes from the
/// session's clock, so a session pinned to a past instant records that instant.
///
/// Fails on `main`, deterministically and for two independent reasons: the
/// lesson's `occurred_at` is stamped from `SystemTime::now()` inside
/// `parse_lessons_checked`, and the `task_id` embeds both `SystemTime::now()`
/// and the pid. Both read *today*, which is not `REPLAY_INSTANT`.
#[tokio::test]
async fn a_session_records_its_clocks_instant_not_todays() {
    let dir = workspace();
    let log = one_scripted_turn(dir.path()).await;
    assert!(
        !log.trim().is_empty(),
        "the scripted reflection should have recorded at least one lesson"
    );

    for line in log.lines().filter(|l| !l.trim().is_empty()) {
        let lesson: ReflectionLesson =
            serde_json::from_str(line).expect("each log line is a ReflectionLesson");
        assert_eq!(
            lesson.occurred_at, REPLAY_INSTANT as u64,
            "occurred_at must come from the session clock, not the wall clock"
        );
        assert_eq!(
            lesson.task_id,
            format!("session:{REPLAY_INSTANT}-witness"),
            "the task id must be built from the session clock and the supplied \
             nonce — no wall-clock read, no pid"
        );
    }
}

/// The property the replay harness actually depends on: one trace, run twice,
/// writes the same bytes. Distinct temp workspaces, same clock, same script.
///
/// Kept beside the assertion above rather than instead of it — on its own this
/// can pass on `main` whenever both sessions land in one wall-clock second, so
/// it certifies the harness's contract without witnessing the fix.
#[tokio::test]
async fn two_runs_of_one_scripted_turn_are_byte_identical() {
    let first = workspace();
    let second = workspace();
    let a = one_scripted_turn(first.path()).await;
    let b = one_scripted_turn(second.path()).await;
    assert!(!a.trim().is_empty(), "the first run recorded nothing");
    assert_eq!(a, b, "two replays of one turn must write identical logs");
}

/// The seam's other half, and the reason it is a defect fix rather than only a
/// harness prerequisite: the truth sweep's `now` is the session's, so a test can
/// finally place time past a record's TTL.
///
/// Impossible to write before this change — `retire_failing_context` built its
/// `now` from `SystemTime::now()`, so every sweep saw today no matter what the
/// test did.
#[tokio::test]
async fn the_truth_sweep_sees_the_session_clock() {
    let dir = workspace();
    let clock = std::sync::Arc::new(FixedClock::new(REPLAY_INSTANT));
    let memory = SessionMemory::open_with_clock(
        dir.path(),
        false,
        false,
        clock.clone() as std::sync::Arc<dyn Clock>,
        "sweep",
    )
    .expect("session memory");

    assert_eq!(
        memory.clock_now_rfc3339(),
        stella_context::format_rfc3339(REPLAY_INSTANT),
        "the session's now is its clock's now"
    );

    // A year later, on demand rather than on the calendar.
    clock.advance(365 * 24 * 60 * 60);
    assert_eq!(
        memory.clock_now_rfc3339(),
        stella_context::format_rfc3339(REPLAY_INSTANT + 365 * 24 * 60 * 60),
        "advancing the session clock moves the instant the sweep will see"
    );
}
