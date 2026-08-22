//! The turn frame: its rails, and the streaming contract they make possible.
//!
//! Split out of the parent module for the 1500-line ceiling, and along this
//! seam because these three hold one property between them — a turn frame that
//! can be written a line at a time, closing with the two facts that do not
//! exist until it ends.

use super::*;

/// The turn frame's two rails are one box: a top that reserves fewer cells for
/// its `─╮` cap than it emits is two cells longer than the bottom it meets.
#[test]
fn the_turn_frame_rails_are_the_same_width() {
    const WIDTH: usize = 100;
    let run = run_with(vec![step(edit("src/lib.rs", "a\n", "b\n"), 1_000)]);
    let lines = grid::render(&run, &FoldState::new(), WIDTH);
    let rail = |corner: char| {
        lines
            .iter()
            .find(|l| l.first().is_some_and(|c| c.text.starts_with(corner)))
            .map(grid::line_width)
    };
    assert_eq!(rail('╭'), Some(WIDTH), "the frame's top rail");
    assert_eq!(rail('╰'), Some(WIDTH), "the frame's bottom rail");
}

/// The property an append-only surface streams on: as a turn runs, its frame
/// only ever grows at the tail. Every line already produced stays byte-for-byte
/// what it was, so a terminal that has printed the first N can print the rest
/// without revisiting one — and therefore has no reason to invent a terser
/// placeholder rendering to show in the meantime.
///
/// This is what makes [`grid::render_turn_lines`]'s contract enforceable rather
/// than aspirational. It fails the moment anything that is unknown at the start
/// of a turn moves back onto the top rail: `stella run` on a terminal used to
/// print `· bash` per call for the whole turn and draw the real frame only at
/// the end, precisely because the frame could not be started before it was
/// finished.
///
/// The closing rail is excluded, and only the closing rail. It carries the two
/// facts that do not exist until the turn is over — how it ended and what it
/// spent — which is exactly why they live there.
#[test]
fn render_turn_is_append_only_as_a_turn_grows() {
    const WIDTH: usize = 100;
    let state = FoldState::new();

    // The turn as it looks after `steps` calls have settled, still running.
    let growing = |steps: usize| {
        let mut run = run_with(
            (0..steps)
                .map(|i| {
                    step(
                        bash(&format!("cargo test -p crate-{i}"), &["ok"], Status::Ok),
                        0,
                    )
                })
                .collect(),
        );
        let turn = &mut run.turns[0];
        turn.status = Status::Running;
        turn.answer = None;
        run
    };

    // Every line but the closing rail, which the streaming surface withholds
    // until the turn ends for the reason in this test's doc comment.
    let body = |run: &Run| -> Vec<String> {
        let lines = grid::render_turn_lines(run, &state, 0, WIDTH);
        lines[..lines.len() - 1]
            .iter()
            .map(|l| grid::to_plain(std::slice::from_ref(l)))
            .collect()
    };

    let mut previous: Vec<String> = Vec::new();
    for steps in 0..4 {
        let next = body(&growing(steps));
        assert!(
            next.len() >= previous.len(),
            "the frame shrank at {steps} steps: {} -> {}",
            previous.len(),
            next.len()
        );
        for (i, (was, now)) in previous.iter().zip(&next).enumerate() {
            assert_eq!(
                was, now,
                "line {i} was rewritten when the turn reached {steps} steps — \
                 a surface that had already printed it cannot take it back"
            );
        }
        previous = next;
    }

    // Ending the turn — an answer arrives and the status settles — still only
    // appends. Were the status on the top rail, this is the assertion that
    // would catch `running` frozen into a finished turn's scrollback.
    let finished = body(&run_with(
        (0..3)
            .map(|i| {
                step(
                    bash(&format!("cargo test -p crate-{i}"), &["ok"], Status::Ok),
                    0,
                )
            })
            .collect(),
    ));
    for (i, (was, now)) in previous.iter().zip(&finished).enumerate() {
        assert_eq!(was, now, "line {i} was rewritten when the turn finished");
    }
    assert!(
        finished.len() > previous.len(),
        "the answer should have appended lines"
    );
}

/// The closing rail is where a turn's outcome is read, on both renderers.
///
/// They are allowed to differ in medium — one draws a box rule, the other a
/// `<div>` — but not in *which end of a turn* answers "how did it go, and what
/// did it cost". A reader comparing `stella run`'s scrollback against
/// `stella observe` for the same session must not find that question answered
/// in two different places (#3764).
#[test]
fn both_renderers_close_a_turn_with_its_status_and_accounting() {
    let run = run_with(vec![step(bash("cargo test", &["ok"], Status::Ok), 0)]);
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);

    let lines = grid::render_turn_lines(&run, &state, 0, 100);
    let closing = grid::to_plain(std::slice::from_ref(
        lines.last().expect("a turn frame has a closing rail"),
    ));
    // `done` is `status_word(Status::Ok)`. Spelled out rather than derived so a
    // change to the vocabulary makes a reader look at both renderers at once,
    // which is the whole point of the test.
    assert!(
        closing.starts_with('╰') && closing.contains("done") && closing.contains('$'),
        "the grid's closing rail lost the status or the cost: {closing:?}"
    );

    let markup = html::render_run(&run, &state);
    let receipt = markup
        .split("turnreceipt")
        .nth(1)
        .expect("an expanded turn closes with a receipt");
    assert!(
        receipt.contains("done") && receipt.contains("chip"),
        "the html receipt lost the status or the chips: {receipt:?}"
    );
    // The summary must not also carry them, or an open turn states its cost
    // twice and the two copies can disagree.
    let summary = markup.split("</summary>").next().unwrap_or_default();
    assert!(
        !summary.contains("class=\"chips\""),
        "an expanded turn put its chips on the summary as well as the receipt"
    );
}
