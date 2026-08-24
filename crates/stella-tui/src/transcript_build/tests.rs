//! The fold's witnesses. Every one of these fails without `RunBuilder`,
//! because before it there was no path at all from an event stream to a
//! [`Run`].

use super::*;
use serde_json::json;
use stella_protocol::{ToolCall, ToolOutput};

fn call(id: &str, name: &str, input: serde_json::Value) -> AgentEvent {
    AgentEvent::ToolStart {
        call: ToolCall {
            call_id: id.to_string(),
            name: name.to_string(),
            input,
        },
    }
}

fn ok_result(id: &str, content: &str) -> AgentEvent {
    AgentEvent::ToolResult {
        call_id: id.to_string(),
        output: ToolOutput::Ok {
            content: content.to_string(),
            data: None,
        },
        duration_ms: 9,
        speculated: false,
    }
}

#[test]
fn a_prompt_and_a_tool_call_become_a_turn_with_one_step() {
    let mut b = RunBuilder::new("latex-proof", "z-ai/glm-5.2");
    b.start_turn("fix the overfull hbox warnings");
    b.push(&call("c1", "read_file", json!({"path": "app/main.tex"})));
    b.push(&ok_result("c1", "\\documentclass{article}\n"));
    b.push(&AgentEvent::Text {
        text: "Done.".to_string(),
    });
    b.finish_turn(Status::Ok);

    let run = b.snapshot();
    assert_eq!(run.turns.len(), 1);
    let turn = &run.turns[0];
    assert_eq!(turn.prompt, "fix the overfull hbox warnings");
    assert_eq!(turn.steps.len(), 1, "call and result must fold to ONE step");
    assert_eq!(turn.answer.as_deref(), Some("Done."));
    assert_eq!(turn.status, Status::Ok);
    let call = turn.steps[0].call.as_ref().expect("call");
    assert_eq!(call.header_object, "app/main.tex");
    assert_eq!(call.status, Status::Ok);
}

/// The reason the fold reads events rather than converting `SessionModel`:
/// nothing in that model retains `StepUsage`, so every accounting chip on
/// every step row would render empty.
#[test]
fn step_usage_bills_the_step_it_paid_for() {
    let mut b = RunBuilder::new("run", "m");
    b.start_turn("go");
    b.push(&call("c1", "bash", json!({"command": "ls"})));
    b.push(&ok_result("c1", "a\nb\n"));
    b.push(&AgentEvent::StepUsage {
        step: 1,
        role: Default::default(),
        provider: "openrouter".to_string(),
        upstream_provider: None,
        output_text: None,
        model: "m".to_string(),
        input_tokens: 7_000,
        output_tokens: 34,
        cached_input_tokens: 6_400,
        cache_write_tokens: 0,
        reasoning_tokens: None,
        estimated_input_tokens: 0,
        cost_usd: 0.0008,
        duration_ms: 1_400,
        retries: 0,
        tool_calls: 1,
        complete: true,
        finish_reason: None,
        effort: None,
        max_output_tokens: None,
        sub_agent_id: None,
    });
    b.finish_turn(Status::Ok);

    let acct = b.snapshot().turns[0].steps[0].accounting;
    assert_eq!(acct.tokens_in, 7_000);
    assert_eq!(acct.cached_in, 6_400);
    assert_eq!(acct.micros, 800, "cost must survive as micros");
}

/// The regression the `Note` node exists to prevent, proven end to end: an
/// event with no place in the turn backbone still reaches the transcript.
#[test]
fn a_non_backbone_event_survives_as_a_note() {
    let mut b = RunBuilder::new("run", "m");
    b.start_turn("go");
    b.push(&AgentEvent::ProviderFallback {
        from: "anthropic".to_string(),
        to: "openrouter".to_string(),
        reason: "circuit open".to_string(),
    });
    b.finish_turn(Status::Ok);

    let notes = &b.snapshot().turns[0].notes;
    assert_eq!(notes.len(), 1, "the fallback was dropped");
    assert_eq!(notes[0].kind, NoteKind::Meter);
    assert!(
        notes[0].summary.contains("openrouter"),
        "wording lost: {:?}",
        notes[0].summary
    );
}

#[test]
fn a_failed_tool_marks_its_step_and_its_turn() {
    let mut b = RunBuilder::new("run", "m");
    b.start_turn("go");
    b.push(&call("c1", "bash", json!({"command": "false"})));
    b.push(&AgentEvent::ToolResult {
        call_id: "c1".to_string(),
        output: ToolOutput::Error {
            message: "exit 1".to_string(),
            class: None,
        },
        duration_ms: 3,
        speculated: false,
    });
    b.finish_turn(Status::Ok);

    let run = b.snapshot();
    assert_eq!(run.turns[0].steps[0].status(), Status::Error);
    assert_eq!(
        run.turns[0].status,
        Status::Error,
        "a turn holding a failed step cannot present itself as done"
    );
}

