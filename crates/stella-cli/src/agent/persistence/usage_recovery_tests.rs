//! `persist_event_detailed`'s recovery tests: a dropped or partial usage
//! frame still lands a flagged telemetry row, and the rendered sentence
//! never claims a dead call was unaffected (#4147, #4383).
//!
//! Split out of `persistence.rs` when that file crowded the 1500-line
//! ceiling (#3776) — a pure move, following the same out-of-line pattern as
//! `stream_tests` and `pipeline_variant_tests` beside it.

use super::*;

fn partial(input: u64, cached: u64, output: u64, cost: f64) -> stella_protocol::PartialUsage {
    stella_protocol::PartialUsage {
        usage: stella_protocol::CompletionUsage {
            input_tokens: input,
            cached_input_tokens: cached,
            output_tokens: output,
            ..Default::default()
        },
        cost_usd: cost,
        input_reported: true,
    }
}

fn incomplete(partial: Option<stella_protocol::PartialUsage>) -> AgentEvent {
    incomplete_because(
        stella_protocol::UsageIncompleteReason::ProviderError,
        partial,
    )
}

fn incomplete_because(
    reason: stella_protocol::UsageIncompleteReason,
    partial: Option<stella_protocol::PartialUsage>,
) -> AgentEvent {
    AgentEvent::UsageIncomplete {
        role: stella_protocol::ModelCallRole::Worker,
        provider: "anthropic".into(),
        model: "claude-opus-5".into(),
        reason,
        duration_ms: 4_200,
        retries: Some(1),
        partial,
        sub_agent_id: None,
    }
}

/// A call that SETTLED and simply went unbilled: the provider closed the
/// stream cleanly but sent no usage frame. The other way into
/// [`PersistOutcome::UsageIncomplete`], and the case whose reassurance is
/// true.
fn settled_without_a_usage_frame() -> AgentEvent {
    AgentEvent::StepUsage {
        upstream_provider: None,
        step: 0,
        role: stella_protocol::ModelCallRole::Worker,
        provider: "anthropic".into(),
        output_text: None,
        model: "claude-opus-5".into(),
        input_tokens: 0,
        output_tokens: 0,
        cached_input_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: None,
        estimated_input_tokens: 0,
        cost_usd: 0.0,
        duration_ms: 1_200,
        retries: 0,
        tool_calls: 0,
        complete: false,
        finish_reason: None,
        sub_agent_id: None,
    }
}

fn sentence_for(event: &AgentEvent) -> String {
    let store = stella_store::Store::in_memory().expect("store");
    let execution_id = store
        .begin_execution("cli", "prompt", "anthropic", "claude-opus-5")
        .expect("begin");
    persist_event_detailed(&store, execution_id, 0, event, "anthropic")
        .message("one model call")
        .expect("an incomplete outcome has a sentence")
}

/// The storage half of the fix. A dropped attempt that salvaged real
/// numbers must leave a row behind — flagged, but present. Writing
/// nothing at all is what made `stella stats` under-report a session's
/// token use with no trace that anything was missing.
#[test]
fn a_recovered_attempt_lands_a_row_flagged_incomplete() {
    let store = stella_store::Store::in_memory().expect("store");
    let execution_id = store
        .begin_execution("cli", "prompt", "anthropic", "claude-opus-5")
        .expect("begin");

    let outcome = persist_event_detailed(
        &store,
        execution_id,
        0,
        &incomplete(Some(partial(14_000, 12_000, 130, 0.0213))),
        "anthropic",
    );

    let rows = store.telemetry_rows_after(0, 10).expect("rows");
    assert_eq!(rows.len(), 1, "the salvaged attempt is recorded");
    let row = &rows[0].telemetry;
    assert_eq!(row.input_tokens, 14_000);
    assert_eq!(row.cache_read_tokens, 12_000);
    assert_eq!(row.cache_miss_tokens, 2_000, "miss = input - cached");
    assert_eq!(row.output_tokens, 130);
    assert!((row.cost_usd - 0.0213).abs() < f64::EPSILON);
    assert!(
        !row.usage_complete,
        "a catalog-priced lower bound must never pass as settled accounting"
    );
    // And the execution as a whole is marked short.
    assert!(!store.execution_usage_complete(execution_id).unwrap());
    assert!(matches!(
        outcome,
        PersistOutcome::UsageIncomplete {
            partial: Some(_),
            ..
        }
    ));
}

