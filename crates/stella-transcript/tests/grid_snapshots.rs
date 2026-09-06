//! Golden-frame snapshots of `stella_transcript::grid`.
//!
//! This is the character-grid renderer. It draws every plain `stella run`
//! transcript. `grid::render` and `grid::render_turn_lines` had one fixture
//! before this file. That fixture pins one function's spans, not a frame.
//! Every other test in this crate is a plain unit test in `src/tests.rs`. A
//! unit test proves a property. It cannot show which rows a frame has, in
//! what order, at what column. This file adds that missing check.
//!
//! `grid.rs`'s whole frame is due for a rewrite. SPEC 6 swaps the
//! `╭─ … ─╮` box for labelled rules. That change will move every row this
//! file pins. A golden landed now gives that change something to diff
//! against.
//!
//! This file follows `crates/stella-tui/tests/deck_render_snapshots.rs`'s
//! harness shape — the pattern this workspace already uses for a golden
//! suite. It asserts [`grid::to_plain`], never the ANSI output. A plain-grid
//! diff is one a human can read. `grid.rs`'s own notes say to check the ANSI
//! encoders on their own.
//!
//! ## Regenerating
//!
//! ```text
//! BLESS=1 cargo test -p stella-transcript --test grid_snapshots
//! ```
//!
//! Read the diff before you commit it. A golden blessed without a look is a
//! changelog, not a test.
//!
//! Every test here starts with `grid_snapshots_`. Keep that prefix: it lets
//! `cargo test -p stella-transcript grid_snapshots` find them with no
//! `--test` flag.

use std::path::PathBuf;

use stella_transcript::grid;
use stella_transcript::{
    Accounting, ArgRow, Call, Extent, FileChange, FileStatus, FoldState, NodeId, Note, NoteKind,
    Output, Prose, Run, Status, Step, ToolKind, Turn, Zoom,
};

/// The command quoted in every failure message below.
const BLESS_CMD: &str = "BLESS=1 cargo test -p stella-transcript --test grid_snapshots";

/// The width most goldens render at. It is wide enough that a step row's
/// verb, object and chips all fit with no cut. That way a fixture shows the
/// column layout, not the cut rules `grid.rs` already covers on its own.
const W: usize = 100;

// ───────────────────────────── the harness ─────────────────────────────
// This mirrors `crates/stella-tui/tests/deck_render_snapshots.rs`. It skips
// the `TestBackend` step: `grid::render` and `render_turn_lines` already
// return the character grid, with no terminal to render into first.

fn snapshot_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots/grid")
        .join(format!("{name}.txt"))
}

/// `BLESS=1` rewrites the goldens instead of asserting against them.
fn blessing() -> bool {
    std::env::var("BLESS").is_ok_and(|v| v == "1")
}

/// The full golden file: a header, then the plain frame.
///
/// The header names the width. A change to a gutter constant then shows up
/// as a header diff, not as rows that re-wrapped for no clear reason. The
/// header also names the bless command, so a reader can regenerate the file.
fn golden_body(name: &str, description: &str, width: usize, frame: &str) -> String {
    format!(
        "# grid golden · {name}\n\
         # {description}\n\
         # width {width} · rendered through grid::to_plain, styling stripped\n\
         # regenerate: {BLESS_CMD}\n\
         \n\
         {frame}\n"
    )
}

/// Assert a rendered frame against its committed golden, or rewrite it under
/// `BLESS=1`.
fn assert_golden(name: &str, description: &str, width: usize, frame: &str) {
    let body = golden_body(name, description, width, frame);
    let path = snapshot_path(name);

    if blessing() {
        let dir = path.parent().expect("snapshot path has a parent");
        std::fs::create_dir_all(dir)
            .unwrap_or_else(|err| panic!("create {}: {err}", dir.display()));
        std::fs::write(&path, &body)
            .unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
        println!("blessed {}", path.display());
        return;
    }

    let Ok(expected) = std::fs::read_to_string(&path) else {
        panic!(
            "no golden for {name:?} at {}.\n\
             Create it with:  {BLESS_CMD}\n\
             \n\
             This run rendered:\n{body}",
            path.display()
        );
    };
    // Normalize line endings: a Windows checkout with `core.autocrlf` on
    // would otherwise fail every golden on the first character of every line.
    let expected = expected.replace("\r\n", "\n");

    assert!(
        expected == body,
        "the {name:?} frame drifted from its golden at {}.\n\n{}\n\
         If the change is intended, re-bless and read the diff:  {BLESS_CMD}",
        path.display(),
        describe_diff(&expected, &body),
    );
}

