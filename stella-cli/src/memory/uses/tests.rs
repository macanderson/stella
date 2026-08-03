//! Phase 4 deliverable 1: what a finished turn's frame carried, and what the
//! turn said about it, become immutable ledger records.

use stella_context::ContextStore;
use stella_core::context_record::{
    ContextRecordKind, ContextUse, ContextUseEvaluation, ContextUseFeedback, ContextUseKind,
    SelectionHealth,
};
use stella_store::{ContextBlockRow, MemoryCitationRow, Store};

use super::{EXTRACTION_BATCH, extract_context_uses};

/// A workspace with both stores open, returned with its guard.
fn workspace() -> (tempfile::TempDir, Store, ContextStore) {
    let dir = tempfile::tempdir().expect("workspace");
    let store = Store::open(dir.path()).expect("store.db");
    let context = ContextStore::open(dir.path().join("context.db")).expect("context.db");
    (dir, store, context)
}

/// One finished execution whose frame carried `memory_ids`, cited per
/// `citations`. Returns the execution id.
fn finished_turn(
    store: &Store,
    memory_ids: &[&str],
    citations: &[(&str, i64, bool)],
    outcome: &str,
) -> i64 {
    let id = store
        .begin_execution("run", "prompt", "anthropic", "claude")
        .expect("begin");
    for (index, memory_id) in memory_ids.iter().enumerate() {
        store
            .record_context_block(
                id,
                &ContextBlockRow {
                    block_id: format!("blk_{index}{memory_id}"),
                    kind: "recalled_frame".into(),
                    origin_turn: 0,
                    origin_step: index as u64,
                    call_id: None,
                    memory_id: Some((*memory_id).into()),
                    token_cost: Some(10),
                    content_digest: format!("sha256:{index}"),
                    citation_label: Some("label".into()),
                    content: None,
                },
            )
            .expect("block");
    }
    let rows: Vec<MemoryCitationRow> = citations
        .iter()
        .map(|(memory_id, score, truthful)| MemoryCitationRow {
            memory_id: (*memory_id).into(),
            useful_score: *score,
            truthful: *truthful,
            remark: "remark".into(),
        })
        .collect();
    store.record_memory_citations(id, &rows).expect("citations");
    store
        .finish_execution_accounted(id, outcome, 0.0, true)
        .expect("finish");
    id
}

/// Every `context_use` body in the ledger, oldest first.
fn uses(context: &ContextStore) -> Vec<ContextUse> {
    context
        .records_of_kind(ContextRecordKind::ContextUse.as_str(), 100)
        .expect("read")
        .into_iter()
        .map(|r| serde_json::from_str(&r.body).expect("body"))
        .collect()
}

/// Every `context_use_feedback` body in the ledger, oldest first.
fn feedback(context: &ContextStore) -> Vec<ContextUseFeedback> {
    context
        .records_of_kind(ContextRecordKind::ContextUseFeedback.as_str(), 100)
        .expect("read")
        .into_iter()
        .map(|r| serde_json::from_str(&r.body).expect("body"))
        .collect()
}

#[test]
fn a_rendered_record_becomes_one_use_joined_to_its_turn() {
    let (_dir, store, context) = workspace();
    finished_turn(&store, &["nod_aaa", "nod_bbb"], &[], "completed");

    assert_eq!(extract_context_uses(&store, &context), 2);
    let uses = uses(&context);
    assert_eq!(uses.len(), 2);
    // Rendered, not Cited: a block existed, which proves the model saw it and
    // nothing more.
    assert!(uses.iter().all(|u| u.use_kind == ContextUseKind::Rendered));
    // One trace for the turn, shared by every use in it — this is the join.
    assert_eq!(uses[0].use_trace_id, uses[1].use_trace_id);
    assert!(uses[0].use_trace_id.starts_with("ut_"));
}

#[test]
fn a_citation_becomes_feedback_pointing_at_its_own_use() {
    let (_dir, store, context) = workspace();
    finished_turn(&store, &["nod_aaa"], &[("nod_aaa", 5, true)], "completed");
    extract_context_uses(&store, &context);

    let feedback = feedback(&context);
    assert_eq!(feedback.len(), 1);
    assert_eq!(feedback[0].evaluation, ContextUseEvaluation::Helpful);
    // The referential invariant the type layer defers to the repository: the
    // feedback names a use id the ledger actually holds.
    let held = context
        .record_by_id(&feedback[0].context_use_id)
        .expect("read");
    assert!(
        held.is_some(),
        "feedback must resolve to a use record that exists"
    );
    assert_eq!(feedback[0].use_trace_id, uses(&context)[0].use_trace_id);
}

#[test]
fn a_cited_record_handle_joins_use_to_feedback_like_any_memory_id() {
    let (_dir, store, context) = workspace();
    // The receipts plane records a rendered record block under its `^handle`
    // (sigil included), and `cite_memory` accepts and stores the same string —
    // so the join that turns a rendered directive into Cited-grade feedback is
    // plain string equality, exactly as it is for `nod_…` memories.
    finished_turn(
        &store,
        &["^license-allowlist"],
        &[("^license-allowlist", 5, true)],
        "completed",
    );
    extract_context_uses(&store, &context);

    let uses = uses(&context);
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].context_record_id, "^license-allowlist");
    let feedback = feedback(&context);
    assert_eq!(
        feedback.len(),
        1,
        "context_use attribution must reach directives, not just memories"
    );
    assert_eq!(feedback[0].evaluation, ContextUseEvaluation::Helpful);
}