/// **The #4383 witness.** An attempt that salvaged *nothing* still leaves
/// a row.
///
/// The telemetry write used to sit inside `if let Some(partial)`, so this
/// case wrote nothing at all: a sub-agent model call still open when the
/// `delegate` tool hit its 900 s ceiling left the execution flagged
/// `usage_complete = 0` with no row anywhere naming the call that did it.
/// Session `ses-1787465453163-60967` has a nine-minute window with no
/// telemetry row and no tool call over a second, and a sub-agent alive
/// through all of it.
///
/// A zero-usage row is not a claim the call was free — the flag says the
/// envelope never landed. It is the record that the call happened.
///
/// This replaces `an_attempt_that_recovered_nothing_writes_no_row`, which
/// asserted the opposite and gave its reason: a zeroed row "reads as a
/// real, free call". #4383 is the counter-evidence, and the flag is why —
/// `usage_complete = false` is exactly what separates zero-because-free
/// from zero-because-unknown, no reader that sums cost is moved by adding
/// 0.0, and the `(role, model)` census AGENTS.md tells a bench reader to
/// run gains a call it could not see at all. That test's other assertion —
/// a dead attempt carries its `died` reason (#4147) — is kept below.
#[test]
fn an_abandoned_attempt_with_nothing_to_salvage_still_lands_a_flagged_row() {
    let store = stella_store::Store::in_memory().expect("store");
    let execution_id = store
        .begin_execution("cli", "prompt", "anthropic", "claude-opus-5")
        .expect("begin");

    let outcome = persist_event_detailed(
        &store,
        execution_id,
        7,
        &incomplete_because(stella_protocol::UsageIncompleteReason::Timeout, None),
        "anthropic",
    );

    let rows = store.telemetry_rows_after(0, 10).expect("rows");
    assert_eq!(
        rows.len(),
        1,
        "an abandoned call must be visible, not a hole in the count"
    );
    let row = &rows[0].telemetry;
    assert_eq!(row.step, 7, "the row names the call that died");
    assert_eq!(row.model, "claude-opus-5");
    assert_eq!(row.input_tokens, 0);
    assert_eq!(row.output_tokens, 0);
    assert!(row.cost_usd.abs() < f64::EPSILON);
    assert_eq!(row.duration_ms, 4_200, "how long it was in flight");
    assert!(
        !row.usage_complete,
        "zero here means unknown, and the flag is what says so"
    );
    assert!(!store.execution_usage_complete(execution_id).unwrap());
    assert!(matches!(
        outcome,
        PersistOutcome::UsageIncomplete {
            partial: None,
            died: Some(stella_protocol::UsageIncompleteReason::Timeout),
        }
    ));

    // A dead attempt carries its own reason, whichever one killed it —
    // this is the case the rendered sentence must NOT call unaffected
    // (#4147).
    let provider_error =
        persist_event_detailed(&store, execution_id, 8, &incomplete(None), "anthropic");
    assert!(matches!(
        provider_error,
        PersistOutcome::UsageIncomplete {
            partial: None,
            died: Some(stella_protocol::UsageIncompleteReason::ProviderError),
        }
    ));
    assert_eq!(
        store.telemetry_rows_after(0, 10).expect("rows").len(),
        2,
        "each dead attempt is its own row"
    );
}

/// A lossy EVENT LOG must not disqualify the COST ROLLUP.
///
/// The witness for the defect found in session `ses-1787342320630-36613`:
/// an execution carrying 69 telemetry rows, every one flagged complete
/// and summing to its recorded cost exactly, reported `usage_complete =
/// false` and was therefore excluded from `stella usage report` and from
/// hub replication — permanently, since the flag is a one-way latch.
///
/// `record_event` is `UNIQUE (execution_id, seq)`, so replaying a seq is
/// a real store write failure with no mocking. The event chosen is a text
/// delta: it touches no telemetry, so the *only* thing that fails is the
/// event-log append. Before this, that alone was enough.
#[test]
fn a_failed_event_log_write_does_not_disqualify_the_cost_rollup() {
    let store = stella_store::Store::in_memory().expect("store");
    let execution_id = store
        .begin_execution("cli", "prompt", "anthropic", "claude-opus-5")
        .expect("begin");
    let event = stella_protocol::event::AgentEvent::TextDelta {
        delta: "hello".into(),
    };

    // Seq 0 lands.
    let first = persist_event_detailed(&store, execution_id, 0, &event, "anthropic");
    assert!(matches!(first, PersistOutcome::Complete));

    // Replaying seq 0 collides on UNIQUE (execution_id, seq).
    let replayed = persist_event_detailed(&store, execution_id, 0, &event, "anthropic");
    assert!(
        matches!(replayed, PersistOutcome::StoreWriteFailed),
        "the lossy event log is still reported — this fix narrows what it \
         disqualifies, it does not hide it"
    );

    store
        .finish_execution_accounted(execution_id, "completed", 1.25, true)
        .expect("finish");
    assert!(
        store.execution_usage_complete(execution_id).unwrap(),
        "no model call went unaccounted for, so the execution's cost total is whole \
         and must stay visible to `stella usage report`"
    );
}

