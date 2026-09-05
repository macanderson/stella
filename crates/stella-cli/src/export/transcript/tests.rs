//! Tests for [`crate::export::transcript`] — the redacting fold from a
//! session's event journal to a [`stella_transcript::model::Run`], plus the
//! call into [`stella_transcript::html::render_page`].
//!
//! Most of these check the **run**, not the rendered HTML. Rendering itself
//! is `stella-transcript`'s own job, and it owns its own tests for what a
//! `Run` looks like. This crate still owns what the fold puts *into* the
//! run: one message per turn, a measured (not invented) elapsed clock, no
//! event dropped, and no credential anywhere in the tree. Break any of
//! those and these tests go red no matter how well `html::render_page`
//! draws the result. A few tests at the bottom check the rendered document
//! directly, for the properties that only show up once the two are wired
//! together.

use std::collections::HashMap;

use stella_protocol::{AgentEvent, ToolCall, ToolOutput};
use stella_store::{SessionEventRecord, SessionJournal};

use super::*;

/// A journal from `(ts, event)` pairs, all in one execution. The timestamp is
/// never read by this fold (module doc, property 2) — it exists only because
/// [`SessionEventRecord`] carries one.
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

/// Fold `journal` the same way [`render`] does, stopping short of redaction
/// and the call into `html::render_page` — the seam these tests assert
/// against, so a rendering change in `stella-transcript` cannot turn one of
/// them red.
fn fold_run(journal: &SessionJournal, prompts: &HashMap<i64, String>, session_id: &str) -> Run {
    let mut fold = Fold::new(prompts, session_id);
    for record in &journal.events {
        fold.push(record);
    }
    fold.finish_turn(Status::Ok);
    fold.run
}

fn tool_start(call_id: &str, name: &str, input: serde_json::Value) -> AgentEvent {
    AgentEvent::ToolStart {
        call: ToolCall {
            call_id: call_id.into(),
            name: name.into(),
            input,
        },
        sub_agent_id: None,
        task_id: None,
    }
}

fn tool_result(call_id: &str, output: ToolOutput, duration_ms: u64) -> AgentEvent {
    AgentEvent::ToolResult {
        call_id: call_id.into(),
        output,
        duration_ms,
        speculated: false,
        sub_agent_id: None,
        task_id: None,
    }
}

// ── One message, one clock, nothing dropped ─────────────────────────────

#[test]
fn a_streamed_message_collapses_to_the_turns_answer_once() {
    // `AgentEvent::Text`'s own contract says a consumer must REPLACE any
    // accumulated preview, never append to it. `set_answer` overwrites
    // `turn.answer` rather than appending, which is what this checks.
    // Change it to an append and the prose shows up twice.
    let run = fold_run(
        &at(vec![
            delta("Step 1 done: "),
            delta("the tree is clean."),
            text("Step 1 done: the tree is clean."),
        ]),
        &no_prompts(),
        "s",
    );
    assert_eq!(run.turns.len(), 1);
    assert_eq!(
        run.turns[0].answer.as_deref(),
        Some("Step 1 done: the tree is clean.")
    );
}

#[test]
fn an_unconsolidated_delta_run_still_reaches_the_answer() {
    // A killed or errored turn leaves a delta run whose `Text` never
    // arrives. `append_answer` writes straight into `turn.answer` as each
    // delta lands, so nothing needs an explicit flush to survive this case.
    let run = fold_run(
        &at(vec![
            delta("I am about to "),
            delta("run the tests"),
            AgentEvent::Error {
                message: "provider timeout".into(),
                retryable: true,
            },
        ]),
        &no_prompts(),
        "s",
    );
    assert_eq!(
        run.turns[0].answer.as_deref(),
        Some("I am about to run the tests")
    );
    assert_eq!(run.turns[0].status, Status::Error, "the error flips it");
}

#[test]
fn a_reasoning_run_coalesces_into_one_prose_block() {
    let run = fold_run(
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
        "s",
    );
    assert_eq!(run.turns[0].prose.len(), 1, "{:?}", run.turns[0].prose);
    assert_eq!(run.turns[0].prose[0].text, "The commit is not on master.");
}

