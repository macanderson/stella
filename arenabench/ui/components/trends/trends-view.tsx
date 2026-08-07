"use client";

import * as React from "react";
import { api } from "@/lib/api";
import type { SeriesPayload, SeriesRow, SeriesOutcomes, SeriesTaskVerdict } from "@/lib/types";
import { fmtClock, fmtMoney, fmtPct } from "@/lib/format";
import { cn } from "@/lib/utils";
import { Tip } from "@/components/ui/tooltip";

/**
 * The trends view (#2068): how the SUT is doing **across** matches, keyed on
 * the SUT commit, with comparability enforced rather than assumed.
 *
 * The honesty rules, in order of how expensive their violation has been:
 *
 * - **Series are grouped by measurement.** Two matches are directly
 *   comparable only when everything but the SUT matches — dataset digest,
 *   task list, every seat's (agent, model, pinned-roles) signature. Each
 *   group renders as its own card and nothing is ever drawn across groups:
 *   a confounded comparison presented as a trend is the most misleading
 *   thing this view could do.
 * - **A point is a SUT commit, not a match.** Two matches an hour apart on
 *   the same binary are one point (their trials pool); two on different
 *   binaries are two points. The x-axis is ordered by time but spaced
 *   evenly — adjacent points may span many SUT commits, and the caption
 *   says so instead of implying one-change deltas.
 * - **Every rate carries its interval.** A five-task cycle moves in 20%
 *   steps; a bare line invites exactly the over-reading this view exists to
 *   prevent, so each point draws its Wilson 95% interval as a whisker.
 * - **Voids are outside every rate and never hidden.** An infrastructure
 *   void is not agent performance; it is excluded from the denominator and
 *   shown as its own count, so a rate built on three surviving trials
 *   cannot masquerade as one built on five.
 * - **Nothing here recomputes a verdict** — `resolved` is Harbor's, summed
 *   server-side by /api/series from the same TrialMetrics every other
 *   surface reads.
 */

/** Wilson 95% score interval — the honest band for small n. */
function wilson(k: number, n: number): [number, number] {
  if (n <= 0) return [0, 0];
  const z = 1.96;
  const p = k / n;
  const denom = 1 + (z * z) / n;
  const center = p + (z * z) / (2 * n);
  const margin = z * Math.sqrt((p * (1 - p) + (z * z) / (4 * n)) / n);
  return [Math.max(0, (center - margin) / denom), Math.min(1, (center + margin) / denom)];
}

function zeroOutcomes(): SeriesOutcomes {
  return {
    judged: 0,
    scored: 0,
    solved: 0,
    failed: 0,
    voids: 0,
    incidents: 0,
    incidents_on_solved: 0,
    timeouts_after_solve: 0,
    timeouts_before_solve: 0,
    clock_time: 0,
    priced_cost: 0,
    priced_cost_per_solve: null,
  };
}

/** Sum outcome aggregates, with the priced figures null-poisoned. */
function addOutcomes(into: SeriesOutcomes, from: SeriesOutcomes): void {
  into.judged += from.judged;
  into.scored += from.scored;
  into.solved += from.solved;
  into.failed += from.failed;
  into.voids += from.voids;
  into.incidents += from.incidents;
  into.incidents_on_solved += from.incidents_on_solved;
  into.timeouts_after_solve += from.timeouts_after_solve;
  into.timeouts_before_solve += from.timeouts_before_solve;
  into.clock_time += from.clock_time;
  into.priced_cost =
    into.priced_cost === null || from.priced_cost === null
      ? null
      : into.priced_cost + from.priced_cost;
}

interface SeatPoint {
  outcomes: SeriesOutcomes;
  /** task -> verdicts pooled across the point's matches (repeats at one SUT
   *  land here, which is exactly the flaky-task view). */
  perTask: Map<string, SeriesTaskVerdict[]>;
}

interface Point {
  sut: string;
  firstAt: number;
  matchIds: string[];
  /** Keyed by seat signature — see {@link seatKey}. */
  seats: Map<string, SeatPoint>;
}

interface Group {
  key: string;
  rows: SeriesRow[];
  points: Point[];
  lastAt: number;
}