/// The other half of the same contract: a receipt that failed to land DOES
/// disqualify the total, because the sum is now genuinely short by that
/// call and no later reconciliation can see the gap.
#[test]
fn an_unaccounted_call_still_disqualifies_the_cost_rollup() {
    let store = stella_store::Store::in_memory().expect("store");
    let execution_id = store
        .begin_execution("cli", "prompt", "anthropic", "claude-opus-5")
        .expect("begin");

    persist_event_detailed(&store, execution_id, 0, &incomplete(None), "anthropic");

    store
        .finish_execution_accounted(execution_id, "completed", 1.25, true)
        .expect("finish");
    assert!(!store.execution_usage_complete(execution_id).unwrap());
}

/// The wording defect from the report: one retried call must not be
/// described as the whole session, and when numbers were recovered the
/// sentence should say so rather than leaving the user to assume the
/// worst.
#[test]
fn the_warning_names_one_call_and_reports_what_was_recovered() {
    let message = PersistOutcome::UsageIncomplete {
        partial: Some(partial(14_000, 12_000, 130, 0.0213)),
        died: Some(stella_protocol::UsageIncompleteReason::ProviderError),
    }
    .message("one model call")
    .expect("an incomplete outcome has a sentence");
    assert!(message.starts_with("one model call"), "{message}");
    assert!(!message.contains("this session"), "{message}");
    assert!(message.contains("14000 input"), "{message}");
    assert!(message.contains("12000 cached"), "{message}");
    assert!(message.contains("130 output"), "{message}");
    assert!(message.contains("0.0213"), "{message}");

    // With nothing recovered it stays honest about the gap, and still
    // scopes itself to the one attempt.
    let bare = PersistOutcome::UsageIncomplete {
        partial: None,
        died: None,
    }
    .message("one model call")
    .expect("still a sentence");
    assert!(bare.contains("tokens and cost"), "{bare}");
    assert!(!bare.contains("this session"), "{bare}");
    // A call that settled without a usage frame did not "fail", and the
    // sentence must not claim it did.
    assert!(!bare.contains("failed"), "{bare}");

    // A genuine store failure keeps its own, more serious wording.
    let store_failed = PersistOutcome::StoreWriteFailed
        .message("this session")
        .expect("a sentence");
    assert!(
        store_failed.contains("store write failed"),
        "{store_failed}"
    );
}

/// The reported panel (#4147): an OpenRouter stream aborted, and the very
/// first line the user read was the accounting warning telling them the
/// work was fine — printed directly above the two rows reporting the
/// abort. The reassurance belongs to a call that SETTLED without a usage
/// frame; on a call that died it asserts a success that did not happen.
///
/// Driven through `persist_event_detailed` rather than by building a
/// `PersistOutcome` by hand, deliberately: the fix changes that enum's
/// shape, so a hand-built witness would fail to *compile* on the parent
/// commit rather than fail its assertion, which proves nothing about the
/// behaviour. Feeding the real event through the real path compiles on
/// both sides and genuinely flips.
#[test]
fn a_dead_call_is_never_described_as_leaving_the_work_unaffected() {
    for reason in [
        stella_protocol::UsageIncompleteReason::ProviderError,
        stella_protocol::UsageIncompleteReason::Timeout,
        stella_protocol::UsageIncompleteReason::Cancelled,
    ] {
        let message = sentence_for(&incomplete_because(reason, None));
        assert!(
            !message.contains("unaffected"),
            "{reason:?} claimed the work landed: {message}"
        );
        // It still has to say what it came to say: the accounting is short.
        assert!(message.contains("tokens and cost"), "{message}");
        assert!(message.starts_with("one model call"), "{message}");
    }
}

/// The negative control for the test above, and the property the original
/// wording existed to protect: a call that settled keeps its reassurance
/// verbatim. One dropped frame out of hundreds is a footnote, and losing
/// this would regress the sentence #4147's fix is built on top of.
#[test]
fn a_settled_call_keeps_its_reassurance() {
    let settled = sentence_for(&settled_without_a_usage_frame());
    assert!(
        settled.contains("the work itself is unaffected"),
        "{settled}"
    );
}
