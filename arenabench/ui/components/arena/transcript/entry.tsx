"use client";

/**
 * One transcript entry, rendered in the transcript grammar.
 *
 * Split out of `transcript-page.tsx`, which had reached 1571 lines — past the
 * 1500-line ratchet AGENTS.md holds the tree to. That ratchet's guard
 * (`scripts/check-file-size.sh`) watches `*.rs`, `*.py` and `*.sh` and not
 * TypeScript, so nothing was going to say so; the guard's own header makes the
 * argument for splitting anyway ("a limit that watches one language is not a
 * property of the repository … the growth simply moves to whatever is
 * unwatched"). Widening it is a separate PR because it fails red on this file
 * the moment it lands.
 *
 * The grammar itself is `crates/stella-transcript`'s, which the Command Deck
 * and the Observatory both render from:
 *
 * - **Stages are section rules**, not rows — the label *is* the stage.
 * - **Reasoning is quiet.** Dim, italic, collapsed to a preview with its line
 *   count; the least required text on screen never outshouts the response.
 * - **The label is coloured, the value is read.** A tool's name takes its
 *   CLASS's hue — read/write/run/verify/repo/delegate
 *   (`crates/stella-tui/src/tool_class.rs`, mirrored server-side by
 *   `arenabench.toolclass`) — never a brand accent; bodies stay plain. A colour
 *   earns its place by being rare, and this one answers the first question a
 *   reader asks of any row: was that a look, a change, a shell, a test, a push,
 *   a hand-off?
 * - **A collapsed result shows the line that matters, not the first one.**
 *   `salientLine` anchors the preview at the first line carrying an
 *   error/warning marker — a build's first line is `Checking foo v0.1.0` and
 *   the line a reader came for is twenty lines down. A success shows one line
 *   from there; a failure shows six.
 * - **A call and its result are ONE row.** The tool is named once, its output
 *   nested under it — [`Call`] owns its `Output` in the shared model, and this
 *   page reaches the same shape over the wire's flat stream via `mergeToolRows`.
 *   The wire carries `tool` and `tool_result` as separate entries because that
 *   is the *journal's* shape; rendering them as separate rows printed the tool's
 *   name twice and the invocation up to three times down the whole transcript,
 *   which is the defect `crates/stella-transcript` exists to end.
 * - **The rails are the deck's**: `●` opens a call, `⎿` a result whose call is
 *   not on screen, `✗` a failed one — a distinct glyph *and* column, so a
 *   failure is findable by margin-scan alone.
 * - **Metadata rides as chips**, right-aligned and muted, never inline at the
 *   weight of the work it paid for.
 *
 * One deliberate departure from the deck, and one degradation. A result whose
 * call is *not on screen* — filtered off by kind, or begun before the stream's
 * cursor — renders alone and keeps its tool's name, because a column of outputs
 * naming nothing is worse than a name that appears once; that is the orphan arm
 * below, and it is not licence to print the name beside its own call. And every
 * body on the wire here is character-capped for the live stream's sake
 * (`arenabench.transcript.TOOL_RESULT_BUDGET`) in a way the deck, reading a
 * session in memory, never has to be.
 */

import * as React from "react";
import { Braces } from "lucide-react";

import { INLINE_DIFF_CAP, selectDiffLines } from "@/lib/diff-view";
import { parseDiff, splitLines, type DiffRow } from "@/lib/transcript-diff";
import type { TranscriptEntry } from "@/lib/types";
import { fmtDuration, fmtMoney, fmtTokens } from "@/lib/format";
import { cn } from "@/lib/utils";
import { TOOL_CLASS_TEXT, toolClassOf } from "@/lib/tool-class";
import { pairedSpans, type Span } from "@/lib/word-highlight";
import { DiffTable } from "@/components/arena/transcript/diff-table";

/** Tools whose raw input IS a file change, so it reads far better as a
 *  git-style diff than as a JSON dump — additions green, removals red, the
 *  path as the hunk header. Every other tool keeps the plain argument view. */
const FILE_MUTATION_TOOLS = new Set([
  "write_file",
  "edit_file",
  "apply_edits",
  "delete_file",
]);

type DiffLine = { sign: "+" | "-"; text: string };

type FileHunk = { path?: string; lines: DiffLine[] };

/** Read a file-mutating tool's raw input into one or more per-file diff hunks,
 *  or `null` when the tool is not a mutation or its input does not parse — the
 *  caller then falls back to the plain JSON. A new file is all-additions; an
 *  edit is its old lines removed then its new lines added; a delete is one
 *  removal marker (the bytes are not in the call). `apply_edits` carries a
 *  `path` on each element of its `edits` array (there is no top-level `path`),
 *  and one call can touch several files — so each edit is grouped under its own
 *  `e.path`, consecutive edits to the same file sharing a hunk, giving every
 *  file its own header instead of one anonymous path-less block. Field names
 *  are the tools' own schemas: write_file `{content,path}`, edit_file
 *  `{old_string,new_string,path}`, apply_edits `{edits:[{path,old_string,
 *  new_string}]}`. */
