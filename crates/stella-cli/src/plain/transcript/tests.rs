//! Witnesses for the plain surface's transcript.
//!
//! The first group fails before this module existed, because the plain surface
//! had no path to `grid::render` at all. The streaming group fails before
//! #3764's follow-up, because this surface used to print `· {tool}` per call
//! for the whole turn and draw the real frame only once the turn was over —
//! two renderings of one turn, with the worse one on the surface a human was
//! actually watching.

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
        sub_agent_id: None,
    }
}

fn result(id: &str, content: &str) -> AgentEvent {
    AgentEvent::ToolResult {
        call_id: id.to_string(),
        output: ToolOutput::Ok {
            content: content.to_string(),
            data: None,
        },
        duration_ms: 9,
        speculated: false,
        sub_agent_id: None,
    }
}

fn built(prompt: &str) -> Run {
    let mut b = RunBuilder::new("latex-proof", "z-ai/glm-5.2");
    b.start_turn(prompt);
    b.push(&call("c1", "read_file", json!({"path": "app/main.tex"})));
    b.push(&result("c1", "\\documentclass{article}\n"));
    b.push(&AgentEvent::Text {
        text: "Read it.".to_string(),
    });
    b.finish_turn(Status::Ok);
    b.snapshot()
}

/// The width every test here draws at.
///
/// Pinned rather than measured, because
/// [`the_width_is_clamped_and_then_fixed_for_the_whole_turn`] mutates `COLUMNS`
/// and the harness runs these in parallel threads of one process: a test that
/// read the environment would be reading that one's fixture. It is not a
/// hypothetical — the streaming witness first failed on exactly this, with a
/// closing rail two dozen cells longer than the top rail it was compared to.
const W: usize = 100;

/// A printer drawing at [`W`], for the same reason [`W`] exists.
fn printer() -> TranscriptPrinter {
    let mut p = TranscriptPrinter::new("latex-proof", "z-ai/glm-5.2");
    p.width = W;
    p
}

/// The frame as one string, the way it reaches scrollback.
fn frame_text(run: &Run, index: usize) -> String {
    strip_ansi(&frame(run, index, W).join("\n"))
}

// ------------------------------------------------------------- the frame

/// The frame is the box-drawing turn frame from the reference mockups — the
/// thing four previous attempts never got onto a live surface.
#[test]
fn a_turn_prints_as_a_box_drawn_frame() {
    let out = frame_text(&built("fix the overfull hbox warnings"), 0);

    let first = out.lines().next().expect("a frame has a top rail");
    assert!(
        first.starts_with("╭─"),
        "no top rail — this is not a frame: {first:?}"
    );
    assert!(
        out.lines().any(|l| l.starts_with("╰─")),
        "no bottom rail:\n{out}"
    );
    assert!(
        out.contains("fix the overfull hbox warnings"),
        "the prompt is the anchor and must be in the frame:\n{out}"
    );
    assert!(out.contains("read_file"), "the step row is missing:\n{out}");
}

/// Every rail and row is drawn to the requested width, so the frame closes.
#[test]
fn every_rail_is_drawn_to_the_requested_width() {
    let run = built("go");
    for w in [60usize, 100, 120] {
        let out = strip_ansi(&frame(&run, 0, w).join("\n"));
        for line in out
            .lines()
            .filter(|l| l.starts_with('╭') || l.starts_with('╰'))
        {
            assert_eq!(line.chars().count(), w, "rail is not {w} wide: {line:?}");
        }
    }
}

/// A whole-run footer under a single-turn frame would state something false
/// about the session, so this surface renders one turn at a time rather than
/// calling `grid::render` and trimming.
#[test]
fn a_single_turn_frame_carries_no_whole_run_footer() {
    let out = frame_text(&built("go"), 0);
    let last = out.lines().last().expect("a frame has a last line");
    assert!(
        last.starts_with("╰─"),
        "the run footer leaked under the frame: {last:?}"
    );
}

