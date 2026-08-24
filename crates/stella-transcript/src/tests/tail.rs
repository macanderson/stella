//! The HTML tail contract (#4566): a settled step group's markup is final,
//! and a tail rendered from a partial model splices byte-for-byte into the
//! full render.
//!
//! These two properties are what let a live page repaint only what changed —
//! the settled DOM it keeps must be exactly what a fresh full render would
//! have produced, or the incremental page and the reloaded page become two
//! documents.

use super::*;

/// A live turn mid-run: a finished bash step, a finished edit step (file
/// diff, so file and hunk ids are covered), a meter note ahead of step 1,
/// prose ahead of step 0, a trailing prose block after the last step, and a
/// still-running step at the end.
fn live_run() -> Run {
    let mut running = bash("cargo test -p stella-core", &[], Status::Running);
    running.duration_ms = 0;
    Run {
        name: "latex-proof".to_string(),
        model: "z-ai/glm-5.2".to_string(),
        started_at: "14:02:11".to_string(),
        turns: vec![Turn {
            name: "fix-overfull".to_string(),
            prompt: "fix the overfull hbox warnings in main.tex".to_string(),
            prose: vec![
                Prose {
                    text: "I'll read the file first. Then fix the warnings.".to_string(),
                    before_step: 0,
                },
                Prose {
                    text: "Now the tests.".to_string(),
                    before_step: 3,
                },
            ],
            notes: vec![Note {
                kind: NoteKind::Meter,
                summary: "step 1 · worker · zai · glm-5.2".to_string(),
                detail: vec!["12.0k in · 450 out".to_string()],
                before_step: 1,
                inspect: Some(CallAnchor {
                    step: 1,
                    role: "worker".to_string(),
                }),
            }],
            steps: vec![
                step(
                    bash("grep -n hbox main.log", &["l.12 overfull"], Status::Ok),
                    800,
                ),
                step(edit("main.tex", "old line\n", "new line\n"), 2_400),
                Step {
                    call: Some(running),
                    accounting: Accounting::default(),
                    offset_ms: 0,
                },
            ],
            answer: None,
            status: Status::Running,
            duration_ms: 2_400,
        }],
    }
}

/// The tail-side model for a split at step `s`: the turn's suffix with local
/// indices, plus the [`html::TailBase`] describing what the prefix
/// contributed. This is by hand what the Observatory's resumable fold
/// produces from a journal suffix.
fn split_at(run: &Run, s: usize) -> (Run, html::TailBase) {
    let turn = &run.turns[0];
    let base = html::TailBase {
        steps: s,
        notes: turn.notes.iter().filter(|n| n.before_step < s).count(),
        prose: turn.prose.iter().filter(|p| p.before_step < s).count(),
        carried: turn.steps[..s]
            .iter()
            .fold(Accounting::default(), |a, st| a.merged(st.accounting)),
        prev_offset_ms: turn.steps[..s].last().map(|st| st.offset_ms),
    };
    let mut tail = turn.clone();
    tail.steps = turn.steps[s..].to_vec();
    tail.notes = turn
        .notes
        .iter()
        .filter(|n| n.before_step >= s)
        .cloned()
        .map(|mut n| {
            n.before_step -= s;
            n
        })
        .collect();
    tail.prose = turn
        .prose
        .iter()
        .filter(|p| p.before_step >= s)
        .cloned()
        .map(|mut p| {
            p.before_step -= s;
            p
        })
        .collect();
    // The head owns the prompt; the tail never renders it.
    tail.prompt = String::new();
    (
        Run {
            turns: vec![tail],
            ..run.clone()
        },
        base,
    )
}

/// #4566's splice witness: at every step boundary, the head plus the tail
/// fragments reproduce the full render exactly. Fold ids, the time column's
/// dedup, the receipt's whole-turn chips and the pinned-open running step all
/// ride in the compared bytes.
#[test]
fn tail_splices_into_the_full_render_at_every_step_boundary() {
    let run = live_run();
    let state = FoldState::new();
    let full = html::render_run(&run, &state);
    for s in 0..=run.turns[0].steps.len() {
        let (tail_run, base) = split_at(&run, s);
        let tail = html::render_turn_tail(&tail_run, &state, 0, &base);
        let spliced = format!(
            "{}{}{}</details></div>",
            html::render_run_prefix(&run, &state, 0, s),
            tail.blocks.concat(),
            tail.close,
        );
        assert_eq!(spliced, full, "splice diverged at step boundary {s}");
    }
}

/// The finality property the splice rests on: as a running turn grows — a new
/// step lands, trailing prose arrives — the bytes of the head and of every
/// already-settled step group do not move.
#[test]
fn settled_prefix_is_byte_stable_as_a_running_turn_grows() {
    let mut before = live_run();
    // The view one tick earlier: the running step has not landed yet, and the
    // trailing prose has not been said.
    {
        let turn = &mut before.turns[0];
        turn.steps.truncate(2);
        turn.prose.retain(|p| p.before_step < 3);
        turn.duration_ms = 2_400;
    }
    let after = live_run();
    let state = FoldState::new();
    for s in 0..=2 {
        assert_eq!(
            html::render_run_prefix(&before, &state, 0, s),
            html::render_run_prefix(&after, &state, 0, s),
            "prefix through step {s} changed as the turn grew"
        );
    }
}

/// A tail whose first step repeats the previous settled step's time offset
/// blanks its time cell, exactly as the full render dedups between adjacent
/// steps.
#[test]
fn tail_time_column_dedups_against_the_settled_prefix() {
    let mut run = live_run();
    run.turns[0].steps[1].offset_ms = 800; // same second as step 0
    let state = FoldState::new();
    let full = html::render_run(&run, &state);
    let (tail_run, base) = split_at(&run, 1);
    let tail = html::render_turn_tail(&tail_run, &state, 0, &base);
    let spliced = format!(
        "{}{}{}</details></div>",
        html::render_run_prefix(&run, &state, 0, 1),
        tail.blocks.concat(),
        tail.close,
    );
    assert_eq!(spliced, full);
}