/**
 * A seat's identity within a group. By signature, never by index: the group
 * key is seat-order-independent (an h2h with its seats swapped is the same
 * measurement), so matches inside one group may list the same seats in a
 * different order. Duplicate signatures within one match (a mirror match —
 * the same model on both seats) are disambiguated by occurrence.
 */
function seatKeys(row: SeriesRow): string[] {
  const seen = new Map<string, number>();
  return row.seats.map((seat) => {
    const roles = Object.entries(seat.roles)
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([role, model]) => `${role}:${model}`)
      .join(",");
    const base = `${seat.agent}=${seat.model}(${roles})`;
    const occurrence = seen.get(base) ?? 0;
    seen.set(base, occurrence + 1);
    return occurrence === 0 ? base : `${base}#${occurrence + 1}`;
  });
}

/** Pool one comparability group's matches into per-SUT points. */
function buildPoints(rows: SeriesRow[]): Point[] {
  const bySut = new Map<string, Point>();
  for (const row of rows) {
    const sut = row.comparability.sut_commit || "unpinned";
    let point = bySut.get(sut);
    if (!point) {
      point = { sut, firstAt: row.created_at, matchIds: [], seats: new Map() };
      bySut.set(sut, point);
    }
    point.firstAt = Math.min(point.firstAt, row.created_at);
    point.matchIds.push(row.id);
    const keys = seatKeys(row);
    row.seats.forEach((seat, index) => {
      let slot = point.seats.get(keys[index]);
      if (!slot) {
        slot = { outcomes: zeroOutcomes(), perTask: new Map() };
        point.seats.set(keys[index], slot);
      }
      addOutcomes(slot.outcomes, seat.outcomes);
      for (const [task, verdict] of Object.entries(seat.per_task)) {
        const list = slot.perTask.get(task) ?? [];
        list.push(verdict);
        slot.perTask.set(task, list);
      }
    });
  }
  return [...bySut.values()].sort((a, b) => a.firstAt - b.firstAt);
}

function groupRows(rows: SeriesRow[]): Group[] {
  const byKey = new Map<string, SeriesRow[]>();
  for (const row of rows) {
    const list = byKey.get(row.comparability.group_key) ?? [];
    list.push(row);
    byKey.set(row.comparability.group_key, list);
  }
  return [...byKey.entries()]
    .map(([key, grouped]) => ({
      key,
      rows: grouped,
      points: buildPoints(grouped),
      lastAt: Math.max(...grouped.map((r) => r.created_at)),
    }))
    .sort((a, b) => b.lastAt - a.lastAt);
}

const CHART_H = 130;
const POINT_SPACING = 84;
const PAD_X = 34;
const PAD_Y = 14;

/**
 * Solve rate per point with Wilson whiskers — one polyline per seat. The
 * connecting line exists only *within* the group, which is what makes it
 * legal to draw at all.
 */
