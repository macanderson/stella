"use client";

import * as React from "react";
import type { Cell, ContestantSnap, TranscriptEntry } from "@/lib/types";
import { fmtClock, fmtMoney, fmtTokens } from "@/lib/format";
import { cn, seatStyle } from "@/lib/utils";
import { Button } from "@/components/ui/button";

/**
 * One task's transcript, in a drawer that flies out over the table.
 *
 * **One transcript at a time, by design.** The old side-by-side lanes split a
 * narrow column two ways and truncated both; a transcript is prose and wants
 * width. So the drawer shows a single seat and lets you switch — which is
 * also how you actually read these, one agent at a time, comparing from
 * memory of the table you just left.
 *
 * # Live and historical are the same reader, with one difference
 *
 * A finished trial's transcript arrives all at once (the server replays the
 * backlog on connect), so "watching" it means **pacing the reveal ourselves**
 * against each entry's own `t` — the elapsed seconds it was emitted at. That
 * makes historical playback a client concern and needs nothing from the
 * server, which is why speed control exists only here.
 *
 * - **Historical**: play/pause, replay once it has run out, and 1×/2×/3×/6×.
 *   Seeking is the scrollbar.
 * - **Live**: pause and resume only. There is no speed for a stream that has
 *   not happened yet, and resuming jumps to the tail — a live reader who
 *   pauses wants to stop the scroll, and on resume wants *now*, not the
 *   middle of a backlog they already scrolled past.
 */

const SPEEDS = [1, 2, 3, 6] as const;

/** Wall-clock ms one playback step waits, given a gap in transcript seconds. */
function pacedDelay(gapSeconds: number, speed: number): number {
  // Clamped so a trial with a four-minute think does not stall playback, and
  // a burst of same-second entries still animates rather than dumping.
  const ms = (gapSeconds * 1000) / speed;
  return Math.max(16, Math.min(1200, ms));
}

