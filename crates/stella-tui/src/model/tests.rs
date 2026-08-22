//! Unit tests for [`crate::model`].
//!
//! Split out of `model.rs` so the module drops back under the 1500-line
//! limit (#629) and can retire its baseline exemption entirely, rather
//! than raising the ceiling. Pure relocation: no test was changed, added,
//! or removed.

use super::*;
// `BudgetMode`, `MediaKind` and `PrStatus` are named here rather than reached
// through `use super::*`: they are used only to build entries, so they moved to
// `model::entry` with the types that hold them and `model.rs` no longer imports
// them (#4217). Naming them keeps the tests independent of what the fold
// happens to need.
use stella_protocol::{
    BudgetMode, ContextFrameRef, MediaArtifactRef, MediaJobState, MediaKind, ModelCallRole,
    PrStatus, ProviderShare, ToolCall, VerdictEvidence,
};

fn text(delta: &str) -> AgentEvent {
    AgentEvent::Text { text: delta.into() }
}

/// Witness for #1857's deck half: the park and the wake fold into typed
/// transcript entries carrying their own facts, rather than being
/// indistinguishable from model narration.
///
/// The distinction is the whole point of the wire pair — before it both
/// arrived as `AgentEvent::Text` and coalesced into whatever answer text
/// happened to precede them.
#[test]
fn a_parked_wait_folds_into_typed_entries_not_narration() {
    let mut model = SessionModel::new();
    model.apply(&text("checking CI"));
    model.apply(&AgentEvent::TurnParked {
        description: "CI for branch main settles".into(),
        poll_interval_secs: 5,
        deadline_secs: 600,
    });
    model.apply(&AgentEvent::TurnWoken {
        reason: "changed".into(),
        polls_used: 3,
    });

    // Three entries, not one coalesced blob: the park did not merge into
    // the answer text the way a synthetic Text delta would have.
    assert_eq!(model.transcript.len(), 3, "{:?}", model.transcript);
    match &model.transcript[1] {
        TranscriptEntry::Parked {
            description,
            poll_interval_secs,
            deadline_secs,
        } => {
            assert_eq!(description, "CI for branch main settles");
            assert_eq!(*poll_interval_secs, 5);
            assert_eq!(*deadline_secs, 600);
        }
        other => panic!("expected a typed park entry, got {other:?}"),
    }
    match &model.transcript[2] {
        TranscriptEntry::Woken { reason, polls_used } => {
            assert_eq!(reason, "changed");
            assert_eq!(*polls_used, 3);
        }
        other => panic!("expected a typed wake entry, got {other:?}"),
    }
}

/// The live half of #2007: alongside the scrollback rows, the fold carries
/// *whether a park is open right now* and what it is waiting on.
///
/// The transcript cannot answer that on its own — it is a log, so both a park
/// that is still running and one that woke an hour ago leave the same ⏳ row.
/// The chip needs current state, and this is the pure half of it (the clock is
/// stamped outside, in `deck::AgentEntry::parked_since_ms`).
#[test]
fn an_open_park_is_live_state_and_the_wake_closes_it() {
    let mut model = SessionModel::new();
    assert!(model.parked.is_none(), "a fresh model is not parked");

    model.apply(&AgentEvent::TurnParked {
        description: "CI for branch main settles".into(),
        poll_interval_secs: 30,
        deadline_secs: 1800,
    });
    let park = model.parked.as_ref().expect("the park is open");
    assert_eq!(park.description, "CI for branch main settles");
    assert_eq!(park.poll_interval_secs, 30);
    assert_eq!(park.deadline_secs, 1800, "the countdown's denominator");

    model.apply(&AgentEvent::TurnWoken {
        reason: "changed".into(),
        polls_used: 41,
    });
    assert!(model.parked.is_none(), "the wake closes the span");
    // …and the scrollback still reads as history afterwards.
    assert!(
        matches!(model.transcript.last(), Some(TranscriptEntry::Woken { .. })),
        "{:?}",
        model.transcript
    );
}

/// A park the turn was cancelled or soft-stopped out of never gets its
/// `TurnWoken` — `driver::waiting` returns early on both paths. Without a
/// close here the ⏳ chip would count up forever on a turn that is over.
#[test]
fn a_turn_that_ends_mid_park_closes_the_span_anyway() {
    for terminal in [
        AgentEvent::RunComplete {
            model: "m".into(),
            cost_usd: 0.0,
        },
        AgentEvent::Error {
            message: "cancelled".into(),
            retryable: false,
        },
    ] {
        let mut model = SessionModel::new();
        model.apply(&AgentEvent::TurnParked {
            description: "the deploy finishes".into(),
            poll_interval_secs: 10,
            deadline_secs: 600,
        });
        assert!(model.parked.is_some());
        model.apply(&terminal);
        assert!(
            model.parked.is_none(),
            "a turn ending mid-park must close the span: {terminal:?}"
        );
    }
}

/// A *retryable* error is a warning mid-flight, not the end of the turn, so it
/// must leave an open park alone — the same reading the plan rail and the
/// plan take of the identical event.
#[test]
fn a_retryable_error_does_not_close_an_open_park() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::TurnParked {
        description: "CI settles".into(),
        poll_interval_secs: 10,
        deadline_secs: 600,
    });
    model.apply(&AgentEvent::Error {
        message: "429".into(),
        retryable: true,
    });
    assert!(model.parked.is_some(), "the wait is still running");
}

#[test]
fn streaming_text_deltas_coalesce_into_one_entry() {
    let mut model = SessionModel::new();
    model.apply(&text("Hel"));
    model.apply(&text("lo, "));
    model.apply(&text("world"));
    assert_eq!(model.transcript.len(), 1);
    match &model.transcript[0] {
        TranscriptEntry::Text(s) => assert_eq!(s, "Hello, world"),
        other => panic!("expected coalesced text, got {other:?}"),
    }
}

