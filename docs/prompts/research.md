---
id: prompt-research
title: "research — the effective prompt"
status: archived
---

# `research`

**This page documents the pre-#3865 staged pipeline, kept for reference; the
shape lives in an installed wrapper plugin now (`plugins/stella-research`).**
`stella-pipeline` was deleted in #3865, so `Pipeline::research_stage` and
every symbol this page cites dispatch from no code in this tree — see
`docs/prompts/README.md § Half of this set is history`.

A read-only research sub-agent answering **one** of triage's pre-plan
questions (#1778). Fans out one child per question, concurrently, before the
planner runs — so the plan names files someone actually verified rather than
files the planner inferred.

| | |
|---|---|
| Call role | `ModelCallRole::Research` (`"research"`) |
| Router tier | `Role::Research` — the worker tier, and the worker's model unless `agents.research`/`pipeline_research_model` names another |
| Dispatch | engine turn as a sub-agent (`SubAgentSpec`) |
| Tools | read-only; `write_access: false` |
| System prompt | `RESEARCH_SYSTEM_PROMPT`, `crates/stella-pipeline/src/research.rs` |
| Instruction | `research_instruction`, same file |
| Sent from | `Pipeline::research_stage`, `crates/stella-pipeline/src/pipeline/research_stage.rs` |
| Max steps | `RESEARCH_MAX_STEPS` = 8 |
| Temperature | 0.0 |
| Output cap | `None` — inherits the engine base |
| Timeout | `research_latency_ceiling`, default 45s, **per child** |
| Effort | `low` under `effort_auto`, thinking off under `reasoning_auto` — retrieval, not deliberation |
| Override | `agents.research` — every field **except** `prompt` |

## Wire shape

An engine turn, so the prompt grows with the child's own tool calls:

```
[ system(RESEARCH_SYSTEM_PROMPT)
  user(research_instruction(goal, question))
  … the child's own tool loop, up to 8 steps … ]
```

Only the child's **final message** reaches the parent. Intermediate work is
discarded, which is what makes "be exhaustive" honest advice rather than an
invitation to spend the parent's budget.

## System message (verbatim)

```text
You are a read-only research agent inside a coding agent's planning phase. Answer ONE question about this workspace by reading files and searching — you cannot modify anything. Be concrete: name the files (paths, not descriptions), the symbols, and the facts you verified, and say plainly when you could not find an answer. Your reply goes to a planner, so keep it short and load-bearing: what it must know, nothing it could have guessed.
```

`&'static str` for the same reason every management instruction block is: the
child's system prompt is identical across the whole fan-out, so the adapters
have one stable prefix to cache-mark across all of them.

**`agents.research.prompt` is deliberately not wired.** This role is the one
whose row shapes an `EngineConfig` rather than a request — `research_stage`
applies it through `apply_role_shaping`, the same seam the witness author uses
— and `prompt` has no seat there. The exclusion is also the point: read-only is
what a research child *is*, and the system message above is where that is
stated. `model`, `provider`, `effort`, `reasoning` and `params` all apply
normally.

## User message (template)

```text
The goal this research serves:
{goal}

Answer this question about the workspace:
{question}
```

The goal rides along so the child can judge relevance — a question answered
without knowing what it serves tends to come back either too broad or about
the wrong subsystem.

## Budgets on the way back

Findings are bounded once, by `bound_research_findings`, before they reach
either sink — the planner prompt and the worker's opening user message (#2415).
Bounding at the source is what keeps a second sink from re-billing the budget:

| Bound | Value | Behaviour on breach |
|---|---|---|
| Per finding | `RESEARCH_FINDING_CHARS` = 4,000 chars | head-kept, `RESEARCH_DROP_MARKER` appended |
| Total | 12,000 chars | remaining findings dropped, marker finding appended |

The marker is literal:

```text
[… research findings truncated at the prompt budget …]
```

Counted in **chars**, not bytes, matching the recall clamp — bytes
over-charge multi-byte content.

## Fan-out and failure

Each child claims from a `FanOutBudget` sized by the number of questions. Two
gates, and both degrade rather than abort:

- **Pre-dispatch** — a parent already over its cap funds nothing. The skip is
  a *missing finding*, not an abort.
- **Per-child timeout** — the ceiling is inside the future, so a timed-out
  child settles its spend through the sub-agent primitive's drop guards and
  the stage still returns. Research degrading to fewer findings must never
  wedge the turn.

A dropped child's event stream stays whole without help (#1954): the
primitive's `CancelBracket` closes the `Started`/`Finished` bracket with the
committed step count and cost, and the engine's cancel guard emits the
abandoned call's `UsageIncomplete { Cancelled }`.

Each child gets its own `turn_instance` (parent + 1 + index), because receipts
key on `(execution_id, turn_instance, step, call_seq)` and every child restarts
`step` at 0 — concurrent children sharing a slot would overwrite each other's
manifests. `depth: 1`, per the sub-agent nesting contract.

## Related

- [triage.md](triage.md) — authors the questions on its `RESEARCH:` line
- [plan.md](plan.md) — receives the findings in their own section, kept
  distinct from recall so the planner can weigh the two provenances differently
- [worker.md](worker.md) — the second sink (#2415): the same findings ride the
  worker's opening user message, so a verified fact no longer reaches it only
  as residue in a plan step
- [worker.md](worker.md) — the `delegate` tool exposes the same read-only sub-agent
  shape to the worker, with its own system prompt
