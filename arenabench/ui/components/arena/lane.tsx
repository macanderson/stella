"use client";

import * as React from "react";
import type { Cell, ContestantSnap, TranscriptEntry } from "@/lib/types";
import { fmtClock, fmtMoney, fmtTokens } from "@/lib/format";
import { cn, seatStyle } from "@/lib/utils";
import { Disclosure } from "@/components/ui/collapsible";
import { FileBrowser } from "@/components/arena/file-browser";

/**
 * One trial's transcript over SSE. Entries carry a `seq`, and a streaming
 * entry is re-sent under the same `seq` as it grows — a Map keyed by seq
 * gives append-or-replace for free, and Map insertion order keeps a growing
 * entry in its original position.
 */
function useTranscript(matchId: string, contestantId: string, task: string) {
  const [entries, setEntries] = React.useState<TranscriptEntry[]>([]);
  const [waiting, setWaiting] = React.useState(true);

  React.useEffect(() => {
    setEntries([]);
    setWaiting(true);
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
        setEntries([...bySeq.values()]);
      }
    });
    source.addEventListener("end", () => {
      // A trial can end without ever emitting a transcript entry (e.g. it
      // never started, or failed before producing output). Clear `waiting`
      // so the empty state renders instead of a permanent loading message —
      // the verdict header already conveys the terminal status.
      setWaiting(false);
      source.close();
    });
    // On transport errors the browser retries by itself; a finished trial
    // ends the stream above.
    return () => source.close();
  }, [matchId, contestantId, task]);

  return { entries, waiting };
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
  let body = entry.body || "";
  if (entry.kind === "usage") {
    return (
      `${meta.model || ""} · in ${fmtTokens(meta.tokens_in)} out ${fmtTokens(meta.tokens_out)}` +
      ` · cache ${fmtTokens(meta.cache_read)}/${fmtTokens(meta.cache_write)}` +
      ` · self-rep ${fmtMoney(meta.cost_usd)}` +
      ` · ${Math.round(((meta.duration_ms as number) || 0) / 100) / 10}s`
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

export function Lane({
  matchId,
  contestant,
  task,
  cell,
  follow,
  showReasoning,
}: {
  matchId: string;
  contestant: ContestantSnap;
  task: string;
  cell: Cell | null | undefined;
  follow: boolean;
  showReasoning: boolean;
}) {
  const { entries, waiting } = useTranscript(matchId, contestant.id, task);
  const feedRef = React.useRef<HTMLDivElement>(null);

  const shown = showReasoning ? entries : entries.filter((e) => e.kind !== "reasoning");

  React.useEffect(() => {
    if (follow && feedRef.current) {
      feedRef.current.scrollTop = feedRef.current.scrollHeight;
    }
  }, [shown, follow]);

  let verdictTone = "text-dim";
  let verdictText = "queued";
  if (cell) {
    if (cell.status === "done") {
      const pass = cell.resolved === true;
      verdictTone = pass ? "text-ok" : "text-bad";
      verdictText = pass ? "✓ SOLVED" : cell.failure || "✗ failed";
    } else {
      verdictTone = "text-accent";
      verdictText = cell.status + (cell.age_s != null ? ` ${Math.round(cell.age_s)}s` : "");
    }
  }

  return (
    <div className="flex min-w-0 flex-col bg-panel" style={seatStyle(contestant.color)}>
      <div className="flex items-center gap-[9px] border-b border-line-soft bg-[linear-gradient(90deg,color-mix(in_srgb,var(--seat)_9%,transparent),transparent_55%)] px-[13px] py-[9px]">
        <span className="size-[7px] rounded-full bg-(--seat)" />
        <span className="text-[12.5px] font-semibold">{contestant.name}</span>
        <span className={cn("ml-auto font-mono text-[11px]", verdictTone)}>{verdictText}</span>
      </div>

      {cell && (
        <div className="flex flex-wrap gap-3 border-b border-line-soft px-[13px] py-[7px] font-mono text-[10.5px] text-muted">
          <span>
            steps <b className="font-medium text-foreground">{cell.steps}</b>
          </span>
          <span>
            tools <b className="font-medium text-foreground">{cell.tools}</b>
          </span>
          <span>
            in <b className="font-medium text-foreground">{fmtTokens(cell.tokens_in)}</b>
          </span>
          <span>
            out <b className="font-medium text-foreground">{fmtTokens(cell.tokens_out)}</b>
          </span>
          <span>
            cache{" "}
            <b className="font-medium text-foreground">
              {fmtTokens(cell.cache_read)}/{fmtTokens(cell.cache_write)}
            </b>
          </span>
          <span>
            cost <b className="font-medium text-foreground">{fmtMoney(cell.priced_cost)}</b>
          </span>
          <span>
            self-rep <b className="font-medium text-foreground">{fmtMoney(cell.total_cost)}</b>
          </span>
          <span>{fmtClock(cell.clock_time)}</span>
          {/* Only a replayed trial (`arenabench flip`) knows when the reward
              actually appeared; everything after that moment could only lose. */}
          {cell.flip_elapsed != null && (
            <span>
              passed at <b className="font-medium text-ok">{fmtClock(cell.flip_elapsed)}</b>
            </span>
          )}
          {cell.wasted_elapsed != null && cell.wasted_elapsed > 0 ? (
            <span className="text-warn">
              ⚠ ran {fmtClock(cell.wasted_elapsed)} after passing
            </span>
          ) : null}
          {cell.cap_hits ? <span className="text-warn">⚠ {cell.cap_hits} output-cap</span> : null}
        </div>
      )}

      {/* `has_video` means the recording carries a `moov` atom, not merely that
          a file exists. A live trial has a growing, unplayable file for its
          whole duration, so gating on existence produced a broken player. */}
      {cell?.has_video && (
        <div className="border-b border-line-soft px-[13px] py-2.5">
          <Disclosure summary="screen recording (MP4)">
            <video
              controls
              preload="none"
              className="block w-full rounded-[7px] bg-black"
              src={`/api/matches/${encodeURIComponent(matchId)}/video/${encodeURIComponent(contestant.id)}/${encodeURIComponent(task)}`}
            />
          </Disclosure>
        </div>
      )}

      {cell && (
        <div className="border-b border-line-soft px-[13px] py-2.5">
          <Disclosure summary="files">
            <FileBrowser matchId={matchId} contestantId={contestant.id} task={task} />
          </Disclosure>
        </div>
      )}

      {/* Viewport-relative height so the transcript fills the screen it is on
          instead of a fixed pixel budget that is a void on a laptop and a
          crop on a monitor. */}
      <div
        ref={feedRef}
        className="max-h-[56vh] min-h-[140px] flex-1 overflow-y-auto px-[11px] py-2 font-mono text-[11.5px] leading-[1.62]"
      >
        {waiting && shown.length === 0 ? (
          <div className="flex gap-[9px] py-0.5">
            <span className="w-[46px] flex-none" />
            <span className="text-accent">waiting for the trial to start…</span>
          </div>
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
    </div>
  );
}