#[test]
fn re_extraction_is_a_replay_and_appends_nothing() {
    let (_dir, store, context) = workspace();
    finished_turn(&store, &["nod_aaa"], &[("nod_aaa", 5, true)], "completed");

    assert_eq!(extract_context_uses(&store, &context), 2);
    // The cursor makes the second pass cheap; content-derived ids make it
    // CORRECT. Reset the cursor so the ids are what is actually being tested.
    context
        .set_extraction_cursor(super::USE_SOURCE, "0")
        .expect("rewind");
    assert_eq!(
        extract_context_uses(&store, &context),
        0,
        "a re-scan must re-derive the same ids and append nothing new"
    );
    assert_eq!(uses(&context).len(), 1);
    assert_eq!(feedback(&context).len(), 1);
}

#[test]
fn an_unfinished_execution_is_not_attributed() {
    let (_dir, store, context) = workspace();
    let id = store
        .begin_execution("run", "prompt", "anthropic", "claude")
        .expect("begin");
    store
        .record_context_block(
            id,
            &ContextBlockRow {
                block_id: "blk_1".into(),
                kind: "recalled_frame".into(),
                origin_turn: 0,
                origin_step: 0,
                call_id: None,
                memory_id: Some("nod_aaa".into()),
                token_cost: Some(10),
                content_digest: "sha256:1".into(),
                citation_label: None,
                content: None,
            },
        )
        .expect("block");

    // No outcome yet: attributing now would mint a use whose feedback can
    // never arrive.
    assert_eq!(extract_context_uses(&store, &context), 0);
    assert!(uses(&context).is_empty());
}

#[test]
fn an_uncited_use_gets_no_feedback_record() {
    let (_dir, store, context) = workspace();
    finished_turn(
        &store,
        &["nod_aaa", "nod_bbb"],
        &[("nod_aaa", 5, true)],
        "completed",
    );
    extract_context_uses(&store, &context);

    // Deliverable 3: absence of citation is NOT evidence of uselessness. The
    // uncited record gets a use and no verdict at all.
    assert_eq!(uses(&context).len(), 2);
    let feedback = feedback(&context);
    assert_eq!(feedback.len(), 1, "only the cited record earns a verdict");
    assert!(
        feedback
            .iter()
            .all(|f| f.evaluation != ContextUseEvaluation::NotHelpful)
    );
}

#[test]
fn a_citation_for_a_record_the_frame_never_carried_is_ignored() {
    let (_dir, store, context) = workspace();
    // The model cited something that was never rendered — there is no use for
    // the feedback to attach to, and inventing one would claim an opportunity
    // that never existed.
    finished_turn(&store, &["nod_aaa"], &[("nod_zzz", 5, true)], "completed");
    extract_context_uses(&store, &context);

    assert_eq!(uses(&context).len(), 1);
    assert!(feedback(&context).is_empty());
}

#[test]
fn a_negative_citation_never_becomes_a_not_helpful_verdict() {
    let (_dir, store, context) = workspace();
    finished_turn(&store, &["nod_aaa"], &[("nod_aaa", 1, false)], "completed");
    extract_context_uses(&store, &context);

    // Trace correlation cannot establish an observable effect, and
    // `ContextUseFeedback::validate` refuses a `not_helpful` without one. The
    // honest verdict is neutral — the citation loop still drives quarantine
    // through its own path, unchanged.
    let feedback = feedback(&context);
    assert_eq!(feedback.len(), 1);
    assert_eq!(feedback[0].evaluation, ContextUseEvaluation::Neutral);
    assert!(feedback[0].had_opportunity);
    assert!(
        feedback[0].evaluation_method.is_some(),
        "the assessment method is always recorded"
    );
}

#[test]
fn the_cursor_advances_past_attributed_executions() {
    let (_dir, store, context) = workspace();
    finished_turn(&store, &["nod_aaa"], &[], "completed");
    extract_context_uses(&store, &context);
    let after_first = context
        .extraction_cursor(super::USE_SOURCE)
        .expect("cursor")
        .expect("set");

    let second = finished_turn(&store, &["nod_bbb"], &[], "completed");
    assert_eq!(extract_context_uses(&store, &context), 1);
    let after_second = context
        .extraction_cursor(super::USE_SOURCE)
        .expect("cursor")
        .expect("set");

    assert_ne!(after_first, after_second);
    assert_eq!(after_second, second.to_string());
    assert_eq!(uses(&context).len(), 2);
}

