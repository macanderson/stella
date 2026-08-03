# `tb21-hh10-20260731` — matched head-to-head on Terminal-Bench 2.1

The measured run behind #995: 89 tasks, one attempt each, on a **native x86_64
Linux host**, with a contemporaneous Claude Code arm run back-to-back on the same
box in the same session.

```
Arm A — Stella        58/89 = 65.17%   95% CI [55.06%, 75.28%] bootstrap · [54.33%, 74.96%] exact
Arm B — Claude Code   44/89 = 49.44%   95% CI [39.33%, 59.55%] bootstrap · [38.67%, 60.25%] exact
```

Both arms ran `glm-5.2` at effort `max`, with no per-trial budget cap, against the
dataset's own unmodified verifier.

## Identity of the run

| Field | Value |
|---|---|
| SUT commit | `62a8ae2d5d3d0839e52c268e0c2f5f0f3e3c4a49` (`v0.5.68-147-g62a8ae2d`, version 0.6.29) |
| Relationship to `main` | unmodified ancestor of `main`, post-#970 |
| Binary SHA-256 | `a6392dccd79ad6669f2e851dd6830e31672b2bad738a348d184c2a0be37f5d77` |
| Dataset | `terminal-bench/terminal-bench-2-1@sha256:7d7bdc1c…3a0699a`, all 89 tasks |
| Harbor | `0.6.1` |
| Engine posture | `a0ab8a753a4ffaf7eff5a4ec051f2e6ba3daef38bfb7455af07a634ebde7a407` |
| Assurance arm | `witness-off` (`dc61da7b…`) |
| Host | AWS EC2 `m6id.8xlarge`, 32 vCPU / 123 GiB, native x86_64, no emulation |
| Window | 2026-07-31T21:02:19Z → 23:02:16Z |

**None of the above is asserted here on trust.** Every one of the 89 Arm A trials
recorded the posture digest, assurance digest, binary SHA, source commit and
Harbor version it actually ran with, and all 89 agree on a single value for each.
Those values are in `trials.jsonl` and are collapsed into the manifest's
`recorded_by_the_run` block, which is what the `system_under_test` fields are
checked against rather than merely accompanied by.

The chain closes on the preregistration: `preregistration.json` was written at
`2026-07-31T21:02:14Z`, five seconds before the first trial started, and pins
`posture_py_sha256 = 28ac06ff…` — which is `bench/harbor_adapter/stella_harbor/posture.py`
at the SUT commit, and which recomputes to exactly the posture digest the trials
recorded.

## What is comparable between the arms, and what is not

The design's rule was that **the endpoint is the only permitted difference**:
Stella reaches `glm-5.2` through OpenRouter, Claude Code through z.ai's
Anthropic-compatible endpoint, because OpenRouter serves no `/v1/messages` and
z.ai serves no GLM-5.1. Everything else — host, day, task set, verifier, Harbor,
effort, thinking, attempts, retries, concurrency, per-task resources and
timeouts at 1.0x — was matched.

**Comparable: the pass counts.** Same 89 tasks, same verifier, same day, same box.

| | Arm A | Arm B |
|---|---:|---:|
| passed | 58 | 44 |
| passed by this arm only | 22 | 8 |
| passed by both | 36 | |
| passed by neither | 23 | |

No inferential test is reported. The preregistration commits to publishing raw
counts and does not name one, and choosing a test after seeing which way the
counts fell is the thing the preregistration exists to prevent.

**Not comparable: cost and tokens.** Three independent reasons, any one of which
is enough:

* The arms bill through **different providers**, so a dollar difference is at
  least as likely to be route pricing as anything about the agent.
* Arm A's 353.7M input tokens are dominated by **cache reads**; the two arms'
  counters do not define "input token" the same way.
* Arm B reports cost on **71 of its 89 trials**. Its `usd_total` of $63.35 is a
  sum over those 71, not over 89 — `score.json` carries `usd_reported_by` beside
  every total for exactly this reason, and the earlier version of the scorer did
  not, so a partial sum would have been published next to Arm A's complete one
  as though the two were the same measurement.

**Not a claim at all: Arm B's tool calls.** `tool_calls` is read from Stella's own
accounting block, which Claude Code does not emit, so it is absent on all 89
Arm B trials. `trials_with_zero_tool_calls: 0` alongside `tool_calls_reported_by: 0`
means *no data*, not *no tool calls*.

## What this run does not measure

**The witness tier was structurally off** (#1007). The benchmark posture pins one
model for every role, and the authored witness requires an author independent of
the worker, so the tier could not be authored on any task. Every trial recorded
`assurance_arm: witness-off`. Stella's 58/89 is therefore a **lower bound on the
full verification ladder**, not a measurement of it.

**This is not a leaderboard row.** `run-manifest.json`'s `claim_eligibility` block
lists the five reasons in full — ambient credentials, no host attestation, no
intent ledger, the plain adapter path rather than `secure_launcher.py`, and no
external maintainer trajectory review. A native host closes none of them. See
[`../README.md`](../README.md) for the line between a development baseline and an
audited claim.

## Files

```
run-manifest.json        frozen identity, cross-checked against what the trials recorded
preregistration.json     written before the first trial; pins SUT, binary, posture, denominator
trials.jsonl             Arm A — one row per trial, the score's whole input
score.json               Arm A — the computed number, as published
results.md               Arm A — per-task table, every task including the failures
comparator/              Arm B (Claude Code), same three files, same denominator
```

## Reproducing the number

Needs only this directory — the multi-gigabyte Harbor job tree is deliberately
not committed:

```bash
python bench/evidence/score_dev_baseline.py score \
  bench/evidence/tb21-hh10-20260731/trials.jsonl --tasks 89
python bench/evidence/score_dev_baseline.py score \
  bench/evidence/tb21-hh10-20260731/comparator/trials.jsonl --tasks 89
```

Both intervals are deterministic given the committed rows: the bootstrap is
seeded (`20260729`, 50,000 draws) and Clopper–Pearson needs no seed.
