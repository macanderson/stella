/**
 * Terminal transcripts — a static, server-rendered record of what a Stella run
 * prints.
 *
 * This file used to render an animated "command deck": two switchable
 * scenarios, a self-typing prompt, five roster frames on timers, pulsing status
 * dots, and a budget meter that filled on a loop. It has been cut back to a
 * transcript, for three reasons.
 *
 * It could not be read. The whole cycle ran about fourteen seconds and then
 * restarted, so a reader who paused on a line lost it. The information — the
 * pipeline stages, the per-task cost, the outcome of each worker — is worth
 * reading, which is exactly why it should sit still.
 *
 * It was noise. Under `prefers-reduced-motion` the component already froze on
 * the final frame and the page still made its point, which is a good sign the
 * motion was never carrying any of it.
 *
 * It was a client component for no reason: 400 lines of scenario data plus
 * timers shipped to the browser to render text that never changes per visitor.
 * These are server components now and ship no JavaScript at all.
 *
 * The content is faithful, not fabricated: the states, the pipeline stages
 * (triage → plan → witness → execute → verify → verifier), and the budget
 * accounting are Stella's own. The numbers illustrate a run; they are not a
 * benchmark claim.
 */

import type { ReactNode } from "react";

function Terminal({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="term">
      <div className="term-head">{title}</div>
      <pre className="term-body">{children}</pre>
    </div>
  );
}

function Prompt({ children }: { children: string }) {
  return (
    <>
      <span className="term-prompt">$ </span>
      {children}
      {"\n"}
    </>
  );
}

function Dim({ children }: { children: string }) {
  return <span className="term-dim">{children}</span>;
}

function Ok({ children }: { children: string }) {
  return <span className="term-ok">{children}</span>;
}

/**
 * The landing-page proof: one command, the pipeline it walks, and the evidence
 * the run ended on. Short enough to read in the time it takes to scroll past.
 */
export function HeroTerminal() {
  return (
    <Terminal title="zsh — stella">
      <Prompt>export ANTHROPIC_API_KEY=…</Prompt>
      <Prompt>stella run &quot;fix the failing test&quot;</Prompt>
      <Dim>{"  triage → plan → witness → execute → verify → verifier\n"}</Dim>
      <Dim>{"  edited src/parser.rs · ran cargo test -p parser\n"}</Dim>
      <Ok>{"  ✓ verified — 1 test now green · $0.04 of $1.00\n"}</Ok>
    </Terminal>
  );
}

/**
 * A longer transcript for the docs: a fleet fanning five tasks out over one
 * repository, with per-worker spend metered against a single cap.
 */
export function FleetTerminal() {
  return (
    <Terminal title="zsh — stella fleet">
      <Prompt>stella --budget 2 fleet --plan release-prep.toml</Prompt>
      <Dim>{"  read release-prep.toml — 5 tasks, dependency-ordered into 3 waves\n\n"}</Dim>
      {"  lead          orchestrating              "}
      <Ok>{"done"}</Ok>
      {"   $0.31  verifier: all goals met\n"}
      {"  parser-tests  unit tests for the parser  "}
      <Ok>{"done"}</Ok>
      {"   $0.34  PR #982 · CI green\n"}
      {"  api-docs      document src/api/          "}
      <Ok>{"done"}</Ok>
      {"   $0.18  committed · 11 files\n"}
      {"  clippy        fix clippy in store        "}
      <Ok>{"done"}</Ok>
      {"   $0.29  committed · clippy clean\n"}
      {"  deps          bump deps to latest minor  waits  $0.12  confirm a major bump\n\n"}
      <Dim>{"  4 done · 1 paused for a decision · $1.24 of $2.00 · branches left to review\n"}</Dim>
    </Terminal>
  );
}
