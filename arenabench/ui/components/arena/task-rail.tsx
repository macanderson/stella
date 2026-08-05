"use client";

import * as React from "react";
import type { Snapshot } from "@/lib/types";
import { cn, seatStyle } from "@/lib/utils";

export function TaskRail({
  snapshot,
  activeTask,
  onSelect,
}: {
  snapshot: Snapshot;
  activeTask: string | null;
  onSelect: (task: string) => void;
}) {
  const done = snapshot.rows.filter((row) =>
    Object.values(row.cells).every((cell) => cell && cell.status === "done"),
  ).length;

  return (
    <aside className="sticky top-[70px] flex max-h-[calc(100vh-92px)] flex-col overflow-hidden rounded-xl border border-line bg-panel max-lg:static max-lg:max-h-[300px]">
      <div className="flex items-center justify-between border-b border-line px-3.5 py-[11px] text-[11px] lowercase tracking-[0.08em] text-muted">
        <span>Tasks</span>
        <span className="font-mono">
          {done}/{snapshot.rows.length}
        </span>
      </div>
      <div className="overflow-y-auto p-[5px]">
        {snapshot.rows.map((row) => (
          <button
            key={row.task}
            type="button"
            onClick={() => onSelect(row.task)}
            className={cn(
              "block w-full cursor-pointer rounded-[7px] border px-2.5 py-2 text-left",
              activeTask === row.task
                ? "border-dim bg-panel-2"
                : "border-transparent hover:bg-panel-2",
            )}
          >
            <div className="truncate font-mono text-xs" title={row.task}>
              {row.task}
            </div>
            <div className="mt-[5px] flex gap-[3px]">
              {snapshot.contestants.map((c) => {
                const cell = row.cells[c.id];
                // A voided trial (harness/host fault) is violet, never red:
                // red publishes a loss for a contest that never happened. A
                // partial score is amber — the verifier said "some", and a
                // strip that can only say yes/no would erase it.
                const partial =
                  cell?.resolved === false && cell.reward != null && cell.reward > 0;
                const verdict = !cell
                  ? "pending"
                  : cell.status === "running"
                    ? "running"
                    : cell.infrastructure
                      ? `void — ${cell.failure || "harness/host"}`
                      : cell.resolved === true
                        ? "solved"
                        : partial
                          ? `partial ${cell.reward}`
                          : cell.resolved === false
                            ? "failed"
                            : "awaiting verdict";
                return (
                  <span
                    key={c.id}
                    title={`${c.name} — ${verdict}`}
                    style={seatStyle(c.color)}
                    className={cn(
                      "h-1 flex-1 rounded-sm",
                      !cell && "bg-line",
                      cell?.status === "running" &&
                        "animate-[cell-pulse_1.5s_ease-in-out_infinite] bg-(--seat)",
                      cell &&
                        cell.status !== "running" &&
                        (cell.infrastructure
                          ? "bg-acc-violet"
                          : cell.resolved === true
                            ? "bg-ok"
                            : partial
                              ? "bg-warn"
                              : cell.resolved === false
                                ? "bg-bad"
                                : "bg-line"),
                    )}
                  />
                );
              })}
            </div>
          </button>
        ))}
      </div>
    </aside>
  );
}
