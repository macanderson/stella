//! Tests, including one per acceptance check in the specification.
//!
//! The acceptance checks are named `acceptance_*` so a reader can find the
//! contract without reading the rest, and each asserts the *property* rather
//! than a rendered string — a golden frame proves what the renderer did today,
//! a property proves what it may never do.

// The one body transform this crate applies before it folds anything, kept in
// its own file because its contract is a property (whitespace only) rather than
// a rendered shape, and a property wants room to state itself.
mod json_reindent;

// The other half of that decision: a body that *does* parse is projected into
// fields, so all three surfaces read a JSON result the same way (#4340).
mod fields_projection;

// The turn frame, and the append-only property the plain surface streams on.
mod frame;

// The width contract, and the generator that proves it over content nobody
// wrote by hand.
mod width;

// `render_turn_tail`'s cutoffs (#4566's fix shape (a)).
mod tail;

// What each file mutation renders as, and where its line numbers come from.
mod file_kinds;

use crate::digest::{self, format_cost, format_duration, format_tokens};
use crate::file_diff::{FileDiff, RowKind};
use crate::fold::{Command, Cursor, FoldState, Zoom, apply};
use crate::grid;
use crate::html;
use crate::model::*;
use crate::syntax;
use crate::word;
use unicode_width::UnicodeWidthStr;

fn plain_span(text: &str) -> word::Span {
    word::Span {
        text: text.to_string(),
        changed: false,
    }
}

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
            patch: None,
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
            notes: Vec::new(),
            steps,
            answer: Some("All three overfull warnings are gone.".to_string()),
            status,
            duration_ms: 41_000,
        }],
    }
}

// ------------------------------------------------- a file read is source

/// One `read_file` body line, in the emitter's own shape
/// (`stella-tools`' `{line_num:>6}\t{line}`), so a test cannot drift from the
/// format it is meant to be reading.
fn numbered(first: usize, source: &[&str]) -> Vec<String> {
    source
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:>6}\t{l}", first + i))
        .collect()
}

fn read_call(path: &str, lines: Vec<String>) -> Call {
    Call {
        tool: ToolKind::ReadFile,
        header_object: path.to_string(),
        args: Vec::new(),
        output: Output { lines, clipped: 0 },
        files: Vec::new(),
        status: Status::Ok,
        duration_ms: 1,
        speculated: false,
    }
}

/// Both export renderers, with everything open.
fn rendered(call: Call) -> (String, String) {
    let run = run_with(vec![step(call, 0)]);
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);
    (
        grid::to_plain(&grid::render(&run, &state, 100)),
        html::render_run(&run, &state),
    )
}

/// A read is numbered by the file's own line numbers, exactly once.
///
/// The witness, and it is two defects in one row. Both renderers drew their own
/// gutter from `ToolKind::ReadFile` while the payload already carried
/// `read_file`'s — so a body line rendered as `1     780\t#[test]`, two gutters
/// deep. And the number they drew was the line's index *in the fold*, not in the
/// file: a read at offset 780 was labelled line 1, which is not a cosmetic
/// error but a wrong fact about where the code lives.
#[test]
fn an_exported_read_is_numbered_once_by_the_files_own_line_numbers() {
    let (plain, markup) = rendered(read_call(
        "src/lib.rs",
        numbered(780, &["#[test]", "fn f() {}"]),
    ));

    for (surface, text) in [("grid", &plain), ("html", &markup)] {
        assert!(
            text.contains("780"),
            "{surface} lost the file's own line number:\n{text}"
        );
        assert!(
            !text.contains('\t'),
            "{surface} left the emitter's gutter tab in the output:\n{text}"
        );
    }
    // The synthetic index is gone: nothing claims this excerpt starts at line 1.
    let body: Vec<&str> = plain.lines().filter(|l| l.contains("#[test]")).collect();
    assert_eq!(body.len(), 1, "expected one row for the line:\n{plain}");
    let (before, _) = body[0].split_once("#[test]").unwrap();
    assert!(
        !before.contains(" 1 ") && !before.contains("  1"),
        "a second, synthetic gutter is still drawn: {:?}",
        body[0]
    );
    assert_eq!(
        markup.matches("class=\"ln\"").count(),
        2,
        "expected exactly one line-number span per body row:\n{markup}"
    );
}

