//! The jet-black web renderer.
//!
//! Emits `<details>`/`<summary>` for every fold, which is the whole reason this
//! renderer needs no JavaScript to be usable: folds work with the script
//! disabled, they are keyboard-operable because the browser already makes a
//! `<summary>` focusable, and the open/closed state is expressed in the markup
//! rather than in a class a script has to keep in sync.
//!
//! The fixed-width gutter rule is enforced by the CSS grid template rather than
//! by padding: `grid-template-columns: 52px 34px 1fr` cannot shift when a
//! chevron rotates, so "toggling any fold shifts zero columns" is a property of
//! the layout system, not a thing to remember.

use std::fmt::Write as _;

use crate::digest::{self, Chip};
use crate::file_diff::{FileDiff, RowKind};
use crate::fold::FoldState;
use crate::model::{Call, FileStatus, NodeId, Run, Status, Step, Turn};
use crate::syntax;
use crate::word::Span;

/// The stylesheet. Inlined rather than linked because the Observatory serves
/// one embedded asset with no build step, and a second file to keep in sync is
/// a second file to drift.
pub const STYLE: &str = include_str!("html/transcript.css");

/// How many characters of an invocation the digest shows before eliding.
const OBJECT_WIDTH: usize = 46;

/// Escape text for HTML text content and quoted attributes.
///
/// Both contexts at once, deliberately: a single escaper that is safe in the
/// stricter context cannot be misapplied, and a transcript is full of model
/// output that is *designed* to look like markup.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Render a complete standalone page.
#[must_use]
pub fn render_page(run: &Run, state: &FoldState) -> String {
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"UTF-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{}</title>\n<style>\n{STYLE}\n</style>\n</head>\n<body>\n\
         <div class=\"wrap\">{}</div>\n</body>\n</html>\n",
        escape(&run.name),
        render_run(run, state)
    )
}

/// Render the run as a fragment, for embedding in an existing page.
#[must_use]
pub fn render_run(run: &Run, state: &FoldState) -> String {
    let mut out = String::new();
    out.push_str("<div class=\"frame\">");
    run_bar(&mut out, run, state);
    for (index, turn) in run.turns.iter().enumerate() {
        turn_block(&mut out, run, state, turn, index);
    }
    out.push_str("</div>");
    out
}

fn run_bar(out: &mut String, run: &Run, state: &FoldState) {
    let _ = write!(
        out,
        "<div class=\"runbar\"><span class=\"name\">{}</span>\
         <span class=\"meta\">{} · {}</span><div class=\"zoom\">",
        escape(&run.name),
        escape(&run.model),
        escape(&run.started_at)
    );
    for zoom in [
        crate::fold::Zoom::Turns,
        crate::fold::Zoom::Steps,
        crate::fold::Zoom::Everything,
    ] {
        let on = if zoom == state.zoom() {
            " class=\"on\""
        } else {
            ""
        };
        let _ = write!(out, "<span{on}>{}</span>", zoom.label());
    }
    out.push_str("</div></div>");
}

fn turn_block(out: &mut String, run: &Run, state: &FoldState, turn: &Turn, index: usize) {
    let node = NodeId::Turn(index);
    let open = state.is_open(run, node);
    let _ = write!(
        out,
        "<details class=\"turn\" id=\"{}\"{}><summary><div class=\"turnline\">\
         <span class=\"chev\">▶</span><span class=\"turnname\">▍{}</span>\
         <span class=\"st {}\">{} {}</span>",
        node.key(),
        open_attr(open),
        escape(&turn.name),
        turn.status.token(),
        turn.status.glyph(),
        status_word(turn.status),
    );

    let dig = digest::turn_digest(turn, 64, digest::ChipStyle::Roomy);
    if !open {
        let _ = write!(
            out,
            "<span class=\"tdigest\"><span class=\"pq\">{}</span>",
            escape(&dig.prompt_line)
        );
        if !dig.answer_line.is_empty() {
            let _ = write!(
                out,
                " <span class=\"sep\">→</span> <span class=\"aq\">{}</span>",
                escape(&dig.answer_line)
            );
        }
        out.push_str("</span>");
        // Only a collapsed turn puts its accounting on the summary, because
        // the summary is the only thing a collapsed `<details>` renders — a
        // turn that showed no cost until you opened it would hide exactly what
        // a reader scans a transcript for. An expanded turn closes with the
        // receipt below instead, in the same place `grid`'s bottom rail puts
        // it, so the two shared renderers agree about where a turn's outcome
        // is read. Rendering both would state it twice on one open turn.
        chips(out, &dig.chips);
    }
    out.push_str("</div></summary>");

    role_block(out, "you", "YOU", &escape_paragraphs(&turn.prompt), false);
    steps_and_prose(out, run, state, turn, index);
    if let Some(answer) = &turn.answer {
        role_block(out, "agent", "AGENT", &escape_paragraphs(answer), true);
    }
    if open {
        turn_receipt(out, turn, &dig.chips);
    }
    out.push_str("</details>");
}

