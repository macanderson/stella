"use client";

import * as React from "react";
import type { Cell, ContestantSnap, Snapshot } from "@/lib/types";
import { fmtClock, fmtMoney, fmtTokens } from "@/lib/format";
import { cn, seatStyle } from "@/lib/utils";
import { Tip } from "@/components/ui/tooltip";

/**
 * The task table: one row per task, every contestant's numbers side by side,
 * with the winner marked **per metric** rather than only per task.
 *
 * Why per metric: "who solved it" is one bit and the seats agree on it far
 * more often than people expect — most of the interesting difference between
 * two agents on the same task is *how* they got there (steps taken, tokens
 * burned, wall-clock spent). A table that marks only the solve hides exactly
 * the comparison this tool exists to make.
 *
 * The rules for marking a winner are deliberately conservative, because a
 * confident tick on a meaningless difference is worse than no tick:
 *
 * - A metric is only contested when **at least two seats have a real value**
 *   for it. One seat reporting and the others silent is not a win.
 * - **Ties are not wins.** Every tied seat is left unmarked rather than the
 *   first one arbitrarily crowned.
 * - **Direction comes from the snapshot**, not from this file's opinion:
 *   `resolved` and steps-to-solve are better high or low depending on the
 *   dimension the server declares.
 * - A cheaper/faster run that **did not solve the task is never a win.**
 *   Losing instantly for free is not beating a seat that solved it, so the
 *   efficiency metrics are only contested among seats that both passed.
 */

/** The per-task metrics, and which direction is better. */
const METRICS: Array<{
  key: keyof Cell;
  label: string;
  better: "higher" | "lower";
  /** Only compare among seats that solved the task (see the doc comment). */
  solvedOnly: boolean;
  format: (value: number) => string;
}> = [
  { key: "clock_time", label: "time", better: "lower", solvedOnly: true, format: (v) => fmtClock(v) },
  { key: "steps", label: "steps", better: "lower", solvedOnly: true, format: (v) => String(v) },
  { key: "tokens_out", label: "tok out", better: "lower", solvedOnly: true, format: (v) => fmtTokens(v) },
  { key: "priced_cost", label: "cost", better: "lower", solvedOnly: true, format: (v) => fmtMoney(v) },
];

function numeric(cell: Cell | null | undefined, key: keyof Cell): number | null {
  if (!cell) return null;
  const raw = cell[key];
  if (raw == null || typeof raw !== "number" || Number.isNaN(raw)) return null;
  return raw;
}

/**
 * Which seats win each metric on one task. Returns a set of
 * `"<metric>:<contestantId>"` keys — absent means "not a winner", which
 * covers ties, single-reporter metrics and unsolved trials alike.
 */
function winnersFor(
  cells: Record<string, Cell | null | undefined>,
  seats: ContestantSnap[],
): Set<string> {
  const won = new Set<string>();

  const solved = seats.filter((s) => cells[s.id]?.resolved === true);
  if (solved.length >= 2) {
    for (const metric of METRICS) {
      const pool = metric.solvedOnly ? solved : seats;
      const scored = pool
        .map((s) => ({ id: s.id, value: numeric(cells[s.id], metric.key) }))
        .filter((entry): entry is { id: string; value: number } => entry.value !== null);
      if (scored.length < 2) continue; // nothing to contest
      const best =
        metric.better === "lower"
          ? Math.min(...scored.map((e) => e.value))
          : Math.max(...scored.map((e) => e.value));
      const atBest = scored.filter((e) => e.value === best);
      if (atBest.length !== 1) continue; // a tie is not a win
      won.add(`${String(metric.key)}:${atBest[0].id}`);
    }
  }

  // The solve itself: a win only when someone passed and someone else did not.
  const passed = seats.filter((s) => cells[s.id]?.resolved === true);
  const judged = seats.filter((s) => cells[s.id]?.status === "done");
  if (passed.length >= 1 && judged.length >= 2 && passed.length < judged.length) {
    for (const seat of passed) won.add(`resolved:${seat.id}`);
  }
  return won;
}

/**
 * The verdict and the incident are two facts, not one (#2066): a trial can
 * score the reward and then raise (`crack-7z-hash` solved at reward 1.0 and
 * still hit `AgentTimeoutError`), and the either/or branch this used to be
 * showed only the tick — under-reporting the incident entirely. The tick
 * stays `text-ok` because the task DID solve (the grading is Harbor's and
 * unchanged); the marker carries the exception in a warning tone so it is
 * legible without implying failure.
 */
function Verdict({ cell }: { cell: Cell | null | undefined }) {
  if (!cell) return <span className="font-mono text-[11px] text-dim">queued</span>;
  if (cell.status !== "done") {
    return (
      <span className="font-mono text-[11px] text-accent">
        {cell.status}
        {cell.age_s != null ? ` ${Math.round(cell.age_s)}s` : ""}
      </span>
    );
  }
  if (cell.resolved === true) {
    if (!cell.failure) {
      return <span className="font-mono text-[11px] font-semibold text-ok">✓ solved</span>;
    }
    return (
      <span className="flex items-center gap-1">
        <span className="font-mono text-[11px] font-semibold text-ok">✓ solved</span>
        <Tip
          content={
            `Solved, then raised ${cell.failure}. The reward was scored before the ` +
            `exception, so the solve stands — but the trial did not end cleanly ` +
            `(for a timeout: the agent kept burning budget after succeeding).`
          }
        >
          <span className="font-mono text-[11px] text-warn">⚠ incident</span>
        </Tip>
      </span>
    );
  }
  return (
    <Tip content={cell.failure || undefined}>
      <span className="font-mono text-[11px] text-bad">✗ failed</span>
    </Tip>
  );
}