/// Results can arrive out of order under speculative execution, so calls are
/// matched by id rather than by position.
#[test]
fn out_of_order_results_match_their_own_call() {
    let mut b = RunBuilder::new("run", "m");
    b.start_turn("go");
    b.push(&call("a", "read_file", json!({"path": "first.txt"})));
    b.push(&call("b", "read_file", json!({"path": "second.txt"})));
    b.push(&ok_result("b", "second"));
    b.push(&ok_result("a", "first"));
    b.finish_turn(Status::Ok);

    let run = b.snapshot();
    let objects: Vec<_> = run.turns[0]
        .steps
        .iter()
        .filter_map(|s| s.call.as_ref())
        .map(|c| c.header_object.as_str())
        .collect();
    assert_eq!(objects, vec!["second.txt", "first.txt"]);
}

/// A live surface renders between events, so an unresolved call must be
/// visible rather than withheld until it lands.
#[test]
fn a_call_still_in_flight_shows_as_running() {
    let mut b = RunBuilder::new("run", "m");
    b.start_turn("go");
    b.push(&call("c1", "bash", json!({"command": "sleep 30"})));

    let run = b.snapshot();
    let step = &run.turns[0].steps[0];
    assert_eq!(step.status(), Status::Running);
    assert_eq!(run.turns[0].status, Status::Running);
}

#[test]
fn reasoning_deltas_accumulate_into_one_block_per_step() {
    let mut b = RunBuilder::new("run", "m");
    b.start_turn("go");
    b.push(&AgentEvent::Reasoning {
        delta: "I'll read ".to_string(),
    });
    b.push(&AgentEvent::Reasoning {
        delta: "the file.".to_string(),
    });
    b.finish_turn(Status::Ok);

    let prose = &b.snapshot().turns[0].prose;
    assert_eq!(prose.len(), 1, "one block per step, not one per token");
    assert_eq!(prose[0].text, "I'll read the file.");
}

// ------------------------------------------------------------ diff recovery

#[test]
fn a_unified_diff_recovers_both_sides_at_their_original_line_numbers() {
    let diff = "\
@@ -3,2 +3,3 @@
 context
-old line
+new line
+added line
";
    let (before, after) = reconstruct(diff);
    assert_eq!(before, "\n\ncontext\nold line\n");
    assert_eq!(after, "\n\ncontext\nnew line\nadded line\n");
}

#[test]
fn a_diffless_file_change_still_records_the_file() {
    let mut b = RunBuilder::new("run", "m");
    b.start_turn("go");
    b.push(&call("c1", "edit_file", json!({"path": "a.txt"})));
    b.push(&ok_result("c1", "ok"));
    b.push(&AgentEvent::FileChange {
        path: "a.txt".to_string(),
        kind: stella_protocol::FileChangeKind::Modified,
        added: 1,
        removed: 1,
        diff: None,
    });
    b.finish_turn(Status::Ok);

    let call = b.snapshot().turns[0].steps[0].call.clone().expect("call");
    assert_eq!(call.files.len(), 1);
    assert_eq!(call.files[0].path, "a.txt");
}

// -------------------------------------------------- naming and wall clock

/// The header rail names the turn. It shares one line with the status word
/// and the chips, so the name is terse — the prompt itself is rendered in
/// full on the `you` row below it.
#[test]
fn a_turn_is_named_from_its_prompt() {
    let mut b = RunBuilder::new("run", "m");
    b.start_turn("Fix the overfull hbox warnings in main.tex");
    b.finish_turn(Status::Ok);
    assert_eq!(b.snapshot().turns[0].name, "fix-the-overfull-hbox");
}

#[test]
fn a_name_survives_punctuation_and_an_empty_prompt() {
    let mut b = RunBuilder::new("run", "m");
    b.start_turn("  ***  ");
    b.finish_turn(Status::Ok);
    assert_eq!(b.snapshot().turns[0].name, "");

    let mut b = RunBuilder::new("run", "m");
    b.start_turn("re-run CI, please!");
    b.finish_turn(Status::Ok);
    assert_eq!(b.snapshot().turns[0].name, "re-run-ci-please");
}

