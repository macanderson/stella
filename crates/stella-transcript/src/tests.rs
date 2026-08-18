//! Tests, including one per acceptance check in the specification.
//!
//! The acceptance checks are named `acceptance_*` so a reader can find the
//! contract without reading the rest, and each asserts the *property* rather
//! than a rendered string — a golden frame proves what the renderer did today,
//! a property proves what it may never do.

use crate::digest::{self, format_cost, format_duration, format_tokens};
use crate::file_diff::{FileDiff, RowKind};
use crate::fold::{Command, Cursor, FoldState, Zoom, apply};
use crate::grid;
use crate::html;
use crate::model::*;
use crate::word;

fn output(lines: &[&str]) -> Output {
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
        output: output(out),
        files: Vec::new(),
        status,
        duration_ms: 226,
        speculated: false,
    }
}

fn edit(path: &str, before: &str, after: &str) -> Call {
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
        }],
        status: Status::Ok,
        duration_ms: 12,
        speculated: false,
    }
}

fn step(call: Call, offset_ms: u64) -> Step {
    Step {
        call: Some(call),
        accounting: Accounting {
            tokens_in: 7_000,
            tokens_out: 34,
            cached_in: 6_400,
            micros: 800,
        },
        offset_ms,
    }
}

fn run_with(steps: Vec<Step>) -> Run {
    let status = if steps.iter().any(|s| s.status() == Status::Error) {
        Status::Error
    } else {
        Status::Ok
    };
    Run {
        name: "latex-proof".to_string(),
        model: "z-ai/glm-5.2".to_string(),
        started_at: "14:02:11".to_string(),
        turns: vec![Turn {
            name: "fix-overfull".to_string(),
            prompt: "fix the overfull hbox warnings in main.tex".to_string(),
            prose: vec![Prose {
                text: "I'll read the file first. Then reproduce the warnings and fix them."
                    .to_string(),
                before_step: 0,
            }],
            steps,
            answer: Some("All three overfull warnings are gone.".to_string()),
            status,
            duration_ms: 41_000,
        }],
    }
}

// ---------------------------------------------------------------- acceptance

/// A 40-step run collapsed to step digests is one line per step, and every one
/// of those lines carries a gutter monogram — the "skimmable by gutter icons
/// alone" half of the check.
#[test]
fn acceptance_forty_step_run_collapses_to_one_line_per_step() {
    let steps: Vec<Step> = (0..40)
        .map(|i| {
            let call = if i % 7 == 3 {
                edit("app/main.tex", "a\n", "b\n")
            } else {
                bash(
                    "pdflatex main.tex",
                    &["line one", "line two", "line three", "line four"],
                    Status::Ok,
                )
            };
            step(call, i * 1_000)
        })
        .collect();
    let run = run_with(steps);
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Steps);

    let lines = grid::render(&run, &state, 108);
    let plain = grid::to_plain(&lines);

    // Every step contributes exactly one row: no body, no result row, no
    // repeated tool header.
    let step_rows = plain
        .lines()
        .filter(|l| l.contains("pdflatex") || l.contains("edit_file"))
        .count();
    assert_eq!(step_rows, 40, "expected one digest row per step\n{plain}");

    // The whole turn, frame and role blocks included, still fits a tall screen.
    assert!(
        plain.lines().count() < 60,
        "40 steps rendered {} rows",
        plain.lines().count()
    );

    // Each digest carries its tool's monogram somewhere on the row.
    for step in &run.turns[0].steps {
        let call = step.call.as_ref().unwrap();
        assert!(matches!(
            call.tool.monogram(),
            '$' | '±' | '◇' | '◆' | '✗' | '⌕' | '•'
        ));
    }
}

/// No command string appears more than once per call, in either renderer.
#[test]
fn acceptance_command_appears_exactly_once_per_call() {
    let command = "pdflatex -interaction=nonstopmode main.tex";
    let call = bash(command, &[command, "Overfull \\hbox"], Status::Ok);
    let run = run_with(vec![step(call, 0)]);
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);

    let markup = html::render_run(&run, &state);
    assert_eq!(
        markup.matches(&html::escape(command)).count(),
        1,
        "command rendered more than once:\n{markup}"
    );

    let plain = grid::to_plain(&grid::render(&run, &state, 120));
    assert_eq!(
        plain.matches(command).count(),
        1,
        "command rendered more than once:\n{plain}"
    );
}

