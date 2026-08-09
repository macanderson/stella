//! The session clock (#2320): every timestamp the learning loop writes comes
//! from one injected time source, never the wall clock.
//!
//! `stella-context` has shipped [`Clock`]/[`FixedClock`] since the bi-temporal
//! store landed, and `ContextStore::open_and_warm` has always taken one — the
//! learning loop simply routed around the port and called `SystemTime::now()`
//! in five places. Two consequences, and this module pins both:
//!
//! 1. **Nothing the loop wrote was reproducible.** `occurred_at` on every mined
//!    lesson, and the session's task id, came from the wall clock, so the
//!    mining log the skill and rule miners re-read differed on every run.
//! 2. **Retirement was unobservable.** `retire_failing_context` judged TTLs
//!    against `SystemTime::now()`, so a test could never place a record past
//!    its expiry — the sweep always saw today.
//!
//! Its own file rather than more of `memory/tests.rs`, which sits close under
//! the 1500-line ratchet `scripts/check-file-size.sh` enforces.

use std::sync::Arc;

use async_trait::async_trait;
use stella_context::{Clock, FixedClock, format_rfc3339};
use stella_protocol::{
    CompletionRequestRef, CompletionResult, CompletionUsage, Provider, ProviderError,
};

use super::msg;
use crate::memory::*;

/// A fixed instant well clear of "now", so a test that accidentally read the
/// wall clock could not pass by coincidence. 2020-09-13T12:26:40Z — the
/// reference instant `stella-context`'s own clock tests use.
const T: i64 = 1_600_000_000;

/// Answers every reflection with one byte-identical lesson.
///
/// The response carries no timestamp of its own: `occurred_at` is stamped by
/// `parse_lessons_checked` from the instant the caller supplies, which is
/// exactly the value under test.
struct ScriptedReflection;

#[async_trait]
impl Provider for ScriptedReflection {
    fn id(&self) -> &str {
        "scripted"
    }

    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        Ok(CompletionResult {
            text: r#"{"lessons": [{"lesson": "prefer withTenantDb over raw db()",
                 "kind": "domain", "domains": []}]}"#
                .into(),
            tool_calls: vec![],
            usage: CompletionUsage {
                reported: true,
                input_tokens: 1,
                ..CompletionUsage::default()
            },
            model: "scripted".into(),
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// Run one scripted turn against a fresh workspace whose session clock is
/// frozen at `at`, and return the bytes of the mining log it produced.
async fn reflection_log_at(at: i64) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".stella")).expect("workspace");

    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(at));
    let mut memory = SessionMemory::open_with_clock(dir.path(), false, false, clock)
        .expect("session memory opens in a temp workspace");

    let transcript = vec![
        msg(MessageRole::User, "fix the tenancy leak"),
        msg(MessageRole::Assistant, "swapped db() for withTenantDb"),
    ];
    let report = memory
        .reflect_and_record(
            &ScriptedReflection,
            "scripted",
            crate::memory::TurnEvidence::from_transcript(&transcript, true),
            true,
            None,
            crate::memory::ReflectionPosture::default(),
        )
        .await;
    assert_eq!(report.recorded, 1, "the scripted lesson was mined");

    let path = stella_store::workspace_private_state_path(dir.path(), "reflections.jsonl")
        .expect("private state path resolves");
    let bytes = std::fs::read_to_string(&path).expect("the mining log was written");
    (dir, bytes)
}

