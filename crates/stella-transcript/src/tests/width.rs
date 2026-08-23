//! The width contract: no row of [`grid::render`] is wider than the `width` it
//! was given.
//!
//! In its own file because the property wants generated content and the
//! generator is most of the code. The ASCII witnesses below name one path each,
//! so a failure says *which* row builder let go; the property is what covers
//! the paths nobody thought to name.

use proptest::prelude::*;

use super::*;

/// The oracle: `unicode-width` over the row's own text, never
/// `grid::line_width`. Measuring the renderer with the measure under test is
/// how #3740 stayed invisible — every row it over-filled also reported itself
/// as fitting.
fn row_cells(line: &grid::Line) -> usize {
    let text: String = line.iter().map(|c| c.text.as_str()).collect();
    UnicodeWidthStr::width(text.as_str())
}

/// Every node in `run`, expanded. A fold state hides most of the row builders,
/// so a property that only ever rendered the collapsed view would be testing a
/// fraction of them.
fn all_open(run: &Run) -> FoldState {
    let mut state = FoldState::new();
    for (ti, turn) in run.turns.iter().enumerate() {
        state.open(NodeId::Turn(ti));
        for pi in 0..turn.prose.len() {
            state.open(NodeId::Prose {
                turn: ti,
                prose: pi,
            });
        }
        for ni in 0..turn.notes.len() {
            state.open(NodeId::Note { turn: ti, note: ni });
        }
        for (si, step) in turn.steps.iter().enumerate() {
            state.open(NodeId::Step { turn: ti, step: si });
            state.open(NodeId::Output { turn: ti, step: si });
            let files = step.call.as_ref().map_or(0, |c| c.files.len());
            for fi in 0..files {
                state.open(NodeId::File {
                    turn: ti,
                    step: si,
                    file: fi,
                });
                // The hunk count is not knowable without building the diff;
                // eight covers every fixture this generator can produce.
                for hi in 0..8 {
                    state.open(NodeId::Hunk {
                        turn: ti,
                        step: si,
                        file: fi,
                        hunk: hi,
                    });
                }
            }
        }
    }
    state
}