#[test]
fn a_stage_between_text_deltas_breaks_coalescing() {
    let mut model = SessionModel::new();
    model.apply(&text("a"));
    model.apply(&AgentEvent::Stage {
        name: StageKind::Verify.into(),
        scope: stella_protocol::StageScope::Run,
    });
    model.apply(&text("b"));
    // text, stage, text
    assert_eq!(model.transcript.len(), 3);
    assert!(matches!(model.transcript[0], TranscriptEntry::Text(_)));
    assert!(matches!(model.transcript[1], TranscriptEntry::Stage { .. }));
    assert!(matches!(model.transcript[2], TranscriptEntry::Text(_)));
}

fn stage(kind: StageKind) -> AgentEvent {
    AgentEvent::Stage {
        name: kind.into(),
        scope: stella_protocol::StageScope::Run,
    }
}

/// Every `TurnOpening` the fold stamped, in transcript order.
fn openings(model: &SessionModel) -> Vec<&TurnOpening> {
    model
        .transcript
        .iter()
        .filter_map(|e| match e {
            TranscriptEntry::Stage { opens, .. } => opens.as_ref(),
            _ => None,
        })
        .collect()
}

/// SPEC 6.1's rule opens a **turn**, not a stage: the first boundary of a turn
/// carries it and the rest of that turn's stages do not.
///
/// The witness for the fold half of #4124. Before it every stage boundary was a
/// bare `Stage(name)` with nothing to open a rule *with* — no turn number, no
/// model, no budget — which is why the renderer written in #4123 had never been
/// called.
#[test]
fn only_the_first_stage_of_a_turn_opens_a_turn_rule() {
    let mut model = SessionModel::new();
    for kind in [StageKind::Triage, StageKind::Plan, StageKind::Execute] {
        model.apply(&stage(kind));
    }
    let opened = openings(&model);
    assert_eq!(opened.len(), 1, "one rule per turn, not one per stage");
    assert_eq!(opened[0].turn, 1);

    // …and the next turn opens its own, numbered by what has completed.
    model.apply(&AgentEvent::TurnComplete {
        model: "kimi-k3".into(),
        cost_usd: 0.11,
    });
    model.apply(&stage(StageKind::Execute));
    let opened = openings(&model);
    assert_eq!(opened.len(), 2);
    assert_eq!(opened[1].turn, 2, "the ordinal follows the closing rule");
    assert_eq!(
        opened[1].turn,
        model.turns_completed + 1,
        "the opening and closing rules must agree on the turn's number",
    );
}

/// One model call, for the opening-rule tests below.
fn manifest(role: ModelCallRole, call_seq: u64, model: &str) -> AgentEvent {
    AgentEvent::StepManifest {
        turn_instance: 0,
        step: 0,
        call_seq,
        role,
        provider: "openrouter".into(),
        model: model.into(),
        blocks: Vec::new(),
        effective_budget_tokens: 100_000,
        calibration_factor: 1.0,
        estimated_input_tokens: 40,
        compiled_frame: None,
    }
}

/// The opening rule states the budget the last `BudgetTick` reported and the
/// model of the turn's own first worker call — and states neither before
/// anything reported one. `None` is "nobody said", not `$0.00` and not the
/// configured default.
#[test]
fn an_opening_rule_carries_only_facts_the_fold_was_given() {
    let mut model = SessionModel::new();
    model.apply(&stage(StageKind::Execute));
    let first = openings(&model)[0].clone();
    assert_eq!(
        first.model, None,
        "no call has committed, so no model has answered"
    );
    assert_eq!(first.budget_usd, None, "no tick has armed a budget");

    // The two facts reach the rule by different routes, and the difference is
    // deliberate. A budget is armed before the turn opens, so it is *stamped*
    // when the boundary is pushed. A model is not known until a call commits,
    // which is strictly after that, so it is *back-filled* onto the same rule.
    model.apply(&manifest(ModelCallRole::Worker, 0, "kimi-k3"));
    assert_eq!(
        openings(&model)[0].model.as_deref(),
        Some("kimi-k3"),
        "the turn's first worker call did not reach its own rule"
    );
    assert_eq!(
        openings(&model)[0].budget_usd,
        None,
        "no tick armed a budget before this turn opened"
    );

    model.apply(&AgentEvent::TurnComplete {
        model: "kimi-k3".into(),
        cost_usd: 0.11,
    });
    model.apply(&AgentEvent::BudgetTick {
        spent_usd: 0.11,
        limit_usd: Some(0.60),
        mode: BudgetMode::Enforced,
        session_spent_usd: None,
        session_limit_usd: None,
        deadline_remaining_ms: None,
    });
    model.apply(&stage(StageKind::Execute));
    let second = openings(&model)[1].clone();
    assert_eq!(second.budget_usd, Some(0.60), "the armed budget is stamped");
    assert_eq!(
        second.model, None,
        "a settled TurnComplete is not evidence about the turn now opening — \
         naming its model here is the defect #4183 closed"
    );
}

/// #4183: the **first** turn names its model, which is the whole point.
///
/// `Hud::model`'s only writers are `TurnComplete` and `RunComplete`, both
/// terminal, so sourcing the rule from it left turn 1 permanently blank and
/// made every later turn name its predecessor's model. The manifest arrives
/// *before* its call commits, which is what makes it early enough to label the
/// turn it belongs to.
#[test]
fn the_first_turns_rule_names_the_model_that_is_answering_it() {
    let mut model = SessionModel::new();
    model.apply(&stage(StageKind::Execute));
    assert_eq!(
        openings(&model)[0].model,
        None,
        "nothing has committed a call yet"
    );

    model.apply(&manifest(ModelCallRole::Worker, 0, "kimi-k3"));

    assert_eq!(
        openings(&model)[0].model.as_deref(),
        Some("kimi-k3"),
        "the turn's own first worker call did not reach its opening rule"
    );
}