/// The expanded turn's closing receipt — `grid::turn_frame_bottom`'s counterpart.
///
/// The two renderers deliberately agree on *what* closes a turn (its status
/// word and its chips) and on *where* (after the last thing the turn did).
/// `grid` has no choice about it: its frame has to be printable a line at a
/// time as the turn runs, and neither fact exists when the top rail goes out.
/// Keeping the web surface in step is what stops a reader who compares
/// `stella run`'s scrollback with `stella observe` from finding two documents.
fn turn_receipt(out: &mut String, turn: &Turn, chip_list: &[Chip]) {
    let _ = write!(
        out,
        "<div class=\"turnreceipt\"><span class=\"st {}\">{} {}</span>",
        turn.status.token(),
        turn.status.glyph(),
        status_word(turn.status),
    );
    chips(out, chip_list);
    out.push_str("</div>");
}

fn steps_and_prose(out: &mut String, run: &Run, state: &FoldState, turn: &Turn, ti: usize) {
    let offsets = digest::offsets(&turn.steps);
    for (si, step) in turn.steps.iter().enumerate() {
        for (ni, note) in turn.notes.iter().enumerate() {
            if note.before_step == si {
                note_block(out, run, state, note, ti, ni);
            }
        }
        for (pi, prose) in turn.prose.iter().enumerate() {
            if prose.before_step == si {
                prose_block(out, run, state, prose, ti, pi);
            }
        }
        step_block(out, run, state, step, ti, si, &offsets[si]);
    }
    for (ni, note) in turn.notes.iter().enumerate() {
        if note.before_step >= turn.steps.len() {
            note_block(out, run, state, note, ti, ni);
        }
    }
    for (pi, prose) in turn.prose.iter().enumerate() {
        if prose.before_step >= turn.steps.len() {
            prose_block(out, run, state, prose, ti, pi);
        }
    }
}

/// One non-call row. Mirrors [`note_lines`](crate::grid) on the character grid:
/// the kind supplies a glyph and a class token, the summary is always visible,
/// and detail rows fold behind it — with no fold control at all when there is
/// no detail.
fn note_block(
    out: &mut String,
    run: &Run,
    state: &FoldState,
    note: &crate::model::Note,
    ti: usize,
    ni: usize,
) {
    let node = NodeId::Note { turn: ti, note: ni };
    let foldable = !note.detail.is_empty();
    let open = foldable && state.is_open(run, node);
    let _ = write!(
        out,
        "<div class=\"note note-{}\"><div class=\"notegut\">{}</div>",
        note.kind.token(),
        escape(note.kind.glyph())
    );
    if foldable {
        let _ = write!(
            out,
            "<details class=\"note-fold\" id=\"{}\"{}><summary>\
             <span class=\"chev\">▶</span> {}</summary><div class=\"note-detail\">",
            node.key(),
            open_attr(open),
            escape(&note.summary)
        );
        for row in &note.detail {
            let _ = write!(out, "<div>{}</div>", escape(row));
        }
        out.push_str("</div></details>");
    } else {
        let _ = write!(
            out,
            "<div class=\"note-summary\">{}</div>",
            escape(&note.summary)
        );
    }
    out.push_str("</div>");
}