/// Each turn is written once. A second flush after nothing happened must not
/// re-write a frame that is already in the user's scrollback.
#[test]
fn a_frame_is_never_printed_twice() {
    let mut p = printer();
    p.start_turn("first");
    assert!(!p.flushed().is_empty(), "the first flush wrote nothing");
    assert_eq!(p.printed_turns, 1);
    assert_eq!(p.flushed(), "", "an idle flush re-wrote a turn");
    assert_eq!(p.printed_turns, 1);
}

/// The user-visible half of the fold's terminator bug: `spawn_renderer` flushes
/// once more after the event channel closes, so an event arriving *after* the
/// turn was flushed used to open a promptless turn that the final flush then
/// printed as an empty trailing frame. `RunComplete` always arrives there, so
/// every `stella run` ended with one.
#[test]
fn the_run_terminator_prints_no_trailing_frame() {
    let mut p = printer();
    p.start_turn("ship it");
    p.observed(&AgentEvent::TurnComplete {
        model: "m".to_string(),
        cost_usd: 0.01,
    });
    assert_eq!(p.printed_turns, 1, "the real turn should have printed");
    p.observed(&AgentEvent::RunComplete {
        model: "m".to_string(),
        cost_usd: 0.01,
    });
    // What `spawn_renderer` does once `rx` closes.
    assert_eq!(
        p.flushed(),
        "",
        "the run terminator wrote a second, promptless frame"
    );
    assert_eq!(p.printed_turns, 1);
}

// --------------------------------------------------------- the streaming

/// Everything one printer writes over a turn, in order.
fn streamed(events: &[AgentEvent]) -> String {
    let mut p = printer();
    p.start_turn("fix the overfull hbox warnings");
    let mut out = String::new();
    for event in events {
        out.push_str(&p.observed(event));
    }
    out.push_str(&p.flushed());
    out
}

fn turn_events() -> Vec<AgentEvent> {
    vec![
        AgentEvent::Reasoning {
            delta: "I'll read the file first.".to_string(),
        },
        call("c1", "read_file", json!({"path": "app/main.tex"})),
        result("c1", "\\documentclass{article}\n"),
        call("c2", "bash", json!({"command": "latexmk main.tex"})),
        result("c2", "Overfull \\hbox (3.1pt too wide)\n"),
        AgentEvent::Text {
            text: "Read it.".to_string(),
        },
    ]
}

/// **The property this surface exists to have.** What a human watches scroll
/// past during the run is, byte for byte, the transcript they are left with —
/// there is no second, terser rendering and nothing is redrawn.
///
/// Fails before the streaming rewrite: the old surface wrote `  · read_file`
/// and `  · bash` while the turn ran, erased them with a cursor-up, and only
/// then wrote the frame.
#[test]
fn what_scrolls_past_during_the_turn_is_the_transcript_that_remains() {
    let live = strip_ansi(&streamed(&turn_events()));

    let mut b = RunBuilder::new("latex-proof", "z-ai/glm-5.2");
    b.start_turn("fix the overfull hbox warnings");
    for event in turn_events() {
        b.push(&event);
    }
    b.finish_turn(Status::Ok);
    let finished = format!("{}\n", frame_text(&b.snapshot(), 0));

    assert_eq!(
        live, finished,
        "the streamed scrollback and the finished frame are different documents"
    );
}

/// No placeholder vocabulary reaches scrollback. The `· {tool}` row is the
/// specific thing being ruled out, and a bare tool name with no digest around
/// it is the shape of the defect rather than that one glyph.
#[test]
fn no_progress_placeholder_is_ever_written() {
    let live = strip_ansi(&streamed(&turn_events()));
    for line in live.lines() {
        assert!(
            !line.trim_start().starts_with('·'),
            "a placeholder progress row reached scrollback: {line:?}"
        );
    }
}