/// The trap the fold has to design around: a manifest is emitted for *every*
/// model call, and an auxiliary one is not the turn's answer.
///
/// An unfiltered fold would let the overflow summarizer's model label the turn
/// — a rule naming a model that never answered it, which is worse than the
/// blank it replaces. Both halves of the predicate are exercised: a wrong role
/// at the worker's `call_seq`, and the worker's own role at an auxiliary seq.
#[test]
fn an_auxiliary_call_never_supplies_the_opening_rules_model() {
    for (role, call_seq) in [
        (ModelCallRole::Summarization, 0),
        (ModelCallRole::Verdict, 0),
        // Legacy sessions recorded no role at all. A call this build cannot
        // identify must not name the turn either — eliding beats asserting a
        // routing decision nothing recorded.
        (ModelCallRole::Unknown, 0),
        // The worker's role riding an auxiliary seq is still not the engine's
        // own call for the step.
        (ModelCallRole::Worker, 1),
    ] {
        let mut model = SessionModel::new();
        model.apply(&stage(StageKind::Execute));
        model.apply(&manifest(role, call_seq, "cheap-summarizer"));
        assert_eq!(
            openings(&model)[0].model,
            None,
            "{role:?} at call_seq {call_seq} labelled the turn"
        );
    }
}

/// A later turn's call must not rewrite a settled rule, and a sub-agent's must
/// not overwrite the lead's.
///
/// Both fall out of the same stop: the back-fill walks to the *nearest* opened
/// turn and stops there whether or not it fills anything, so the first worker
/// call of each turn claims that turn's rule and nothing later can take it.
#[test]
fn a_later_call_cannot_rewrite_a_rule_another_turn_already_claimed() {
    let mut model = SessionModel::new();
    model.apply(&stage(StageKind::Execute));
    model.apply(&manifest(ModelCallRole::Worker, 0, "kimi-k3"));
    // A delegated child's calls carry the worker role too, and may run on a
    // different model.
    model.apply(&manifest(ModelCallRole::Worker, 0, "child-model"));
    assert_eq!(
        openings(&model)[0].model.as_deref(),
        Some("kimi-k3"),
        "a second worker call overwrote the turn's model"
    );

    model.apply(&AgentEvent::TurnComplete {
        model: "kimi-k3".into(),
        cost_usd: 0.11,
    });
    model.apply(&stage(StageKind::Execute));
    model.apply(&manifest(ModelCallRole::Worker, 0, "glm-5"));

    let opened = openings(&model);
    assert_eq!(
        opened[0].model.as_deref(),
        Some("kimi-k3"),
        "turn 1's settled rule was rewritten by turn 2's call"
    );
    assert_eq!(
        opened[1].model.as_deref(),
        Some("glm-5"),
        "turn 2's rule did not take its own turn's model"
    );
}

/// A turn that died never emits `TurnComplete`, so nothing else clears the
/// latch — and the turn after it would open with no rule at all, leaving its
/// events hanging under the dead turn's boundary.
#[test]
fn a_turn_that_died_still_lets_the_next_one_open() {
    let mut model = SessionModel::new();
    model.apply(&stage(StageKind::Execute));
    model.apply(&AgentEvent::Error {
        message: "provider refused".into(),
        retryable: false,
    });
    model.apply(&stage(StageKind::Execute));
    assert_eq!(openings(&model).len(), 2, "the next turn opened no rule");
}

fn delta(text: &str) -> AgentEvent {
    AgentEvent::TextDelta { delta: text.into() }
}

#[test]
fn text_deltas_accumulate_as_a_preview_the_authoritative_text_replaces() {
    let mut model = SessionModel::new();
    model.apply(&delta("Hel"));
    model.apply(&delta("lo!"));
    assert_eq!(model.streaming_text, "Hello!");
    assert!(
        model.transcript.is_empty(),
        "the preview is not a transcript entry"
    );

    // The step commits: bookkeeping events land between the last delta
    // and the authoritative Text (exactly the live wire order).
    model.apply(&AgentEvent::BudgetTick {
        spent_usd: 0.01,
        limit_usd: None,
        mode: BudgetMode::Observed,
        session_spent_usd: None,
        session_limit_usd: None,
        deadline_remaining_ms: None,
    });
    model.apply(&text("Hello!"));
    assert!(
        model.streaming_text.is_empty(),
        "the authoritative Text replaces the preview outright"
    );
    let texts: Vec<&String> = model
        .transcript
        .iter()
        .filter_map(|e| match e {
            TranscriptEntry::Text(s) => Some(s),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["Hello!"], "the answer appears exactly once");
}

#[test]
fn streaming_preview_clears_on_error_complete_and_a_new_prompt() {
    for terminal in [
        AgentEvent::Error {
            message: "aborted".into(),
            retryable: false,
        },
        AgentEvent::RunComplete {
            model: "glm".into(),
            cost_usd: 0.01,
        },
    ] {
        let mut model = SessionModel::new();
        model.apply(&delta("partial answ"));
        assert!(!model.streaming_text.is_empty());
        model.apply(&terminal);
        assert!(
            model.streaming_text.is_empty(),
            "an uncommitted preview must not outlive the turn: {terminal:?}"
        );
    }
    let mut model = SessionModel::new();
    model.apply(&delta("stale"));
    model.push_user_prompt("next question");
    assert!(model.streaming_text.is_empty());
}

#[test]
fn streaming_preview_is_middle_out_capped() {
    let mut model = SessionModel::new();
    for _ in 0..(OUTPUT_BUDGET / 4) {
        model.apply(&delta("abcdefgh"));
    }
    assert!(
        model.streaming_text.chars().count() <= OUTPUT_BUDGET,
        "the preview respects the render cap"
    );
    assert!(
        model.streaming_text.contains("truncated"),
        "middle-out elision marker present"
    );
}

#[test]
fn replaying_a_log_with_deltas_is_deterministic() {
    let log = vec![
        delta("Hel"),
        delta("lo"),
        AgentEvent::BudgetTick {
            spent_usd: 0.01,
            limit_usd: None,
            mode: BudgetMode::Observed,
            session_spent_usd: None,
            session_limit_usd: None,
            deadline_remaining_ms: None,
        },
        text("Hello"),
        AgentEvent::RunComplete {
            model: "glm".into(),
            cost_usd: 0.01,
        },
    ];
    let a = SessionModel::replay(&log);
    let b = SessionModel::replay(&log);
    assert_eq!(a, b);
    assert!(a.streaming_text.is_empty());
}

#[test]
fn budget_tick_folds_into_the_hud_gauge_but_never_the_transcript() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::BudgetTick {
        spent_usd: 0.42,
        limit_usd: Some(2.0),
        mode: BudgetMode::Enforced,
        session_spent_usd: None,
        session_limit_usd: None,
        deadline_remaining_ms: None,
    });
    assert_eq!(model.hud.spent_usd, 0.42);
    assert_eq!(model.hud.limit_usd, Some(2.0));
    assert_eq!(model.hud.budget_mode, Some(BudgetMode::Enforced));
    // A tick is a gauge reading, not an event. It fires after every model
    // call that spends, so admitting it to the transcript meant four or
    // five near-identical spend rows per turn — the exact noise the
    // composer's live cost cell and the single `✓ cost` line replaced.
    assert!(
        model.transcript.is_empty(),
        "a budget tick must not push a transcript row: {:?}",
        model.transcript
    );
}

