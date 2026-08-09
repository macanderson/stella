//! Reflection's mining half — what a turn's model output parses into, and what
//! `reflect_and_record` then writes to the log, the store, and the recall bands.
//!
//! Its own module for the reason `skill_creation` is: this is the largest
//! single seam in these tests and the one the file kept growing on, and
//! #2465's counterfactual prompt is what finally took `tests.rs` over the
//! 1500-line ratchet. The ratchet's answer to a file crossing for the first
//! time is a split and never a baseline entry (AGENTS.md § "God files — plan
//! around them, never into them"). Moved verbatim; the assertions are
//! unchanged.
//!
//! The line count only chose the moment, not the seam. Everything here starts
//! at a model response and ends at a stored lesson or a self-review, and the
//! three fixtures it shares — `lessons_of`, `lessons_with` and
//! `reflection_execution_ids` — have no reader on the recall side, so they
//! move with it rather than staying behind as parent-module surface.

use super::msg;
use crate::memory::*;

#[test]
fn parse_lessons_drops_invented_domains_and_caps_at_the_per_turn_limit() {
    let allowed = vec!["api".to_string(), "cli".to_string()];
    let mut entries = vec![
        r#"{"lesson": "prefer tables", "domains": ["cli", "made-up"]}"#.to_string(),
        r#"{"lesson": "b", "domains": []}"#.to_string(),
        r#"{"lesson": "c", "domains": ["API"]}"#.to_string(),
    ];
    // Two past the cap, whatever the cap currently is: the assertion below is
    // about the limit being enforced, and hard-coding the overflow count means
    // raising the cap silently turns this into a test of nothing.
    while entries.len() < crate::memory::reflection::MAX_LESSONS_PER_TURN + 2 {
        entries.push(format!(r#"{{"lesson": "filler {}"}}"#, entries.len()));
    }
    let text = format!("Sure! [{}]", entries.join(","));
    let lessons = lessons_with(&text, &allowed);
    assert_eq!(
        lessons.len(),
        crate::memory::reflection::MAX_LESSONS_PER_TURN,
        "capped at the per-turn limit"
    );
    assert_eq!(lessons[0].domains, vec!["cli"], "invented domain dropped");
    assert_eq!(
        lessons[2].domains,
        vec!["API"],
        "case-insensitive match kept"
    );
    // The parser deliberately stamps NO instant (#2320). It used to assert
    // `occurred_at > 0` here, back when `parse_lessons_checked` read the wall
    // clock — the read that made the mining log unreproducible. Stamping is now
    // the session's, in `reflect_and_record`'s existing task-id pass, and is
    // pinned end to end in `memory::tests::determinism`. Asserting the zero
    // keeps that boundary honest: a clock creeping back into the parser would
    // fail here rather than pass quietly.
    assert_eq!(
        lessons[0].occurred_at, 0,
        "parsing assigns no instant — the session does"
    );
}

#[test]
fn parse_lessons_tolerates_garbage_and_empty_output() {
    assert!(lessons_of("[]").is_empty());
    assert!(lessons_of("[{\"lesson\": \"   \"}]").is_empty());
}

/// An unreadable response and a turn with nothing to learn are different
/// outcomes and must not report the same way.
///
/// Reporting them identically is what let the context lifecycle starve in
/// silence: a model that narrates instead of answering yields `recorded: 0,
/// error: null` on every turn, which is indistinguishable from an agent that
/// keeps getting everything right.
#[test]
fn unreadable_reflection_is_distinguished_from_having_nothing_to_say() {
    use crate::memory::reflection::{ReflectionParse, parse_lessons_checked};

    assert!(
        matches!(
            parse_lessons_checked("", &[]),
            ReflectionParse::Lessons(lessons) if lessons.is_empty()
        ),
        "an empty response is a legitimate 'nothing to record'"
    );
    assert!(
        matches!(
            parse_lessons_checked("no json here", &[]),
            ReflectionParse::Unreadable(_)
        ),
        "prose with no array is a response we failed to read, not an empty one"
    );
}

/// The real-world failure: a model that thinks out loud before answering.
///
/// The previous first-`[`-to-last-`]` rule broke on exactly this, because the
/// narration contains brackets of its own. Observed live on
/// `z-ai/glm-4.7-flash` and `deepseek/deepseek-v4-flash`, both of which
/// produced zero lessons on every turn.
#[test]
fn parse_lessons_reads_an_array_out_of_narrated_output() {
    let allowed = vec!["cli".to_string()];
    let narrated = "1. **Analyze the request:**\n   *   Output format: JSON array \
         (max 3 items).\n   *   Allowed tags: [\"cli\", \"api\"].\n\n\
         2. Now the answer:\n\n```json\n\
         [{\"lesson\": \"register handlers in registry.py\", \"domains\": [\"cli\"]}]\n\
         ```\nHope that helps!";
    let lessons = lessons_with(narrated, &allowed);
    assert_eq!(lessons.len(), 1, "the real array is found past the prose");
    assert_eq!(lessons[0].lesson, "register handlers in registry.py");
    assert_eq!(lessons[0].domains, vec!["cli"]);
}

/// The self-review rides in the same response as the lessons, so the one
/// reflection call that already runs now fills `execution_reflection`'s
/// model-authored half — the columns the Observatory's AVG SELF-RATING,
/// DELIVERED and "what to improve" panels read, and which no producer in the
/// tree had ever written.
#[test]
fn reflection_response_yields_both_lessons_and_a_self_review() {
    let allowed = vec!["cli".to_string()];
    let response = "{\"lessons\": [{\"lesson\": \"register handlers in registry.py\", \
         \"kind\": \"domain\", \"domains\": [\"cli\"]}], \
         \"self_review\": {\"delivered\": true, \"rating\": 7, \
         \"went_well\": \"found the seam fast\", \
         \"to_improve\": \"should have run the gate by exit code\", \
         \"critique\": \"correct, but slow to verify\"}}";
    let lessons = lessons_with(response, &allowed);
    assert_eq!(lessons.len(), 1, "the lessons array is still read");
    assert_eq!(lessons[0].lesson, "register handlers in registry.py");

    let review = crate::memory::reflection::parse_self_review(response)
        .expect("the self_review object is read");
    assert_eq!(review.delivered, Some(true));
    assert_eq!(review.self_rating, Some(7));
    assert_eq!(
        review.what_to_improve,
        "should have run the gate by exit code"
    );
}

/// A model that answers with a bare lesson array — the format this prompt asked
/// for until the self-review was added, and what any model that ignores the new
/// envelope will still produce — must keep mining lessons exactly as before.
/// Lesson mining is the one stage of this loop that already worked; adding the
/// self-review must not put it at risk.
#[test]
fn a_bare_lesson_array_still_mines_lessons_and_simply_has_no_self_review() {
    let allowed = vec!["cli".to_string()];
    let bare = "[{\"lesson\": \"money is integer minor units\", \
         \"kind\": \"domain\", \"domains\": [\"cli\"]}]";
    assert_eq!(lessons_with(bare, &allowed).len(), 1);
    assert!(
        crate::memory::reflection::parse_self_review(bare).is_none(),
        "no self_review offered is None, not an invented row"
    );
}

/// Narration around the JSON breaks a naive slice in both directions, and the
/// self-review scanner has to tolerate it for the same reason the lesson
/// scanner does.
#[test]
fn a_self_review_is_read_out_of_narrated_output() {
    let narrated = "Let me reflect. The rubric said {rating: 0-10}.\n\n```json\n\
         {\"lessons\": [], \"self_review\": {\"rating\": 3, \
         \"to_improve\": \"read the whole file first\"}}\n```\nDone!";
    let review =
        crate::memory::reflection::parse_self_review(narrated).expect("found past the prose");
    assert_eq!(review.self_rating, Some(3));
    assert_eq!(review.what_to_improve, "read the whole file first");
    assert_eq!(review.delivered, None, "a field not offered stays absent");
}

/// A rating on some other scale is dropped rather than clamped. Clamping 95 to
/// 10 would put a fabricated perfect score under a label that promises the
/// model's own number; `None` reads as "declined to grade", which is true.
#[test]
fn an_out_of_range_rating_is_dropped_not_clamped() {
    let of = |rating: &str| {
        crate::memory::reflection::parse_self_review(&format!(
            "{{\"self_review\": {{\"rating\": {rating}}}}}"
        ))
        .expect("object parses")
        .self_rating
    };
    assert_eq!(of("95"), None);
    assert_eq!(of("-1"), None);
    assert_eq!(of("10"), Some(10));
    assert_eq!(of("0"), Some(0));
}

/// End-to-end proof that the self-review reaches the table the Observatory
/// reads. The dashboard's AVG SELF-RATING / DELIVERED / "what to improve"
/// panels select from `execution_reflection`, and before this every row in a
/// real workspace had NULL in all five model-authored columns because no
/// producer existed — the writer hardcoded `self_rating: None` and nothing else
/// ever wrote them. This drives the actual loop against a stub provider and
/// asserts the row.
#[tokio::test]
async fn reflect_and_record_stores_the_models_self_review_against_its_execution() {
    use async_trait::async_trait;
    use stella_protocol::{
        CompletionRequestRef, CompletionResult, CompletionUsage, Provider, ProviderError,
    };

    struct StubProvider;
    #[async_trait]
    impl Provider for StubProvider {
        fn id(&self) -> &str {
            "stub"
        }
        async fn complete_ref(
            &self,
            _req: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            Ok(CompletionResult {
                text: r#"{"lessons": [{"lesson": "prefer withTenantDb over raw db()",
                     "kind": "domain", "domains": []}],
                     "self_review": {"delivered": true, "rating": 6,
                     "went_well": "found the leak", "to_improve": "should have run the suite",
                     "critique": "correct fix, thin verification"}}"#
                    .into(),
                tool_calls: vec![],
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 1,
                    ..CompletionUsage::default()
                },
                model: "stub".into(),
                cost_usd: 0.0,
                finish_reason: None,
            })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".stella")).unwrap();
    let store = stella_store::Store::open(dir.path()).expect("open store in temp workspace");
    let execution_id = store
        .begin_execution("deck-pipeline", "fix the tenancy leak", "stub", "stub")
        .unwrap();
    let mut memory =
        SessionMemory::open(dir.path(), false).expect("open session memory in temp workspace");
    memory.set_execution_id(execution_id);

    let transcript = vec![
        msg(MessageRole::User, "fix the tenancy leak"),
        msg(MessageRole::Assistant, "swapped db() for withTenantDb"),
    ];
    let report = memory
        .reflect_and_record(
            &StubProvider,
            "stub",
            &transcript,
            true,
            true,
            None,
            crate::memory::ReflectionPosture::default(),
        )
        .await;
    assert_eq!(report.recorded, 1, "lesson mining still works");

    let review = store
        .self_review(execution_id)
        .unwrap()
        .expect("a reflection row exists for this execution");
    assert_eq!(
        review.self_rating,
        Some(6),
        "AVG SELF-RATING now has something to average"
    );
    assert_eq!(
        review.delivered,
        Some(true),
        "DELIVERED now has something to count"
    );
    assert_eq!(review.what_to_improve, "should have run the suite");
    assert_eq!(review.what_went_well, "found the leak");

    // The lesson is traceable to the turn that taught it, too — the same
    // missing execution id that starved the self-review left every mined
    // reflection row unattributed.
    assert_eq!(
        reflection_execution_ids(dir.path()),
        vec![Some(execution_id)],
        "the mined lesson names its execution"
    );
}