/// A short report of what changed: only the rows that differ, each with the
/// column where it first differs. Two full walls of box-drawing left for a
/// reader to eyeball would show much less.
fn describe_diff(expected: &str, actual: &str) -> String {
    use std::fmt::Write as _;

    let expected: Vec<&str> = expected.lines().collect();
    let actual: Vec<&str> = actual.lines().collect();
    let mut out = String::new();

    if expected.len() != actual.len() {
        let _ = writeln!(
            out,
            "the frame changed height: {} lines -> {} lines",
            expected.len(),
            actual.len()
        );
    }

    for i in 0..expected.len().max(actual.len()) {
        let (e, a) = (
            expected.get(i).copied().unwrap_or("<past end of golden>"),
            actual.get(i).copied().unwrap_or("<past end of frame>"),
        );
        if e == a {
            continue;
        }
        let col = e
            .chars()
            .zip(a.chars())
            .position(|(x, y)| x != y)
            .unwrap_or_else(|| e.chars().count().min(a.chars().count()));
        let _ = writeln!(
            out,
            "line {}, first difference at character {}:",
            i + 1,
            col + 1
        );
        let _ = writeln!(out, "  golden │{e}│");
        let _ = writeln!(out, "  actual │{a}│");
    }
    out
}

// ───────────────────────────── the fixtures ─────────────────────────────
// These build `stella_transcript::model` values by hand, the way a real
// caller does. This crate never touches an `AgentEvent`. It takes owned
// data and hands back a grid — no scripted event stream needed.

fn plain_output(lines: &[&str]) -> Output {
    Output {
        lines: lines.iter().map(|l| (*l).to_string()).collect(),
        clipped: 0,
    }
}

fn bash(command: &str, out: &[&str], status: Status) -> Call {
    Call {
        tool: ToolKind::Bash,
        header_object: command.to_string(),
        args: vec![ArgRow {
            key: "command".to_string(),
            value: command.to_string(),
        }],
        output: plain_output(out),
        files: Vec::new(),
        status,
        duration_ms: 226,
        speculated: false,
        sub_agent_id: None,
    }
}

fn read_file(path: &str, out: &[&str]) -> Call {
    Call {
        tool: ToolKind::ReadFile,
        header_object: path.to_string(),
        args: Vec::new(),
        output: plain_output(out),
        files: Vec::new(),
        status: Status::Ok,
        duration_ms: 4,
        speculated: false,
        sub_agent_id: None,
    }
}

fn edit_file(path: &str, before: &str, after: &str) -> Call {
    Call {
        tool: ToolKind::EditFile,
        header_object: path.to_string(),
        args: Vec::new(),
        output: Output::default(),
        files: vec![FileChange {
            path: path.to_string(),
            before: before.to_string(),
            after: after.to_string(),
            status: FileStatus::Modified,
            extent: Extent::default(),
            patch: None,
        }],
        status: Status::Ok,
        duration_ms: 9,
        speculated: false,
        sub_agent_id: None,
    }
}

/// A deletion nothing measured. Both sides are empty, and there is no
/// patch, so `FileDiff::build` returns [`Extent::default`]. It does not
/// guess a count from a diff it never ran. The header must draw no `−N` at
/// all — never a fake `−0`.
fn delete_file_unmeasured(path: &str) -> Call {
    Call {
        tool: ToolKind::DeleteFile,
        header_object: path.to_string(),
        args: Vec::new(),
        output: Output::default(),
        files: vec![FileChange {
            path: path.to_string(),
            before: String::new(),
            after: String::new(),
            status: FileStatus::Deleted,
            extent: Extent::default(),
            patch: None,
        }],
        status: Status::Ok,
        duration_ms: 3,
        speculated: false,
        sub_agent_id: None,
    }
}