#[test]
fn file_change_keeps_latest_diff_and_counts_touches() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::FileChange {
        path: "src/a.rs".into(),
        kind: FileChangeKind::Created,
        added: 1,
        removed: 0,
        diff: Some("+first".into()),
    });
    model.apply(&AgentEvent::FileChange {
        path: "src/a.rs".into(),
        kind: FileChangeKind::Modified,
        added: 1,
        removed: 0,
        diff: Some("+second".into()),
    });
    assert_eq!(model.files.len(), 1);
    let f = &model.files[0];
    assert_eq!(f.changes, 2);
    assert_eq!(f.kind, FileChangeKind::Modified);
    assert_eq!(f.latest_diff.as_deref(), Some("+second"));
}

#[test]
fn reads_count_without_clobbering_mutation_state() {
    let mut model = SessionModel::new();
    // First touch is a read: the file appears in the panel as read-only.
    model.apply(&AgentEvent::FileChange {
        path: "src/a.rs".into(),
        kind: FileChangeKind::Read,
        added: 0,
        removed: 0,
        diff: None,
    });
    assert_eq!(model.files.len(), 1, "reads appear in the files panel");
    let f = &model.files[0];
    assert_eq!(f.kind, FileChangeKind::Read);
    assert_eq!((f.changes, f.reads), (0, 1));

    // A mutation takes over kind/diff; a later re-read only grows the
    // read count — `changes` is the inline-diff freshness tag and must
    // not move on reads.
    model.apply(&AgentEvent::FileChange {
        path: "src/a.rs".into(),
        kind: FileChangeKind::Modified,
        added: 1,
        removed: 0,
        diff: Some("+x".into()),
    });
    model.apply(&AgentEvent::FileChange {
        path: "src/a.rs".into(),
        kind: FileChangeKind::Read,
        added: 0,
        removed: 0,
        diff: None,
    });
    let f = &model.files[0];
    assert_eq!(
        f.kind,
        FileChangeKind::Modified,
        "a re-read never regresses the badge"
    );
    assert_eq!(f.latest_diff.as_deref(), Some("+x"));
    assert_eq!((f.changes, f.reads), (1, 2));
}

#[test]
fn files_are_kept_in_first_touched_order() {
    let mut model = SessionModel::new();
    for p in ["z.rs", "a.rs", "m.rs"] {
        model.apply(&AgentEvent::FileChange {
            path: p.into(),
            kind: FileChangeKind::Modified,
            added: 0,
            removed: 0,
            diff: None,
        });
    }
    let order: Vec<&str> = model.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(order, vec!["z.rs", "a.rs", "m.rs"]);
}

/// One abort, one row. The pipeline emits `Error` for an abort it decided
/// and also returns `Aborted`, which the host re-emits — the reported
/// screenshot showed "aborted at scope review" twice and it read as two
/// failed attempts.
#[test]
fn an_identical_error_repeated_immediately_is_reported_once() {
    let mut model = SessionModel::new();
    let err = AgentEvent::Error {
        message: "aborted at scope review".into(),
        retryable: false,
    };
    model.apply(&err);
    model.apply(&err);
    assert_eq!(
        model
            .transcript
            .iter()
            .filter(|e| matches!(e, TranscriptEntry::Error { .. }))
            .count(),
        1
    );
}

#[test]
fn complete_populates_hud() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::RunComplete {
        model: "glm-5.2".into(),
        cost_usd: 0.033,
    });
    assert_eq!(model.hud.model.as_deref(), Some("glm-5.2"));
    assert_eq!(model.hud.final_cost_usd, Some(0.033));
    assert!(model.hud.complete);
    assert_eq!(model.hud.stage, Some(StageKind::Complete.into()));
}

#[test]
fn context_recall_cites_by_label_never_id() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ContextRecall {
        frames: vec![ContextFrameRef {
            id: Some("913d6df1-uuid".into()),
            citation_label: "driver.rs step-driver".into(),
            provider: "code-graph".into(),
            source: "code-graph".into(),
            kind: "symbol".into(),
            uri: None,
            method: None,
            token_cost: 100,
            block_id: None,
            content_digest: None,
        }],
        provider_mix: vec![ProviderShare {
            provider: "code-graph".into(),
            frames: 1,
        }],
        tokens: 100,
        usage: None,
        latency_ms: 0,
        used_ann_index: None,
    });
    match model.transcript.last() {
        Some(TranscriptEntry::ContextRecall { frames, .. }) => {
            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].label, "driver.rs step-driver");
            // L-C4 is a rule about the *primary* identifier a surface shows,
            // not about what the read-model may hold: the id "belongs only in
            // inspectable detail views". So it is carried — the deck's ctrl+o
            // provenance line is exactly such a view — and it is the renderer
            // that must keep it out of the collapsed row. That half is pinned
            // by `render::tests::collapsed_recall_cites_by_label_never_id`.
            assert!(!frames[0].label.contains("uuid"));
            assert_eq!(frames[0].id.as_deref(), Some("913d6df1-uuid"));
        }
        other => panic!("expected a context recall entry, got {other:?}"),
    }
}