/// A read is coloured in the language of the file it read, on both surfaces.
#[test]
fn an_exported_read_is_coloured_in_its_files_language() {
    let (_, markup) = rendered(read_call(
        "src/lib.rs",
        numbered(1, &["fn f() -> u8 { 1 }"]),
    ));
    assert!(
        markup.contains("<span class=\"tk\">fn</span>"),
        "a Rust keyword was not classified in the export:\n{markup}"
    );
    assert!(
        markup.contains("<span class=\"tn\">1</span>"),
        "a Rust literal was not classified in the export:\n{markup}"
    );
}

/// Reading a `.json` file is coloured too — the sharper half of #4019, which
/// the export inherited unchanged: the gutter defeated `body_reads_as_json`
/// just as thoroughly as it defeated everything else, so the one format these
/// renderers were built to colour was the one a file read could never get.
#[test]
fn an_exported_read_of_a_json_file_is_coloured_despite_its_gutter() {
    let (_, markup) = rendered(read_call(
        ".stella/settings.json",
        numbered(1, &["{\"name\": \"stella\"}"]),
    ));
    assert!(
        markup.contains("<span class=\"tk\">&quot;name&quot;</span>"),
        "a JSON key read through read_file was not coloured:\n{markup}"
    );
}

/// The line number is never lexed as a numeric literal.
#[test]
fn the_exported_gutter_is_not_lexed_as_source() {
    let (_, markup) = rendered(read_call("src/lib.rs", numbered(780, &["fn f() {}"])));
    assert!(
        !markup.contains("<span class=\"tn\">780</span>"),
        "the line number was classified as a literal:\n{markup}"
    );
    assert!(
        markup.contains("<span class=\"ln\"> 780</span>"),
        "the line number lost its own column:\n{markup}"
    );
}

/// A read of an extension no lexer covers keeps its gutter and gains no colour.
///
/// The guard on the rest: stripping the gutter and classifying source are two
/// decisions, and only one of them depends on knowing the language.
#[test]
fn an_exported_read_of_an_unknown_extension_is_numbered_but_not_coloured() {
    let (plain, markup) = rendered(read_call(
        "notes/scratch.xyz",
        numbered(12, &["fn not really any language"]),
    ));
    assert!(
        plain.contains("12"),
        "an unlexed read lost its numbering:\n{plain}"
    );
    for class in ["\"tk\"", "\"ts\"", "\"tn\"", "\"tc\""] {
        assert!(
            !markup.contains(class),
            "an unlexed extension picked up {class} colouring:\n{markup}"
        );
    }
}

/// **Both export renderers gain the grammar in one change (#4283).**
///
/// The issue's whole point was *where* the upgrade lands. A deck-local
/// highlighter would have left this test impossible to write: a Go body would
/// light up on the deck and stay flat in an exported transcript and in the
/// Observatory, which is the drift #3644 and #4036 each closed once. So the
/// witness is stated against the two surfaces that are not the deck, and it
/// covers both classes the upgrade added — a class with no palette entry in one
/// renderer is a surface disagreeing about hue, which is the other half of what
/// the palette split is allowed to differ on.
#[test]
fn a_go_read_reaches_both_export_renderers_with_the_grammars_classes() {
    let source = numbered(1, &["func Sum(a int) error {", "\treturn nil", "}"]);
    let (_, markup) = rendered(read_call("cmd/serve/main.go", source.clone()));
    for (class, what) in [
        ("\"tk\"", "the `func` keyword"),
        ("\"ty\"", "the `int` type"),
        ("\"tf\"", "the `Sum` function name"),
    ] {
        assert!(
            markup.contains(class),
            "the Observatory renderer lost {what} ({class}):\n{markup}"
        );
    }

    let run = run_with(vec![step(read_call("cmd/serve/main.go", source), 0)]);
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);
    let lines = grid::render(&run, &state, 100);
    for (color, what) in [
        (grid::Color::Cyan, "a type"),
        (grid::Color::Magenta, "a function name"),
    ] {
        assert!(
            lines.iter().flatten().any(|cell| cell.fg == color),
            "the export grid painted no cell as {what} ({color:?}) — a Go body \
             reached it uncoloured"
        );
    }
}

