"use client";

import * as React from "react";
import type { TrialProof } from "@/lib/types";
import { fmtMoney, fmtTokens } from "@/lib/format";
import { cn } from "@/lib/utils";
import { FlipStrip, GRADE_COPY, flipSentence } from "@/components/arena/proof-badge";

/**
 * The proof rail of one trial, in full: what was demanded, who wrote the test,
 * whether it flipped, and — when it did not — the untruncated sentence saying
 * why.
 *
 * This is the page's answer to the only question a verdict cannot answer on
 * its own: *is this trial done, or does it merely say it is?* Four sections,
 * in the order a reader needs them:
 *
 * 1. **The flip.** The oracle's own run sequence, oldest first. A trace like
 *    ✗ ✓ ✓ is the whole thesis in one line — the model thought it had
 *    finished, a test disagreed, and only then did it actually finish.
 * 2. **The witness.** Which file, which command, which fingerprint, and
 *    whether the worker left it alone.
 * 3. **The pipeline.** Which model served which role. The witness author is
 *    the one that matters and the one no config file can tell you: it rides
 *    the verifier's resolution and is then filtered for independence against
 *    the worker.
 * 4. **The reasons.** Verbatim, never clipped.
 *
 * Sections with nothing to say are omitted rather than rendered empty — but
 * the *reason* a section is empty is itself content, so "no witness was
 * authored" prints its sentence instead of vanishing.
 */

function Row({
  label,
  children,
  tone,
}: {
  label: string;
  children: React.ReactNode;
  tone?: string;
}) {
  return (
    <div className="flex gap-3 py-[3px]">
      <span className="w-[104px] flex-none text-[10.5px] lowercase text-dim">{label}</span>
      <span className={cn("min-w-0 flex-1 break-words text-[11.5px]", tone ?? "text-muted")}>
        {children}
      </span>
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="border-t border-line-soft py-2.5 first:border-t-0">
      <h3 className="pb-1 text-[10.5px] font-semibold lowercase tracking-[0.08em] text-dim">
        {title}
      </h3>
      {children}
    </section>
  );
}

/** A yes/no/unknown fact. `null` renders as "unmeasured", never as "no". */
function Flag({
  value,
  yes,
  no,
  unknown = "unmeasured",
  invert = false,
}: {
  value: boolean | null | undefined;
  yes: string;
  no: string;
  unknown?: string;
  invert?: boolean;
}) {
  if (value == null) return <span className="text-dim">{unknown}</span>;
  const good = invert ? !value : value;
  return <span className={good ? "text-ok" : "text-bad"}>{value ? yes : no}</span>;
}

