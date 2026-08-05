---
id: verification-gate
title: "The verification gate — how CI says whether a change helps or hurts the agent"
status: living
---

# The verification gate — how CI says whether a change helps or hurts the agent

Experiments on the agent loop fail in a specific, ugly way: the change ships,
every unit test stays green, and three weeks later a benchmark run shows the
agent quietly got worse — or more expensive, which is the same thing arriving
on a different invoice. This document names the three layers CI uses to catch
that early, what each one can honestly claim, and what none of them can.

The layers are ordered by cost. Every PR pays for the first two on every
`cargo test --workspace` (they are ordinary tests — free, deterministic, no
API key, no Docker). The third is bought deliberately.

## Layer 1 — the degradation gate (blocking, per-PR)

`crates/stella-pipeline/src/pipeline/tests/degradation_gate.rs` drives the **real
pipeline** — triage, execution, witness plumbing, the verification ladder,
the verifier — over scripted model/test doubles, through a fixed matrix of
scenarios, and pins three things per scenario:

1. **The decision.** A clean fail→pass flip fast-submits deterministically; a
   flaky flip escalates; a timed-out baseline never manufactures a flip; a
   fresh lint error vetoes; a red suite revises without buying a verifier.
2. **The spend.** The exact number of model calls, by role, and the exact
   number of test-suite invocations. A change that leaves every verdict
   intact but buys a verifier call where the ladder used to decide for free is
   a **cost regression**, and it fails here with a message naming the extra
   call.
3. **The evidence.** Verdicts carry their ladder snapshot (provenance), and
   the verifier prompt carries the oracle trace — degradations in what the
   verifier is told are degradations in verifier quality one step removed.

What it proves: **the decision policy and its price did not drift.** What it
cannot prove: that the policy is *good* — both sides of every assertion are
this codebase's own logic.

When it fails on your PR, one of two things is true. Either you broke
something — fix it — or you *intended* the new behavior, in which case the
scenario's expectation is updated **in the same PR**, where the reviewer
sees "this change makes flaky flips escalate" stated as a diff line instead
of discovering it in production. Never widen an expectation to "whatever it
now does"; the gate's entire value is that intent has to be written down.

## Layer 2 — golden trajectories (blocking, per-PR)

`pipeline/tests/golden.rs` records full `AgentEvent` streams from fixed runs
and structurally diffs them against committed fixtures. This is a **drift
baseline**: it catches a stage that stopped being emitted, an event that
moved, a protocol field that vanished — the wire-contract regressions no
single-flow assertion notices. Refresh with `make record-golden` and review
the fixture diff as a contract change. It is not independent evidence; both
sides are the same code.

The wire schema itself (`docs/wire/agentevent.schema.json`) is committed and
asserted by `crates/stella-protocol/tests/wire_contract.rs`; regenerate with
`scripts/export-agentevent-schema.sh`. Protocol changes must be additive —
old streams must keep parsing (pinned by tests).

## Layer 3 — the benchmark arm (deliberate, expensive, honest by protocol)

Terminal-Bench runs under `bench/` measure the thing the first two layers
cannot: **capability on real tasks**. They cost real money and hours, so
they are not per-PR. What keeps them honest is protocol, not frequency:

- **Preregistration.** The comparison, task set, SUT commit, and scoring are
  filed as an issue *before* the run (e.g. #1002, #1013). No moving the
  goalposts after seeing the number.
- **The witness arm.** A frozen control adapter (digest-pinned) runs beside
  the candidate, so "the harness changed" and "the agent changed" cannot be
  confused.
- **Small-N discipline.** A 7-task smoke run distinguishes "obviously
  broken" from "plausibly fine"; it does not rank models or prove a 3%
  improvement. Claims must match the N.

## How to read the three layers together

- Layer 1 red → your change altered what the agent *decides* or *spends*.
  This is the signal the gate exists for; treat an unintended red as a bug
  in the change, not the gate.
- Layer 2 red → your change altered what the agent *emits*. Consumers
  (TUI, serve, store projections) are affected; review the fixture diff.
- Layers 1–2 green but you touched the loop, prompts, routing, or
  verification: the change is *safe*, not yet *good*. If it claims to make
  the agent better, it earns a preregistered Layer 3 run before that claim
  appears anywhere.

None of this measures model quality drift, provider-side changes, or tasks
outside the scenario matrix and bench set. When an experiment targets
something no layer observes, the first commit should extend the matrix with
a scenario that observes it — a gate that never grows is a gate experiments
learn to route around.