function SolveRateChart({ group }: { group: Group }) {
  const seats = group.rows[0].seats;
  const keys = seatKeys(group.rows[0]);
  const width = PAD_X * 2 + Math.max(1, group.points.length - 1) * POINT_SPACING;
  const y = (rate: number) => PAD_Y + (1 - rate) * (CHART_H - PAD_Y * 2);
  const x = (index: number) => PAD_X + index * POINT_SPACING;

  return (
    <div className="overflow-x-auto">
      <svg
        width={width}
        height={CHART_H + 26}
        viewBox={`0 0 ${width} ${CHART_H + 26}`}
        className="block"
        role="img"
        aria-label="solve rate by SUT commit"
      >
        {[0, 0.5, 1].map((tick) => (
          <g key={tick}>
            <line
              x1={PAD_X - 12}
              x2={width - PAD_X + 12}
              y1={y(tick)}
              y2={y(tick)}
              className="stroke-line-soft"
              strokeWidth={1}
            />
            <text
              x={2}
              y={y(tick) + 3}
              className="fill-dim font-mono"
              fontSize={9}
            >
              {Math.round(tick * 100)}
            </text>
          </g>
        ))}
        {seats.map((seat, seatIndex) => {
          const estimates = group.points.map((point) => {
            const o = point.seats.get(keys[seatIndex])?.outcomes ?? zeroOutcomes();
            const rate = o.scored > 0 ? o.solved / o.scored : null;
            const [lo, hi] = wilson(o.solved, o.scored);
            return { rate, lo, hi, o };
          });
          const line = estimates
            .map((e, i) => (e.rate === null ? null : `${x(i)},${y(e.rate)}`))
            .filter((p): p is string => p !== null)
            .join(" ");
          return (
            <g key={seat.id} style={{ color: seat.color }}>
              {line.includes(" ") && (
                <polyline
                  points={line}
                  fill="none"
                  stroke="currentColor"
                  strokeWidth={1.5}
                  opacity={0.75}
                />
              )}
              {estimates.map((e, i) =>
                e.rate === null ? null : (
                  <g key={group.points[i].sut}>
                    {/* The Wilson 95% whisker: the interval IS the point at
                        these sample sizes, so it is drawn first-class. */}
                    <line
                      x1={x(i)}
                      x2={x(i)}
                      y1={y(e.lo)}
                      y2={y(e.hi)}
                      stroke="currentColor"
                      strokeWidth={2.5}
                      opacity={0.28}
                      strokeLinecap="round"
                    />
                    <circle cx={x(i)} cy={y(e.rate)} r={3.2} fill="currentColor">
                      <title>
                        {`${seat.name}: ${e.o.solved}/${e.o.scored} solved` +
                          ` (95% CI ${Math.round(e.lo * 100)}–${Math.round(e.hi * 100)}%)` +
                          (e.o.voids ? ` · ${e.o.voids} void` : "")}
                      </title>
                    </circle>
                  </g>
                ),
              )}
            </g>
          );
        })}
        {group.points.map((point, i) => (
          <text
            key={point.sut}
            x={x(i)}
            y={CHART_H + 14}
            textAnchor="middle"
            className="fill-muted font-mono"
            fontSize={9.5}
          >
            {point.sut === "unpinned" ? "unpinned" : point.sut.slice(0, 8)}
          </text>
        ))}
      </svg>
    </div>
  );
}

/** Per-point mechanism numbers — the measurement under the headline rate. */
function PointTable({ group }: { group: Group }) {
  const seats = group.rows[0].seats;
  const keys = seatKeys(group.rows[0]);
  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[520px] border-collapse text-left">
        <thead>
          <tr className="border-b border-line-soft text-[10px] lowercase tracking-[0.09em] text-dim">
            <th className="px-2 py-1.5 font-normal">sut</th>
            <th className="px-2 py-1.5 font-normal">seat</th>
            <th className="px-2 py-1.5 text-right font-normal">solved</th>
            <th className="px-2 py-1.5 text-right font-normal">voids</th>
            <th className="px-2 py-1.5 text-right font-normal">incidents</th>
            <th className="px-2 py-1.5 text-right font-normal">
              <Tip content="Timeouts split by whether the solve had already happened: after-solve means the agent did not stop when done; before-solve means it ran out of time. They mean opposite things and are never merged.">
                <span>t/o after·before</span>
              </Tip>
            </th>
            <th className="px-2 py-1.5 text-right font-normal">cost/solve</th>
            <th className="px-2 py-1.5 text-right font-normal">time/solve</th>
          </tr>
        </thead>
        <tbody>
          {group.points.map((point) =>
            seats.map((seat, seatIndex) => {
              const o = point.seats.get(keys[seatIndex])?.outcomes ?? zeroOutcomes();
              return (
                <tr
                  key={`${point.sut}:${seat.id}`}
                  className="border-b border-line-soft/60 font-mono text-[11.5px] text-muted"
                >
                  {seatIndex === 0 && (
                    <td rowSpan={seats.length} className="px-2 py-1.5 align-middle">
                      <Tip content={`${point.matchIds.length} match(es): ${point.matchIds.join(", ")}`}>
                        <span>{point.sut === "unpinned" ? "unpinned" : point.sut.slice(0, 8)}</span>
                      </Tip>
                    </td>
                  )}
                  <td className="px-2 py-1.5" style={{ color: seat.color }}>
                    {seat.name}
                  </td>
                  <td className="px-2 py-1.5 text-right">
                    {o.scored > 0 ? `${o.solved}/${o.scored} (${fmtPct((o.solved / o.scored) * 100)})` : "—"}
                  </td>
                  <td className={cn("px-2 py-1.5 text-right", o.voids > 0 && "text-warn")}>
                    {o.voids || "0"}
                  </td>
                  <td className={cn("px-2 py-1.5 text-right", o.incidents_on_solved > 0 && "text-warn")}>
                    {o.incidents}
                    {o.incidents_on_solved > 0 ? ` (${o.incidents_on_solved} on solved)` : ""}
                  </td>
                  <td className="px-2 py-1.5 text-right">
                    {o.timeouts_after_solve}·{o.timeouts_before_solve}
                  </td>
                  <td className="px-2 py-1.5 text-right">
                    {o.priced_cost === null || o.solved === 0
                      ? "—"
                      : fmtMoney(o.priced_cost / o.solved)}
                  </td>
                  <td className="px-2 py-1.5 text-right">
                    {o.solved > 0 ? fmtClock(o.clock_time / o.solved) : "—"}
                  </td>
                </tr>
              );
            }),
          )}
        </tbody>
      </table>
    </div>
  );
}