/// Assert the contract over one run at one width, in both fold states, and
/// report *every* offending row — the failure this guards against walks a whole
/// column, and one row out of context reads as a one-off.
fn assert_fits(run: &Run, width: usize) -> Result<(), TestCaseError> {
    for state in [FoldState::new(), all_open(run)] {
        let lines = grid::render(run, &state, width);
        let overruns: Vec<String> = lines
            .iter()
            .filter(|l| row_cells(l) > width)
            .map(|l| {
                let text: String = l.iter().map(|c| c.text.as_str()).collect();
                format!("{} cells: {text:?}", row_cells(l))
            })
            .collect();
        prop_assert!(
            overruns.is_empty(),
            "{} of {} rows overran a {width}-cell grid:\n{}",
            overruns.len(),
            lines.len(),
            overruns.join("\n")
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The named witnesses: one row builder each.
// ---------------------------------------------------------------------------

/// The issue's own repro (#3769): one unbreakable 400-cell word, which no
/// whitespace break can help with.
#[test]
fn an_unbreakable_run_stays_inside_the_grid() {
    const WIDTH: usize = 100;
    let mut run = run_with(vec![step(edit("src/lib.rs", "a\n", "b\n"), 1_000)]);
    run.turns[0].prompt = "x".repeat(400);
    assert_fits(&run, WIDTH).unwrap();
}

/// A prose block opening with one very long sentence. `first_sentence` returns
/// the whole sentence whenever a `.`/`!`/`?` boundary exists, so the fold head
/// was as long as the model's first thought.
#[test]
fn a_long_first_sentence_stays_inside_the_grid() {
    const WIDTH: usize = 100;
    let mut run = run_with(vec![step(edit("src/lib.rs", "a\n", "b\n"), 1_000)]);
    run.turns[0].prose = vec![Prose {
        text: format!("{}. And then the rest of it.", "word ".repeat(80)),
        before_step: 0,
    }];
    assert_fits(&run, WIDTH).unwrap();
}

/// Content that lands exactly on `width`: the fills floored at one cell, so a
/// row that exactly fitted was pushed one cell past the edge. Swept across a
/// range of widths because the exact-fit width is a different number for each
/// row builder.
#[test]
fn a_row_that_exactly_fits_is_not_pushed_one_cell_over() {
    let mut run = run_with(vec![step(edit("src/lib.rs", "a\n", "b\n"), 1_000)]);
    run.turns[0].name = "x".repeat(40);
    run.turns[0].notes = vec![Note {
        kind: NoteKind::Context,
        summary: "y".repeat(40),
        detail: vec!["detail".to_string()],
        before_step: 0, inspect: None,
    }];
    for width in 40..=140 {
        assert_fits(&run, width).unwrap();
    }
}

/// A turn name is a branch name or a goal slug — as long as whoever named it
/// made it — and it sits on the rail that has to close with `─╮`.
#[test]
fn a_long_turn_name_does_not_push_the_rail_cap_off_the_edge() {
    const WIDTH: usize = 100;
    let mut run = run_with(vec![step(edit("src/lib.rs", "a\n", "b\n"), 1_000)]);
    run.turns[0].name = "feature/".to_string() + &"deeply-nested-".repeat(20);
    assert_fits(&run, WIDTH).unwrap();

    // The rails still meet, and still at the right edge — a rail that fits by
    // losing its cap has not kept the contract.
    let lines = grid::render(&run, &FoldState::new(), WIDTH);
    let rail = |corner: char| {
        lines
            .iter()
            .find(|l| l.first().is_some_and(|c| c.text.starts_with(corner)))
            .map(|l| {
                let text: String = l.iter().map(|c| c.text.as_str()).collect();
                (row_cells(l), text)
            })
    };
    let (top_w, top) = rail('╭').expect("the frame's top rail");
    let (bottom_w, bottom) = rail('╰').expect("the frame's bottom rail");
    assert_eq!(top_w, WIDTH, "top rail: {top:?}");
    assert_eq!(bottom_w, WIDTH, "bottom rail: {bottom:?}");
    assert!(top.ends_with("─╮"), "top rail lost its cap: {top:?}");
    assert!(
        bottom.ends_with("─╯"),
        "bottom rail lost its cap: {bottom:?}"
    );
}

// ---------------------------------------------------------------------------
// The property.
// ---------------------------------------------------------------------------

/// Text shapes a fixed grid has no good answer for, alongside ordinary prose.
fn hostile_text() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        "[ -~]{0,60}",
        // One unbreakable unit: a URL, a minified line, a stack frame.
        "[a-z/._-]{1,300}",
        // A paragraph written the way Japanese and Chinese are written.
        "[日本語猫時間]{1,120}",
        // Punctuated prose, so `first_sentence` finds a boundary and returns
        // whatever precedes it.
        "([a-z]{2,12} ){1,60}[.!?] ([a-z]{2,12} ){0,10}",
    ]
}

fn tool_kind() -> impl Strategy<Value = ToolKind> {
    prop_oneof![
        Just(ToolKind::Bash),
        Just(ToolKind::ReadFile),
        Just(ToolKind::WriteFile),
        Just(ToolKind::EditFile),
        Just(ToolKind::DeleteFile),
        Just(ToolKind::Search),
        "[a-z_]{1,40}".prop_map(ToolKind::Other),
    ]
}

fn status() -> impl Strategy<Value = Status> {
    prop_oneof![
        Just(Status::Ok),
        Just(Status::Warn),
        Just(Status::Error),
        Just(Status::Running),
    ]
}

fn a_call() -> impl Strategy<Value = Call> {
    (
        tool_kind(),
        hostile_text(),
        prop::collection::vec((hostile_text(), hostile_text()), 0..3),
        prop::collection::vec(hostile_text(), 0..12),
        prop::collection::vec((hostile_text(), hostile_text(), hostile_text()), 0..2),
        status(),
    )
        .prop_map(|(tool, object, args, out, files, status)| Call {
            tool,
            header_object: object,
            args: args
                .into_iter()
                .map(|(key, value)| ArgRow { key, value })
                .collect(),
            output: Output {
                lines: out,
                clipped: 0,
            },
            files: files
                .into_iter()
                .map(|(path, before, after)| FileChange {
                    path,
                    before: format!("{before}\ncommon\n"),
                    after: format!("{after}\ncommon\n"),
                    status: FileStatus::Modified,
                })
                .collect(),
            status,
            duration_ms: 226,
            speculated: false,
        })
}

fn a_turn() -> impl Strategy<Value = Turn> {
    (
        hostile_text(),
        hostile_text(),
        prop::collection::vec((hostile_text(), 0usize..3), 0..3),
        prop::collection::vec(
            (hostile_text(), prop::collection::vec(hostile_text(), 0..3)),
            0..3,
        ),
        prop::collection::vec(a_call(), 0..3),
        prop::option::of(hostile_text()),
        status(),
    )
        .prop_map(|(name, prompt, prose, notes, calls, answer, status)| Turn {
            name,
            prompt,
            prose: prose
                .into_iter()
                .map(|(text, before_step)| Prose { text, before_step })
                .collect(),
            notes: notes
                .into_iter()
                .map(|(summary, detail)| Note {
                    kind: NoteKind::Context,
                    summary,
                    detail,
                    before_step: 0, inspect: None,
                })
                .collect(),
            steps: calls
                .into_iter()
                .enumerate()
                .map(|(i, call)| step(call, i as u64 * 1_000))
                .collect(),
            answer,
            status,
            duration_ms: 41_000,
        })
}

fn a_run() -> impl Strategy<Value = Run> {
    (
        hostile_text(),
        hostile_text(),
        prop::collection::vec(a_turn(), 1..3),
    )
        .prop_map(|(name, model, turns)| Run {
            name,
            model,
            started_at: "14:02:11".to_string(),
            turns,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// The contract, for any run and any width. No floor: a width too narrow to
    /// hold a rail's own corners still gets rows that fit it, because the last
    /// thing `render` does is cut what the row builders could not fold.
    #[test]
    fn no_rendered_row_is_wider_than_the_grid(run in a_run(), width in 0usize..200) {
        assert_fits(&run, width)?;
    }
}
