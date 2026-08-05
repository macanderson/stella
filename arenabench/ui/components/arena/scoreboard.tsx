"use client";

import * as React from "react";
import type { Snapshot } from "@/lib/types";
import { fmtClock, fmtMoney, fmtPct, fmtTokens } from "@/lib/format";
import { cn, seatStyle } from "@/lib/utils";

export function Scoreboard({ snapshot }: { snapshot: Snapshot }) {
  const leaders = snapshot.leaders || {};
  const crowns: Record<string, number> = {};
  for (const winners of Object.values(leaders)) {
    for (const id of winners) crowns[id] = (crowns[id] || 0) + 1;
  }
  const top = Math.max(0, ...Object.values(crowns));
  // Neutral dimensions are reported but crown nobody, so they are not part of
  // the denominator a seat is being scored out of. Neither is a dimension
  // nobody has a number for — wasted time in a match nobody replayed with
  // `arenabench flip` can crown nobody, and counting it would score every
  // seat out of a total that includes an unwinnable column.
  const crownable = snapshot.dimensions.filter(
    (d) =>
      d.direction !== "neutral" &&
      snapshot.contestants.some((c) => c.totals[d.key] != null),
  ).length;

  return (
    <section className="mb-2 grid gap-2 [grid-template-columns:repeat(auto-fit,minmax(230px,1fr))]">
      {snapshot.contestants.map((c) => {
        const t = c.totals;
        const isLeader = top > 0 && crowns[c.id] === top;
        const best = (key: string) => (leaders[key] || []).includes(c.id);
        const stat = (key: string, label: string, value: string) => (
          <div key={key} className="flex items-baseline justify-between gap-2 text-[11.5px]">
            <span className="text-[9.5px] lowercase tracking-[0.05em] text-dim">{label}</span>
            <span
              className={cn(
                "font-mono text-[12.5px]",
                best(key) && "font-semibold text-(--seat-fg)",
              )}
            >
              {value}
            </span>
          </div>
        );

        return (
          <div
            key={c.id}
            style={seatStyle(c.color)}
            className={cn(
              "relative overflow-hidden rounded-xl border bg-panel px-[17px] py-4",
              "bg-[linear-gradient(180deg,color-mix(in_srgb,var(--seat)_10%,transparent),transparent_62%)]",
              isLeader
                ? "border-[color-mix(in_srgb,var(--seat)_55%,transparent)] shadow-[0_0_0_1px_color-mix(in_srgb,var(--seat)_22%,transparent),0_8px_34px_-18px_var(--seat)]"
                : "border-line",
            )}
          >
            {isLeader ? (
              <div className="absolute right-3.5 top-[11px] text-[15px]">👑</div>
            ) : (
              <div className="absolute right-[15px] top-[13px] font-mono text-[11px] text-dim">
                {crowns[c.id] || 0}/{crownable}
              </div>
            )}
            <div className="flex items-center gap-2 text-[15px] font-[650]">
              <span className="size-[9px] rounded-[2px] bg-(--seat)" />
              {c.name}
            </div>
            <div className="mt-[3px] font-mono text-[11px] text-muted">
              {c.agent} · {c.engine_label}
            </div>
            <div className="mt-[2px] font-mono text-[10.5px] text-dim">
              {c.state}
              {t.running ? ` · ${t.running} running` : ""} · {t.judged}/{t.trials} judged
            </div>
            {/* An arm whose trials never started has not lost them. Saying so
                beside the rate is the difference between "won 8 of 10" and
                "won 8 of the 8 that managed to start". */}
            {t.infrastructure ? (
              <div className="mt-1.5 font-mono text-[11px] text-acc-violet">
                {t.infrastructure} never started (harness/host)
              </div>
            ) : null}
            <div className="mb-1 mt-3.5 flex items-baseline gap-2">
              <span className="font-mono text-[38px] font-light leading-none tracking-[-0.03em] text-(--seat-fg)">
                {fmtPct(t.solve_rate)}
              </span>
              <span className="font-mono text-xs text-muted">
                {t.passed} / {t.judged || 0} solved
              </span>
            </div>
            <div className="mb-3.5 mt-2.5 h-1 overflow-hidden rounded-sm bg-line">
              <i
                className="block h-full rounded-sm bg-(--seat) transition-[width] duration-[450ms] ease-[cubic-bezier(0.4,0,0.2,1)]"
                style={{ width: `${Math.min(100, t.solve_rate)}%` }}
              />
            </div>
            <div className="grid grid-cols-2 gap-x-3.5 gap-y-[7px]">
              {stat("clock_time", "clock", fmtClock(t.clock_time))}
              {/* Exists only when someone ran `arenabench flip`. The label
                  carries the denominator because a sum over an unknown number
                  of replayed trials reads as a measurement of the whole run. */}
              {t.wasted_time != null &&
                stat(
                  "wasted_time",
                  `wasted (${t.flip_trials ?? 0} replayed)`,
                  fmtClock(t.wasted_time),
                )}
              {/* The comparable figure carries the label; what the agent said
                  it spent sits beside it, named, and crowns nobody — the two
                  come from different price tables. */}
              {stat("priced_cost", "cost", fmtMoney(t.priced_cost))}
              {stat("total_cost", "self-rep", fmtMoney(t.total_cost))}
              {stat("tokens_in", "tok in", fmtTokens(t.tokens_in))}
              {stat("tokens_out", "tok out", fmtTokens(t.tokens_out))}
              {stat("cache_read", "cache r", fmtTokens(t.cache_read))}
              {stat("cache_write", "cache w", fmtTokens(t.cache_write))}
            </div>
            {(c.warnings || []).length > 0 && (
              <div className="mt-[11px] font-mono text-[11px] text-warn">
                {c.warnings!.join(" · ")}
              </div>
            )}
            {(c.notes || []).length > 0 && (
              <div className="mt-1.5 font-mono text-[11px] text-muted">{c.notes!.join(" · ")}</div>
            )}
            {c.error && <div className="mt-[11px] font-mono text-[11px] text-warn">{c.error}</div>}
          </div>
        );
      })}
    </section>
  );
}
