# Timeout policy for leaderboard-comparable runs

Status: decided 2026-08-14 (#3264, #3256). Revisit only with new evidence
from tbench.ai's maintainers.

## The rule

A run intended to be comparable with the Terminal-Bench 2.1 leaderboard uses
**stock timeouts on both sides**: the agent's budget
(`agent_timeout_multiplier`) *and* the grader's
(`verifier_timeout_multiplier`) stay at 1.0.

tbench.ai says submissions "may not modify timeouts or resources". It is
genuinely unclear whether that binds the *grader's* budget as well as the
agent's — the grader is arguably part of the harness, not the submission.
Until the maintainers settle it, the conservative reading is the one a
pre-registered series can defend:

- **Leave both multipliers stock.**
- **Report verifier timeouts as voids, never as zeros.** A
  `VerifierTimeoutError` with no reward on disk means nothing graded the
  trial; `arenabench` classifies it `void_verifier` (offline scoring,
  `telemetry.py`) and renders it `VOID` (live scoreboard,
  `cloud_watch.void_reason`), excluded from every solve-rate denominator
  with its count surfaced beside the rate.

## Why not raise the grader's budget

Raising `verifier_timeout_multiplier` would recover those voids as real
measurements — but if the stricter reading of the rule is correct, every
number in the run becomes leaderboard-ineligible, which is precisely how the
earlier 2x-agent-timeout series lost its eligibility (#3256). Voids cost
sample size; an ineligible series costs the whole run. Voids are cheaper.

## The observed shape

Run `preregA2`, task `torch-tensor-parallelism`, 2 of 3 trials: agent
finished in ~200s of a 900s budget (`stella_status = completed`, rc 0), the
verifier timed out, `reward` absent. The heavy long-horizon tail is exactly
where this bites, so a scorer that coerced the absence to 0.0 would bias the
headline downward in a way that reads as a capability finding. That coercion
is what #3264 removed.
