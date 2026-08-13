//! Tests for [`crate::export::transcript`] — the fold from a session's event
//! journal to the archive's readable half.
//!
//! Four of these are witnesses for properties stated in the module doc, and
//! each is written so that removing the property it guards turns it red rather
//! than merely reducing coverage — see the comment on each.

use std::collections::HashMap;

use stella_protocol::{AgentEvent, StageKind, ToolCall, ToolOutput};
use stella_store::{SessionEventRecord, SessionJournal};

use super::*;

/// A journal from `(ts, event)` pairs, all in one execution.
fn journal(events: Vec<(&str, AgentEvent)>) -> SessionJournal {
    SessionJournal {
        events: events
            .into_iter()
            .enumerate()
            .map(|(seq, (ts, event))| SessionEventRecord {
                execution_id: 1,
                seq: seq as i64,
                ts: ts.to_string(),
                event,
            })
            .collect(),
        skipped: 0,
    }
}

/// The same instant for every event, when only the fold order matters.
fn at(events: Vec<AgentEvent>) -> SessionJournal {
    journal(
        events
            .into_iter()
            .map(|e| ("2026-08-09 12:00:00", e))
            .collect(),
    )
}

fn no_prompts() -> HashMap<i64, String> {
    HashMap::new()
}

fn text(s: &str) -> AgentEvent {
    AgentEvent::Text { text: s.into() }
}

fn delta(s: &str) -> AgentEvent {
    AgentEvent::TextDelta { delta: s.into() }
}

/// How many times `needle` occurs in `haystack`.
fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn an_assistant_message_streamed_as_deltas_renders_exactly_once() {
    // THE witness for module-doc property 3. The engine streams prose as
    // `text_delta` fragments and then emits one consolidated `text` carrying
    // the same prose. A fold that coalesces the delta run AND renders the
    // `text` draws the message twice — and the duplicate reads, to anyone
    // holding the artifact rather than the stream, as a second paid model
    // call that never happened.
    //
    // Delete the `pending_text.clear()` in the `Text` arm and this goes red
    // with two `ev say` rows: that line IS the property.
    let out = render(
        &at(vec![
            delta("Step 1 done: "),
            delta("the tree is clean."),
            text("Step 1 done: the tree is clean."),
        ]),
        &no_prompts(),
    );

    assert_eq!(out.rendered, 1, "one message is one row:\n{}", out.body);
    assert_eq!(
        count(&out.body, r#"class="ev say""#),
        1,
        "exactly one assistant row:\n{}",
        out.body
    );
    assert_eq!(
        count(&out.body, "Step 1 done: the tree is clean."),
        1,
        "the prose appears once, not twice:\n{}",
        out.body
    );
}

#[test]
fn a_delta_run_with_no_consolidating_text_is_still_rendered() {
    // The other half of the same property, and the reason the fix cannot be
    // "always drop the deltas": a killed or errored turn leaves a delta run
    // whose `Text` never arrives. Dropping it silently would lose the last
    // thing the model said — usually the most interesting row in the file.
    let out = render(
        &at(vec![
            delta("I am about to "),
            delta("run the tests"),
            AgentEvent::Error {
                message: "provider timeout".into(),
                retryable: true,
            },
        ]),
        &no_prompts(),
    );

    assert!(
        out.body.contains("I am about to run the tests"),
        "an unconsolidated delta run survives:\n{}",
        out.body
    );
    assert_eq!(count(&out.body, r#"class="ev say""#), 1, "as one row");
}

#[test]
fn a_reasoning_run_coalesces_into_one_thinking_row() {
    // `Reasoning` really is a fragment stream with no consolidated sibling, so
    // unlike prose it always flushes — the opposite handling from the test
    // above, which is exactly why they are separate buffers.
    let out = render(
        &at(vec![
            AgentEvent::Reasoning {
                delta: "The commit is ".into(),
            },
            AgentEvent::Reasoning {
                delta: "not on master.".into(),
            },
            text("Found it."),
        ]),
        &no_prompts(),
    );

    assert_eq!(count(&out.body, r#"class="ev think""#), 1, "{}", out.body);
    assert!(
        out.body.contains("The commit is not on master."),
        "{}",
        out.body
    );
}

#[test]
fn a_tool_result_is_named_by_its_call_and_takes_its_verdict_from_the_tag() {
    // Witness for module-doc property 4, and for the trap recorded in the
    // tool-event wire notes: `ToolResult` carries NO error field and never
    // has, so a consumer that looks for one renders every failed call as a
    // success. The name and arguments live on the `ToolStart` sharing the
    // `call_id`; the `ToolOutput::Error` tag is the only verdict.
    let out = render(
        &at(vec![
            AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: "c1".into(),
                    name: "bash".into(),
                    input: serde_json::json!({"command": "git status"}),
                },
            },
            AgentEvent::ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::error("fatal: not a git repository"),
                duration_ms: 40,
                speculated: false,
            },
        ]),
        &no_prompts(),
    );

    assert_eq!(
        out.rendered, 1,
        "call and result are one row:\n{}",
        out.body
    );
    assert!(
        out.body.contains(r#"class="ev tool err""#),
        "the Error tag flips the row to failed:\n{}",
        out.body
    );
    assert!(
        out.body.contains("<b>bash</b>"),
        "named from its call:\n{}",
        out.body
    );
    assert!(
        out.body.contains("git status"),
        "arguments come from ToolCall::input:\n{}",
        out.body
    );
    assert!(
        out.body.contains("fatal: not a git repository"),
        "the failure text is shown:\n{}",
        out.body
    );
    assert!(
        !out.body.contains("no result recorded"),
        "the pending placeholder was replaced:\n{}",
        out.body
    );
}

#[test]
fn a_call_that_never_returned_says_so_rather_than_looking_successful() {
    // A killed run leaves `tool_start` with no `tool_result`. The row must
    // read as unfinished; an empty output pane would read as a tool that ran
    // and printed nothing.
    let out = render(
        &at(vec![AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "c9".into(),
                name: "bash".into(),
                input: serde_json::json!({"command": "sleep 600"}),
            },
        }]),
        &no_prompts(),
    );

    assert!(
        out.body.contains("no result recorded"),
        "an unreturned call is marked:\n{}",
        out.body
    );
}

#[test]
fn a_credential_in_the_event_stream_never_reaches_the_transcript() {
    // THE witness for module-doc property 1, and the one with teeth: the
    // transcript is a THIRD egress sink that #817's `redact_dump` does not
    // reach, because it is folded from `session_events` rather than from the
    // dumps. Remove the `clean` call on either the tool arguments or the
    // prose and this goes red — which is the point, since the archive exists
    // to be mailed to someone.
    let secret = "ghp_016C7e4a9b2d3f5081726354ABCDabcd1234";
    let out = render(
        &at(vec![
            AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: "c1".into(),
                    name: "bash".into(),
                    input: serde_json::json!({"command": format!("curl -H 'token: {secret}'")}),
                },
            },
            AgentEvent::ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::Ok {
                    content: format!("echoed {secret}"),
                },
                duration_ms: 10,
                speculated: false,
            },
            text(&format!("I used {secret} to authenticate.")),
        ]),
        &no_prompts(),
    );

    assert!(
        !out.body.contains("ghp_016C7e4a9b2d3f5081726354"),
        "a credential reached the transcript:\n{}",
        out.body
    );
    assert!(
        out.body.contains("[redacted]"),
        "and it was masked rather than dropped:\n{}",
        out.body
    );
    assert!(out.redacted, "the page reports that masking happened");
}