/// Every field the wire carries about a recall survives the fold.
///
/// The read-model used to keep `{ frames: usize, tokens: u32, labels }` and
/// drop the rest, which is why no surface ever rendered the per-frame cost, the
/// frame kind, the recall latency (#875) or the ANN flag — the data was gone
/// two layers before the renderer. A fold that silently narrows is invisible
/// until someone asks the UI a question it can no longer answer, so it is
/// pinned here rather than left to the render tests.
#[test]
fn context_recall_fold_keeps_every_wire_field() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ContextRecall {
        frames: vec![ContextFrameRef {
            id: Some("nod_01H".into()),
            citation_label: "fn review".into(),
            provider: "code-graph".into(),
            source: "stella-graph".into(),
            kind: "symbol".into(),
            uri: Some("crates/stella-cli/src/command_deck/hunk_gate.rs:32".into()),
            method: Some("symbol-name".into()),
            token_cost: 104,
            block_id: None,
            content_digest: Some("sha256:9f2c1abdeadbeef".into()),
        }],
        provider_mix: vec![ProviderShare {
            provider: "code-graph".into(),
            frames: 1,
        }],
        tokens: 104,
        usage: Some(stella_protocol::ContextUsage {
            budget_requested: 4000,
            budget_consumed: 104,
            as_of: "2026-08-09T00:00:00Z".into(),
            providers: vec![stella_protocol::ContextProviderUsage {
                provider_id: "code-graph".into(),
                frames_served: 1,
                frames_rejected: 2,
                token_cost: 104,
            }],
        }),
        latency_ms: 34,
        used_ann_index: Some(true),
    });
    let Some(TranscriptEntry::ContextRecall {
        frames,
        tokens,
        latency_ms,
        used_ann_index,
        providers,
        budget,
    }) = model.transcript.last()
    else {
        panic!("expected a context recall entry");
    };
    assert_eq!(*tokens, 104);
    assert_eq!(*latency_ms, 34);
    assert_eq!(*used_ann_index, Some(true));
    assert_eq!(providers, &vec![("code-graph".to_string(), 1)]);

    let f = &frames[0];
    assert_eq!(f.kind, "symbol");
    assert_eq!(f.tokens, 104);
    assert_eq!(f.provider, "code-graph");
    assert_eq!(f.source, "stella-graph");
    assert_eq!(f.method.as_deref(), Some("symbol-name"));
    assert_eq!(
        f.uri.as_deref(),
        Some("crates/stella-cli/src/command_deck/hunk_gate.rs:32")
    );
    assert_eq!(f.digest.as_deref(), Some("sha256:9f2c1abdeadbeef"));

    // `frames_rejected` is the number the frame list cannot show — a rejected
    // frame never reaches it — so losing the budget report loses the only
    // evidence of a provider misdeclaring cost.
    let budget = budget.as_ref().expect("the usage report must survive");
    assert_eq!(budget.requested, 4000);
    assert_eq!(budget.consumed, 104);
    assert_eq!(
        budget.providers,
        vec![("code-graph".to_string(), 1, 2, 104)]
    );
}

#[test]
fn tool_result_summary_is_middle_out_truncated() {
    let mut model = SessionModel::new();
    let big = format!("HEAD{}TAIL", "x".repeat(500));
    model.apply(&AgentEvent::ToolResult {
        call_id: "c1".into(),
        output: ToolOutput::Ok {
            content: big,
            data: None,
        },
        duration_ms: 5,
        speculated: false,
    });
    match model.transcript.last() {
        Some(TranscriptEntry::ToolResult { summary, .. }) => {
            assert!(summary.starts_with("HEAD"), "kept head: {summary}");
            assert!(summary.ends_with("TAIL"), "kept tail: {summary}");
            assert!(summary.contains("..."), "elided middle: {summary}");
            assert!(summary.chars().count() <= SUMMARY_BUDGET);
        }
        other => panic!("expected a tool result entry, got {other:?}"),
    }
}

#[test]
fn colourised_tool_output_folds_to_clean_text() {
    // A `cargo build` failure as a colour-detecting child process emits it.
    // The escapes must be gone from BOTH the summary and the expanded full
    // text — the fold cache means anything kept here is kept forever (#934).
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ToolResult {
        call_id: "c1".into(),
        output: ToolOutput::error(
            "\u{1b}[0m\u{1b}[1m\u{1b}[38;5;9merror[E0308]\u{1b}[0m\u{1b}[1m: \
                      mismatched types in [u8; 4]\u{1b}[0m",
        ),
        duration_ms: 5,
        speculated: false,
    });
    match model.transcript.last() {
        Some(TranscriptEntry::ToolResult { summary, full, .. }) => {
            // The escape residue is gone, but legitimate bracket text — the
            // error code and the array type — survives untouched.
            assert_eq!(summary, "error[E0308]: mismatched types in [u8; 4]");
            assert_eq!(full, "error[E0308]: mismatched types in [u8; 4]");
        }
        other => panic!("expected a tool result entry, got {other:?}"),
    }
}

#[test]
fn oversized_tool_args_stay_valid_pretty_printable_json() {
    let mut model = SessionModel::new();
    let big = "x".repeat(INPUT_BUDGET * 2);
    model.apply(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".into(),
            name: "write_file".into(),
            input: serde_json::json!({ "path": "a.rs", "content": big }),
        },
    });
    match model.transcript.last() {
        Some(TranscriptEntry::ToolStart { raw, .. }) => {
            assert!(
                raw.chars().count() <= INPUT_BUDGET,
                "retained args stay within budget ({} chars)",
                raw.chars().count()
            );
            // The cap lands *inside* the JSON, so the expanded (ctrl+o)
            // view can still pretty-print the arguments.
            let v: serde_json::Value =
                serde_json::from_str(raw).expect("capped raw stays valid JSON");
            assert_eq!(v.get("path").and_then(|p| p.as_str()), Some("a.rs"));
            let content = v.get("content").and_then(|c| c.as_str()).unwrap();
            assert!(content.contains("[…]"), "long leaf carries the marker");
        }
        other => panic!("expected a tool start entry, got {other:?}"),
    }
}