#[test]
fn every_event_kind_becomes_a_note_rather_than_being_silently_dropped() {
    // The catch-all this fold's completeness rests on: an event kind with no
    // backbone arm still carries its wire tag and its whole payload.
    let run = fold_run(
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
        "s",
    );
    let notes = &run.turns[0].notes;
    assert_eq!(notes.len(), 2, "both became notes: {notes:?}");
    assert_eq!(
        notes[0].summary, "provider_fallback",
        "the wire tag is shown"
    );
    assert!(
        notes[0].detail.iter().any(|l| l.contains("529 overloaded")),
        "and the whole payload with it: {:?}",
        notes[0].detail
    );
    assert_eq!(notes[1].summary, "commit");
    assert!(notes[1].detail.iter().any(|l| l.contains("fix: the thing")));
}

#[test]
fn an_unreplayable_event_is_counted_rather_than_hidden() {
    // `SessionJournal::skipped` is the store's count of rows that no longer
    // parse as an `AgentEvent`. A reader must be able to tell a quiet
    // session from a lossy read.
    let out = render(
        &SessionJournal {
            events: Vec::new(),
            skipped: 3,
        },
        &no_prompts(),
        "s",
    );
    assert_eq!(out.unparseable, 3);
    assert!(
        out.provenance().contains("older build"),
        "{}",
        out.provenance()
    );
}

// ── Tool calls ────────────────────────────────────────────────────────────

#[test]
fn a_tool_result_is_named_by_its_call_and_takes_its_verdict_from_the_tag() {
    // `ToolResult` carries no `error` field and never has — the `ToolOutput`
    // tag is the only verdict, and the name/arguments live on the matching
    // `ToolStart`.
    let run = fold_run(
        &at(vec![
            tool_start("c1", "bash", serde_json::json!({"command": "git status"})),
            tool_result("c1", ToolOutput::error("fatal: not a git repository"), 40),
        ]),
        &no_prompts(),
        "s",
    );
    let steps = &run.turns[0].steps;
    assert_eq!(steps.len(), 1, "call and result are one step");
    let call = steps[0].call.as_ref().expect("the step carries its call");
    assert_eq!(call.tool, ToolKind::Bash);
    assert_eq!(
        call.status,
        Status::Error,
        "the Error tag flips the call to failed"
    );
    assert_eq!(call.header_object, "git status");
    assert!(
        call.output
            .lines
            .iter()
            .any(|l| l.contains("fatal: not a git repository")),
        "{:?}",
        call.output.lines
    );
}

#[test]
fn a_delegates_call_is_named_apart_from_the_leads() {
    // The witness for #4699: a delegate's call carries `sub_agent_id`; the
    // lead's own carries none.
    let child_start = AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "child".into(),
            name: "search".into(),
            input: serde_json::json!({"query": "retry"}),
        },
        sub_agent_id: Some("d:1".into()),
        task_id: None,
    };
    let child_result = AgentEvent::ToolResult {
        call_id: "child".into(),
        output: ToolOutput::ok("retry.rs"),
        duration_ms: 20,
        speculated: false,
        sub_agent_id: Some("d:1".into()),
        task_id: None,
    };
    let run = fold_run(
        &at(vec![
            tool_start("lead", "bash", serde_json::json!({"command": "git status"})),
            tool_result("lead", ToolOutput::ok("clean"), 10),
            child_start,
            child_result,
        ]),
        &no_prompts(),
        "s",
    );
    let steps = &run.turns[0].steps;
    assert_eq!(steps[0].call.as_ref().unwrap().sub_agent_id, None);
    assert_eq!(
        steps[1].call.as_ref().unwrap().sub_agent_id.as_deref(),
        Some("d:1")
    );
}