/// A terminal and a log file receive the same bytes. This surface used to
/// branch on `stdout().is_terminal()` and print the progress rows only to a
/// terminal, which is why `stella run --foreground` and the same run under the
/// daemon looked like different products.
#[test]
fn the_same_bytes_reach_a_terminal_and_a_log_file() {
    // Stated structurally, because that is the only way to state it: the two
    // destinations differ if and only if some branch asks which one it is
    // writing to. Nothing in this module may ask.
    let source = include_str!("../transcript.rs");
    assert!(
        !source.contains("is_terminal"),
        "the surface grew a terminal-only branch again — that is the divergence"
    );
}

/// A dispatched call reaches scrollback when its **result** does, not when it
/// dispatches. Its row carries the duration, the output fold and the cost, so
/// writing it at dispatch would mean rewriting a line already committed.
#[test]
fn a_call_reaches_scrollback_only_once_it_has_settled() {
    let mut p = printer();
    p.start_turn("go");
    let on_dispatch = strip_ansi(&p.observed(&call("c1", "bash", json!({"command": "ls -la"}))));
    assert!(
        !on_dispatch.contains("ls -la"),
        "an in-flight call was committed to scrollback: {on_dispatch:?}"
    );
    let on_result = strip_ansi(&p.observed(&result("c1", "main.tex\n")));
    assert!(
        on_result.contains("ls -la"),
        "the settled call never arrived: {on_result:?}"
    );
}

/// The closing rail is withheld until the turn ends, because it is the one
/// line carrying facts that do not exist while the turn runs.
#[test]
fn the_closing_rail_arrives_only_with_the_turn_it_closes() {
    let mut p = printer();
    p.start_turn("go");
    let during = strip_ansi(&p.observed(&result("c1", "x")));
    assert!(
        !during.contains('╰'),
        "a running turn wrote its closing rail: {during:?}"
    );
    assert!(
        strip_ansi(&p.flushed()).contains('╰'),
        "the finished turn never closed its frame"
    );
}

/// The width clamp keeps a frame readable on a very wide terminal and legal on
/// a narrow one — and a turn is drawn at **one** width for its whole life.
///
/// Both halves live in one test because both mutate `COLUMNS`, which is
/// process-global while the harness runs tests in parallel threads. Split, they
/// would read each other's fixture; that is exactly how the streaming witness
/// first failed, with a rail two dozen cells too long.
///
/// The second half is the one that matters at runtime. A streamed frame cannot
/// be redrawn, so a printer that re-measured the terminal would close a resized
/// turn at a width its own top rail was never drawn to.
#[test]
fn the_width_is_clamped_and_then_fixed_for_the_whole_turn() {
    // SAFETY: the only test in this module that touches `COLUMNS`, by design.
    unsafe { std::env::set_var("COLUMNS", "400") };
    assert_eq!(width(), MAX_WIDTH);
    unsafe { std::env::set_var("COLUMNS", "10") };
    assert_eq!(width(), 40);
    unsafe { std::env::remove_var("COLUMNS") };
    assert_eq!(width(), FALLBACK_WIDTH);

    unsafe { std::env::set_var("COLUMNS", "100") };
    let mut p = TranscriptPrinter::new("run", "m");
    p.start_turn("go");
    let opened = strip_ansi(&p.observed(&result("c1", "x")));
    unsafe { std::env::set_var("COLUMNS", "60") };
    let closed = strip_ansi(&p.flushed());
    unsafe { std::env::remove_var("COLUMNS") };

    let rails: Vec<usize> = opened
        .lines()
        .chain(closed.lines())
        .filter(|l| l.starts_with('╭') || l.starts_with('╰'))
        .map(|l| l.chars().count())
        .collect();
    assert!(
        rails.len() >= 2,
        "expected an opening and a closing rail, got {rails:?}"
    );
    assert!(
        rails.iter().all(|&w| w == 100),
        "the terminal was re-measured mid-turn and the frame tore: {rails:?}"
    );
}

/// Strip SGR sequences so an assertion reads the characters, not the paint.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        for c in chars.by_ref() {
            if c == 'm' {
                break;
            }
        }
    }
    out
}