#[test]
fn cap_middle_respects_char_boundaries_on_multibyte_text() {
    let text = "é".repeat(100);
    let capped = cap_middle(&text, 50);
    assert!(capped.chars().count() <= 50);
    assert!(capped.contains("truncated"), "marker present: {capped}");
    assert!(
        capped.starts_with('é') && capped.ends_with('é'),
        "head and tail preserved without splitting a char: {capped}"
    );
}

#[test]
fn media_and_verifier_and_pr_events_land_on_the_transcript() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::MediaProgress {
        artifact_id: "a1".into(),
        kind: MediaKind::Video,
        state: MediaJobState::Failed {
            reason: "nsfw".into(),
        },
    });
    model.apply(&AgentEvent::MediaComplete {
        artifact: MediaArtifactRef {
            id: "a2".into(),
            kind: MediaKind::Image,
            path: ".stella/artifacts/a2.png".into(),
            label: "diagram".into(),
        },
    });
    model.apply(&AgentEvent::Verdict {
        passed: true,
        evidence: VerdictEvidence {
            summary: "flip oracle passed".into(),
            deterministic: true,
            evidence_refs: vec![],
            ladder: None,
        },
    });
    model.apply(&AgentEvent::Pr {
        url: "https://x/pr/1".into(),
        status: PrStatus::Open,
        number: Some(1),
        ci: None,
    });
    assert_eq!(model.transcript.len(), 4);
    assert!(matches!(
        model.transcript[0],
        TranscriptEntry::MediaProgress { .. }
    ));
    assert!(matches!(
        model.transcript[3],
        TranscriptEntry::Pr {
            status: PrStatus::Open,
            ..
        }
    ));
}

/// Witness for #463: a `GoalVerdict` event lands as its own transcript row
/// (it used to fold as a no-op, leaving the transcript empty) — symmetric
/// to `Verdict`. Its `cost_usd` is *not* double-counted into HUD spend.
#[test]
fn goal_verdict_lands_on_the_transcript_without_touching_spend() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::GoalVerdict {
        round: 3,
        met: true,
        reasoning: "the witness test passes now".into(),
        cost_usd: 0.02,
    });
    assert_eq!(model.transcript.len(), 1, "goal verdict is recorded");
    match &model.transcript[0] {
        TranscriptEntry::GoalVerdict {
            met,
            round,
            reasoning,
        } => {
            assert!(*met);
            assert_eq!(*round, 3);
            assert_eq!(reasoning, "the witness test passes now");
        }
        other => panic!("expected GoalVerdict, got {other:?}"),
    }
    // `cost_usd` is billing state, not HUD state (`BudgetTick` drives spend).
    assert_eq!(
        model.hud.spent_usd, 0.0,
        "goal-verdict cost is not folded here"
    );
}

#[test]
fn ask_user_sets_pending_and_the_matching_tool_result_clears_it() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::AskUser {
        id: "call_ask_1".into(),
        question: "which database?".into(),
        options: vec!["postgres".into(), "sqlite".into()],
    });
    let pending = model.pending_ask_user.as_ref().expect("question pending");
    assert_eq!(pending.id, "call_ask_1");
    assert_eq!(pending.options.len(), 2);
    // An unrelated tool result must NOT clear it.
    model.apply(&AgentEvent::ToolResult {
        call_id: "call_other".into(),
        output: ToolOutput::Ok {
            content: "x".into(),
            data: None,
        },
        duration_ms: 1,
        speculated: false,
    });
    assert!(model.pending_ask_user.is_some());
    // The answer arrives as the ask_user tool's own result (matched by id).
    model.apply(&AgentEvent::ToolResult {
        call_id: "call_ask_1".into(),
        output: ToolOutput::Ok {
            content: "postgres".into(),
            data: None,
        },
        duration_ms: 1,
        speculated: false,
    });
    assert!(
        model.pending_ask_user.is_none(),
        "matching result clears it"
    );
}

#[test]
fn hunk_review_sets_pending_and_the_matching_tool_result_clears_it() {
    let mut model = SessionModel::new();
    let proposal = stella_protocol::HunkProposal {
        id: "hunk-review-1".into(),
        tool: "apply_edits".into(),
        hunks: vec![
            stella_protocol::ProposedHunk {
                path: "a.rs".into(),
                diff: "@@ -1,1 +1,1 @@\n-a\n+A\n".into(),
                lines_added: 1,
                lines_removed: 1,
            },
            stella_protocol::ProposedHunk {
                path: "b.rs".into(),
                diff: "@@ -1,1 +1,1 @@\n-b\n+B\n".into(),
                lines_added: 1,
                lines_removed: 1,
            },
        ],
    };
    model.apply(&AgentEvent::HunkReview {
        proposal: proposal.clone(),
    });
    assert_eq!(
        model
            .pending_hunk_review
            .as_ref()
            .expect("review pending")
            .hunks
            .len(),
        2
    );
    // The scrollback row counts DISTINCT files, not hunks — "2 hunks" alone
    // does not say whether one file or two were about to change.
    match model.transcript.last() {
        Some(TranscriptEntry::HunkReview { tool, hunks, files }) => {
            assert_eq!(tool, "apply_edits");
            assert_eq!((*hunks, *files), (2, 2));
        }
        other => panic!("expected HunkReview, got {other:?}"),
    }
    // An unrelated tool result must NOT clear it.
    model.apply(&AgentEvent::ToolResult {
        call_id: "call_other".into(),
        output: ToolOutput::Ok {
            content: "x".into(),
            data: None,
        },
        duration_ms: 1,
        speculated: false,
    });
    assert!(model.pending_hunk_review.is_some());
    // The host echoes a result carrying the proposal's id — the event-pure
    // clear. Without it the card eats keys for the rest of the turn.
    model.apply(&AgentEvent::ToolResult {
        call_id: "hunk-review-1".into(),
        output: ToolOutput::Ok {
            content: "applying 1 of 2 hunk(s)".into(),
            data: None,
        },
        duration_ms: 1,
        speculated: false,
    });
    assert!(model.pending_hunk_review.is_none());
}