fn prose_block(
    out: &mut String,
    run: &Run,
    state: &FoldState,
    prose: &crate::model::Prose,
    ti: usize,
    pi: usize,
) {
    let node = NodeId::Prose {
        turn: ti,
        prose: pi,
    };
    let open = state.is_open(run, node);
    let head = digest::first_sentence(&prose.text);
    let trimmed = prose.text.trim();
    // When `first_sentence` finds a real boundary, `head` is a literal prefix and
    // the remainder is what follows it. When it falls back to eliding (no sentence
    // boundary in a long wall of text), `head` is a middle-ellipsis string that is
    // NOT a prefix, so `strip_prefix` returns `None` — in that case show the full
    // text in the expanded body rather than losing everything.
    let rest = trimmed.strip_prefix(head.as_str()).unwrap_or(trimmed);
    let _ = write!(
        out,
        "<div class=\"role agent\"><div class=\"rolegut\">\
         <span class=\"roletag\">AGENT</span></div><div class=\"prose\">\
         <details class=\"prose-fold\" id=\"{}\"{}><summary><span class=\"chev\">▶</span> {}",
        node.key(),
        open_attr(open),
        escape(&head)
    );
    if !rest.trim().is_empty() {
        out.push_str(" <span class=\"rest\">…</span>");
    }
    out.push_str("</summary>");
    if !rest.trim().is_empty() {
        let _ = write!(out, "{}", escape_paragraphs(rest.trim()));
    }
    out.push_str("</details></div></div>");
}

fn step_block(
    out: &mut String,
    run: &Run,
    state: &FoldState,
    step: &Step,
    ti: usize,
    si: usize,
    offset: &str,
) {
    let node = NodeId::Step { turn: ti, step: si };
    let open = state.is_open(run, node);
    let dig = digest::step_digest(step, OBJECT_WIDTH);
    let err = if dig.status == Status::Error {
        " err"
    } else {
        ""
    };

    let _ = write!(
        out,
        "<details class=\"step{err}\" id=\"{}\"{}><summary><div class=\"grid\">\
         <span class=\"t\">{}</span><span class=\"node\">\
         <span class=\"dot {}\" aria-hidden=\"true\">{}</span></span>\
         <div class=\"digest\"><span class=\"chev\">▶</span>\
         <span class=\"tool {}\">{}</span><span class=\"cmd\">{}</span>",
        node.key(),
        open_attr(open),
        escape(offset),
        dig.class,
        dig.monogram,
        dig.class,
        escape(&dig.verb),
        escape(&dig.object),
    );
    if let Some(delta) = &dig.delta {
        let tone = match delta.kind {
            FileStatus::Deleted => "del",
            _ => "add",
        };
        let _ = write!(
            out,
            "<span class=\"dm {tone}\">{}</span>",
            escape(&delta.label())
        );
    }
    if dig.status == Status::Error {
        out.push_str("<span class=\"st err\">✗ failed</span>");
    }
    chips(out, &dig.chips);
    out.push_str("</div></div></summary>");

    if let Some(call) = &step.call {
        out.push_str("<div class=\"body\"><div class=\"body-inner\">");
        call_body(out, run, state, call, &dig.object, ti, si);
        out.push_str("</div></div>");
    }
    out.push_str("</details>");
}

fn call_body(
    out: &mut String,
    run: &Run,
    state: &FoldState,
    call: &Call,
    shown: &str,
    ti: usize,
    si: usize,
) {
    if let Some(command) = digest::command_bar(call, shown) {
        let _ = write!(
            out,
            "<div class=\"cmdbar\"><span class=\"ps\">$</span> {}</div>",
            escape(command)
        );
    }
    args_toggle(out, call);

    if call.tool.is_mutation() {
        for (fi, change) in call.files.iter().enumerate() {
            file_diff(out, run, state, &FileDiff::build(change), ti, si, fi);
        }
        return;
    }
    output_body(out, run, state, call, ti, si);
}

/// The `args` toggle — only rendered when there is something the header did not
/// already say. This is deduplication rule 1's visible half.
fn args_toggle(out: &mut String, call: &Call) {
    let extra = call.extra_args();
    if extra.is_empty() {
        return;
    }
    out.push_str(
        "<details class=\"args\"><summary><span class=\"chev\">▶</span> args</summary><dl>",
    );
    for row in extra {
        let _ = write!(
            out,
            "<dt>{}</dt><dd>{}</dd>",
            escape(&row.key),
            escape(&row.value)
        );
    }
    out.push_str("</dl></details>");
}

