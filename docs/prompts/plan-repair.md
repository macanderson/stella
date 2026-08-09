---
id: prompt-plan-repair
title: "plan_repair — the effective prompt"
status: living
---

# `plan_repair`

Re-authoring a plan the parser rejected. The pipeline's one bounded repair
retry for the plan stage (the L-V2 "bounded repair loops" pattern) — it runs at
most once, and a second failure falls through to the one-step plan rather than
looping.

| | |
|---|---|
| Call role | `ModelCallRole::PlanRepair` (`"plan_repair"`) |
| Router tier | `Role::Plan` — the same resolved provider as the call it repairs |
| Dispatch | raw completion, `tools: []` |
| Built by | `plan_repair_prompt`, `crates/stella-pipeline/src/plan.rs` |
| Sent from | `Pipeline::plan_stage`, `crates/stella-pipeline/src/pipeline.rs` |
| Retry policy | `RetryPolicy::deterministic()` — no retry-hang on the retry |
| Output cap | 4,096 visible + 4,096 reasoning headroom |
| Effort | inherited |
| Override | `agents.worker.prompt` — **wired**, same as `plan` |

It is a distinct call role rather than a second `plan` call so that repair
spend stays separable in the paid-call ledger: a run whose planner needs a
repair on every turn is a routing problem, and it is only visible if the
repairs are counted apart.

## Wire shape

The same `[system, user]` split as the plan call it follows (#2416) — it
re-bills the same fixed instruction block on every repair, so it needs the same
stable prefix, and it is authoring the same plan, so it takes the same operator
prose. It is a **fresh completion**, not a continuation — the model does not
see the original planner prompt, which is why the unparseable response has to
be echoed back.

```
[ system(agents.worker.prompt)?      ← config.role_overrides.worker
  system(PLAN_REPAIR_INSTRUCTIONS)   ← &'static str, byte-identical every call
  user(payload) ]
```

## System message (verbatim)

```text
Your previous response could not be parsed as a plan. Re-emit the plan as a strict JSON array of step strings and NOTHING else — no prose, no code fences.
```

## User message (template)

```text
Previous response:
{echoed}

JSON array:
```

## The echo bound

`{echoed}` is the previous response clamped to `PLAN_REPAIR_ECHO_CHARS`
(16,000 chars, roughly 4k tokens), head-kept, with a marker when it was cut:

```text
[… response truncated for the repair prompt …]
```

Head-kept because the plan content, when present at all, leads the response —
the tail of a rambling one is the part worth losing.

The bound exists because an unbounded echo pays for a pathological response
**twice**: once to receive it, once to repeat it back. A healthy plan is a
short JSON array, so the echo's job is to remind the model what it just said,
not to re-bill a planner that answered with an essay.

## Outcome

- **Parses** — those steps become the plan.
- **Does not parse** — the stage returns the fallback: a single step whose
  description is the goal verbatim. There is no second repair.

## Related

- [plan.md](plan.md) — the call this repairs, and the parser that rejects
- [witness-repair.md](witness-repair.md) — the same bounded-repair pattern on
  the witness side, with the notable difference that it *is* a continuation of
  the original thread
