"use client";

import * as React from "react";
import type { EngineInfo, Snapshot } from "@/lib/types";
import { fmtClock, fmtMoney, fmtPct, fmtReward, fmtTokens } from "@/lib/format";
import { cn, seatStyle } from "@/lib/utils";
import { Disclosure } from "@/components/ui/collapsible";

/**
 * The full engine pinning behind the one-line label — what a tuner is
 * actually A/B testing, rendered from the same redacted spec the server
 * launched from. A role's unset field says "inherit" in so many words: the
 * spec records inheritance honestly (a posture never claims to pin what it
 * left open), and this panel must not flatten that back out.
 */
function PosturePanel({ engine, envKeys }: { engine: EngineInfo; envKeys?: string[] }) {
  const roles = Object.entries(engine.roles || {});
  const line = (label: string, value: React.ReactNode) => (
    <div className="flex items-baseline gap-2.5">
      <span className="w-[74px] flex-none text-[9.5px] lowercase tracking-[0.05em] text-dim">
        {label}
      </span>
      <span className="min-w-0 break-all font-mono text-[11px] text-muted">{value}</span>
    </div>
  );
  const inherit = <span className="text-dim">inherit</span>;
  return (
    <div className="grid gap-[5px] pb-1 pt-0.5">
      {line("engine", `${engine.api} · ${engine.qualified_model || engine.model || "unset"}`)}
      {line(
        "reasoning",
        engine.reasoning ? `on · ${engine.effort}` : "off",
      )}
      {(engine.max_tokens != null || engine.budget_usd != null) &&
        line(
          "limits",
          [
            engine.max_tokens != null ? `${fmtTokens(engine.max_tokens)} tok/step` : null,
            engine.budget_usd != null ? `${fmtMoney(engine.budget_usd)}/task` : null,
          ]
            .filter(Boolean)
            .join(" · "),
        )}
      {engine.base_url && line("routed at", engine.base_url)}
      {(envKeys || []).length > 0 && line("env", envKeys!.join(", "))}
      {roles.length > 0 && (
        <div className="mt-1 overflow-x-auto">
          <div className="grid min-w-[320px] gap-y-[3px] [grid-template-columns:74px_1fr_64px_64px_64px] font-mono text-[10.5px]">
            <span className="text-[9.5px] lowercase tracking-[0.05em] text-dim">role</span>
            <span className="text-[9.5px] lowercase tracking-[0.05em] text-dim">model</span>
            <span className="text-[9.5px] lowercase tracking-[0.05em] text-dim">effort</span>
            <span className="text-[9.5px] lowercase tracking-[0.05em] text-dim">reason</span>
            <span className="text-[9.5px] lowercase tracking-[0.05em] text-dim">cap</span>
            {roles.map(([name, role]) => (
              <React.Fragment key={name}>
                <span className="text-muted">{name}</span>
                <span className="truncate text-foreground" title={role.model ?? undefined}>
                  {role.model ? role.model : inherit}
                </span>
                <span>{role.effort ? role.effort : inherit}</span>
                <span>{role.reasoning == null ? inherit : role.reasoning ? "on" : "off"}</span>
                <span>{role.max_tokens != null ? fmtTokens(role.max_tokens) : inherit}</span>
              </React.Fragment>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

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
        // The monitor's verdicts for this arm. Severity semantics are the
        // agent monitor protocol's: critical invalidates the numbers, and an
        // unknown severity from a newer server is treated as at least a
        // warning. Notices are bookkeeping and stay off the card.
        const detections = (snapshot.detections || []).filter(
          (d) => d.contestant === c.id,
        );
        const critical = detections.filter((d) => d.severity === "critical");
        const warned = detections.filter(
          (d) => d.severity !== "critical" && d.severity !== "notice",
        );
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
            {/* Critical means invalid, and the card says so in words — a
                tinted cell reads as "something is off", and off is not the
                message when the numbers must not be published at all. */}
            {critical.length > 0 && (
              <div className="mt-2.5 rounded-lg border border-bad/40 bg-bad/7 px-3 py-2 text-[11.5px] text-bad">
                <div className="font-[650]">
                  {critical.length} critical detection{critical.length > 1 ? "s" : ""} — this
                  arm&apos;s numbers must not be published
                </div>
                {critical.map((d) => (
                  <div
                    key={`${d.task}:${d.rule}`}
                    className="mt-1 font-mono text-[10.5px]"
                    title={d.evidence}
                  >
                    {d.rule} · {d.task}
                  </div>
                ))}
              </div>
            )}
            <div className="mb-1 mt-3.5 flex items-baseline gap-2">
              <span className="font-mono text-[38px] font-light leading-none tracking-[-0.03em] text-(--seat-fg)">
                {fmtPct(t.solve_rate)}
              </span>
              <span className="font-mono text-xs text-muted">
                {t.passed} / {t.judged || 0} solved
                {/* Partial credit beside the pass/fail rate: two seats at 50%
                    where one averages 0.8 and the other 0.5 did not perform
                    equally, and only this number can say so. */}
                {t.mean_reward != null && (
                  <span title="average verifier score over judged trials — partial credit">
                    {" "}
                    · avg {fmtReward(t.mean_reward)}
                  </span>
                )}
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
              {/* The rate beside the raw counts: the counts reward sheer
                  prompt volume, the rate grades prompt-cache discipline —
                  the number a tuner actually turns. */}
              {t.cache_hit_rate != null && stat("cache_hit_rate", "cache hit", fmtPct(t.cache_hit_rate))}
              {/* Steps and tools are a design fingerprint, not an efficiency
                  score — reported for the divergence, crowning nobody. */}
              {stat("steps", "steps", fmtTokens(t.steps))}
              {stat("tools", "tools", fmtTokens(t.tools))}
            </div>
            {/* Output-cap truncations: the classic mis-tune — reasoning that
                cannot fit an answer — named where the tuner is looking. */}
            {t.cap_hits ? (
              <div className="mt-2 font-mono text-[11px] text-warn">
                ⚠ {t.cap_hits} output-cap truncation{t.cap_hits === 1 ? "" : "s"} — raise max
                output tokens or lower effort
              </div>
            ) : null}
            {c.engine && (
              <Disclosure className="mt-1.5" summary="engine posture">
                <PosturePanel engine={c.engine} envKeys={c.env_keys} />
              </Disclosure>
            )}
            {warned.length > 0 && (
              <div className="mt-[11px]">
                <span
                  className="inline-flex items-center rounded-full border border-warn/40 bg-warn/10 px-2.5 py-[3px] font-mono text-[10.5px] lowercase tracking-[0.05em] text-warn"
                  title={warned
                    .map((d) => `${d.rule} · ${d.task} — ${d.evidence}`)
                    .join("\n")}
                >
                  ▲ {warned.length} monitor warning{warned.length > 1 ? "s" : ""}
                </span>
              </div>
            )}
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
