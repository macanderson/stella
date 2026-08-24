// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Retention: what the transcript drops when it hits `MAX_TRANSCRIPT_ENTRIES`,
//! and the replay determinism that must survive the drop.
//!
//! Split out of `model/tests.rs` so that file stays under the ungrandfathered
//! 1500-line ceiling rather than taking a baseline entry (#4217, #3441). Pure
//! relocation: no test was changed, added, or removed in the move.
//!
//! These belong together because they are one claim seen from two sides. The
//! retention cap is the only thing in the fold that *forgets*, so it is the
//! only thing that can break L-T1 — replaying a log must yield an identical
//! model, and a cap that evicted by wall-clock, by a non-cumulative marker
//! count, or by draining the live tail would make the retained window a
//! function of something other than the event sequence. So the eviction tests
//! and the replay tests are testing one property, and a change to either half
//! should fail in view of the other.

use super::*;

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
            name: StageKind::Execute.into(),
            scope: stella_protocol::StageScope::Run,
        },
        text("hi "),
        text("there"),
        AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "c1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "src/lib.rs"}),
            },
            sub_agent_id: None,
        },
        AgentEvent::FileChange {
            path: "src/lib.rs".into(),
            kind: FileChangeKind::Modified,
            added: 1,
            removed: 1,
            diff: Some("@@\n-a\n+b".into()),
        },
        AgentEvent::RunComplete {
            model: "glm".into(),
            cost_usd: 0.01,
        },
    ];
    let a = SessionModel::replay(&log);
    let b = SessionModel::replay(&log);
    assert_eq!(a, b);
}
