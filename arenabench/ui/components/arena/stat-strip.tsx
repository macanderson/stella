"use client";

import * as React from "react";
import type { ContestantSnap, Snapshot } from "@/lib/types";
import { fmtClock, fmtCount, fmtMoney, fmtPct, fmtTokens } from "@/lib/format";
import { seatStyle } from "@/lib/utils";
import { Tip } from "@/components/ui/tooltip";

/**
 * The match at a glance: five cards, one per question — tool calls, clock,
 * tokens, tasks, cost — each holding one column **per arm**, so the header
 * answers "who did what" without a trip to the task table. The old strip
 * summed every seat into match-level figures, which answered "how did this
 * run go" while hiding the comparison the page exists to draw.
 *
 * Two honesty rules survive from that strip, because a header number hides
 * its provenance:
 *
 * - **Cost is the priced figure**, every arm's tokens through one price
 *   table — the only money comparable across seats. The agents' self-reported
 *   tallies are not, and are not shown here at all.
 * - **An unmeasured quantity renders as `—`, never as zero.** Zero is a
 *   score; absence is not. `priced_cost` is null for an unpriced model, an
 *   average over zero attempted trials has no value, and a cache ratio with
 *   no prompt tokens behind it is silence rather than 0%.
 *
 * A `—` still has to say *why* (#2108): when an arm's cost is unknown the
 * cost card's sub-line names the models with no price row.
 */

/** Per-arm figures the wire totals do not reliably carry, summed from the
 *  per-task cells instead. Native snapshots do publish `tools`/`steps` in
 *  `totals` (telemetry.py `aggregate`), but a cloud-merged match rebuilds its
 *  totals through `cloud_merge._totals`, which carries neither — the cells
 *  are the one shape every payload agrees on. */
interface CellSums {
  /** Trials that actually started — status `running` or `done`. A queued
   *  trial has not been attempted, and dividing by it would report averages
   *  that improve as the match drains its queue. */
  attempted: number;
  tools: number;
  steps: number;
}

function useCellSums(snapshot: Snapshot): Record<string, CellSums> {
  return React.useMemo(() => {
    const sums: Record<string, CellSums> = {};
    for (const seat of snapshot.contestants) {
      sums[seat.id] = { attempted: 0, tools: 0, steps: 0 };
    }
    for (const row of snapshot.rows) {
      for (const seat of snapshot.contestants) {
        const cell = row.cells[seat.id];
        if (!cell) continue;
        const sum = sums[seat.id];
        if (cell.status === "running" || cell.status === "done") sum.attempted += 1;
        sum.tools += cell.tools || 0;
        sum.steps += cell.steps || 0;
      }
    }
    return sums;
  }, [snapshot.rows, snapshot.contestants]);
}

/**
 * Whether ANY trial of this arm published usage at all (#2132).
 *
 * `false` means every token figure below is the absence of a measurement
 * rather than a measurement of zero, and rendering it as `0` states a fact
 * nobody observed — a real arm reads `tokens_in: 0, unmeasured_trials: 3 of 3`
 * on match `feebd80ba873`, where `0/0` would claim it sent no prompt.
 *
 * Prefers the wire's own declaration and falls back to the same test the
 * server applies per trial (`TrialMetrics.usage_measured`), because
 * `unmeasured_trials` is absent from payloads recorded before the field
 * existed. No real model call reports zero of everything, so "every counter
 * zero" is "unknown", never "free".
 */
function usageMeasured(seat: ContestantSnap): boolean {
  const t = seat.totals;
  const trials = t.trials || 0;
  const unmeasured = t.unmeasured_trials;
  if (typeof unmeasured === "number" && trials > 0) return unmeasured < trials;
  return Boolean(
    t.tokens_in || t.tokens_out || t.cache_read || t.cache_write || t.total_cost,
  );
}

/** Share of prompt tokens served from cache, 0–100, or null when nothing was
 *  measured. The same definition as the server's `TrialMetrics.cache_hit_rate`
 *  (telemetry.py) — one definition, not a second one that drifts. */
function cacheHitRate(seat: ContestantSnap): number | null {
  const tokensIn = seat.totals.tokens_in || 0;
  if (tokensIn <= 0) return null;
  return Math.min(100, ((seat.totals.cache_read || 0) / tokensIn) * 100);
}

