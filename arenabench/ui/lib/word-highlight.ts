/**
 * Word-level highlighting inside a modified line pair — the TypeScript half of
 * a cross-language port of `crates/stella-transcript/src/word.rs`.
 *
 * A line-level diff answers "did this line change". It does not answer the
 * question a reader actually has, which is *what* in it changed. An arena
 * transcript is read *beside* the Command Deck and the Observatory when someone
 * is arguing about what a run did, so one edit must not look like two — which
 * is the same argument `lib/diff-view.ts` carries for the fold policy.
 *
 * This page is a Next.js client with no way to call into the workspace, so the
 * algorithm is reimplemented rather than shared. What is shared is a **file**:
 * `crates/stella-transcript/tests/fixtures/word-highlight-matrix.txt`,
 * generated from the Rust and pinned there by `tests/word_highlight_matrix.rs`.
 * `scripts/check-word-highlight-parity.mjs` runs this module over the same
 * matrix and asserts it reproduces that file exactly, so a Rust-side change to
 * the rules fails here until the port catches up (#3676).
 *
 * Everything below mirrors `word.rs` structurally, name for name, so the two
 * can be read side by side. The one systematic difference is iteration unit:
 * Rust iterates `char` (Unicode scalar values) where JavaScript strings are
 * UTF-16, so every place `word.rs` says `.chars()` this file goes through
 * {@link chars}, never `.length` or index arithmetic.
 */

/**
 * Above this fraction of a line being "changed", word highlights are dropped as
 * noise and the line tint carries the signal alone. Mirrors `word::SATURATION`.
 */
export const SATURATION = 0.8;

/**
 * A changed run shorter than this many characters triggers the character-level
 * re-diff. Mirrors `word::SHORT_RUN`.
 */
export const SHORT_RUN = 3;

/**
 * Beyond this many LCS table cells, word highlighting is abandoned for the line
 * tint alone. Mirrors `word::LCS_CELL_CAP` — and matters more here than in the
 * Rust: this runs on the reader's main thread, where a pathological line does
 * not merely stall a render, it freezes the tab.
 */
export const LCS_CELL_CAP = 250_000;

/** One rendered span of a line: text, plus whether it is the *changed* part. */
export type Span = {
  text: string;
  changed: boolean;
};

/** Unicode scalar values, the unit `word.rs` counts in. */
function chars(s: string): string[] {
  return Array.from(s);
}

function isWord(ch: string): boolean {
  // Rust's `char::is_alphanumeric` is the Unicode Alphabetic or Nd property,
  // which is what these two classes match with the `u` flag.
  return ch === "_" || /[\p{Alphabetic}\p{Nd}]/u.test(ch);
}

/**
 * Split a line into diffable tokens: a maximal run of alphanumerics and `_`, or
 * a single other character. Whitespace is its own token so re-indentation shows
 * up rather than being absorbed into a neighbour. Mirrors `word::tokenize`.
 */
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

/**
 * Whether an LCS table over these two sequence lengths would exceed
 * {@link LCS_CELL_CAP}. Mirrors `word::over_cap` — including that it measures
 * `(n + 1) * (m + 1)`, the table actually allocated, not `n * m`.
 */
function overCap(n: number, m: number): boolean {
  return (n + 1) * (m + 1) > LCS_CELL_CAP;
}

type Edit =
  | { kind: "keep"; text: string }
  | { kind: "remove"; text: string }
  | { kind: "add"; text: string };

/**
 * Exact LCS over tokens — quadratic in time *and* space. Every caller must
 * first put its two lengths through {@link overCap}. Mirrors `word::lcs_script`,
 * including the `>=` tie-break, which is what decides whether an equal-cost
 * edit is reported as a remove-then-add or an add-then-remove; flipping it
 * silently reorders spans and breaks parity.
 */
function lcsScript(oldTokens: string[], newTokens: string[]): Edit[] {
  const n = oldTokens.length;
  const m = newTokens.length;
  const table = new Uint32Array((n + 1) * (m + 1));
  const idx = (i: number, j: number) => i * (m + 1) + j;
  for (let i = n - 1; i >= 0; i -= 1) {
    for (let j = m - 1; j >= 0; j -= 1) {
      table[idx(i, j)] =
        oldTokens[i] === newTokens[j]
          ? table[idx(i + 1, j + 1)] + 1
          : Math.max(table[idx(i + 1, j)], table[idx(i, j + 1)]);
    }
  }

  const script: Edit[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (oldTokens[i] === newTokens[j]) {
      script.push({ kind: "keep", text: oldTokens[i] });
      i += 1;
      j += 1;
    } else if (table[idx(i + 1, j)] >= table[idx(i, j + 1)]) {
      script.push({ kind: "remove", text: oldTokens[i] });
      i += 1;
    } else {
      script.push({ kind: "add", text: newTokens[j] });
      j += 1;
    }
  }
  for (; i < n; i += 1) script.push({ kind: "remove", text: oldTokens[i] });
  for (; j < m; j += 1) script.push({ kind: "add", text: newTokens[j] });
  return script;
}

