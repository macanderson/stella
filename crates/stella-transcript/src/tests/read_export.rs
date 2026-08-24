//! A `read_file` body is source, and the export renders it as source.
//!
//! In its own file because these tests carry their own fixture vocabulary:
//! [`numbered`] reproduces the emitter's `{line_num:>6}\t{line}` shape so a
//! test cannot drift from the format it is reading, and [`read_call`] wraps it
//! in the call the renderers see. Nothing outside this module uses either.
//! `rendered` stays in the parent — `fields_projection` calls it too.
//!
//! What they hold down is that the file's own line numbers survive the round
//! trip exactly once, and that the deck and the export paint a body the same
//! way — a decision that lives in one place precisely because copies of it
//! drift (#3644).

use super::*;

// The painter these tests hold to one copy. It moved here with them: after the
// split this module is its only caller in the test tree.
use crate::syntax;

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
