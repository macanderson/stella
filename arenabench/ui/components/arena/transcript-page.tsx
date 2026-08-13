"use client";

import * as React from "react";
import { api } from "@/lib/api";
import type { Cell, Snapshot, TranscriptEntry } from "@/lib/types";
import { fmtClock, fmtDuration, fmtMoney, fmtTokens } from "@/lib/format";
import { cn, seatStyle } from "@/lib/utils";
import {
  TOOL_CLASSES,
  TOOL_CLASS_LABEL,
  TOOL_CLASS_TEXT,
  toolClassOf,
} from "@/lib/tool-class";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ProofPanel } from "@/components/arena/proof-panel";

/**
 * A trial's transcript as its own page: `/transcript?match=…&task=…&seat=…`.
 *
 * This replaced the fly-out drawer. A transcript is the artifact people
 * actually study after a match — it deserves a URL you can bookmark, send,
 * and reload, plus the reading tools a drawer never had room for: kind
 * filters and full-text search across every entry.
 *
 * The rendering follows the Command Deck's transcript grammar, because a
 * reader who lives in Stella should not have to relearn the page:
 *
 * - **Stages are section rules**, not rows — the label *is* the stage.
 * - **Reasoning is quiet.** Dim, italic, collapsed to a short preview with
 *   its line count in the header; the least load-bearing text on screen
 *   never outshouts the response.
 * - **The label is coloured, the value is read.** A tool's name takes its
 *   CLASS's hue — read/write/run/verify/repo/delegate
 *   (`crates/stella-tui/src/tool_class.rs`, mirrored server-side by
 *   `arenabench.toolclass` and surfaced as `meta.tool_class`) — never the
 *   brand accent; bodies stay plain. A colour earns its place by being rare,
 *   and here it answers the first question a reader asks of any row before a
 *   single name is read: was that a look, a change, a shell, a test, a push,
 *   a hand-off?
 * - **A collapsed result shows the line that matters, not the first one.**
 *   `salient_line` (ported below) anchors the preview at the first line
 *   carrying an error/warning/failure marker rather than line 1 — a build's
 *   first line is `Checking foo v0.1.0`, and the line a reader came for is
 *   twenty lines down. A success shows one line from there; a failure shows
 *   six (`crates/stella-tui/src/render/row.rs::{salient_line,FAIL_PREVIEW}`).
 * - **The rails are the deck's**: `●` opens a call, `⎿` its result, `✗` a
 *   failed one — a distinct glyph *and* column, so a failure is findable by
 *   margin-scan alone (`crates/stella-tui/src/render/row.rs::Rail`).
 *
 * Two deliberate departures. The deck renders a call and its result as one
 * tight block that nothing can split, so the result row need not repeat the
 * tool's name. Here the kind filters can hide every call row, which would
 * leave a column of results naming nothing — so a result row carries its
 * tool's name (and its class colour), subordinate to the call's. And every
 * body on the wire here is character-capped for the live stream's sake
 * (`arenabench.transcript.TOOL_RESULT_BUDGET`) in a way the deck, reading a
 * session in memory, never has to be — the "load full transcript" button
 * fetches the same trial uncapped for the one reader who needs every byte.
 *
 * Playback matches the drawer it replaced: a finished trial replays paced by
 * each entry's own elapsed stamp, a live one streams. Search and filters are
 * reading tools, so an active search shows every match immediately — pacing
 * a search result would just hide hits behind a timer.
 */

const SPEEDS = [1, 2, 3, 6] as const;

/** Wall-clock ms one playback step waits, given a gap in transcript seconds. */
function pacedDelay(gapSeconds: number, speed: number): number {
  const ms = (gapSeconds * 1000) / speed;
  return Math.max(16, Math.min(1200, ms));
}

