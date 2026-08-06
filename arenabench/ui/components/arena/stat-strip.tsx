"use client";

import * as React from "react";
import type { Snapshot } from "@/lib/types";
import { fmtClock, fmtMoney, fmtPct, fmtTokens } from "@/lib/format";
import { cn } from "@/lib/utils";
import { Tip } from "@/components/ui/tooltip";

/**
 * The match at a glance, across the full width: the numbers someone asks for
 * before they ask anything else.
 *
 * These are **match-level** figures — every contestant summed — deliberately
 * separate from the per-seat comparison beside the task table. A stat here
 * answers "how did this run go"; the race answers "who is ahead". Conflating
 * them is what made the old header a wall of per-seat cards you had to read
 * twice to total in your head.
 *
 * Two honesty rules survive from the seat cards and matter more here, because
 * a summed number hides its provenance:
 *
 * - **Cost is the priced figure**, every seat's tokens through one price
 *   table. The agents' self-reported spend is not comparable across seats and
 *   is shown separately, never added to this one.
 * - **An unmeasured quantity renders as `—`, never as zero.** Zero is a
 *   score; absence is not. `priced_cost` is null for an unpriced model, and
 *   wasted time is null until some trial has actually been replayed.
 */
function Stat({
  label,
  value,
  sub,
  tone,
  blurb,
}: {
  label: string;
  value: React.ReactNode;
  sub?: React.ReactNode;
  tone?: "ok" | "warn" | "accent";
  blurb?: string;
}) {
  const body = (
    <div className="min-w-0 rounded-[10px] border border-line bg-panel px-3.5 py-2.5">
      <div className="text-[10px] lowercase tracking-[0.1em] text-dim">{label}</div>
      <div
        className={cn(
          "mt-1 truncate font-mono text-[19px] leading-none tracking-[-0.02em]",
          tone === "ok" && "text-ok",
          tone === "warn" && "text-warn",
          tone === "accent" && "text-accent",
        )}
      >
        {value}
      </div>
      {sub ? <div className="mt-1 truncate text-[10.5px] text-muted">{sub}</div> : null}
    </div>
  );
  return blurb ? <Tip content={blurb}>{body}</Tip> : body;
}

export function StatStrip({ snapshot }: { snapshot: Snapshot }) {
  const seats = snapshot.contestants;

  // Summed across seats. `priced_cost` is the only cross-seat-comparable
  // money, and it stays null-poisoned: if any seat is unpriced the total is
  // unknown, not a smaller number.
  const totals = React.useMemo(() => {
    let trials = 0;
    let judged = 0;
    let passed = 0;
    let running = 0;
    let tokensIn = 0;
    let tokensOut = 0;
    let selfReported = 0;
    let priced: number | null = 0;
    for (const seat of seats) {
      const t = seat.totals;
      trials += t.trials || 0;
      judged += t.judged || 0;
      passed += t.passed || 0;
      running += t.running || 0;
      tokensIn += t.tokens_in || 0;
      tokensOut += t.tokens_out || 0;
      selfReported += t.total_cost || 0;
      if (priced !== null) {
        priced = t.priced_cost == null ? null : priced + t.priced_cost;
      }
    }
    return { trials, judged, passed, running, tokensIn, tokensOut, selfReported, priced };
  }, [seats]);

  // Solve rate over *judged* trials, not over every trial that exists: a
  // queued trial is not a failure, and dividing by it reports a rate that
  // climbs as the match runs for no reason but arithmetic.
  const solveRate = totals.judged > 0 ? totals.passed / totals.judged : 0;

  // The leader line, stated only when there is a strict winner on solve rate.
  const ranked = [...seats].sort(
    (a, b) => (Number(b.totals.solve_rate) || 0) - (Number(a.totals.solve_rate) || 0),
  );
  const leader =
    ranked.length >= 2 &&
    (Number(ranked[0].totals.solve_rate) || 0) > (Number(ranked[1].totals.solve_rate) || 0)
      ? ranked[0]
      : null;

  const critical = (snapshot.detections || []).filter((d) => d.severity === "critical").length;

  return (
    <section className="mb-4 grid gap-2.5 [grid-template-columns:repeat(auto-fit,minmax(150px,1fr))]">
      <Stat
        label="solve rate"
        value={fmtPct(solveRate)}
        sub={`${totals.passed} / ${totals.judged} judged`}
        tone="accent"
        blurb="Passed trials over judged trials, every seat combined. Queued and running trials are excluded — an unjudged trial is not a failure."
      />
      <Stat
        label="leader"
        value={leader ? leader.name : "tied"}
        sub={leader ? fmtPct(Number(leader.totals.solve_rate) || 0) : "no strict winner yet"}
        tone={leader ? "ok" : undefined}
        blurb="The seat strictly ahead on solve rate. Reads 'tied' rather than picking one when the top two are level."
      />
      <Stat
        label="cost"
        value={fmtMoney(totals.priced)}
        sub="one price table, all seats"
        blurb="Every seat's tokens priced through a single table, which is the only cost figure comparable across contestants. '—' means some model in this match is unpriced, so the total is unknown rather than lower."
      />
      <Stat
        label="self-reported"
        value={fmtMoney(totals.selfReported)}
        sub="each agent's own tally"
        blurb="What the agents said they spent, each on its own price table. Shown because it is what the agent believes, never added to the priced figure."
      />
      <Stat
        label="elapsed"
        value={fmtClock(snapshot.elapsed)}
        sub={totals.running > 0 ? `${totals.running} running` : snapshot.status}
        blurb="Wall-clock time since the match started."
      />
      <Stat
        label="trials"
        value={`${totals.judged}/${totals.trials}`}
        sub={`${snapshot.rows.length} tasks × ${seats.length} seats`}
        blurb="Judged trials over total trials."
      />
      <Stat
        label="tokens"
        value={`${fmtTokens(totals.tokensIn)}/${fmtTokens(totals.tokensOut)}`}
        sub="in / out"
        blurb="Input and output tokens summed across every seat."
      />
      {critical > 0 ? (
        <Stat
          label="integrity"
          value={`${critical} critical`}
          sub="numbers are not publishable"
          tone="warn"
          blurb="The agent monitor fired a critical rule. A critical detection means this match's numbers are invalid and must not be published."
        />
      ) : null}
    </section>
  );
}
