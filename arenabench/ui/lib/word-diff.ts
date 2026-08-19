/**
 * Word-level highlighting inside a modified line pair.
 *
 * A **port of `crates/stella-transcript/src/word.rs`**, which is how every
 * Stella surface that draws a diff decides *what in a line* changed — the
 * Command Deck's character grid and the jet-black web transcript the
 * Observatory serves both render from it. An arena transcript is read beside
 * those artifacts when someone is arguing about what a run did, which is
 * exactly when one edit must not look like two.
 *
 * It lives in its own module rather than inside `transcript-page.tsx` for one
 * reason: so `scripts/check-word-diff-parity.mjs` can import it and prove it
 * still agrees with the Rust. See that script, and
 * `crates/stella-transcript/tests/word_highlight_matrix.rs` on the other side.
 * That is the same arrangement `diff-view.ts` has, and for the same reason:
 * the two halves cannot share a test runner, so they share a golden file.
 *
 * ## Why line tint is not enough
 *
 * A line-level diff answers "did this line change". It does not answer the
 * question a reader actually has, which is *what* in it changed — and on a long
 * line the difference between those two answers is the whole value of looking
 * at the diff. `\setlength{\parindent}{15pt}` against
 * `\setlength{\parindent}{12pt}` is one changed token in thirty; a renderer
 * that tints the whole line has told the reader nothing the `−`/`+` sigil did
 * not.
 *
 * ## Tokenisation
 *
 * A token is either a maximal run of alphanumerics and `_`, or a single other
 * character. Whitespace is its own token, so re-indentation shows up rather
 * than being absorbed into a neighbour. Punctuation-as-single-tokens matters
 * more than it looks against this codebase's actual inputs — LaTeX, JSON and
 * Rust are all mostly punctuation, and a tokeniser that lumped `}{` together
 * would report a change in `{\parindent}{15pt}` as spanning the brace run.
 *
 * ## Character-level fallback
 *
 * Token granularity over-reports when the real change is a fragment of a word:
 * `--fast` → `--fast2` is one token replaced, and tinting the whole flag hides
 * which end moved. So a sole changed run that is either short or shares an
 * affix with its counterpart is re-diffed at character granularity.
 *
 * ## The all-changed guard
 *
 * If word highlighting would tint essentially the whole line it is dropped: the
 * line tint already carries "this line changed", and a second tint over all of
 * it is noise that makes the genuinely-partial changes elsewhere harder to
 * spot.
 *
 * ## Unicode
 *
 * Every length and every split here is in **code points**, never UTF-16 code
 * units — `[...text]` and `Array.from`, never `.length` or `.split("")`. The
 * Rust counts `chars()`, so a line containing an emoji or any astral character
 * would otherwise saturate at a different threshold on the two sides and split
 * a surrogate pair into two "characters" on this one.
 */

/**
 * Above this fraction of a line being "changed", word highlights are dropped as
 * noise and the line tint carries the signal alone. `word.rs::SATURATION`.
 */
export const SATURATION = 0.8;

/**
 * A changed run shorter than this many characters triggers the character-level
 * re-diff, because at that size a whole-token tint is mostly wrong.
 * `word.rs::SHORT_RUN`.
 */
export const SHORT_RUN = 3;

/** One rendered span of a line: text, plus whether it is the *changed* part. */
export type Span = {
  text: string;
  /** Whether this span gets the stronger tint. */
  changed: boolean;
};

const plain = (text: string): Span => ({ text, changed: false });
const hot = (text: string): Span => ({ text, changed: true });

/** Code points of `text`, so every count and slice below agrees with Rust. */
function chars(text: string): string[] {
  return Array.from(text);
}

function isWord(ch: string): boolean {
  // `\p{L}\p{N}` is `char::is_alphanumeric`: Unicode letters and numbers, not
  // the ASCII-only set `\w` would give.
  return /[\p{L}\p{N}_]/u.test(ch);
}

