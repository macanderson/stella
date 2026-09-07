//! The recorded turn this module cannot reach, and the pin that keeps saying
//! so (`doc:adr/0032-a-loop-rung-reads-structure`).
//!
//! `extract_moves_from_video.json` holds one bench trial's tool history. It
//! made 62 shell calls. It called nothing else. It produced no answer.
//! `#3292` opened on it as a gap in this module.
//!
//! Three candidate signals were then measured over 497 recorded trials. None
//! of them tells this turn apart from the turns that won. What is wrong with
//! it is not in its calls. The commands vary. The answers vary. The narration
//! names a fresh next step each time. The approach it is working on simply
//! never gets there.
//!
//! ADR 0032 makes that a decision rather than a gap. A rung here reads
//! structure: a repeat, a cycle, a constant answer, a wrapped sweep. It never
//! asks whether the turn is getting closer to the goal. So this trace is the
//! module's edge. The tests below hold both halves of it. What the trace
//! contains, and that nothing fires on it.
//!
//! The pin is what turns a future rung into a decision. A new rung that
//! fires here reddens `no_rung_fires_at_any_step_of_the_recorded_grind`. Its
//! author then has to say in review why firing is right. The
//! self-appending rung (`#5863`) is the first one added since this pin,
//! and it stays silent on the trace.

use std::collections::BTreeSet;

use serde::Deserialize;

use super::*;

/// The trial's tool history, in order: every `tool_start` paired with the
/// `tool_result` that answered it.
///
/// Extracted from
/// `~/.arenabench/matches/5a38720c145f/jobs/5a38720c145f-stella-glm-5-2-via-openrouter-bare-loop/extract-moves-from-video__3AqwMdY/agent/stella-events.jsonl`,
/// which lives on one machine. Committing the calls is what makes the claim
/// this issue turns on checkable by anyone.
const TRACE: &str = include_str!("extract_moves_from_video.json");

/// One recorded call: the tool, the arguments it was given, and the answer it
/// produced — `ok` for a completed run, `error` for a failed one, exactly one
/// of the two.
#[derive(Deserialize)]
struct Recorded {
    tool: String,
    input: serde_json::Value,
    #[serde(default)]
    ok: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

fn recorded_grind() -> Vec<CallRecord<'static>> {
    let rows: Vec<Recorded> = serde_json::from_str(TRACE).expect("the fixture parses");
    rows.into_iter()
        .map(|row| {
            let output = match (row.ok, row.error) {
                (Some(content), None) => ToolOutput::Ok {
                    content,
                    data: None,
                },
                (None, Some(message)) => ToolOutput::error(message),
                _ => panic!("a recorded call carries exactly one of `ok` and `error`"),
            };
            record(&row.tool, row.input, Some(output))
        })
        .collect()
}

/// What the trace is, held to the numbers rather than to prose.
///
/// The census in `#3292` says "62 distinct tool inputs". It is 59. Three
/// commands are sent again later in the turn.
///
/// That does not wake the input-keyed rungs. Each of them also needs the
/// answer to come back unchanged, and all 62 answers differ. The count still
/// matters. A reader who believes no input ever repeats reaches for the
/// wrong rung next.
#[test]
fn the_recorded_grind_varies_both_its_inputs_and_its_outputs() {
    let records = recorded_grind();
    assert_eq!(records.len(), 62, "the trial made 62 tool calls");

    let tools: BTreeSet<&str> = records
        .iter()
        .map(|record| record.call.name.as_str())
        .collect();
    assert_eq!(
        tools,
        BTreeSet::from(["bash"]),
        "every call in this turn is a shell call — no other tool is reached"
    );

    let inputs: BTreeSet<String> = records
        .iter()
        .map(|record| record.call.input.to_string())
        .collect();
    assert_eq!(
        inputs.len(),
        59,
        "59 of the 62 commands are distinct: three are re-sent"
    );

    let outputs: BTreeSet<String> = records
        .iter()
        .map(|record| format!("{:?}", record.output))
        .collect();
    assert_eq!(
        outputs.len(),
        62,
        "no two calls in this turn came back with the same bytes, which is \
         what silences every rung defined on a repeated answer"
    );
}

/// **The boundary pin.** Loop detection is asked the question the driver asks
/// it — after every call, over the turn so far — and answers `NoLoop` every
/// time.
///
/// The window is a prefix rather than a fixed slice because
/// `driver::loop_evidence` bounds the window at the last genuine user turn,
/// which here is the start of the run: at step *n* the detector sees all *n*
/// calls made so far. Sixty-two questions, sixty-two `NoLoop`s.
///
/// This is not a wish. It is the shape of the trace: three of the five rungs
/// need one input to come back twice, the fourth needs one answer to come
/// back twice, the fifth needs cursors. The trace has none of those, and ADR
/// 0032 decides that no rung here will be built to reach it.
#[test]
fn no_rung_fires_at_any_step_of_the_recorded_grind() {
    let records = recorded_grind();
    for step in 1..=records.len() {
        assert_eq!(
            detect_loop(&records[..step], LoopDetectionConfig::default()),
            LoopVerdict::NoLoop,
            "a rung fired at step {step} of a turn whose calls, answers and \
             narration all vary — read ADR 0032 before changing this number"
        );
    }
}

/// Anti-vacuity for the pin above: the same fixture, with its answers
/// flattened to one constant, does trip a rung.
///
/// Without this, a bug that made `detect_loop` return `NoLoop` for every
/// input would pass the boundary pin and prove nothing at all.
#[test]
fn the_fixture_is_detectable_once_its_answers_stop_changing() {
    let flattened: Vec<CallRecord<'static>> = recorded_grind()
        .into_iter()
        .map(|record| {
            call(
                &record.call.name,
                record.call.input.clone(),
                "the same answer every time",
            )
        })
        .collect();
    assert_eq!(
        detect_loop(&flattened, LoopDetectionConfig::default()),
        LoopVerdict::Stagnant {
            tool: "bash".to_string(),
            count: 62,
        },
        "62 shell calls that all answer identically are stagnation — so the \
         pin above reports a property of the trace, not a broken detector"
    );
}