/// Every `reflections.execution_id` in a workspace store, in row order. Read
/// with a fresh connection because the attribution is what is under test and
/// `Store` exposes no targeted reader for it.
fn reflection_execution_ids(workspace_root: &std::path::Path) -> Vec<Option<i64>> {
    let conn = rusqlite::Connection::open(workspace_root.join(".stella/private/store.db")).unwrap();
    let mut stmt = conn
        .prepare("SELECT execution_id FROM reflections ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, Option<i64>>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// A path that never adopted `set_execution_id` must degrade exactly as before
/// — lessons still mine, the self-review is dropped rather than written against
/// a guessed row. Attributing one turn's grade to another execution would be
/// worse than not recording it.
#[tokio::test]
async fn without_an_execution_id_the_self_review_is_dropped_not_misattributed() {
    use async_trait::async_trait;
    use stella_protocol::{
        CompletionRequestRef, CompletionResult, CompletionUsage, Provider, ProviderError,
    };

    struct StubProvider;
    #[async_trait]
    impl Provider for StubProvider {
        fn id(&self) -> &str {
            "stub"
        }
        async fn complete_ref(
            &self,
            _req: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            Ok(CompletionResult {
                text: r#"{"lessons": [{"lesson": "money is integer minor units",
                     "kind": "domain", "domains": []}],
                     "self_review": {"rating": 9}}"#
                    .into(),
                tool_calls: vec![],
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 1,
                    ..CompletionUsage::default()
                },
                model: "stub".into(),
                cost_usd: 0.0,
                finish_reason: None,
            })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".stella")).unwrap();
    let store = stella_store::Store::open(dir.path()).unwrap();
    let orphan = store
        .begin_execution("deck-pipeline", "unrelated turn", "stub", "stub")
        .unwrap();
    let mut memory = SessionMemory::open(dir.path(), false).unwrap();
    // Deliberately no `set_execution_id`.

    let report = memory
        .reflect_and_record(
            &StubProvider,
            "stub",
            &[msg(MessageRole::User, "convert the totals")],
            true,
            true,
            None,
            crate::memory::ReflectionPosture::default(),
        )
        .await;
    assert_eq!(report.recorded, 1, "lessons are unaffected");

    assert_eq!(
        store.self_review(orphan).unwrap(),
        None,
        "no rating was pinned to an unrelated execution"
    );
    assert_eq!(
        reflection_execution_ids(dir.path()),
        vec![None],
        "and the lesson is filed unattributed, as it was before"
    );
}