fn step(call: Call, offset_ms: u64) -> Step {
    Step {
        call: Some(call),
        accounting: Accounting {
            tokens_in: 4_000,
            tokens_out: 120,
            cached_in: 3_500,
            micros: 600,
        },
        offset_ms,
    }
}

fn run(name: &str, turns: Vec<Turn>) -> Run {
    Run {
        name: name.to_string(),
        model: "z-ai/glm-5.2".to_string(),
        started_at: "09:14:02".to_string(),
        turns,
    }
}

// ───────────────────────── a bash call, a read, a mutation ─────────────────
// One turn that touches all three call shapes, pinned at every zoom preset.
// That way the fold defaults are in the frame too, not just the calls.

fn mixed_turn() -> Turn {
    Turn {
        name: "widen-offset".to_string(),
        prompt: "widen the offset gutter so a five-digit step count still lines up".to_string(),
        prose: vec![Prose {
            text: "I'll check the current width, run the suite, then widen the constant."
                .to_string(),
            before_step: 0,
        }],
        notes: Vec::new(),
        steps: vec![
            step(
                bash(
                    "cargo test -p stella-transcript",
                    &["running 12 tests", "test result: ok. 12 passed; 0 failed"],
                    Status::Ok,
                ),
                0,
            ),
            step(
                read_file(
                    "crates/stella-transcript/src/grid.rs",
                    &["pub const OFFSET_W: usize = 5;"],
                ),
                400,
            ),
            step(
                edit_file(
                    "crates/stella-transcript/src/grid.rs",
                    "pub const OFFSET_W: usize = 5;\n",
                    "pub const OFFSET_W: usize = 6;\n",
                ),
                900,
            ),
        ],
        answer: Some("Widened OFFSET_W from 5 to 6; the suite still passes.".to_string()),
        status: Status::Ok,
        duration_ms: 4_200,
    }
}

fn mixed_run() -> Run {
    run("widen-offset-gutter", vec![mixed_turn()])
}

#[test]
fn grid_snapshots_pin_a_mixed_turn_at_zoom_turns() {
    let r = mixed_run();
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Turns);
    let frame = grid::to_plain(&grid::render(&r, &state, W));
    assert_golden(
        "mixed_turn_zoom_turns",
        "a bash call, a read and a file edit, at Zoom::Turns — turn headers only",
        W,
        &frame,
    );
}

#[test]
fn grid_snapshots_pin_a_mixed_turn_at_zoom_steps() {
    let r = mixed_run();
    let state = FoldState::new(); // Steps is the default preset
    let frame = grid::to_plain(&grid::render(&r, &state, W));
    assert_golden(
        "mixed_turn_zoom_steps",
        "the same turn at Zoom::Steps — one-line digests, bodies folded",
        W,
        &frame,
    );
}

#[test]
fn grid_snapshots_pin_a_mixed_turn_at_zoom_everything() {
    let r = mixed_run();
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);
    let frame = grid::to_plain(&grid::render(&r, &state, W));
    assert_golden(
        "mixed_turn_zoom_everything",
        "the same turn at Zoom::Everything — every body and diff open",
        W,
        &frame,
    );
}

// ───────────────────────────── a failed step ─────────────────────────────