/** A mean with an honest denominator: `—` when nothing was attempted. */
function avg(numer: number, denom: number): number | null {
  return denom > 0 ? numer / denom : null;
}

/**
 * One card: a label, then a small table — the arms across the top, one row
 * per metric. The first column is the metric's own label, so a card with
 * three rows (tokens) and a card with two (cost) read the same way.
 */
function CompareCard({
  label,
  blurb,
  seats,
  rows,
  sub,
}: {
  label: string;
  blurb: string;
  seats: ContestantSnap[];
  rows: Array<{ label: string; values: React.ReactNode[] }>;
  sub?: React.ReactNode;
}) {
  const body = (
    <div className="min-w-0 border border-line bg-panel px-3.5 py-2.5">
      <div className="text-[10px] tracking-[0.1em] text-dim">{label}</div>
      <div
        className="mt-1.5 grid items-baseline gap-x-3 gap-y-[3px]"
        style={{
          gridTemplateColumns: `minmax(0,auto) repeat(${seats.length}, minmax(0,1fr))`,
        }}
      >
        <span />
        {seats.map((seat) => (
          /* The arm is identified by its COLOUR CHIP, not by tinted text.
             Five cards across a 1600px page leave each name ~90px, so every
             name here truncates — and `text-(--seat-fg)` cannot rescue it:
             that token is declared on `:root`, where `var(--seat, …)` is
             substituted against a `--seat` that does not exist yet, so every
             seat inherits the same paper fallback. Measured, not assumed
             (see the PR). The chip reads `--seat` on the element that sets
             it, which is the same shape `task-table.tsx`'s legend uses. */
          <div
            key={seat.id}
            style={seatStyle(seat.color)}
            title={seat.name}
            className="flex min-w-0 items-center justify-end gap-1.5"
          >
            <span className="size-[7px] flex-none bg-(--seat)" />
            <span className="truncate font-mono text-[10.5px] text-muted">{seat.name}</span>
          </div>
        ))}
        {rows.map((row) => (
          <React.Fragment key={row.label}>
            <div className="whitespace-nowrap text-[10px] text-muted">{row.label}</div>
            {row.values.map((value, index) => (
              <div
                key={seats[index]?.id ?? index}
                className="truncate text-right font-mono text-[12.5px] tracking-[-0.02em]"
              >
                {value}
              </div>
            ))}
          </React.Fragment>
        ))}
      </div>
      {sub ? <div className="mt-1.5 truncate text-[10.5px] text-warn">{sub}</div> : null}
    </div>
  );
  return <Tip content={blurb}>{body}</Tip>;
}

