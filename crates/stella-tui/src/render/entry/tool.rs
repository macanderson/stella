//! The tool-call block: a call's argument body, and its result's row, preview
//! and inline diff.
//!
//! Split out of `entry.rs` rather than grown there, on `recall.rs`'s test: the
//! recall table earned its own module because it lays out a *grid*, and this
//! earns one because it stacks four rendering decisions that are each their own
//! concern — syntax highlighting in the body's own language, the emitter's
//! line-number gutter, a preview window anchored on a salient line, and a
//! truncation notice that has to agree with what the export and Observatory
//! surfaces fold. It is the densest arm of `entry_body` and the one that grows
//! whenever a tool gains a rendering decision (#4019, #4020, #4036, #4123,
//! #4155, #4169, #4210 all landed in it), which is why the room is made
//! deliberately instead of by whoever next needs it (#4230).
//!
//! **No behaviour changes**, and every body below is the one it replaced in
//! `entry.rs` line for line. Two *signatures* moved rather than being copied,
//! and both are named here so the diff is reviewable as a move:
//!
//! * [`result_body`] takes the whole [`TranscriptEntry`] instead of the seven
//!   fields the `match` arm had destructured. Seven fields plus the view and
//!   the three render parameters is eleven arguments, over
//!   `clippy::too_many_arguments`; the honest answer to that lint is fewer
//!   arguments, not an `#[allow]` (CLAUDE.md's rule on expedients).
//! * `argument_rows` spells its first parameter `Color` rather than
//!   `ratatui::style::Color`, which is the same type under this module's
//!   imports.
//!
//! A child module reaches its parent's private items, so nothing widened.
//!
//! The call **head** is deliberately not here. It is v2's
//! ([`crate::v2::transcript_source::head_rows`]) and is drawn by `entry`'s v2
//! router; what this module owns is the block that hangs beneath a head, on the
//! head's own rail.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::model::TranscriptEntry;
use crate::render::row::*;
use crate::{diff, syntax, theme};

// Owned by `render`, two levels up: `resolve_inline_diff` and
// `resolve_inline_delta_total` read the draw-side file list and
// `INLINE_DIFF_CAP` bounds the collapsed rendering. Named here so the doc links
// below resolve.
use crate::render::{INLINE_DIFF_CAP, resolve_inline_delta_total, resolve_inline_diff};

/// How a tool-result body is colored, and the gutter parser that goes with it.
///
/// Both are [`stella_transcript::syntax`]'s now rather than this file's. They
/// were written here for #4019 and moved down in #4036 for the reason the JSON
/// predicate moved down in #3644: the export and Observatory renderers ask the
/// identical question of the identical bodies, and a rendering decision held in
/// three copies is a rendering decision that drifts. The deck keeps the
/// *palette* ([`syntax::tok_style`]) and nothing else.
use stella_transcript::syntax::{BodyPaint, body_paint, paint_line};

use super::EntryView;

/// How many lines of a *successful* tool result the collapsed fold shows.
///
/// This was 1, on the argument that a successful call's output is chatter and
/// its size belongs in the metric column. That is wrong for the calls whose
/// output *is* the answer — a `search`, a `read_file`, a `get_state` — where
/// one line plus a count told a reader only that something had been found, and
/// left the finding itself behind a keystroke they had no reason to press.
///
/// It is now [`stella_transcript::digest::PREVIEW_LINES`] rather than a number
/// of this crate's own, because the export and Observatory surfaces fold the
/// same result through `digest::fold_output` and were answering "how much do I
/// see" differently — six lines here against three there, for one run (#3644).
/// Equality by *construction*; `render::tests::tool_output` then asserts the two
/// renderers really do show the same count, since sharing a constant does not
/// by itself prove two fold implementations agree.
///
/// It still equals [`FAIL_PREVIEW`], which keeps one preview rule instead of
/// two; that is now a fact a test pins rather than a definition.
const OK_PREVIEW: usize = stella_transcript::digest::PREVIEW_LINES;

