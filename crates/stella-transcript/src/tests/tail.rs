//! [`html::render_turn_tail`]'s cutoffs (#4566's fix shape (a)): what a live
//! poll may skip, what it must re-render, and what a stale cursor does.
//!
//! Split out of the parent module for the 1500-line ceiling.

use super::*;

/// #4566's server-cost half: a still-open step at the caller's `from_step` is
/// rewritten in place once it settles, and a step that was already settled at
/// `from_step` — or the run has not yet reached it — is never re-emitted.
#[test]
fn a_step_still_open_at_the_last_render_is_the_replace_boundary() {
    let mut run = run_with(vec![
        step(bash("ls", &["a.txt"], Status::Ok), 100),
        step(bash("cat a.txt", &[], Status::Running), 200),
    ]);
    let state = FoldState::new();

    // The caller's last render saw step 1 still running, so it resumes from
    // index 1 (not `steps.len()`, which would skip the now-stale block).
    let tail = html::render_turn_tail(
        &run,
        &state,
        0,
        html::TailCursor {
            step: 1,
            note: 0,
            prose: 0,
        },
    )
    .expect("in range");
    assert!(tail.blocks.contains("id=\"t0s1\""), "{}", tail.blocks);
    assert!(
        !tail.blocks.contains("id=\"t0s0\""),
        "an already-settled step must not be re-emitted: {}",
        tail.blocks
    );

    // The call settles, and a third step is appended.
    let running_call = run.turns[0].steps[1].call.as_mut().expect("call");
    running_call.status = Status::Ok;
    running_call.output = output(&["a.txt"]);
    run.turns[0]
        .steps
        .push(step(bash("wc -l a.txt", &["1 a.txt"], Status::Ok), 300));

    let tail = html::render_turn_tail(
        &run,
        &state,
        0,
        html::TailCursor {
            step: 1,
            note: 0,
            prose: 0,
        },
    )
    .expect("in range");
    assert!(tail.blocks.contains("id=\"t0s1\""));
    assert!(tail.blocks.contains("id=\"t0s2\""));
    assert!(tail.blocks.contains("cat a.txt"));
    assert!(
        !tail.blocks.contains("id=\"t0s0\""),
        "the settled prefix must still be untouched: {}",
        tail.blocks
    );
}

/// A note or prose block, once folded, never needs replacing — only its own
/// index into the notes/prose vectors decides whether it is new.
#[test]
fn a_note_before_the_cutoff_is_not_re_emitted_a_later_one_is() {
    let mut run = run_with(vec![step(bash("true", &[], Status::Ok), 0)]);
    run.turns[0].notes = vec![
        Note {
            kind: NoteKind::Meter,
            summary: "first metered call".to_string(),
            detail: Vec::new(),
            before_step: 1,
            inspect: None,
        },
        Note {
            kind: NoteKind::Meter,
            summary: "second metered call".to_string(),
            detail: Vec::new(),
            before_step: 1,
            inspect: None,
        },
    ];
    let state = FoldState::new();

    let tail = html::render_turn_tail(
        &run,
        &state,
        0,
        html::TailCursor {
            step: 1,
            note: 1,
            prose: 0,
        },
    )
    .expect("in range");
    assert!(tail.blocks.contains("second metered call"));
    assert!(
        !tail.blocks.contains("first metered call"),
        "a note already rendered before the cutoff must not repeat: {}",
        tail.blocks
    );
}

/// A cutoff past what the run actually holds means the caller's view is stale
/// — the server refuses rather than rendering nothing and calling it correct.
#[test]
fn tail_cutoffs_past_the_runs_length_are_refused() {
    let run = run_with(vec![step(bash("true", &[], Status::Ok), 0)]);
    let state = FoldState::new();
    assert!(
        html::render_turn_tail(
            &run,
            &state,
            0,
            html::TailCursor {
                step: 5,
                note: 0,
                prose: 0
            }
        )
        .is_none()
    );
    assert!(
        html::render_turn_tail(
            &run,
            &state,
            0,
            html::TailCursor {
                step: 0,
                note: 5,
                prose: 0
            }
        )
        .is_none()
    );
    assert!(
        html::render_turn_tail(
            &run,
            &state,
            0,
            html::TailCursor {
                step: 0,
                note: 0,
                prose: 5
            }
        )
        .is_none()
    );
    assert!(
        html::render_turn_tail(
            &run,
            &state,
            1,
            html::TailCursor {
                step: 0,
                note: 0,
                prose: 0
            }
        )
        .is_none(),
        "a turn index the run does not have must refuse too"
    );
}

/// The tail's receipt carries the same id a full render used, so a host page
/// can find and replace the one already-rendered element whose content (the
/// accounting chips) changes on every poll tick.
#[test]
fn the_receipt_id_is_stable_between_a_full_render_and_a_tail_render() {
    let run = run_with(vec![step(bash("true", &[], Status::Running), 0)]);
    let state = FoldState::new();
    let full = html::render_run(&run, &state);
    let tail = html::render_turn_tail(
        &run,
        &state,
        0,
        html::TailCursor {
            step: 0,
            note: 0,
            prose: 0,
        },
    )
    .expect("in range");
    assert!(full.contains("id=\"t0-receipt\""), "{full}");
    assert!(
        tail.receipt.contains("id=\"t0-receipt\""),
        "{}",
        tail.receipt
    );
}

/// A step's status rides the markup itself (`data-status`), which is what
/// lets a host page find the still-running boundary without any out-of-band
/// bookkeeping — matching the fold state, which is likewise read out of
/// `details[open]` rather than tracked in a script-side map.
#[test]
fn a_steps_status_is_readable_from_its_own_markup() {
    let run = run_with(vec![
        step(bash("ls", &["a"], Status::Ok), 0),
        step(bash("cat missing", &[], Status::Running), 100),
    ]);
    let state = FoldState::new();
    let full = html::render_run(&run, &state);
    assert!(full.contains("id=\"t0s0\" data-status=\"ok\""), "{full}");
    assert!(full.contains("id=\"t0s1\" data-status=\"run\""), "{full}");
}