/// **The witness.** Two sessions frozen at the same instant write the same
/// bytes, *and those bytes carry that instant*.
///
/// Byte equality rather than a field-by-field comparison is deliberate: the log
/// is re-read verbatim by `mine_skill_candidates` and the observation
/// extractor, so *the bytes* are the interface, and a new field that quietly
/// reintroduced a clock read would slip past a narrower assertion.
///
/// The second half is not decoration, and measuring it changed this test.
/// Equality **alone passes on the unfixed code**: both sessions run in one
/// process, so a wall-clock `occurred_at` and a pid-bearing task id agree with
/// each other whenever the two runs land in the same second — which is almost
/// always. Verified by reverting the three internals behind their new
/// signatures: the equality-only form passed while the three value-pinning
/// tests below failed. An assertion that holds on the broken code except when
/// it straddles a second boundary is a flake, not a witness, so this one pins
/// the value too.
#[tokio::test]
async fn two_sessions_at_one_fixed_clock_write_byte_identical_mining_logs() {
    let (_a_dir, a) = reflection_log_at(T).await;
    let (_b_dir, b) = reflection_log_at(T).await;
    assert_eq!(
        a, b,
        "two sessions frozen at the same instant must produce an identical \
         mining log — this is the property trace replay rests on"
    );
    assert!(
        a.contains(&format!("\"occurred_at\":{T}"))
            && a.contains(&format!("\"task_id\":\"session:{T}\"")),
        "…and they must agree on the *injected* instant, not merely on \
         whatever the wall clock read twice in one second: {a}"
    );
}

/// The companion the byte-equality test needs to be sound.
///
/// Equality alone can pass for the wrong reason: two sessions that both read
/// the wall clock inside one second agree with each other while agreeing with
/// nothing. This pins the actual value, so the log can only match if the
/// injected clock — and not the machine's — is what reached the record.
#[tokio::test]
async fn the_mined_lesson_is_stamped_with_the_injected_instant() {
    let (_dir, log) = reflection_log_at(T).await;
    let line = log.lines().next().expect("one lesson was logged");
    let lesson: ReflectionLesson = serde_json::from_str(line).expect("the log line is a lesson");

    assert_eq!(
        lesson.occurred_at, T as u64,
        "occurred_at comes from the session clock, not the wall clock"
    );
    assert_eq!(
        lesson.task_id,
        format!("session:{T}"),
        "the deterministic constructor derives its task id from the same clock"
    );
}

/// Different instants must still produce different records — the fix must not
/// have frozen time into a constant, which would make every replay agree for
/// the uninteresting reason.
#[tokio::test]
async fn a_later_clock_stamps_a_later_lesson() {
    let (_early_dir, early) = reflection_log_at(T).await;
    let (_late_dir, late) = reflection_log_at(T + 86_400).await;
    assert_ne!(
        early, late,
        "a session a day later records a different instant"
    );

    let later: ReflectionLesson =
        serde_json::from_str(late.lines().next().expect("one lesson")).expect("a lesson");
    assert_eq!(later.occurred_at, (T + 86_400) as u64);
}

/// The clock the session hands the context store is the same one it stamps its
/// own records with.
///
/// Before #2320 `open_*` built the store with `SystemClock` unconditionally, so
/// even a caller that wanted a fixed clock got a store that disagreed with it —
/// the bi-temporal `recorded_at` on every memory came from the wall clock while
/// the mining log came from somewhere else. Nothing could line the two up.
#[tokio::test]
async fn the_context_store_shares_the_session_clock() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".stella")).expect("workspace");

    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(T));
    let mut memory = SessionMemory::open_with_clock(dir.path(), false, false, clock)
        .expect("session memory opens in a temp workspace");

    let transcript = vec![
        msg(MessageRole::User, "fix the tenancy leak"),
        msg(MessageRole::Assistant, "swapped db() for withTenantDb"),
    ];
    let report = memory
        .reflect_and_record(
            &ScriptedReflection,
            "scripted",
            crate::memory::TurnEvidence::from_transcript(&transcript, true),
            true,
            None,
            crate::memory::ReflectionPosture::default(),
        )
        .await;
    assert_eq!(report.recorded, 1, "the scripted lesson was mined");

    // `recorded_at` is the store's own transaction time, written by the clock
    // handed to `ContextStore::open_and_warm` — a different write path from the
    // mining log above, and the one that proves both halves read one source.
    let nodes = memory.store.memory_nodes().expect("memory nodes readable");
    let node = nodes.first().expect("the lesson landed in context.db");
    assert_eq!(
        node.recorded_at,
        format_rfc3339(T),
        "the store stamps its transaction time from the session clock; before \
         #2320 `open_*` built it with SystemClock no matter what the caller asked for"
    );
}
