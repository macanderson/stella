---
id: verification-gate
title: "The verification gate — how CI says whether a change helps or hurts the agent"
status: living
---

# The verification gate — how CI says whether a change helps or hurts the agent

Experiments on the agent loop fail in a specific, ugly way: the change ships,
every unit test stays green, and three weeks later a benchmark run shows the
agent quietly got worse — or more expensive, which is the same thing arriving on
a different invoice. This document names the layers CI uses to catch that early,
what each one can honestly claim, and what none of them can.

> **Read this first: two of the three layers no longer exist.** Layers 1 and 2
> were tests inside `crates/stella-pipeline`, deleted from the workspace in
> #3865. Nothing replaced them, so **no per-PR gate observes what the agent
> decides or spends today** — only the wire-contract half of Layer 2 survives.
> They are described below in the past tense because they are the shape a
> verification plugin is asked to port (`doc:pipeline-as-plugins` §8) and
> because the hole they left is the thing a reader most needs to know about.
> Tracked in #3901 alongside the rest of the sweep.

## Layer 1 — the degradation gate (**gone**; was blocking, per-PR)

`crates/stella-pipeline/src/pipeline/tests/degradation_gate.rs` drove the real
staged pipeline — triage, execution, witness plumbing, the verification ladder,
the verifier — over scripted model/test doubles, through a fixed matrix of
scenarios, and pinned three things per scenario:

1. **The decision.** A clean fail→pass flip fast-submitted deterministically; a
   flaky flip escalated; a timed-out baseline never manufactured a flip; a fresh
   lint error vetoed; a red suite revised without buying a verifier.
2. **The spend.** The exact number of model calls, by role, and the exact number
   of test-suite invocations. A change that left every verdict intact but bought
   a verifier call where the ladder used to decide for free was a **cost
   regression**, and it failed here with a message naming the extra call.
3. **The evidence.** Verdicts carried their ladder snapshot, and the verifier
   prompt carried the oracle trace — degradations in what the verifier is told
   are degradations in verifier quality one step removed.

What it proved: **the decision policy and its price did not drift.** What it
could not prove: that the policy was *good* — both sides of every assertion were
this codebase's own logic.

Its operating rule is the part worth porting. When it failed on your PR, one of
two things was true: either you broke something — fix it — or you *intended* the
new behavior, in which case the scenario's expectation was updated **in the same
PR**, where the reviewer sees "this change makes flaky flips escalate" stated as
a diff line instead of discovering it in production. Never widen an expectation
to "whatever it now does"; the gate's entire value is that intent has to be
written down.

**A verification plugin owns this now.** Its oracle and its declared verdict rule
are its decision policy, and pinning them against scripted doubles is work on the
plugin's own side of the wrapper socket — a host that neither runs the check nor
re-checks the evidence (#3511) cannot gate on either.

## Layer 2 — golden trajectories (**mostly gone**; the wire half survives)

`pipeline/tests/golden.rs` recorded full `AgentEvent` streams from fixed runs and
structurally diffed them against committed fixtures — a **drift baseline** for a
stage that stopped being emitted, an event that moved, a protocol field that
vanished. It went with the crate in #3865, and `make record-golden` no longer
exists as a target. It was never independent evidence; both sides were the same
code.

**What still runs:** the wire schema itself. `docs/wire/agentevent.schema.json`
is committed and asserted by `crates/stella-protocol/tests/wire_contract.rs`;
regenerate with `scripts/export-agentevent-schema.sh`. Protocol changes must be
additive — old streams must keep parsing, pinned by tests — and the `wire-schema`
gate step plus `.github/workflows/wire-schema.yml` enforce that a hand-edited
generated schema cannot land alone.

So the wire *contract* is still gated. The event *trajectory* is not.

## Layer 3 — the benchmark arm (deliberate, expensive, honest by protocol)

Terminal-Bench runs under `bench/` measure the thing the other layers cannot:
**capability on real tasks**. They cost real money and hours, so they are not
per-PR. What keeps them honest is protocol, not frequency:

- **Preregistration.** The comparison, task set, SUT commit, and scoring are
  filed as an issue *before* the run (e.g. #1002, #1013). No moving the
  goalposts after seeing the number.
- **The witness arm.** A frozen control adapter (digest-pinned) runs beside the
  candidate, so "the harness changed" and "the agent changed" cannot be
  confused.
- **Small-N discipline.** A 7-task smoke run distinguishes "obviously broken"
  from "plausibly fine"; it does not rank models or prove a 3% improvement.
  Claims must match the N.

This is the only layer still standing at full strength, which raises its price:
a change to the loop, prompts, routing or a wrapper plugin now has no cheap
per-PR observation between it and a benchmark run.

## How to read the layers together

- Wire schema red → your change altered what the agent *emits*. Consumers (TUI,
  serve, store projections) are affected; review the schema diff.
- Green but you touched the loop, prompts, routing, or a verification plugin:
  the change is *unobserved*, not *safe*. With Layers 1 and 2 gone there is no
  per-PR signal for what the agent decides or spends, so "the tests pass" now
  carries strictly less information than this document was written to describe.
- If a change claims to make the agent better, it earns a preregistered Layer 3
  run before that claim appears anywhere.

None of this measures model quality drift, provider-side changes, or tasks
outside the bench set. When an experiment targets something no layer observes,
the first commit should extend the observation — a gate that never grows is a
gate experiments learn to route around, and a gate that shrinks without anyone
saying so is worse.