fn suite_run_with_a_failure() -> Run {
    run(
        "loop-detect-suite",
        vec![
            Turn {
                name: "run-suite".to_string(),
                prompt: "run the loop-detect suite".to_string(),
                prose: Vec::new(),
                notes: Vec::new(),
                steps: vec![step(
                    bash(
                        "cargo test -p stella-core loop_detect",
                        &[
                            "running 4 tests",
                            "test loop_detect::tests::detects_a_two_cycle_repeat ... FAILED",
                            "failures:",
                            "    loop_detect::tests::detects_a_two_cycle_repeat",
                            "test result: FAILED. 3 passed; 1 failed; 0 ignored",
                        ],
                        Status::Error,
                    ),
                    0,
                )],
                answer: None,
                status: Status::Error,
                duration_ms: 6_100,
            },
            Turn {
                name: "fix-suite".to_string(),
                prompt: "fix it".to_string(),
                prose: Vec::new(),
                notes: Vec::new(),
                steps: vec![step(
                    bash(
                        "cargo test -p stella-core loop_detect",
                        &["running 4 tests", "test result: ok. 4 passed; 0 failed"],
                        Status::Ok,
                    ),
                    0,
                )],
                answer: Some(
                    "Fixed the off-by-one in the cycle window; the suite passes.".to_string(),
                ),
                status: Status::Ok,
                duration_ms: 3_400,
            },
        ],
    )
}

/// A failed step stays open at `Zoom::Turns`, the coarsest fold level.
/// [`Status::pins_open`] wins over the zoom default. This holds even for
/// the run's *first* turn, not its last — `Zoom::Turns` would leave only
/// the last turn open by default. Renders through `render_turn_lines`, the
/// entry point a plain `stella run` prints through.
#[test]
fn grid_snapshots_pin_a_failed_step_open_at_zoom_turns() {
    let r = suite_run_with_a_failure();
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Turns);
    let frame = grid::to_plain(&grid::render_turn_lines(&r, &state, 0, W));
    assert_golden(
        "failed_step_pinned_open",
        "the first of two turns, not the last, failed — Zoom::Turns would \
         otherwise collapse it, but the failure pins the step open",
        W,
        &frame,
    );
}

/// `suite_run_with_a_failure`'s first turn kept open, minus the failure —
/// the same shape with a passing step and an answer instead of `None`.
fn suite_run_without_a_failure() -> Run {
    run(
        "loop-detect-suite",
        vec![
            Turn {
                name: "run-suite".to_string(),
                prompt: "run the loop-detect suite".to_string(),
                prose: Vec::new(),
                notes: Vec::new(),
                steps: vec![step(
                    bash(
                        "cargo test -p stella-core loop_detect",
                        &["running 4 tests", "test result: ok. 4 passed; 0 failed"],
                        Status::Ok,
                    ),
                    0,
                )],
                answer: Some("The suite passes; nothing to fix.".to_string()),
                status: Status::Ok,
                duration_ms: 3_400,
            },
            Turn {
                name: "add-a-test".to_string(),
                prompt: "add one more case".to_string(),
                prose: Vec::new(),
                notes: Vec::new(),
                steps: vec![step(
                    bash(
                        "cargo test -p stella-core loop_detect",
                        &["running 5 tests", "test result: ok. 5 passed; 0 failed"],
                        Status::Ok,
                    ),
                    0,
                )],
                answer: Some("Added the new case; the suite still passes.".to_string()),
                status: Status::Ok,
                duration_ms: 2_100,
            },
        ],
    )
}

/// A turn that is not the run's last and has no failing step, collapsed at
/// `Zoom::Turns`. Neither existing `Zoom::Turns` golden covers it —
/// `grid_snapshots_pin_a_mixed_turn_at_zoom_turns` renders a single-turn run,
/// so `fold.rs`'s `default_open` keeps its only turn open through
/// `is_last_turn` regardless of collapsing; `failed_step_pinned_open` above
/// uses a non-last turn but pins it open with `Status::pins_open`, which
/// golden shows the *escape* from collapsing, not collapsing itself. This
/// fixture has two turns, the first finished `Status::Ok` with no failing
/// step, so nothing pins it open and the header actually collapses.
#[test]
fn grid_snapshots_pin_a_collapsed_turn_header_at_zoom_turns() {
    let r = suite_run_without_a_failure();
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Turns);
    let frame = grid::to_plain(&grid::render_turn_lines(&r, &state, 0, W));
    assert_golden(
        "collapsed_turn_at_zoom_turns",
        "the first of two turns, neither failing — Zoom::Turns collapses it, \
         unlike failed_step_pinned_open where a failure pins it open",
        W,
        &frame,
    );
}