/** Split a line into diffable tokens. `word.rs::tokenize`. */
export function tokenize(line: string): string[] {
  const cs = chars(line);
  const out: string[] = [];
  let i = 0;
  while (i < cs.length) {
    if (isWord(cs[i])) {
      let j = i + 1;
      while (j < cs.length && isWord(cs[j])) j += 1;
      out.push(cs.slice(i, j).join(""));
      i = j;
    } else {
      out.push(cs[i]);
      i += 1;
    }
  }
  return out;
}

type Edit = { kind: "keep" | "remove" | "add"; text: string };

/**
 * Exact LCS over tokens. `word.rs::lcs_script`.
 *
 * Lines are short enough that the quadratic table is never the cost that
 * matters, and the caller has already bounded how many pairs reach here. The
 * tie-break — `>=` toward removing from the old side — is load-bearing for
 * parity: a different tie-break produces an equally valid edit script with
 * different spans, and the golden would disagree on every symmetric case.
 */
function lcsScript(old: string[], next: string[]): Edit[] {
  const n = old.length;
  const m = next.length;
  const table = new Uint32Array((n + 1) * (m + 1));
  const at = (i: number, j: number) => i * (m + 1) + j;
  for (let i = n - 1; i >= 0; i -= 1) {
    for (let j = m - 1; j >= 0; j -= 1) {
      table[at(i, j)] =
        old[i] === next[j]
          ? table[at(i + 1, j + 1)] + 1
          : Math.max(table[at(i + 1, j)], table[at(i, j + 1)]);
    }
  }

  const script: Edit[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (old[i] === next[j]) {
      script.push({ kind: "keep", text: old[i] });
      i += 1;
      j += 1;
    } else if (table[at(i + 1, j)] >= table[at(i, j + 1)]) {
      script.push({ kind: "remove", text: old[i] });
      i += 1;
    } else {
      script.push({ kind: "add", text: next[j] });
      j += 1;
    }
  }
  for (; i < n; i += 1) script.push({ kind: "remove", text: old[i] });
  for (; j < m; j += 1) script.push({ kind: "add", text: next[j] });
  return script;
}

function spansFromScript(script: Edit[]): [Span[], Span[]] {
  const oldSpans: Span[] = [];
  const newSpans: Span[] = [];
  for (const edit of script) {
    if (edit.kind === "keep") {
      oldSpans.push(plain(edit.text));
      newSpans.push(plain(edit.text));
    } else if (edit.kind === "remove") {
      oldSpans.push(hot(edit.text));
    } else {
      newSpans.push(hot(edit.text));
    }
  }
  return [oldSpans, newSpans];
}

/** The index of the only changed span, or `null` when there are zero or many. */
function soleChanged(spans: Span[]): number | null {
  let found: number | null = null;
  for (let i = 0; i < spans.length; i += 1) {
    if (!spans[i].changed) continue;
    if (found !== null) return null;
    found = i;
  }
  return found;
}

/**
 * Whether two token runs share a leading or trailing character run long enough
 * that character granularity would say something token granularity cannot.
 * `word.rs::shares_affix`.
 */
function sharesAffix(old: string, next: string): boolean {
  if (old.length === 0 || next.length === 0) return false;
  const a = chars(old);
  const b = chars(next);
  let prefix = 0;
  while (prefix < a.length && prefix < b.length && a[prefix] === b[prefix]) prefix += 1;
  let suffix = 0;
  while (
    suffix < a.length &&
    suffix < b.length &&
    a[a.length - 1 - suffix] === b[b.length - 1 - suffix]
  ) {
    suffix += 1;
  }
  return prefix >= SHORT_RUN || suffix >= SHORT_RUN;
}

/**
 * Re-diff a short changed run at character granularity. `word.rs::refine_short_runs`.
 *
 * Only fires when both sides have exactly one changed run and it is either
 * short or shares an affix — the cases where a whole-token tint is demonstrably
 * coarser than the truth. A multi-run pair is left alone: character diffing
 * there produces the speckled highlight this module exists to avoid.
 */
