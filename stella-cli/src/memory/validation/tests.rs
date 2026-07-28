//! #753: deterministic validation is the first evidence source that can retire
//! a memory without a human — and the `agent_self_report` exclusion survives
//! it untouched.

use stella_context::{ContextDelta, ContextStore, MemoryInput};
use stella_core::context_record::{
    ContextEvaluationMethod, ContextRecordKind, ContextUseEvaluation, ContextUseFeedback,
    PromotionAction, PromotionActor, PromotionEventRecord,
};
use stella_store::{ContextBlockRow, Store};

use super::super::retirement;
use super::{record_vanished_verdicts, vanished_memories};

const AT: &str = "2026-07-27T00:00:00Z";

/// A workspace whose context store holds `memories`, with one live source file
/// at `stella-cli/src/live.rs` for anchors that should verify.
fn workspace_with(memories: &[&str]) -> (tempfile::TempDir, ContextStore) {
    let dir = tempfile::tempdir().expect("workspace");
    std::fs::create_dir_all(dir.path().join("stella-cli/src")).expect("mkdir");
    std::fs::write(dir.path().join("stella-cli/src/live.rs"), "pub fn x() {}").expect("write");
    let context = ContextStore::open(dir.path().join("context.db")).expect("context.db");
    let rt = tokio::runtime::Runtime::new().expect("rt");
    rt.block_on(async {
        let delta = ContextDelta {
            memories: memories
                .iter()
                .map(|m| MemoryInput::reflection(*m, Vec::<String>::new()))
                .collect(),
            ..Default::default()
        };
        context.upsert(delta).await.expect("upsert");
    });
    (dir, context)
}

/// The public id of the one memory whose content contains `needle`.
fn id_of(context: &ContextStore, needle: &str) -> String {
    context
        .recallable_nodes()
        .expect("nodes")
        .iter()
        .find(|n| n.content.contains(needle))
        .expect("memory present")
        .public_id
        .clone()
}

#[test]
fn only_a_fully_vanished_memory_is_flagged() {
    let (dir, context) = workspace_with(&[
        "the helper in stella-cli/src/live.rs is canonical",
        "deleted/gone.rs and also deleted/other.rs were removed",
        "partially: stella-cli/src/live.rs plus deleted/gone.rs",
        "no path anchors here at all",
    ]);

    let vanished = vanished_memories(&context, dir.path());
    assert_eq!(
        vanished.len(),
        1,
        "a live anchor or an absent anchor list must both disqualify"
    );
    assert_eq!(vanished[0].public_id, id_of(&context, "were removed"));
    assert_eq!(
        vanished[0].missing_paths,
        vec![
            "deleted/gone.rs".to_string(),
            "deleted/other.rs".to_string()
        ]
    );
}

/// The headline done-when: a memory whose anchors have all disappeared is
/// retired automatically, with a reason naming the missing paths, on a
/// workspace where nobody ran `stella memory retire`.
#[test]
fn a_vanished_memory_is_retired_with_the_missing_paths_in_the_reason() {
    let (dir, context) = workspace_with(&["deleted/gone.rs held the old adapter"]);
    let vanished = vanished_memories(&context, dir.path());

    let sweep = retirement::sweep_vanished(&context, &vanished, AT);
    let id = id_of(&context, "old adapter");
    assert_eq!(sweep.retired, vec![id.clone()]);
    assert!(retirement::retired_ids(&context).contains(&id));

    let standings = retirement::standings(&context);
    let reason = &standings.get(&id).expect("standing").reason;
    assert!(
        reason.contains("deleted/gone.rs"),
        "the reason must name the missing paths, got: {reason}"
    );

    // Steady state: the next scan re-finds the same vanished memory and must
    // neither duplicate the retirement nor report a refusal.
    let again = retirement::sweep_vanished(&context, &vanished, "2026-07-28T00:00:00Z");
    assert!(again.retired.is_empty());
    assert!(again.refused.is_empty());
}

