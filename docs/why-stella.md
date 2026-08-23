---
id: why-stella
title: "Why stella"
status: living
---

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="brand/wordmark-paper.svg">
    <source media="(prefers-color-scheme: light)" srcset="brand/wordmark-ink.svg">
    <img src="brand/wordmark-ink.svg" alt="Stella" width="240">
  </picture>
</p>

<p align="center"><strong>Why Stella — a technical overview</strong></p>
<p align="center"><em>A fast, BYOK, model-agnostic terminal coding agent with a deterministic definition of done you can opt into. Built in Rust.</em></p>

<p align="center">
  <img src="https://img.shields.io/badge/engine-zero--I%2FO%20core-EFC53F?style=flat-square" alt="Zero-I/O core">
  <img src="https://img.shields.io/badge/providers-9%20%2B%20local-EFC53F?style=flat-square" alt="9 providers + local">
  <img src="https://img.shields.io/badge/community%20telemetry-local--only-EFC53F?style=flat-square" alt="Community telemetry is local-only">
  <img src="https://img.shields.io/badge/rust-1.90%2B-EFC53F?style=flat-square&logo=rust&logoColor=white" alt="Rust 1.90+">
</p>

---

Stella is an open-source coding agent that runs in your terminal. It is not a
SaaS, not a swarm, and not another wrapper that declares victory on a green test
suite. It is a single static Rust binary built around one uncommon idea: **an
agent should have to prove the change it made is the change that fixed the
problem** — and everything else in the design exists to make that proof cheap,
local, and reproducible when you ask for it.

## The definition of done, and who makes it

Most agents decide they are *done* when a test suite passes. That accepts two
failure modes silently: a suite that was already green, and an edit that doesn't
actually exercise the fix.

A plain `stella run` claims neither way. It drives one deterministic step loop
and reports what it changed — the files it touched, what the turn cost, and the
event journal both came from. Nothing is running over it.

The fail → pass contract belongs to a **verification wrapper plugin**, and
`stella run --pipeline <plugin-id>` is how you ask for it. That hands the turn
to an installed plugin whose oracle demands a **witness test**: one that must
**fail on the previous code** and **pass on your change**. A green suite alone
is not accepted on that path — the fail → pass transition *is* the evidence.
What the plugin reports is self-reported evidence: Stella evaluates it against
the verdict rule that plugin declared at install, and never re-runs or
re-checks it. The built-in staged pipeline that used to run the check inside
Stella has been deleted (#3865); `--pipeline classic` is refused outright,
naming `stella plugin install` as the remedy, and `--keep-witness` /
`--require-verified` are refused on every path. Oxagen's Vera is the reference
verification plugin, private and not shipped in this repository — see
[the plugin socket](https://stella.oxagen.sh/docs/plugins).

## An engine you can actually reason about

`stella-core` performs **no I/O**. It drives every model call through a
`Provider` port and every tool through a `ToolExecutor` port, emitting an
`AgentEvent` stream over a channel. Compaction, eviction, loop detection,
routing, retry, and budget are plain **synchronous functions over owned data** —
so the whole decision core is property-testable with no network and no
filesystem, and adding a vendor or a tool is an *adapter, never a rewrite*. The
workspace is a set of focused crates; `stella-protocol` is a zero-logic stability
contract every boundary round-trips through `serde_json` byte-for-byte. There is
one deterministic step loop — plan, fan tools out in parallel, observe, compact,
repeat — that you can read top to bottom. No coordinator, no hidden control
plane.

## Trust boundaries that are actually boundaries

| Property | How it works |
|---|---|
| **BYOK, model-agnostic** | Nine hosted providers (Anthropic, OpenAI, Gemini, xAI, DeepSeek, Z.ai, OpenRouter, Vertex, Bedrock) plus **any** OpenAI-compatible local server (Ollama, vLLM, LM Studio, llama.cpp). No account, no gateway. Pin per run with `--model provider/id`. |
| **Zero telemetry egress by default** | Community/default Stella sends no telemetry anywhere. Executions, the full event stream, per-call token/cost telemetry, and a `[C·R·U·D] path` files-touched ledger land in a local `.stella/private/store.db` you can open with any SQLite client — and the store is never a dependency of a turn. The sole exception is an [explicitly enrolled Oxagen Enterprise managed deployment](https://stella.oxagen.sh/docs/telemetry#oxagen-enterprise-managed-export): a current signed policy may authorize one minimal content-free operational rollup to one exact allowlisted HTTPS sink. |
| **Budget you can trust** | `--spend-limit <usd>` aborts cleanly **between** steps, never mid-tool, so a cap can't corrupt a half-written edit. |
| **Bounded blast radius** | The built-ins that ship are `bash`, file CRUD, `search`, the task board, sub-agent delegation, scratch state, an environment probe, and `ask_question` — so yes, one of them runs a shell and four of them touch files, and the boundary is where those calls land rather than whether they exist. File paths are confined by held directory descriptors (`stella_tools::rootfd`): every component after the workspace root is opened `openat(dirfd, name, O_DIRECTORY \| O_NOFOLLOW)`, `..` pops the descriptor stack instead of opening `".."`, and popping past the root is refused — so there is no resolved string left for a rename or a planted symlink to re-point. Note what is *not* claimed: `bash`, workspace custom tools (gated, default off) and hooks all spawn processes, and a spawned command is not confined in-process by any of that. A cloned repo's own hooks never auto-execute (`STELLA_PROJECT_HOOKS=1` to opt in). Real containment means running Stella inside a container. |

## Also in the box

An **offline tree-sitter code graph** queried instead of grepping (`stella
search`; Rust/TS/TSX/JS/Python/Go/Java/C/PHP/SQL, no
key needed) ·
**prompt-cache-native memory** that loads once into a byte-stable system prompt
at ~0.1× input cost · a **fleet mode** that fans a task DAG out to
wave-scheduled workers — one shared tree under cooperative file claims by
default, an isolated git worktree per task on request · **lifecycle
hooks** and an **MCP client** that merges external tools into the registry · and
the **Command Deck** TUI with PR-style diffs and an editable prompt queue. Deep
dives: [lifecycle hooks](https://stella.oxagen.sh/docs/agent-tools/hooks),
[the files-touched ledger](https://stella.oxagen.sh/docs/telemetry/files-touched),
[the memory citation loop](https://stella.oxagen.sh/docs/context-engine#the-citation-loop-memories-that-earn-their-place).

## What it optimizes for

Provable, reproducible progress over flashy autonomy. If you want an agent whose
every decision is a synchronous function you can test, whose Community/default
telemetry never leaves your disk, whose budget is a hard boundary, and whose
"done" — once you bind a verification plugin to the run — is a fact you can
re-run rather than a sentence you take on faith, Stella is built for you. An
explicitly enrolled Oxagen Enterprise managed deployment has the single signed
operational egress exception documented above.

```bash
curl -fsSL https://raw.githubusercontent.com/macanderson/stella/main/install.sh | sh
export ANTHROPIC_API_KEY=…        # or OPENAI_API_KEY, GEMINI_API_KEY, a local server, …
stella run "fix the failing test in src/auth.rs"
```

<sub>AGPL-3.0-only, commercial licenses available · Rust 1.90+ · <a href="https://github.com/macanderson/stella">github.com/macanderson/stella</a></sub>