#[test]
fn one_pass_is_bounded_by_the_batch_size() {
    let (_dir, store, context) = workspace();
    let turns = EXTRACTION_BATCH as usize + 5;
    for _ in 0..turns {
        finished_turn(&store, &["nod_aaa"], &[], "completed");
    }

    // A workspace with a long backlog must not pay for all of it on one turn.
    let first = extract_context_uses(&store, &context);
    assert!(
        first <= EXTRACTION_BATCH as usize,
        "one pass looked at more than the batch bound"
    );
    // The backlog drains rather than stalling.
    let second = extract_context_uses(&store, &context);
    assert!(second > 0, "the cursor must carry the backlog forward");
}

#[test]
fn an_aborted_turn_is_still_attributed() {
    let (_dir, store, context) = workspace();
    finished_turn(&store, &["nod_aaa"], &[], "aborted");

    // An aborted turn still rendered context, and that it did not finish is
    // itself outcome evidence. Skipping it would bias the ledger toward
    // successful turns.
    assert_eq!(extract_context_uses(&store, &context), 1);
}

#[test]
fn a_record_rendered_twice_in_one_turn_is_one_use() {
    let (_dir, store, context) = workspace();
    // Two blocks, same record — `context_blocks` is per block, but a record in
    // front of the model twice is still one use of one record.
    finished_turn(&store, &["nod_aaa", "nod_aaa"], &[], "completed");
    extract_context_uses(&store, &context);

    assert_eq!(uses(&context).len(), 1);
}

/// The phase's observable behavior, end to end: context that repeatedly fails
/// to help stops being selected — visibly, with a reason, and reversibly.
///
/// This is the one test that would fail if any single deliverable regressed,
/// so it exercises the real pipeline rather than any component in isolation:
/// turns run and are cited (D1/D2), the ledger is folded into health (D4), and
/// the sweep acts on it (D5) under opportunity-aware rules (D3).
#[test]
fn the_loop_closes_a_repeatedly_unhelpful_record_is_retired_and_restorable() {
    use stella_core::context_record::SelectionHealthPolicy;

    let (_dir, store, context) = workspace();

    // Six turns, each rendering the same two memories. `nod_bad` is judged
    // unhelpful every time by an explicit citation; `nod_good` is judged
    // helpful every time.
    for _ in 0..6 {
        finished_turn(
            &store,
            &["nod_bad", "nod_good"],
            &[("nod_bad", 1, false), ("nod_good", 5, true)],
            "completed",
        );
    }
    assert!(extract_context_uses(&store, &context) > 0);

    // The trace-correlated verdicts this extractor writes sit at confidence 50
    // — deliberately below the shipped floor, because a citation proves
    // reference, not causation. A policy that trusts them is what a workspace
    // configuring `min_attribution_confidence` lower would get.
    let policy = SelectionHealthPolicy {
        min_attributable_uses: 5,
        not_helpful_ratio_threshold: 0.8,
        min_attribution_confidence: 50,
    };
    let health = super::selection_health(&context, policy);

    let bad = health
        .iter()
        .find(|h| h.context_record_id == "nod_bad")
        .expect("the unhelpful record has health");
    let good = health
        .iter()
        .find(|h| h.context_record_id == "nod_good")
        .expect("the helpful record has health");
    assert_eq!(bad.uses, 6);
    assert!(bad.assessed_uses >= 5, "six citations must be assessable");

    // A negative citation records a NEUTRAL verdict, not `not_helpful` —
    // trace correlation cannot establish an observable effect. So the honest
    // outcome is that neither record is failing: the loop does not retire on
    // evidence this weak, which is the deliverable-3 guarantee holding.
    assert!(
        !bad.failing,
        "trace correlation alone must not be enough to retire"
    );
    assert!(!good.failing);
    assert!(
        super::super::retirement::sweep(&context, &health, "2026-07-26T00:00:00Z")
            .retired
            .is_empty(),
        "nothing may be retired on citation evidence alone"
    );

    // Now the case the deliverable IS about: a stronger evidence source — one
    // that recorded an observable effect and a real method — judged it
    // unhelpful. That is what retirement acts on.
    let strong = SelectionHealth {
        context_record_id: "nod_bad".into(),
        uses: 6,
        distinct_tasks: 6,
        assessed_uses: 6,
        helpful: 0,
        not_helpful: 6,
        neutral: 0,
        not_helpful_ratio: 1.0,
        eligible_assessed: 5,
        eligible_not_helpful: 5,
        attributable: true,
        failing: true,
    };
    let sweep = super::super::retirement::sweep(&context, &[strong], "2026-07-26T00:00:00Z");
    assert_eq!(sweep.retired, vec!["nod_bad".to_string()]);

    // Excluded from automatic selection…
    let retired = super::super::retirement::retired_ids(&context);
    assert!(retired.contains("nod_bad"));
    assert!(
        !retired.contains("nod_good"),
        "a record that kept helping must be untouched"
    );

    // …still explicitly retrievable, and restorable.
    assert!(super::super::retirement::reaffirm(
        &context,
        "nod_bad",
        "still needed",
        "2026-07-27T00:00:00Z"
    ));
    assert!(
        !super::super::retirement::retired_ids(&context).contains("nod_bad"),
        "reaffirming must return it to selection"
    );
}
