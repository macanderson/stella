/**
 * A unified diff, parsed into the rows a dual-gutter table renders.
 *
 * Split out of `transcript-page.tsx` when the page took the crate's diff
 * grammar (`crates/stella-transcript/src/file_diff.rs`), which numbers **both**
 * sides. The old single-gutter rendering had to pick one: a removed line has no
 * new number and an added line has no old one, so whichever column it drew, one
 * kind of row carried a number belonging to the other side of the change.
 *
 * The numbering walk is a port of `crates/stella-diff/src/diff.rs::body_lines`.
 * Tones deliberately do not depend on hunk state — the event path can emit a
 * headerless pseudo-diff of bare `+`/`-` lines — so malformed input degrades to
 * unnumbered coloured text and never to a crash.
 */

export type DiffTone = "add" | "remove" | "hunk" | "meta" | "context";

export type DiffRow = {
  /** The raw line, sign included. */
  text: string;
  /** The line without its leading sign — what the code column shows. */
  code: string;
  /** `"+"`, `"-"`, or `""`. */
  sign: string;
  tone: DiffTone;
  /** The old-side number, or `null` for an added line and outside a hunk. */
  oldNo: number | null;
  /** The new-side number, or `null` for a removed line and outside a hunk. */
  newNo: number | null;
};

/** Diff metadata rather than source content (`stella-diff`'s `is_meta`). */
export function isDiffMeta(line: string): boolean {
  return (
    line.startsWith("+++ ") ||
    line.startsWith("--- ") ||
    line.startsWith("diff ") ||
    line.startsWith("index ")
  );
}

/** Split on `\n` the way Rust's `str::lines()` does: no trailing empty element
 *  for text ending in a newline, so an index computed on one agrees with a
 *  slice taken from the other. */
export function splitLines(text: string): string[] {
  const lines = text.split("\n");
  if (text.endsWith("\n")) lines.pop();
  return lines;
}

/** One row per diff line, with both gutters tracked from the `@@` headers. */
export function parseDiff(diff: string): DiffRow[] {
  let oldNo: number | null = null;
  let newNo: number | null = null;

  return splitLines(diff).map((text): DiffRow => {
    if (text.startsWith("@@")) {
      const m = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(text);
      oldNo = m ? parseInt(m[1], 10) : null;
      newNo = m ? parseInt(m[2], 10) : null;
      return { text, code: text, sign: "", tone: "hunk", oldNo: null, newNo: null };
    }
    if (isDiffMeta(text)) {
      return { text, code: text, sign: "", tone: "meta", oldNo: null, newNo: null };
    }
    const head = text.charAt(0);
    const code = text.slice(1);
    if (head === "+") {
      const at = newNo;
      if (newNo !== null) newNo += 1;
      return { text, code, sign: "+", tone: "add", oldNo: null, newNo: at };
    }
    if (head === "-") {
      const at = oldNo;
      if (oldNo !== null) oldNo += 1;
      return { text, code, sign: "-", tone: "remove", oldNo: at, newNo: null };
    }
    if (head === " ") {
      const oldAt = oldNo;
      const newAt = newNo;
      if (oldNo !== null) oldNo += 1;
      if (newNo !== null) newNo += 1;
      // Context is the one row that legitimately carries both numbers, which
      // is the whole reason a reader can follow a hunk across a change.
      return { text, code, sign: "", tone: "context", oldNo: oldAt, newNo: newAt };
    }
    // An elision marker (`⋯ 25 lines not shown`) or anything else the emitter
    // interleaves: rendered plain and unnumbered.
    return { text, code: text, sign: "", tone: "context", oldNo: null, newNo: null };
  });
}