/// A grid row's measured width is the width it draws at.
///
/// A tab is zero columns to `unicode-width` and a real stop to a terminal, so a
/// row holding one was drawn wider than `line_width` reported — and every
/// column measured from that walks. Unlike the deck, where ratatui *deletes* the
/// tab (#4020), here it survives into the cell and the disagreement is silent.
#[test]
fn a_grid_row_measures_what_it_draws() {
    let call = read_call("main.go", numbered(1, &["func f() {", "\treturn 1", "}"]));
    let run = run_with(vec![step(call, 0)]);
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);

    for line in &grid::render(&run, &state, 100) {
        let text: String = line.iter().map(|c| c.text.as_str()).collect();
        assert!(
            !text.contains('\t'),
            "a tab survived into a grid cell: {text:?}"
        );
        assert_eq!(
            grid::line_width(line),
            UnicodeWidthStr::width(text.as_str()),
            "measured width disagrees with the drawn text: {text:?}"
        );
    }
}

/// The three surfaces classify a body identically.
///
/// The cross-surface half, and a genuine round trip rather than a restatement
/// of a shared constant: each renderer asks [`syntax::lines_body_paint`] and
/// then splits each line with [`syntax::paint_line`], and this asserts the two
/// answers are the same ones the deck's own inputs produce. #3644 exists
/// because copies of a rendering decision drift; this is the assertion that
/// there is one copy.
#[test]
fn every_surface_paints_a_body_the_same_way() {
    let cases: [(&str, &[&str]); 4] = [
        ("src/lib.rs", &["fn f() {}"]),
        ("settings.json", &["{\"a\": 1}"]),
        ("notes.xyz", &["whatever"]),
        ("script.py", &["def f(): return 1"]),
    ];
    for (path, source) in cases {
        let lines = numbered(1, source);
        let call = read_call(path, lines.clone());

        // What the export decides, through the model.
        let export = syntax::lines_body_paint(call.read_path(), &lines);
        // What the deck decides, from the same path and the same body as one
        // string — its shape, not the export's.
        let deck = syntax::body_paint(Some(path), &lines.join("\n"));
        assert_eq!(
            export, deck,
            "{path}: the deck and the export disagree about how to paint this body"
        );

        // And the per-line split, which is where the gutter decision lives.
        for line in &lines {
            let painted = syntax::paint_line(export, line);
            assert!(
                painted.gutter.is_some(),
                "{path}: a numbered line lost its gutter"
            );
            assert!(
                !painted.source.starts_with('\t'),
                "{path}: the gutter tab leaked into the source"
            );
        }
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

/// A tool is NAMED exactly once per call, in either renderer.
///
/// The sibling of the command check above, and the half that was never pinned.
/// A journal carries a call and its result as two events, and a surface that
/// renders them as two rows prints the tool's name on both — so a reader
/// scrolling a run sees `bash … bash … bash …`, twice per call, with the second
/// one naming nothing the first did not. That is exactly what the arena's
/// transcript did until `mergeToolRows`, and the reason it could not happen
/// *here* is structural: a [`Call`] owns its [`Output`], so there is one header
/// and no second place for the name to live.
///
/// Pinned anyway, because "structurally impossible" is a claim about today's
/// shape: a later result row added to either renderer would reintroduce it
/// silently, and the Observatory renders through this crate.
#[test]
fn acceptance_tool_is_named_exactly_once_per_call() {
    // A body that repeats the tool's own name, so a renderer echoing the result
    // under its own header cannot pass by accident.
    let call = bash("ls -la", &["bash: command output", "a.txt"], Status::Ok);
    let run = run_with(vec![step(call, 0)]);
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);

    // The name as a *header* — `>bash<` in the markup's tool span, and the
    // padded verb column in the grid — rather than the bare substring, which
    // the output line above deliberately also contains.
    let markup = html::render_run(&run, &state);
    assert_eq!(
        markup.matches("class=\"tool bash\">bash</span>").count(),
        1,
        "tool named more than once:\n{markup}"
    );

    let plain = grid::to_plain(&grid::render(&run, &state, 120));
    let header_rows = plain.lines().filter(|line| line.contains("ls -la")).count();
    assert_eq!(header_rows, 1, "tool header rendered twice:\n{plain}");
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
        patch: None,
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
    // Nine lines: past `PREVIEW_LINES` but short of `TAIL_FOLD_THRESHOLD`, so
    // this is the head-only fold. It used to be six, which was past the old
    // three-line head — the same shape, restated against the shared preview
    // budget the deck also uses now (#3644).
    let out = output(&["1", "2", "3", "4", "5", "6", "7", "8", "9"]);
    let fold = digest::fold_output(&out, "cmd");
    assert_eq!(fold.head.len(), digest::PREVIEW_LINES);
    assert!(fold.tail.is_empty(), "a short fold keeps no tail");
    assert_eq!(fold.hidden, 3);
    assert_eq!(fold.more_label(), "▸ 3 more lines");
}

/// The invariant `PREVIEW_LINES` exists to state: **however** an output folds,
/// a reader sees the same number of lines of it.
///
/// Before this, the head-only fold and the head…tail fold showed different
/// totals (three against five), and the Command Deck showed a third number
/// again (six) from its own constant — so "how much of this tool's output do I
/// get" depended on both which surface you opened and how long the output
/// happened to be. `crates/stella-tui/src/render/tests/tool_output.rs` is the other half
/// of this: the same assertion made against the deck's renderer.
#[test]
fn every_fold_shows_the_same_number_of_lines_whatever_its_shape() {
    for total in 0..40usize {
        let lines: Vec<String> = (0..total).map(|i| format!("line {i}")).collect();
        let out = Output { lines, clipped: 0 };
        let fold = digest::fold_output(&out, "cmd");
        let shown = fold.head.len() + fold.tail.len();
        assert_eq!(
            shown,
            total.min(digest::PREVIEW_LINES),
            "a {total}-line output shows {shown} lines, not the shared preview budget"
        );
        assert_eq!(
            fold.hidden,
            total - shown,
            "a {total}-line output must account for every line it does not show"
        );
    }
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
        patch: None,
    });
    for row in diff.hunks.iter().flat_map(|h| h.rows.iter()) {
        if row.kind == RowKind::Context {
            assert!(row.spans.iter().all(|s| !s.changed));
        }
    }
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