export function ProofPanel({ proof }: { proof: TrialProof | null | undefined }) {
  if (!proof) {
    return (
      <div className="rounded-[10px] border border-line bg-panel px-3.5 py-3 font-mono text-[11.5px] text-dim">
        This seat publishes no proof rail — there is no witness, no oracle and no
        pipeline roster to read. Nothing is missing; the machinery is not there.
      </div>
    );
  }

  const copy = GRADE_COPY[proof.grade] ?? {
    label: proof.grade,
    tone: "text-muted",
    blurb: "",
  };
  const reasons: Array<{ label: string; text: string }> = [
    ...proof.witness_unavailable.map((text) => ({ label: "no witness", text })),
    ...proof.verification_unavailable.map((text) => ({ label: "unverifiable", text })),
    ...proof.verdict_degraded.map((text) => ({ label: "degraded", text })),
  ];

  return (
    <div className="rounded-[10px] border border-line bg-panel px-3.5 py-1 font-mono">
      <Section title="proof">
        <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1 pb-1">
          <span className={cn("text-[13px] font-semibold", copy.tone)}>{copy.label}</span>
          {proof.claimed_without_proof && (
            <span className="rounded-[5px] bg-bad/15 px-1.5 py-[1px] text-[10.5px] text-bad">
              ⚠ said done, proved nothing
            </span>
          )}
          {proof.verifier_independent === false && (
            <span className="rounded-[5px] bg-bad/15 px-1.5 py-[1px] text-[10.5px] text-bad">
              graded its own work
            </span>
          )}
          {proof.unstable_flip && (
            <span className="rounded-[5px] bg-warn/15 px-1.5 py-[1px] text-[10.5px] text-warn">
              flaky flip
            </span>
          )}
        </div>
        <p className="pb-1 text-[11.5px] leading-[1.55] text-muted">{copy.blurb}</p>
        <Row label="rung">
          {proof.rung || <span className="text-dim">none</span>}
          <span className="pl-2 text-dim">
            {proof.deterministic
              ? "a test was run and observed"
              : "no test was run for this verdict"}
          </span>
        </Row>
        <Row label="demanded">
          {proof.warranted === null ? (
            <span className="text-dim">no warrant was published</span>
          ) : proof.warranted ? (
            "proof was required of this change"
          ) : (
            proof.warrant_reason || "no proof was required"
          )}
          {proof.diff_lines != null && (
            <span className="pl-2 text-dim">· {proof.diff_lines} diff lines</span>
          )}
        </Row>
      </Section>

      <Section title="the flip">
        {proof.oracle_runs.length ? (
          <>
            <Row label="oracle">
              <span className="inline-flex items-center gap-2">
                <FlipStrip proof={proof} />
                <span className={proof.flip_achieved ? "text-ok" : "text-warn"}>
                  {flipSentence(proof)}
                </span>
              </span>
            </Row>
            <Row label="command">
              <code className="break-all text-accent">
                {proof.tracked_command || proof.oracle_runs[0]?.command || "—"}
              </code>
            </Row>
            {proof.flip_refused && (
              <Row label="refused" tone="text-warn">
                The baseline failed for a different reason than the test asserts, so the
                flip was not credited.
              </Row>
            )}
            <Row label="coverage">
              {proof.diff_coverage === "covered" ? (
                <span className="text-ok">the test exercises the changed lines</span>
              ) : proof.diff_coverage === "not_covered" ? (
                <span className="text-bad">the test does not touch the changed lines</span>
              ) : (
                <span className="text-dim">
                  unmeasured — no coverage tooling, so whether the test ran the changed
                  lines is unknown rather than false
                </span>
              )}
            </Row>
          </>
        ) : (
          <Row label="oracle" tone="text-dim">
            The oracle never ran on this trial, so nothing observed the work
            {proof.rung ? ` — the verdict came from the ${proof.rung} rung.` : "."}
          </Row>
        )}
      </Section>

      <Section title="the witness">
        {proof.witness_authored ? (
          <>
            <Row label="authored">
              <code className="break-all text-accent">{proof.witness_path}</code>
            </Row>
            <Row label="command">
              <code className="break-all">{proof.witness_command}</code>
            </Row>
            <Row label="fingerprint">
              <code className="break-all text-dim">{proof.witness_fingerprint}</code>
            </Row>
            <Row label="tamper">
              <Flag
                value={proof.witness_intact}
                yes="intact — every artifact matched its pinned identity"
                no="modified"
                unknown="the check did not report"
              />
            </Row>
          </>
        ) : proof.witness_unavailable.length ? (
          <Row label="authored" tone="text-warn">
            No witness was authored. The reason is below, verbatim.
          </Row>
        ) : (
          <Row label="authored" tone="text-dim">
            {proof.stage_ran
              ? "The witness stage started and produced nothing."
              : "The witness stage never started."}
          </Row>
        )}
        <Row label="touched tests">
          <Flag
            value={proof.touched_tests_passed}
            yes="green after the change"
            no="failed after the change"
            unknown="unobserved"
          />
        </Row>
      </Section>

      {proof.roles.length > 0 && (
        <Section title="the pipeline">
          <table className="w-full border-collapse text-left text-[11px]">
            <thead>
              <tr className="text-[10px] lowercase tracking-[0.08em] text-dim">
                <th className="py-1 pr-3 font-normal">role</th>
                <th className="py-1 pr-3 font-normal">model</th>
                <th className="py-1 pr-3 text-right font-normal">calls</th>
                <th className="py-1 pr-3 text-right font-normal">in</th>
                <th className="py-1 pr-3 text-right font-normal">out</th>
                <th className="py-1 text-right font-normal">cost</th>
              </tr>
            </thead>
            <tbody>
              {proof.roles.map((call) => {
                // The two roles the whole rail turns on. Everything else is
                // context; these two decide whether the proof means anything.
                const pivotal =
                  call.role === "witness_author" ||
                  call.role === "witness_repair" ||
                  call.role === "verdict";
                return (
                  <tr key={`${call.role}:${call.model}`} className="border-t border-line-soft/60">
                    <td
                      className={cn(
                        "py-1 pr-3 whitespace-nowrap",
                        pivotal ? "font-semibold text-accent" : "text-muted",
                      )}
                    >
                      {call.role.replace(/_/g, " ")}
                    </td>
                    <td className="py-1 pr-3 break-all text-foreground">{call.model || "—"}</td>
                    <td className="py-1 pr-3 text-right text-muted">{call.calls}</td>
                    <td className="py-1 pr-3 text-right text-muted">
                      {fmtTokens(call.tokens_in)}
                    </td>
                    <td className="py-1 pr-3 text-right text-muted">
                      {fmtTokens(call.tokens_out)}
                    </td>
                    <td className="py-1 text-right text-muted">{fmtMoney(call.cost_usd)}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          <div className="pt-1.5">
            <Row label="independence">
              <Flag
                value={proof.verifier_independent}
                yes="the verifier resolved to a different model than the worker"
                no="the verifier WAS the worker's model — it graded its own work"
                unknown="not stamped (only the model-verdict arm records it)"
              />
            </Row>
          </div>
        </Section>
      )}

      {(reasons.length > 0 || proof.verdict_summary) && (
        <Section title="the reasons">
          {reasons.map((reason, index) => (
            <div key={index} className="py-1">
              <span className="text-[10.5px] lowercase text-warn">{reason.label}</span>
              <p className="whitespace-pre-wrap break-words text-[11.5px] leading-[1.55] text-muted">
                {reason.text}
              </p>
            </div>
          ))}
          {proof.verdict_summary && (
            <div className="py-1">
              <span className="text-[10.5px] lowercase text-dim">
                verdict{proof.honesty ? ` · ${proof.honesty}` : ""}
              </span>
              <p className="whitespace-pre-wrap break-words text-[11.5px] leading-[1.55] text-muted">
                {proof.verdict_summary}
              </p>
            </div>
          )}
        </Section>
      )}
    </div>
  );
}