fn lessons_of(text: &str) -> Vec<crate::memory::ReflectionLesson> {
    lessons_with(text, &[])
}

fn lessons_with(text: &str, allowed: &[String]) -> Vec<crate::memory::ReflectionLesson> {
    match crate::memory::reflection::parse_lessons_checked(text, allowed) {
        crate::memory::reflection::ReflectionParse::Lessons(lessons) => lessons,
        crate::memory::reflection::ReflectionParse::Unreadable(excerpt) => {
            panic!("expected a readable lesson array, got unreadable: {excerpt}")
        }
    }
}

#[test]
fn reflection_gate_fires_on_tool_use_and_skips_tool_free_turns() {
    use stella_protocol::ToolCall;

    // A pure conversational turn — no tool calls — is not worth a
    // reflection model call (the common, cheap-to-skip case).
    let chat_only = vec![
        msg(MessageRole::User, "what does this crate do?"),
        msg(MessageRole::Assistant, "it is a terminal coding agent"),
    ];
    assert!(!turn_warrants_reflection(&chat_only));

    // A turn where the assistant called a tool DID work worth mining.
    let mut worked = msg(MessageRole::Assistant, "reading the file first");
    worked.tool_calls = vec![ToolCall {
        call_id: "c1".into(),
        name: "read_file".into(),
        input: serde_json::json!({ "path": "src/main.rs" }),
    }];
    assert!(turn_warrants_reflection(&[worked]));

    // An empty turn slice (nothing happened) is trivially skippable.
    assert!(!turn_warrants_reflection(&[]));
}

