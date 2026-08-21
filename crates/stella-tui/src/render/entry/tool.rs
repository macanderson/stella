// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One tool call, head to diff — the transcript's densest block.
//!
//! Split out of [`super`] rather than grown there, following `recall`: this is
//! the one entry pair that composes *four* rendering decisions on top of each
//! other — a syntax-highlighted body in the file's own language (#4019, #4036),
//! word-level inline diffs, the emitter's line-number gutter rendered as it
//! arrived (#4020), and a truncation notice that has to agree with the export
//! and Observatory fold (#3644). Every one of them is a thing a redesign can
//! silently drop, and #4123 left the result on the v1 renderer precisely to
//! avoid dropping them; this module is where they come back together under
//! SPEC 6.
//!
//! ## One event, two entries, one rail
//!
//! SPEC 6.2 makes the rail a property of the event. A call and its result are
//! one event — the deck records them as
//! [`ToolStart`](crate::model::TranscriptEntry::ToolStart) and
//! [`ToolResult`](crate::model::TranscriptEntry::ToolResult) only because the
//! head must draw before the result exists, or a two-minute `cargo test` would
//! show nothing at all while it ran. So both entries take the same metal, read
//! once from [`v2::metal_for`], and a failure overrides it: a call that failed
//! has stopped being the kind of thing its verb claims.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::model::FileState;
use crate::render::row::*;
use crate::v2::transcript_source as v2;
use crate::{diff, syntax, theme};

use super::super::{INLINE_DIFF_CAP, resolve_inline_delta, resolve_inline_diff};