/// A minified line is one line, and nothing upstream bounds how many tokens it
/// holds: the token LCS over a 60,000-character pair is 3.6 billion `u32`
/// cells, about 14 GB. Over [`word::LCS_CELL_CAP`] the pair degrades to the
/// plain spans a too-dissimilar pair returns, instantly. Witness for #3739.
#[test]
fn a_very_long_line_pair_degrades_instead_of_allocating_the_table() {
    let old = "{\"k\":1},".repeat(7_500);
    let new = format!("{old}{{\"k\":2}}");
    assert!(word::tokenize(&old).len() > 50_000, "input is not dense");

    let started = std::time::Instant::now();
    let (old_spans, new_spans) = word::highlight(&old, &new);
    let elapsed = started.elapsed();

    assert_eq!(old_spans, vec![plain_span(&old)]);
    assert_eq!(new_spans, vec![plain_span(&new)]);
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "the guarded path took {elapsed:?} — the quadratic table was built"
    );
}

/// The same hazard through the character-level re-diff: one 60,000-character
/// *token* is a single changed run, so the token LCS is 1x1 and passes, and it
/// is [`word::refine_short_runs`]'s per-character table that would allocate.
#[test]
fn a_very_long_single_token_change_never_reaches_the_character_table() {
    let old = "x".repeat(60_000);
    let new = format!("{old}y");
    assert_eq!(word::tokenize(&old).len(), 1, "input is not one token");

    let started = std::time::Instant::now();
    let (old_spans, new_spans) = word::highlight(&old, &new);
    let elapsed = started.elapsed();

    assert_eq!(old_spans, vec![plain_span(&old)]);
    assert_eq!(new_spans, vec![plain_span(&new)]);
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "the guarded path took {elapsed:?} — the character table was built"
    );
}

/// A pair just under the cap still gets word granularity: the guard bounds the
/// pathological line without quietly disabling the feature on a long one.
#[test]
fn a_long_line_under_the_cap_still_gets_word_highlights() {
    let prefix = "a,".repeat(200);
    let (_, new) = word::highlight(&format!("{prefix}old"), &format!("{prefix}new"));
    let hot: String = new
        .iter()
        .filter(|s| s.changed)
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(hot, "new", "a 400-token line lost its word highlight");
}

