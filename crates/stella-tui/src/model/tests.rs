//! Unit tests for [`crate::model`].
//!
//! Split out of `model.rs` so the module drops back under the 1500-line
//! limit (#629) and can retire its baseline exemption entirely, rather
//! than raising the ceiling. Pure relocation: no test was changed, added,
//! or removed.

use super::*;
use stella_protocol::{
    ContextFrameRef, JudgeEvidence, MediaArtifactRef, MediaJobState, ProviderShare, ToolCall,
};

fn text(delta: &str) -> AgentEvent {
    AgentEvent::Text {
        delta: delta.into(),
    }
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
        name: StageKind::Verify,
    });
    model.apply(&text("b"));
    // text, stage, text
    assert_eq!(model.transcript.len(), 3);
    assert!(matches!(model.transcript[0], TranscriptEntry::Text(_)));
    assert!(matches!(model.transcript[1], TranscriptEntry::Stage(_)));
    assert!(matches!(model.transcript[2], TranscriptEntry::Text(_)));
}

fn delta(text: &str) -> AgentEvent {
    AgentEvent::TextDelta { text: text.into() }
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
        AgentEvent::Complete {
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
        },
        text("Hello"),
        AgentEvent::Complete {
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

#[test]
fn scope_review_sets_then_clears_on_next_stage() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ScopeReview {
        proposal: ScopeProposal {
            summary: "big refactor".into(),
            steps: vec!["s1".into(), "s2".into()],
            estimated_files: 12,
            estimated_cost_usd: Some(1.0),
            ..Default::default()
        },
    });
    assert!(model.pending_scope_review.is_some());
    // The scope-review stage marker itself must NOT clear it.
    model.apply(&AgentEvent::Stage {
        name: StageKind::ScopeReview,
    });
    assert!(model.pending_scope_review.is_some());
    // The engine moving on to execute clears it.
    model.apply(&AgentEvent::Stage {
        name: StageKind::Execute,
    });
    assert!(model.pending_scope_review.is_none());
    // …but the plan itself survives: the gate closing is the approval, and the
    // approved steps have to stay recallable for the rest of the turn.
    let approved = model.approved_scope.as_ref().expect("the plan is kept");
    assert_eq!(approved.steps, vec!["s1".to_string(), "s2".to_string()]);
}

/// The reported defect, isolated: the steps a user consented to were the one
/// thing the session could no longer show them one minute later. The scrollback
/// record keeps a summary and two counts — never the steps — so dropping the
/// proposal destroyed the only copy.
#[test]
fn an_approved_plan_outlives_the_gate_that_carried_it() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::ScopeReview {
        proposal: ScopeProposal {
            summary: "collapse the panels".into(),
            steps: vec!["gate on relevance".into(), "pin the scope".into()],
            estimated_files: 9,
            estimated_cost_usd: Some(1.4),
            ..Default::default()
        },
    });
    model.apply(&AgentEvent::Stage {
        name: StageKind::Execute,
    });
    model.apply(&AgentEvent::Complete {
        model: "glm".into(),
        cost_usd: 0.5,
    });
    let approved = model
        .approved_scope
        .as_ref()
        .expect("the plan survives the whole turn, not just the gate");
    assert_eq!(approved.summary, "collapse the panels");
    assert_eq!(approved.steps.len(), 2);

    // And it belongs to *that* turn: the next one starts unapproved rather
    // than inheriting consent it was never given.
    model.apply(&AgentEvent::Stage {
        name: StageKind::Execute,
    });
    assert!(
        model.approved_scope.is_none(),
        "a new turn inherited the previous turn's approval"
    );
}

/// A turn that died at the gate was never approved, and must not be recorded
/// as though it were — an abandoned proposal is not a plan.
#[test]
fn a_turn_that_dies_at_the_gate_records_no_approval() {
    for terminal in [
        AgentEvent::Error {
            message: "aborted".into(),
            retryable: false,
        },
        AgentEvent::Complete {
            model: "glm".into(),
            cost_usd: 0.01,
        },
    ] {
        let mut model = SessionModel::new();
        model.apply(&AgentEvent::ScopeReview {
            proposal: ScopeProposal {
                summary: "never approved".into(),
                steps: vec!["s1".into()],
                estimated_files: 1,
                estimated_cost_usd: None,
                ..Default::default()
            },
        });
        model.apply(&terminal);
        assert!(model.pending_scope_review.is_none());
        assert!(
            model.approved_scope.is_none(),
            "an abandoned proposal was promoted to an approved plan"
        );
    }
}

#[test]
fn scope_review_clears_on_error_and_complete() {
    for terminal in [
        AgentEvent::Error {
            message: "aborted".into(),
            retryable: false,
        },
        AgentEvent::Complete {
            model: "glm".into(),
            cost_usd: 0.01,
        },
    ] {
        let mut model = SessionModel::new();
        model.apply(&AgentEvent::ScopeReview {
            proposal: ScopeProposal {
                summary: "x".into(),
                steps: vec![],
                estimated_files: 1,
                estimated_cost_usd: None,
                ..Default::default()
            },
        });
        assert!(model.pending_scope_review.is_some());
        model.apply(&terminal);
        assert!(model.pending_scope_review.is_none());
    }
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
    model.apply(&AgentEvent::Complete {
        model: "glm-5.2".into(),
        cost_usd: 0.033,
    });
    assert_eq!(model.hud.model.as_deref(), Some("glm-5.2"));
    assert_eq!(model.hud.final_cost_usd, Some(0.033));
    assert!(model.hud.complete);
    assert_eq!(model.hud.stage, Some(StageKind::Complete));
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
        Some(TranscriptEntry::ContextRecall { labels, .. }) => {
            assert_eq!(labels, &vec!["driver.rs step-driver".to_string()]);
            assert!(!labels.iter().any(|l| l.contains("uuid")));
        }
        other => panic!("expected a context recall entry, got {other:?}"),
    }
}