/// How a tool-result body is colored, and the gutter parser that goes with it.
///
/// Both are [`stella_transcript::syntax`]'s now rather than this crate's. They
/// were written in `render` for #4019 and moved down in #4036 for the reason the
/// JSON predicate moved down in #3644: the export and Observatory renderers ask
/// the identical question of the identical bodies, and a rendering decision held
/// in three copies is a rendering decision that drifts. The deck keeps the
/// *palette* ([`syntax::tok_style`]) and nothing else.
use stella_transcript::syntax::{BodyPaint, body_paint, paint_line};

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
pub(super) fn push_body_line(
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
    // [`crate::syntax::lex_count`].
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

/// The dispatched call: SPEC 6.2's head, plus its argument object when the row
/// is expanded.
///
/// The head itself is [`v2::head_rows`]'. What is *not* v2's is the expanded
/// body under it, and it had stopped rendering at all: #4123 routed `ToolStart`
/// through the v2 router, which returns before the v1 arm that drew the
/// pretty-printed arguments, and `head_rows` took no `expanded` flag — so
/// `ctrl+o` on a call row silently showed nothing from that PR until this one.
///
/// `measured` is the emitter's `(added, removed)` for this call once the paired
/// result has landed and the turn boundary has measured the tree, and `None`
/// until then — the router resolves it (`v2::measured_delta`), so this function
/// and the projection under it stay pure functions of one call (#4154).
pub(super) fn start_rows(
    name: &str,
    input: &str,
    raw: &str,
    path: Option<&str>,
    measured: Option<(u32, u32)>,
    expanded: bool,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    out.extend(v2::head_rows(name, path, input, measured, width));
    if !expanded {
        return;
    }
    // Bound once: every row of this call's argument object reproduces the same
    // margin, and re-deriving it per row is how one of them ends up a cell out
    // of line with the others.
    let margin = Rail::Call(v2::metal_for(name)).continuation();
    // ctrl+o: the full argument object, pretty-printed and dim. An over-budget
    // argument may not parse (char-capped raw) — show it wrapped rather than
    // clipped at the pane edge. Pretty-printing is what makes the coloring
    // worth having: a compact one-line object has no shape for a key hue to
    // mark. A body that failed to re-parse is still lexed — it is capped JSON,
    // not another format.
    let pretty = serde_json::from_str::<serde_json::Value>(raw)
        .and_then(|v| serde_json::to_string_pretty(&v))
        .unwrap_or_else(|_| raw.to_owned());
    for l in pretty.lines() {
        push_body_line(&margin, l, BodyPaint::json(), width, out);
    }
}

/// The returned call: the metric row, a bounded preview of its output, the
/// truncation notice, and the mutation's inline diff.
#[allow(clippy::too_many_arguments)] // the entry's own fields, one to one; a struct here would just be a second shape to keep in step
pub(super) fn result_rows(
    name: &str,
    ok: bool,
    path: Option<&str>,
    full: &str,
    duration_ms: u64,
    speculated: bool,
    diff_ref: Option<&crate::model::InlineDiffRef>,
    files: &[FileState],
    expanded: bool,
    width: usize,
    out: &mut Vec<Line<'static>>,
) {
    // One event, one metal (SPEC 6.2) — the head above this row read it from
    // the same table. Failure is the one override: a `bash` that failed is not
    // the gold "stella acting" row its verb promised, and red is the whole
    // reason a reader's scan stops there.
    let rail = if ok {
        Rail::Result(v2::metal_for(name))
    } else {
        Rail::Fail
    };
    // Bound once: every row of this result's block reproduces the same margin,
    // and re-deriving it per row is how one of them ends up a cell out of line
    // with the others.
    let margin = rail.continuation();
    let dim = Style::new().fg(theme::MUTED);
    // A JSON body is re-laid one member to a line *before* anything counts,
    // anchors or folds it. An API response — `gh api`, an MCP server, a REST
    // tool — arrives as one line, so the fold measured a 1-line result, hid
    // nothing, offered no `ctrl+o`, and handed the pane eight thousand unbroken
    // columns to wrap. Six lines of an object with a reveal affordance under
    // them is the same content, read rather than survived.
    //
    // [`stella_transcript::syntax`]'s and not this file's, because
    // `digest::fold_output` normalises the identical body for the export and
    // Observatory surfaces: a re-indenter living here would be the deck and the
    // export disagreeing about how many lines a result has, which is the drift
    // #3644 closed once already.
    let reindented = stella_transcript::syntax::reindent_json_body(full);
    let full: &str = reindented.as_deref().unwrap_or(full);
    let total = full.lines().count();
    // ⚡ marks a speculated result: the duration overlapped the model's own
    // streaming instead of following it.
    let dur = if speculated {
        format!("⚡{}", human_duration(duration_ms))
    } else {
        human_duration(duration_ms)
    };
    let inline = diff_ref.and_then(|d| resolve_inline_diff(d, files));
    // The delta the emitter measured for this very mutation, carried alongside
    // its diff — not a recount of the rendered hunk, which is a bounded view of
    // the changed region and reports a smaller number.
    let inline_delta = diff_ref.and_then(|d| resolve_inline_delta(d, files));

    // The right-hand metric column. A diff states its own size in
    // added/removed lines, which is the honest unit for an edit — "42 lines of
    // output" would describe the tool's chatter, not the change. Everything
    // else reports output size, and only when there is more than the one line
    // already shown.
    //
    // Gated on the *measurement*, not on the diff text: a change can be measured
    // without a patch being attachable, and gating on the text denied the row a
    // size it actually knew (#4155). The two resolve together in the ordinary
    // case; where they part, the row states the size and falls back to the
    // tool's own preview below rather than showing nothing at all.
    // `unwrap_or((0, 0))` is gone with it — a fabricated `+0 −0` over a real
    // edit is the defect #4156 removed from the head row, and it has no place
    // here.
    let mut metric: Vec<Span<'static>> = Vec::new();
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
    // The size chip that used to sit here stated the same count as the hint row
    // below, one of them without the affordance. Now the count is stated once,
    // in the row that also says which key reveals it.
    metric.push(Span::styled(dur, dim));

    if expanded {
        push_row(
            rail,
            justify(vec![], metric, width, rail.indent()),
            width,
            out,
        );
        let paint = body_paint(path, full);
        for l in full.lines() {
            push_body_line(&margin, l, paint, width, out);
        }
    } else {
        // With a diff below, a prose summary ("Applied edit to src/agent.rs")
        // would restate the call row above it and the diff under it in the same
        // breath. The row carries only its metrics and gets out of the way.
        let paint = body_paint(path, full);
        let shown: Vec<&str> = if inline.is_some() {
            Vec::new()
        } else {
            // A failure never collapses to a single line. The point of reading a
            // transcript at the moment something breaks is to see *why*, and a
            // one-line preview of a stack trace is a prompt to go hunting rather
            // than an answer. A success now gets the same window, for the reason
            // on [`OK_PREVIEW`].
            let budget = if ok { OK_PREVIEW } else { FAIL_PREVIEW };
            // `salient_line` skips a tool's preamble to the line worth reading.
            // A *document* has no preamble — a JSON body's first line is the
            // opening delimiter, and starting anywhere else shows an object with
            // its shape cut off; a numbered listing's first line is the line the
            // caller asked for by offset, and hunting inside it for the word
            // "error" would anchor a source file's preview on its own
            // error-handling code.
            //
            // Clamped so the window is never starved: anchoring on a salient
            // line near the *end* of the output would otherwise leave fewer than
            // `budget` lines to take, and the fold would show one line where the
            // export surfaces showed six — the same cross-surface divergence
            // #3644 closed, sneaking back in through the offset instead of the
            // budget. Sliding the window back to fill keeps the salient line on
            // screen (it is the last thing shown rather than the first) while
            // honouring the shared preview budget.
            let skip = if paint.colored() {
                0
            } else {
                let total = full.lines().count();
                salient_line(full).min(total.saturating_sub(budget))
            };
            full.lines().skip(skip).take(budget).collect()
        };
        // A colored preview stays whole in the body column. Promoting its first
        // line to the result row would strip that line's coloring (the row is
        // one flat style) and split an object — or a numbered listing's own
        // gutter column — across two different columns.
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
        // The "there is more" row, for a success as well as a failure — it is
        // the only place the hidden count is stated now, and the only place the
        // ctrl+o affordance appears. Not under an inline diff: there the
        // rendered hunk is the result, and the tool's own chatter is what would
        // be counted.
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
    // The mutation's diff, inline under the result — GitHub-PR style via
    // `crate::diff` (the one implementation of "how a diff looks"), gated on
    // freshness: a later mutation of the same path bumps `FileState::changes`
    // past the recorded seq and the diff no longer belongs to this call, so it
    // is hidden rather than misattributed. Collapsed shows at most
    // [`INLINE_DIFF_CAP`] styled lines; ctrl+o reveals the whole diff.
    if let (Some(dref), Some(d)) = (diff_ref, inline) {
        // No path header and no counts footer here, unlike the standalone
        // viewer: the call row above already names the file and the metric
        // column already states `+n −m`, so both rules would be the same facts a
        // second time — four rows of chrome around what is often a two-row
        // change.
        let cap = if expanded {
            usize::MAX
        } else {
            INLINE_DIFF_CAP
        };
        // The fold row is the renderer's, not this call site's: a head-and-tail
        // rendering elides the *middle*, so the marker has to sit where the
        // missing lines were. Appending it after the body — which is what
        // happened here until the shared policy landed — would put "and there is
        // more" under a rendering whose last row is already the file's last row.
        let (body, _) = diff::body_lines_inline(d, Some(&dref.path), cap, Some(" · ctrl+o"));
        for line in body {
            push_diff_line(&margin, line, width, out);
        }
    }
}