function refineShortRuns(oldSpans: Span[], newSpans: Span[]): [Span[], Span[]] {
  const oldRun = soleChanged(oldSpans);
  const newRun = soleChanged(newSpans);
  if (oldRun === null || newRun === null) return [oldSpans, newSpans];

  const oldText = oldSpans[oldRun].text;
  const newText = newSpans[newRun].text;
  const short = chars(oldText).length < SHORT_RUN || chars(newText).length < SHORT_RUN;
  if (!short && !sharesAffix(oldText, newText)) return [oldSpans, newSpans];

  const [refinedOld, refinedNew] = spansFromScript(
    lcsScript(chars(oldText), chars(newText)),
  );
  return [
    [...oldSpans.slice(0, oldRun), ...refinedOld, ...oldSpans.slice(oldRun + 1)],
    [...newSpans.slice(0, newRun), ...refinedNew, ...newSpans.slice(newRun + 1)],
  ];
}

function saturated(spans: Span[], line: string): boolean {
  const total = chars(line).length;
  if (total === 0) return false;
  const changed = spans
    .filter((span) => span.changed)
    .reduce((sum, span) => sum + chars(span.text).length, 0);
  return changed / total > SATURATION;
}

/**
 * Collapse adjacent spans of the same kind, so the renderer emits one element
 * per run rather than one per token. `word.rs::merge`.
 */
function merge(spans: Span[]): Span[] {
  const out: Span[] = [];
  for (const span of spans) {
    const last = out[out.length - 1];
    if (last && last.changed === span.changed) last.text += span.text;
    else out.push({ ...span });
  }
  return out;
}

/**
 * Highlight one removed/added line pair. `word.rs::highlight`.
 *
 * Returns `[oldSpans, newSpans]`. When the pair is too dissimilar to be worth
 * annotating, both sides come back as a single unchanged span and the caller
 * renders line tint alone.
 */
export function highlight(old: string, next: string): [Span[], Span[]] {
  if (old === next) return [[plain(old)], [plain(next)]];

  const script = lcsScript(tokenize(old), tokenize(next));
  const [rawOld, rawNew] = spansFromScript(script);
  const [oldSpans, newSpans] = refineShortRuns(rawOld, rawNew);

  if (saturated(oldSpans, old) || saturated(newSpans, next)) {
    return [[plain(old)], [plain(next)]];
  }
  return [merge(oldSpans), merge(newSpans)];
}

/**
 * Pair up the `-`/`+` runs of a parsed diff so each modified line knows its
 * counterpart, and return the spans for every line by index.
 *
 * The renderer has rows, not pairs: a hunk is a flat list where a run of
 * removals is followed by a run of additions. Highlighting needs the pairing,
 * and the pairing is positional — the *k*th removal of a run against the *k*th
 * addition of the run that follows it, which is what `git diff --word-diff`
 * does and what the crate's own `file_diff` feeds `highlight`.
 *
 * A run whose two sides differ in length pairs what it can and leaves the
 * surplus lines unhighlighted: a 2-for-5 replacement has no honest pairing for
 * the last three, and inventing one tints an arbitrary counterpart.
 *
 * `signs[i]` is `"-"`, `"+"` or anything else for a line that is neither.
 * Returns a sparse array: index *i* holds that line's spans, or `undefined`
 * when the line has no counterpart and should render as plain tinted text.
 */
export function pairedSpans(
  signs: readonly string[],
  texts: readonly string[],
): Array<Span[] | undefined> {
  const out: Array<Span[] | undefined> = new Array(signs.length).fill(undefined);
  let i = 0;
  while (i < signs.length) {
    if (signs[i] !== "-") {
      i += 1;
      continue;
    }
    const removedFrom = i;
    while (i < signs.length && signs[i] === "-") i += 1;
    const addedFrom = i;
    while (i < signs.length && signs[i] === "+") i += 1;
    const removed = addedFrom - removedFrom;
    const added = i - addedFrom;
    for (let k = 0; k < Math.min(removed, added); k += 1) {
      const [oldSpans, newSpans] = highlight(
        texts[removedFrom + k],
        texts[addedFrom + k],
      );
      out[removedFrom + k] = oldSpans;
      out[addedFrom + k] = newSpans;
    }
  }
  return out;
}
