---
id: prompt-plan
title: "plan — the effective prompt"
status: living
---

# `plan`

Authoring the ordered plan. Runs after triage, recall and research; produces a
JSON array of step strings that becomes the task board the user watches and the
step loop the worker walks.

| | |
|---|---|
| Call role | `ModelCallRole::Plan` (`"plan"`) |
| Router tier | `Role::Plan` — the worker tier, matching the router's tiering |
| Dispatch | raw completion, `tools: []` |
| Built by | `build_planner_prompt`, `crates/stella-pipeline/src/plan.rs` |
| Sent from | `Pipeline::plan_stage`, `crates/stella-pipeline/src/pipeline.rs` |
| Retry policy | `RetryPolicy::standard()` |
| Timeout | `config.engine.model_timeout` |
| Output cap | 4,096 visible + 4,096 reasoning headroom |
| Effort | inherited — plan quality rides the session's own reasoning posture |
| Override | `agents.worker.prompt` — **not wired** (see below) |

## Wire shape

The planner is the **one raw pipeline role that is not a `ManagementPrompt`**.
Everything — instructions and payload alike — goes as a single user message:

```
[ user(build_planner_prompt(...)) ]
```

So the planner has no cacheable system prefix, and `agents.worker.prompt` never
reaches it: `plan_stage` resolves its provider from the worker tier but hands
`metered_raw_call` a `RoleCallOverrides::default()`. The model routing rides
the worker's settings; the prompt does not. Both halves are tracked in **#2416**
— including the in-code comment that claims the coupling exists.

## Prompt (template)

Sections are emitted in this exact order, and every one after the goal is
conditional on having content.

```text
You are the planner for a coding agent. Produce a short ordered plan of concrete steps to accomplish the goal. Respond with a JSON array of step strings, e.g. ["step one", "step two"]. Keep it minimal — the fewest steps that fully accomplish the goal.

## Goal
{goal}

## Revision requested (overrides the goal where they disagree)
A human reviewed your previous plan for this goal, rejected it, and asked for this instead. Follow it exactly — do not re-emit the rejected plan, and do not add steps it did not ask for.
{revision}

## Research findings
### {question}
{answer}

## Recalled context
- [{citation_label}] ({source})
  {content}

## Repository structure
{repo_structure}

## Plan (JSON array of step strings)
```

## Why the sections sit where they do

**The revision note is placed after the goal and marked as overriding.** The
plan it corrects was already a defensible reading of the goal alone, so a
planner that weighs the two equally tends to re-emit the plan the human just
rejected. It arrives from `ScopeDecision::Revise` and is `None` on a turn's
first plan.

**Research findings are their own section, not recall frames.** Recall is what
the context plane remembered; research is what a read-only sub-agent verified
against this workspace moments ago (#1778). The planner should be able to weigh
the two provenances differently, and it cannot if they arrive in one list.

**Recall frames are cited by human label** (L-C4) with their content included
as grounding, so a plan step can name where its premise came from.

**The transcript is never here.** That is the whole point of the split context
(L-E6): the planner sees goal, recall, research and structure, and never the
worker's running conversation.

## Parsing and the repair path

`parse_plan` accepts a JSON array first, then falls back to list-marker
scraping (`1.`, `2)`, `-`, `*`, `•`). The JSON scan uses a depth-tracked
balanced-span walk that ignores brackets inside string literals, so a `]`
inside a step description cannot close the array early.

If neither parse yields steps, the stage spends exactly one bounded repair
retry — see [plan-repair.md](plan-repair.md).

If the repair also fails, or the provider errored or timed out, the fallback is
a one-step plan whose single step is the goal verbatim. A plan is never
invented.

## Related

- [triage.md](triage.md) — `CLASS: multi` is what makes a plan worth authoring
- [research.md](research.md) — fills the research section
- [plan-repair.md](plan-repair.md) — the one bounded retry
- [worker.md](worker.md) — walks the steps, and receives `step_prompt` per step
