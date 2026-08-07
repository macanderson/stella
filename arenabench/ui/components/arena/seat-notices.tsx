"use client";

import * as React from "react";
import type { Snapshot } from "@/lib/types";
import { seatStyle } from "@/lib/utils";

/**
 * Per-seat warnings, notes and errors — the half of the old scoreboard that
 * was never decoration.
 *
 * This exists as its own strip because of a specific failure it has to keep
 * catching: a seat with no credential in its environment runs, produces
 * nothing, and scores 0%. The number alone is indistinguishable from an agent
 * that tried and failed, and the only thing that tells the two apart is the
 * line "no credential in this seat's env". A redesign that shows the 0% and
 * drops that line turns a configuration mistake into a benchmark result,
 * which is the single worst thing this tool can do.
 *
 * So: errors are always shown, and never behind a disclosure.
 */
export function SeatNotices({ snapshot }: { snapshot: Snapshot }) {
  const seats = snapshot.contestants.filter(
    (seat) => seat.error || (seat.warnings || []).length > 0 || (seat.notes || []).length > 0,
  );
  if (seats.length === 0) return null;

  return (
    <section className="mb-4 grid gap-2">
      {seats.map((seat) => (
        <div
          key={seat.id}
          style={seatStyle(seat.color)}
          className="rounded-[10px] border border-line bg-panel px-3.5 py-2.5"
        >
          <div className="flex items-center gap-2">
            <span className="size-[7px] rounded-full bg-(--seat)" />
            <span className="text-[12px] font-semibold">{seat.name}</span>
            <span className="font-mono text-[10.5px] text-dim">{seat.engine_label}</span>
            <span className="ml-auto font-mono text-[10.5px] text-dim">{seat.state}</span>
          </div>
          {seat.error && (
            <div className="mt-1.5 font-mono text-[11px] text-bad">{seat.error}</div>
          )}
          {(seat.warnings || []).length > 0 && (
            <div className="mt-1.5 font-mono text-[11px] text-warn">
              {seat.warnings!.join(" · ")}
            </div>
          )}
          {(seat.notes || []).length > 0 && (
            <div className="mt-1.5 font-mono text-[11px] text-muted">{seat.notes!.join(" · ")}</div>
          )}
        </div>
      ))}
    </section>
  );
}