/// A gate must never outlive its turn: a turn that dies with a card up would
/// otherwise leave the deck parked on a decision nothing is waiting for.
#[test]
fn a_terminal_event_clears_a_pending_hunk_review() {
    for terminal in [
        AgentEvent::RunComplete {
            model: "m".into(),
            cost_usd: 0.0,
        },
        AgentEvent::Error {
            message: "boom".into(),
            retryable: false,
        },
    ] {
        let mut model = SessionModel::new();
        model.apply(&AgentEvent::HunkReview {
            proposal: stella_protocol::HunkProposal {
                id: "hunk-review-1".into(),
                tool: "edit_file".into(),
                hunks: vec![stella_protocol::ProposedHunk {
                    path: "a.rs".into(),
                    diff: "@@ -1,1 +1,1 @@\n-a\n+A\n".into(),
                    lines_added: 1,
                    lines_removed: 1,
                }],
            },
        });
        assert!(model.pending_hunk_review.is_some());
        model.apply(&terminal);
        assert!(
            model.pending_hunk_review.is_none(),
            "{terminal:?} must close the gate"
        );
    }
}

/// Witness: a long pass's live counter occupies ONE transcript line however
/// many times it ticks, and its last value survives the milestone that follows.
///
/// The defect it pins: `/init`'s three long passes (the code-graph walk, then
/// the file and chunk embedding passes) narrate once a second, and every tick
/// arrived as an `AgentEvent::Text` that [`SessionModel::push_text`] coalesced
/// into the trailing buffer. A large workspace therefore buried the `✓`
/// summaries — the actual record of what init did — under a hundred
/// near-identical `· chunk index: N files embedded…` lines.
#[test]
fn a_progress_counter_rewrites_its_line_instead_of_stacking_them() {
    let mut model = SessionModel::new();
    model.apply(&text("◈ embedding code chunks for search…\n"));
    for embedded in [2, 3, 37] {
        model.set_progress_line(&format!("· chunk index: {embedded} files embedded…"));
    }
    model.apply(&text(
        "✓ chunk index: 37 file(s) embedded by voyage-code-3\n",
    ));

    let TranscriptEntry::Text(rendered) = &model.transcript[0] else {
        panic!(
            "expected one coalesced text entry, got {:?}",
            model.transcript
        );
    };
    assert_eq!(
        rendered.lines().collect::<Vec<_>>(),
        vec![
            "◈ embedding code chunks for search…",
            // Exactly one counter line, holding the LAST count — the earlier
            // ticks were rewritten, not appended.
            "· chunk index: 37 files embedded…",
            "✓ chunk index: 37 file(s) embedded by voyage-code-3",
        ],
        "{rendered}"
    );
}

/// The counter is rewritable only while it is still the end of the transcript:
/// once anything else has been folded, the tick it wrote is ordinary
/// scrollback, and the next pass starts a line of its own rather than
/// overwriting a finished pass's final count.
#[test]
fn a_settled_progress_line_is_never_overwritten_by_a_later_pass() {
    let mut model = SessionModel::new();
    model.set_progress_line("· semantic index: 12 files embedded…");
    model.apply(&AgentEvent::Stage {
        name: StageKind::Execute.into(),
        scope: stella_protocol::StageScope::Run,
    });
    model.set_progress_line("· chunk index: 4 files embedded…");

    let rendered: Vec<&str> = model
        .transcript
        .iter()
        .filter_map(|entry| match entry {
            TranscriptEntry::Text(s) => Some(s.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        rendered,
        vec![
            "· semantic index: 12 files embedded…\n",
            "· chunk index: 4 files embedded…\n",
        ],
        "{:?}",
        model.transcript
    );
}

// ---- #4155: a mutating row resolves the change its own call made ----

/// Fold one successful `edit_file` call: the start that names the path, then
/// the result that folds the transcript row.
fn edit_call(model: &mut SessionModel, call_id: &str, path: &str) {
    model.apply(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: call_id.into(),
            name: "edit_file".into(),
            input: serde_json::json!({ "path": path }),
        },
    });
    model.apply(&AgentEvent::ToolResult {
        call_id: call_id.into(),
        output: ToolOutput::ok("replaced 1 occurrence(s)"),
        duration_ms: 7,
        speculated: false,
    });
}

/// The turn boundary's measurement: `emit_shared_tree_changes` emits one
/// aggregate change per path *after* every `ToolResult` of the turn has
/// folded. That ordering is the whole defect, so these tests reproduce it
/// rather than emitting the change alongside the call.
fn turn_boundary(model: &mut SessionModel, path: &str, diff: Option<&str>, adds: u32, dels: u32) {
    model.apply(&AgentEvent::FileChange {
        path: path.into(),
        kind: FileChangeKind::Modified,
        added: adds,
        removed: dels,
        diff: diff.map(Into::into),
    });
}

/// The *leading* inline-diff reference that call's row kept, if any — the one
/// its body renders. Panics if the call folded no row at all, which would be a
/// different defect. Use [`inline_refs`] for the whole claim.
fn inline_ref<'a>(model: &'a SessionModel, call_id: &str) -> Option<&'a InlineDiffRef> {
    inline_refs(model, call_id).first()
}

/// Every inline-diff reference that call's row claimed, in the order the row
/// reads them (#4214).
fn inline_refs<'a>(model: &'a SessionModel, call_id: &str) -> &'a [InlineDiffRef] {
    model
        .transcript
        .iter()
        .find_map(|e| match e {
            TranscriptEntry::ToolResult {
                call_id: cid, diff, ..
            } if cid == call_id => Some(diff.as_slice()),
            _ => None,
        })
        .expect("the call folded a result row")
}