#[test]
fn tool_result_summary_is_middle_out_truncated() {
    let mut model = SessionModel::new();
    let big = format!("HEAD{}TAIL", "x".repeat(500));
    model.apply(&AgentEvent::ToolResult {
        call_id: "c1".into(),
        output: ToolOutput::Ok { content: big },
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
        output: ToolOutput::Error {
            message: "\u{1b}[0m\u{1b}[1m\u{1b}[38;5;9merror[E0308]\u{1b}[0m\u{1b}[1m: \
                      mismatched types in [u8; 4]\u{1b}[0m"
                .into(),
        },
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
fn media_and_judge_and_pr_events_land_on_the_transcript() {
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
    model.apply(&AgentEvent::JudgeVerdict {
        passed: true,
        evidence: JudgeEvidence {
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
/// to `JudgeVerdict`. Its `cost_usd` is *not* double-counted into HUD spend.
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
        AgentEvent::Complete {
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

/// A non-coalescing one-entry event, for growing the transcript by
/// exactly one entry per apply.
fn retry(attempt: u32) -> AgentEvent {
    AgentEvent::Retry {
        attempt,
        reason: "r".into(),
    }
}

#[test]
fn below_the_cap_nothing_evicts() {
    let mut model = SessionModel::new();
    for i in 0..(MAX_TRANSCRIPT_ENTRIES - 1) {
        model.apply(&retry(i as u32));
    }
    assert_eq!(model.transcript.len(), MAX_TRANSCRIPT_ENTRIES - 1);
    assert_eq!(model.evicted_entries(), 0);
    assert!(matches!(
        model.transcript[0],
        TranscriptEntry::Retry { attempt: 0, .. }
    ));
}

#[test]
fn transcript_caps_with_a_front_eviction_marker() {
    let mut model = SessionModel::new();
    let total = MAX_TRANSCRIPT_ENTRIES + 250;
    for i in 0..total {
        model.apply(&retry(i as u32));
    }
    assert!(model.transcript.len() <= MAX_TRANSCRIPT_ENTRIES);
    let count = match model.transcript[0] {
        TranscriptEntry::Evicted { count } => count,
        ref other => panic!("expected the eviction marker first, got {other:?}"),
    };
    // The marker plus the retained entries account for every entry pushed.
    assert_eq!(count + (model.transcript.len() - 1), total);
    // The tail is untouched: the newest event is still the last entry.
    match model.transcript.last() {
        Some(TranscriptEntry::Retry { attempt, .. }) => {
            assert_eq!(*attempt, (total - 1) as u32);
        }
        other => panic!("expected the newest retry last, got {other:?}"),
    }
}

#[test]
fn eviction_marker_accumulates_across_passes() {
    let mut model = SessionModel::new();
    // Enough to trigger a second pass, which drains the first marker.
    let total = MAX_TRANSCRIPT_ENTRIES + TRANSCRIPT_EVICTION_CHUNK + 10;
    for i in 0..total {
        model.apply(&retry(i as u32));
    }
    let count = model.evicted_entries();
    assert!(
        count > TRANSCRIPT_EVICTION_CHUNK,
        "second pass absorbed the first marker's count: {count}"
    );
    assert_eq!(count + (model.transcript.len() - 1), total);
    // Exactly one marker survives, at the front.
    let markers = model
        .transcript
        .iter()
        .filter(|e| matches!(e, TranscriptEntry::Evicted { .. }))
        .count();
    assert_eq!(markers, 1);
}

#[test]
fn user_prompts_count_against_the_cap() {
    let mut model = SessionModel::new();
    for i in 0..(MAX_TRANSCRIPT_ENTRIES + 5) {
        model.push_user_prompt(&format!("prompt {i}"));
    }
    assert!(model.transcript.len() <= MAX_TRANSCRIPT_ENTRIES);
    assert!(model.evicted_entries() >= TRANSCRIPT_EVICTION_CHUNK);
}

#[test]
fn replay_past_the_cap_stays_deterministic() {
    let log: Vec<AgentEvent> = (0..(MAX_TRANSCRIPT_ENTRIES + TRANSCRIPT_EVICTION_CHUNK + 3))
        .map(|i| retry(i as u32))
        .collect();
    let a = SessionModel::replay(&log);
    let b = SessionModel::replay(&log);
    assert_eq!(a, b);
    assert!(a.transcript.len() <= MAX_TRANSCRIPT_ENTRIES);
}

#[test]
fn replay_of_the_same_log_yields_identical_models() {
    let log = vec![
        AgentEvent::Stage {
            name: StageKind::Execute,
        },
        text("hi "),
        text("there"),
        AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "c1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "src/lib.rs"}),
            },
        },
        AgentEvent::FileChange {
            path: "src/lib.rs".into(),
            kind: FileChangeKind::Modified,
            added: 1,
            removed: 1,
            diff: Some("@@\n-a\n+b".into()),
        },
        AgentEvent::Complete {
            model: "glm".into(),
            cost_usd: 0.01,
        },
    ];
    let a = SessionModel::replay(&log);
    let b = SessionModel::replay(&log);
    assert_eq!(a, b);
}
