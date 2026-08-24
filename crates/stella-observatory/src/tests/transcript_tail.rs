//! The incremental transcript protocol's fold-level witness (#4566).
//!
//! Simulates the poll loop the dashboard runs — journal grows, each tick
//! folds only the rows after the echoed cursor — and holds the composed page
//! to the full render at every tick: what the page keeps (head plus settled
//! fragments) and what it just received (the tail) must together be exactly
//! the transcript a fresh full render would produce. One assertion per tick,
//! over every prefix of a journal that exercises the fold's corners: a
//! double `step_usage` merging backward, an answer that settles behind the
//! cursor, prose between steps, and a call still running at the end.

use serde_json::{Value, json};
use stella_transcript::FoldState;
use stella_transcript::html;

use crate::transcript_view::{self, TailCursor, advance_cursor, build_run_tail};

fn execution() -> Value {
    json!({
        "id": 1,
        "kind": "goal",
        "prompt": "make the tests pass",
        "provider": "zai",
        "model": "glm-5.2",
        "outcome": null,
        "cost_usd": 0.0,
        "started_at": "14:02:11",
        "finished_at": null,
    })
}

fn usage(seq: i64, ts: i64, step: u64, tokens_in: u64) -> Value {
    json!({
        "seq": seq, "ts": ts, "type": "step_usage",
        "step": step, "role": "worker", "provider": "zai", "model": "glm-5.2",
        "input_tokens": tokens_in, "output_tokens": 40,
        "cached_input_tokens": 100, "cache_write_tokens": 10,
        "cost_usd": 0.01, "duration_ms": 900,
    })
}

fn start(seq: i64, ts: i64, id: &str, name: &str, args: Value) -> Value {
    json!({
        "seq": seq, "ts": ts, "type": "tool_start",
        "call_id": id, "name": name,
        "body": serde_json::to_string_pretty(&args).unwrap(),
    })
}

fn result(seq: i64, ts: i64, id: &str, body: &str) -> Value {
    json!({
        "seq": seq, "ts": ts, "type": "tool_result",
        "call_id": id, "ok": true, "duration_ms": 120, "body": body,
    })
}

fn reasoning(seq: i64, ts: i64, text: &str) -> Value {
    json!({ "seq": seq, "ts": ts, "type": "reasoning", "body": text })
}

fn text(seq: i64, ts: i64, body: &str) -> Value {
    json!({ "seq": seq, "ts": ts, "type": "text", "body": body })
}

/// A live journal with five completed calls, so several settle boundaries
/// occur; the answer lands mid-stream (before the last two calls) so a late
/// cursor moves past it and the point-read path runs.
fn journal() -> Vec<Value> {
    vec![
        reasoning(0, 1_000, "I'll read the file first."),
        usage(1, 1_100, 1, 3_000),
        start(2, 1_200, "c1", "read_file", json!({"path": "src/lib.rs"})),
        result(3, 1_500, "c1", "fn a() {}"),
        usage(4, 1_600, 2, 3_200),
        start(5, 1_700, "c2", "bash", json!({"command": "cargo test"})),
        result(6, 2_200, "c2", "ok. 12 passed"),
        usage(7, 2_300, 3, 3_400),
        // Two metering rows with no tool call between them: the first must
        // merge backward into the call already pushed.
        usage(8, 2_350, 3, 3_500),
        start(9, 2_400, "c3", "bash", json!({"command": "cargo build"})),
        result(10, 3_000, "c3", "Compiling"),
        reasoning(11, 3_100, "Nearly there."),
        text(12, 3_200, "All green."),
        start(13, 3_300, "c4", "read_file", json!({"path": "README.md"})),
        result(14, 3_600, "c4", "# readme"),
        start(15, 3_700, "c5", "bash", json!({"command": "cargo doc"})),
        result(16, 4_200, "c5", "Documenting"),
        // Still open at the journal's edge: renders as a running step.
        start(17, 4_300, "c6", "bash", json!({"command": "cargo bench"})),
    ]
}

