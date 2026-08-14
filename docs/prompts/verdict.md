---
id: prompt-verdict
title: "verdict — the effective prompt"
status: living
---

# `verdict`

**One caller, and it is not the staged pipeline.** This role now belongs
entirely to `stella goal`'s outer assessor: the model that decides whether an
*objective* has been met across rounds. The pipeline's own verdict call — a raw
`PASS`/`FAIL` on a candidate's diff — was deleted in #2584, and nothing replaced
it.

**Wire alias:** this role shipped as `judge`, so `#[serde(alias = "judge")]`
keeps every recorded model call in every stored session readable.

| | |
|---|---|
| Call role | `ModelCallRole::Verdict` (`"verdict"`, alias `"judge"`) |
| Router tier | `Role::Verifier` — prefers a family other than the worker's |
| Dispatch | **engine sub-agent turn** (`SubAgentSpec`), `max_steps: 8`, `write_access: false`, `temperature: 0.0` |
| System message | `VERIFIER_SYSTEM_PROMPT`, `crates/stella-core/src/goal.rs` |
| Payload | the `instruction` field built in `Engine::assess`, same file |
| Sent from | `crates/stella-core/src/goal.rs::assess` |
| Tools | four, allowlisted at execution: `task_list`, `get_state`, `list_state`, `get_environment` — the catalog's read-only rows |
| Output cap | `GoalConfig::verifier_max_output_tokens` (caller-stated; no role default) |
| Assignable via `responsibilities` | **no** — this is not a pipeline call, so `Roster::apply` rejects the key as `NotAssignable` |

The verifier's model is resolved independently of the worker's and prefers a
different model family (#1795). A verifier that is the worker is not a second
opinion.

## Why this call survived and the pipeline's did not

They answer different questions, and only one of them has an oracle.

"Did this change do what it claimed?" is answerable by running a command: a test
that failed before the change and passes after it settles it, and where no such
flip exists, no amount of reading settles it either. That is the pipeline's
question, and #2584 replaced its model verdict with
`LadderDecision`'s five terminal outcomes — measured, that verdict agreed with
Terminal-Bench's grader 46% of the time and 17 of its false passes cost 5 tasks
outright.

"Has the goal been reached?" is not a question any single command can run. Goal
mode's assessor is therefore a genuine judgement call, and it is shaped like one:
it gathers its own evidence with read-only tools rather than being handed a
bounded diff, and its "not yet" is *feedback to the worker*, not a gate on
completion.

## Wire shape

```
[ system(VERIFIER_SYSTEM_PROMPT)   ← fixed, byte-identical every round
  user(instruction) ]              ← then the child's own tool loop
```

This is an engine turn, not a raw completion, so only the opening pair is
knowable up front — everything after the first tool call is transcript. The
child's evidence-gathering **never enters the goal transcript**: only the verdict
crosses back, which is what lets the assessor be thorough without every later
worker round paying to re-send what it read.

There is no `agents.verifier.prompt` door here. The system message is the
contract that makes the child read-only, not a preference to override.

## System message (verbatim)

```text
You are an impartial verifier assessing whether a coding agent has fully met a stated goal. Judge from EVIDENCE, never from claims: use whatever read-only tools you are offered to verify the work directly whenever the transcript alone is not conclusive — read the changed files, check the tests exist, inspect CI. Claimed success without supporting evidence is NOT met. The strongest completion evidence is a witness test observed to fail on the previous code and pass on the new code; a merely green test suite is weak evidence, since it cannot distinguish real work from vacuous tests or unwired code. If you need something only the worker can provide (a trace, a screenshot, a system log, an explanation), set met:false and put the request in feedback — the worker acts on it next round. When decided, end your reply with ONLY a JSON object, no prose after it:
{"met": true|false, "reasoning": "why, in one or two sentences", "feedback": "if not met: the single most useful next action or evidence request"}
```

The prompt deliberately names no individual tools: the verifier judges with
whatever read-only surface the host actually offers, so the offered set can
vary without the prompt drifting. The offered set is
`VERIFIER_TOOL_ALLOWLIST` (`crates/stella-cli/src/agent/goal.rs`), pinned by a
test to exactly the catalog's read-only rows so the allowlist and the catalog
cannot drift apart. The allowlist narrows the session stack **before** the
read-only view applies: a bare read-only wrap admits every schema
self-declaring `read_only: true` — including any MCP or custom tool that says
so about itself, an outbound egress channel from a role that reads
worker-influenced content, which is a prompt-injection hazard (#1783).

## User message (template)

```text
GOAL:
{goal}

AGENT TRANSCRIPT (most recent last):
{transcript}

Has the goal been fully met? Verify with your tools where the transcript is not conclusive.
```

`{transcript}` is the tail of the worker's conversation, rendered to
`GoalConfig::verifier_transcript_chars`.

## The JSON contract

Parsed from the **end** of the reply, matching the prompt's own "no prose after
it" clause. A verdict of `met: false` becomes the worker's next round via
`verifier_feedback_text`, which falls back to `reasoning` when `feedback` is
empty — the verifier explained *why* even when it offered no next action, and
that fallback lives in one place so no surface re-implements it differently.

## What is gone

The pipeline half of this page — `VERIFIER_INSTRUCTIONS`, `verifier_prompt`,
`verifier_stage.rs`, the `agents.verifier.prompt` override for a raw verdict
call, the diff-last D5 framing and the 5,000-token `DiffScope::Budgeted` render —
was deleted in #2584. `VERIFIER_DIFF_BUDGET_TOKENS` and `bounded_worker_diff`
still exist in `crates/stella-pipeline/src/verify/diff_render.rs` but have no
production caller (tracked separately); their presence is not evidence that a
pipeline verdict call happens.

Two pieces of that machinery *did* survive, and neither is a review:

- **`verifier_waiver_stands`** (`crates/stella-pipeline/src/pipeline.rs`) still
  decides whether triage's `VERIFIER: no` may stand — but "buying the verifier"
  no longer means buying a model call, only whether the ladder may waive the
  question. See `doc:witness-protocol` §7.1.
- **`evidence_demand_prompt`** (`crates/stella-pipeline/src/verify.rs`) still
  spends one revision asking for corroboration when nothing deterministic backs
  the turn. It is a fixed template over the tracked command, issued to the
  worker, making no claim about the change.

On the wire, `LadderRung` keeps `model_judge`, `model_verdict`, and
`heuristic_fallback` as read-only aliases of `unverified` so historical streams
still parse. New runs never write them.

## Related

- [distress-guidance.md](distress-guidance.md) — the other role #2584 removed from the pipeline
- [witness-author.md](witness-author.md) — the one verifier-tier call the pipeline still buys
- [worker.md](worker.md) — receives goal-mode feedback, and the evidence demand
- [triage.md](triage.md) — `VERIFIER: yes|no`, which now gates a waiver rather than a call