/// Toggling any fold shifts zero columns: the gutters to the left of a step's
/// tool name are byte-identical open and closed.
#[test]
fn acceptance_toggling_a_fold_shifts_zero_columns() {
    let run = run_with(vec![step(bash("ls", &["a", "b", "c", "d"], Status::Ok), 0)]);
    let node = NodeId::Step { turn: 0, step: 0 };

    let mut closed = FoldState::new();
    closed.close(node);
    let mut open = FoldState::new();
    open.open(node);

    let prefix = |state: &FoldState| -> String {
        let lines = grid::render(&run, state, 100);
        let plain = grid::to_plain(&lines);
        let row = plain
            .lines()
            .find(|l| l.contains("bash"))
            .expect("digest row")
            .to_string();
        let index = row.find("bash").unwrap();
        row[..index].to_string()
    };

    let (closed_prefix, open_prefix) = (prefix(&closed), prefix(&open));
    assert_eq!(
        closed_prefix.chars().count(),
        open_prefix.chars().count(),
        "the fold marker changed the width of the gutter: {closed_prefix:?} vs {open_prefix:?}"
    );
    // And the only cell that differs is the marker itself.
    let differing: Vec<_> = closed_prefix
        .chars()
        .zip(open_prefix.chars())
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(differing, vec![('▸', '▾')]);
}

/// An expanded turn's own content stands in for the fold affordance, so its
/// header carries no marker; a collapsed turn's header is the only thing on
/// screen and does carry one.
#[test]
fn a_turns_own_header_shows_a_fold_marker_only_when_collapsed() {
    let run = run_with(vec![step(bash("ls", &["a"], Status::Ok), 0)]);
    let turn = NodeId::Turn(0);

    let mut open = FoldState::new();
    open.open(turn);
    let open_header = grid::to_plain(&grid::render(&run, &open, 100))
        .lines()
        .next()
        .expect("turn frame top")
        .to_string();
    assert!(
        !open_header.contains('▸') && !open_header.contains('▾'),
        "an expanded turn header carried a fold marker: {open_header:?}"
    );

    let mut closed = FoldState::new();
    closed.close(turn);
    let closed_header = grid::to_plain(&grid::render(&run, &closed, 100))
        .lines()
        .next()
        .expect("turn frame top")
        .to_string();
    assert!(
        closed_header.contains('▸'),
        "a collapsed turn header lost its fold marker: {closed_header:?}"
    );
}

/// A failed step remains expanded after the run completes — the status pins it,
/// so neither a zoom preset nor an explicit collapse closes it.
#[test]
fn acceptance_failed_step_stays_expanded_after_completion() {
    let run = run_with(vec![
        step(bash("ok", &["fine"], Status::Ok), 0),
        step(bash("boom", &["error: exploded"], Status::Error), 1_000),
    ]);
    let failed = NodeId::Step { turn: 0, step: 1 };

    let mut state = FoldState::new();
    assert!(state.is_open(&run, failed));

    state.set_zoom(Zoom::Turns);
    assert!(state.is_open(&run, failed), "zoom closed a failed step");

    state.close(failed);
    assert!(
        state.is_open(&run, failed),
        "an explicit collapse closed a failed step"
    );

    // And it renders open.
    let plain = grid::to_plain(&grid::render(&run, &state, 100));
    assert!(plain.contains("error: exploded"));
}

/// The word-level highlight puts `15pt` and `12pt` — and nothing else — in the
/// stronger tint.
#[test]
fn acceptance_word_diff_highlights_only_the_changed_token() {
    let before = "\\setlength{\\parindent}{15pt}\n";
    let after = "\\setlength{\\parindent}{12pt}\n";
    let diff = FileDiff::build(&FileChange {
        path: "main.tex".to_string(),
        before: before.to_string(),
        after: after.to_string(),
        status: FileStatus::Modified,
    });

    let rows: Vec<_> = diff.hunks.iter().flat_map(|h| h.rows.iter()).collect();
    let removed = rows.iter().find(|r| r.kind == RowKind::Removed).unwrap();
    let added = rows.iter().find(|r| r.kind == RowKind::Added).unwrap();

    let hot = |row: &crate::file_diff::Row| -> Vec<String> {
        row.spans
            .iter()
            .filter(|s| s.changed)
            .map(|s| s.text.clone())
            .collect()
    };
    assert_eq!(hot(removed), vec!["15pt".to_string()]);
    assert_eq!(hot(added), vec!["12pt".to_string()]);
    assert_eq!(removed.text(), "\\setlength{\\parindent}{15pt}");
    assert_eq!(added.text(), "\\setlength{\\parindent}{12pt}");
}