/// One poll tick, exactly as `render_transcript_tail` performs it, but pure
/// over the row set: filter by the cursor, fold the suffix, recover the
/// answer by point lookup when the suffix carries none, render, split at the
/// settle boundary, advance.
fn tick(rows: &[Value], carry: &TailCursor) -> (String, String, TailCursor) {
    let suffix: Vec<Value> = rows
        .iter()
        .filter(|r| r["seq"].as_i64().unwrap() > carry.seq)
        .cloned()
        .collect();
    let mut carry = *carry;
    if carry.seq < 0 {
        carry.base_ts = suffix.first().and_then(|r| r["ts"].as_i64()).unwrap_or(0);
    }
    let (mut run, facts) = build_run_tail(&execution(), &suffix, &carry);
    if run.turns[0].answer.is_none()
        && let Some(seq) = carry.answer_seq
    {
        let prior = rows
            .iter()
            .find(|r| r["seq"].as_i64() == Some(seq))
            .unwrap();
        run.turns[0].answer = Some(prior["body"].as_str().unwrap().to_string());
    }
    let state = FoldState::new();
    let base = html::TailBase {
        steps: carry.steps,
        notes: carry.notes,
        prose: carry.prose,
        carried: carry.carried,
        prev_offset_ms: carry.prev_offset_ms,
    };
    let tail = html::render_turn_tail(&run, &state, 0, &base);
    let (newly, next) = advance_cursor(&carry, &run.turns[0], &facts);
    let settled = tail.blocks[..newly].concat();
    let mut moving = tail.blocks[newly..].concat();
    moving.push_str(&tail.close);
    (settled, moving, next)
}

/// The protocol's whole claim, asserted at every journal length: the page's
/// composition — the head it never repaints, the settled fragments it
/// appended over the run, and the tail it just received — is byte-identical
/// to a fresh full render of the same journal.
#[test]
fn ticked_composition_matches_the_full_render_at_every_journal_length() {
    let rows = journal();
    let state = FoldState::new();
    let mut carry = TailCursor::start(0);
    let mut settled_acc = String::new();
    let mut head: Option<String> = None;
    for upto in 1..=rows.len() {
        let now = &rows[..upto];
        let (settled, tail, next) = tick(now, &carry);
        settled_acc.push_str(&settled);
        carry = next;

        let full = html::render_run(&transcript_view::build_run(&execution(), now), &state);
        let composed = format!("{settled_acc}{tail}</details></div>");
        assert!(
            full.ends_with(&composed),
            "tick at {upto} rows diverged from the full render\nfull tail:\n…{}\ncomposed:\n…{}",
            &full[full.len().saturating_sub(600)..],
            &composed[composed.len().saturating_sub(600)..],
        );
        // Everything the composition does not cover is the head the page
        // painted once — it must never move while the run is live.
        let head_now = full[..full.len() - composed.len()].to_string();
        match &head {
            None => head = Some(head_now),
            Some(first) => assert_eq!(
                first, &head_now,
                "the never-repainted head changed at {upto} rows"
            ),
        }
    }
}

/// A tick with no journal movement re-renders the same tail and echoes the
/// cursor unchanged — polling while quiet neither settles nor drifts.
#[test]
fn a_quiet_tick_is_idempotent() {
    let rows = journal();
    let mut carry = TailCursor::start(0);
    let mut last = (String::new(), String::new());
    for _ in 0..2 {
        let (settled, tail, next) = tick(&rows, &carry);
        if carry == next {
            assert_eq!(settled, "", "a quiet tick settled markup");
            assert_eq!(tail, last.1, "a quiet tick re-rendered a different tail");
        }
        last = (settled, tail);
        carry = next;
    }
    // The second pass over an unchanged journal is the quiet tick.
    let (settled, tail, next) = tick(&rows, &carry);
    assert_eq!(next, carry);
    assert_eq!(settled, "");
    assert_eq!(tail, last.1);
}

/// The cursor survives its wire round trip bit-for-bit, and a mangled echo
/// reads as no cursor at all rather than as an error.
#[test]
fn cursor_round_trips_and_a_mangled_echo_degrades() {
    let rows = journal();
    let (_, _, cursor) = tick(&rows, &TailCursor::start(0));
    assert_ne!(cursor.seq, -1, "the fixture must settle something");
    assert_eq!(TailCursor::from_value(&cursor.to_value()), Some(cursor));
    assert_eq!(TailCursor::from_value(&json!({"seq": "nope"})), None);
    assert_eq!(TailCursor::from_value(&json!(null)), None);
}