fn output_body(out: &mut String, run: &Run, state: &FoldState, call: &Call, ti: usize, si: usize) {
    let fold = digest::fold_output(&call.output, &call.header_object);
    // Decided once, from the whole body, so the head, the hidden middle and the
    // tail cannot disagree — and from the *body* rather than per line, because a
    // JSON object's inner lines do not start with a delimiter. The grid renderer
    // and the Command Deck make the same decision from the same shared function
    // (#3644, #4036).
    let paint = syntax::lines_body_paint(call.read_path(), &fold.body);
    let node = NodeId::Output { turn: ti, step: si };
    let open = state.is_open(run, node);

    if fold.echo_hidden {
        out.push_str("<div class=\"echo\">echo hidden</div>");
    }
    if fold.head.is_empty() && !fold.has_more() {
        return;
    }

    let _ = write!(out, "<div class=\"out\"><pre>");
    emit_lines(out, &fold.head, paint);
    out.push_str("</pre></div>");

    if fold.has_more() {
        let _ = write!(
            out,
            "<details class=\"more\" id=\"{}\"{}><summary><span class=\"c\">{}</span>\
             <span class=\"o\">▾ hide</span></summary><div class=\"out\"><pre>",
            node.key(),
            open_attr(open),
            escape(&fold.more_label())
        );
        let hidden_start = fold.head.len();
        let hidden_end = fold.body.len() - fold.tail.len();
        let hidden = &fold.body[hidden_start.min(hidden_end)..hidden_end];
        emit_lines(out, hidden, paint);
        out.push_str("</pre></div></details>");
    }

    if !fold.tail.is_empty() {
        out.push_str("<div class=\"out tail\"><pre>");
        emit_lines(out, &fold.tail, paint);
        out.push_str("</pre></div>");
    }
}

/// Emit result-body lines, syntax-coloured when `json`.
///
/// The colouring is the one thing this renderer does that the character grid
/// cannot do identically — HTML has classes where a terminal cell has a single
/// foreground — so what the two share is the *lexer*, not the mechanism
/// (`syntax::json_runs`). Both therefore break a line into the same runs and
/// classify each one the same way; only the paint differs. That is the honest
/// reading of "one information model, two renderers", and it is what #3644
/// asked for: the grid and the page had been sharing neither.
fn emit_lines(out: &mut String, lines: &[String], paint: syntax::BodyPaint) {
    for line in lines {
        let painted = syntax::paint_line(paint, line);
        // The emitter's own gutter *is* this renderer's `ln` column. It used to
        // draw a second one counting position in the fold, which both stacked
        // two gutters on one line and stated the wrong number — a read at
        // offset 780 was labelled line 1 (#4036).
        if paint.numbered {
            match painted.gutter {
                Some(g) => {
                    let _ = write!(out, "<span class=\"ln\">{:>4}</span>  ", g.number);
                }
                // The read footer keeps the column's width so the source above
                // it stays in one straight run.
                None => out.push_str("<span class=\"ln\"></span>  "),
            }
        }
        match painted.lang {
            Some(lang) => {
                emit_lexed_line(out, painted.source, lang);
                out.push('\n');
            }
            None => {
                let _ = writeln!(out, "{}", escape(painted.source));
            }
        }
    }
}

/// One line as classified spans.
///
/// Untagged runs are written bare rather than wrapped in a no-op span, so the
/// common case — punctuation and indentation — costs no markup at all. The
/// lexer's losslessness is what makes that safe: concatenating the runs
/// reproduces the line, so nothing is dropped by not wrapping it.
fn emit_lexed_line(out: &mut String, line: &str, lang: syntax::Lang) {
    for (text, tok) in syntax::tokenize(line, lang) {
        match tok {
            Some(t) => {
                let _ = write!(
                    out,
                    "<span class=\"{}\">{}</span>",
                    tok_class(t),
                    escape(&text)
                );
            }
            None => out.push_str(&escape(&text)),
        }
    }
}

/// The CSS class for a token class.
///
/// Short names because a large tool result multiplies them by every run on
/// every line; `transcript.css` carries the matching rules beside the palette
/// they draw from.
fn tok_class(t: syntax::Tok) -> &'static str {
    match t {
        syntax::Tok::Keyword => "tk",
        syntax::Tok::Str => "ts",
        syntax::Tok::Number => "tn",
        syntax::Tok::Comment => "tc",
    }
}