export function TaskTable({ snapshot }: { snapshot: Snapshot }) {
  const seats = snapshot.contestants;

  return (
    <section className="min-w-0 overflow-hidden rounded-[10px] border border-line bg-panel">
      <header className="flex items-center gap-3 border-b border-line px-4 py-[11px]">
        <h2 className="text-[13px] font-semibold lowercase tracking-[0.04em]">tasks</h2>
        <span className="font-mono text-[11px] text-muted">{snapshot.rows.length}</span>
        <div className="ml-auto flex items-center gap-3">
          {seats.map((seat) => (
            <span key={seat.id} className="flex items-center gap-1.5" style={seatStyle(seat.color)}>
              <span className="size-[7px] rounded-full bg-(--seat)" />
              <span className="text-[11px] text-muted">{seat.name}</span>
            </span>
          ))}
        </div>
      </header>

      {/* Horizontal scroll is confined to this container so the page body
          never scrolls sideways on a narrow window. */}
      <div className="overflow-x-auto">
        <table className="w-full min-w-[620px] border-collapse text-left">
          <thead>
            <tr className="border-b border-line-soft text-[10px] lowercase tracking-[0.09em] text-dim">
              <th className="px-4 py-2 font-normal">task</th>
              <th className="px-2 py-2 font-normal">seat</th>
              <th className="px-2 py-2 font-normal">verdict</th>
              {METRICS.map((m) => (
                <th key={String(m.key)} className="px-2 py-2 text-right font-normal">
                  {m.label} <i className="not-italic text-line">{m.better === "lower" ? "↓" : "↑"}</i>
                </th>
              ))}
              <th className="px-3 py-2 font-normal" />
            </tr>
          </thead>
          <tbody>
            {snapshot.rows.map((row) => {
              const won = winnersFor(row.cells, seats);
              return seats.map((seat, seatIndex) => {
                const cell = row.cells[seat.id];
                return (
                  <tr
                    key={`${row.task}:${seat.id}`}
                    style={seatStyle(seat.color)}
                    className={cn(
                      "border-b border-line-soft/60 align-middle",
                      seatIndex === seats.length - 1 && "border-b-line",
                    )}
                  >
                    {seatIndex === 0 && (
                      <td
                        rowSpan={seats.length}
                        className="max-w-[220px] truncate px-4 py-2 align-middle font-mono text-[12px]"
                      >
                        {row.task}
                      </td>
                    )}
                    <td className="whitespace-nowrap px-2 py-1.5">
                      <span className="flex items-center gap-1.5">
                        <span className="size-[6px] flex-none rounded-full bg-(--seat)" />
                        <span className="text-[11.5px] text-muted">{seat.name}</span>
                      </span>
                    </td>
                    <td className="whitespace-nowrap px-2 py-1.5">
                      <span className="flex items-center gap-1">
                        <Verdict cell={cell} />
                        {won.has(`resolved:${seat.id}`) && (
                          <Tip content="Solved this task where another seat did not.">
                            <span className="text-[10px] text-ok">▲</span>
                          </Tip>
                        )}
                      </span>
                    </td>
                    {METRICS.map((metric) => {
                      const value = numeric(cell, metric.key);
                      const isWinner = won.has(`${String(metric.key)}:${seat.id}`);
                      return (
                        <td
                          key={String(metric.key)}
                          className={cn(
                            "whitespace-nowrap px-2 py-1.5 text-right font-mono text-[11.5px]",
                            isWinner ? "font-semibold text-(--seat-fg)" : "text-muted",
                          )}
                        >
                          {value === null ? (
                            <span className="text-dim">—</span>
                          ) : (
                            <>
                              {metric.format(value)}
                              {isWinner && (
                                <Tip
                                  content={`Best ${metric.label} among the seats that solved this task.`}
                                >
                                  <i className="ml-1 not-italic text-ok">▲</i>
                                </Tip>
                              )}
                            </>
                          )}
                        </td>
                      );
                    })}
                    {seatIndex === 0 && (
                      <td rowSpan={seats.length} className="px-3 py-2 text-right align-middle">
                        {/* A link, not a drawer: the transcript is its own
                            page, so it can be bookmarked and shared. */}
                        <a
                          href={
                            `/transcript?match=${encodeURIComponent(snapshot.match.id)}` +
                            `&task=${encodeURIComponent(row.task)}`
                          }
                          title={`Read the transcripts for ${row.task}`}
                          className="inline-flex cursor-pointer items-center rounded-[7px] border border-line bg-transparent px-2.5 py-[5px] font-mono text-xs lowercase text-foreground transition-colors hover:border-dim"
                        >
                          transcript
                        </a>
                      </td>
                    )}
                  </tr>
                );
              });
            })}
          </tbody>
        </table>
      </div>
    </section>
  );
}
