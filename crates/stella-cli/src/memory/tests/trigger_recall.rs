//! **The witness for #2459.** A lesson's `trigger` decides what recall returns.
//!
//! Reflection has asked for a trigger on every lesson — the condition a future
//! task has to satisfy for it to be worth reading — since the counterfactual
//! prompt landed, and nothing consulted it. A lesson that stated exactly when it
//! applied was retrieved by the same embedding guess as one that did not.
//!
//! The experiment is built so that **only** the trigger can produce the result,
//! and it is built *against* the change rather than for it. Each lesson's body
//! is given a weak lexical tie to the goal the *other* lesson is meant to win,
//! and each trigger a strong tie to its own. So the bodies alone rank the pair
//! backwards, and a run in which the right lesson wins is a run in which
//! something other than the body decided. There is no arrangement of body-only
//! scores that passes this by luck — which is what makes it a witness rather
//! than a plausible assertion.
//!
//! Checked, not assumed. Reverting the one call that folds the trigger in
//! (`learning.rs`, `recall_text` → the bare lesson) fails both tests below, and
//! fails them the predicted way: the resize goal comes back holding *the ledger
//! lesson*, because "resize" appears in that lesson's body. The third test —
//! the restatement band — passes on both sides, which is the point of it.
//!
//! Everything reaches the store through `reflect_and_record`, the production
//! write path, so what is pinned is the wiring and not a helper.

use async_trait::async_trait;
use stella_protocol::{
    CompletionRequestRef, CompletionResult, CompletionUsage, Provider, ProviderError,
};

use super::msg;
use crate::memory::*;

/// The two goals, and the lesson each one should surface. Held as constants so
/// the crossing is visible in one place: `RESIZE_GOAL` names the *body* of the
/// ledger lesson and the *trigger* of the money lesson.
const RESIZE_GOAL: &str = "the terminal resize path drops a redraw";
const RETRY_GOAL: &str = "the provider retry backoff never settles";

/// Answers reflection with two lessons whose bodies and triggers point at
/// opposite goals.
///
/// The bodies are unrelated facts — 0 shared tokens, so both clear
/// `partition_known`'s restatement band and both reach the store. Each carries
/// one weak nod to the wrong goal (`retry` in the money lesson, `resize` in the
/// ledger lesson) so that body-only ranking has a definite, wrong answer rather
/// than a coin flip.
struct CrossedTriggers;

#[async_trait]
impl Provider for CrossedTriggers {
    fn id(&self) -> &str {
        "crossed"
    }

    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        Ok(CompletionResult {
            text: r#"{"lessons": [
                {"lesson": "money is stored in integer minor units, so a retry that re-reads a total must not divide",
                 "trigger": "the terminal resize path drops a redraw",
                 "saves": "an afternoon spent on rounding drift",
                 "kind": "domain", "domains": []},
                {"lesson": "the spend ledger is append-only, and a resize of the window never rewrites a row",
                 "trigger": "the provider retry backoff never settles",
                 "saves": "a corrupted ledger",
                 "kind": "domain", "domains": []}
            ]}"#
            .into(),
            tool_calls: vec![],
            usage: CompletionUsage {
                reported: true,
                input_tokens: 1,
                ..CompletionUsage::default()
            },
            model: "crossed".into(),
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// Run one scripted reflection turn against a fresh workspace and hand back the
/// session, so both goals below query one store.
async fn session_with_crossed_lessons() -> (tempfile::TempDir, SessionMemory) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".stella")).expect("workspace");
    let mut memory = SessionMemory::open(dir.path(), false).expect("session memory");

    let transcript = vec![
        msg(MessageRole::User, "reconcile the spend ledger"),
        msg(MessageRole::Assistant, "reconciled"),
    ];
    let report = memory
        .reflect_and_record(
            &CrossedTriggers,
            "crossed",
            crate::memory::TurnEvidence::from_transcript(&transcript, true),
            true,
            None,
            crate::memory::ReflectionPosture::default(),
        )
        .await;
    assert_eq!(
        report.recorded, 2,
        "both lessons must reach the store — they share no tokens, so neither \
         is a restatement of the other: {report:?}"
    );
    (dir, memory)
}

/// The text of the highest-ranked recalled memory for `goal`.
async fn top_frame(memory: &SessionMemory, goal: &str) -> String {
    let recall = memory
        .recalled_frames_reporting(goal, |_| {})
        .await
        .frames
        .into_iter()
        .next()
        .expect("recall returned at least one frame");
    recall.content
}

