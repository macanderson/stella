---
id: prompt-plan
title: "plan — the effective prompt"
status: archived
---

# `plan`

**This page documents the pre-#3865 staged pipeline, kept for reference; the
shape lives in an installed wrapper plugin now (`plugins/stella-plan`).**
`stella-pipeline` was deleted in #3865, so `Pipeline::plan_stage` and every
symbol this page cites dispatch from no code in this tree — see
`docs/prompts/README.md § Half of this set is history`.

Authoring the ordered plan. Runs after triage, recall and research; produces a
JSON array of step strings that becomes the task board the user watches and the
step loop the worker walks.

| | |
|---|---|
| Call role | `ModelCallRole::Plan` (`"plan"`) |
| Router tier | `Role::Plan` — the worker tier, and the worker's model unless `agents.plan`/`pipeline_plan_model` names another |
| Dispatch | raw completion, `tools: []` |
| Built by | `build_planner_prompt`, `crates/stella-pipeline/src/plan.rs` |
| Sent from | `Pipeline::plan_stage`, `crates/stella-pipeline/src/pipeline.rs` |
| Retry policy | `RetryPolicy::standard()` |
| Timeout | `config.engine.model_timeout` |
| Output cap | 4,096 visible + 4,096 reasoning headroom |
| Effort | inherited — plan quality rides the session's own reasoning posture |
| Override | `agents.plan.prompt`, falling back to `agents.worker.prompt` — **wired**, prepended as a system message |

## Wire shape

```
[ system(agents.plan.prompt ?? agents.worker.prompt)?   ← config.role_overrides.plan
  system(PLANNER_INSTRUCTIONS)    ← &'static str, byte-identical every call
  user(payload) ]
```

Until #2416 the planner was the **one raw pipeline role that was not a
`ManagementPrompt`** — instructions and payload alike went as a single user
message, so its fixed opener was re-billed as uncached user text on every plan
call and again on every repair. Per #1855 the honest claim for the split is
**stability**, not a cache hit: the instruction block alone does not clear
Anthropic's ~1024-token minimum. A configured `agents.worker.prompt` joins the
same system prefix, so a tuned planner clears it sooner, not later.

## System message (verbatim)

```text
You are the planner for a coding agent. Produce a short ordered plan of concrete steps to accomplish the goal. Respond with a JSON array of step strings, e.g. ["step one", "step two"]. Keep it minimal — the fewest steps that fully accomplish the goal.
```

## User message (template)

Sections are emitted in this exact order, and every one after the goal is
conditional on having content.

```text
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

**Every section above is volatile, so all of it is payload.** The instruction
block is a `&'static str` on purpose: byte-stability across calls is the entire
point of the split, and a static is the strongest structural guarantee of it
the type system offers.

## Why `agents.worker.prompt` reaches the planner

`plan_stage` resolves its provider from the worker tier and always said so in a
comment — "Plan rides the worker's settings (same router tier, same tuning)" —
while handing `metered_raw_call` a `RoleCallOverrides::default()`. The model
routing rode the worker's settings; none of the request shaping did, and the
comment was the real hazard, because it stated a coupling that did not exist.

`prompt` is the field that needed the wire, and the reason is structural:
`temperature`, `effort`, `reasoning` and `params` already reach this call
through `PipelineConfig::engine`, which the CLI builds from the same
`agents.worker` tuning, so `metered_raw_call`'s fallback serves them. A system
prompt has no seat in an `EngineConfig` at all. So operator prose reached
worker turns via `build_pipeline_system_prompt` and stopped at the planner
that writes the worker's work order — a planner free to emit steps the
worker's own instructions forbid.

It is **prepended**, never substituted: the built-in block carries the
JSON-array output contract `parse_plan` depends on, and an override that
replaced it would break parsing on every turn it was set.

The planner has since gained a row of its own (`agents.plan`, #2374), and that
did not cost this property. The caller resolves `agents.plan` over
`agents.worker` field by field before the pipeline ever sees it, so an unset
`agents.plan.prompt` still arrives here carrying the worker's — and an operator
who wants the planner steered separately now has somewhere to say so.

The conversational fast path deliberately does *not* take this row. Its whole
job is to replace the engineering persona with `CONVERSATIONAL_SYSTEM_PROMPT`
("no tools, no code, no plan, no test"), and prepending the operator's worker
prose would re-arm exactly what that replacement suppresses — on a turn that
has no task.

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