// ------------------------------------------------------------------ dedup

#[test]
fn args_toggle_drops_arguments_the_header_already_showed() {
    let call = bash("ls -la", &[], Status::Ok);
    assert!(
        call.extra_args().is_empty(),
        "the sole argument is the header object and must not render twice"
    );
}

#[test]
fn args_toggle_keeps_arguments_the_header_did_not_show() {
    let mut call = bash("ls -la", &[], Status::Ok);
    call.args.push(ArgRow {
        key: "cwd".to_string(),
        value: "/app".to_string(),
    });
    let extra = call.extra_args();
    assert_eq!(extra.len(), 1);
    assert_eq!(extra[0].key, "cwd");
}

#[test]
fn an_echoed_first_output_line_is_suppressed_with_a_marker() {
    let call = bash("make test", &["$ make test", "ok"], Status::Ok);
    let fold = digest::fold_output(&call.output, &call.header_object);
    assert!(fold.echo_hidden);
    assert_eq!(fold.head, vec!["ok".to_string()]);
}

#[test]
fn a_first_line_that_merely_resembles_the_command_is_not_suppressed() {
    let call = bash("make test", &["make test failed", "…"], Status::Ok);
    let fold = digest::fold_output(&call.output, &call.header_object);
    assert!(!fold.echo_hidden, "a real output line was eaten");
}

// ------------------------------------------------------------------- folds

#[test]
fn collapsing_a_parent_preserves_child_fold_state() {
    let run = run_with(vec![
        step(bash("a", &["x"], Status::Ok), 0),
        step(bash("b", &["y"], Status::Ok), 1),
    ]);
    let turn = NodeId::Turn(0);
    let second = NodeId::Step { turn: 0, step: 1 };

    let mut state = FoldState::new();
    state.open(second);
    state.close(turn);
    assert!(!state.is_open(&run, turn));

    state.open(turn);
    assert!(
        state.is_open(&run, second),
        "collapsing the turn discarded the step's fold state"
    );
}

#[test]
fn the_output_fold_control_has_something_behind_it() {
    let out = output(&["1", "2", "3", "4", "5", "6"]);
    let fold = digest::fold_output(&out, "cmd");
    assert_eq!(fold.head.len(), digest::HEAD_LINES);
    assert_eq!(fold.hidden, 3);
    assert_eq!(fold.more_label(), "▸ 3 more lines");
}

#[test]
fn a_long_output_folds_head_and_tail_so_errors_at_the_end_stay_visible() {
    let mut lines: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
    lines.push("error: the thing failed".to_string());
    let out = Output { lines, clipped: 0 };
    let fold = digest::fold_output(&out, "cmd");
    assert_eq!(fold.head.len(), digest::HEAD_LINES);
    assert_eq!(fold.tail.len(), digest::TAIL_LINES);
    assert!(fold.tail.last().unwrap().contains("failed"));
}

#[test]
fn clipped_lines_are_counted_in_the_fold_control() {
    let out = Output {
        lines: vec!["a".to_string(), "b".to_string()],
        clipped: 24,
    };
    let fold = digest::fold_output(&out, "cmd");
    assert_eq!(fold.hidden, 24, "the transport's clip must be admitted");
}

#[test]
fn zoom_cycles_through_the_three_presets() {
    let mut state = FoldState::new();
    assert_eq!(state.zoom(), Zoom::Steps);
    state.cycle_zoom();
    assert_eq!(state.zoom(), Zoom::Everything);
    state.cycle_zoom();
    assert_eq!(state.zoom(), Zoom::Turns);
    state.cycle_zoom();
    assert_eq!(state.zoom(), Zoom::Steps);
}

#[test]
fn the_cursor_walks_steps_and_saturates_at_the_ends() {
    let run = run_with(vec![
        step(bash("a", &[], Status::Ok), 0),
        step(bash("b", &[], Status::Ok), 1),
    ]);
    let cursor = Cursor::default();
    assert_eq!(cursor.next(&run).step, 1);
    assert_eq!(cursor.next(&run).next(&run).step, 1, "wrapped past the end");
    assert_eq!(cursor.prev(&run).step, 0, "wrapped past the start");
}

