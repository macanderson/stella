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
    <section className="mb-2 rounded-[10px] border border-line bg-panel px-3 py-2">
      {snapshot.dimensions.map((dim) => {
        const values = snapshot.contestants.map((c) => Number(c.totals[dim.key]) || 0);
        const max = Math.max(...values, 0);
        const positives = values.filter((v) => v > 0);
        const min = positives.length ? Math.min(...positives) : 0;
        const arrow = dim.direction === "higher" ? "↑" : dim.direction === "lower" ? "↓" : "·";

        return (
          <div
            key={dim.key}
            className="grid items-center gap-3.5 py-1.5 [grid-template-columns:116px_1fr]"
          >
            <Tip content={dim.blurb}>
              <div className="text-[10.5px] lowercase tracking-[0.07em] text-dim">
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
                    className="grid items-center gap-2.5 [grid-template-columns:1fr_78px]"
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
                      {fmtDim(dim.key, value)}
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