#[test]
fn a_call_that_never_returned_stays_running_rather_than_looking_successful() {
    // A killed run leaves `tool_start` with no `tool_result`. The step must
    // read as unfinished — an empty, `Ok`-status output would read as a tool
    // that ran and printed nothing.
    let run = fold_run(
        &at(vec![tool_start(
            "c9",
            "bash",
            serde_json::json!({"command": "sleep 600"}),
        )]),
        &no_prompts(),
        "s",
    );
    let call = run.turns[0].steps[0].call.as_ref().unwrap();
    assert_eq!(call.status, Status::Running);
}

#[test]
fn a_result_whose_call_was_never_recorded_still_renders() {
    // This journal never saw the matching `tool_start` — a `?after_seq` page,
    // or a store whose early rows were pruned. The completeness contract
    // this fold exists to hold forbids treating the correlation miss as a
    // reason to drop the result.
    let run = fold_run(
        &at(vec![tool_result("ghost", ToolOutput::ok("survived"), 5)]),
        &no_prompts(),
        "s",
    );
    let steps = &run.turns[0].steps;
    assert_eq!(steps.len(), 1, "the orphan result still becomes a step");
    let call = steps[0].call.as_ref().unwrap();
    assert!(call.output.lines.iter().any(|l| l == "survived"));
}

#[test]
fn an_oversized_tool_output_is_clipped_and_says_so() {
    let huge = "x".repeat(MAX_EMBEDDED_BYTES + 5_000);
    let run = fold_run(
        &at(vec![
            tool_start(
                "c1",
                "bash",
                serde_json::json!({"command": "cat build.log"}),
            ),
            tool_result("c1", ToolOutput::ok(huge), 10),
        ]),
        &no_prompts(),
        "s",
    );
    let call = run.turns[0].steps[0].call.as_ref().unwrap();
    let joined = call.output.lines.join("\n");
    assert!(joined.contains("bytes truncated"), "{joined}");
    assert!(
        joined.len() < MAX_EMBEDDED_BYTES + 200,
        "the output was actually cut, not just labelled"
    );
}

// ── File diffs ──────────────────────────────────────────────────────────

#[test]
fn a_file_change_attaches_to_the_call_that_produced_it() {
    let run = fold_run(
        &at(vec![
            tool_start("c1", "edit_file", serde_json::json!({"path": "src/big.rs"})),
            tool_result("c1", ToolOutput::ok("ok"), 5),
            AgentEvent::FileChange {
                path: "src/big.rs".into(),
                kind: stella_protocol::FileChangeKind::Modified,
                added: 3,
                removed: 1,
                diff: Some(
                    "--- a/src/big.rs\n+++ b/src/big.rs\n@@ -1,1 +1,3 @@\n-old\n+new1\n+new2\n+new3\n"
                        .into(),
                ),
                minimal: true,
                task_id: None,
            },
        ]),
        &no_prompts(),
        "s",
    );
    let call = run.turns[0].steps[0].call.as_ref().unwrap();
    assert_eq!(call.files.len(), 1);
    assert_eq!(call.files[0].extent.added, Some(3));
    assert_eq!(call.files[0].extent.removed, Some(1));
    assert!(
        call.files[0]
            .patch
            .as_ref()
            .expect("a diff was supplied")
            .text
            .contains("+new1")
    );
}

// ── Elapsed clock ─────────────────────────────────────────────────────────

#[test]
fn step_offsets_sum_from_measured_durations_not_from_the_journal_clock() {
    // Module-doc property 2, made structural: this fold never reads
    // `events.ts`, so every event in this journal shares one timestamp and
    // the offsets below can only have come from `duration_ms`.
    let run = fold_run(
        &at(vec![
            tool_start("a", "bash", serde_json::json!({"command": "one"})),
            tool_result("a", ToolOutput::ok(""), 1500),
            tool_start("b", "bash", serde_json::json!({"command": "two"})),
            tool_result("b", ToolOutput::ok(""), 2500),
        ]),
        &no_prompts(),
        "s",
    );
    let steps = &run.turns[0].steps;
    assert_eq!(steps[0].offset_ms, 0);
    assert_eq!(steps[1].offset_ms, 1500);
    assert_eq!(run.turns[0].duration_ms, 4000);
}