function useTranscript(matchId: string, contestantId: string, task: string) {
  const [entries, setEntries] = React.useState<TranscriptEntry[]>([]);
  const [waiting, setWaiting] = React.useState(true);
  const [ended, setEnded] = React.useState(false);

  React.useEffect(() => {
    setEntries([]);
    setWaiting(true);
    setEnded(false);
    const bySeq = new Map<number, TranscriptEntry>();
    const url =
      `/api/matches/${encodeURIComponent(matchId)}/transcript/` +
      `${encodeURIComponent(contestantId)}/${encodeURIComponent(task)}`;
    const source = new EventSource(url);
    source.addEventListener("entries", (event) => {
      const payload = JSON.parse((event as MessageEvent).data) as { entries: TranscriptEntry[] };
      for (const entry of payload.entries) bySeq.set(entry.seq, entry);
      if (bySeq.size) {
        setWaiting(false);
        setEntries([...bySeq.values()]);
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

const KIND_TONE: Record<string, string> = {
  reasoning: "text-acc-violet opacity-85",
  text: "text-foreground",
  tool: "text-acc-cyan",
  tool_result: "text-muted",
  stage: "text-accent",
  error: "text-bad",
  verdict: "font-semibold text-ok",
  complete: "text-ok",
  usage: "text-dim",
};

function entryBody(entry: TranscriptEntry): string {
  const meta = (entry.meta || {}) as Record<string, unknown>;
  const body = entry.body || "";
  if (entry.kind === "usage") {
    return (
      `${meta.model || ""} · in ${fmtTokens(meta.tokens_in)} out ${fmtTokens(meta.tokens_out)}` +
      ` · cache ${fmtTokens(meta.cache_read)}/${fmtTokens(meta.cache_write)}` +
      ` · self-rep ${fmtMoney(meta.cost_usd)}`
    );
  }
  if (entry.kind === "tool") return `${entry.title ?? ""} ${body}`;
  if (entry.kind === "tool_result") {
    const lines = body.split("\n");
    return lines.slice(0, 12).join("\n") + (lines.length > 12 ? `\n… +${lines.length - 12} lines` : "");
  }
  if (entry.kind === "stage" || entry.kind === "complete" || entry.kind === "verdict") {
    return (entry.title ?? "") + (body ? ` — ${body}` : "");
  }
  return body;
}

export function TranscriptDrawer({
  matchId,
  task,
  seats,
  cells,
  live,
  onClose,
}: {
  matchId: string;
  task: string;
  seats: ContestantSnap[];
  cells: Record<string, Cell | null | undefined>;
  /** Whether the match is still running — decides pause-vs-speed controls. */
  live: boolean;
  onClose: () => void;
}) {
  // Contestant 1 by default, as specified; the picker switches seats without
  // tearing the drawer down.
  const [seatId, setSeatId] = React.useState(seats[0]?.id ?? "");
  React.useEffect(() => {
    if (!seats.some((s) => s.id === seatId)) setSeatId(seats[0]?.id ?? "");
  }, [seats, seatId]);

  const [showReasoning, setShowReasoning] = React.useState(true);
  const [playing, setPlaying] = React.useState(true);
  const [speed, setSpeed] = React.useState<number>(1);
  /** How many entries of the historical transcript have been revealed. */
  const [revealed, setRevealed] = React.useState(0);

  const { entries, waiting, ended } = useTranscript(matchId, seatId, task);
  const feedRef = React.useRef<HTMLDivElement>(null);

  const filtered = React.useMemo(
    () => (showReasoning ? entries : entries.filter((e) => e.kind !== "reasoning")),
    [entries, showReasoning],
  );

  // Historical playback: a finished trial reveals on a timer paced by each
  // entry's own elapsed stamp. A live trial reveals everything immediately —
  // the stream itself is the pacing.
  const historical = ended && !live;

  React.useEffect(() => {
    // Restart the reveal whenever the source changes.
    setRevealed(historical ? 0 : filtered.length);
    setPlaying(true);
  }, [seatId, task, historical, filtered.length]);

  React.useEffect(() => {
    if (!historical || !playing) return;
    if (revealed >= filtered.length) return;
    const previous = filtered[revealed - 1]?.t ?? 0;
    const current = filtered[revealed]?.t ?? previous;
    const timer = window.setTimeout(
      () => setRevealed((n) => Math.min(n + 1, filtered.length)),
      pacedDelay(Math.max(0, current - previous), speed),
    );
    return () => window.clearTimeout(timer);
  }, [historical, playing, revealed, filtered, speed]);

  const shown = historical ? filtered.slice(0, revealed) : filtered;

  // Follow the tail while playing. On resume this snaps to the newest entry,
  // which is the documented live behaviour and the sane historical one too.
  React.useEffect(() => {
    if (playing && feedRef.current) {
      feedRef.current.scrollTop = feedRef.current.scrollHeight;
    }
  }, [shown.length, playing]);

  // Escape closes, as every drawer should.
  React.useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const seat = seats.find((s) => s.id === seatId);
  const cell = cells[seatId];
  const done = historical && revealed >= filtered.length && filtered.length > 0;

  // Three states share one button, and `done` has to be read *before*
  // `playing`: playback stops by running out of entries, not by clearing the
  // flag, so a finished replay is still `playing` and would otherwise offer
  // "pause" — and toggling the flag on a fully-revealed transcript is a no-op,
  // which is what made the old "replay" do nothing. Replay rewinds instead.
  const replay = React.useCallback(() => {
    setRevealed(0);
    setPlaying(true);
  }, []);

  return (
    <div className="fixed inset-0 z-50 flex justify-end" role="dialog" aria-modal="true">
      {/* Scrim: clicking outside closes, which is the gesture people try
          first and the one a modal drawer owes them. */}
      <button
        type="button"
        aria-label="close the transcript"
        className="absolute inset-0 cursor-default bg-black/45 backdrop-blur-[2px]"
        onClick={onClose}
      />

      {/* Full width on mobile, a readable column on desktop. */}
      <aside
        className="relative flex h-full w-full max-w-full flex-col border-l border-line bg-background shadow-2xl sm:w-[min(760px,80vw)]"
        style={seatStyle(seat?.color)}
      >
        <header className="flex flex-wrap items-center gap-x-3 gap-y-2 border-b border-line px-4 py-3">
          <div className="min-w-0">
            <div className="truncate font-mono text-[13px]">{task}</div>
            <div className="text-[10.5px] text-dim">
              {historical ? "historical · replay" : live ? "live" : "finished"}
            </div>
          </div>
          <Button variant="ghost" size="sm" className="ml-auto" onClick={onClose}>
            close ✕
          </Button>
        </header>

        {/* Whose transcript. Default is contestant 1; switching is one click
            and keeps the drawer open. */}
        <div className="flex flex-wrap items-center gap-1.5 border-b border-line px-4 py-2">
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
              {cells[option.id]?.resolved === true && <span className="text-ok">✓</span>}
              {cells[option.id]?.resolved === false && <span className="text-bad">✗</span>}
            </button>
          ))}
        </div>

        {/* Playback. Historical gets speed; live gets pause/resume only. */}
        <div className="flex flex-wrap items-center gap-2 border-b border-line px-4 py-2">
          <Button
            variant="ghost"
            size="sm"
            onClick={done ? replay : () => setPlaying((p) => !p)}
          >
            {done ? "▶ replay" : playing ? "❙❙ pause" : "▶ play"}
          </Button>
          {historical ? (
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
          ) : (
            <span className="text-[10.5px] text-dim">
              live · resuming jumps to the newest entry
            </span>
          )}
          {historical && (
            <span className="ml-auto font-mono text-[10.5px] text-dim">
              {Math.min(revealed, filtered.length)} / {filtered.length}
            </span>
          )}
          <Button
            variant="ghost"
            size="sm"
            className={historical ? "" : "ml-auto"}
            onClick={() => setShowReasoning((v) => !v)}
          >
            {showReasoning ? "hide reasoning" : "show reasoning"}
          </Button>
        </div>

        {cell && (
          <div className="flex flex-wrap gap-3 border-b border-line-soft px-4 py-2 font-mono text-[10.5px] text-muted">
            <span>steps <b className="font-medium text-foreground">{cell.steps}</b></span>
            <span>tools <b className="font-medium text-foreground">{cell.tools}</b></span>
            <span>in <b className="font-medium text-foreground">{fmtTokens(cell.tokens_in)}</b></span>
            <span>out <b className="font-medium text-foreground">{fmtTokens(cell.tokens_out)}</b></span>
            <span>cost <b className="font-medium text-foreground">{fmtMoney(cell.priced_cost)}</b></span>
            <span>{fmtClock(cell.clock_time)}</span>
          </div>
        )}

        <div
          ref={feedRef}
          className="flex-1 overflow-y-auto px-4 py-2.5 font-mono text-[11.5px] leading-[1.62]"
        >
          {waiting && shown.length === 0 ? (
            <div className="py-1 text-accent">waiting for the trial to start…</div>
          ) : shown.length === 0 ? (
            <div className="py-1 text-dim">no transcript entries for this trial.</div>
          ) : (
            shown.map((entry) => (
              <div key={entry.seq} className="flex gap-[9px] py-0.5">
                <span className="w-[46px] flex-none pt-0.5 text-[10px] text-dim">
                  {fmtClock(entry.t)}
                </span>
                <span
                  className={cn(
                    "min-w-0 flex-1 whitespace-pre-wrap break-words",
                    KIND_TONE[entry.kind] ?? "text-foreground",
                    entry.kind === "usage" && "text-[10.5px]",
                  )}
                >
                  {entryBody(entry)}
                </span>
              </div>
            ))
          )}
        </div>
      </aside>
    </div>
  );
}