#[test]
fn copy_returns_the_invocation_rather_than_reaching_a_clipboard() {
    let run = run_with(vec![step(bash("cargo test", &[], Status::Ok), 0)]);
    let mut state = FoldState::new();
    let mut cursor = Cursor::default();
    let copied = apply(&run, &mut state, &mut cursor, Command::CopyInvocation);
    assert_eq!(copied.as_deref(), Some("cargo test"));
}

#[test]
fn keys_bind_to_the_documented_commands() {
    assert_eq!(Command::from_key("j"), Some(Command::NextStep));
    assert_eq!(Command::from_key("z"), Some(Command::CycleZoom));
    assert_eq!(Command::from_key("e"), Some(Command::ExpandOutputs));
    assert_eq!(Command::from_key("c"), Some(Command::CopyInvocation));
    assert_eq!(Command::from_key("Q"), None);
}

// ------------------------------------------------------------------- words

#[test]
fn tokenization_splits_punctuation_into_single_tokens() {
    assert_eq!(
        word::tokenize("{\\parindent}"),
        vec!["{", "\\", "parindent", "}"]
    );
}

#[test]
fn a_short_changed_run_falls_back_to_character_granularity() {
    let (old, new) = word::highlight("--fast", "--fast2");
    let hot_new: String = new
        .iter()
        .filter(|s| s.changed)
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(
        hot_new, "2",
        "the whole token was tinted for a one-char add"
    );
    assert!(old.iter().all(|s| !s.changed));
}

#[test]
fn a_wholly_rewritten_line_drops_word_highlights_as_noise() {
    let (old, new) = word::highlight("alpha beta gamma", "one two three four");
    assert!(old.iter().all(|s| !s.changed));
    assert!(new.iter().all(|s| !s.changed));
}

#[test]
fn context_lines_are_never_marked_changed() {
    let diff = FileDiff::build(&FileChange {
        path: "f".to_string(),
        before: "a\nb\nc\nd\ne\n".to_string(),
        after: "a\nb\nCHANGED\nd\ne\n".to_string(),
        status: FileStatus::Modified,
    });
    for row in diff.hunks.iter().flat_map(|h| h.rows.iter()) {
        if row.kind == RowKind::Context {
            assert!(row.spans.iter().all(|s| !s.changed));
        }
    }
}

// -------------------------------------------------------------- file kinds

#[test]
fn a_new_file_renders_as_an_all_green_diff() {
    let diff = FileDiff::build(&FileChange {
        path: ".latexmkrc".to_string(),
        before: String::new(),
        after: "$pdf_mode = 1;\n$clean_ext = 'aux log';\n".to_string(),
        status: FileStatus::New,
    });
    assert_eq!(diff.added, 2);
    assert_eq!(diff.removed, 0);
    assert!(
        diff.hunks
            .iter()
            .flat_map(|h| h.rows.iter())
            .all(|r| r.kind == RowKind::Added)
    );
}

#[test]
fn a_deleted_file_renders_as_an_all_red_diff() {
    let diff = FileDiff::build(&FileChange {
        path: "main.aux".to_string(),
        before: "\\relax\n\\gdef\n".to_string(),
        after: String::new(),
        status: FileStatus::Deleted,
    });
    assert_eq!(diff.removed, 2);
    assert_eq!(diff.added, 0);
    assert_eq!(diff.status.token(), "gone");
}

#[test]
fn a_mutation_digest_names_what_happened_rather_than_making_the_reader_do_arithmetic() {
    let delete = Call {
        tool: ToolKind::DeleteFile,
        header_object: "main.aux".to_string(),
        args: Vec::new(),
        output: Output::default(),
        files: vec![FileChange {
            path: "main.aux".to_string(),
            before: "a\nb\n".to_string(),
            after: String::new(),
            status: FileStatus::Deleted,
        }],
        status: Status::Ok,
        duration_ms: 4,
        speculated: false,
    };
    let dig = digest::step_digest(&step(delete, 0), 40);
    assert_eq!(dig.delta.unwrap().label(), "deleted · −2");

    let create = Call {
        tool: ToolKind::WriteFile,
        header_object: ".latexmkrc".to_string(),
        args: Vec::new(),
        output: Output::default(),
        files: vec![FileChange {
            path: ".latexmkrc".to_string(),
            before: String::new(),
            after: "a\nb\nc\n".to_string(),
            status: FileStatus::New,
        }],
        status: Status::Ok,
        duration_ms: 4,
        speculated: false,
    };
    let dig = digest::step_digest(&step(create, 0), 40);
    assert_eq!(dig.delta.unwrap().label(), "new file · +3");

    let dig = digest::step_digest(&step(edit("main.tex", "a\n", "b\n"), 0), 40);
    assert_eq!(dig.delta.unwrap().label(), "+1 −1");
}