// ── Turns ───────────────────────────────────────────────────────────────

#[test]
fn each_turn_opens_with_the_prompt_that_started_it() {
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
    let run = fold_run(&SessionJournal { events, skipped: 0 }, &prompts, "s");

    assert_eq!(run.turns.len(), 2);
    assert_eq!(run.turns[0].prompt, "find my lost commit");
    assert_eq!(run.turns[1].prompt, "now merge it to master");
}

#[test]
fn the_row_budget_caps_steps_and_notes_and_reports_the_overflow() {
    let events: Vec<AgentEvent> = (0..MAX_ROWS + 5)
        .map(|i| AgentEvent::Commit {
            sha: format!("sha{i}"),
            message: format!("m{i}"),
        })
        .collect();
    let out = render(&at(events), &no_prompts(), "s");
    assert_eq!(out.rendered, MAX_ROWS);
    assert_eq!(out.overflow, 5);
}

// ── Redaction ──────────────────────────────────────────────────────────

#[test]
fn a_credential_anywhere_in_the_run_is_masked_by_the_whole_tree_pass() {
    // THE witness for module-doc property 1: `redact_run` serializes the
    // whole built run and redacts every string in it, so a credential in the
    // prompt, a tool argument, a tool result, and the answer are all caught
    // by the same pass rather than by four separate call sites.
    let secret = "ghp_016C7e4a9b2d3f5081726354ABCDabcd1234";
    let mut prompts = HashMap::new();
    prompts.insert(1, format!("use {secret} to auth"));

    let mut run = fold_run(
        &at(vec![
            tool_start(
                "c1",
                "bash",
                serde_json::json!({"command": format!("curl -H 'token: {secret}'")}),
            ),
            tool_result("c1", ToolOutput::ok(format!("echoed {secret}")), 10),
            text(&format!("I used {secret} to authenticate.")),
        ]),
        &prompts,
        "s",
    );

    let redacted = redact_run(&mut run);
    assert!(redacted, "the pass reports that masking happened");
    let json = serde_json::to_string(&run).unwrap();
    assert!(!json.contains(secret), "a credential survived redaction");
    assert!(json.contains("[redacted]"), "masked, not dropped");
}

#[test]
fn a_run_with_no_credential_reports_no_redaction() {
    let mut run = fold_run(
        &at(vec![text("nothing sensitive here")]),
        &no_prompts(),
        "s",
    );
    assert!(!redact_run(&mut run));
}

// ── End to end: the rendered document ────────────────────────────────────

#[test]
fn the_rendered_document_needs_no_script_to_read() {
    let out = render(&SessionJournal::default(), &no_prompts(), "s");
    assert!(
        !out.html.contains("<script"),
        "the transcript is pure markup and CSS:\n{}",
        out.html
    );
    assert!(out.html.starts_with("<!DOCTYPE html>"));
}

#[test]
fn a_credential_never_reaches_the_rendered_document() {
    let secret = "ghp_016C7e4a9b2d3f5081726354ABCDabcd1234";
    let out = render(
        &at(vec![text(&format!("I used {secret} to authenticate."))]),
        &no_prompts(),
        "s",
    );
    assert!(
        !out.html.contains(secret),
        "a credential reached the rendered document"
    );
    assert!(out.html.contains("[redacted]"));
    assert!(out.redacted);
}

#[test]
fn markup_in_the_event_stream_cannot_escape_its_row() {
    // Every string here is chosen by a model, an MCP server or a repository.
    // `stella_transcript::html::escape` is what neutralizes it; this checks
    // the fold actually hands the text to the renderer rather than
    // pre-formatting it into markup of its own.
    let out = render(
        &at(vec![text("</pre><img src=x onerror=alert(1)>")]),
        &no_prompts(),
        "s",
    );
    assert!(
        !out.html.contains("<img"),
        "markup neutralized:\n{}",
        out.html
    );
}