/// Emit one body line at the detail column, colored per `paint`.
///
/// The deck renders the emitter's gutter as it arrived, rather than as its own
/// column: the transcript is a scrollback, and a reader who wants to open the
/// file at that line wants the number the tool actually printed.
fn push_body_line(
    margin: &[Span<'static>],
    line: &str,
    paint: BodyPaint,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    let painted = paint_line(paint, line);
    let Some(lang) = painted.lang else {
        push_detail_line(margin, line, width, out);
        return;
    };
    // The deck's single highlight site, and so where SPEC 6.4's "once when the
    // event arrives, never per frame" budget is counted. See
    // [`crate::syntax::lex_count`], and `v2::session::fold` for what holds it.
    #[cfg(test)]
    syntax::lex_count::bump();
    let mut spans: Vec<Span<'static>> = Vec::new();
    if let Some(gutter) = painted.gutter {
        // The gutter is chrome, not source: it wears the dimmest text tone so
        // the eye reads down the code and not down the numbers.
        spans.push(Span::styled(
            gutter.text.to_owned(),
            Style::new().fg(theme::TEXT_TERTIARY),
        ));
    }
    spans.extend(
        syntax::tokenize(painted.source, lang)
            .into_iter()
            .map(|(text, tok)| match tok {
                Some(t) => Span::styled(text, syntax::tok_style(t)),
                // Punctuation and whitespace keep the body's muted base tone, so
                // the colored tokens are what the eye lands on.
                None => Span::styled(text, Style::new().fg(theme::MUTED)),
            }),
    );
    push_detail_spans(margin, spans, width, out);
}

/// The `ctrl+o` body of a tool **call**: its full argument object, laid out and
/// coloured under the head, on the head's own rail.
///
/// Pretty-printed rather than shown as it arrived, because that is what makes
/// the colouring worth having — a compact one-line object has no shape for a
/// key hue to mark. `raw` is capped to a char budget at fold time, so an
/// over-budget argument may not re-parse; it is still lexed and wrapped rather
/// than clipped at the pane edge, since it is capped JSON and not some other
/// format.
///
/// `metal` comes from [`crate::v2::transcript_source::head_metal`] rather than
/// from a `Rail`, so these rows carry the same rail colour as the head they
/// hang from: a `read_file`'s block is silver-dim end to end, a mutation's is
/// gold end to end.
pub(super) fn argument_rows(metal: Color, raw: &str, width: usize, out: &mut Vec<Line<'static>>) {
    let margin = block_margin(Style::new().fg(metal));
    // Read as fields, never printed as JSON (`crate::v2::fields`). `raw` is
    // capped to a char budget at fold time, so an over-budget argument may not
    // re-parse; it is then lexed and wrapped rather than clipped at the pane
    // edge, since it is capped JSON and not some other format.
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(value) => {
            for line in crate::v2::fields::rows(&value) {
                push_detail_spans(&margin, line.spans, width, out);
            }
        }
        Err(_) => {
            for l in raw.lines() {
                push_body_line(&margin, l, BodyPaint::json(), width, out);
            }
        }
    }
}

