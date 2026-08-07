"use client";

import * as React from "react";
import type { Snapshot } from "@/lib/types";
import { fmtDim } from "@/lib/format";
import { cn, seatStyle } from "@/lib/utils";
import { Tip } from "@/components/ui/tooltip";

/**
 * The race: one row per dimension, one bar per contestant, normalised so the
 * leader's bar is full. Bars are relative because the absolute numbers span
 * orders of magnitude across dimensions — the question this answers is "who
 * is ahead and by how much", not "how many tokens exactly".
 */
export function Race({ snapshot }: { snapshot: Snapshot }) {
  const leaders = snapshot.leaders || {};
  return (
    <section className="rounded-[10px] border border-line bg-panel px-3 py-2">
      {snapshot.dimensions.map((dim) => {
        // A dimension nobody has a number for has nothing to race: wasted
        // time without a single `arenabench flip` replay, cost without a
        // priced model. Drawing it would show every seat at zero — and zero
        // is the best score on a lower-is-better dimension, not an absence.
        const raw = snapshot.contestants.map((c) => c.totals[dim.key]);
        if (raw.length > 0 && raw.every((v) => v == null)) return null;
        const values = raw.map((v) => Number(v) || 0);
        const max = Math.max(...values, 0);
        const positives = values.filter((v) => v > 0);
        const min = positives.length ? Math.min(...positives) : 0;
        const arrow = dim.direction === "higher" ? "↑" : dim.direction === "lower" ? "↓" : "·";

        return (
          /* Stacked, not two columns: this now lives in a quarter-width rail,
             where a fixed 116px label gutter left the bars too short to read
             a ratio off. The label sits above its own bars instead. */
          <div key={dim.key} className="py-1.5">
            <Tip content={dim.blurb}>
              <div className="mb-1 text-[10.5px] lowercase tracking-[0.07em] text-dim">
                {dim.label} <i className="not-italic text-line">{arrow}</i>
              </div>
            </Tip>
            <div className="flex flex-col gap-[3px]">
              {snapshot.contestants.map((c, index) => {
                const value = values[index];
                let width = 0;
                if (dim.direction === "lower") {
                  // Invert: the smallest non-zero value gets the longest bar.
                  // A zero means "has not spent anything yet", which is not a
                  // lead — it draws empty rather than winning by default.
                  width = value > 0 && min > 0 ? (min / value) * 100 : 0;
                } else {
                  width = max > 0 ? (value / max) * 100 : 0;
                }
                const isBest = (leaders[dim.key] || []).includes(c.id);
                return (
                  <div
                    key={c.id}
                    style={seatStyle(c.color)}
                    className="grid items-center gap-2 [grid-template-columns:1fr_64px]"
                  >
                    <div className="h-[9px] overflow-hidden rounded-[3px] bg-line-soft">
                      <i
                        className={cn(
                          "block h-full rounded-[3px] bg-(--seat) transition-[width,opacity] duration-500",
                          isBest ? "opacity-100 shadow-[0_0_12px_-2px_var(--seat)]" : "opacity-55",
                        )}
                        style={{ width: `${Math.max(2, Math.min(100, width))}%` }}
                      />
                    </div>
                    <div
                      className={cn(
                        "text-right font-mono text-[11.5px]",
                        isBest ? "text-(--seat-fg)" : "text-muted",
                      )}
                    >
                      {/* The RAW value, not the coerced one (#1599): `values`
                          exists for the bar arithmetic, where an absence
                          legitimately draws as zero. Formatting from it
                          renders a seat nobody priced as `$0.0000` — a
                          fabricated number in the one column the pricing path
                          exists to keep honest. Missing beats wrong. */}
                      {fmtDim(dim.key, raw[index])}
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        );
      })}
    </section>
  );
}