export function StatStrip({ snapshot }: { snapshot: Snapshot }) {
  const seats = snapshot.contestants;
  const cellSums = useCellSums(snapshot);

  const critical = (snapshot.detections || []).filter((d) => d.severity === "critical").length;

  // Models with no row in the shared price table, across every arm — the
  // reason a cost cell reads `—`, named so the dash is actionable (#2108).
  const unpriced = [
    ...new Set(seats.flatMap((seat) => seat.totals.unpriced_models || [])),
  ].sort();

  // The same discipline for the token dashes: how many of each arm's trials
  // published no usage, which is both why a figure is missing and the
  // denominator the figures that ARE shown were summed over. A dash that
  // cannot say why is how a whole arm's cost went unnoticed for weeks.
  const unmeasured = seats.flatMap((seat) => {
    const count = seat.totals.unmeasured_trials;
    if (typeof count !== "number" || count <= 0) return [];
    return [`${seat.name}: ${count}/${seat.totals.trials || 0} trials unmeasured`];
  });

  return (
    <>
      <section className="mb-4 grid gap-2.5 [grid-template-columns:repeat(auto-fit,minmax(250px,1fr))]">
        <CompareCard
          label="tool calls"
          blurb={
            "Every tool invocation the arm made, and the mean per attempted task. " +
            "Summed from the per-task cells, so a running trial's calls count as they happen."
          }
          seats={seats}
          rows={[
            {
              label: "total",
              values: seats.map((seat) => fmtCount(cellSums[seat.id]?.tools ?? 0)),
            },
            {
              label: "avg / task",
              values: seats.map((seat) => {
                const sums = cellSums[seat.id];
                const perTask = sums ? avg(sums.tools, sums.attempted) : null;
                return perTask == null ? "—" : perTask.toFixed(1);
              }),
            },
          ]}
        />
        <CompareCard
          label="clock"
          blurb={
            "Wall clock inside the trials: the mean per attempted task, and the sum over " +
            "every trial. This is the agents' working time, not the match's elapsed time — " +
            "trials run concurrently, so the total can exceed the clock at the top of the page."
          }
          seats={seats}
          rows={[
            {
              label: "per task",
              values: seats.map((seat) => {
                const perTask = avg(
                  seat.totals.clock_time || 0,
                  cellSums[seat.id]?.attempted ?? 0,
                );
                return fmtClock(perTask);
              }),
            },
            {
              label: "total",
              values: seats.map((seat) => fmtClock(seat.totals.clock_time || 0)),
            },
          ]}
        />
        <CompareCard
          label="tokens"
          blurb={
            "Prompt and completion tokens, the mean per model call (step), and the share of " +
            "prompt tokens served from cache — the rate, not the count, because the count " +
            "rewards sending more context. `—` means nothing was measured, never zero."
          }
          seats={seats}
          rows={[
            {
              label: "in / out",
              values: seats.map((seat) =>
                usageMeasured(seat)
                  ? `${fmtTokens(seat.totals.tokens_in)}/${fmtTokens(seat.totals.tokens_out)}`
                  : "—",
              ),
            },
            {
              label: "per step",
              values: seats.map((seat) => {
                const steps = cellSums[seat.id]?.steps ?? 0;
                // Steps can be nonzero on an arm that published no usage at
                // all, so the step count alone is not enough of a guard: the
                // division would then report a measured-looking `0/0`.
                if (steps <= 0 || !usageMeasured(seat)) return "—";
                return `${fmtTokens((seat.totals.tokens_in || 0) / steps)}/${fmtTokens(
                  (seat.totals.tokens_out || 0) / steps,
                )}`;
              }),
            },
            {
              label: "cache hit",
              values: seats.map((seat) => {
                const rate = cacheHitRate(seat);
                return rate == null ? "—" : fmtPct(rate);
              }),
            },
          ]}
          sub={unmeasured.length > 0 ? unmeasured.join(" · ") : undefined}
        />
        <CompareCard
          label="tasks"
          blurb={
            "Attempted counts every trial that started (running or done). Solved is trials " +
            "the verifier passed. The success rate is over judged trials only — a queued or " +
            "running trial is not a failure, and an infrastructure void is outside the rate."
          }
          seats={seats}
          rows={[
            {
              label: "attempted",
              values: seats.map((seat) => fmtCount(cellSums[seat.id]?.attempted ?? 0)),
            },
            {
              label: "solved",
              values: seats.map((seat) => fmtCount(seat.totals.passed || 0)),
            },
            {
              label: "success",
              values: seats.map((seat) => fmtPct(seat.totals.solve_rate)),
            },
          ]}
        />
        <CompareCard
          label="cost"
          blurb={
            "Each arm's tokens priced through a single shared table — the only cost figure " +
            "comparable across arms; self-reported spend is not and is not shown. `—` means " +
            "some model is unpriced, so the figure is unknown rather than lower."
          }
          seats={seats}
          rows={[
            {
              label: "per task",
              values: seats.map((seat) => {
                const priced = seat.totals.priced_cost;
                const attempted = cellSums[seat.id]?.attempted ?? 0;
                if (priced == null || attempted <= 0) return "—";
                return fmtMoney(priced / attempted);
              }),
            },
            {
              label: "total",
              values: seats.map((seat) => fmtMoney(seat.totals.priced_cost)),
            },
          ]}
          sub={unpriced.length > 0 ? `no price for ${unpriced.join(", ")}` : undefined}
        />
      </section>
      {critical > 0 ? (
        <section className="mb-4 border border-warn/40 bg-warn/7 px-3.5 py-2.5">
          <div className="text-[10px] tracking-[0.1em] text-warn">integrity</div>
          <div className="mt-1 font-mono text-[15px] text-warn">{critical} critical</div>
          <div className="mt-0.5 text-[10.5px] text-muted">
            The agent monitor fired a critical rule — this match&apos;s numbers are invalid
            and must not be published.
          </div>
        </section>
      ) : null}
    </>
  );
}
