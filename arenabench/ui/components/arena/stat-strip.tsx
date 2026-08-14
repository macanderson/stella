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
 *
 * The tasks card carries an `incidents` row for the third rule, which is
 * about what a rate CANNOT say: a trial that scored the reward and then
 * raised is invisible in a solve rate, so exceptions are counted beside it and
 * never folded into it (#2066, #3225).
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
  /** Attempted trials that observed ANY behaviour — a step or a tool call.
   *
   *  The denominator behind `tools`, and deliberately NOT `usage_measured`
   *  (#3224). Behaviour and spend come from the same artifacts but different
   *  sub-objects: `telemetry.py` counts steps/tools off the trajectory's
   *  `steps` array and reads tokens off its `final_metrics`, so `final_metrics`
   *  can be missing while the step array is intact. On match `feebd80ba873`
   *  the `claude code` arm is exactly that — `usage_measured: false` on all
   *  three trials, but two of them recorded `steps: 1, tools: 0`, which is a
   *  real observation of zero tool calls. Gating tools on the usage flag would
   *  hide those two genuine zeros, so the honest test is whether this seat was
   *  ever observed doing anything at all. */
  observed: number;
}

function useCellSums(snapshot: Snapshot): Record<string, CellSums> {
  return React.useMemo(() => {
    const sums: Record<string, CellSums> = {};
    for (const seat of snapshot.contestants) {
      sums[seat.id] = { attempted: 0, tools: 0, steps: 0, observed: 0 };
    }
    for (const row of snapshot.rows) {
      for (const seat of snapshot.contestants) {
        const cell = row.cells[seat.id];
        if (!cell) continue;
        const sum = sums[seat.id];
        if (cell.status === "running" || cell.status === "done") sum.attempted += 1;
        if ((cell.steps || 0) > 0 || (cell.tools || 0) > 0) sum.observed += 1;
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

/** One arm's exceptions, split by whether the trial had already solved. */
interface Incidents {
  onSolved: number;
  onFailed: number;
  timeoutAfterSolve: number;
  timeoutBeforeSolve: number;
}

/**
 * Trials that raised, per arm, counted STRICTLY APART from the solve rate in
 * both directions (#2066).
 *
 * A trial can score the reward and then raise, so folding either number into
 * the other hides exactly the trials a headline rate flatters — which is why
 * this is its own row beside `success` rather than a correction applied to it.
 * An incident on a SOLVED trial means the agent kept burning budget after
 * succeeding; on a failed trial it is the ordinary kind.
 *
 * Timeouts split for the same reason: `solved_then_timeout` (did not stop when
 * done) and `timeout_before_solve` (ran out of time) mean opposite things, and
 * a merged count cannot be acted on — the first cost four trials on one
 * certification panel before the halt was wired (#2661).
 *
 * Per arm rather than per match: the old header summed both arms into one
 * tile, which could not say which agent was the one that would not stop.
 */
function useIncidents(snapshot: Snapshot): Record<string, Incidents> {
  return React.useMemo(() => {
    const out: Record<string, Incidents> = {};
    for (const seat of snapshot.contestants) {
      out[seat.id] = {
        onSolved: 0,
        onFailed: 0,
        timeoutAfterSolve: 0,
        timeoutBeforeSolve: 0,
      };
    }
    for (const row of snapshot.rows) {
      for (const seat of snapshot.contestants) {
        const cell = row.cells[seat.id];
        if (!cell || cell.status !== "done" || !cell.failure) continue;
        // The #2076 taxonomy label is authoritative — only the exception the
        // agent-budget machinery actually raises counts as a timeout. The
        // substring test survives solely for payloads recorded before the
        // field existed.
        const isTimeout =
          cell.outcome_reason != null
            ? cell.outcome_reason === "solved_then_timeout" ||
              cell.outcome_reason === "timeout_before_solve"
            : /timeout/i.test(cell.failure);
        const seen = out[seat.id];
        if (cell.resolved === true) {
          seen.onSolved += 1;
          if (isTimeout) seen.timeoutAfterSolve += 1;
        } else {
          seen.onFailed += 1;
          if (isTimeout) seen.timeoutBeforeSolve += 1;
        }
      }
    }
    return out;
  }, [snapshot.rows, snapshot.contestants]);
}

/**
 * The timeout split, spelled per arm for the tasks card's tooltip.
 *
 * It lives in prose rather than in a row because the two numbers are a
 * breakdown OF the incident count, not a fourth metric — but it cannot be
 * dropped: "did not stop when done" and "ran out of time before it" mean
 * opposite things about an agent, and only the first is a halt bug.
 */
function incidentTimeoutNote(
  seats: ContestantSnap[],
  incidents: Record<string, Incidents>,
): string {
  const parts = seats.flatMap((seat) => {
    const seen = incidents[seat.id];
    if (!seen || seen.timeoutAfterSolve + seen.timeoutBeforeSolve === 0) return [];
    return [
      `${seat.name}: ${seen.timeoutAfterSolve} timed out after the solve, ` +
        `${seen.timeoutBeforeSolve} before it`,
    ];
  });
  return parts.length > 0 ? `Timeouts — ${parts.join("; ")}.` : "No timeouts recorded.";
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
          /* The chip carries the identity, because five cards across a 1600px
             page leave each name ~90px and every name here truncates. The
             tint is the second signal rather than the only one, which is the
             same shape `task-table.tsx`'s legend uses — and it is now a real
             per-seat colour: `--seat-fg` used to be declared on `:root`, where
             it resolved against a `--seat` that does not exist and every seat
             inherited one paper fallback (#3223). */
          <div
            key={seat.id}
            style={seatStyle(seat.color)}
            title={seat.name}
            className="flex min-w-0 items-center justify-end gap-1.5"
          >
            <span className="size-[7px] flex-none bg-(--seat)" />
            <span className="truncate font-mono text-[10.5px] text-(--seat-fg)">
              {seat.name}
            </span>
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
  const incidents = useIncidents(snapshot);

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
            "Summed from the per-task cells, so a running trial's calls count as they happen. " +
            "`—` means no trial of this arm was ever observed taking a step or calling a " +
            "tool; a zero here is a trial that genuinely called nothing."
          }
          seats={seats}
          rows={[
            {
              label: "total",
              values: seats.map((seat) => {
                const sums = cellSums[seat.id];
                // Zero observed trials means nothing watched this arm work,
                // so the sum is unknown rather than none (#3224). One
                // observed trial is enough to make the total a measurement.
                if (!sums || sums.observed <= 0) return "—";
                return fmtCount(sums.tools);
              }),
            },
            {
              label: "avg / task",
              values: seats.map((seat) => {
                const sums = cellSums[seat.id];
                if (!sums || sums.observed <= 0) return "—";
                const perTask = avg(sums.tools, sums.attempted);
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
            "running trial is not a failure, and an infrastructure void is outside the rate. " +
            "Incidents are trials that RAISED, read solved/failed, and are counted apart " +
            "from the rate in both directions: a trial can score the reward and then raise, " +
            "and the tick alone hides it. An incident on a solved trial means the agent kept " +
            "burning budget after succeeding — amber flags exactly that. " +
            incidentTimeoutNote(seats, incidents)
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
            {
              // Restored from the tile the five-card rebuild dropped (#3225),
              // now per arm: the old one summed both arms and so could not say
              // which agent was the one that would not stop.
              label: "incidents",
              values: seats.map((seat) => {
                const seen = incidents[seat.id];
                if (!seen) return "—";
                return (
                  <span className={seen.onSolved > 0 ? "text-warn" : undefined}>
                    {seen.onSolved}/{seen.onFailed}
                  </span>
                );
              }),
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