/// Elapsed time is summed from the durations the events themselves report.
/// The fold reads no clock — one would stop it being a pure fold, and would
/// stop a replay reproducing the same frame.
#[test]
fn wall_clock_is_summed_from_the_events_not_read_from_a_clock() {
    let mut b = RunBuilder::new("run", "m");
    b.start_turn("go");
    b.push(&call("c1", "bash", json!({"command": "one"})));
    b.push(&AgentEvent::ToolResult {
        call_id: "c1".to_string(),
        output: ToolOutput::Ok {
            content: "ok".to_string(),
            data: None,
        },
        duration_ms: 7_000,
        speculated: false,
    });
    b.push(&call("c2", "bash", json!({"command": "two"})));
    b.push(&AgentEvent::ToolResult {
        call_id: "c2".to_string(),
        output: ToolOutput::Ok {
            content: "ok".to_string(),
            data: None,
        },
        duration_ms: 3_000,
        speculated: false,
    });
    b.finish_turn(Status::Ok);

    let turn = &b.snapshot().turns[0];
    assert_eq!(turn.steps[0].offset_ms, 0, "the first step starts at zero");
    assert_eq!(
        turn.steps[1].offset_ms, 7_000,
        "the second step starts when the first finished"
    );
    assert_eq!(turn.duration_ms, 10_000, "the turn is as long as its steps");
}

/// A second turn starts its clock again rather than continuing the first's.
#[test]
fn each_turn_starts_its_own_clock() {
    let mut b = RunBuilder::new("run", "m");
    b.start_turn("first");
    b.push(&call("c1", "bash", json!({"command": "x"})));
    b.push(&AgentEvent::ToolResult {
        call_id: "c1".to_string(),
        output: ToolOutput::Ok {
            content: "ok".to_string(),
            data: None,
        },
        duration_ms: 5_000,
        speculated: false,
    });
    b.start_turn("second");
    b.push(&call("c2", "bash", json!({"command": "y"})));
    b.push(&AgentEvent::ToolResult {
        call_id: "c2".to_string(),
        output: ToolOutput::Ok {
            content: "ok".to_string(),
            data: None,
        },
        duration_ms: 2_000,
        speculated: false,
    });
    b.finish_turn(Status::Ok);

    let run = b.snapshot();
    assert_eq!(run.turns[0].duration_ms, 5_000);
    assert_eq!(
        run.turns[1].duration_ms, 2_000,
        "the clock leaked across turns"
    );
    assert_eq!(run.turns[1].steps[0].offset_ms, 0);
}

#[test]
fn the_run_terminator_does_not_open_a_turn_of_its_own() {
    // The ordinary end of every `stella run`: the engine closes its turn, then
    // whoever owns the run emits `RunComplete` exactly once *after* it
    // (`AgentEvent::RunComplete`'s contract). The terminator arriving against a
    // closed turn is therefore the normal path, not an edge case — and a fold
    // that lazily opens a turn for it manufactures a second, promptless turn
    // that the plain surface then prints as an empty trailing frame.
    let mut b = RunBuilder::new("run", "z-ai/glm-5.2");
    b.start_turn("ship it");
    b.push(&AgentEvent::Text {
        text: "Done.".to_string(),
    });
    b.push(&AgentEvent::TurnComplete {
        model: "z-ai/glm-5.2".to_string(),
        cost_usd: 0.01,
    });
    b.push(&AgentEvent::RunComplete {
        model: "z-ai/glm-5.2".to_string(),
        cost_usd: 0.01,
    });
    b.finish_turn(Status::Ok);

    let run = b.snapshot();
    assert_eq!(
        run.turns.len(),
        1,
        "the run terminator must not materialise a turn; got {:#?}",
        run.turns.iter().map(|t| &t.prompt).collect::<Vec<_>>()
    );
    assert_eq!(run.turns[0].prompt, "ship it");
}

#[test]
fn a_turn_terminator_against_a_closed_turn_is_inert() {
    // Same shape one level down, and the reason the fix covers both
    // terminators rather than just the one the reviewer found: a duplicate or
    // replayed `TurnComplete` would otherwise open a turn and immediately
    // close it, leaving the same empty frame behind.
    let mut b = RunBuilder::new("run", "z-ai/glm-5.2");
    b.start_turn("ship it");
    b.push(&AgentEvent::TurnComplete {
        model: "z-ai/glm-5.2".to_string(),
        cost_usd: 0.01,
    });
    b.push(&AgentEvent::TurnComplete {
        model: "z-ai/glm-5.2".to_string(),
        cost_usd: 0.01,
    });

    assert_eq!(b.snapshot().turns.len(), 1);
}