function fileDiffFromRaw(
  name: string | undefined,
  raw: string,
): FileHunk[] | null {
  if (!name || !FILE_MUTATION_TOOLS.has(name)) return null;
  let input: Record<string, unknown>;
  try {
    input = JSON.parse(raw) as Record<string, unknown>;
  } catch {
    return null;
  }
  const path = typeof input.path === "string" ? input.path : undefined;
  const asLines = (v: unknown, sign: DiffLine["sign"]): DiffLine[] =>
    typeof v === "string" && v.length > 0
      ? v.replace(/\n$/, "").split("\n").map((text) => ({ sign, text }))
      : [];
  const hunks: FileHunk[] = [];
  if (name === "write_file") {
    const lines = asLines(input.content, "+");
    if (lines.length > 0) hunks.push({ path, lines });
  } else if (name === "edit_file") {
    const lines = [...asLines(input.old_string, "-"), ...asLines(input.new_string, "+")];
    if (lines.length > 0) hunks.push({ path, lines });
  } else if (name === "apply_edits") {
    const edits = Array.isArray(input.edits) ? input.edits : [];
    for (const e of edits as Record<string, unknown>[]) {
      const editPath = typeof e.path === "string" ? e.path : undefined;
      const lines = [...asLines(e.old_string, "-"), ...asLines(e.new_string, "+")];
      if (lines.length === 0) continue;
      const last = hunks[hunks.length - 1];
      if (last && last.path === editPath) last.lines.push(...lines);
      else hunks.push({ path: editPath, lines });
    }
  } else if (name === "delete_file") {
    hunks.push({ path, lines: [{ sign: "-", text: "(file deleted)" }] });
  }
  return hunks.length > 0 ? hunks : null;
}

/**
 * The stronger wash a changed span gets inside an already-tinted line.
 *
 * Deliberately the *same* `--ok`/`--bad` tokens as the line tint, at a heavier
 * alpha, rather than a new hue: the Instrument palette carries exactly one
 * colour outside the semantic triad and it is identity, never a state. This is
 * also what `crates/stella-cli/src/export/transcript.rs` paints its `.ww`
 * word-tint with, so the exported HTML and this block agree by construction.
 */
const WORD_TINT: Record<"+" | "-", string> = {
  "+": "bg-ok/25",
  "-": "bg-bad/25",
};

/** A tool's file change as a git-style diff: each touched file its own hunk
 *  with its path as the header, additions on a green ground and removals on a
 *  red one, each with its `+`/`-` sign. Line-level tint (not just glyph colour)
 *  so a scroll of edits reads at a glance the way `git diff` does. An
 *  `apply_edits` batch spanning several files renders one hunk per file so a
 *  multi-file change is never flattened into one anonymous block.
 *
 *  Within a change block the *changed tokens* of a paired removal/addition are
 *  washed harder still, by the same `pairedSpans` the transcript's own diff
 *  table uses, so an `edit_file` that moved one number does not read as a whole
 *  line rewritten — and reads the way the Command Deck and the Observatory
 *  render the identical edit. */
function FileDiffBlock({
  hunks,
}: {
  hunks: FileHunk[];
}) {
  return (
    <div className="ml-5 mt-1 space-y-1">
      {hunks.map((hunk, h) => (
        <FileHunkBlock key={h} hunk={hunk} />
      ))}
    </div>
  );
}

function FileHunkBlock({ hunk }: { hunk: FileHunk }) {
  // Pairing is positional over the run of removals and the run of additions
  // that follows it — the rule lives in `lib/word-highlight.ts` and is pinned
  // to the Rust by the golden matrix, so this never re-derives it.
  const spans = React.useMemo(
    () =>
      pairedSpans(
        hunk.lines.map((ln) => ln.sign),
        hunk.lines.map((ln) => ln.text),
      ),
    [hunk],
  );
  return (
    <div className="overflow-x-auto rounded border border-line text-[11px]">
      {hunk.path && (
        <div className="border-b border-line bg-panel-2 px-2 py-1 font-mono text-dim">
          {hunk.path}
        </div>
      )}
      <div className="font-mono leading-[1.4]">
        {hunk.lines.map((ln, i) => (
          <div
            key={i}
            className={cn(
              "whitespace-pre-wrap break-words px-2",
              ln.sign === "+" ? "bg-ok/10 text-ok" : "bg-bad/10 text-bad",
            )}
          >
            <span className="select-none opacity-60">{ln.sign} </span>
            <WordSpans text={ln.text} spans={spans[i]} sign={ln.sign} />
          </div>
        ))}
      </div>
    </div>
  );
}

/**
 * One diff line's text as word-level spans, the changed runs washed harder.
 *
 * Falls back to the plain line whenever there is nothing to say — an unpaired
 * line, or a pair `highlight` judged too dissimilar to annotate. That fallback
 * is the honest rendering: tinting every token of a wholly-rewritten line says
 * only what the line tint already said.
 */
function WordSpans({
  text,
  spans,
  sign,
}: {
  text: string;
  spans: Span[] | undefined;
  sign: string;
}) {
  if (!spans || !spans.some((s) => s.changed)) return <>{text}</>;
  const tint = WORD_TINT[sign === "+" ? "+" : "-"];
  return (
    <>
      {spans.map((span, i) =>
        span.changed ? (
          <span key={i} className={tint}>
            {span.text}
          </span>
        ) : (
          <React.Fragment key={i}>{span.text}</React.Fragment>
        ),
      )}
    </>
  );
}

/** Case-insensitive `<mark>` highlighting, keeping the original casing. */
export function Highlight({ text, query }: { text: string; query: string }) {
  if (!query) return <>{text}</>;
  const needle = query.toLowerCase();
  const parts: React.ReactNode[] = [];
  let rest = text;
  let key = 0;
  for (;;) {
    const at = rest.toLowerCase().indexOf(needle);
    if (at === -1) break;
    if (at > 0) parts.push(rest.slice(0, at));
    parts.push(
      <mark key={key++} className="bg-accent text-on-accent">
        {rest.slice(at, at + query.length)}
      </mark>,
    );
    rest = rest.slice(at + query.length);
  }
  parts.push(rest);
  return <>{parts}</>;
}