/// End-to-end proof that the self-improvement write path works: a
/// reflection model call returning lessons must land them in BOTH the
/// mining log (`.stella/private/reflections.jsonl`) and the recallable context
/// store. Uses a stub provider so the assertion is deterministic (the
/// live model legitimately returns `[]` for trivial turns).
#[tokio::test]
async fn reflect_and_record_writes_lessons_to_log_and_store() {
    use async_trait::async_trait;
    use stella_protocol::{
        AgentEvent, CompletionRequestRef, CompletionResult, CompletionUsage, Provider,
        ProviderError,
    };

    struct StubProvider;
    #[async_trait]
    impl Provider for StubProvider {
        fn id(&self) -> &str {
            "stub"
        }
        async fn complete_ref(
            &self,
            _req: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            Ok(CompletionResult {
                text: r#"[{"lesson": "prefer withTenantDb over raw db()", "domains": []}]"#.into(),
                tool_calls: vec![],
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 1,
                    ..CompletionUsage::default()
                },
                model: "stub".into(),
                cost_usd: 0.0,
                finish_reason: None,
            })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".stella")).unwrap();
    let mut memory =
        SessionMemory::open(dir.path(), false).expect("open session memory in temp workspace");

    let transcript = vec![
        msg(MessageRole::User, "fix the tenancy leak"),
        msg(MessageRole::Assistant, "swapped db() for withTenantDb"),
    ];
    let report = memory
        .reflect_and_record(
            &StubProvider,
            "stub",
            &transcript,
            true,
            true,
            None,
            crate::memory::ReflectionPosture::default(),
        )
        .await;

    assert_eq!(report.recorded, 1, "the lesson was stored");
    assert!(report.model_error.is_none());
    assert!(report.events.iter().any(|event| matches!(
        event,
        AgentEvent::StepUsage {
            role: stella_protocol::ModelCallRole::Reflection,
            ..
        }
    )));

    // The mining log now carries the lesson, one JSON object per line.
    let log = std::fs::read_to_string(dir.path().join(".stella/private/reflections.jsonl"))
        .expect("reflections.jsonl was written");
    assert!(
        log.contains("withTenantDb"),
        "the lesson reached the mining log: {log}"
    );

    // And the durable `reflections` table mirrors it — the surface the
    // observatory panel, the JSON export, and the prune carve-out actually
    // read (the jsonl is only the mining log).
    let store = stella_store::Store::open(dir.path()).expect("open store.db");
    let export = store.export_all_json().expect("export store tables");
    let (_, reflections) = export
        .iter()
        .find(|(name, _)| *name == "reflections")
        .expect("reflections table exported");
    assert!(
        reflections.contains("withTenantDb"),
        "the lesson reached the store's reflections table: {reflections}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir.path().join(".stella/private")), 0o700);
        assert_eq!(
            mode(&dir.path().join(".stella/private/reflections.jsonl")),
            0o600
        );
    }
}