// ──────────────────────── a note with detail, one without ─────────────────

fn notes_run() -> Run {
    run(
        "recall-and-budget",
        vec![Turn {
            name: "recall-and-budget".to_string(),
            prompt: "keep going".to_string(),
            prose: Vec::new(),
            notes: vec![
                Note {
                    kind: NoteKind::Context,
                    summary: "recalled 3 memories".to_string(),
                    detail: vec![
                        "stella-transcript-golden-frame.md".to_string(),
                        "grid-gutter-widths.md".to_string(),
                        "append-only-streaming.md".to_string(),
                    ],
                    before_step: 0,
                    inspect: None,
                },
                Note {
                    kind: NoteKind::Meter,
                    summary: "budget: 40% remaining".to_string(),
                    detail: Vec::new(),
                    before_step: 1,
                    inspect: None,
                },
            ],
            steps: vec![
                step(
                    bash(
                        "cargo build -p stella-transcript",
                        &["Compiling stella-transcript"],
                        Status::Ok,
                    ),
                    0,
                ),
                step(
                    bash(
                        "cargo test -p stella-transcript",
                        &["test result: ok. 12 passed; 0 failed"],
                        Status::Ok,
                    ),
                    500,
                ),
            ],
            answer: Some("Both crates build and the suite passes.".to_string()),
            status: Status::Ok,
            duration_ms: 2_000,
        }],
    )
}

/// A note with detail rows draws a fold control. One without draws none —
/// the control never opens onto an empty body. The detailed note is opened
/// by hand here, so its rows show in the frame, not just its marker.
#[test]
fn grid_snapshots_pin_notes_with_and_without_detail() {
    let r = notes_run();
    let mut state = FoldState::new();
    state.open(NodeId::Note { turn: 0, note: 0 });
    let frame = grid::to_plain(&grid::render_turn_lines(&r, &state, 0, W));
    assert_golden(
        "notes_with_and_without_detail",
        "a context note with three detail rows, opened, beside a budget note \
         with none — only the first draws a fold marker",
        W,
        &frame,
    );
}

// ───────────────────────── an unmeasured deletion ─────────────────────────

fn unmeasured_delete_run() -> Run {
    run(
        "drop-legacy-printer",
        vec![Turn {
            name: "drop-legacy-printer".to_string(),
            prompt: "delete the dead legacy printer".to_string(),
            prose: Vec::new(),
            notes: Vec::new(),
            steps: vec![step(
                delete_file_unmeasured("crates/stella-cli/src/plain/legacy.rs"),
                0,
            )],
            answer: Some("Removed the dead legacy printer.".to_string()),
            status: Status::Ok,
            duration_ms: 400,
        }],
    )
}

/// Pins [`stella_transcript::Extent`]'s rule as a frame, not just as a unit
/// check: a side nothing measured draws no count at all, never a fake `−0`.
#[test]
fn grid_snapshots_pin_an_unmeasured_deleted_file() {
    let r = unmeasured_delete_run();
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);
    let frame = grid::to_plain(&grid::render_turn_lines(&r, &state, 0, W));
    assert_golden(
        "unmeasured_deleted_file",
        "a delete_file call whose extent nothing measured — the header \
         draws no count",
        W,
        &frame,
    );
}

// ───────────────────────────── CJK and emoji ─────────────────────────────

fn cjk_emoji_run() -> Run {
    run(
        "設定を更新",
        vec![Turn {
            name: "🔧 設定を更新".to_string(),
            prompt: "設定ファイルを更新して絵文字のラベルを直してください 🎉".to_string(),
            prose: Vec::new(),
            notes: Vec::new(),
            steps: vec![step(
                edit_file(
                    "設定/config.toml",
                    "名前 = \"旧いラベル\"\n",
                    "名前 = \"新しいラベル\" 🎉\n",
                ),
                0,
            )],
            answer: Some("設定を更新しました ✅".to_string()),
            status: Status::Ok,
            duration_ms: 900,
        }],
    )
}