/// A CJK ideograph is one `char` and two terminal columns. Measuring a row in
/// characters therefore under-counts exactly the text a fixed column cannot
/// absorb: the fill computed from the undercount is too wide, the row runs past
/// the grid, and every gutter to the right of it walks. Witness for #3740.
#[test]
fn double_width_text_keeps_every_row_inside_the_grid() {
    const WIDTH: usize = 100;
    let mut run = run_with(vec![step(
        edit("src/日本語/データ処理.rs", "a\n", "b\n"),
        1_000,
    )]);
    // Spaced so the wrapper has somewhere to break: an unbroken run of any
    // script overflows a fixed grid, which is a wrapping limit rather than a
    // measurement one.
    run.turns[0].prompt = "日本語 の 警告 を 直して ください ".repeat(6);
    run.turns[0].answer = Some("警告 は ✅ 全部 消えました ".repeat(6));
    run.turns[0].prose = vec![Prose {
        text: "Reading the file first.".to_string(),
        before_step: 0,
    }];

    let lines = grid::render(&run, &FoldState::new(), WIDTH);

    let mut saw_wide = false;
    let mut overruns = Vec::new();
    for line in &lines {
        let text: String = line.iter().map(|c| c.text.as_str()).collect();
        // The oracle is `unicode-width` over the row's own text, never
        // `grid::line_width`: measuring the renderer with the measure under
        // test is how this bug stayed invisible: every row it over-fills also
        // reports itself as fitting.
        let width = UnicodeWidthStr::width(text.as_str());
        saw_wide |= width > text.chars().count();
        assert_eq!(
            grid::line_width(line),
            width,
            "the grid disagrees with the terminal about {text:?}"
        );
        // Every offending row, not just the first: the failure this guards
        // against walks a whole column, and one row out of context reads as a
        // one-off.
        if width > WIDTH {
            overruns.push(format!("{width} cells: {text:?}"));
        }
    }
    assert!(saw_wide, "no double-width text survived to the grid");
    assert!(
        overruns.is_empty(),
        "{} of {} rows overran a {WIDTH}-cell grid:\n{}",
        overruns.len(),
        lines.len(),
        overruns.join("\n")
    );
}

/// The step digest's chips are flush right, and a double-width object must not
/// move them: the row occupies the grid's width either way.
#[test]
fn a_double_width_object_leaves_the_chip_column_flush_right() {
    const WIDTH: usize = 100;
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Steps);
    let widths: Vec<usize> = ["src/lib.rs", "src/日本語/データ処理.rs"]
        .iter()
        .map(|path| {
            let run = run_with(vec![step(edit(path, "a\n", "b\n"), 1_000)]);
            let lines = grid::render(&run, &state, WIDTH);
            let row = lines
                .iter()
                .find(|l| l.iter().any(|c| c.text.contains("edit_file")))
                .expect("no step digest row");
            let text: String = row.iter().map(|c| c.text.as_str()).collect();
            UnicodeWidthStr::width(text.as_str())
        })
        .collect();
    assert_eq!(
        widths,
        vec![WIDTH, WIDTH],
        "the double-width path moved the right edge of its row"
    );
}

// --------------------------------------------------------------------- notes

/// Every note glyph occupies exactly one terminal cell.
///
/// `grid.rs`'s column discipline is that a fixed-width gutter only stays fixed
/// if nobody writes a variable-width prefix into it. `NoteKind::glyph` feeds
/// the offset column, so a two-cell glyph (a default-emoji-presentation
/// codepoint like U+23F8 is the trap) walks every column to its right on the
/// rows that use it — and only on those rows, which is the hardest kind of
/// misalignment to spot in a screenshot.
#[test]
fn every_note_glyph_is_one_display_cell() {
    use crate::model::NoteKind;
    for kind in [
        NoteKind::Stage,
        NoteKind::Context,
        NoteKind::Meter,
        NoteKind::Wait,
        NoteKind::Verdict,
        NoteKind::Handoff,
        NoteKind::Other,
    ] {
        assert_eq!(
            grid::cells(kind.glyph()),
            1,
            "{:?}'s glyph {:?} is not one display cell",
            kind,
            kind.glyph()
        );
    }
}

fn run_with_notes(notes: Vec<Note>) -> Run {
    let mut run = run_with(vec![step(
        bash("pdflatex main.tex", &["ok"], Status::Ok),
        1_000,
    )]);
    run.turns[0].notes = notes;
    run
}

/// A note the model has no step for still reaches the transcript.
///
/// This is the regression the `Note` node exists to prevent: the deck carries
/// some twenty-odd non-call entry kinds, and before notes existed a surface
/// rendering through this model could only drop them.
#[test]
fn a_note_renders_its_summary_as_its_own_row() {
    let run = run_with_notes(vec![Note {
        kind: NoteKind::Context,
        summary: "recalled 5 frames · 408 tok · 126ms".to_string(),
        detail: Vec::new(),
        before_step: 0,
        inspect: None,
    }]);
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Steps);

    let plain = grid::to_plain(&grid::render(&run, &state, 100));

    assert!(
        plain.contains("recalled 5 frames · 408 tok · 126ms"),
        "the note's summary never reached the grid:\n{plain}"
    );
}