fn file_diff(
    out: &mut String,
    run: &Run,
    state: &FoldState,
    diff: &FileDiff,
    ti: usize,
    si: usize,
    fi: usize,
) {
    let node = NodeId::File {
        turn: ti,
        step: si,
        file: fi,
    };
    let open = state.is_open(run, node);
    let _ = write!(
        out,
        "<details class=\"file\" id=\"{}\"{}><summary><span class=\"chev\">▶</span>\
         <span class=\"fstat {}\"></span><span class=\"fpath\">{}</span>\
         <span class=\"fcount\">",
        node.key(),
        open_attr(open),
        diff.status.token(),
        escape(&diff.path)
    );
    match diff.status {
        FileStatus::Deleted => {
            let _ = write!(out, "<span class=\"d\">deleted · −{}</span>", diff.removed);
        }
        FileStatus::New => {
            let _ = write!(out, "<span class=\"a\">new file · +{}</span>", diff.added);
        }
        FileStatus::Modified => {
            let _ = write!(
                out,
                "<span class=\"a\">+{}</span> <span class=\"d\">−{}</span>",
                diff.added, diff.removed
            );
        }
    }
    out.push_str("</span></summary>");

    if !diff.minimal {
        out.push_str(
            "<div class=\"blunt\">diff exceeded the exact-comparison bound — \
             shown as replace-everything</div>",
        );
    }

    for (hi, hunk) in diff.hunks.iter().enumerate() {
        let hunk_node = NodeId::Hunk {
            turn: ti,
            step: si,
            file: fi,
            hunk: hi,
        };
        let hunk_open = state.is_open(run, hunk_node);
        if hunk.is_large() {
            let _ = write!(
                out,
                "<details class=\"hunk-fold\" id=\"{}\"{}><summary>\
                 <span class=\"chev\">▶</span> {} <span class=\"hn\">{} lines</span></summary>",
                hunk_node.key(),
                open_attr(hunk_open),
                escape(&hunk.header),
                hunk.rows.len()
            );
            diff_table(out, hunk, false);
            out.push_str("</details>");
        } else {
            diff_table(out, hunk, true);
        }
    }
    out.push_str("</details>");
}

fn diff_table(out: &mut String, hunk: &crate::file_diff::RenderHunk, with_header: bool) {
    out.push_str("<table class=\"diff\">");
    if with_header {
        let _ = write!(
            out,
            "<tr class=\"hunk\"><td class=\"no\"></td><td class=\"no\"></td>\
             <td class=\"sign\"></td><td class=\"code\">{}</td></tr>",
            escape(&hunk.header)
        );
    }
    for row in &hunk.rows {
        let _ = write!(
            out,
            "<tr class=\"{}\"><td class=\"no\">{}</td><td class=\"no\">{}</td>\
             <td class=\"sign\">{}</td><td class=\"code\">",
            row.kind.token(),
            row.old_no.map(|n| n.to_string()).unwrap_or_default(),
            row.new_no.map(|n| n.to_string()).unwrap_or_default(),
            row.kind.sigil(),
        );
        emit_spans(out, &row.spans, row.kind);
        out.push_str("</td></tr>");
    }
    out.push_str("</table>");
}

fn emit_spans(out: &mut String, spans: &[Span], kind: RowKind) {
    let hot = match kind {
        RowKind::Removed => "dw",
        RowKind::Added => "aw",
        RowKind::Context => "",
    };
    for span in spans {
        if span.changed && !hot.is_empty() {
            let _ = write!(out, "<span class=\"{hot}\">{}</span>", escape(&span.text));
        } else {
            out.push_str(&escape(&span.text));
        }
    }
}

fn role_block(out: &mut String, class: &str, tag: &str, body: &str, answer: bool) {
    let inner = if answer { " answer" } else { "" };
    let _ = write!(
        out,
        "<div class=\"role {class}\"><div class=\"rolegut\">\
         <span class=\"roletag\">{tag}</span></div>\
         <div class=\"prose{inner}\">{body}</div></div>"
    );
}

fn chips(out: &mut String, chips: &[Chip]) {
    if chips.is_empty() {
        return;
    }
    out.push_str("<span class=\"chips\">");
    for chip in chips {
        let _ = write!(
            out,
            "<span class=\"chip {}\">{}</span>",
            chip.tone,
            escape(&chip.text)
        );
    }
    out.push_str("</span>");
}

fn escape_paragraphs(text: &str) -> String {
    text.trim()
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(|p| format!("<p>{}</p>", escape(p.trim())))
        .collect()
}

fn open_attr(open: bool) -> &'static str {
    if open { " open" } else { "" }
}

fn status_word(status: Status) -> &'static str {
    match status {
        Status::Ok => "done",
        Status::Warn => "warnings",
        Status::Error => "failed",
        Status::Running => "running",
    }
}
