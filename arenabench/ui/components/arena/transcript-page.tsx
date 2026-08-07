"use client";

import * as React from "react";
import { api } from "@/lib/api";
import type { Cell, Snapshot, TranscriptEntry } from "@/lib/types";
import { fmtClock, fmtMoney, fmtTokens } from "@/lib/format";
import { cn, seatStyle } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

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
 * - **The label is coloured, the value is read.** Tool names take the
 *   accent; bodies stay plain. A colour earns its place by being rare.
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
  { key: "flow", label: "stages", kinds: ["stage", "proof", "verdict", "complete"] },
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
      <mark key={key++} className="rounded-[2px] bg-gold/35 text-inherit">
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

const RESULT_PREVIEW_LINES = 12;
const THINKING_PREVIEW_LINES = 5;

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
        <span className="font-semibold lowercase text-muted">{entry.title || body}</span>
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
    return (
      <div>
        <span className="text-acc-cyan">
          ⚙ <Highlight text={entry.title ?? "tool"} query={query} />
        </span>
        {body && (
          <div className="whitespace-pre-wrap break-words text-muted">
            <Highlight text={body} query={query} />
          </div>
        )}
      </div>
    );
  }

  if (entry.kind === "tool_result") {
    const isError = Boolean((entry.meta as Record<string, unknown> | undefined)?.error);
    const lines = body.split("\n");
    const folded = !resultOpen && lines.length > RESULT_PREVIEW_LINES;
    const shown = resultOpen ? lines : lines.slice(0, RESULT_PREVIEW_LINES);
    return (
      <div>
        <span className={cn("text-[10.5px]", isError ? "text-bad" : "text-dim")}>
          ↳ {isError ? "error" : "result"}
        </span>
        <div
          className={cn(
            "whitespace-pre-wrap break-words",
            isError ? "text-bad/90" : "text-muted",
          )}
        >
          <Highlight text={shown.join("\n")} query={query} />
        </div>
        {folded && (
          <button
            type="button"
            onClick={toggleResult}
            className="cursor-pointer text-[10.5px] text-dim hover:text-muted"
          >
            ⋯ {lines.length - RESULT_PREVIEW_LINES} more lines
          </button>
        )}
        {resultOpen && lines.length > RESULT_PREVIEW_LINES && (
          <button
            type="button"
            onClick={toggleResult}
            className="cursor-pointer text-[10.5px] text-dim hover:text-muted"
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

  if (entry.kind === "verdict" || entry.kind === "complete") {
    return (
      <div className="font-semibold text-ok">
        {(entry.title ?? entry.kind) + (body ? ` — ${body}` : "")}
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
  const { entries, waiting, ended } = useTranscript(matchId, seatId, task);

  // -- reading tools ------------------------------------------------------
  const [enabled, setEnabled] = React.useState<Record<string, boolean>>(() =>
    Object.fromEntries(GROUPS.map((group) => [group.key, true])),
  );
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

  const visible = React.useMemo(() => {
    const needle = query.trim().toLowerCase();
    return entries.filter((entry) => {
      const group = groupOf(entry.kind);
      if (group !== null && !enabled[group]) return false;
      if (!needle) return true;
      return (
        (entry.body ?? "").toLowerCase().includes(needle) ||
        (entry.title ?? "").toLowerCase().includes(needle) ||
        (entry.kind === "usage" && usageLine(entry).toLowerCase().includes(needle))
      );
    });
  }, [entries, enabled, query]);

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
          className="rounded-[7px] border border-line bg-panel px-2 py-1.5 font-mono text-[12px]"
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
              "flex cursor-pointer items-center gap-1.5 rounded-[7px] px-2.5 py-1.5 text-[12px]",
              option.id === seatId
                ? "bg-(--seat)/16 font-semibold text-(--seat-fg)"
                : "text-muted hover:bg-panel",
            )}
          >
            <span className="size-[7px] rounded-full bg-(--seat)" />
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
                "cursor-pointer rounded-[6px] px-2 py-1 font-mono text-[11px]",
                enabled[group.key]
                  ? "bg-gold/15 text-accent"
                  : "text-dim line-through hover:bg-panel",
              )}
            >
              {group.label}
            </button>
          ))}
        </div>
        <div className="ml-auto flex items-center gap-2">
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
                      "cursor-pointer rounded-[6px] px-2 py-1 font-mono text-[11px]",
                      speed === option
                        ? "bg-gold font-semibold text-ink"
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
