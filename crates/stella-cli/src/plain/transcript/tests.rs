//! Witnesses for the plain surface's frames. Each fails before this module
//! existed, because the plain surface had no path to `grid::render` at all —
//! the renderer's only callers were its own tests and one example.

use super::*;
use serde_json::json;
use stella_protocol::{ToolCall, ToolOutput};

fn built(prompt: &str) -> Run {
    let mut b = RunBuilder::new("latex-proof", "z-ai/glm-5.2");
    b.start_turn(prompt);
    b.push(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".to_string(),
            name: "read_file".to_string(),
            input: json!({"path": "app/main.tex"}),
        },
    });
    b.push(&AgentEvent::ToolResult {
        call_id: "c1".to_string(),
        output: ToolOutput::Ok {
            content: "\\documentclass{article}\n".to_string(),
            data: None,
        },
        duration_ms: 9,
        speculated: false,
    });
    b.push(&AgentEvent::Text {
        text: "Read it.".to_string(),
    });
    b.finish_turn(Status::Ok);
    b.snapshot()
}

/// The frame is the box-drawing turn frame from the reference mockups — the
/// thing four previous attempts never got onto a live surface.
#[test]
fn a_turn_prints_as_a_box_drawn_frame() {
    let run = built("fix the overfull hbox warnings");
    let out = strip_ansi(&frame(&run, 0, 100));

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
    for width in [60usize, 100, 120] {
        let out = strip_ansi(&frame(&run, 0, width));
        for line in out.lines().filter(|l| l.starts_with('╭') || l.starts_with('╰')) {
            assert_eq!(
                line.chars().count(),
                width,
                "rail is not {width} wide: {line:?}"
            );
        }
    }
}

/// A whole-run footer under a single-turn frame would state something false
/// about the session, so it is dropped.
#[test]
fn a_single_turn_frame_carries_no_whole_run_footer() {
    let out = strip_ansi(&frame(&built("go"), 0, 100));
    let last = out.lines().last().expect("a frame has a last line");
    assert!(
        last.starts_with("╰─"),
        "the run footer leaked under the frame: {last:?}"
    );
}

/// Each turn is printed once. A second flush after nothing happened must not
/// re-print a frame that is already in the user's scrollback.
#[test]
fn a_frame_is_never_printed_twice() {
    let mut p = TranscriptPrinter::new("run", "m");
    p.interactive = false;
    p.start_turn("first");
    p.flush();
    assert_eq!(p.printed_turns, 1);
    p.flush();
    assert_eq!(p.printed_turns, 1, "an idle flush re-printed a turn");
}

/// Nothing is printed before the frame when no human is watching, so a log
/// file holds the transcript rather than the transcript plus a progress trace
/// that repeats it.
#[test]
fn a_non_interactive_surface_prints_no_progress_rows() {
    let mut p = TranscriptPrinter::new("run", "m");
    p.interactive = false;
    p.start_turn("go");
    p.observe(&AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".to_string(),
            name: "bash".to_string(),
            input: json!({"command": "ls"}),
        },
    });
    assert_eq!(
        p.progress_rows, 0,
        "a log file must not receive progress rows"
    );
}

/// The width clamp keeps a frame readable on a very wide terminal and legal on
/// a narrow one.
#[test]
fn width_is_clamped_at_both_ends() {
    // SAFETY: single-threaded test process; the var is read, not cached.
    unsafe { std::env::set_var("COLUMNS", "400") };
    assert_eq!(width(), MAX_WIDTH);
    unsafe { std::env::set_var("COLUMNS", "10") };
    assert_eq!(width(), 40);
    unsafe { std::env::remove_var("COLUMNS") };
    assert_eq!(width(), FALLBACK_WIDTH);
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

