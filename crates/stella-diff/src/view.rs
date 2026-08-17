// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! How much of a diff a surface shows, and which part.
//!
//! A diff is already only the changed lines plus their context — that is what
//! [`crate::unified_diff`] and `git diff -p` both produce, and no surface may
//! fall back to printing a whole file. But "only the changed lines" is not by
//! itself a bound: a `write_file` that creates a two-thousand-line file is two
//! thousand changed lines, and a transcript row that prints all of them has
//! stopped being a transcript.
//!
//! So every surface caps the rendering, and the cap has to be *the same
//! answer everywhere* — the deck, the plain `stella run` scrollback, the
//! Observatory and an exported dashboard are four views of one run, and a
//! reader who sees three different amounts of one edit cannot tell which is
//! the edit. This module is that one answer: a pure plan over line indices,
//! with no knowledge of ratatui, HTML, or ANSI. Each renderer decides how a
//! shown line looks; none of them decides *which* lines those are.
//!
//! # Beginning and end, never just the beginning
//!
//! The policy that shipped before this module took whole hunks from the front
//! until the budget ran out. That is honest about what it shows and silent
//! about the shape of what it hides: a long edit rendered as its first twenty
//! lines reads as an edit that starts here and trails off, when the reader's
//! actual question — did this rewrite land where I think it did — is usually
//! answered at the *other* end. [`plan`] therefore fills from both ends and
//! elides the middle, which is the shape a reviewer already knows from a
//! collapsed hunk on a pull request.
//!
//! Two levels, because a diff has two natural units:
//!
//! - **Whole hunks**, taken alternately from the front and the back while
//!   they fit. A hunk is the smallest unit that is honest on its own: a cut
//!   inside one lands routinely between a `-` line and the `+` line that
//!   replaces it, leaving a change that reads as a pure deletion.
//! - **A line window**, when no whole hunk fits at all — one enormous hunk,
//!   which is exactly the created-file shape above. Half the budget from the
//!   top, the rest from the bottom.
//!
//! Either way there is **at most one elision**, so a caller has exactly one
//! place to draw the marker and exactly one number to put in it. That is a
//! guaranteed property of [`Plan`], not a coincidence of the inputs — see
//! [`Plan::fold_before`].

use core::ops::Range;

use crate::{DiffLine, Hunk, Op};

/// Lines an *inline* diff shows before eliding — a diff drawn under a
/// transcript row, where it shares the viewport with everything else the turn
/// did.
///
/// Twenty is a judgement, not a measurement: it is about a third of a
/// standard terminal, enough for a typical multi-hunk edit to arrive whole,
/// and small enough that a file rewrite cannot push the rest of the turn off
/// the screen. Surfaces that can reveal the remainder (the deck's `ctrl+o`)
/// still start here.
pub const INLINE_CAP: usize = 20;

/// Lines a diff *viewer* shows before eliding — a panel whose whole job is
/// the diff, and which scrolls.
///
/// Ten times [`INLINE_CAP`], on the same judgement inverted: the reader came
/// here for the diff, so the cap exists only to keep one generated artifact
/// from turning a dashboard poll into a megabyte of DOM.
pub const VIEW_CAP: usize = 200;

/// Which lines of a rendered diff survive a line budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Half-open line ranges to show, ascending and non-overlapping. One
    /// range when everything shown is contiguous, two when a head and a tail
    /// straddle an elided middle — never more, so there is never more than
    /// one marker to draw.
    pub shown: Vec<Range<usize>>,
    /// Lines the ranges skip. `0` means the whole diff is shown.
    pub hidden: usize,
    /// Lines the diff has in total, so a caller can ask about any index
    /// without carrying the length separately.
    pub total: usize,
}

impl Plan {
    /// Whether line `i` is shown.
    #[must_use]
    pub fn shows(&self, i: usize) -> bool {
        self.shown.iter().any(|r| r.contains(&i))
    }

    /// The line index the elision marker sits in front of, or `None` when
    /// nothing was elided.
    ///
    /// Always the **first hidden line**. For a head-and-tail plan that is the
    /// start of the elided middle; for a head-only plan it is one past the
    /// last shown line, which is where a caller walking `0..total` naturally
    /// arrives; and for a plan that kept only a *tail* it is `0`, because the
    /// elision precedes everything shown. Because [`Plan::shown`] holds at
    /// most two ranges there is at most one such index — a caller may draw one
    /// marker and stop looking.
    #[must_use]
    pub fn fold_before(&self) -> Option<usize> {
        if self.hidden == 0 {
            return None;
        }
        match self.shown.first() {
            // Nothing shown at all, or a leading run hidden: the marker goes
            // in front. Reading `r.end` here — which is what this did until a
            // cross-check against the arenabench port disagreed — puts the
            // marker AFTER the only thing on screen and reads as "and there
            // was more below" when every hidden line was above.
            None => Some(0),
            Some(r) if r.start > 0 => Some(0),
            Some(r) => Some(r.end),
        }
    }
}