/**
 * One task's verdicts pooled at one point. All-same renders a glyph;
 * disagreement renders the fraction — which is the flaky-task signature
 * (git-multibranch's PASS FAIL FAIL FAIL was invisible until this grid).
 */
function TaskCell({ verdicts }: { verdicts: SeriesTaskVerdict[] | undefined }) {
  if (!verdicts || verdicts.length === 0) {
    return <span className="text-dim">·</span>;
  }
  const solved = verdicts.filter((v) => v.outcome === "solved").length;
  const voids = verdicts.filter((v) => v.outcome === "void").length;
  const incident = verdicts.some((v) => v.incident && v.outcome === "solved");
  const scored = verdicts.length - voids;
  let body: React.ReactNode;
  if (scored === 0) {
    body = <span className="text-dim">∅</span>;
  } else if (solved === scored) {
    body = <span className="text-ok">✓</span>;
  } else if (solved === 0) {
    body = <span className="text-bad">✗</span>;
  } else {
    body = <span className="text-warn">{`${solved}/${scored}`}</span>;
  }
  return (
    <Tip
      content={
        `${solved}/${scored} solved` +
        (voids ? `, ${voids} void` : "") +
        (incident ? ", incident on a solved trial" : "")
      }
    >
      <span className="inline-flex items-center gap-0.5 font-mono text-[11px]">
        {body}
        {incident && <span className="text-warn text-[9px]">⚠</span>}
        {voids > 0 && <span className="text-dim text-[9px]">∅</span>}
      </span>
    </Tip>
  );
}

/** Task × SUT-commit history — the only view that separates a regression
 *  from a flaky task. */