/// The body of a [`TranscriptEntry::ToolResult`]: its metric row, its preview
/// or full output, and the mutation's inline diff.
///
/// Takes the whole entry rather than the seven fields the `match` arm had in
/// hand, because seven plus the view and the three render parameters is over
/// `clippy::too_many_arguments` and the honest fix is fewer arguments, not an
/// `#[allow]`. Any other entry renders nothing: the arm in [`super::entry_body`]
/// is the only caller and it matches on this variant, so the `else` is
/// structural rather than a case that can happen.
pub(super) fn result_body(
    entry: &TranscriptEntry,
    view: EntryView<'_>,
    expanded: bool,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    let TranscriptEntry::ToolResult {
        name,
        ok,
        path,
        full,
        duration_ms,
        speculated,
        diff: diff_ref,
        ..
    } = entry
    else {
        return;
    };
    let (ok, duration_ms, speculated) = (*ok, *duration_ms, *speculated);
    let path = path.as_deref();
    let diff_ref = diff_ref.as_slice();
    let (name, full) = (name.as_str(), full.as_str());
    // One event, one metal (SPEC 6.2). The head above this row read the
    // same table, so the block is one unbroken vertical instead of a v2
    // rail over a v1 body. Failure is the one override — see
    // [`Rail::Fail`].
    let rail = if ok {
        Rail::Result(crate::v2::transcript_source::head_metal(name))
    } else {
        Rail::Fail
    };
    // Bound once: every row of this result's block reproduces the same
    // margin, and re-deriving it per row is how one of them ends up a
    // cell out of line with the others.
    let margin = rail.continuation();
    let dim = Style::new().fg(theme::MUTED);
    // A JSON body is re-laid one member to a line *before* anything
    // counts, anchors or folds it. An API response — `gh api`, an MCP
    // server, a REST tool — arrives as one line, so the fold measured a
    // 1-line result, hid nothing, offered no `ctrl+o`, and handed the
    // pane eight thousand unbroken columns to wrap. Six lines of an
    // object with a reveal affordance under them is the same content,
    // read rather than survived.
    //
    // [`stella_transcript::syntax`]'s and not this file's, because
    // `digest::fold_output` normalises the identical body for the
    // export and Observatory surfaces: a re-indenter living here would
    // be the deck and the export disagreeing about how many lines a
    // result has, which is the drift #3644 closed once already.
    let reindented = stella_transcript::syntax::reindent_json_body(full);
    let full: &str = reindented.as_deref().unwrap_or(full);
    // A result that *is* a JSON document — a `get_state` read, an MCP
    // server's answer, a REST tool's body — is read as fields
    // (`crate::v2::fields`), one per row, and never painted as JSON. The
    // reindented text above still decides the line count the fold states,
    // so the deck and the export agree about how much there is (#3644);
    // only the rows drawn differ.
    let table = crate::v2::fields::parse_document(full).map(|doc| crate::v2::fields::rows(&doc));
    let total = table
        .as_ref()
        .map_or_else(|| full.lines().count(), Vec::len);
    // ⚡ marks a speculated result: the duration overlapped the
    // model's own streaming instead of following it.
    let dur = if speculated {
        format!("⚡{}", human_duration(duration_ms))
    } else {
        human_duration(duration_ms)
    };
    // The change the body shows: the first the claim window handed out,
    // which is the call's own subject where it named one. The rest are
    // counted rather than drawn — a codemod across twenty files would
    // otherwise be the whole transcript (#4214).
    let lead = diff_ref.first();
    let inline = lead.and_then(|d| resolve_inline_diff(d, view.files));
    // The delta the emitter measured for this call, summed over *every*
    // path it claimed — not a recount of the rendered hunk, which is a
    // bounded view of the changed region and reports a smaller number,
    // and not the lead file's alone, which would disagree with the file
    // count standing beside it.
    let inline_delta = resolve_inline_delta_total(diff_ref, view.files);
    // Of paths, not of claims: two edits to one file are one file.
    let files_touched = crate::model::distinct_diff_paths(diff_ref);

    // The right-hand metric column. A diff states its own size in
    // added/removed lines, which is the honest unit for an edit —
    // "42 lines of output" would describe the tool's chatter, not the
    // change. Everything else reports output size, and only when
    // there is more than the one line already shown.
    //
    // Gated on the *measurement*, not on the diff text: a change can
    // be measured without a patch being attachable, and gating on the
    // text denied the row a size it actually knew (#4155). The two
    // resolve together in the ordinary case; where they part, the row
    // states the size and falls back to the tool's own preview below
    // rather than showing nothing at all. `unwrap_or((0, 0))` is gone
    // with it — a fabricated `+0 −0` over a real edit is the defect
    // #4156 removed from the head row, and it has no place here.
    let mut metric: Vec<Span<'static>> = Vec::new();
    // The scope of the counts beside it, stated only when there is more
    // than one — a `1 files` (or `1 file`) chip over the overwhelmingly
    // common single-path row would be a column that never varies, and
    // the N=1 row is byte-identical to what it was without it.
    if files_touched > 1 {
        metric.push(Span::styled(format!("{files_touched} files"), dim));
        metric.push(Span::styled(" · ".to_string(), dim));
    }
    if let Some((added, removed)) = inline_delta {
        metric.push(Span::styled(
            format!("+{added}"),
            Style::new().fg(theme::OK),
        ));
        metric.push(Span::styled(" ".to_string(), dim));
        metric.push(Span::styled(
            format!("−{removed}"),
            Style::new().fg(theme::BAD),
        ));
        metric.push(Span::styled(" · ".to_string(), dim));
    }
    // The size chip that used to sit here stated the same count as the
    // hint row below, one of them without the affordance. Now the count
    // is stated once, in the row that also says which key reveals it.
    metric.push(Span::styled(dur, dim));

    if expanded {
        push_row(
            rail,
            justify(vec![], metric, width, rail.indent()),
            width,
            out,
        );
        if let Some(table) = table {
            for line in table {
                push_detail_spans(&margin, line.spans, width, out);
            }
        } else {
            let paint = body_paint(path, full);
            for l in full.lines() {
                push_body_line(&margin, l, paint, width, out);
            }
        }
    } else if let Some(table) = table {
        // The field table's preview: the metric row, then the first
        // `OK_PREVIEW` fields, then the count of the rest behind `ctrl+o`.
        push_row(
            rail,
            justify(vec![], metric, width, rail.indent()),
            width,
            out,
        );
        let budget = if ok { OK_PREVIEW } else { FAIL_PREVIEW };
        let shown = table.len().min(budget);
        for line in table.into_iter().take(shown) {
            push_detail_spans(&margin, line.spans, width, out);
        }
        let hidden = total.saturating_sub(shown);
        if hidden > 0 {
            push_detail_line(
                &margin,
                &format!("⋯ {} · ctrl+o", plural_fields(hidden)),
                width,
                out,
            );
        }
    } else {
        // With a diff below, a prose summary ("Applied edit to
        // src/agent.rs") would restate both the call row above it and the
        // diff under it. The row carries only its
        // metrics and gets out of the way.
        let paint = body_paint(path, full);
        // A file tool's own prose is never a transcript body.
        //
        // `read_file` returns the file, `edit_file` returns a sentence
        // ("replaced 1 occurrence(s) in <path> at byte 1286 (file
        // sha256/8 e951e674)"). Neither belongs under a head that
        // already names the path:
        //
        // * A **read** changed nothing, and SPEC 2 collapses what did
        //   not change — the head's `N lines` is the fact, and `↵ open`
        //   is where the content lives. Five lines of a file the reader
        //   did not ask to see is the transcript spending its scarcest
        //   resource on the one event that has nothing to report.
        // * A **mutation** has exactly one interesting body, its diff.
        //   When the diff has not arrived yet — the measurement lands
        //   with the `FileChange`, which can be a beat behind the
        //   result — the honest row is quiet. Falling back to the
        //   tool's sentence restates the path, adds a byte offset and a
        //   truncated hash, and reads as though *that* were the report.
        //   It is why the same edit looks informative on one turn and
        //   useless on the next: nothing changed but the timing.
        //
        // A failure still shows its body whatever the tool: the point
        // of reading a transcript at the moment something breaks is to
        // see why, and that argument does not care which tool broke.
        let body_is_tool_prose = ok
            && matches!(
                name,
                "read_file" | "edit_file" | "write_file" | "delete_file" | "apply_edits"
            );
        let shown: Vec<&str> = if inline.is_some() || body_is_tool_prose {
            Vec::new()
        } else {
            // A failure never collapses to a single line. The point of
            // reading a transcript at the moment something breaks is to
            // see *why*, and a one-line preview of a stack trace is a
            // prompt to go hunting rather than an answer. A success now
            // gets the same window, for the reason on [`OK_PREVIEW`].
            let budget = if ok { OK_PREVIEW } else { FAIL_PREVIEW };
            // `salient_line` skips a tool's preamble to the line worth
            // reading. A *document* has no preamble — a JSON body's
            // first line is the opening delimiter, and starting
            // anywhere else shows an object with its shape cut off; a
            // numbered listing's first line is the line the caller
            // asked for by offset, and hunting inside it for the word
            // "error" would anchor a source file's preview on its own
            // error-handling code.
            //
            // Clamped so the window is never starved: anchoring on a
            // salient line near the *end* of the output would otherwise
            // leave fewer than `budget` lines to take, and the fold
            // would show one line where the export surfaces showed six
            // — the same cross-surface divergence #3644 closed, sneaking
            // back in through the offset instead of the budget. Sliding
            // the window back to fill keeps the salient line on screen
            // (it is the last thing shown rather than the first) while
            // honouring the shared preview budget.
            let skip = if paint.colored() {
                0
            } else {
                let total = full.lines().count();
                salient_line(full).min(total.saturating_sub(budget))
            };
            full.lines().skip(skip).take(budget).collect()
        };
        // A colored preview stays whole in the body column. Promoting
        // its first line to the result row would strip that line's
        // coloring (the row is one flat style) and split an object —
        // or a numbered listing's own gutter column — across two
        // different columns.
        let head: Vec<Span<'static>> = match shown.first() {
            Some(l) if !paint.colored() => vec![Span::styled(
                l.trim_end().to_owned(),
                if ok { dim } else { Style::new().fg(theme::BAD) },
            )],
            _ => Vec::new(),
        };
        push_row(
            rail,
            justify(head, metric, width, rail.indent()),
            width,
            out,
        );
        for l in shown.iter().skip(usize::from(!paint.colored())) {
            push_body_line(&margin, l.trim_end(), paint, width, out);
        }
        // The "there is more" row, for a success as well as a failure —
        // it is the only place the hidden count is stated now, and the
        // only place the ctrl+o affordance appears. Not under an inline
        // diff: there the rendered hunk is the result, and the tool's
        // own chatter is what would be counted.
        let hidden = total.saturating_sub(shown.len());
        if hidden > 0 && inline.is_none() {
            push_detail_line(
                &margin,
                &format!("⋯ {} · ctrl+o", plural_lines(hidden)),
                width,
                out,
            );
        }
    }
    // The mutation's diff, inline under the result — GitHub-PR style
    // via `crate::diff` (the one implementation of "how a diff
    // looks"), gated on freshness: a later mutation of the same path
    // bumps `FileState::changes` past the recorded seq and the diff
    // no longer belongs to this call, so it is hidden rather than
    // misattributed. Collapsed shows the lead change, at most
    // [`INLINE_DIFF_CAP`] styled lines of it; ctrl+o reveals every
    // change the call made, whole.
    if expanded {
        // ctrl+o is where the elided files live, so expanded draws every
        // one of them — an affordance that revealed only the lead file's
        // remaining lines would be advertising content it does not show.
        // Each is uncapped, exactly as the single diff has always been
        // when expanded.
        for dref in diff_ref {
            let Some(d) = resolve_inline_diff(dref, view.files) else {
                continue;
            };
            // The path *is* drawn here, and only here: with several
            // diffs stacked the reader has no other way to tell where
            // one file ends and the next begins. A single-diff block
            // still draws none — the head above it names the file, and
            // restating it is the chrome the collapsed rendering
            // deliberately does without.
            if diff_ref.len() > 1 {
                push_detail_line(&margin, &dref.path, width, out);
            }
            let (body, _) =
                diff::body_lines_inline(d, Some(&dref.path), usize::MAX, Some(" · ctrl+o"));
            for line in body {
                push_diff_line(&margin, line, width, out);
            }
        }
    } else if let (Some(dref), Some(d)) = (lead, inline) {
        // No path header and no counts footer here, unlike the
        // standalone viewer: the call row above already names the file
        // and the metric column already states `+n −m`, so both rules
        // would be the same facts a second time — four rows of chrome
        // around what is often a two-row change.
        //
        // The fold row is the renderer's, not this call site's: a
        // head-and-tail rendering elides the *middle*, so the marker
        // has to sit where the missing lines were. Appending it after
        // the body — which is what happened here until the shared
        // policy landed — would put "and there is more" under a
        // rendering whose last row is already the file's last row.
        let (body, _) =
            diff::body_lines_inline(d, Some(&dref.path), INLINE_DIFF_CAP, Some(" · ctrl+o"));
        for line in body {
            push_diff_line(&margin, line, width, out);
        }
        // The same argument, one level out: what is elided here is the
        // *other files*, and they come after the one shown, so the
        // marker sits at the end of the diff block rather than being
        // appended to the result's own body above it.
        //
        // Only under a rendered diff. With nothing drawn there is no
        // elision to mark — the row has already degraded to stating
        // `n files · +a −b` and nothing more, which is the same
        // degradation a single unresolvable reference has always had.
        if files_touched > 1 {
            push_detail_line(
                &margin,
                &format!("⋯ {} more files · ctrl+o", files_touched - 1),
                width,
                out,
            );
        }
    }
}

/// `1 more field` / `3 more fields` — the hidden count under a folded table.
fn plural_fields(n: usize) -> String {
    if n == 1 {
        "1 more field".to_string()
    } else {
        format!("{n} more fields")
    }
}