/// Plan a `cap`-line view over a diff of `total` lines whose hunks begin at
/// `hunk_starts`.
///
/// `hunk_starts` is advisory: out-of-order, out-of-range and duplicate entries
/// are ignored, and an empty slice means "one unbroken hunk" — the shape a
/// headerless pseudo-diff has. A leading entry other than `0` still leaves the
/// file metadata above the first `@@` attached to it, because that metadata is
/// what names the file the hunk is in.
///
/// See the module docs for the policy. Note that `cap` bounds the *rendered
/// lines*, not the changed ones: context lines and `@@` headers are lines a
/// reader has to look at, so they are lines the budget pays for.
#[must_use]
pub fn plan(hunk_starts: &[usize], total: usize, cap: usize) -> Plan {
    if total <= cap {
        // Pushed rather than written as `vec![0..total]`: clippy reads a
        // one-range vec literal as a mistyped `(0..total).collect()`, and the
        // single range is exactly what is meant here.
        let mut shown = Vec::with_capacity(1);
        if total > 0 {
            shown.push(0..total);
        }
        return Plan {
            shown,
            hidden: 0,
            total,
        };
    }
    if cap == 0 {
        return Plan {
            shown: Vec::new(),
            hidden: total,
            total,
        };
    }

    // Hunk boundaries as a strictly ascending fence `[0, …, total]`, so hunk
    // `k` is `bounds[k]..bounds[k + 1]`.
    let mut bounds = vec![0usize];
    for &start in hunk_starts {
        // `>` and not `>=`: a duplicate or backwards entry is dropped rather
        // than producing an empty hunk the loop below would spin on.
        if start > *bounds.last().unwrap_or(&0) && start < total {
            bounds.push(start);
        }
    }
    bounds.push(total);
    let hunks = bounds.len() - 1;

    // Fill from both ends, front first, until neither end's next whole hunk
    // fits. Front-biased on purpose: with an odd budget the beginning is the
    // half a reader orients from.
    let (mut head, mut tail, mut used) = (0usize, 0usize, 0usize);
    loop {
        let mut progressed = false;
        if head + tail < hunks {
            let len = bounds[head + 1] - bounds[head];
            if used + len <= cap {
                used += len;
                head += 1;
                progressed = true;
            }
        }
        if head + tail < hunks {
            let k = hunks - tail - 1;
            let len = bounds[k + 1] - bounds[k];
            if used + len <= cap {
                used += len;
                tail += 1;
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }

    if head > 0 || tail > 0 {
        // `head + tail < hunks` here: taking every hunk would mean
        // `used == total`, and `total > cap >= used` rules that out. So the
        // two ranges below never touch, and the elision between them is real.
        let mut shown = Vec::with_capacity(2);
        if head > 0 {
            shown.push(0..bounds[head]);
        }
        if tail > 0 {
            shown.push(bounds[hunks - tail]..total);
        }
        return Plan {
            shown,
            hidden: total - used,
            total,
        };
    }

    // No whole hunk fits — one hunk dominates the diff, the shape a created
    // or wholesale-rewritten file has. Split the budget across its two ends.
    let head_lines = cap.div_ceil(2);
    let tail_lines = cap - head_lines;
    let mut shown = Vec::with_capacity(2);
    shown.push(0..head_lines);
    if tail_lines > 0 {
        shown.push(total - tail_lines..total);
    }
    Plan {
        shown,
        hidden: total - cap,
        total,
    }
}

/// Apply [`plan`] to a diff's *structured* hunks, returning the hunks a
/// `cap`-line view shows and how many content lines it hid.
///
/// This is the same policy [`plan`] states, for the surfaces that render from
/// [`Hunk`] values rather than from diff text — the Observatory and the
/// exported dashboard. Both draw a `@@` row per hunk, so a hunk costs its
/// header plus its lines, and the budget pays for both.
///
/// **An elision is expressed as hunks, not as a hole in one.** When the cut
/// falls inside a hunk, the surviving head and tail come back as two hunks
/// with their own recomputed `@@` headers, each describing exactly the lines
/// it carries. That keeps the result a *valid diff* — a renderer walking line
/// numbers from each header cannot be misled by a gap it does not know about,
/// which is the bug a "same hunk, fewer lines" shape would have handed every
/// consumer. The caller draws its `⋯ n lines` marker between the pieces; the
/// returned count is what to put in it.
///
/// One consequence worth naming: splitting a hunk replaces one header row
/// with two, so the rendering can exceed `cap` by one row per split. The
/// alternative — dropping a header — would be a diff that does not say where
/// it is.
#[must_use]
pub fn elide(hunks: &[Hunk], cap: usize) -> Elided {
    let mut starts = Vec::with_capacity(hunks.len());
    let mut total = 0usize;
    for hunk in hunks {
        starts.push(total);
        total += 1 + hunk.lines.len(); // the `@@` row is a row a reader reads
    }
    let plan = plan(&starts, total, cap);
    if plan.hidden == 0 {
        return Elided {
            hunks: hunks.to_vec(),
            hidden: 0,
            fold_before: None,
        };
    }

    let content: usize = hunks.iter().map(|h| h.lines.len()).sum();
    let mut out: Vec<Hunk> = Vec::new();
    let mut shown_content = 0usize;
    let mut fold_before: Option<usize> = None;
    for (k, hunk) in hunks.iter().enumerate() {
        let base = starts[k];
        // Walk this hunk's own line numbers so a split piece can be given a
        // header describing where it really starts.
        let (mut old_no, mut new_no) = (hunk.old_start, hunk.new_start);
        let mut piece: Option<Hunk> = None;
        for (i, line) in hunk.lines.iter().enumerate() {
            let (at_old, at_new) = (old_no, new_no);
            match line.op {
                Op::Equal => {
                    old_no += 1;
                    new_no += 1;
                }
                Op::Add => new_no += 1,
                Op::Remove => old_no += 1,
            }
            if !plan.shows(base + 1 + i) {
                // The run ended: close the piece so the next shown line opens
                // a new hunk with a header of its own.
                if let Some(done) = piece.take() {
                    out.push(reheader(done));
                }
                // The plan hides one contiguous run, so this fires once — at
                // the boundary the caller draws its marker on. Deliberately
                // NOT gated on having shown something first: a view that kept
                // only a tail hides its leading run, and the marker belongs in
                // front of the surviving hunk (`out.len()` is 0 there), not
                // after it.
                if fold_before.is_none() {
                    fold_before = Some(out.len());
                }
                continue;
            }
            shown_content += 1;
            let open = piece.get_or_insert_with(|| Hunk {
                old_start: at_old,
                old_count: 0,
                new_start: at_new,
                new_count: 0,
                lines: Vec::new(),
            });
            if line.op != Op::Add {
                open.old_count += 1;
            }
            if line.op != Op::Remove {
                open.new_count += 1;
            }
            open.lines.push(DiffLine {
                op: line.op,
                text: line.text.clone(),
            });
        }
        if let Some(done) = piece.take() {
            out.push(reheader(done));
        }
    }
    // A trailing elision — and the degenerate everything-hidden one — folds
    // after the last surviving hunk rather than in front of one.
    let fold_before = fold_before.or(Some(out.len()));
    Elided {
        hunks: out,
        hidden: content - shown_content,
        fold_before,
    }
}

/// What a `cap`-line view of a structured diff shows — see [`elide`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Elided {
    /// The hunks to render, each with a header describing exactly the lines
    /// it carries.
    pub hunks: Vec<Hunk>,
    /// Content lines the view omitted. `0` means nothing was elided.
    pub hidden: usize,
    /// Index into [`Elided::hunks`] the `⋯ n lines` marker belongs in front
    /// of, or `hunks.len()` when the omitted lines are the tail. `None` when
    /// nothing was elided.
    pub fold_before: Option<usize>,
}

/// Git renders a zero-length side as the line *before* the insertion point, so
/// a piece that carries no line on one side must not claim one there.
fn reheader(mut hunk: Hunk) -> Hunk {
    if hunk.old_count == 0 {
        hunk.old_start = hunk.old_start.saturating_sub(1);
    }
    if hunk.new_count == 0 {
        hunk.new_start = hunk.new_start.saturating_sub(1);
    }
    hunk
}

/// The line indices at which each hunk of a rendered unified diff begins.
///
/// A hunk begins at its `@@` header. Anything before the first one is file
/// metadata (`diff --git`, `index`, `---`/`+++`) that belongs with the hunk it
/// introduces, so the first boundary is pulled back to `0` — which is also
/// what makes a headerless pseudo-diff one unbroken hunk rather than none.
///
/// Recognising `@@` by prefix is safe here in a way it is not inside a hunk
/// body: a source line starting with `@@` arrives with a `+`/`-`/space marker
/// in front of it, so it cannot be mistaken for a header.
#[must_use]
pub fn hunk_starts(lines: &[&str]) -> Vec<usize> {
    let mut starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.starts_with("@@"))
        .map(|(i, _)| i)
        .collect();
    match starts.first() {
        // Leading metadata: the first `@@` stops being a boundary, so the
        // lines above it join the hunk they introduce instead of forming a
        // hunk of their own — which the budget would otherwise spend two
        // lines on before showing a single line of code.
        Some(&first) if first > 0 => starts[0] = 0,
        Some(_) => {}
        None => starts.push(0),
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line index the plan shows, in order — what a renderer would
    /// actually draw.
    fn drawn(plan: &Plan) -> Vec<usize> {
        (0..plan.total).filter(|i| plan.shows(*i)).collect()
    }

    /// The shown ranges as `(start, end)` pairs. Written this way because a
    /// `vec![0..8]` expectation trips clippy's single-range-in-vec lint, and
    /// pairs read at least as clearly in an assertion.
    fn spans(plan: &Plan) -> Vec<(usize, usize)> {
        plan.shown.iter().map(|r| (r.start, r.end)).collect()
    }

    #[test]
    fn a_diff_inside_the_budget_is_shown_whole() {
        let plan = plan(&[0, 4], 8, 20);
        assert_eq!(spans(&plan), vec![(0, 8)]);
        assert_eq!(plan.hidden, 0);
        assert_eq!(plan.fold_before(), None, "nothing elided, nothing to mark");
    }

    #[test]
    fn an_empty_diff_shows_no_range_at_all() {
        // Not one empty range: a caller mapping `shown` to markers would draw
        // a fold rule around nothing.
        let plan = plan(&[], 0, 20);
        assert!(plan.shown.is_empty());
        assert_eq!(plan.hidden, 0);
    }

    #[test]
    fn an_over_budget_diff_shows_its_beginning_and_its_end() {
        // The regression this module exists for: five three-line hunks under
        // a ten-line budget used to render as the first three hunks and
        // nothing else, so the end of the change was invisible and unmarked.
        let starts = [0, 3, 6, 9, 12];
        let plan = plan(&starts, 15, 10);
        assert_eq!(
            spans(&plan),
            vec![(0, 6), (12, 15)],
            "two hunks from the front, one from the back"
        );
        assert_eq!(plan.hidden, 6);
        assert_eq!(plan.fold_before(), Some(6), "one marker, at the gap");
        assert_eq!(drawn(&plan), vec![0, 1, 2, 3, 4, 5, 12, 13, 14]);
    }

    #[test]
    fn whole_hunks_are_kept_rather_than_sliced_mid_change() {
        // Two three-line hunks under a four-line budget. Slicing would show
        // the second hunk's `-` line without the `+` that replaces it — a
        // change that reads as a pure deletion. One whole hunk is the honest
        // answer, and the other is reported as hidden.
        let plan = plan(&[0, 3], 6, 4);
        assert_eq!(spans(&plan), vec![(0, 3)]);
        assert_eq!(plan.hidden, 3);
        assert_eq!(
            plan.fold_before(),
            Some(3),
            "a trailing elision is still one"
        );
    }

    #[test]
    fn one_enormous_hunk_falls_back_to_a_window_at_each_end() {
        // The created-file shape: `write_file` on a long file is one hunk of
        // nothing but `+` lines, and no whole-hunk plan can fit it. Half the
        // budget from the top, the rest from the bottom — never the top alone.
        let plan = plan(&[0], 500, 20);
        assert_eq!(spans(&plan), vec![(0, 10), (490, 500)]);
        assert_eq!(plan.hidden, 480);
        assert_eq!(plan.fold_before(), Some(10));
    }

    #[test]
    fn a_headerless_pseudo_diff_is_one_hunk_and_still_gets_both_ends() {
        // The event path emits bare `+`/`-` lines with no `@@` at all.
        let plan = plan(&[], 100, 9);
        assert_eq!(
            spans(&plan),
            vec![(0, 5), (96, 100)],
            "an odd budget gives the extra line to the head"
        );
        assert_eq!(plan.hidden, 91);
    }

    #[test]
    fn a_single_line_budget_shows_the_beginning_and_says_so() {
        let plan = plan(&[], 40, 1);
        assert_eq!(spans(&plan), vec![(0, 1)], "no room for a tail");
        assert_eq!(plan.hidden, 39);
    }

    #[test]
    fn a_zero_budget_shows_nothing_and_hides_everything() {
        // A caller that reserves rows for its own chrome can arrive here, and
        // must get "all of it is hidden" rather than a panic or a stray line.
        let plan = plan(&[0, 3], 6, 0);
        assert!(plan.shown.is_empty());
        assert_eq!(plan.hidden, 6);
        assert_eq!(plan.fold_before(), Some(0));
    }

    #[test]
    fn a_view_that_keeps_only_a_tail_folds_in_front_of_it_not_behind_it() {
        // Found by diffing this policy against its arenabench TypeScript port
        // over a matrix of budgets, which is the only reason it was found at
        // all: every hand-written case happened to keep a head.
        //
        // Hunks of 7 lines with a 2-line last one, under a budget that fits
        // only that last one. Every hidden line is ABOVE what survives, so a
        // marker drawn after it says "and there was more below" about a
        // rendering whose last row is the file's last row — the exact defect
        // the head-and-tail policy exists to remove.
        let starts: Vec<usize> = (0..100).step_by(7).collect();
        let plan = plan(&starts, 100, 4);
        assert_eq!(spans(&plan), vec![(98, 100)], "only the last hunk fits");
        assert_eq!(plan.hidden, 98);
        assert_eq!(plan.fold_before(), Some(0), "in front of the tail");
    }

    #[test]
    fn elide_folds_in_front_when_it_keeps_only_a_tail_hunk() {
        // The structured half of the case above.
        let mut hunks: Vec<Hunk> = vec![adds(1, 40)];
        hunks.push(adds(500, 1));
        let view = elide(&hunks, 3);
        assert_eq!(view.hunks.len(), 1, "only the small trailing hunk fits");
        assert_eq!(view.hunks[0].new_start, 500);
        assert_eq!(
            view.fold_before,
            Some(0),
            "the marker precedes the one surviving hunk: {view:?}"
        );
    }

    #[test]
    fn the_shown_ranges_never_exceed_two_so_there_is_only_ever_one_marker() {
        // The property `fold_before` relies on, over every budget against a
        // many-hunk diff — including the budgets where the alternation stops
        // on the front pass and the ones where it stops on the back.
        let starts: Vec<usize> = (0..20).map(|k| k * 4).collect();
        for cap in 0..=100 {
            let plan = plan(&starts, 80, cap);
            assert!(
                plan.shown.len() <= 2,
                "cap {cap} produced {} ranges: {:?}",
                plan.shown.len(),
                plan.shown
            );
            let shown_lines: usize = plan.shown.iter().map(|r| r.len()).sum();
            assert_eq!(
                shown_lines + plan.hidden,
                80,
                "cap {cap}: shown + hidden must account for every line"
            );
            assert!(
                shown_lines <= cap.min(80),
                "cap {cap} drew {shown_lines} lines"
            );
        }
    }

    #[test]
    fn malformed_hunk_starts_are_ignored_rather_than_trusted() {
        // Out of order, out of range, duplicated — a caller deriving these
        // from a malformed diff must get a plan, never a hang or a panic.
        let plan = plan(&[9, 3, 3, 400, 0], 12, 6);
        assert!(plan.shown.len() <= 2, "{:?}", plan.shown);
        let shown: usize = plan.shown.iter().map(|r| r.len()).sum();
        assert_eq!(shown + plan.hidden, 12);
    }

    /// A hunk of `n` single-line additions starting at new line `at`.
    fn adds(at: usize, n: usize) -> Hunk {
        Hunk {
            old_start: at.saturating_sub(1),
            old_count: 0,
            new_start: at,
            new_count: n,
            lines: (0..n)
                .map(|i| DiffLine {
                    op: Op::Add,
                    text: format!("line{}", at + i),
                })
                .collect(),
        }
    }

    #[test]
    fn elide_leaves_a_diff_inside_the_budget_completely_alone() {
        let hunks = vec![adds(1, 3), adds(40, 2)];
        let view = elide(&hunks, 20);
        assert_eq!(view.hunks, hunks);
        assert_eq!(view.hidden, 0);
        assert_eq!(view.fold_before, None, "no elision, no marker");
    }

    #[test]
    fn elide_splits_an_over_budget_hunk_into_two_correctly_headed_ones() {
        // The created-file shape. What comes back must be a *valid* diff: a
        // renderer walking line numbers from each `@@` header has to land on
        // the file's real lines on both sides of the gap, which only holds if
        // the tail is its own hunk with its own header.
        let view = elide(&[adds(1, 100)], 10);
        let out = &view.hunks;
        assert_eq!(out.len(), 2, "a head hunk and a tail hunk: {out:?}");
        assert_eq!(view.hidden, 91, "100 lines less the 9 the two pieces carry");
        assert_eq!(
            view.fold_before,
            Some(1),
            "the marker sits between the two pieces: {out:?}"
        );

        let (head, tail) = (&out[0], &out[1]);
        assert_eq!(head.new_start, 1);
        assert_eq!(head.lines.first().map(|l| l.text.as_str()), Some("line1"));
        assert_eq!(
            tail.new_start,
            1 + 100 - tail.new_count,
            "the tail's header names the real line it starts on, not line 1"
        );
        assert_eq!(
            tail.lines.last().map(|l| l.text.as_str()),
            Some("line100"),
            "the change's last line survives — the point of a tail"
        );
        for piece in out {
            assert_eq!(
                piece.new_count,
                piece.lines.len(),
                "each header counts exactly the lines it carries: {piece:?}"
            );
            assert_eq!(piece.old_count, 0, "pure additions: {piece:?}");
        }
    }

    #[test]
    fn elide_drops_whole_hunks_from_the_middle_and_keeps_both_ends() {
        let hunks: Vec<Hunk> = (0..6).map(|k| adds(1 + k * 100, 3)).collect();
        let view = elide(&hunks, 8);
        let out = &view.hunks;
        assert!(!out.is_empty());
        assert_eq!(
            out.first().map(|h| h.new_start),
            Some(1),
            "the first hunk survives: {out:?}"
        );
        assert_eq!(
            out.last().map(|h| h.new_start),
            Some(501),
            "and so does the last: {out:?}"
        );
        let shown: usize = out.iter().map(|h| h.lines.len()).sum();
        assert_eq!(
            shown + view.hidden,
            18,
            "every line is shown or counted hidden"
        );
        assert!(
            view.fold_before.is_some_and(|i| i > 0 && i < out.len()),
            "the marker falls between the ends, not outside them: {view:?}"
        );
    }

    #[test]
    fn elide_preserves_the_removal_side_of_a_replacement_it_keeps() {
        let hunk = Hunk {
            old_start: 10,
            old_count: 2,
            new_start: 10,
            new_count: 2,
            lines: vec![
                DiffLine {
                    op: Op::Equal,
                    text: "ctx".into(),
                },
                DiffLine {
                    op: Op::Remove,
                    text: "old".into(),
                },
                DiffLine {
                    op: Op::Add,
                    text: "new".into(),
                },
                DiffLine {
                    op: Op::Equal,
                    text: "tail".into(),
                },
            ],
        };
        let view = elide(std::slice::from_ref(&hunk), 100);
        assert_eq!(view.hunks, vec![hunk], "nothing to elide, nothing changed");
        assert_eq!(view.hidden, 0);
    }

    #[test]
    fn hunk_starts_folds_leading_metadata_into_the_first_hunk() {
        let lines = ["--- a/x.rs", "+++ b/x.rs", "@@ -1,2 +1,2 @@", "-a", "+A"];
        assert_eq!(
            hunk_starts(&lines),
            vec![0],
            "the `---`/`+++` pair names the file the first hunk is in, so it \
             joins that hunk rather than becoming a two-line hunk of its own"
        );
        // …and a second hunk is still a boundary, so the fold can land there.
        let two = ["--- a/x.rs", "@@ -1 +1 @@", "-a", "@@ -9 +9 @@", "-b"];
        assert_eq!(hunk_starts(&two), vec![0, 3]);
    }

    #[test]
    fn hunk_starts_finds_every_boundary_and_treats_no_header_as_one_hunk() {
        let lines = ["@@ -1 +1 @@", "-a", "+A", "@@ -9 +9 @@", "-b", "+B"];
        assert_eq!(hunk_starts(&lines), vec![0, 3]);
        assert_eq!(
            hunk_starts(&["+one", "+two"]),
            vec![0],
            "a headerless pseudo-diff is one unbroken hunk"
        );
        assert_eq!(hunk_starts(&[]), vec![0]);
    }
}
