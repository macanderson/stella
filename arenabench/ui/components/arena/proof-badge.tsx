"use client";

import * as React from "react";
import type { TrialProof } from "@/lib/types";
import { cn } from "@/lib/utils";
import { Tip } from "@/components/ui/tooltip";

/**
 * The proof rail at a glance: one grade, and the oracle's own run sequence.
 *
 * A benchmark's verdict column answers "did it pass". This one answers the
 * question underneath it — *what says so*. They come apart more often than
 * anyone expects: Stella's pipeline publishes `passed: true` for a real
 * deterministic flip, for an `unverifiable` outcome where every channel was
 * blind, for a `witness_unsatisfiable` one where the witness never
 * discriminated, and for a `waived` one where triage skipped review. Read the
 * verdict alone and all of those are the same cell.
 *
 * Two deliberate choices about colour:
 *
 * - **`refuted` is not red.** The oracle running and never passing means the
 *   agent claimed done, a test disagreed, and the claim did not ship. That is
 *   the machinery working. It is marked, not scolded.
 * - **`none` is grey, never a zero.** A seat with no pipeline proved nothing
 *   because it has no way to — the absence of the apparatus, not a score.
 */

/** What each grade means, and how loudly to say it. */
const GRADE_COPY: Record<
  string,
  { label: string; tone: string; blurb: string }
> = {
  flip: {
    label: "flip",
    tone: "text-ok",
    blurb:
      "An independently authored witness test failed on the work as it stood, " +
      "then passed. This is the only outcome that earns the word 'done'.",
  },
  oracle: {
    label: "oracle",
    tone: "text-ok",
    blurb:
      "A tracked test command went fail→pass. No witness was authored — the " +
      "command was configured rather than written for this change.",
  },
  refuted: {
    label: "refuted",
    tone: "text-warn",
    blurb:
      "The oracle ran and never passed: the agent's claim was caught before " +
      "it shipped. The rail working as designed, not an agent failure to " +
      "read twice.",
  },
  unsatisfiable: {
    label: "unsatisfiable",
    tone: "text-warn",
    blurb:
      "The witness failed the same way before and after the work: it never " +
      "discriminated between them, so its red says nothing about the change. " +
      "An instrument fault, not an agent one.",
  },
  unproven: {
    label: "unproven",
    tone: "text-warn",
    blurb:
      "Proof was warranted and no witness could be authored. The reason is on " +
      "the transcript page, in full.",
  },
  waived: {
    label: "waived",
    tone: "text-bad",
    blurb: "Verification was waived by triage. Nothing checked this work.",
  },
  unwarranted: {
    label: "no proof due",
    tone: "text-muted",
    blurb:
      "No proof was demanded of this change — documentation, config or a pure " +
      "removal has no runtime behaviour to flip.",
  },
  model: {
    label: "model said so",
    tone: "text-bad",
    blurb:
      "A model asserted the result with no deterministic backing. An " +
      "assertion, not an observation.",
  },
  heuristic: {
    label: "heuristic",
    tone: "text-bad",
    blurb: "Not even a model — a fallback rule decided the verdict.",
  },
  none: {
    label: "—",
    tone: "text-dim",
    blurb: "This seat publishes no proof rail. Nothing to read, and nothing missing.",
  },
};

/** The oracle's runs as a strip of glyphs, oldest first. */
export function FlipStrip({ proof }: { proof: TrialProof }) {
  if (!proof.oracle_runs.length) return null;
  return (
    <span className="inline-flex items-center gap-[3px] font-mono text-[10px]">
      {proof.oracle_runs.map((run, index) => (
        <span
          key={index}
          className={run.passed ? "text-ok" : "text-bad"}
          title={`${run.tree}: ${run.passed ? "passed" : "failed"}`}
        >
          {run.passed ? "✓" : "✗"}
        </span>
      ))}
    </span>
  );
}

/**
 * How the flip reads on screen: whether it is good news, bad news, or no news.
 *
 * The third case is why `flip` is three states rather than a bool (#2556).
 * `"unobserved"` says nothing was ever in a position to observe a flip, which
 * is a statement about the instrument and not a finding about the work — so it
 * renders neutral. Painting it with the same warn styling as a real
 * `"not_achieved"` tells a reader the agent fell short of something nobody
 * measured.
 */
export function flipTone(proof: TrialProof): "text-ok" | "text-warn" | "text-dim" {
  if (proof.flip === "achieved") return "text-ok";
  if (proof.flip === "not_achieved") return "text-warn";
  return "text-dim";
}

/** The sentence describing what the strip shows, for a tooltip or a caption. */
export function flipSentence(proof: TrialProof): string {
  const { failed_attempts: wrong, flip } = proof;
  if (!proof.oracle_runs.length) return "The oracle never ran on this trial.";
  if (flip === "unobserved") {
    return "No command was tracked, so nothing could have flipped — this is not a finding about the work.";
  }
  if (flip === "achieved" && wrong > 0) {
    return (
      `The model believed it was finished ${wrong} time${wrong === 1 ? "" : "s"} ` +
      `before the test agreed.`
    );
  }
  if (flip === "achieved") {
    return "The test passed on the first observation after the change.";
  }
  if (flip === "not_achieved") {
    return (
      `The test never passed — ${wrong} failing observation${wrong === 1 ? "" : "s"}, ` +
      `and the work went back for revision.`
    );
  }
  return "No verdict recorded a flip finding for this trial.";
}

export function ProofBadge({
  proof,
  showStrip = true,
}: {
  proof: TrialProof | null | undefined;
  showStrip?: boolean;
}) {
  // Absent and null are the same to a reader: there is nothing to show. They
  // differ only in whether the server is old, which is not the reader's
  // problem.
  if (!proof) {
    return <span className="font-mono text-[11px] text-dim">—</span>;
  }
  const copy = GRADE_COPY[proof.grade] ?? {
    label: proof.grade,
    tone: "text-muted",
    blurb: "",
  };

  const notes: string[] = [copy.blurb];
  if (proof.oracle_runs.length) notes.push(flipSentence(proof));
  if (proof.claimed_without_proof) {
    notes.push("This trial reported a pass with nothing deterministic behind it.");
  }
  if (proof.verifier_independent === false) {
    notes.push("The verifier was the same model as the worker — it graded its own work.");
  }
  if (proof.unstable_flip) {
    notes.push("The same command both passed and failed: the flip shows flakiness, not a fix.");
  }

  return (
    <Tip content={notes.filter(Boolean).join(" ")}>
      <span className="inline-flex items-center gap-1.5 whitespace-nowrap">
        <span className={cn("font-mono text-[11px] font-semibold", copy.tone)}>
          {copy.label}
        </span>
        {showStrip && <FlipStrip proof={proof} />}
        {proof.claimed_without_proof && (
          <span className="font-mono text-[10px] text-bad" aria-label="unproven claim">
            ⚠
          </span>
        )}
      </span>
    </Tip>
  );
}

export { GRADE_COPY };