/// The live cause of #4155: a successful edit rendered no diff and no
/// `+N −M`, because the seq the row stamped named the change *before* its own.
#[test]
fn an_edit_result_resolves_the_change_its_own_call_made() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/a.rs");
    turn_boundary(
        &mut model,
        "src/a.rs",
        Some("@@ -1 +1,2 @@\n+first\n"),
        2,
        1,
    );

    let dref = inline_ref(&model, "c1").expect("a successful edit keeps an inline-diff ref");
    let file = model.files.iter().find(|f| f.path == "src/a.rs").unwrap();
    assert_eq!(
        file.diff_at(dref.seq),
        Some("@@ -1 +1,2 @@\n+first\n"),
        "the row resolves the diff of the change its own call produced"
    );
    assert_eq!(
        file.delta_at(dref.seq),
        Some((2, 1)),
        "and the measurement that rides with it"
    );
}

/// The half the off-by-one hid: it did not merely blank the row, it pointed
/// every turn after the first at the PREVIOUS turn's change to that path —
/// the misattribution `render::resolve_inline_diff` exists to prevent.
#[test]
fn a_later_turns_edit_never_renders_an_earlier_turns_diff() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/a.rs");
    turn_boundary(&mut model, "src/a.rs", Some("@@ first turn @@\n"), 1, 0);
    edit_call(&mut model, "c2", "src/a.rs");
    turn_boundary(&mut model, "src/a.rs", Some("@@ second turn @@\n"), 3, 2);

    let file = model.files.iter().find(|f| f.path == "src/a.rs").unwrap();
    let first = inline_ref(&model, "c1").expect("turn one keeps its ref");
    let second = inline_ref(&model, "c2").expect("turn two keeps its ref");
    assert_ne!(first.seq, second.seq, "two turns, two distinct changes");
    assert_eq!(file.diff_at(first.seq), Some("@@ first turn @@\n"));
    assert_eq!(
        file.diff_at(second.seq),
        Some("@@ second turn @@\n"),
        "turn two's row shows turn two's change, not the one before it"
    );
}

/// One aggregate change per path per turn, so exactly one row may claim it.
/// Stamping every call that touched the path would render the turn's whole
/// change under each of them; the last keeps it, the earlier ones degrade to
/// naming their change.
#[test]
fn only_the_last_call_to_a_path_in_a_turn_claims_the_turns_change() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/a.rs");
    edit_call(&mut model, "c2", "src/a.rs");
    turn_boundary(&mut model, "src/a.rs", Some("@@ both edits @@\n"), 4, 1);

    assert!(
        inline_ref(&model, "c1").is_none(),
        "the superseded row gives up its ref rather than restating the aggregate"
    );
    let last = inline_ref(&model, "c2").expect("the last call keeps it");
    let file = model.files.iter().find(|f| f.path == "src/a.rs").unwrap();
    assert_eq!(file.diff_at(last.seq), Some("@@ both edits @@\n"));
}

/// Supersession is per path: two files edited in one turn keep one row each.
#[test]
fn calls_to_different_paths_in_one_turn_each_keep_their_own_ref() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/a.rs");
    edit_call(&mut model, "c2", "src/b.rs");
    turn_boundary(&mut model, "src/a.rs", Some("@@ a @@\n"), 1, 0);
    turn_boundary(&mut model, "src/b.rs", Some("@@ b @@\n"), 2, 0);

    for (call, path, text) in [
        ("c1", "src/a.rs", "@@ a @@\n"),
        ("c2", "src/b.rs", "@@ b @@\n"),
    ] {
        let dref = inline_ref(&model, call).expect("each path's row keeps its ref");
        let file = model.files.iter().find(|f| f.path == path).unwrap();
        assert_eq!(file.diff_at(dref.seq), Some(text));
    }
}

/// A turn that measured no net change to the path leaves the ref dangling,
/// and a dangling ref renders nothing. Silence is the honest answer — an edit
/// reverted within the same turn changed nothing on disk.
#[test]
fn a_turn_that_measured_no_change_leaves_the_row_silent() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/a.rs");
    let dref = inline_ref(&model, "c1").expect("the ref is still stamped");
    assert_eq!(dref.path, "src/a.rs");
    assert!(
        model.files.iter().all(|f| f.path != "src/a.rs"),
        "nothing measured the path, so nothing resolves"
    );
}

/// A failed mutation still carries no reference: the change it would point at
/// is one it never made.
#[test]
fn a_failed_mutation_keeps_no_inline_diff_ref() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".into(),
            name: "edit_file".into(),
            input: serde_json::json!({ "path": "src/a.rs" }),
        },
    });
    model.apply(&AgentEvent::ToolResult {
        call_id: "c1".into(),
        output: ToolOutput::error("no such file"),
        duration_ms: 3,
        speculated: false,
    });
    turn_boundary(&mut model, "src/a.rs", Some("@@ someone else @@\n"), 1, 0);
    assert!(inline_ref(&model, "c1").is_none());
}

/// #4155's second named cause: counts and diff text arrive independently, and
/// a change measured without an attachable patch used to be dropped entirely —
/// so the row lost its `+N −M` as well as its diff.
#[test]
fn a_measured_change_with_no_patch_still_reports_its_delta() {
    let mut model = SessionModel::new();
    edit_call(&mut model, "c1", "src/a.rs");
    turn_boundary(&mut model, "src/a.rs", None, 3, 1);

    let dref = inline_ref(&model, "c1").expect("the row keeps its ref");
    let file = model.files.iter().find(|f| f.path == "src/a.rs").unwrap();
    assert_eq!(
        file.delta_at(dref.seq),
        Some((3, 1)),
        "the measurement survives the missing patch"
    );
    assert_eq!(
        file.diff_at(dref.seq),
        None,
        "and no patch is invented for it"
    );
}

mod producer_seq;
mod retention;
mod scope_review;
