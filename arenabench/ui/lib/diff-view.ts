/**
 * How much of a diff the arena transcript draws, and which part.
 *
 * A **port of `crates/stella-diff/src/view.rs::plan`**, which is the one
 * policy every Stella surface spends its diff budget by: the Command Deck,
 * the plain `stella run` scrollback, the Observatory, an exported dashboard,
 * and this page. An arena transcript is read beside those artifacts when
 * someone is arguing about what a run did, which is exactly when one edit
 * must not look like two.
 *
 * It lives in its own module rather than inside `transcript-page.tsx` for one
 * reason: so `scripts/check-diff-view-parity.mjs` can import it and prove it
 * still agrees with the Rust. See that script, and
 * `crates/stella-diff/tests/view_plan_matrix.rs` on the other side.
 *
 * ## The policy
 *
 * Fill from **both ends** and elide the middle. Filling only from the front —
 * what this did until the shared policy landed — is honest about what it shows
 * and silent about the shape of what it hides: a long edit rendered as its
 * first twenty lines reads as one that starts here and trails off, when the
 * reader's actual question is usually answered at the other end.
 *
 * Two levels, because a diff has two natural units:
 *
 * - **Whole hunks**, taken alternately from the front and the back while they
 *   fit. A cut inside a hunk lands routinely between a `-` line and the `+`
 *   line replacing it, leaving a change that reads as a pure deletion.
 * - **A line window at each end**, when no whole hunk fits at all — one
 *   enormous hunk, which is exactly what a created file looks like.
 *
 * Either way there is **at most one elision**, so there is at most one marker
 * to draw and one number to put in it.
 */

/**
 * Lines an inline diff shows before eliding — the deck's
 * `crates/stella-diff/src/view.rs::INLINE_CAP` exactly, which
 * `crates/stella-tui/src/render.rs::INLINE_DIFF_CAP` also re-exports.
 */
export const INLINE_DIFF_CAP = 20;

export type DiffPlan = {
  /** Whether each input line survives the budget. */
  keep: boolean[];
  /** Lines the budget dropped. */
  hidden: number;
  /**
   * The index the `⋯ n lines` marker belongs in FRONT of — the first hidden
   * line. That is `raw.length` when the elided lines are the tail, and `0`
   * when the view kept only a tail. `-1` when nothing was elided.
   *
   * `0` is a real, reachable position, which is why "nothing elided" is a
   * different value rather than a falsy one.
   */
  foldBefore: number;
};

/**
 * Which diff lines a `cap`-limited render keeps, and where it elided the rest.
 *
 * `raw` is the diff's lines; a line starting `@@` opens a hunk. Anything above
 * the first `@@` is metadata naming the file that hunk is in, so it joins that
 * hunk rather than spending two lines of the budget as a hunk of its own.
 *
 * Note that `cap` bounds the *rendered lines*, not the changed ones: context
 * lines and `@@` headers are lines a reader has to look at, so they are lines
 * the budget pays for.
 */
export function selectDiffLines(raw: string[], cap: number): DiffPlan {
  const n = raw.length;
  if (n <= cap) return { keep: raw.map(() => true), hidden: 0, foldBefore: -1 };
  if (cap <= 0) return { keep: raw.map(() => false), hidden: n, foldBefore: 0 };

  // Hunk boundaries as a strictly ascending fence `[0, …, n]`, so hunk `k` is
  // `bounds[k]..bounds[k + 1]`.
  const bounds: number[] = [0];
  raw.forEach((line, i) => {
    if (i > 0 && line.startsWith("@@") && i > bounds[bounds.length - 1]) {
      bounds.push(i);
    }
  });
  // Leading metadata: the first `@@` stops being a boundary, so the lines
  // above it join the hunk they introduce.
  const firstHunk = raw.findIndex((line) => line.startsWith("@@"));
  if (firstHunk > 0 && bounds[1] === firstHunk) bounds.splice(1, 1);
  bounds.push(n);
  const hunks = bounds.length - 1;

  // Fill from both ends, front first, until neither end's next whole hunk
  // fits. Front-biased on purpose: with an odd budget the beginning is the
  // half a reader orients from.
  let head = 0;
  let tail = 0;
  let used = 0;
  for (;;) {
    let progressed = false;
    if (head + tail < hunks) {
      const len = bounds[head + 1] - bounds[head];
      if (used + len <= cap) {
        used += len;
        head += 1;
        progressed = true;
      }
    }
    if (head + tail < hunks) {
      const k = hunks - tail - 1;
      const len = bounds[k + 1] - bounds[k];
      if (used + len <= cap) {
        used += len;
        tail += 1;
        progressed = true;
      }
    }
    if (!progressed) break;
  }

  const keep = raw.map(() => false);
  if (head > 0 || tail > 0) {
    keep.fill(true, 0, bounds[head]);
    if (tail > 0) keep.fill(true, bounds[hunks - tail], n);
    // `bounds[head]` and not the end of the shown range: when the budget kept
    // only a tail, every hidden line is ABOVE what survives and the marker
    // belongs in front of it. Reading the end instead puts "and there was
    // more below" under a rendering whose last row is the file's last row —
    // the exact defect this policy exists to remove, and a bug that was live
    // in the Rust until this port disagreed with it.
    return { keep, hidden: n - used, foldBefore: bounds[head] };
  }

  // No whole hunk fits: split the budget across the sole hunk's two ends.
  const headLines = Math.ceil(cap / 2);
  const tailLines = cap - headLines;
  keep.fill(true, 0, headLines);
  if (tailLines > 0) keep.fill(true, n - tailLines, n);
  return { keep, hidden: n - cap, foldBefore: headLines };
}
