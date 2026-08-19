"use client";

import * as React from "react";

import type { DiffRow } from "@/lib/transcript-diff";
import { pairedSpans, type Span } from "@/lib/word-highlight";
import { cn } from "@/lib/utils";

/**
 * A diff in the transcript grammar: dual line-number gutters, a sign column,
 * line tint, and a second stronger tint on the tokens that actually changed.
 *
 * The word-level highlight is the part that earns the table. A line-level diff
 * answers "did this line change"; it does not answer the question a reader has,
 * which is *what* in it changed — and on a long line those are entirely
 * different answers. `\setlength{\parindent}{15pt}` against `…{12pt}` is one
 * changed token in thirty, and tinting the whole row tells the reader nothing
 * the `−`/`+` sigil did not.
 *
 * Which tokens get the second tint is not decided here. It is
 * `crates/stella-transcript/src/word.rs`, ported to `lib/word-highlight.ts` and held
 * to the Rust by a golden matrix both languages render
 * (`scripts/check-word-highlight-parity.mjs`), so the arena tints the same tokens
 * the Command Deck and the Observatory do — which matters exactly when someone
 * is reading two of those surfaces side by side to check a claim.
 */
export function DiffTable({
  rows,
  query,
  highlightText,
}: {
  rows: DiffRow[];
  query: string;
  /** The page's search highlighter, injected so the drawer and the feed cannot
   *  disagree about what a match looks like. */
  highlightText: (text: string, query: string) => React.ReactNode;
}) {
  // Pairing is positional — the kth removal of a run against the kth addition
  // of the run that follows it — and it is computed over the rows as rendered,
  // so an elided middle never pairs two lines that were never adjacent.
  const spans = React.useMemo(
    () =>
      pairedSpans(
        rows.map((row) => row.sign),
        rows.map((row) => row.code),
      ),
    [rows],
  );

  return (
    <table className="tx-diff">
      <tbody>
        {rows.map((row, i) => {
          // Every row carries all four cells, including a `@@` header and an
          // elision marker. Spanning them was the obvious rendering and the
          // wrong one twice over: `table-layout: fixed` derives its column
          // widths from the first row, the first row of a diff is routinely a
          // hunk header, and a spanned first row leaves the browser to split
          // the table into four equal parts — which pushed the code a third of
          // the way across the page. (A `colgroup` is the textbook answer and
          // was measurably ignored here.) Keeping the cells also puts the `@@`
          // over the code column rather than starting it under the gutters,
          // which is where a reader expects it: the header describes the lines
          // below it, not the numbers beside it.
          const spanning = row.tone === "hunk" || row.tone === "meta";
          return (
            <tr
              key={i}
              className={
                row.tone === "add"
                  ? "add"
                  : row.tone === "remove"
                    ? "del"
                    : row.tone === "hunk"
                      ? "hunk"
                      : undefined
              }
            >
              <td className="no">{spanning ? "" : (row.oldNo ?? "")}</td>
              <td className="no new">{spanning ? "" : (row.newNo ?? "")}</td>
              <td className="sign">{row.sign}</td>
              <td className="code">
                {spanning ? (
                  highlightText(row.text, query)
                ) : (
                  <DiffCode
                    row={row}
                    spans={spans[i]}
                    query={query}
                    highlightText={highlightText}
                  />
                )}
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

/**
 * One code cell: word spans when the line has a counterpart, plain text when it
 * does not.
 *
 * A search match wins over a word tint where they overlap — they are different
 * questions ("what changed" vs "where is the thing I typed"), and the one the
 * reader asked *just now* is the one that should be findable. So the search
 * highlighter runs inside each span rather than the span suppressing it.
 */
function DiffCode({
  row,
  spans,
  query,
  highlightText,
}: {
  row: DiffRow;
  spans: Span[] | undefined;
  query: string;
  highlightText: (text: string, query: string) => React.ReactNode;
}) {
  if (!spans) return <>{highlightText(row.code, query)}</>;
  return (
    <>
      {spans.map((span, i) => (
        <span
          key={i}
          className={cn(span.changed && (row.tone === "add" ? "tx-aw" : "tx-dw"))}
        >
          {highlightText(span.text, query)}
        </span>
      ))}
    </>
  );
}