export function usageLine(entry: TranscriptEntry): string {
  const meta = (entry.meta || {}) as Record<string, unknown>;
  return (
    `${entry.title ?? "usage"} — ${meta.model || ""}` +
    ` · in ${fmtTokens(meta.tokens_in)} out ${fmtTokens(meta.tokens_out)}` +
    ` · cache ${fmtTokens(meta.cache_read)}/${fmtTokens(meta.cache_write)}` +
    ` · self-rep ${fmtMoney(meta.cost_usd)}`
  );
}

const THINKING_PREVIEW_LINES = 5;

/** Collapsed-result line budgets, matching the deck and the export surfaces:
 * six lines either way, anchored on the salient point.
 *
 * A success used to show one line here, on the argument that its output is
 * chatter and its size belongs in the metric column. That is wrong for the
 * calls whose output *is* the answer — a `search`, a `read_file` — and it was
 * also a third answer to a question the deck and the export had already stopped
 * disagreeing about: the shared policy is
 * `stella_transcript::digest::PREVIEW_LINES`, which is 6 (#3644). Six is also
 * what a failure wants — a compiler error with its location and caret line, or
 * the top of a panic backtrace — so the two budgets coincide rather than being
 * separately chosen.
 *
 * These stay hand-mirrored rather than generated. Unlike the diff-view policy
 * and word highlighting, which are *algorithms* two languages can disagree
 * about subtly, this is one integer, and a golden matrix to pin one integer
 * would cost more than it caught. If a third constant shows up here, that
 * judgement should be revisited. */
const OK_PREVIEW_LINES = 6;
const FAIL_PREVIEW_LINES = 6;

/** Markers that make a line of tool output worth anchoring a collapsed
 * preview on, ported verbatim from `crates/stella-tui/src/render/row.rs::SALIENT`. */
const SALIENT_MARKERS = [
  "error",
  "warning",
  "failed",
  "failure",
  "panic",
  "assert",
  "fatal",
  "exception",
];

/**
 * The line index a collapsed result should start its preview from.
 *
 * A direct port of `row.rs::salient_line`: showing line 1 is the obvious
 * choice and the wrong one — a build's first line is `Checking foo v0.1.0`
 * while the line that matters is twenty lines down — so this finds the first
 * line carrying a failure marker (`error:`, `warning:`, `panic: …`, matched
 * case-insensitively at the start of the trimmed line or as `marker:` within
 * its first 12 characters, so a log line that merely *mentions* an error
 * later on does not hijack the row) and falls back to the first non-blank
 * line when nothing stands out.
 */
function salientLine(text: string): number {
  const lines = splitLines(text);
  let firstNonBlank = 0;
  let seenNonBlank = false;
  for (let i = 0; i < lines.length; i++) {
    const trimmed = lines[i].replace(/^\s+/, "");
    if (!seenNonBlank && trimmed !== "") {
      firstNonBlank = i;
      seenNonBlank = true;
    }
    const lower = trimmed.toLowerCase();
    const hit = SALIENT_MARKERS.some((marker) => {
      if (lower.startsWith(marker)) return true;
      const at = lower.indexOf(`${marker}:`);
      return at !== -1 && at <= 12;
    });
    if (hit) return i;
  }
  return firstNonBlank;
}

/** Frames shown before the fold, matching the deck's `RECALL_PREVIEW`. */
const RECALL_PREVIEW_FRAMES = 3;

type RecallFrame = {
  kind?: string;
  label?: string;
  uri?: string | null;
  provider?: string;
  source?: string;
  method?: string | null;
  id?: string | null;
  digest?: string | null;
  tokens?: number;
};

/**
 * One context recall, as a table.
 *
 * The recall stage reached this page for the first time with this component:
 * `TranscriptReader` had no `context_recall` arm, so the event fell through to
 * `return []` and **every arena transcript silently omitted the stage that
 * decides what the model sees**. A benchmark transcript is the artifact used to
 * argue whether recall helped a run, and the evidence was never in it.
 *
 * Laid out the way the Command Deck lays it out
 * (`crates/stella-tui/src/render/entry.rs`), because it is the same data
 * answering the same questions: a header with the totals and the latency, one
 * row per frame carrying its kind, citation, location and token cost, and a
 * disclosure holding the provenance chain and the budget report.
 */