/// **The witness.** Each goal surfaces the lesson whose *trigger* it satisfies,
/// not the one whose *body* it echoes.
#[tokio::test]
async fn a_goal_recalls_the_lesson_whose_trigger_it_satisfies() {
    let (_dir, memory) = session_with_crossed_lessons().await;

    let for_resize = top_frame(&memory, RESIZE_GOAL).await;
    assert!(
        for_resize.contains("minor units"),
        "a task about the resize path must surface the lesson whose trigger \
         names that path — even though the OTHER lesson's body is the one that \
         says `resize`. Before #2459 the trigger was recorded and never read, \
         so the body's word won and this returned the ledger lesson: {for_resize}"
    );

    let for_retry = top_frame(&memory, RETRY_GOAL).await;
    assert!(
        for_retry.contains("append-only"),
        "and symmetrically: the retry goal must surface the lesson triggered on \
         retry backoff, not the one whose body happens to say `retry`. Both \
         halves are asserted because one alone could be satisfied by a store \
         that simply ranks the same memory first for everything: {for_retry}"
    );

    assert_ne!(
        for_resize, for_retry,
        "two tasks satisfying different triggers must not be handed the same \
         memory — that is the whole claim, stated without reference to which \
         lesson is which"
    );
}

/// The trigger reaches the prompt, not just the ranking.
///
/// Folding is what puts it in the embedded body (`ContextStore::upsert` keys a
/// vector by `sha256(content)`, so a field outside `content` has no vector), and
/// the same fold is what lets a recalled memory announce the condition it holds
/// under instead of arriving as a bare assertion.
#[tokio::test]
async fn a_recalled_memory_states_the_condition_it_applies_under() {
    let (_dir, memory) = session_with_crossed_lessons().await;
    let block = memory
        .recall_block(RESIZE_GOAL)
        .await
        .expect("the goal recalls something");

    assert!(
        block.contains("(applies when: the terminal resize path drops a redraw)"),
        "the recalled-context block must carry the lesson's applicability, so \
         the model can judge whether it holds here rather than guessing: {block}"
    );
}

/// The restatement band is unmoved by the fold — the property #2459 makes
/// conditional on this change.
///
/// A second turn re-learning the first turn's fact, in different words and
/// under a *different* trigger, must still be diverted as a restatement. It is
/// the same fact; the trigger records only when it was noticed. Storing it
/// twice would spend two of five recall frames restating one thing, which is
/// exactly the 61%-restatement store #2358 measured.
#[tokio::test]
async fn a_paraphrase_under_a_new_trigger_is_still_a_restatement() {
    let (_dir, mut memory) = session_with_crossed_lessons().await;

    struct Paraphrase;
    #[async_trait]
    impl Provider for Paraphrase {
        fn id(&self) -> &str {
            "paraphrase"
        }
        async fn complete_ref(
            &self,
            _req: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            Ok(CompletionResult {
                // Every token of the stored ledger lesson bar one, so the pair
                // is comfortably over the 0.5 Jaccard bar on bodies alone —
                // and a trigger that shares nothing with the stored one, which
                // is what would drag a composed comparison under the bar.
                text: r#"{"lessons": [
                    {"lesson": "the spend ledger is append-only and a resize of the window never rewrites a row",
                     "trigger": "a task prunes telemetry rows from store.db",
                     "saves": "a corrupted ledger", "kind": "domain", "domains": []}
                ]}"#
                .into(),
                tool_calls: vec![],
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 1,
                    ..CompletionUsage::default()
                },
                model: "paraphrase".into(),
                cost_usd: 0.0,
                finish_reason: None,
            })
        }
    }

    let before = memory.store.memory_nodes().expect("nodes readable").len();
    let transcript = vec![
        msg(MessageRole::User, "prune old telemetry"),
        msg(MessageRole::Assistant, "pruned"),
    ];
    let report = memory
        .reflect_and_record(
            &Paraphrase,
            "paraphrase",
            crate::memory::TurnEvidence::from_transcript(&transcript, true),
            true,
            None,
            crate::memory::ReflectionPosture::default(),
        )
        .await;
    assert!(
        report.model_error.is_none(),
        "the scripted turn must reach the split, not fail before it: {report:?}"
    );
    let after = memory.store.memory_nodes().expect("nodes readable").len();

    assert_eq!(
        after, before,
        "the band compares bodies to bodies on both sides; a comparison that \
         let the trigger in would read this paraphrase as novel and store it, \
         which is the silent flood #2459 required measuring for"
    );
}
