/**
 * Terminal transcripts — a static, server-rendered record of what a Stella run
 * prints. Server components: they ship no JavaScript and render text that is
 * the same for every visitor.
 *
 * The content is faithful, not fabricated: every row is one the shipping
 * renderer writes, in the shape it writes it. The numbers illustrate a run;
 * they are not a benchmark claim.
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
 * The landing-page proof: one command, the tool calls it made, what it
 * changed, and what the turn cost. Short enough to read in the time it takes
 * to scroll past.
 *
 * A plain `stella run` is the raw step loop. It prints no stage line and
 * reaches no verdict — the staged pipeline that once produced both was
 * deleted, and `--pipeline classic` is refused. So the rows below are the ones
 * the plain surface really writes: `plain::tool_call_card`,
 * `plain::tool_result_card`, `plain::file_change_card` and
 * `plain::cost_summary` in `crates/stella-cli/src/plain.rs`. Verification is
 * opt-in through an installed wrapper plugin and is not what this transcript
 * shows.
 */
export function HeroTerminal() {
  return (
    <Terminal title="zsh — stella">
      <Prompt>export ANTHROPIC_API_KEY=…</Prompt>
      <Prompt>stella run &quot;fix the failing test&quot;</Prompt>
      <Dim>{"  ▶ read_file(path=src/parser.rs)\n"}</Dim>
      <Dim>{"  ± modified src/parser.rs +3 −1\n"}</Dim>
      <Dim>{"  ▶ bash(command=cargo test -p parser)\n"}</Dim>
      <Ok>{"    ✓ ok in 1174ms — test result: ok. 12 passed; 0 failed\n"}</Ok>
      <Dim>{"\n  ◆ claude-sonnet-5 · $0.0413 · 18.6s\n"}</Dim>
    </Terminal>
  );
}