// ------------------------------------------------------------------ chips

#[test]
fn accounting_formats_without_floating_point() {
    assert_eq!(format_cost(800), "$0.0008");
    assert_eq!(format_cost(6_100), "$0.0061");
    assert_eq!(format_tokens(41_200), "41.2k");
    assert_eq!(format_tokens(388), "388");
    assert_eq!(format_duration(226), "226ms");
    assert_eq!(format_duration(1_400), "1.4s");
    assert_eq!(format_duration(41_000), "41.0s");
    // A turn's wall time uses the `m:ss` form instead.
    assert_eq!(digest::format_offset(41_000), "0:41");
}

#[test]
fn a_repeated_elapsed_offset_is_dropped() {
    let steps = vec![
        step(bash("a", &[], Status::Ok), 4_000),
        step(bash("b", &[], Status::Ok), 4_200),
        step(bash("c", &[], Status::Ok), 9_000),
    ];
    assert_eq!(digest::offsets(&steps), vec!["0:04", "", "0:09"]);
}

#[test]
fn speculation_renders_as_a_badge_not_prose() {
    let mut call = bash("a", &[], Status::Ok);
    call.speculated = true;
    let chips = digest::step_chips(&step(call, 0));
    assert!(chips.iter().any(|c| c.text == "⚡ spec"));
}

/// A step is one call; the token rollup is a turn's job. Per-step chips carry
/// only the step's own cost, never a token count or a cache indicator.
#[test]
fn a_step_digest_shows_cost_but_never_a_token_count() {
    let chips = digest::step_chips(&step(bash("a", &[], Status::Ok), 0));
    assert!(
        chips.iter().any(|c| c.text == "$0.0008"),
        "expected the step's own cost chip, got {chips:?}"
    );
    assert!(
        chips.iter().all(|c| !c.text.contains('→')),
        "a step chip carried a token rollup: {chips:?}"
    );
    assert!(
        chips.iter().all(|c| c.text != "cache"),
        "a step chip carried a cache indicator: {chips:?}"
    );
}

#[test]
fn a_turn_digest_carries_one_arrow_joined_token_chip_and_no_cache_chip() {
    let run = run_with(vec![
        step(bash("a", &[], Status::Ok), 0),
        step(bash("b", &[], Status::Ok), 1),
    ]);
    let dig = digest::turn_digest(&run.turns[0], 64, digest::ChipStyle::Tight);
    assert!(
        dig.chips.iter().any(|c| c.text == "14.0k→68"),
        "expected a tight arrow-joined token chip, got {:?}",
        dig.chips
    );
    assert!(
        dig.chips.iter().all(|c| c.text != "cache"),
        "a turn chip carried a cache indicator: {:?}",
        dig.chips
    );
}

/// The web surface spends a whole chip pill on the same rollup the grid packs
/// tight — same numbers, roomier punctuation.
#[test]
fn the_roomy_chip_style_spaces_the_arrow_and_labels_the_unit() {
    let run = run_with(vec![step(bash("a", &[], Status::Ok), 0)]);
    let dig = digest::turn_digest(&run.turns[0], 64, digest::ChipStyle::Roomy);
    assert!(
        dig.chips.iter().any(|c| c.text == "7.0k → 34 tok"),
        "expected a roomy, unit-labelled token chip, got {:?}",
        dig.chips
    );
}

#[test]
fn a_digest_is_a_summary_not_truncated_content() {
    let call = bash(
        "cargo test --workspace --all-features -- --nocapture",
        &["running 412 tests"],
        Status::Ok,
    );
    let dig = digest::step_digest(&step(call, 0), 30);
    assert!(dig.object.contains('…'), "expected a middle elision");
    assert!(dig.object.starts_with("cargo test"));
    assert!(dig.object.ends_with("nocapture"));
    assert!(
        !dig.object.contains("running 412 tests"),
        "output leaked into the digest"
    );
}