#[test]
fn the_elapsed_column_counts_whole_seconds_and_invents_no_precision() {
    // Witness for module-doc property 2. `events.ts` takes SQLite's
    // second-resolution `CURRENT_TIMESTAMP` at every writer, so a tenth of a
    // second in this column would be a number the renderer made up. The
    // per-call duration below IS millisecond-precise and is printed as such —
    // the two must not be confused, which is why both are asserted here.
    let out = render(
        &journal(vec![
            (
                "2026-08-09 12:00:00",
                AgentEvent::Stage {
                    name: StageKind::Triage,
                },
            ),
            ("2026-08-09 12:00:34", text("thirty-four seconds later")),
            ("2026-08-09 12:02:14", text("two minutes fourteen")),
        ]),
        &no_prompts(),
    );

    assert!(
        out.body.contains(r#"<span class="t">    0s</span>"#),
        "{}",
        out.body
    );
    assert!(
        out.body.contains(r#"<span class="t">   34s</span>"#),
        "{}",
        out.body
    );
    assert!(
        out.body.contains(r#"<span class="t">  134s</span>"#),
        "{}",
        out.body
    );
    assert!(
        !out.body.contains(".0s</span>"),
        "no fabricated decisecond in the elapsed column:\n{}",
        out.body
    );
}

#[test]
fn the_elapsed_clock_crosses_a_day_boundary() {
    // The reason the column goes through a civil-date conversion at all
    // rather than subtracting the HH:MM:SS fields: a session running over
    // midnight would otherwise report a large negative elapsed time.
    let out = render(
        &journal(vec![
            ("2026-08-09 23:59:50", text("before midnight")),
            ("2026-08-10 00:00:10", text("after midnight")),
        ]),
        &no_prompts(),
    );

    assert!(
        out.body.contains(r#"<span class="t">   20s</span>"#),
        "{}",
        out.body
    );
}

#[test]
fn an_unreadable_timestamp_holds_the_previous_reading() {
    // A row's position in the stream is authoritative; a jump back to 0s
    // mid-transcript would read as the start of a second run.
    let out = render(
        &journal(vec![
            ("2026-08-09 12:00:00", text("first")),
            ("2026-08-09 12:00:12", text("twelve")),
            ("", text("no timestamp")),
        ]),
        &no_prompts(),
    );

    assert_eq!(
        count(&out.body, r#"<span class="t">   12s</span>"#),
        2,
        "the unreadable row holds 12s rather than resetting:\n{}",
        out.body
    );
}

#[test]
fn each_turn_opens_with_the_prompt_that_started_it() {
    // The prompt is a column on `executions`, not an event, so it reaches the
    // transcript from the (already redacted) dump. A transcript that opened
    // with the model's answer would be missing the question.
    let mut prompts = HashMap::new();
    prompts.insert(1, "find my lost commit".to_string());
    prompts.insert(2, "now merge it to master".to_string());

    let mut events = at(vec![text("looking")]).events;
    events.push(SessionEventRecord {
        execution_id: 2,
        seq: 0,
        ts: "2026-08-09 12:00:05".into(),
        event: text("merging"),
    });
    let out = render(&SessionJournal { events, skipped: 0 }, &prompts);

    assert!(out.body.contains("find my lost commit"), "{}", out.body);
    assert!(out.body.contains("now merge it to master"), "{}", out.body);
    assert_eq!(
        count(&out.body, r#"class="ev user""#),
        2,
        "one prompt row per turn, not per event:\n{}",
        out.body
    );
}

#[test]
fn every_event_kind_renders_rather_than_being_silently_dropped() {
    // A transcript that quietly skipped the variants it has no bespoke arm
    // for would be a summary claiming to be a replay — and the reader could
    // not tell. The catch-all renders the wire tag plus the whole payload.
    let out = render(
        &at(vec![
            AgentEvent::ProviderFallback {
                from: "anthropic".into(),
                to: "openrouter".into(),
                reason: "529 overloaded".into(),
            },
            AgentEvent::Commit {
                sha: "abc123def456789".into(),
                message: "fix: the thing".into(),
            },
        ]),
        &no_prompts(),
    );

    assert_eq!(out.rendered, 2, "both rendered:\n{}", out.body);
    assert!(
        out.body.contains("provider_fallback"),
        "the wire tag is shown, so a reader can grep raw/ for it:\n{}",
        out.body
    );
    assert!(
        out.body.contains("529 overloaded"),
        "and the whole payload with it:\n{}",
        out.body
    );
    assert!(out.body.contains("fix: the thing"), "{}", out.body);
}

#[test]
fn an_oversized_payload_is_cut_and_says_that_it_was() {
    // Truncation is fine; silent truncation is not. An elided transcript that
    // looks complete is worse evidence than a short one that states the cut.
    let huge = "x".repeat(MAX_EMBEDDED_BYTES + 5_000);
    let out = render(
        &at(vec![
            AgentEvent::ToolStart {
                call: ToolCall {
                    call_id: "c1".into(),
                    name: "bash".into(),
                    input: serde_json::json!({"command": "cat build.log"}),
                },
            },
            AgentEvent::ToolResult {
                call_id: "c1".into(),
                output: ToolOutput::Ok { content: huge },
                duration_ms: 10,
                speculated: false,
            },
        ]),
        &no_prompts(),
    );

    assert!(
        out.body.contains("bytes truncated"),
        "the cut is stated:\n{}",
        &out.body[..out.body.len().min(400)]
    );
}

#[test]
fn truncation_never_splits_a_utf8_character() {
    // The byte budget is a size limit, and slicing a multi-byte sequence in
    // half panics — on exactly the output (a tree-drawing tool, a non-English
    // file) that a fixture of ASCII would never produce.
    let multibyte = "é".repeat(MAX_EMBEDDED_BYTES);
    let clipped = clip(&multibyte);
    assert!(clipped.contains("bytes truncated"));
    assert!(clipped.chars().count() > 0);
}

#[test]
fn markup_in_the_event_stream_cannot_escape_its_row() {
    // Every string here is chosen by a model, an MCP server or a repository.
    // The page has a `default-src 'none'` CSP, but escaping at the sink is
    // what keeps the row structure intact rather than relying on it.
    let out = render(
        &at(vec![text("</pre><img src=x onerror=alert(1)>")]),
        &no_prompts(),
    );

    assert!(
        !out.body.contains("<img"),
        "markup neutralized:\n{}",
        out.body
    );
    assert!(
        out.body.contains("&lt;img"),
        "and shown as text:\n{}",
        out.body
    );
}

#[test]
fn an_unreplayable_event_is_counted_rather_than_hidden() {
    // `SessionJournal::skipped` is the store's count of rows that no longer
    // parse as an `AgentEvent` (a stream recorded before a variant existed).
    // A reader must be able to tell a quiet session from a lossy read.
    let out = render(
        &SessionJournal {
            events: Vec::new(),
            skipped: 3,
        },
        &no_prompts(),
    );

    assert_eq!(out.unparseable, 3);
    assert!(
        out.provenance().contains("older build"),
        "stated on the page: {}",
        out.provenance()
    );
}
