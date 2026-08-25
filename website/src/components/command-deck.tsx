/**
 * Terminal transcripts — a static, server-rendered record of what a Stella run
 * prints. Server components: they ship no JavaScript and render text that is
 * the same for every visitor.
 *
 * The content is faithful, not fabricated: the states, the wrapper steps, and
 * the budget accounting are Stella's own. The numbers illustrate a run; they
 * are not a benchmark claim.
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
 * The landing-page proof: one command, the path it walks, and the evidence
 * the run ended on. Short enough to read in the time it takes to scroll past.
 */
export function HeroTerminal() {
  return (
    <Terminal title="zsh — stella">
      <Prompt>export ANTHROPIC_API_KEY=…</Prompt>
      <Prompt>stella run &quot;fix the failing test&quot;</Prompt>
      <Dim>{"  triage → plan → witness → execute → verify → verdict\n"}</Dim>
      <Dim>{"  edited src/parser.rs · ran cargo test -p parser\n"}</Dim>
      <Ok>{"  ✓ verified — 1 test now green · $0.04 of $1.00\n"}</Ok>
    </Terminal>
  );
}
