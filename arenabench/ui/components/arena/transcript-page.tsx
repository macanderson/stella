"use client";

import * as React from "react";
import { api } from "@/lib/api";
import type { Cell, Snapshot, TranscriptEntry } from "@/lib/types";
import { fmtClock, fmtMoney, fmtTokens } from "@/lib/format";
import { cn, seatStyle } from "@/lib/utils";
import {
  TOOL_CLASSES,
  TOOL_CLASS_LABEL,
  TOOL_CLASS_TEXT,
  toolClassOf,
} from "@/lib/tool-class";
import {
  erroredSeqs,
  indexByCallId,
  rawExchange,
  type RawExchange,
} from "@/lib/transcript-view";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { ProofPanel } from "@/components/arena/proof-panel";
import { Entry, usageLine } from "@/components/arena/transcript/entry";
import { RawDrawer } from "@/components/arena/transcript/raw-drawer";

/**
 * A trial's transcript as its own page: `/transcript?match=…&task=…&seat=…`.
 *
 * This replaced a fly-out drawer. A transcript is the artifact people actually
 * study after a match — it deserves a URL you can bookmark, send and reload,
 * plus the reading tools a drawer never had room for.
 *
 * This file is the page: which trial, the reading tools, playback, and the
 * shell. How one entry is drawn is `transcript/entry.tsx`, in the grammar
 * `crates/stella-transcript` renders for the Command Deck and the Observatory.
 *
 * ## The reading tools, and why each is separate
 *
 * - **Search** answers over the whole transcript and never behind the replay
 *   timer: pacing a search result would hide hits behind a clock.
 * - **Kind groups** and **tool names** narrow by category and by name.
 * - **Errors only** is its own control rather than a kind group, because a
 *   failure is not a kind — it is a *property* of a result — and it drags its
 *   own call back in by `call_id`. A failed result read alone names the tool
 *   and the message and says nothing about what was asked, which is the first
 *   thing a reader wants and the only thing that makes the failure actionable
 *   (`lib/transcript-view.ts::erroredSeqs`).
 *
 * ## Playback, and what "load full transcript" means
 *
 * A finished trial **replays by default**, paced by each entry's own elapsed
 * stamp; a live one streams. That is the right default and it stays.
 *
 * "Load full transcript" is a different request, and it used to be answered as
 * if it were the same one. Every tool body on the SSE channel is
 * character-capped for the live feed's sake
 * (`arenabench.transcript.TOOL_RESULT_BUDGET`), and the elided bytes were never
 * sent — so no client-side "show more" can recover them, and this button
 * refetches the trial from byte zero with the caps off. But the refetch
 * replaced the entry array, `visible.length` changed, and the pacing effect
 * reset `revealed` to 0 — so asking for the whole transcript started the replay
 * over from the beginning, which is the opposite of what the words say and of
 * why anyone clicks it. Loading the full transcript now reveals all of it at
 * once (`skipPacing`), and replay is still one click away.
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
  // Loading the whole transcript is a request to *see* the whole transcript.
  // It used to be answered by replacing the entries and letting the pacing
  // effect start the replay over from zero — so the button whose words are
  // "load full transcript" showed the reader the first entry again. Replay
  // stays the default for a finished trial; this is the one thing that turns
  // it off, and the replay control turns it back on.
  const [skipPacing, setSkipPacing] = React.useState(false);
  React.useEffect(() => {
    setFullEntries(null);
    setFullError(null);
    setSkipPacing(false);
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
      setSkipPacing(true);
    } catch (err) {
      // Deliberately not set on the failure path: a fetch that did not arrive
      // must leave the reader with the paced transcript they already had,
      // rather than jumping them to the end of a stale one.
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

  // Errors only. Computed over `entries` rather than inside the filter, because
  // keeping a failed result means also keeping the *call* that produced it —
  // a decision about the whole transcript that no per-entry predicate can make.
  const [errorsOnly, setErrorsOnly] = React.useState(false);
  const errored = React.useMemo(() => erroredSeqs(entries), [entries]);

  const visible = React.useMemo(() => {
    const needle = query.trim().toLowerCase();
    return entries.filter((entry) => {
      if (errorsOnly && !errored.has(entry.seq)) return false;
      const group = groupOf(entry.kind);
      // The kind groups do not get to veto an errors-only view: a reader who
      // asked for failures and has "results" switched off from ten minutes ago
      // means "show me the failures", not "show me nothing".
      if (!errorsOnly && group !== null && !enabled[group]) return false;
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
      // A result's inline diff is part of what the entry shows, so the
      // search that promises "every entry" must read it too — it lives in
      // `meta`, not `body`, precisely so the body cap can never garble it.
      const diff = entry.meta?.diff;
      return (
        (entry.body ?? "").toLowerCase().includes(needle) ||
        (entry.title ?? "").toLowerCase().includes(needle) ||
        (typeof diff === "string" && diff.toLowerCase().includes(needle)) ||
        (entry.kind === "usage" && usageLine(entry).toLowerCase().includes(needle))
      );
    });
  }, [entries, enabled, disabledTools, query, errorsOnly, errored]);

  // -- playback (paced replay for a finished trial) -----------------------
  const [playing, setPlaying] = React.useState(true);
  const [speed, setSpeed] = React.useState<number>(1);
  const [revealed, setRevealed] = React.useState(0);
  const historical = ended && !live;
  // Three things turn pacing off, and each is a reader saying "show me all of
  // it": a live trial (there is nothing to replay), an active search (pacing a
  // search result hides hits behind a clock), and an explicit full load.
  const paced = historical && !searching && !skipPacing;

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
    // Also clears the full-load skip, so "replay" means replay even for a
    // reader who jumped to the end first. The loaded entries stay loaded —
    // they are strictly better data, and refetching them would be a second
    // request for bytes already in hand.
    setSkipPacing(false);
    setRevealed(0);
    setPlaying(true);
  }, []);

  // -- the raw exchange drawer ---------------------------------------------
  // Held by `call_id` rather than by `seq`, so the drawer survives a reload
  // that renumbers nothing but re-sorts, and so opening it from a call and
  // from its result is the same act on the same object.
  const callIndex = React.useMemo(() => indexByCallId(entries), [entries]);
  // The trial's last response — the answer. Taken from `entries` rather than
  // from what is on screen, so filtering or pacing cannot promote an earlier
  // response into the answer's rule and tell the reader the run ended there.
  const answerSeq = React.useMemo(() => {
    let last: number | null = null;
    for (const entry of entries) if (entry.kind === "text") last = entry.seq;
    return last;
  }, [entries]);
  const [inspecting, setInspecting] = React.useState<string | null>(null);
  React.useEffect(() => setInspecting(null), [seatId, task]);
  const exchange: RawExchange | null = React.useMemo(() => {
    if (!inspecting) return null;
    const pair = callIndex.get(inspecting);
    const anchor = pair?.call ?? pair?.result;
    return anchor ? rawExchange(anchor, callIndex) : null;
  }, [inspecting, callIndex]);

  return (
    <div
      className="tx mx-auto flex h-dvh max-w-[1100px] flex-col px-4 py-3"
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
        {/* Its own control, apart from the kind groups above, because a failure
            is not a KIND — it is a property of a result — and because this one
            reaches across rows: keeping a failed result also keeps the call
            that produced it. Reads as a state the page is in rather than as one
            more chip in the row, since it overrides the chips beside it. */}
        <button
          type="button"
          onClick={() => setErrorsOnly((value) => !value)}
          aria-pressed={errorsOnly}
          className={cn(
            "cursor-pointer px-2 py-1 font-mono text-[11px]",
            errorsOnly
              ? "bg-bad text-on-seat"
              : errored.size > 0
                ? "text-bad hover:bg-panel"
                : "text-dim hover:bg-panel",
          )}
          title={
            errored.size > 0
              ? "show only failed calls, with the calls that produced them"
              : "no failures in this trial"
          }
        >
          ✗ errors{errored.size > 0 ? ` (${errored.size})` : ""}
        </button>
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

      {/* The frame. A transcript is one artifact, and the runbar says whose it
          is — a reader arriving from a shared link has a seat, a model and a
          verdict in view before scrolling. Same shape the Command Deck and the
          Observatory put around the same events. */}
      <div className="tx-frame mt-2 flex min-h-0 flex-1 flex-col">
        <div className="tx-runbar">
          <span className="name">{task || "—"}</span>
          <span className="meta">
            {seat?.name ?? seatId}
            {seat?.engine_label ? ` · ${seat.engine_label}` : ""}
            {cell ? ` · ${fmtClock(cell.clock_time)}` : ""}
          </span>
          <span className="tx-chips">
            {errorsOnly && <span className="tx-chip err">errors only</span>}
            {skipPacing && <span className="tx-chip">full · unpaced</span>}
            {cell?.resolved === true && <span className="tx-chip ok">✓ solved</span>}
            {cell?.resolved === false && <span className="tx-chip err">✗ failed</span>}
          </span>
        </div>

        <div ref={feedRef} className="min-h-0 flex-1 overflow-y-auto py-2">
          {waiting && shown.length === 0 ? (
            <div className="px-3 py-1 text-accent">waiting for the trial to start…</div>
          ) : shown.length === 0 ? (
            <div className="px-3 py-1 text-dim">
              {errorsOnly
                ? "no failed calls in this trial."
                : searching
                  ? "nothing matches this search."
                  : "no transcript entries for this trial."}
            </div>
          ) : (
            shown.map((entry) => {
              // The icon is offered only where there is genuinely an exchange
              // behind the row, which is what keeps it rare enough to notice.
              const callId =
                typeof entry.meta?.call_id === "string" ? entry.meta.call_id : "";
              const hasExchange =
                Boolean(callId) && rawExchange(entry, callIndex) !== null;
              return (
                <div key={entry.seq} className="tx-row py-0.5">
                  <span className="tx-clock">{fmtClock(entry.t)}</span>
                  {/* The spine. A dot in the class's own hue, so a scroll of
                      hundreds of rows can be read by margin alone; filled for
                      a mutation, hollow for everything else, because a write
                      is the one kind whose position a reader reconstructs a
                      run from. */}
                  <span className="tx-node">
                    {(entry.kind === "tool" || entry.kind === "tool_result") && (
                      <span
                        className={cn(
                          "tx-dot",
                          entry.meta?.error
                            ? "text-bad"
                            : TOOL_CLASS_TEXT[toolClassOf(entry.meta)],
                          toolClassOf(entry.meta) === "mutate" && "solid",
                        )}
                      />
                    )}
                  </span>
                  <div className="min-w-0">
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
                      onInspect={
                        hasExchange
                          ? () =>
                              setInspecting((open) => (open === callId ? null : callId))
                          : undefined
                      }
                      inspecting={Boolean(callId) && inspecting === callId}
                      isAnswer={entry.seq === answerSeq}
                    />
                  </div>
                </div>
              );
            })
          )}
        </div>

        {/* Along the bottom, inside the frame, so the transcript stays visible
            and scrollable while a payload is being read against it. */}
        {exchange && (
          <RawDrawer exchange={exchange} onClose={() => setInspecting(null)} />
        )}
      </div>
    </div>
  );
}