/** Filterable groups, in reading order. Unknown kinds always render. */
const GROUPS: Array<{ key: string; label: string; kinds: string[] }> = [
  { key: "text", label: "responses", kinds: ["text"] },
  { key: "reasoning", label: "thinking", kinds: ["reasoning"] },
  { key: "tool", label: "tools", kinds: ["tool"] },
  { key: "tool_result", label: "results", kinds: ["tool_result"] },
  { key: "usage", label: "usage", kinds: ["usage"] },
  { key: "flow", label: "stages", kinds: ["stage", "complete"] },
  // Its own group, not folded into "stages": the reason to filter a
  // transcript down to the proof rail is to read the rail, and leaving it
  // bundled with every stage rule means turning the stages off to see it.
  { key: "proof", label: "proof", kinds: ["proof", "verdict"] },
  // Likewise its own group. "What did recall put in front of the model, and
  // what did it cost?" is a question asked of a whole run at once, and it is
  // the question a transcript is opened with when a benchmark arm is being
  // argued about — filtering to it should not mean turning off the stages.
  { key: "context", label: "recall", kinds: ["context_recall"] },
];

function groupOf(kind: string): string | null {
  for (const group of GROUPS) if (group.kinds.includes(kind)) return group.key;
  return null;
}

function useTranscript(matchId: string, contestantId: string, task: string) {
  const [entries, setEntries] = React.useState<TranscriptEntry[]>([]);
  const [waiting, setWaiting] = React.useState(true);
  const [ended, setEnded] = React.useState(false);

  React.useEffect(() => {
    if (!matchId || !contestantId || !task) return;
    setEntries([]);
    setWaiting(true);
    setEnded(false);
    const bySeq = new Map<number, TranscriptEntry>();
    const url =
      `/api/matches/${encodeURIComponent(matchId)}/transcript/` +
      `${encodeURIComponent(contestantId)}/${encodeURIComponent(task)}`;
    const source = new EventSource(url);
    source.addEventListener("entries", (event) => {
      const payload = JSON.parse((event as MessageEvent).data) as {
        entries: TranscriptEntry[];
      };
      for (const entry of payload.entries) bySeq.set(entry.seq, entry);
      if (bySeq.size) {
        setWaiting(false);
        setEntries([...bySeq.values()].sort((a, b) => a.seq - b.seq));
      }
    });
    source.addEventListener("end", () => {
      setWaiting(false);
      setEnded(true);
      source.close();
    });
    return () => source.close();
  }, [matchId, contestantId, task]);

  return { entries, waiting, ended };
}