/// `grid.rs` measures width through [`grid::cells`], never through
/// `chars().count()`. An ASCII fixture cannot show the difference. CJK
/// text and emoji can, because each one takes two columns, not one. A
/// narrow width forces the turn name and the diff header to cut, so a bug
/// in that measure shows up here as a moved column.
#[test]
fn grid_snapshots_pin_cjk_and_emoji_content() {
    const NARROW: usize = 56;
    let r = cjk_emoji_run();
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);
    let frame = grid::to_plain(&grid::render_turn_lines(&r, &state, 0, NARROW));
    assert_golden(
        "cjk_and_emoji_content",
        "a Japanese turn name/prompt/answer and an emoji, at a width narrow \
         enough to force a cut",
        NARROW,
        &frame,
    );
}

// ───────────────────────── append-only, made legible ─────────────────────

fn growing_run(steps: usize) -> Run {
    run(
        "widen-fixture-matrix",
        vec![Turn {
            name: "widen-matrix".to_string(),
            prompt: "widen the fixture matrix, crate by crate".to_string(),
            prose: Vec::new(),
            notes: Vec::new(),
            steps: (0..steps)
                .map(|i| {
                    step(
                        bash(
                            &format!("cargo test -p crate-{i}"),
                            &["test result: ok. 1 passed; 0 failed"],
                            Status::Ok,
                        ),
                        (i * 200) as u64,
                    )
                })
                .collect(),
            answer: None,
            status: Status::Running,
            duration_ms: 0,
        }],
    )
}

/// A line the streaming surface has already printed must not change as the
/// turn grows. That is [`grid::render_turn_lines`]'s own rule. A unit test
/// already checks it: `render_turn_is_append_only_as_a_turn_grows`, in
/// `src/tests/frame.rs`. This test shows the same rule as a diff. A bug
/// that moves an already-printed line then shows up as a changed section
/// of the golden, not just a failed check with no shape to it.
///
/// Every growing stage below skips the closing rail, the same way the
/// streaming surface does (`stella-cli`'s `plain::transcript::frame`). That
/// rail carries the status and the cost. Neither exists while the turn is
/// still running.
#[test]
fn grid_snapshots_pin_the_append_only_growth_of_a_streaming_turn() {
    let mut sections = String::new();
    let mut previous: Vec<String> = Vec::new();
    for steps in 0..=3 {
        let r = growing_run(steps);
        let state = FoldState::new();
        let lines = grid::render_turn_lines(&r, &state, 0, W);
        let body: Vec<String> = lines[..lines.len().saturating_sub(1)]
            .iter()
            .map(|l| grid::to_plain(std::slice::from_ref(l)))
            .collect();
        assert!(
            body.len() >= previous.len(),
            "the frame shrank at {steps} steps: {} -> {}",
            previous.len(),
            body.len()
        );
        for (i, (was, now)) in previous.iter().zip(&body).enumerate() {
            assert_eq!(
                was, now,
                "line {i} was rewritten when the turn reached {steps} steps"
            );
        }
        sections.push_str(&format!(
            "── after {steps} step{} (running) ──\n",
            if steps == 1 { "" } else { "s" }
        ));
        sections.push_str(&body.join("\n"));
        sections.push_str("\n\n");
        previous = body;
    }

    let mut finished = growing_run(3);
    finished.turns[0].status = Status::Ok;
    finished.turns[0].answer = Some("Every crate in the matrix now has a fixture.".to_string());
    let full = grid::to_plain(&grid::render_turn_lines(&finished, &FoldState::new(), 0, W));
    sections.push_str("── finished (3 steps + answer) ──\n");
    sections.push_str(&full);

    assert_golden(
        "append_only_growth",
        "render_turn_lines at 0..=3 steps while the turn runs, then \
         finished — every earlier line survives unchanged into the next stage",
        W,
        &sections,
    );
}