#[test]
fn protected_memories_are_refused_and_reaffirm_restores() {
    let (dir, context) = workspace_with(&["deleted/gone.rs held the old adapter"]);
    let id = id_of(&context, "old adapter");
    let vanished = vanished_memories(&context, dir.path());

    // A person confirmed it: the loop may not touch it, and must say so.
    let event = PromotionEventRecord::new(
        &id,
        PromotionAction::Confirmed,
        PromotionActor::User,
        None,
        None,
        "user confirmed",
        AT,
    )
    .expect("event");
    let body = serde_json::to_string(&event).expect("body");
    context
        .append_record(stella_context::LedgerAppend {
            record_id: &event.record_id,
            lineage_id: &event.lineage_id,
            record_kind: ContextRecordKind::PromotionEvent.as_str(),
            record_hash: &event.record_hash,
            schema_version: stella_core::context_record::LIFECYCLE_SCHEMA_VERSION,
            body: &body,
            observed_at: &event.occurred_at,
            supersedes: None,
        })
        .expect("append");
    let sweep = retirement::sweep_vanished(&context, &vanished, "2026-07-28T00:00:00Z");
    assert!(sweep.retired.is_empty());
    assert_eq!(sweep.refused.len(), 1, "refusals are reported, not silent");

    // Without protection it retires — and reaffirming restores it.
    let (dir2, context2) = workspace_with(&["deleted/gone.rs held the old adapter"]);
    let id2 = id_of(&context2, "old adapter");
    let vanished2 = vanished_memories(&context2, dir2.path());
    retirement::sweep_vanished(&context2, &vanished2, AT);
    assert!(retirement::retired_ids(&context2).contains(&id2));
    retirement::reaffirm(&context2, &id2, "still relevant", "2026-07-29T00:00:00Z");
    assert!(!retirement::retired_ids(&context2).contains(&id2));
}

#[test]
fn the_verdict_is_deterministic_validation_attached_to_the_latest_use_once() {
    let (dir, context) = workspace_with(&["deleted/gone.rs held the old adapter"]);
    let id = id_of(&context, "old adapter");

    // One finished turn whose frame carried the memory, so the ledger holds a
    // use to attach the verdict to.
    let store = Store::open(dir.path()).expect("store.db");
    let execution = store
        .begin_execution("run", "prompt", "anthropic", "claude")
        .expect("begin");
    store
        .record_context_block(
            execution,
            &ContextBlockRow {
                block_id: "blk_0".into(),
                kind: "recalled_frame".into(),
                origin_turn: 0,
                origin_step: 0,
                call_id: None,
                memory_id: Some(id.clone()),
                token_cost: 10,
                content_digest: "sha256:0".into(),
                citation_label: Some("label".into()),
                content: None,
            },
        )
        .expect("block");
    store
        .finish_execution_accounted(execution, "completed", 0.0, true)
        .expect("finish");
    super::super::uses::extract_context_uses(&store, &context);

    let vanished = vanished_memories(&context, dir.path());
    assert_eq!(record_vanished_verdicts(&context, &vanished, AT), 1);

    let verdicts: Vec<ContextUseFeedback> = context
        .records_of_kind(ContextRecordKind::ContextUseFeedback.as_str(), 100)
        .expect("read")
        .into_iter()
        .filter_map(|r| serde_json::from_str(&r.body).ok())
        .collect();
    let verdict = verdicts
        .iter()
        .find(|f| {
            f.evaluation_method
                .as_ref()
                .is_some_and(|m| m.as_str() == ContextEvaluationMethod::DETERMINISTIC_VALIDATION)
        })
        .expect("a non-agent_self_report source writes verdicts");
    assert_eq!(verdict.evaluation, ContextUseEvaluation::NotHelpful);
    assert!(verdict.had_opportunity);
    assert_eq!(
        verdict.observable_effect_refs,
        vec!["path-missing:deleted/gone.rs".to_string()]
    );
    // Pruning-eligible — the whole point — while the tier itself is untouched:
    // the self-report exclusion must survive this issue.
    assert!(
        verdict
            .evaluation_method
            .as_ref()
            .expect("method")
            .is_pruning_eligible()
    );
    assert!(
        !ContextEvaluationMethod::new(ContextEvaluationMethod::AGENT_SELF_REPORT)
            .expect("method")
            .is_pruning_eligible(),
        "agent_self_report must remain excluded"
    );

    // Re-running the scan asserts the same fact; it must not manufacture a
    // second piece of evidence.
    assert_eq!(
        record_vanished_verdicts(&context, &vanished, "2026-07-28T00:00:00Z"),
        0
    );
}

#[test]
fn a_memory_with_no_recorded_use_gets_no_verdict_but_still_retires() {
    let (dir, context) = workspace_with(&["deleted/gone.rs held the old adapter"]);
    let vanished = vanished_memories(&context, dir.path());

    assert_eq!(
        record_vanished_verdicts(&context, &vanished, AT),
        0,
        "no use in the ledger means nothing to attach a verdict to"
    );
    let sweep = retirement::sweep_vanished(&context, &vanished, AT);
    assert_eq!(sweep.retired.len(), 1);
}