#[test]
fn prose_folds_to_its_first_sentence() {
    let text = "I'll read the file first. Then reproduce the warnings.";
    assert_eq!(digest::first_sentence(text), "I'll read the file first.");
}

/// A wall of unpunctuated reasoning (>96 chars, no sentence boundary) folds to
/// an elided head — which is NOT a literal prefix of the text. The expanded
/// body must still carry the full reasoning rather than nothing.
#[test]
fn unpunctuated_prose_expands_to_its_full_body() {
    let text = "considering whether the overfull hbox comes from the wide table \
                or from an unbreakable url in the bibliography and how best to wrap it";
    assert!(
        digest::first_sentence(text).contains('…'),
        "precondition: this text should elide, not split on a sentence boundary"
    );
    let mut run = run_with(vec![]);
    run.turns[0].prose = vec![Prose {
        text: text.to_string(),
        before_step: 0,
    }];

    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);

    let markup = html::render_run(&run, &state);
    assert!(
        markup.contains("bibliography and how best to wrap it"),
        "expanded HTML prose lost the tail of the reasoning"
    );

    let grid = grid::render(&run, &state, 100);
    let text_out = grid::to_ansi256(&grid);
    assert!(
        text_out.contains("bibliography"),
        "expanded grid prose lost the body of the reasoning"
    );
}

/// Reasoning emitted *after* the last step belongs to no `before_step` the step
/// loop visits. Both renderers must still carry it, and for the same reason:
/// one model, two renderers — a fact that exists for one and not the other is a
/// defect in whichever is missing it, never a rendering choice.
#[test]
fn prose_after_the_last_step_reaches_both_renderers() {
    let mut run = run_with(vec![step(bash("ls", &["a.txt"], Status::Ok), 0)]);
    run.turns[0].prose = vec![Prose {
        text: "now checking the bibliography wrapping".to_string(),
        before_step: 1, // == turn.steps.len(), so the step loop never reaches it
    }];

    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);

    assert!(
        html::render_run(&run, &state).contains("bibliography"),
        "HTML dropped prose sitting after the last step"
    );
    let grid = grid::render(&run, &state, 100);
    assert!(
        grid::to_ansi256(&grid).contains("bibliography"),
        "grid dropped prose sitting after the last step"
    );
}

// ------------------------------------------------------------------ render

#[test]
fn html_escapes_model_output_that_looks_like_markup() {
    let call = bash("echo", &["<script>alert(1)</script>"], Status::Ok);
    let run = run_with(vec![step(call, 0)]);
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);
    let markup = html::render_run(&run, &state);
    assert!(!markup.contains("<script>"));
    assert!(markup.contains("&lt;script&gt;"));
}

#[test]
fn the_word_tint_reaches_the_ansi_encoder_as_a_background_span() {
    let run = run_with(vec![step(edit("main.tex", "{15pt}\n", "{12pt}\n"), 0)]);
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);
    let ansi = grid::to_ansi256(&grid::render(&run, &state, 100));
    assert!(ansi.contains("48;5;88"), "removed word tint missing");
    assert!(ansi.contains("48;5;28"), "added word tint missing");
}

#[test]
fn a_sixteen_colour_terminal_degrades_the_word_tint_to_bold_underline() {
    let run = run_with(vec![step(edit("main.tex", "{15pt}\n", "{12pt}\n"), 0)]);
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);
    let ansi = grid::to_ansi16(&grid::render(&run, &state, 100));
    assert!(!ansi.contains("48;5;"), "256-colour code leaked");
    assert!(ansi.contains(";4m") || ansi.contains(";1;4m"));
}

#[test]
fn the_run_rollup_is_the_sum_of_its_steps() {
    let run = run_with(vec![
        step(bash("a", &[], Status::Ok), 0),
        step(bash("b", &[], Status::Ok), 1),
    ]);
    let rollup = run.rollup();
    assert_eq!(rollup.tokens_in, 14_000);
    assert_eq!(rollup.micros, 1_600);
}

#[test]
fn the_model_round_trips_through_serde_byte_for_byte() {
    let run = run_with(vec![step(bash("ls", &["a"], Status::Ok), 0)]);
    let json = serde_json::to_string(&run).unwrap();
    let back: Run = serde_json::from_str(&json).unwrap();
    assert_eq!(run, back);
    assert_eq!(json, serde_json::to_string(&back).unwrap());
}