/// A note with detail folds it away at `Steps` and reveals it when opened; a
/// note with no detail advertises no fold control at all.
#[test]
fn note_detail_folds_and_a_detailless_note_offers_no_control() {
    let with_detail = run_with_notes(vec![Note {
        kind: NoteKind::Context,
        summary: "recalled 2 frames".to_string(),
        detail: vec!["symbol fn find — stella-cli/src/config_wiring.rs".to_string()],
        before_step: 0,
        inspect: None,
    }]);
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Steps);

    let closed = grid::to_plain(&grid::render(&with_detail, &state, 100));
    assert!(
        !closed.contains("config_wiring.rs"),
        "detail leaked while folded:\n{closed}"
    );
    let summary_row = closed
        .lines()
        .find(|l| l.contains("recalled 2 frames"))
        .expect("summary row");
    assert!(
        summary_row.contains(grid::fold_mark(false)),
        "a foldable note must show a fold control: {summary_row:?}"
    );

    state.toggle(&with_detail, NodeId::Note { turn: 0, note: 0 });
    let opened = grid::to_plain(&grid::render(&with_detail, &state, 100));
    assert!(
        opened.contains("config_wiring.rs"),
        "detail never appeared when opened:\n{opened}"
    );

    let bare = run_with_notes(vec![Note {
        kind: NoteKind::Meter,
        summary: "spend $0.0085".to_string(),
        detail: Vec::new(),
        before_step: 0,
        inspect: None,
    }]);
    let bare_row = grid::to_plain(&grid::render(&bare, &FoldState::new(), 100))
        .lines()
        .find(|l| l.contains("spend $0.0085"))
        .expect("meter row")
        .to_string();
    assert!(
        !bare_row.contains(grid::fold_mark(false)) && !bare_row.contains(grid::fold_mark(true)),
        "a note with no detail must not advertise a fold control: {bare_row:?}"
    );
}

/// Notes, prose and steps interleave in the order they happened.
#[test]
fn notes_interleave_with_steps_by_before_step() {
    let mut run = run_with(vec![
        step(bash("first", &["a"], Status::Ok), 0),
        step(bash("second", &["b"], Status::Ok), 1_000),
    ]);
    run.turns[0].prose = Vec::new();
    run.turns[0].notes = vec![
        Note {
            kind: NoteKind::Stage,
            summary: "BEFORE-FIRST".to_string(),
            detail: Vec::new(),
            before_step: 0,
            inspect: None,
        },
        Note {
            kind: NoteKind::Verdict,
            summary: "AFTER-LAST".to_string(),
            detail: Vec::new(),
            before_step: 2,
            inspect: None,
        },
    ];
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Steps);
    let plain = grid::to_plain(&grid::render(&run, &state, 100));

    let at = |needle: &str| {
        plain
            .lines()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle} missing from:\n{plain}"))
    };
    assert!(
        at("BEFORE-FIRST") < at("first"),
        "note ordered after its step"
    );
    assert!(
        at("AFTER-LAST") > at("second"),
        "a note past the last step must still render (the trailing pass)"
    );
}

/// Both renderers show a note. The crate's charter is one model and *two*
/// renderers, so a node the grid draws and the page drops is precisely the
/// drift this crate was extracted to end.
#[test]
fn both_renderers_show_a_note() {
    let run = run_with_notes(vec![Note {
        kind: NoteKind::Handoff,
        summary: "delegated to reviewer subagent".to_string(),
        detail: vec!["attempt 1 of 3".to_string()],
        before_step: 0,
        inspect: None,
    }]);
    let state = FoldState::new();

    let grid_out = grid::to_plain(&grid::render(&run, &state, 100));
    let page = html::render_page(&run, &state);

    assert!(
        grid_out.contains("delegated to reviewer subagent"),
        "grid dropped the note"
    );
    assert!(
        page.contains("delegated to reviewer subagent"),
        "html dropped the note"
    );
    // The fold control exists on the page, addressed by the same node key the
    // grid folds on, so a cursor round-trips between surfaces.
    assert!(
        page.contains(&NodeId::Note { turn: 0, note: 0 }.key()),
        "the note carries no stable node id on the page"
    );
}