/** The match snapshot, refreshed while the match is live so ✓/✗ stay true. */
function useMatchSnapshot(matchId: string) {
  const [snapshot, setSnapshot] = React.useState<Snapshot | null>(null);
  const [error, setError] = React.useState<string | null>(null);

  React.useEffect(() => {
    if (!matchId) return;
    let cancelled = false;
    let timer: number | undefined;
    const pull = async () => {
      try {
        const snap = await api<Snapshot>(`/api/matches/${encodeURIComponent(matchId)}`);
        if (cancelled) return;
        setSnapshot(snap);
        if (snap.status === "running") {
          timer = window.setTimeout(pull, 10_000);
        }
      } catch (err) {
        if (!cancelled) setError(String(err));
      }
    };
    pull();
    return () => {
      cancelled = true;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [matchId]);

  return { snapshot, error };
}

/** Case-insensitive `<mark>` highlighting, keeping the original casing. */
function Highlight({ text, query }: { text: string; query: string }) {
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

function usageLine(entry: TranscriptEntry): string {
  const meta = (entry.meta || {}) as Record<string, unknown>;
  return (
    `${entry.title ?? "usage"} — ${meta.model || ""}` +
    ` · in ${fmtTokens(meta.tokens_in)} out ${fmtTokens(meta.tokens_out)}` +
    ` · cache ${fmtTokens(meta.cache_read)}/${fmtTokens(meta.cache_write)}` +
    ` · self-rep ${fmtMoney(meta.cost_usd)}`
  );
}

const THINKING_PREVIEW_LINES = 5;

/** Collapsed-result line budgets, matching the deck's `row.rs` exactly: a
 * successful call shows one line from the salient point, a failed one shows
 * six — enough for a compiler error with its location and caret line, or the
 * top of a panic backtrace. */
const OK_PREVIEW_LINES = 1;
const FAIL_PREVIEW_LINES = 6;

/** Split on `\n` the way Rust's `str::lines()` does: no trailing empty
 * element for text ending in a newline. Every result body downstream of
 * `salientLine` goes through this rather than a bare `.split("\n")`, so an
 * index computed on one agrees with a slice taken from the other. */
function splitLines(text: string): string[] {
  const lines = text.split("\n");
  if (text.endsWith("\n")) lines.pop();
  return lines;
}

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

/** One transcript entry, rendered in the deck's grammar. */
function Entry({
  entry,
  query,
  thinkingOpen,
  toggleThinking,
  resultOpen,
  toggleResult,
}: {
  entry: TranscriptEntry;
  query: string;
  thinkingOpen: boolean;
  toggleThinking: () => void;
  resultOpen: boolean;
  toggleResult: () => void;
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
    // gold does not belong in chrome every seat is judged under), so unlike
    // the deck's `Rail::Call` — which paints the glyph `ACCENT_DEEP` — the
    // rail here is undyed and the class colour is spent on the one place a
    // reader actually scans: the tool's own name.
    const cls = toolClassOf(meta);
    return (
      <div>
        <div className="flex items-baseline gap-2">
          <span className="select-none text-dim">●</span>
          <span
            className={cn(
              "min-w-[8.5rem] shrink-0 font-semibold",
              TOOL_CLASS_TEXT[cls],
            )}
          >
            <Highlight text={entry.title ?? "tool"} query={query} />
          </span>
          {body && (
            <span className="min-w-0 flex-1 truncate text-muted">
              <Highlight text={body} query={query} />
            </span>
          )}
          {expandable && (
            <button
              type="button"
              onClick={toggleResult}
              aria-label={resultOpen ? "hide arguments" : "show arguments"}
              className="shrink-0 cursor-pointer text-[10.5px] text-dim hover:text-muted"
            >
              {resultOpen ? "⏶" : "⋯"}
            </button>
          )}
        </div>
        {expandable && resultOpen && (
          <pre className="ml-5 overflow-x-auto whitespace-pre-wrap break-words text-[11px] text-dim">
            <Highlight text={raw} query={query} />
          </pre>
        )}
      </div>
    );
  }

  if (entry.kind === "tool_result") {
    // `⎿ name · 141ms · 12 lines`, then the body — the deck's result rail,
    // subordinate to the call above it. A failure takes `✗` and its own
    // colour so it is findable by margin-scan alone, overriding the class
    // colour below: an outcome always outranks a category, the same
    // precedence the deck gives a stage rule over its own hue.
    //
    // The name is on the row rather than left implicit in the call above,
    // which is where this page departs from the deck on purpose: the deck
    // renders a call and its result as one tight block that cannot be split,
    // while here the kind filters can hide every call row and leave a column
    // of results naming nothing. It carries the same class colour as its
    // call for the same reason — a reader who has filtered down to "results"
    // alone should not lose which kind of call produced each one.
    const meta = (entry.meta || {}) as Record<string, unknown>;
    const isError = Boolean(meta.error);
    const cls = toolClassOf(meta);
    const lines = body ? splitLines(body) : [];
    const total = lines.length;
    // The collapsed window anchors on the SALIENT line, not line 1 — see
    // `salientLine`'s doc comment — and its size is the deck's own budget: a
    // success shows a single line, a failure shows six.
    const budget = isError ? FAIL_PREVIEW_LINES : OK_PREVIEW_LINES;
    const anchor = total > 0 ? salientLine(body ?? "") : 0;
    const collapsedShown = lines.slice(anchor, anchor + budget);
    const shown = resultOpen ? lines : collapsedShown;
    const hidden = resultOpen ? 0 : total - collapsedShown.length;
    const metrics = [
      meta.duration_ms != null ? fmtDuration(meta.duration_ms) : null,
      total > 1 ? `${total} lines` : null,
      // ⚡ marks a speculated result: its duration overlapped the model's own
      // streaming instead of following it, so the number is not latency the
      // run actually spent waiting.
      meta.speculated ? "⚡ speculated" : null,
      // The protocol grew a `ToolOutput` arm this page predates. Say so
      // rather than presenting the JSON fallback as if it were output.
      meta.unrecognized ? "unrecognized output shape" : null,
    ].filter(Boolean);
    return (
      <div>
        <div className="flex items-baseline gap-2">
          <span className={cn("select-none", isError ? "text-bad" : "text-dim")}>
            {isError ? "✗" : "⎿"}
          </span>
          <span
            className={cn("font-medium", isError ? "text-bad" : TOOL_CLASS_TEXT[cls])}
          >
            <Highlight text={entry.title ?? "tool"} query={query} />
          </span>
          {metrics.length > 0 && (
            <span className="text-[10.5px] text-dim">· {metrics.join(" · ")}</span>
          )}
        </div>
        {shown.length > 0 && (
          <pre
            className={cn(
              "ml-5 overflow-x-auto whitespace-pre-wrap break-words",
              isError ? "text-bad/90" : "text-muted",
            )}
          >
            <Highlight text={shown.join("\n")} query={query} />
          </pre>
        )}
        {/* The deck only earns this row for a failure — a successful result
            already states its size in the metric column above. A mouse-driven
            page has no ctrl+o, though, so a folded SUCCESS still gets a quiet
            way to see the rest; it just does not compete for attention the
            way the failure's does. */}
        {!resultOpen && hidden > 0 && (
          <button
            type="button"
            onClick={toggleResult}
            className={cn(
              "ml-5 cursor-pointer text-[10.5px] hover:text-muted",
              isError ? "text-dim" : "text-dim/70",
            )}
          >
            ⋯ {hidden} more {hidden === 1 ? "line" : "lines"}
          </button>
        )}
        {resultOpen && total > budget && (
          <button
            type="button"
            onClick={toggleResult}
            className="ml-5 cursor-pointer text-[10.5px] text-dim hover:text-muted"
          >
            ⏶ collapse
          </button>
        )}
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

  // The agent's response — plain foreground, full width.
  return (
    <div className="whitespace-pre-wrap break-words text-foreground">
      <Highlight text={body || entry.title || ""} query={query} />
    </div>
  );
}

function readParams(): { match: string; task: string; seat: string } {
  const params = new URLSearchParams(window.location.search);
  return {
    match: params.get("match") ?? "",
    task: params.get("task") ?? "",
    seat: params.get("seat") ?? "",
  };
}

function writeParams(update: Partial<{ task: string; seat: string }>): void {
  const url = new URL(window.location.href);
  for (const [key, value] of Object.entries(update)) {
    if (value) url.searchParams.set(key, value);
  }
  window.history.replaceState(null, "", url);
}

export function TranscriptPage() {
  // Read once on mount: this is a static export, so the URL is the only
  // input. Same pattern as the app shell's match restore.
  const [params, setParams] = React.useState<{
    match: string;
    task: string;
    seat: string;
  } | null>(null);
  React.useEffect(() => setParams(readParams()), []);

  if (params === null) return null;
  if (!params.match) {
    return (
      <div className="mx-auto max-w-3xl px-4 py-10 font-mono text-[13px]">
        <p className="text-bad">no match in the URL.</p>
        <a href="/" className="text-accent underline">
          ← back to the arena
        </a>
      </div>
    );
  }
  return <TranscriptView matchId={params.match} initial={params} />;
}

function TranscriptView({
  matchId,
  initial,
}: {
  matchId: string;
  initial: { task: string; seat: string };
}) {
  const { snapshot, error } = useMatchSnapshot(matchId);

  const tasks = React.useMemo(
    () => (snapshot ? snapshot.rows.map((row) => row.task) : []),
    [snapshot],
  );
  const seats = snapshot?.contestants ?? [];

  const [task, setTask] = React.useState(initial.task);
  const [seatId, setSeatId] = React.useState(initial.seat);

  // Fill unset selections once the snapshot names the options.
  React.useEffect(() => {
    if (!snapshot) return;
    if (!task && tasks.length) setTask(tasks[0]);
    if (!seatId && seats.length) setSeatId(seats[0].id);
  }, [snapshot, task, seatId, tasks, seats]);

  React.useEffect(() => {
    if (task || seatId) writeParams({ task, seat: seatId });
    if (task) document.title = `transcript · ${task}`;
  }, [task, seatId]);

  const live = snapshot?.status === "running";
  const { entries: streamedEntries, waiting, ended } = useTranscript(matchId, seatId, task);

  // -- the full, uncapped transcript, fetched on demand --------------------
  // The SSE channel above character-caps a tool call/result body
  // (`arenabench.transcript.TOOL_RESULT_BUDGET`/`TOOL_INPUT_BUDGET`) so a
  // match running for hours stays cheap to stream. That cap is
  // unrecoverable client-side — the elided bytes were never sent — so a
  // reader who has decided this one trial is worth the extra bytes gets a
  // plain request-response fetch of the same file from byte zero with both
  // caps disabled, wholesale replacing the capped array rather than
  // patching it (every field but the body is identical either way).
  const [fullEntries, setFullEntries] = React.useState<TranscriptEntry[] | null>(null);
  const [loadingFull, setLoadingFull] = React.useState(false);
  const [fullError, setFullError] = React.useState<string | null>(null);
  React.useEffect(() => {
    setFullEntries(null);
    setFullError(null);
  }, [matchId, seatId, task]);
  const loadFullTranscript = React.useCallback(async () => {
    setLoadingFull(true);
    setFullError(null);
    try {
      const payload = await api<{ entries: TranscriptEntry[] }>(
        `/api/matches/${encodeURIComponent(matchId)}/transcript-full/` +
          `${encodeURIComponent(seatId)}/${encodeURIComponent(task)}`,
      );
      setFullEntries(payload.entries);
    } catch (err) {
      setFullError(String(err));
    } finally {
      setLoadingFull(false);
    }
  }, [matchId, seatId, task]);
  const entries = fullEntries ?? streamedEntries;

  // -- reading tools ------------------------------------------------------
  const [enabled, setEnabled] = React.useState<Record<string, boolean>>(() =>
    Object.fromEntries(GROUPS.map((group) => [group.key, true])),
  );
  // Tools explicitly excluded by name — empty means no filter is active.
  // Unlike `enabled` (whole kind-groups, seeded once), this has to grow with
  // the transcript: a trial's tool names are not known until entries arrive.
  const [disabledTools, setDisabledTools] = React.useState<Set<string>>(
    () => new Set(),
  );
  const toolIndex = React.useMemo(() => {
    const classByName = new Map<string, ReturnType<typeof toolClassOf>>();
    for (const entry of entries) {
      if (entry.kind === "tool" && entry.title && !classByName.has(entry.title)) {
        classByName.set(entry.title, toolClassOf(entry.meta));
      }
    }
    const names = [...classByName.keys()].sort((a, b) => a.localeCompare(b));
    return { names, classByName };
  }, [entries]);
  const toggleTool = React.useCallback((name: string) => {
    setDisabledTools((state) => {
      const next = new Set(state);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  }, []);
  const [query, setQuery] = React.useState("");
  const searching = query.trim().length > 0;

  // Per-entry fold overrides ride on top of the global thinking default.
  const [thinkingDefault, setThinkingDefault] = React.useState(false);
  const [thinkingOverrides, setThinkingOverrides] = React.useState<
    Record<number, boolean>
  >({});
  const [openResults, setOpenResults] = React.useState<Record<number, boolean>>({});
  React.useEffect(() => {
    setThinkingOverrides({});
    setOpenResults({});
  }, [seatId, task]);

  // Open by default, because it is the answer to the question that brought
  // most readers here — the transcript below is the supporting detail.
  const [proofOpen, setProofOpen] = React.useState(true);

  const visible = React.useMemo(() => {
    const needle = query.trim().toLowerCase();
    return entries.filter((entry) => {
      const group = groupOf(entry.kind);
      if (group !== null && !enabled[group]) return false;
      // A call and its result are both named by the tool's own name — the
      // one identifier both kinds share — so one filter set hides both
      // halves of a call the reader asked not to see.
      if (
        (entry.kind === "tool" || entry.kind === "tool_result") &&
        entry.title &&
        disabledTools.has(entry.title)
      ) {
        return false;
      }
      if (!needle) return true;
      return (
        (entry.body ?? "").toLowerCase().includes(needle) ||
        (entry.title ?? "").toLowerCase().includes(needle) ||
        (entry.kind === "usage" && usageLine(entry).toLowerCase().includes(needle))
      );
    });
  }, [entries, enabled, disabledTools, query]);

  // -- playback (paced replay for a finished trial) -----------------------
  const [playing, setPlaying] = React.useState(true);
  const [speed, setSpeed] = React.useState<number>(1);
  const [revealed, setRevealed] = React.useState(0);
  const historical = ended && !live;
  // Search is a reading tool: it answers over the whole transcript, never
  // behind the replay timer.
  const paced = historical && !searching;

  React.useEffect(() => {
    setRevealed(paced ? 0 : visible.length);
    setPlaying(true);
  }, [seatId, task, paced, visible.length]);

  React.useEffect(() => {
    if (!paced || !playing) return;
    if (revealed >= visible.length) return;
    const previous = visible[revealed - 1]?.t ?? 0;
    const current = visible[revealed]?.t ?? previous;
    const timer = window.setTimeout(
      () => setRevealed((n) => Math.min(n + 1, visible.length)),
      pacedDelay(Math.max(0, current - previous), speed),
    );
    return () => window.clearTimeout(timer);
  }, [paced, playing, revealed, visible, speed]);

  const shown = paced ? visible.slice(0, revealed) : visible;
  const done = paced && revealed >= visible.length && visible.length > 0;

  const feedRef = React.useRef<HTMLDivElement>(null);
  React.useEffect(() => {
    if (playing && !searching && feedRef.current) {
      feedRef.current.scrollTop = feedRef.current.scrollHeight;
    }
  }, [shown.length, playing, searching]);

  const seat = seats.find((option) => option.id === seatId);
  const row = snapshot?.rows.find((r) => r.task === task);
  const cell: Cell | null | undefined = row?.cells?.[seatId];

  const replay = React.useCallback(() => {
    setRevealed(0);
    setPlaying(true);
  }, []);

  return (
    <div
      className="mx-auto flex h-dvh max-w-[1100px] flex-col px-4 py-3"
      style={seatStyle(seat?.color)}
    >
      <header className="flex flex-wrap items-baseline gap-x-3 gap-y-1 border-b border-line pb-2.5">
        <a
          href={`/?match=${encodeURIComponent(matchId)}`}
          className="font-mono text-[12px] text-accent hover:underline"
        >
          ← arena
        </a>
        <span className="font-mono text-[13px] font-semibold">
          {snapshot?.match.name ?? matchId}
        </span>
        <span className="text-[11px] text-dim">
          {snapshot?.dataset.title ?? ""} · {live ? "live" : (snapshot?.status ?? "…")}
        </span>
        {error && <span className="text-[11px] text-bad">{error}</span>}
      </header>

      {/* Which trial: task × seat. Both write back to the URL, so the page
          you are looking at is always the page you can share. */}
      <div className="flex flex-wrap items-center gap-2 border-b border-line py-2">
        <select
          value={task}
          onChange={(event) => setTask(event.target.value)}
          className=" border border-line bg-panel px-2 py-1.5 font-mono text-[12px]"
          aria-label="task"
        >
          {tasks.map((option) => (
            <option key={option} value={option}>
              {option}
            </option>
          ))}
        </select>
        {seats.map((option) => (
          <button
            key={option.id}
            type="button"
            onClick={() => setSeatId(option.id)}
            style={seatStyle(option.color)}
            className={cn(
              "flex cursor-pointer items-center gap-1.5 px-2.5 py-1.5 text-[12px]",
              option.id === seatId
                ? "bg-(--seat)/16 font-semibold text-(--seat-fg)"
                : "text-muted hover:bg-panel",
            )}
          >
            <span className="size-[7px] bg-(--seat)" />
            {option.name}
            {row?.cells?.[option.id]?.resolved === true && <span className="text-ok">✓</span>}
            {row?.cells?.[option.id]?.resolved === false && <span className="text-bad">✗</span>}
          </button>
        ))}
        {cell && (
          <span className="ml-auto flex flex-wrap gap-3 font-mono text-[10.5px] text-muted">
            <span>steps <b className="font-medium text-foreground">{cell.steps}</b></span>
            <span>tools <b className="font-medium text-foreground">{cell.tools}</b></span>
            <span>in <b className="font-medium text-foreground">{fmtTokens(cell.tokens_in)}</b></span>
            <span>out <b className="font-medium text-foreground">{fmtTokens(cell.tokens_out)}</b></span>
            <span>cost <b className="font-medium text-foreground">{fmtMoney(cell.priced_cost)}</b></span>
            <span>{fmtClock(cell.clock_time)}</span>
          </span>
        )}
      </div>

      {/* Reading tools: search, kind filters, playback. */}
      <div className="flex flex-wrap items-center gap-2 border-b border-line py-2">
        <Input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape") setQuery("");
          }}
          placeholder="search the transcript…"
          aria-label="search the transcript"
          className="h-8 w-[240px] font-mono text-[12px]"
        />
        {searching && (
          <span className="font-mono text-[10.5px] text-dim">
            {visible.length} of {entries.length} entries
          </span>
        )}
        <div className="flex flex-wrap items-center gap-1">
          {GROUPS.map((group) => (
            <button
              key={group.key}
              type="button"
              onClick={() =>
                setEnabled((state) => ({ ...state, [group.key]: !state[group.key] }))
              }
              className={cn(
                "cursor-pointer px-2 py-1 font-mono text-[11px]",
                enabled[group.key]
                  ? "bg-accent/15 text-accent"
                  : "text-dim line-through hover:bg-panel",
              )}
            >
              {group.label}
            </button>
          ))}
        </div>
        <div className="ml-auto flex items-center gap-2">
          {/* Every tool call/result body on the streamed channel is
              character-capped for the live feed's sake; this fetches the same
              trial from byte zero with the cap disabled. Bytes elided on the
              wire cannot be recovered by any amount of client-side "show
              more", so this is the only way back to the whole payload. */}
          <Button
            variant="ghost"
            size="sm"
            disabled={loadingFull || fullEntries !== null}
            onClick={loadFullTranscript}
          >
            {fullEntries !== null
              ? "full transcript loaded ✓"
              : loadingFull
                ? "loading full transcript…"
                : "load full transcript"}
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              setThinkingDefault((value) => !value);
              setThinkingOverrides({});
            }}
          >
            {thinkingDefault ? "collapse thinking" : "expand thinking"}
          </Button>
          {paced || done ? (
            <>
              <Button
                variant="ghost"
                size="sm"
                onClick={done ? replay : () => setPlaying((value) => !value)}
              >
                {done ? "▶ replay" : playing ? "❙❙ pause" : "▶ play"}
              </Button>
              <div className="flex items-center gap-1">
                {SPEEDS.map((option) => (
                  <button
                    key={option}
                    type="button"
                    onClick={() => setSpeed(option)}
                    className={cn(
                      "cursor-pointer px-2 py-1 font-mono text-[11px]",
                      speed === option
                        ? "bg-accent font-semibold text-on-accent"
                        : "text-muted hover:bg-panel",
                    )}
                  >
                    {option}×
                  </button>
                ))}
              </div>
              <span className="font-mono text-[10.5px] text-dim">
                {Math.min(revealed, visible.length)} / {visible.length}
              </span>
            </>
          ) : live ? (
            <Button variant="ghost" size="sm" onClick={() => setPlaying((v) => !v)}>
              {playing ? "❙❙ pause" : "▶ resume"}
            </Button>
          ) : null}
        </div>
      </div>

      {fullError && (
        <div className="border-b border-line py-1.5 font-mono text-[11px] text-bad">
          full transcript failed to load: {fullError}
        </div>
      )}

      {/* Filter by tool name, and the class legend that explains the colour
          each name is painted in. One row: the chips are the working filter,
          the legend on the right is the key a first-time reader needs to
          learn it. Only rendered once the trial has produced at least one
          tool call — an empty filter row above an empty transcript answers
          a question nobody asked yet. */}
      {toolIndex.names.length > 0 && (
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5 border-b border-line py-2">
          <div className="flex flex-wrap items-center gap-1">
            {toolIndex.names.map((name) => {
              const cls = toolIndex.classByName.get(name) ?? "execute";
              const off = disabledTools.has(name);
              return (
                <button
                  key={name}
                  type="button"
                  onClick={() => toggleTool(name)}
                  aria-pressed={!off}
                  className={cn(
                    "cursor-pointer px-2 py-1 font-mono text-[11px]",
                    off
                      ? "text-dim line-through hover:bg-panel"
                      : ["bg-current/10", TOOL_CLASS_TEXT[cls]],
                  )}
                >
                  {name}
                </button>
              );
            })}
            {disabledTools.size > 0 && (
              <button
                type="button"
                onClick={() => setDisabledTools(new Set())}
                className="cursor-pointer px-2 py-1 font-mono text-[11px] text-dim underline hover:text-muted"
              >
                reset
              </button>
            )}
          </div>
          <div className="ml-auto flex flex-wrap items-center gap-x-2.5 gap-y-1 font-mono text-[10px] text-dim">
            {TOOL_CLASSES.map((cls) => (
              <span key={cls} className="flex items-center gap-1">
                <span className={TOOL_CLASS_TEXT[cls]}>●</span>
                {TOOL_CLASS_LABEL[cls]}
              </span>
            ))}
          </div>
        </div>
      )}

      {/* The proof rail, above the transcript it summarises. A verdict says
          whether the trial passed; only this says what claims that. */}
      <div className="border-b border-line py-2">
        <button
          type="button"
          onClick={() => setProofOpen((value) => !value)}
          className="cursor-pointer pb-1.5 font-mono text-[11px] text-dim hover:text-muted"
        >
          {proofOpen ? "⏶" : "⏵"} proof
        </button>
        {proofOpen && <ProofPanel proof={cell?.proof} />}
      </div>

      <div
        ref={feedRef}
        className="flex-1 overflow-y-auto py-2.5 font-mono text-[11.5px] leading-[1.62]"
      >
        {waiting && shown.length === 0 ? (
          <div className="py-1 text-accent">waiting for the trial to start…</div>
        ) : shown.length === 0 ? (
          <div className="py-1 text-dim">
            {searching ? "nothing matches this search." : "no transcript entries for this trial."}
          </div>
        ) : (
          shown.map((entry) => (
            <div key={entry.seq} className="flex gap-[9px] py-0.5">
              <span className="w-[46px] flex-none pt-0.5 text-[10px] text-dim">
                {fmtClock(entry.t)}
              </span>
              <div className="min-w-0 flex-1">
                <Entry
                  entry={entry}
                  query={query.trim()}
                  thinkingOpen={thinkingOverrides[entry.seq] ?? thinkingDefault}
                  toggleThinking={() =>
                    setThinkingOverrides((state) => ({
                      ...state,
                      [entry.seq]: !(state[entry.seq] ?? thinkingDefault),
                    }))
                  }
                  resultOpen={openResults[entry.seq] ?? false}
                  toggleResult={() =>
                    setOpenResults((state) => ({
                      ...state,
                      [entry.seq]: !state[entry.seq],
                    }))
                  }
                />
              </div>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