/// #2175 witness: an unreadable reflection response leaves a durable record,
/// and a legitimately empty one does not.
///
/// The two must stay distinguishable — that is the entire reason
/// `ReflectionParse::Unreadable` exists. Before this, both produced
/// `recorded: 0` with nothing in the store, the reflection execution closed
/// `outcome = 'completed'` (the model call DID complete; only the parse
/// failed), and the only trace of the failure was an `eprintln!`. An operator
/// looking at the Improve tab a week later could not tell "nothing to learn
/// yet" from "the learning loop has been broken for nine days".
#[tokio::test]
async fn an_unreadable_reflection_is_recorded_and_an_empty_one_is_not() {
    use async_trait::async_trait;
    use stella_protocol::{
        CompletionRequestRef, CompletionResult, CompletionUsage, Provider, ProviderError,
    };

    /// Answers with whatever text it was built with.
    struct Scripted(&'static str);
    #[async_trait]
    impl Provider for Scripted {
        fn id(&self) -> &str {
            "scripted"
        }
        async fn complete_ref(
            &self,
            _req: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            Ok(CompletionResult {
                text: self.0.into(),
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

    // The shape a truncated reasoning model actually produces: narration, cut
    // off before the JSON. It is not an empty lessons array, and recording it
    // as one is the defect.
    let unreadable = "Let me analyze this turn step by step. First, I should";
    // The shape a model that read the prompt and had nothing to add produces.
    let empty = r#"{"lessons": []}"#;

    for (response, expect_recorded) in [(unreadable, true), (empty, false)] {
        let dir = tempfile::tempdir().expect("workspace");
        std::fs::create_dir_all(dir.path().join(".stella")).expect("workspace");
        let store = stella_store::Store::open(dir.path()).expect("store");
        let execution = store
            .begin_execution("reflection", "convert the totals", "scripted", "scripted")
            .expect("execution");
        let mut memory = SessionMemory::open(dir.path(), false).expect("memory");
        memory.set_execution_id(execution);

        let report = memory
            .reflect_and_record(
                &Scripted(response),
                "scripted",
                &[msg(MessageRole::User, "convert the totals")],
                true,
                true,
                None,
                crate::memory::ReflectionPosture::default(),
            )
            .await;
        assert_eq!(report.recorded, 0, "neither response carries a lesson");

        let recorded = store
            .reflection_parse_failure(execution)
            .expect("query the parse-failure column");
        if expect_recorded {
            assert_eq!(
                recorded.flatten().as_deref(),
                Some(unreadable),
                "an unreadable response must leave the excerpt behind — \
                 stderr is not an artifact"
            );
            assert!(
                report.model_error.is_some(),
                "and it must still be reported to the caller in-band"
            );
        } else {
            assert!(
                recorded.flatten().is_none(),
                "a turn with genuinely nothing to learn is not a broken \
                 learning loop, and marking it as one would make every healthy \
                 workspace look starved"
            );
            assert!(report.model_error.is_none());
        }
    }
}

#[tokio::test]
async fn reflection_preserves_settled_cost_when_budget_rejects_model_output() {
    use async_trait::async_trait;
    use stella_protocol::{
        AgentEvent, CompletionRequestRef, CompletionResult, CompletionUsage, Provider,
        ProviderError,
    };

    struct PaidReflection;
    #[async_trait]
    impl Provider for PaidReflection {
        fn id(&self) -> &str {
            "paid-reflection"
        }

        async fn complete_ref(
            &self,
            _request: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            Ok(CompletionResult {
                text: r#"[{"lesson":"must not apply","domains":[]}]"#.into(),
                tool_calls: Vec::new(),
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 8,
                    output_tokens: 2,
                    ..CompletionUsage::default()
                },
                model: "paid-reflection-model".into(),
                cost_usd: 0.02,
                finish_reason: None,
            })
        }
    }

    let dir = tempfile::tempdir().expect("root");
    let mut memory = SessionMemory::open(dir.path(), false).expect("memory");
    let report = memory
        .reflect_and_record(
            &PaidReflection,
            "paid-reflection-model",
            &[msg(MessageRole::User, "worked")],
            true,
            true,
            Some(0.001),
            crate::memory::ReflectionPosture::default(),
        )
        .await;
    assert_eq!(report.recorded, 0);
    assert_eq!(report.cost_usd, 0.02);
    assert!(report.model_error.is_some());
    assert!(report.events.iter().any(|event| matches!(
        event,
        AgentEvent::StepUsage {
            role: stella_protocol::ModelCallRole::Reflection,
            cost_usd,
            ..
        } if (*cost_usd - 0.02).abs() < f64::EPSILON
    )));
}

/// A lesson carries the session's task boundary, not a per-turn fallback.
///
/// Governance promotes only after evidence spans three *distinct tasks*. With
/// no boundary the count fell back to `turn:<timestamp>`, which miscounts in
/// both directions: three lessons from one reflection call share a timestamp
/// and read as one task, while three turns on one task read as three.
#[test]
fn lessons_are_stamped_with_the_sessions_task_boundary() {
    let dir = tempfile::tempdir().expect("workspace");
    let mut memory = SessionMemory::open(dir.path(), false).expect("memory opens");

    let default_id = memory.task_id_for_test().to_string();
    assert!(
        default_id.starts_with("session:"),
        "the default boundary is session-scoped, got {default_id:?}"
    );

    memory.set_task_id("proving-ground:eval-03-interest");
    assert_eq!(memory.task_id_for_test(), "proving-ground:eval-03-interest");
}

/// A process note is written into the deferred band; a domain fact is not.
///
/// This is the wiring assertion, and it is deliberately about the *store*, not
/// about `LessonKind`. An earlier version of this test compared
/// `LessonKind::Domain.recall_rank()` against `LessonKind::Process.recall_rank()`
/// — which restates the function body and passes no matter what the recall path
/// does. The distinction only means something once it survives the write, so
/// that is what is asserted here: the tier that comes back off the node row.
///
/// The ordering this tier buys is proven end-to-end against a real budget in
/// `stella-context`'s `a_deferred_memory_loses_the_last_slot_to_a_normal_one`.
#[tokio::test]
async fn a_process_lesson_is_stored_in_the_deferred_recall_band() {
    use crate::memory::LessonKind;
    use stella_context::RecallTier;

    assert_eq!(LessonKind::Domain.recall_tier(), RecallTier::Normal);
    assert_eq!(LessonKind::Process.recall_tier(), RecallTier::Deferred);
    assert_eq!(
        LessonKind::default(),
        LessonKind::Process,
        "unlabelled lessons are not promoted to facts by accident"
    );

    let dir = tempfile::tempdir().expect("workspace");
    let memory = SessionMemory::open(dir.path(), false).expect("memory opens");
    memory
        .store
        .upsert(ContextDelta {
            memories: vec![
                MemoryInput::reflection("money is integer minor units", Vec::<String>::new())
                    .with_recall_tier(LessonKind::Domain.recall_tier()),
                MemoryInput::reflection("the agent should not retry blindly", Vec::<String>::new())
                    .with_recall_tier(LessonKind::Process.recall_tier()),
            ],
            ..ContextDelta::default()
        })
        .await
        .expect("memories land");

    let tiers: std::collections::HashMap<String, RecallTier> = memory
        .store
        .memory_nodes()
        .expect("memory nodes")
        .into_iter()
        .map(|node| (node.content.clone(), node.recall_tier))
        .collect();
    assert_eq!(
        tiers.get("money is integer minor units"),
        Some(&RecallTier::Normal),
        "a durable fact competes on rank like any other memory"
    );
    assert_eq!(
        tiers.get("the agent should not retry blindly"),
        Some(&RecallTier::Deferred),
        "a note about the turn yields first when the budget binds"
    );
}

/// The wire format tolerates a lesson written before `kind` existed.
#[test]
fn a_lesson_logged_before_kind_existed_still_parses() {
    let line = r#"{"lesson":"registry.py is the command registry","domains":[],"occurred_at":7}"#;
    let parsed: ReflectionLesson = serde_json::from_str(line).expect("legacy line parses");
    assert_eq!(parsed.kind, crate::memory::LessonKind::Process);
    assert!(parsed.task_id.is_empty());
}