function RecallEntry({
  entry,
  query,
  open,
  toggle,
}: {
  entry: TranscriptEntry;
  query: string;
  open: boolean;
  toggle: () => void;
}) {
  const meta = (entry.meta || {}) as Record<string, unknown>;
  const frames = (Array.isArray(meta.frames) ? meta.frames : []) as RecallFrame[];
  const budget = (meta.budget || null) as {
    requested?: number;
    consumed?: number;
    providers?: {
      provider_id?: string;
      frames_served?: number;
      frames_rejected?: number;
      token_cost?: number;
    }[];
  } | null;
  const shown = open ? frames : frames.slice(0, RECALL_PREVIEW_FRAMES);
  const hidden = frames.length - shown.length;
  // The folded frames' cost, so the fold names what it hides. Without it the
  // preview keeps the host's render order — which is the honest order, the one
  // the model actually saw — while silently burying the outlier that made the
  // case for per-frame costs in the first place.
  const hiddenTokens = frames
    .slice(shown.length)
    .reduce((sum, f) => sum + (f.tokens ?? 0), 0);

  return (
    <div>
      <button
        type="button"
        onClick={toggle}
        className="cursor-pointer font-semibold text-accent hover:text-foreground"
      >
        ◉ <Highlight text={entry.title ?? "recall"} query={query} />
      </button>
      <table className="w-full border-collapse text-[11px]">
        {/* The columns are named once they are worth naming. Collapsed this is
            a three-row preview whose cells label themselves; expanded it is the
            whole recall with a provenance line under each row, and there the
            heading is what keeps `82 tok` readable as a per-frame cost rather
            than a running total. Same reasoning, and the same wording, as the
            deck's ctrl+o heading in
            `crates/stella-tui/src/render/entry/recall.rs`. */}
        {open && (
          <thead>
            <tr className="border-b border-line-soft text-left align-baseline text-dim">
              <th className="w-[1%] whitespace-nowrap pr-3 font-normal">kind</th>
              <th className="pr-3 font-normal">citation</th>
              <th className="pr-3 font-normal">location</th>
              <th className="w-[1%] whitespace-nowrap text-right font-normal">
                cost
              </th>
            </tr>
          </thead>
        )}
        <tbody>
          {shown.map((frame, i) => (
            <React.Fragment key={`${frame.label}-${i}`}>
              <tr className="align-baseline">
                {/* The field the old rendering dropped, and the one that
                    changes how a row is read: a `memory` and a `symbol` cost
                    the prompt the same tokens and mean entirely different
                    things about what retrieval did. */}
                <td className="w-[1%] whitespace-nowrap pr-3 text-dim">
                  {frame.kind || "frame"}
                </td>
                <td className="max-w-0 truncate pr-3 text-foreground">
                  <Highlight text={frame.label ?? ""} query={query} />
                </td>
                {/* `direction: rtl` elides a path from the *left*: the tail
                    (`…/command_deck/hunk_gate.rs:32`) identifies the frame and
                    the head is a repo prefix every row already shares, so a
                    plain right-truncation removes exactly the useful half. */}
                <td
                  dir="rtl"
                  className="max-w-0 truncate pr-3 text-left text-muted"
                  title={frame.uri ?? undefined}
                >
                  <bdi>
                    <Highlight text={frame.uri ?? ""} query={query} />
                  </bdi>
                </td>
                <td className="w-[1%] whitespace-nowrap text-right tabular-nums text-dim">
                  {frame.tokens ?? 0} tok
                </td>
              </tr>
              {open && (
                <tr>
                  <td />
                  <td colSpan={3} className="pb-1 text-[10.5px] text-dim">
                    {recallProvenance(frame)}
                  </td>
                </tr>
              )}
            </React.Fragment>
          ))}
        </tbody>
      </table>
      {hidden > 0 && (
        <button
          type="button"
          onClick={toggle}
          className="cursor-pointer text-[10.5px] text-dim hover:text-muted"
        >
          ⋯ {hidden} more · {hiddenTokens} tok · provenance and budget
        </button>
      )}
      {open && budget && (
        <div className="pt-1 text-[10.5px] text-dim">
          <div>
            budget {budget.consumed ?? 0} of {budget.requested ?? 0} tok
          </div>
          {/* A grid, not a `·`-joined line. The legs are two or three rows of
              the same four fields, and joined into prose the rejected count
              pushes the cost eleven characters right on the one row that has
              one — so the two numbers a reader is here to compare never share
              an edge. `tabular-nums` keeps the digits themselves in a column,
              which is the browser's half of what the deck's fitted `{n:>w}`
              does in a terminal. */}
          <table className="border-collapse pl-3 tabular-nums">
            <tbody>
              {(budget.providers ?? []).map((leg) => (
                <tr key={leg.provider_id} className="align-baseline">
                  <td className="pl-3 pr-3">{leg.provider_id}</td>
                  <td className="pr-3 text-right">{leg.frames_served ?? 0} served</td>
                  {/* The number the frame list cannot carry — a rejected frame
                      never reaches it — and so the only visible evidence that a
                      provider misdeclared its cost. A leg that rejected nothing
                      leaves the cell empty rather than writing a `0` the eye
                      then has to filter out of the column it is scanning. */}
                  <td className="pr-3 text-right text-warn">
                    {(leg.frames_rejected ?? 0) > 0
                      ? `${leg.frames_rejected} rejected`
                      : ""}
                  </td>
                  <td className="text-right">{leg.token_cost ?? 0} tok</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

/** `provider ← source · method · id · digest` for an expanded frame row. */
function recallProvenance(frame: RecallFrame): string {
  const parts: string[] = [];
  // Two fields on purpose: an adapter fronting another store
  // (`workspace-memory` over `stella-context`) is the case one field hides.
  if (frame.provider && frame.source && frame.provider !== frame.source) {
    parts.push(`${frame.provider} ← ${frame.source}`);
  } else if (frame.provider || frame.source) {
    parts.push(frame.provider || frame.source || "");
  }
  if (frame.method) parts.push(frame.method);
  if (frame.id) parts.push(frame.id);
  // A missing digest is not nothing: per the context-reuse spec such a frame
  // is *not verifiable* and a host must re-query rather than reuse it.
  parts.push(
    frame.digest ? `${frame.digest.slice(0, 14)}…` : "unverifiable (no digest)",
  );
  return parts.join(" · ");
}

/**
 * The control that opens the raw exchange drawer.
 *
 * `Braces` rather than a panel or an arrow glyph: what the reader is reaching
 * for is the *payload* — the argument object Stella sent and what came back —
 * and `{ }` says "the JSON behind this row" in a way no chevron does. Its
 * placement is the same on a call row and a result row, because they open the
 * same drawer showing the same exchange; a reader should not have to learn
 * which half of a pair carries the control.
 */
function Inspect({ onOpen, open }: { onOpen: () => void; open: boolean }) {
  return (
    <button
      type="button"
      onClick={onOpen}
      aria-label="show the raw request and response"
      aria-pressed={open}
      title="raw request and response"
      className={cn(
        "shrink-0 cursor-pointer px-1 leading-none",
        open ? "text-foreground" : "text-dim hover:text-foreground",
      )}
    >
      <Braces size={12} strokeWidth={2} aria-hidden />
    </button>
  );
}

/**
 * Everything a result contributes to the row it belongs to.
 *
 * Computed in one place because a merged call row and an orphan result row show
 * the *same* result and must not be able to disagree about what it says — the
 * shape of defect this whole change is about. The only thing that differs
 * between the two is whether the row re-states the tool's name.
 */
type ResultParts = {
  isError: boolean;
  hasDiff: boolean;
  diffAdded: number;
  diffRemoved: number;
  metrics: string[];
  /** Output lines to print, already windowed on the salient line when folded. */
  shown: string[];
  diffShown: DiffRow[];
  /** What the fold control says it is hiding. Empty means nothing is hidden. */
  foldParts: string[];
  /** Whether an expanded row has anything to collapse back down. */
  collapsible: boolean;
};

function resultParts(result: TranscriptEntry, resultOpen: boolean): ResultParts {
  const body = result.body || "";
  const meta = (result.meta || {}) as Record<string, unknown>;
  const isError = Boolean(meta.error);
  const lines = body ? splitLines(body) : [];
  const total = lines.length;
  // The mutation's diff, inline under the result — correlated server-side
  // (`arenabench.transcript`, the `tool_result` arm) and rendered in the deck's
  // grammar (`crates/stella-tui/src/render/entry.rs`): the metric column states
  // the emitter's own `+N −M` instead of a line count, the collapsed row
  // suppresses the output preview (a prose "Applied edit to …" would restate
  // both the call row above it and the diff under it), and the
  // diff shows at most `INLINE_DIFF_CAP` lines until disclosed. Only a
  // successful mutation carries one, so `isError` never coincides with a diff.
  const diffText = typeof meta.diff === "string" ? meta.diff : "";
  const hasDiff = diffText.length > 0;
  const diffAll = hasDiff ? parseDiff(diffText) : [];
  const diffPlan = resultOpen
    ? { keep: diffAll.map(() => true), hidden: 0, foldBefore: -1 }
    : selectDiffLines(
        diffAll.map((row) => row.text),
        INLINE_DIFF_CAP,
      );
  const diffKeep = diffPlan.keep;
  // A lone hunk header is dropped (`diff.rs::body_lines_inline`): inline under
  // a call row it restates what the gutter beside it already says, and a
  // two-line change should not cost three rows. With several hunks the headers
  // stay — there they are the boundary between two disjoint regions of the file.
  const multiHunk = diffAll.filter((row) => row.tone === "hunk").length > 1;
  const diffHidden = diffPlan.hidden;
  // The elision is drawn WHERE the lines were, as a row of its own. Stated only
  // in the fold summary below, a head-and-tail rendering would read as "the
  // change continues past the bottom" under a row that is already the change's
  // last line.
  const elision = (): DiffRow => {
    const text = `⋯ ${diffHidden} ${diffHidden === 1 ? "line" : "lines"} not shown`;
    return { text, code: text, sign: "", tone: "meta", oldNo: null, newNo: null };
  };
  const diffShown: DiffRow[] = [];
  diffAll.forEach((row, i) => {
    if (i === diffPlan.foldBefore && diffHidden > 0) diffShown.push(elision());
    if (diffKeep[i] && (multiHunk || row.tone !== "hunk")) diffShown.push(row);
  });
  if (diffPlan.foldBefore === diffAll.length && diffHidden > 0) {
    diffShown.push(elision());
  }
  // The collapsed window anchors on the SALIENT line, not line 1 — see
  // `salientLine`'s doc comment — and its size is the shared preview budget.
  //
  // The anchor is clamped so the window is never starved: a salient line near
  // the *end* of the output would otherwise leave fewer than `budget` lines to
  // take, and this surface would show one line where the deck and the export
  // showed six. Sliding the window back keeps the salient line on screen as the
  // last thing shown rather than the first. A port of the same clamp in
  // `crates/stella-tui/src/render/entry.rs`.
  const budget = isError ? FAIL_PREVIEW_LINES : OK_PREVIEW_LINES;
  const anchor =
    total > 0 ? Math.min(salientLine(body), Math.max(0, total - budget)) : 0;
  const collapsedShown = hasDiff ? [] : lines.slice(anchor, anchor + budget);
  const shown = resultOpen ? lines : collapsedShown;
  const hidden = resultOpen ? 0 : total - collapsedShown.length;
  const metrics = [
    meta.duration_ms != null ? fmtDuration(meta.duration_ms) : null,
    // A diff states its own size in `+N −M` — the honest unit for an edit;
    // "12 lines" would describe the tool's chatter, not the change.
    total > 1 && !hasDiff ? `${total} lines` : null,
    // ⚡ marks a speculated result: its duration overlapped the model's own
    // streaming instead of following it, so the number is not latency the run
    // actually spent waiting.
    meta.speculated ? "⚡ speculated" : null,
    // The protocol grew a `ToolOutput` arm this page predates. Say so rather
    // than presenting the JSON fallback as if it were output.
    meta.unrecognized ? "unrecognized output shape" : null,
  ].filter(Boolean) as string[];
  const foldParts = [
    // Not the diff's own hidden count: the `⋯ n lines not shown` row inside the
    // diff already carries it, in the place it happened.
    hidden > 0
      ? hasDiff
        ? `${hidden} output ${hidden === 1 ? "line" : "lines"}`
        : `${hidden} more ${hidden === 1 ? "line" : "lines"}`
      : null,
  ].filter(Boolean) as string[];

  return {
    isError,
    hasDiff,
    diffAdded: Number(meta.diff_added ?? 0),
    diffRemoved: Number(meta.diff_removed ?? 0),
    metrics,
    shown,
    diffShown,
    foldParts,
    collapsible: total > budget || hasDiff,
  };
}

/**
 * A result's chips, for the header of whichever row carries it.
 *
 * `+N −M` first, from the counts the emitter measured
 * (`meta.diff_added`/`diff_removed`) — never a recount of the rendered hunk,
 * which is a bounded view of the changed region and reports a smaller number.
 * Then the metrics, right-aligned and muted: the rule the transcript grammar is
 * built on is that a duration and a cost are the accounting, and the output is
 * the thing.
 */
function ResultChips({ parts }: { parts: ResultParts }) {
  return (
    <>
      {parts.hasDiff && (
        <span className="text-[10.5px] tabular-nums">
          <span className="text-ok">+{parts.diffAdded}</span>{" "}
          <span className="text-bad">−{parts.diffRemoved}</span>
        </span>
      )}
      {parts.metrics.length > 0 && (
        <span className="tx-chips">
          {parts.metrics.map((metric) => (
            <span
              key={metric}
              className={cn("tx-chip", parts.isError && "err")}
            >
              {metric}
            </span>
          ))}
        </span>
      )}
    </>
  );
}

/**
 * A result's body: the output window, the inline diff, and the fold control.
 *
 * Indented onto the row's spine (`ml-5`), which is the character grid's `│`
 * carrying a call's output under the call — the same relationship
 * `crates/stella-transcript`'s two renderers draw, in a browser's units.
 */
function ResultBody({
  parts,
  query,
  resultOpen,
  toggleResult,
}: {
  parts: ResultParts;
  query: string;
  resultOpen: boolean;
  toggleResult: () => void;
}) {
  return (
    <>
      {parts.shown.length > 0 && (
        <pre
          className={cn(
            "ml-5 overflow-x-auto whitespace-pre-wrap break-words",
            parts.isError ? "text-bad/90" : "text-muted",
          )}
        >
          <Highlight text={parts.shown.join("\n")} query={query} />
        </pre>
      )}
      {/* Dual gutters and word-level highlight, from the shared policy — see
          `DiffTable`. The old rendering carried one number column, which for a
          removed line had to show a new-side number it does not have. */}
      {parts.diffShown.length > 0 && (
        <div className="ml-5 overflow-x-auto">
          <DiffTable
            rows={parts.diffShown}
            query={query}
            highlightText={(text, q) => <Highlight text={text} query={q} />}
          />
        </div>
      )}
      {/* The deck only earns this row for a failure — a successful result
          already states its size in the metric column above. A mouse-driven
          page has no ctrl+o, though, so a folded SUCCESS still gets a quiet way
          to see the rest; it just does not compete for attention the way the
          failure's does. With a diff, the fold names both of the things it
          hides: the diff's overflow and the output the collapsed row
          suppressed entirely. */}
      {!resultOpen && parts.foldParts.length > 0 && (
        <button
          type="button"
          onClick={toggleResult}
          className={cn(
            "ml-5 cursor-pointer text-[10.5px] hover:text-muted",
            parts.isError ? "text-dim" : "text-dim/70",
          )}
        >
          ⋯ {parts.foldParts.join(" · ")}
        </button>
      )}
      {resultOpen && parts.collapsible && (
        <button
          type="button"
          onClick={toggleResult}
          className="ml-5 cursor-pointer text-[10.5px] text-dim hover:text-muted"
        >
          ⏶ collapse
        </button>
      )}
    </>
  );
}

/** One transcript entry, rendered in the transcript grammar. */
export function Entry({
  entry,
  result,
  pending,
  query,
  thinkingOpen,
  toggleThinking,
  resultOpen,
  toggleResult,
  argsOpen,
  toggleArgs,
  onInspect,
  inspecting,
  isAnswer,
}: {
  entry: TranscriptEntry;
  /** The `tool_result` folded into this call's row. A call and its result are
   *  ONE row — see `mergeToolRows`. Absent for every non-tool entry, and for a
   *  call still running or whose result the reader filtered away. */
  result?: TranscriptEntry;
  /** Whether this call has no result anywhere in the transcript — it never
   *  returned. Decided by the page, which can see the whole stream; the absence
   *  of `result` above only says it is not in the current filtered view. */
  pending?: boolean;
  query: string;
  thinkingOpen: boolean;
  toggleThinking: () => void;
  resultOpen: boolean;
  toggleResult: () => void;
  /** The call's raw-argument disclosure, which is its own fold: the row's
   *  primary fold belongs to the OUTPUT, exactly as `args` is a sub-fold of a
   *  call in `crates/stella-transcript`. */
  argsOpen?: boolean;
  toggleArgs?: () => void;
  /** Absent for a row with no exchange behind it — which is how the icon
   *  stays rare enough to mean something. */
  onInspect?: () => void;
  inspecting?: boolean;
  /** Whether this is the trial's LAST response — the answer, which takes the
   *  highest visual priority on the page after the prompt. Decided by the page
   *  rather than here: only it can see whether another response follows. */
  isAnswer?: boolean;
}) {
  const body = entry.body || "";

  if (entry.kind === "stage") {
    // A section rule, not a row: the label is the stage.
    return (
      <div className="flex items-center gap-2 py-2" role="separator">
        <span className="font-semibold text-muted">{entry.title || body}</span>
        <span className="h-px flex-1 bg-line" />
      </div>
    );
  }

  if (entry.kind === "reasoning") {
    const lines = body.split("\n");
    const folded = !thinkingOpen && lines.length > THINKING_PREVIEW_LINES;
    const shown = thinkingOpen ? lines : lines.slice(0, THINKING_PREVIEW_LINES);
    return (
      <div>
        <button
          type="button"
          onClick={toggleThinking}
          className="cursor-pointer text-[10.5px] text-dim hover:text-muted"
        >
          {thinkingOpen ? "⏶" : "⏵"} thinking · {lines.length}{" "}
          {lines.length === 1 ? "line" : "lines"}
        </button>
        <div className="whitespace-pre-wrap break-words italic text-dim">
          <Highlight text={shown.join("\n")} query={query} />
        </div>
        {folded && (
          <button
            type="button"
            onClick={toggleThinking}
            className="cursor-pointer text-[10.5px] text-dim hover:text-muted"
          >
            ⋯ {lines.length - THINKING_PREVIEW_LINES} more lines
          </button>
        )}
      </div>
    );
  }

  if (entry.kind === "tool") {
    // `● name  argument`, the deck's call row. The name is soft-padded to a
    // common column so arguments line up down a run of calls — soft, not
    // hard, because a long MCP name (`mcp__github__create_pull_request`)
    // overruns the column rather than being truncated: the tool's identity
    // outranks the alignment it would cost.
    const meta = (entry.meta || {}) as Record<string, unknown>;
    const raw = typeof meta.raw === "string" ? meta.raw : "";
    const expandable = Boolean(raw) && raw !== "{}";
    // The name's colour is the call's CLASS — read/write/run/verify/repo/
    // delegate — never the arena's flat `--accent`: the class answers "what
    // kind of thing was that" from the margin before a single name is read.
    // The `●` glyph itself stays neutral: arenabench carries no brand/identity
    // hue at all (the arena scores stella as one seat among several, so its
    // the brand hue does not belong in chrome every seat is judged under), so unlike
    // the deck's `Rail::Call` — which paints the glyph `ACCENT_DEEP` — the
    // rail here is undyed and the class colour is spent on the one place a
    // reader actually scans: the tool's own name.
    const cls = toolClassOf(meta);
    // The result folded into this row. Its outcome outranks the call's class
    // for the rail glyph and colour: a failure must be findable by margin-scan
    // alone, the same precedence the deck gives an outcome over a category.
    const parts = result ? resultParts(result, resultOpen) : null;
    const failed = Boolean(parts?.isError);
    const showArgs = Boolean(argsOpen);
    return (
      <div>
        <div className="flex items-baseline gap-2">
          <span className={cn("select-none", failed ? "text-bad" : "text-dim")}>
            {failed ? "✗" : "●"}
          </span>
          <span
            className={cn(
              "min-w-[8.5rem] shrink-0 font-semibold",
              failed ? "text-bad" : TOOL_CLASS_TEXT[cls],
            )}
          >
            <Highlight text={entry.title ?? "tool"} query={query} />
          </span>
          {body && (
            <span className="min-w-0 flex-1 truncate text-muted">
              <Highlight text={body} query={query} />
            </span>
          )}
          {parts && <ResultChips parts={parts} />}
          {/* A call that never returned says so rather than looking like one
              that returned nothing — a killed run leaves calls that genuinely
              never came back, and the two must not look alike. Driven by
              `pending` (the whole transcript) rather than by the absence of
              `result` (this filtered view), because a reader who switched
              results off has not made every call on screen start running. */}
          {pending && (
            <span className="tx-chips">
              <span className="tx-chip">running…</span>
            </span>
          )}
          {onInspect && <Inspect onOpen={onInspect} open={Boolean(inspecting)} />}
          {expandable && toggleArgs && (
            <button
              type="button"
              onClick={toggleArgs}
              aria-label={showArgs ? "hide arguments" : "show arguments"}
              className="shrink-0 cursor-pointer text-[10.5px] text-dim hover:text-muted"
            >
              {showArgs ? "⏶" : "⋯"}
            </button>
          )}
        </div>
        {expandable && showArgs &&
          (() => {
            // A file-mutating tool renders as a coloured diff; everything else
            // keeps the raw argument JSON. The diff is null-safe: a call whose
            // input does not parse falls back to the plain view.
            const diff = fileDiffFromRaw(entry.title, raw);
            return diff ? (
              <FileDiffBlock hunks={diff} />
            ) : (
              <pre className="ml-5 overflow-x-auto whitespace-pre-wrap break-words text-[11px] text-dim">
                <Highlight text={raw} query={query} />
              </pre>
            );
          })()}
        {parts && (
          <ResultBody
            parts={parts}
            query={query}
            resultOpen={resultOpen}
            toggleResult={toggleResult}
          />
        )}
      </div>
    );
  }

  if (entry.kind === "tool_result") {
    // An ORPHAN result: one whose call is not on screen — it scrolled past the
    // stream's cursor, or the reader filtered calls off. A paired result never
    // reaches here; it is folded into its call's row by `mergeToolRows`, which
    // is what stops the tool's name being printed twice down the transcript.
    //
    // Here, and only here, the row keeps the tool's name: a bare column of
    // outputs naming nothing is worse than a name that appears once. `⎿` marks
    // it as the result half of a call whose header is elsewhere, and a failure
    // takes `✗` and its own colour so it is findable by margin-scan alone —
    // an outcome always outranks a category.
    const parts = resultParts(entry, resultOpen);
    const cls = toolClassOf(entry.meta || {});
    return (
      <div>
        <div className="flex items-baseline gap-2">
          <span className={cn("select-none", parts.isError ? "text-bad" : "text-dim")}>
            {parts.isError ? "✗" : "⎿"}
          </span>
          <span
            className={cn(
              "font-medium",
              parts.isError ? "text-bad" : TOOL_CLASS_TEXT[cls],
            )}
          >
            <Highlight text={entry.title ?? "tool"} query={query} />
          </span>
          <ResultChips parts={parts} />
          {onInspect && <Inspect onOpen={onInspect} open={Boolean(inspecting)} />}
        </div>
        <ResultBody
          parts={parts}
          query={query}
          resultOpen={resultOpen}
          toggleResult={toggleResult}
        />
      </div>
    );
  }
  if (entry.kind === "usage") {
    return <div className="text-[10.5px] text-dim">{usageLine(entry)}</div>;
  }

  if (entry.kind === "context_recall") {
    return (
      <RecallEntry
        entry={entry}
        query={query}
        open={resultOpen}
        toggle={toggleResult}
      />
    );
  }

  if (entry.kind === "proof") {
    // The proof rail inline, where it happened. Colour tracks what the step
    // means for the claim rather than the step's name: an oracle that failed
    // and a witness that could not be authored are both the rail *working*,
    // so they are marked, not alarmed — but a reader must be able to spot
    // them while scrolling.
    const meta = (entry.meta || {}) as Record<string, unknown>;
    const step = String(meta.step ?? "");
    const bad = step === "witness_unavailable" || step === "verification_unavailable";
    const failed = step === "oracle" && meta.passed === false;
    const good = step === "witness_authored" || (step === "oracle" && meta.passed === true);
    return (
      <div>
        <span
          className={cn(
            "text-[11px] font-semibold",
            good ? "text-ok" : bad || failed ? "text-warn" : "text-accent",
          )}
        >
          ⊢ <Highlight text={entry.title ?? "proof"} query={query} />
        </span>
        {body && (
          <div className="whitespace-pre-wrap break-words text-muted">
            <Highlight text={body} query={query} />
          </div>
        )}
      </div>
    );
  }

  if (entry.kind === "verdict" || entry.kind === "complete") {
    // A verdict is not automatically good news. `passed: true` also covers an
    // `unverifiable` outcome and a `waived` one, so the tone follows the
    // ladder rung — the field that actually separates a proven pass from an
    // unexamined claim.
    const meta = (entry.meta || {}) as Record<string, unknown>;
    const rung = String(meta.rung ?? "");
    const proven = rung === "submit_fast" || rung === "revise";
    const passed = meta.passed === true;
    return (
      <div>
        <span
          className={cn(
            "font-semibold",
            entry.kind === "complete" || proven ? "text-ok" : passed ? "text-warn" : "text-bad",
          )}
        >
          {entry.title ?? entry.kind}
          {rung && <span className="pl-2 font-normal text-dim">rung: {rung}</span>}
          {passed && !proven && (
            <span className="pl-2 font-normal text-warn">nothing deterministic behind it</span>
          )}
        </span>
        {body && (
          <div className="whitespace-pre-wrap break-words text-muted">
            <Highlight text={body} query={query} />
          </div>
        )}
      </div>
    );
  }

  if (entry.kind === "error") {
    return (
      <div className="text-bad">
        <Highlight text={(entry.title ? `${entry.title}: ` : "") + body} query={query} />
      </div>
    );
  }

  // The agent's response, with a role gutter.
  //
  // The gutter is what makes a transcript read as a conversation once the steps
  // between are folded away — the tool rows are the *work*, and without a badge
  // beside it a response is one more paragraph in a column of them. The final
  // response additionally takes the answer rule: it is the highest-priority
  // text on the page after the prompt, and a reader who scrolled past forty
  // tool calls should be able to find it by shape.
  //
  // Prose wraps at 72ch (`.tx-prose`). A response is read rather than scanned,
  // and a monospaced line much past that is measurably slower to track back to
  // the start of — which is why this is the one place on a page of full-width
  // rows that has a measure at all.
  return (
    <div className={cn("tx-role agent", isAnswer && "border-b-0")}>
      <div className="tx-rolegut">
        <span className="tx-roletag">{isAnswer ? "ANSWER" : "AGENT"}</span>
      </div>
      <div className={cn("tx-prose", isAnswer && "tx-answer")}>
        <Highlight text={body || entry.title || ""} query={query} />
      </div>
    </div>
  );
}