function spansFromScript(script: Edit[]): [Span[], Span[]] {
  const oldSpans: Span[] = [];
  const newSpans: Span[] = [];
  for (const edit of script) {
    if (edit.kind === "keep") {
      oldSpans.push({ text: edit.text, changed: false });
      newSpans.push({ text: edit.text, changed: false });
    } else if (edit.kind === "remove") {
      oldSpans.push({ text: edit.text, changed: true });
    } else {
      newSpans.push({ text: edit.text, changed: true });
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
 * Mirrors `word::shares_affix`.
 */
function sharesAffix(oldText: string, newText: string): boolean {
  if (oldText.length === 0 || newText.length === 0) return false;
  const a = chars(oldText);
  const b = chars(newText);
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
 * Re-diff a short changed run at character granularity. Mirrors
 * `word::refine_short_runs`, including that it mutates the arrays in place and
 * bails on a multi-run pair.
 */
function refineShortRuns(oldSpans: Span[], newSpans: Span[]): void {
  const oldRun = soleChanged(oldSpans);
  const newRun = soleChanged(newSpans);
  if (oldRun === null || newRun === null) return;

  const oldText = oldSpans[oldRun].text;
  const newText = newSpans[newRun].text;
  const oldChars = chars(oldText);
  const newChars = chars(newText);
  const short = oldChars.length < SHORT_RUN || newChars.length < SHORT_RUN;
  if (!short && !sharesAffix(oldText, newText)) return;

  // A sole changed run can be the whole line. Leaving the token-level spans in
  // place is the right fallback: whatever the line-level pass decided stands.
  if (overCap(oldChars.length, newChars.length)) return;

  const [refinedOld, refinedNew] = spansFromScript(lcsScript(oldChars, newChars));
  oldSpans.splice(oldRun, 1, ...refinedOld);
  newSpans.splice(newRun, 1, ...refinedNew);
}

function saturated(spans: Span[], line: string): boolean {
  const total = chars(line).length;
  if (total === 0) return false;
  let changed = 0;
  for (const span of spans) {
    if (span.changed) changed += chars(span.text).length;
  }
  return changed / total > SATURATION;
}

/**
 * Collapse adjacent spans of the same kind so the renderer emits one element
 * per run rather than one per token. Mirrors `word::merge`.
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
 * Highlight one removed/added line pair.
 *
 * Returns `[oldSpans, newSpans]`. When the pair is too dissimilar to be worth
 * annotating, both sides come back as a single unchanged span and the caller
 * renders line tint alone. Mirrors `word::highlight`.
 */
export function highlight(oldLine: string, newLine: string): [Span[], Span[]] {
  const plain = (): [Span[], Span[]] => [
    [{ text: oldLine, changed: false }],
    [{ text: newLine, changed: false }],
  ];

  if (oldLine === newLine) return plain();

  const oldTokens = tokenize(oldLine);
  const newTokens = tokenize(newLine);
  if (overCap(oldTokens.length, newTokens.length)) return plain();

  const [oldSpans, newSpans] = spansFromScript(lcsScript(oldTokens, newTokens));
  refineShortRuns(oldSpans, newSpans);

  if (saturated(oldSpans, oldLine) || saturated(newSpans, newLine)) return plain();
  return [merge(oldSpans), merge(newSpans)];
}

/**
 * Word-highlight a run of removals against its run of additions.
 *
 * Pairing is positional: the `k`th removal is compared with the `k`th addition,
 * and unpaired lines carry no word spans. A port of
 * `crates/stella-transcript/src/file_diff.rs::emit_change_block` — the pairing
 * is the half that must stay correct, because a "smarter" similarity match that
 * highlighted line 1 against line 3 would be correct and unreadable.
 */
export function highlightChangeBlock(
  removals: string[],
  additions: string[],
): { removed: Span[][]; added: Span[][] } {
  const paired = Math.min(removals.length, additions.length);
  const removed: Span[][] = [];
  const added: Span[][] = [];
  for (let k = 0; k < paired; k += 1) {
    const [o, n] = highlight(removals[k], additions[k]);
    removed.push(o);
    added.push(n);
  }
  for (let k = paired; k < removals.length; k += 1) {
    removed.push([{ text: removals[k], changed: false }]);
  }
  for (let k = paired; k < additions.length; k += 1) {
    added.push([{ text: additions[k], changed: false }]);
  }
  return { removed, added };
}

/**
 * Word-highlight the rows of a rendered diff, keyed by their sign column.
 *
 * A renderer holds one flat list of rows rather than the removal/addition runs
 * {@link highlightChangeBlock} takes, so this walks the list, finds each run of
 * `-` immediately followed by its run of `+`, and delegates the pairing itself
 * — the caller never re-derives which removal answers which addition, because
 * two implementations of that rule is exactly what the golden matrix exists to
 * prevent.
 *
 * `signs[i]` is `"-"`, `"+"`, or anything else for a line that is neither.
 * Returns a sparse array: index *i* holds that line's spans, or `undefined`
 * when the line is not part of a change block and renders as plain text.
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
    const { removed, added } = highlightChangeBlock(
      texts.slice(removedFrom, addedFrom),
      texts.slice(addedFrom, i),
    );
    removed.forEach((spans, k) => {
      out[removedFrom + k] = spans;
    });
    added.forEach((spans, k) => {
      out[addedFrom + k] = spans;
    });
  }
  return out;
}