function TaskGrid({ group }: { group: Group }) {
  const seats = group.rows[0].seats;
  const keys = seatKeys(group.rows[0]);
  const tasks = group.rows[0].comparability.tasks;
  return (
    <div className="overflow-x-auto">
      <table className="border-collapse text-left">
        <thead>
          <tr className="border-b border-line-soft text-[10px] lowercase tracking-[0.09em] text-dim">
            <th className="px-2 py-1.5 font-normal">task</th>
            {seats.length > 1 && <th className="px-2 py-1.5 font-normal">seat</th>}
            {group.points.map((point) => (
              <th key={point.sut} className="px-2 py-1.5 text-center font-mono font-normal">
                {point.sut === "unpinned" ? "unpinned" : point.sut.slice(0, 8)}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {tasks.map((task) =>
            seats.map((seat, seatIndex) => (
              <tr key={`${task}:${seat.id}`} className="border-b border-line-soft/60">
                {seatIndex === 0 && (
                  <td
                    rowSpan={seats.length}
                    className="max-w-[220px] truncate px-2 py-1 align-middle font-mono text-[11.5px]"
                  >
                    {task}
                  </td>
                )}
                {seats.length > 1 && (
                  <td className="px-2 py-1 text-[11px]" style={{ color: seat.color }}>
                    {seat.name}
                  </td>
                )}
                {group.points.map((point) => (
                  <td key={point.sut} className="px-2 py-1 text-center">
                    <TaskCell verdicts={point.seats.get(keys[seatIndex])?.perTask.get(task)} />
                  </td>
                ))}
              </tr>
            )),
          )}
        </tbody>
      </table>
    </div>
  );
}

function GroupCard({ group }: { group: Group }) {
  const comparability = group.rows[0].comparability;
  const totalVoids = group.points.reduce(
    (sum, point) =>
      sum + [...point.seats.values()].reduce((s, seat) => s + seat.outcomes.voids, 0),
    0,
  );
  return (
    <section className="min-w-0 overflow-hidden rounded-[10px] border border-line bg-panel">
      <header className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-line px-4 py-[11px]">
        <h2 className="text-[13px] font-semibold lowercase tracking-[0.04em]">
          {comparability.dataset}
        </h2>
        <span className="font-mono text-[11px] text-muted">
          {comparability.task_count} tasks
          {comparability.tasks_source === "observed" ? " (observed)" : ""} · digest{" "}
          {comparability.task_digest}
        </span>
        {comparability.seats.map((seat) => (
          <span key={seat.name} className="font-mono text-[11px] text-muted">
            {seat.agent}={seat.model}
            <span className="text-dim"> [{seat.arm}]</span>
          </span>
        ))}
        <span className="ml-auto font-mono text-[11px] text-dim">
          {group.rows.length} match{group.rows.length === 1 ? "" : "es"} ·{" "}
          {group.points.length} SUT commit{group.points.length === 1 ? "" : "s"}
          {totalVoids > 0 ? ` · ${totalVoids} void trials excluded from rates` : ""}
        </span>
      </header>
      <div className="space-y-4 px-4 py-3">
        <SolveRateChart group={group} />
        <p className="text-[10.5px] text-dim">
          Whiskers are Wilson 95% intervals — at these sample sizes the interval is the
          measurement. The x-axis is ordered by time and spaced evenly: adjacent points may
          span many SUT commits, so a step between them is not a one-change delta.
        </p>
        <PointTable group={group} />
        <TaskGrid group={group} />
      </div>
    </section>
  );
}

export function TrendsView({ active }: { active: boolean }) {
  const [payload, setPayload] = React.useState<SeriesPayload | null>(null);
  const [error, setError] = React.useState<string | null>(null);
  const [loadedOnce, setLoadedOnce] = React.useState(false);

  const load = React.useCallback(async () => {
    try {
      setError(null);
      setPayload(await api<SeriesPayload>("/api/series"));
      setLoadedOnce(true);
    } catch (err) {
      setError(String(err));
    }
  }, []);

  // Lazy: fetched when the tab is first shown, re-fetched on demand — a
  // history view has no business holding a live stream open.
  React.useEffect(() => {
    if (active && !loadedOnce) void load();
  }, [active, loadedOnce, load]);

  const groups = React.useMemo(
    () => (payload ? groupRows(payload.matches.filter((row) => row.seats.length > 0)) : []),
    [payload],
  );

  return (
    <div className="mx-auto max-w-[1600px] space-y-4 px-5 py-4">
      <div className="flex items-center gap-3">
        <h1 className="text-[15px] font-semibold lowercase tracking-[0.03em]">trends</h1>
        <span className="text-[11px] text-muted">
          outcomes across matches, keyed on the SUT commit
        </span>
        <button
          type="button"
          onClick={() => void load()}
          className="ml-auto inline-flex cursor-pointer items-center rounded-[7px] border border-line bg-transparent px-2.5 py-[5px] font-mono text-xs lowercase text-foreground transition-colors hover:border-dim"
        >
          refresh
        </button>
      </div>
      {groups.length > 1 && (
        <p className="text-[11px] text-muted">
          Series are grouped by measurement — dataset, task list, and every seat&apos;s
          (agent, model, roles). Points in different groups are{" "}
          <b className="font-semibold">not directly comparable</b> and are never connected.
        </p>
      )}
      {error && <p className="font-mono text-[12px] text-bad">{error}</p>}
      {!error && payload && groups.length === 0 && (
        <p className="text-[12px] text-muted">
          No matches on disk yet — run one and it will appear here.
        </p>
      )}
      {groups.map((group) => (
        <GroupCard key={group.key} group={group} />
      ))}
    </div>
  );
}
